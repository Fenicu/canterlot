#![forbid(unsafe_code)]
#![cfg(all(feature = "fault-injection", feature = "synthetic-adapter"))]

#[path = "support/e2e_gate.rs"]
mod e2e_gate;
#[path = "support/process_guard.rs"]
mod process_guard;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use canterlot_hub::{
    config::{ActivityConfig, HubTimers, LimitsConfig, SchedulerConfig},
    events::{CLIENT_KINDS, is_client_visible},
    machines,
    testkit::{
        self, Method, RequestBody, TEST_ORIGIN, TEST_OWNER_LOGIN, TestClient, TestHub,
        TestHubOptions, TestResponse, TestWs,
    },
};
use canterlot_worker::{
    config::{ExecutionConfig, MachineConfig, SupervisorTimers, WorkerConfig, WorkerTimers},
    session::{SessionState, Worker, WorkerError},
    store::WorkerStore,
};
use process_guard::ProcessGuard;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tempfile::TempDir;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{sleep, timeout},
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(40);

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

async fn until(what: &str, mut condition: impl AsyncFnMut() -> bool) {
    timeout(WAIT, async {
        while !condition().await {
            sleep(ms(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"));
}

fn seq(value: &Value) -> i64 {
    value
        .as_str()
        .expect("seq is a decimal string")
        .parse()
        .expect("decimal seq")
}

fn new_key() -> String {
    Uuid::now_v7().simple().to_string()
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn uuid(value: &Value) -> Uuid {
    value.as_str().expect("uuid string").parse().expect("uuid")
}

fn script(steps: Value) -> String {
    json!({ "synthetic": steps }).to_string()
}

// ---------------------------------------------------------------- WSS

/// Одно соединение WSS: кадры складываются в общий журнал записи по порядку прихода.
struct Connection {
    ops: mpsc::UnboundedSender<Value>,
    task: JoinHandle<()>,
}

/// Клиент WSS, переживающий переподключение: подписки возобновляются с курсора последнего кадра.
struct Recorder {
    addr: SocketAddr,
    frames: Arc<Mutex<Vec<Value>>>,
    /// Подписки: id, область и курсор, с которого подписка началась.
    subscriptions: Vec<(String, String, i64)>,
    connection: Option<Connection>,
    connections: usize,
}

impl Recorder {
    async fn connect(addr: SocketAddr) -> Self {
        let mut recorder = Self {
            addr,
            frames: Arc::default(),
            subscriptions: Vec::new(),
            connection: None,
            connections: 0,
        };
        recorder.open().await;
        recorder
    }

    async fn open(&mut self) {
        let mut ws = TestWs::connect(self.addr, TEST_ORIGIN, Some(TEST_OWNER_LOGIN))
            .await
            .expect("WSS upgrade");
        let (ops, mut queue) = mpsc::unbounded_channel::<Value>();
        let frames = self.frames.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    op = queue.recv() => match op {
                        Some(op) => ws.send(op).await,
                        None => {
                            ws.close().await;
                            return;
                        }
                    },
                    frame = ws.next(Duration::from_secs(3600)) => match frame {
                        Some(frame) => frames.lock().unwrap().push(frame),
                        None => return,
                    },
                }
            }
        });
        self.connection = Some(Connection { ops, task });
        self.connections += 1;
    }

    fn send(&self, op: Value) {
        self.connection
            .as_ref()
            .expect("open connection")
            .ops
            .send(op)
            .expect("WSS task alive");
    }

    /// Подписка с начала журнала области: сверка с базой видит каждый номер.
    async fn subscribe(&mut self, id: &str, scope: &str) {
        self.subscribe_after(id, scope, 0).await;
    }

    /// Подписка после границы снимка, как у PWA.
    async fn subscribe_after(&mut self, id: &str, scope: &str, after: i64) {
        self.subscriptions.push((id.into(), scope.into(), after));
        self.send(json!({"op":"subscribe","id":id,"scope":scope,"after":after.to_string()}));
        self.wait(&format!("subscribed {id}"), |frames| {
            frames
                .iter()
                .any(|f| f["op"] == "subscribed" && f["id"] == id)
        })
        .await;
    }

    fn frames(&self) -> Vec<Value> {
        self.frames.lock().unwrap().clone()
    }

    fn cursor(&self, id: &str) -> i64 {
        let start = self
            .subscriptions
            .iter()
            .find(|(sub, _, _)| sub == id)
            .map_or(0, |(_, _, start)| *start);
        self.frames()
            .iter()
            .filter(|f| f["op"] == "event" && f["id"] == id)
            .map(|f| seq(&f["event"]["scope_seq"]))
            .max()
            .unwrap_or(start)
    }

    /// События области в порядке прихода (по всем соединениям).
    fn events(&self, scope: &str) -> Vec<Value> {
        self.frames()
            .into_iter()
            .filter(|f| f["op"] == "event" && f["event"]["scope"] == scope)
            .map(|f| f["event"].clone())
            .collect()
    }

    fn payloads(&self, scope: &str, kind: &str) -> Vec<Value> {
        self.events(scope)
            .into_iter()
            .filter(|e| e["kind"] == kind)
            .map(|e| e["payload"].clone())
            .collect()
    }

    async fn wait(&self, what: &str, condition: impl Fn(&[Value]) -> bool) {
        until(what, async || condition(&self.frames())).await;
    }

    async fn wait_event(
        &self,
        what: &str,
        scope: &str,
        kind: &str,
        check: impl Fn(&Value) -> bool,
    ) {
        until(what, async || self.payloads(scope, kind).iter().any(&check)).await;
    }

    async fn disconnect(&mut self) {
        let connection = self.connection.take().expect("open connection");
        drop(connection.ops);
        timeout(WAIT, connection.task)
            .await
            .expect("WSS task stops")
            .expect("WSS task");
    }

    /// Новое соединение и подписки с курсоров, достигнутых до обрыва.
    async fn reconnect(&mut self) {
        if self.connection.is_some() {
            self.disconnect().await;
        }
        self.open().await;
        let mark = self.frames.lock().unwrap().len();
        for (id, scope, _) in self.subscriptions.clone() {
            let after = self.cursor(&id);
            self.send(json!({"op":"subscribe","id":id,"scope":scope,"after":after.to_string()}));
        }
        let ids: Vec<String> = self
            .subscriptions
            .iter()
            .map(|(id, _, _)| id.clone())
            .collect();
        self.wait("resubscribed", |frames| {
            ids.iter().all(|id| {
                frames[mark..]
                    .iter()
                    .any(|f| f["op"] == "subscribed" && f["id"] == id.as_str())
            })
        })
        .await;
    }
}

fn server_op_validator() -> jsonschema::Validator {
    let mut schema: Value =
        serde_json::from_str(include_str!("../../../docs/api/events.schema.json")).unwrap();
    schema["$ref"] = json!("#/$defs/ServerOp");
    schema.as_object_mut().unwrap().remove("oneOf");
    jsonschema::validator_for(&schema).unwrap()
}

fn internal_scope(owner: Uuid, scope: &str) -> String {
    match scope {
        "conversations" => format!("owner:{owner}:conversations"),
        "machines" => format!("owner:{owner}:machines"),
        other => other.to_owned(),
    }
}

/// Поток WSS против журнала: только клиентские виды и заглушки, номера каждой подписки
/// непрерывны от её начала и покрывают каждое событие базы ровно один раз; внутренние события
/// приходят только заглушками без payload. Возвращает число внутренних событий под заглушками.
async fn check_streams(pool: &PgPool, owner: Uuid, recorder: &Recorder) -> usize {
    let mut heads = BTreeMap::new();
    for (id, scope, _) in &recorder.subscriptions {
        let head: Option<i64> =
            sqlx::query_scalar("select max(scope_seq) from events where scope=$1")
                .bind(internal_scope(owner, scope))
                .fetch_one(pool)
                .await
                .unwrap();
        heads.insert(id.clone(), head.unwrap_or(0));
    }
    until("WSS reaches the journal head", async || {
        heads.iter().all(|(id, head)| recorder.cursor(id) >= *head)
    })
    .await;
    let validator = server_op_validator();
    let frames = recorder.frames();
    for frame in &frames {
        assert!(
            validator.is_valid(frame),
            "frame violates ServerOp: op={} kind={}",
            frame["op"],
            frame["event"]["kind"]
        );
    }
    let mut hidden_internal = 0;
    for (id, scope, start) in &recorder.subscriptions {
        let mut cursor = *start;
        let mut covered = Vec::new();
        for frame in frames.iter().filter(|f| f["id"] == id.as_str()) {
            match frame["op"].as_str() {
                Some("subscribed") => assert_eq!(frame["scope"], scope.as_str()),
                Some("event") => {
                    let event = &frame["event"];
                    assert_eq!(event["scope"], scope.as_str());
                    let from = seq(&event["scope_seq_from"]);
                    let to = seq(&event["scope_seq"]);
                    assert_eq!(from, cursor + 1, "{scope}: gap or duplicate at {from}");
                    assert!(to >= from);
                    let kind = event["kind"].as_str().unwrap().to_owned();
                    assert!(CLIENT_KINDS.contains(&kind.as_str()), "{kind} left the hub");
                    if kind == "hidden" {
                        assert_eq!(event["payload"], json!({}));
                        assert!(event["conversation_id"].is_null());
                        assert!(event["execution_id"].is_null());
                    }
                    covered.push((from, to, kind));
                    cursor = to;
                }
                other => panic!("{scope}: unexpected op {other:?} in the pilot flow"),
            }
        }
        assert!(
            cursor >= heads[id],
            "{scope}: stream stopped before the head"
        );
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "select scope_seq,kind from events where scope=$1 and scope_seq>$2 and scope_seq<=$3 order by scope_seq",
        )
        .bind(internal_scope(owner, scope))
        .bind(start)
        .bind(cursor)
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.0).collect::<Vec<_>>(),
            (start + 1..=cursor).collect::<Vec<_>>(),
            "{scope}: journal is not contiguous"
        );
        let kinds: BTreeMap<i64, String> = rows.into_iter().collect();
        for (from, to, kind) in covered {
            for n in from..=to {
                let stored = &kinds[&n];
                match kind.as_str() {
                    "hidden" => {
                        assert!(!is_client_visible(stored), "{scope}:{n} {stored} hidden");
                        hidden_internal += 1;
                    }
                    "oversized" => {
                        assert_eq!(from, to);
                        assert!(is_client_visible(stored));
                    }
                    visible => assert_eq!(stored, visible, "{scope}:{n}"),
                }
            }
        }
    }
    hidden_internal
}

// ---------------------------------------------------------------- стенд

struct PilotOptions {
    limits: LimitsConfig,
    activity: ActivityConfig,
    metrics_interval: Duration,
}

impl Default for PilotOptions {
    fn default() -> Self {
        Self {
            limits: LimitsConfig {
                reset_grace: ms(200),
                verify_backoff: vec![ms(300)],
                poll_min_interval: ms(100),
                poll_floor: [("claude".into(), ms(300)), ("codex".into(), ms(300))].into(),
                ..Default::default()
            },
            activity: ActivityConfig {
                debounce: ms(50),
                tick: ms(200),
                ..Default::default()
            },
            metrics_interval: Duration::from_secs(30),
        }
    }
}

type RunningWorker = (watch::Sender<bool>, JoinHandle<Result<(), WorkerError>>);

/// Настоящий хаб (HTTP и TLS gRPC), настоящая управляющая служба воркера, процессы
/// сопровождения и синтетический CLI; браузер - HTTP и WSS через сокет loopback.
struct Pilot {
    pool: PgPool,
    hub: Option<TestHub>,
    api: TestClient,
    owner: Uuid,
    machine: Uuid,
    config: WorkerConfig,
    work: PathBuf,
    worker: Option<RunningWorker>,
    guard: ProcessGuard,
    _root: TempDir,
}

impl Drop for Pilot {
    fn drop(&mut self) {
        if let Some((stop, task)) = self.worker.take() {
            let _ = stop.send(true);
            task.abort();
        }
        if let Some(hub) = self.hub.take() {
            let _ = hub.shutdown.send(());
        }
        self.guard.cleanup();
    }
}

impl Pilot {
    async fn new(pool: &PgPool, options: PilotOptions) -> Self {
        let root = tempfile::tempdir().unwrap();
        let guard = ProcessGuard::new(root.path());
        let hub = testkit::spawn_test_hub_with(
            pool.clone(),
            TestHubOptions {
                timers: HubTimers {
                    dispatch_sweep: ms(20),
                    ..Default::default()
                },
                http: Some(testkit::test_http_config()),
                limits: options.limits,
                activity: options.activity,
                scheduler: SchedulerConfig {
                    idle_poll: ms(100),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
        let owner = hub.installation.owner_id;
        let machine = machines::add(pool, owner, "pilot").await.unwrap();
        let token_file = root.path().join("token");
        fs::write(&token_file, machine.token).unwrap();
        fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
        let ca_cert = root.path().join("ca.pem");
        fs::write(&ca_cert, &hub.pki.ca_pem).unwrap();
        let work = root.path().join("work");
        fs::create_dir(&work).unwrap();
        let work = fs::canonicalize(work).unwrap();
        let config = WorkerConfig {
            hub_url: format!("https://localhost:{}", hub.addr.port()),
            ca_cert,
            token_file,
            machine_id: machine.machine_id,
            installation_id: hub.installation.installation_id,
            restore_generation: hub.installation.restore_generation,
            data_dir: root.path().join("store"),
            machine: MachineConfig {
                metrics_interval: options.metrics_interval,
            },
            attachments: Default::default(),
            claude_code: None,
            codex: None,
            timers: WorkerTimers {
                heartbeat: ms(100),
                heartbeat_ack: Duration::from_secs(2),
                range_ack: Duration::from_secs(2),
                handshake: Duration::from_secs(5),
                backoff_max: ms(500),
                ..Default::default()
            },
            execution: ExecutionConfig {
                allowed_workdir_roots: vec![work.clone()],
                supervisor_exe: Some(env!("CARGO_BIN_EXE_canterlot-worker").into()),
                supervisor: SupervisorTimers {
                    stop_grace: ms(300),
                    stop_retry: ms(200),
                    ..Default::default()
                },
                ..Default::default()
            },
        };
        let api = TestClient::over_socket(hub.http_addr()).await.as_owner();
        let mut pilot = Self {
            pool: pool.clone(),
            hub: Some(hub),
            api,
            owner,
            machine: machine.machine_id,
            config,
            work,
            worker: None,
            guard,
            _root: root,
        };
        pilot.start_worker().await;
        until("machine report applied", async || {
            let machine = pilot
                .get(&format!("/api/v1/machines/{}", pilot.machine))
                .await;
            machine["capabilities"]
                .as_array()
                .is_some_and(|c| c.contains(&json!("pilot_v1")))
                && machine["harnesses"]
                    .as_array()
                    .is_some_and(|h| !h.is_empty())
                && machine["availability"] == "available"
        })
        .await;
        pilot
    }

    fn hub(&self) -> &TestHub {
        self.hub.as_ref().unwrap()
    }

    async fn start_worker(&mut self) {
        let store = WorkerStore::open(&self.config.data_dir).unwrap();
        let worker = Worker::new(self.config.clone(), store);
        let state = worker.state();
        let (stop, rx) = watch::channel(false);
        self.worker = Some((stop, tokio::spawn(worker.run(rx))));
        until("worker live and reconciled", async || {
            *state.borrow() == SessionState::Live && self.hub().registry.is_reconciled(self.machine)
        })
        .await;
    }

    async fn ws(&self) -> Recorder {
        Recorder::connect(self.hub().http_addr()).await
    }

    async fn get(&self, path: &str) -> Value {
        let response = self.api.send(Method::GET, path, None, None).await;
        assert_eq!(response.status, 200, "GET {path}");
        response.json()
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        self.api.send(method, path, Some(&new_key()), body).await
    }

    async fn post(&self, path: &str, body: Option<Value>, status: u16) -> Value {
        let response = self.call(Method::POST, path, body).await;
        assert_eq!(response.status, status, "POST {path}: {:?}", response.body);
        if response.body.is_empty() {
            Value::Null
        } else {
            response.json()
        }
    }

    /// Сессия с запуском: живое исполнение в состоянии `running`.
    async fn create(&self, harness: &str, crunch: bool) -> Uuid {
        let created = self
            .post(
                "/api/v1/conversations",
                Some(json!({
                    "machine_id": self.machine,
                    "harness": harness,
                    "workdir": self.work.to_str().unwrap(),
                    "model": "synthetic-large",
                    "effort": "high",
                    "crunch": {"enabled": crunch},
                    "start": true,
                })),
                201,
            )
            .await;
        let id = uuid(&created["conversation_id"]);
        assert!(created["execution"]["execution_id"].is_string());
        until("execution running", async || {
            self.detail(id).await["execution"]["state"] == "running"
        })
        .await;
        id
    }

    async fn detail(&self, conversation: Uuid) -> Value {
        self.get(&format!("/api/v1/conversations/{conversation}"))
            .await
    }

    async fn submit(&self, conversation: Uuid, text: &str, attachments: &[Uuid]) -> Value {
        self.post(
            &format!("/api/v1/conversations/{conversation}/inputs"),
            Some(json!({"content":[{"type":"text","text":text}],"attachment_ids":attachments})),
            201,
        )
        .await
    }

    async fn input_state(&self, input: Uuid) -> String {
        sqlx::query_scalar("select state from inputs where id=$1")
            .bind(input)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn accepted(&self, input: Uuid) {
        until("input accepted", async || {
            self.input_state(input).await == "accepted"
        })
        .await;
    }

    async fn history(&self, conversation: Uuid) -> Vec<Value> {
        self.get(&format!(
            "/api/v1/conversations/{conversation}/history?limit=200"
        ))
        .await["items"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// Окончательные текстовые блоки ответа на ввод с данной командой.
    async fn answer(&self, conversation: Uuid, command: Uuid) -> Vec<Value> {
        self.history(conversation)
            .await
            .into_iter()
            .filter(|i| {
                i["kind"] == "block"
                    && i["block_kind"] == "text"
                    && i["input_command_id"] == command.to_string()
            })
            .collect()
    }

    async fn answered(&self, conversation: Uuid, command: Uuid, text: &str) {
        until("answer text final", async || {
            self.answer(conversation, command)
                .await
                .iter()
                .any(|b| b["state"] == "final" && b["content"] == text)
        })
        .await;
    }

    async fn upload(&self, conversation: Uuid, name: &str, bytes: &[u8]) -> Uuid {
        let sha = sha_hex(bytes);
        let created = self
            .post(
                "/api/v1/attachments",
                Some(json!({"conversation_id":conversation,"name":name,"mime":"text/plain","size":bytes.len(),"sha256":sha})),
                201,
            )
            .await;
        let id = uuid(&created["id"]);
        assert_eq!(created["upload"]["offset"], 0);
        let half = bytes.len() / 2;
        for (offset, part) in [(0, &bytes[..half]), (half, &bytes[half..])] {
            let response = self
                .api
                .send_body(
                    Method::PATCH,
                    &format!("/api/v1/attachments/{id}/content"),
                    &[("upload-offset", offset.to_string())],
                    RequestBody::from(part.to_vec()),
                )
                .await;
            assert_eq!(response.status, 204);
            assert_eq!(
                response.headers["upload-offset"],
                (offset + part.len()).to_string().as_str()
            );
        }
        let head = self
            .api
            .send(
                Method::HEAD,
                &format!("/api/v1/attachments/{id}/content"),
                None,
                None,
            )
            .await;
        assert_eq!(head.status, 200);
        assert_eq!(
            head.headers["upload-offset"],
            bytes.len().to_string().as_str()
        );
        let ready = self
            .post(
                &format!("/api/v1/attachments/{id}/complete"),
                Some(json!({"sha256":sha})),
                200,
            )
            .await;
        assert_eq!(ready["state"], "ready");
        assert_eq!(ready["sha256"], sha.as_str());
        id
    }

    fn limits_file(&self, provider: &str, value: Value) {
        let directory = self.config.data_dir.join("synthetic-limits");
        fs::create_dir_all(&directory).unwrap();
        let temporary = directory.join(format!(".{provider}.tmp"));
        fs::write(&temporary, value.to_string()).unwrap();
        fs::rename(temporary, directory.join(format!("{provider}.json"))).unwrap();
    }

    /// Процесс синтетического CLI живого исполнения разговора: (pid, start ticks) потомка
    /// процесса сопровождения этого исполнения.
    async fn cli(&self, conversation: Uuid) -> Option<(u32, u64)> {
        let execution: Uuid = sqlx::query_scalar(
            "select id from executions where conversation_id=$1 order by generation desc limit 1",
        )
        .bind(conversation)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let args = |pid: u32| -> Vec<String> {
            fs::read(format!("/proc/{pid}/cmdline"))
                .unwrap_or_default()
                .split(|b| *b == 0)
                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                .collect()
        };
        let parent = |pid: u32| -> Option<u32> {
            let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            stat[stat.rfind(')')? + 1..]
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()
        };
        let members = self.guard.members();
        let supervisor = members.iter().find(|(pid, _)| {
            let args = args(*pid);
            args.iter().any(|a| a == "supervise")
                && args.iter().any(|a| a.ends_with(&execution.to_string()))
        })?;
        members.iter().copied().find(|(pid, _)| {
            args(*pid).iter().any(|a| a == "synthetic-cli") && parent(*pid) == Some(supervisor.0)
        })
    }

    async fn wake_inputs(&self, conversation: Uuid) -> Vec<(Uuid, Uuid, String)> {
        sqlx::query_as(
            "select id,command_id,idempotency_key from inputs where conversation_id=$1 and idempotency_key like 'wake:%' order by created_at",
        )
        .bind(conversation)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis() as u64
}

fn states(payloads: &[Value], conversation: Uuid) -> Vec<String> {
    payloads
        .iter()
        .filter(|p| p["conversation_id"] == conversation.to_string())
        .map(|p| p["activity"]["state"].as_str().unwrap().to_owned())
        .collect()
}

// ---------------------------------------------------------------- сценарии

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn pilot_flow_over_http_and_wss(pool: PgPool) {
    let _permit = e2e_gate::acquire().await;
    let p = Pilot::new(&pool, PilotOptions::default()).await;

    let machines = p.get("/api/v1/machines").await;
    let list = machines.as_array().unwrap();
    assert_eq!(list.len(), 1);
    let machine = &list[0];
    assert_eq!(machine["id"], p.machine.to_string());
    assert_eq!(machine["availability"], "available");
    assert_eq!(machine["limits_ok"], true);
    assert!(machine["os"].as_str().is_some_and(|os| !os.is_empty()));
    assert_eq!(machine["allowed_roots"], json!([p.work.to_str().unwrap()]));
    let harnesses: BTreeMap<String, String> = machine["harnesses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["id"].as_str().unwrap().to_owned(),
                h["provider"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(harnesses["synthetic"], "claude");
    assert_eq!(harnesses["synthetic-codex"], "codex");
    // Профили инструкций воркера убраны планом 2c задачей 3: пресеты хранит хаб,
    // поле `profiles` зарезервировано в proto и отсутствует в представлении.
    assert!(machine.get("profiles").is_none());

    let mut ws = p.ws().await;
    ws.subscribe("list", "conversations").await;
    let id = p.create("synthetic", false).await;
    let chat = format!("conversation:{id}");
    ws.subscribe("chat", &chat).await;
    assert_eq!(
        p.get(&format!("/api/v1/machines/{}/workdirs/recent", p.machine))
            .await["workdirs"],
        json!([p.work.to_str().unwrap()])
    );
    ws.wait_event(
        "conversation_created over WSS",
        "conversations",
        "conversation_created",
        |c| c["id"] == id.to_string(),
    )
    .await;

    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let attachment = p.upload(id, "notes.txt", &bytes).await;
    // Обрыв WSS до хода: пропущенное приходит после переподключения с курсора.
    ws.disconnect().await;

    let text = script(json!([
        {"delta": "Начинаю. "},
        {"delay_ms": 700},
        {"model": "synthetic-small"},
        {"delta": "Малой моделью. "},
        {"subagent": {"name": "Explore", "model": "synthetic-small", "task": "Найти файл", "steps": [
            {"delta": "Ищу файл"},
            {"usage": {"input": 30, "output": 7}}
        ]}},
        {"read_attachment": 0},
        {"usage": {"model": "synthetic-large", "agent": "main", "input": 120, "output": 40, "cache_read": 10, "cache_write": 5}},
        {"usage": {"model": "synthetic-small", "agent": "main", "input": 60, "output": 20}}
    ]));
    let submitted = p.submit(id, &text, &[attachment]).await;
    let input = uuid(&submitted["input_id"]);
    let command = uuid(&submitted["command_id"]);
    p.accepted(input).await;
    until("run finished", async || {
        sqlx::query_scalar::<_, bool>(
            "select exists(select 1 from runs where input_command_id=$1 and finished_at is not null)",
        )
        .bind(command)
        .fetch_one(&pool)
        .await
        .unwrap()
    })
    .await;
    ws.reconnect().await;

    ws.wait_event("run_finished over WSS", &chat, "run_finished", |r| {
        r["input_command_id"] == command.to_string()
    })
    .await;
    let started = ws.payloads(&chat, "block_started");
    let block = |agent: &str, model: &str, kind: &str| {
        started
            .iter()
            .find(|b| b["agent"] == agent && b["model"] == model && b["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} block of {agent}/{model} over WSS"))
            .clone()
    };
    assert_eq!(
        block("main", "synthetic-large", "text")["parent_block_id"],
        Value::Null
    );
    let call = block("main", "synthetic-small", "tool_call");
    assert_eq!(call["tool_name"], "Agent");
    assert_eq!(call["summary"], "Найти файл");
    let child = block("Explore", "synthetic-small", "text");
    assert_eq!(child["parent_block_id"], call["block_id"]);
    assert!(
        started
            .iter()
            .all(|b| b["input_command_id"] == command.to_string())
    );
    until("activity back to idle", async || {
        states(&ws.payloads(&chat, "derived_changed"), id).last() == Some(&"idle".to_owned())
    })
    .await;
    for scope in [chat.as_str(), "conversations"] {
        let seen = states(&ws.payloads(scope, "derived_changed"), id);
        let working = seen
            .iter()
            .rposition(|s| s == "working")
            .unwrap_or_else(|| panic!("{scope}: no working state: {seen:?}"));
        assert!(
            seen[working..].iter().any(|s| s == "idle"),
            "{scope}: working never turned idle: {seen:?}"
        );
    }
    let generating = ws.payloads(&chat, "derived_changed").into_iter().any(|d| {
        d["activity"]["state"] == "working" && d["activity"]["evidence"]["phase"] == "generating"
    });
    assert!(generating, "working evidence carries the generating phase");
    ws.wait_event(
        "attachment ready on the machine",
        &chat,
        "attachment_state_changed",
        |a| a["attachment_id"] == attachment.to_string() && a["machine_state"] == "ready",
    )
    .await;
    let upload_states: Vec<_> = ws
        .payloads(&chat, "attachment_state_changed")
        .into_iter()
        .filter(|a| a["attachment_id"] == attachment.to_string() && a.get("state").is_some())
        .map(|a| a["state"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(upload_states.last().map(String::as_str), Some("ready"));

    let run = p
        .get(&format!("/api/v1/conversations/{id}/runs/{command}"))
        .await;
    assert_eq!(run["run_id"], command.to_string());
    assert_eq!(run["input_id"], input.to_string());
    let usage: BTreeSet<(String, String, i64, i64, i64, i64)> = run["usage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            (
                u["model"].as_str().unwrap().to_owned(),
                u["agent"].as_str().unwrap().to_owned(),
                u["input_tokens"].as_i64().unwrap(),
                u["output_tokens"].as_i64().unwrap(),
                u["cache_read_tokens"].as_i64().unwrap(),
                u["cache_write_tokens"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        usage,
        BTreeSet::from([
            ("synthetic-large".into(), "main".into(), 120, 40, 10, 5),
            ("synthetic-small".into(), "main".into(), 60, 20, 0, 0),
            ("synthetic-small".into(), "Explore".into(), 30, 7, 0, 0),
        ])
    );
    let agents: BTreeSet<_> = run["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part["agent"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(agents, BTreeSet::from(["main".into(), "Explore".into()]));
    assert!(run["duration_ms"].as_i64().is_some());

    let tree = p
        .get(&format!("/api/v1/conversations/{id}/delegation"))
        .await;
    let tree = tree.as_array().unwrap();
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0]["block_id"], call["block_id"]);
    assert_eq!(tree[0]["task"], "Найти файл");
    assert_eq!(tree[0]["state"], "done");
    assert_eq!(
        tree[0]["subagent"],
        json!({"name":"Explore","model":"synthetic-small"})
    );

    let checksum = format!("sha256:{}", sha_hex(&bytes));
    let answer = p.answer(id, command).await;
    assert!(
        answer
            .iter()
            .any(|b| b["agent"] == "main" && b["content"].as_str().unwrap().contains(&checksum)),
        "the agent echoed the checksum of the uploaded file"
    );
    let history = p.history(id).await;
    let input_item = history
        .iter()
        .find(|i| i["kind"] == "input" && i["id"] == input.to_string())
        .expect("input in the feed");
    assert_eq!(
        input_item["attachments"][0]["attachment_id"],
        attachment.to_string()
    );
    assert_eq!(
        input_item["attachments"][0]["sha256"],
        sha_hex(&bytes).as_str()
    );
    let execution: Uuid = sqlx::query_scalar(
        "select id from executions where conversation_id=$1 order by generation desc limit 1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let published = p
        .config
        .data_dir
        .join("executions")
        .join(execution.to_string())
        .join("attachments")
        .join(canterlot_worker::attachments::safe_file_name(
            attachment,
            "notes.txt",
        ));
    assert_eq!(fs::read(&published).unwrap(), bytes);
    assert!(!published.starts_with(&p.work));

    // Снимок и хвост, как у PWA: граница снимка, затем подписка строго после неё.
    let snapshot = p.get(&format!("/api/v1/conversations/{id}/snapshot")).await;
    assert_eq!(snapshot["scope"], chat.as_str());
    let boundary = seq(&snapshot["cursor"]);
    let items = snapshot["data"]["history"]["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["id"] == input.to_string()));
    assert!(items.iter().any(|i| i["id"] == child["block_id"]));
    let list = p.get("/api/v1/snapshot/conversations").await;
    assert_eq!(list["scope"], "conversations");
    let item = list["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id.to_string())
        .expect("conversation in the list snapshot")
        .clone();
    assert_eq!(item["machine"], json!({"id": p.machine, "name": "pilot"}));
    assert_eq!(item["activity"]["state"], "idle");
    ws.subscribe_after("chat_tail", &chat, boundary).await;
    assert!(p.detail(id).await["unread_count"].as_i64().unwrap() > 0);
    let read = p
        .api
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/read"),
            None,
            Some(json!({"seq": boundary.to_string()})),
        )
        .await;
    assert_eq!(read.status, 204);
    ws.wait_event(
        "unread cleared in the list",
        "conversations",
        "derived_changed",
        |d| d["conversation_id"] == id.to_string() && d["unread_count"] == 0,
    )
    .await;
    until("unread cleared in the tail", async || {
        ws.frames().iter().any(|f| {
            f["id"] == "chat_tail"
                && f["event"]["kind"] == "derived_changed"
                && f["event"]["payload"]["unread_count"] == 0
        })
    })
    .await;
    assert_eq!(p.detail(id).await["unread_count"], 0);

    check_streams(&pool, p.owner, &ws).await;
    assert_eq!(ws.connections, 2);
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn crunch_five_hour_end_to_end(pool: PgPool) {
    let _permit = e2e_gate::acquire().await;
    let p = Pilot::new(&pool, PilotOptions::default()).await;
    let mut ws = p.ws().await;
    ws.subscribe("list", "conversations").await;
    let id = p.create("synthetic", true).await;
    let chat = format!("conversation:{id}");
    ws.subscribe("chat", &chat).await;
    let detail = p.detail(id).await;
    let resume_text = LimitsConfig::default().default_resume_text;
    assert_eq!(detail["crunch"]["enabled"], true);
    assert_eq!(detail["crunch"]["resume_text"], resume_text.as_str());
    let cli = p.cli(id).await.expect("CLI of the live execution");

    let submitted = p
        .submit(
            id,
            &script(json!([
                {"delta": "Работаю"},
                {"limit_reached": {"window": "five_hour", "resets_in_ms": 2000}}
            ])),
            &[],
        )
        .await;
    let input = uuid(&submitted["input_id"]);
    p.accepted(input).await;
    ws.wait_event("crunch_held over WSS", &chat, "crunch_held", |h| {
        h["reason"] == "quota_exhausted" && h["window"] == "five_hour" && h["provider"] == "claude"
    })
    .await;
    ws.wait_event("limit_reached over WSS", &chat, "limit_reached", |_| true)
        .await;
    let held = p.detail(id).await;
    assert_eq!(held["crunch"]["resume_pending"], true);
    let hold = held["holds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["reason"] == "quota_exhausted")
        .expect("quota hold in the detail")
        .clone();
    assert_eq!(hold["provider"], "claude");
    assert_eq!(hold["window"], "five_hour");
    let (job, wake_run_at): (Uuid, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "select id,run_at from scheduled_jobs where conversation_id=$1 and kind='wake' and state='pending'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    until("activity quota_paused", async || {
        p.detail(id).await["activity"]["state"] == "quota_paused"
    })
    .await;

    // Сброс по часам прошёл, но без свежего показания пробуждение не подтверждено.
    until("wake retried with a poll", async || {
        let attempts: i32 = sqlx::query_scalar("select attempts from scheduled_jobs where id=$1")
            .bind(job)
            .fetch_one(&pool)
            .await
            .unwrap();
        let polled: bool =
            sqlx::query_scalar("select exists(select 1 from limit_polls where provider='claude')")
                .fetch_one(&pool)
                .await
                .unwrap();
        attempts >= 1 && polled
    })
    .await;
    assert!(chrono::Utc::now() > wake_run_at);
    assert!(ws.payloads(&chat, "crunch_woken").is_empty());
    assert!(p.wake_inputs(id).await.is_empty());
    assert!(
        p.detail(id).await["holds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["reason"] == "quota_exhausted")
    );

    p.limits_file(
        "claude",
        json!({"five_hour": {"used_permille": 120, "resets_at_ms": now_ms() + 5 * 3_600_000}}),
    );
    ws.wait_event(
        "crunch_woken with the resume input",
        &chat,
        "crunch_woken",
        |w| w["input_id"].is_string() && w["skipped"].is_null(),
    )
    .await;
    let polled: i64 = sqlx::query_scalar(
        "select count(*) from limit_snapshots where provider='claude' and window_kind='five_hour' and source='poll' and used_permille=120",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(polled >= 1);
    let wakes = p.wake_inputs(id).await;
    assert_eq!(wakes.len(), 1);
    let (resume, resume_command, wake_key) = wakes[0].clone();
    assert_eq!(wake_key, format!("wake:{job}"));
    p.accepted(resume).await;
    p.answered(id, resume_command, &resume_text).await;
    let after = p.detail(id).await;
    assert_eq!(after["crunch"]["resume_pending"], false);
    assert!(
        !after["holds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["reason"] == "quota_exhausted")
    );
    until("activity idle after the resume", async || {
        p.detail(id).await["activity"]["state"] == "idle"
    })
    .await;
    sleep(ms(500)).await;
    assert_eq!(p.wake_inputs(id).await.len(), 1);
    // Лимит и пробуждение не трогают процесс: тот же CLI ответил на текст продолжения.
    assert_eq!(p.cli(id).await, Some(cli));
    let executions: i64 =
        sqlx::query_scalar("select count(*) from executions where conversation_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(executions, 1);
    check_streams(&pool, p.owner, &ws).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn weekly_reserve_end_to_end(pool: PgPool) {
    let _permit = e2e_gate::acquire().await;
    let p = Pilot::new(&pool, PilotOptions::default()).await;
    let mut ws = p.ws().await;
    ws.subscribe("list", "conversations").await;
    let a = p.create("synthetic", true).await;
    let b = p.create("synthetic", true).await;
    let c = p.create("synthetic-codex", false).await;
    let scope = |id: Uuid| format!("conversation:{id}");
    for (name, id) in [("a", a), ("b", b), ("c", c)] {
        ws.subscribe(name, &scope(id)).await;
    }
    let holds = async |id: Uuid| -> Vec<String> {
        p.detail(id).await["holds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["reason"].as_str().unwrap().to_owned())
            .collect()
    };

    let a_cli = p.cli(a).await.expect("CLI of A");
    // Недельное показание 96 % посреди хода A; ход доходит до конца, затем лимит пяти часов.
    let first = p
        .submit(
            a,
            &script(json!([
                {"delta": "a1 "},
                {"limit_observed": {"window": "weekly", "used_permille": 960, "resets_in_ms": 3_600_000}},
                {"delay_ms": 700},
                {"delta": "a2"},
                {"limit_reached": {"window": "five_hour", "resets_in_ms": 1000}}
            ])),
            &[],
        )
        .await;
    let first_input = uuid(&first["input_id"]);
    let first_command = uuid(&first["command_id"]);
    until("weekly reserve active for claude only", async || {
        let limits = p.get("/api/v1/limits").await;
        limits
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["provider"] == "claude" && l["weekly_reserve_active"] == true)
    })
    .await;
    assert!(
        !p.get("/api/v1/limits")
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["provider"] == "codex" && l["weekly_reserve_active"] == true)
    );
    for id in [a, b] {
        ws.wait_event("weekly hold over WSS", &scope(id), "crunch_held", |h| {
            h["reason"] == "weekly_reserve" && h["provider"] == "claude"
        })
        .await;
    }
    ws.wait_event(
        "limits_changed in the list",
        "conversations",
        "limits_changed",
        |l| {
            l["provider"] == "claude"
                && l["window"] == "weekly"
                && l["weekly_reserve_active"] == true
        },
    )
    .await;
    p.accepted(first_input).await;
    p.answered(a, first_command, "a1 a2").await;
    ws.wait_event("A's turn completed", &scope(a), "run_finished", |r| {
        r["input_command_id"] == first_command.to_string()
    })
    .await;
    ws.wait_event(
        "A cut by the five-hour limit",
        &scope(a),
        "crunch_held",
        |h| h["reason"] == "quota_exhausted" && h["resume_pending"] == true,
    )
    .await;
    assert!(holds(a).await.contains(&"weekly_reserve".to_owned()));
    assert!(holds(b).await.contains(&"weekly_reserve".to_owned()));
    assert!(holds(c).await.is_empty());
    assert!(ws.payloads(&scope(c), "crunch_held").is_empty());
    ws.wait_event(
        "weekly hold of B in the list item",
        "conversations",
        "derived_changed",
        |d| {
            d["conversation_id"] == b.to_string()
                && d["queue"]["hold_reasons"]
                    .as_array()
                    .is_some_and(|h| h.iter().any(|h| h["reason"] == "weekly_reserve"))
        },
    )
    .await;

    let codex = p.submit(c, "codex работает", &[]).await;
    p.answered(c, uuid(&codex["command_id"]), "codex работает")
        .await;

    let waiting = p.submit(b, "ждёт резерва", &[]).await;
    let waiting_input = uuid(&waiting["input_id"]);
    sleep(Duration::from_secs(1)).await;
    assert_eq!(p.input_state(waiting_input).await, "queued");
    let b_detail = p.detail(b).await;
    assert_eq!(b_detail["activity"]["state"], "quota_paused");
    assert_eq!(b_detail["activity"]["evidence"]["reason"], "weekly_reserve");

    let spent = p
        .post(
            &format!("/api/v1/conversations/{b}/spend-reserve"),
            None,
            200,
        )
        .await;
    assert_eq!(spent, json!("continued"));
    p.answered(b, uuid(&waiting["command_id"]), "ждёт резерва")
        .await;
    let again = p
        .post(
            &format!("/api/v1/conversations/{b}/spend-reserve"),
            None,
            200,
        )
        .await;
    assert_eq!(again, json!("already_continued"));
    assert!(p.detail(b).await["crunch"]["reserve_override_until"].is_string());
    let a_holds = holds(a).await;
    assert!(
        a_holds.contains(&"weekly_reserve".to_owned()),
        "{a_holds:?}"
    );
    assert!(p.wake_inputs(a).await.is_empty());

    // Новая неделя по показанию провайдера: резерв снят, A будится текстом продолжения.
    p.limits_file(
        "claude",
        json!({
            "five_hour": {"used_permille": 50, "resets_at_ms": now_ms() + 5 * 3_600_000},
            "weekly": {"used_permille": 100, "resets_at_ms": now_ms() + 7 * 24 * 3_600_000}
        }),
    );
    until("one resume input for A", async || {
        p.wake_inputs(a).await.len() == 1
    })
    .await;
    let (resume, resume_command, _) = p.wake_inputs(a).await[0].clone();
    p.accepted(resume).await;
    let resume_text = LimitsConfig::default().default_resume_text;
    p.answered(a, resume_command, &resume_text).await;
    until("reserve left", async || {
        p.get("/api/v1/limits")
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["provider"] == "claude" && l["weekly_reserve_active"] == false)
    })
    .await;
    assert!(holds(a).await.is_empty());
    assert!(holds(b).await.is_empty());
    let a_detail = p.detail(a).await;
    assert_eq!(a_detail["crunch"]["resume_pending"], false);
    let b_detail = p.detail(b).await;
    assert!(b_detail["crunch"]["reserve_override_until"].is_null());
    ws.wait_event(
        "override cleared over WSS",
        &scope(b),
        "crunch_changed",
        |c| c["reserve_override_until"].is_null(),
    )
    .await;
    sleep(ms(600)).await;
    assert_eq!(p.wake_inputs(a).await.len(), 1);
    assert!(p.wake_inputs(b).await.is_empty());
    assert!(p.wake_inputs(c).await.is_empty());
    assert_eq!(
        p.cli(a).await,
        Some(a_cli),
        "A was continued by its own CLI"
    );
    check_streams(&pool, p.owner, &ws).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn machine_metrics_and_stalled_activity_over_wss(pool: PgPool) {
    let _permit = e2e_gate::acquire().await;
    let interval = ms(500);
    let p = Pilot::new(
        &pool,
        PilotOptions {
            activity: ActivityConfig {
                stalled_generating: Duration::from_secs(1),
                stalled_tool: Duration::from_secs(1),
                tick: ms(100),
                debounce: ms(50),
            },
            metrics_interval: interval,
            ..Default::default()
        },
    )
    .await;
    let mut ws = p.ws().await;
    ws.subscribe("machines", "machines").await;
    ws.subscribe("list", "conversations").await;

    let head: Option<i64> = sqlx::query_scalar("select max(scope_seq) from events where scope=$1")
        .bind(internal_scope(p.owner, "machines"))
        .fetch_one(&pool)
        .await
        .unwrap();
    until("machines catch-up", async || {
        ws.cursor("machines") >= head.unwrap_or(0)
    })
    .await;
    let start = Instant::now();
    let seen = ws.payloads("machines", "machine_metrics").len();
    until("first machine_metrics over WSS", async || {
        ws.payloads("machines", "machine_metrics").len() > seen
    })
    .await;
    assert!(
        start.elapsed() < interval * 4,
        "first metrics after {:?}",
        start.elapsed()
    );
    until("four machine_metrics over WSS", async || {
        ws.payloads("machines", "machine_metrics").len() >= seen + 4
    })
    .await;
    let committed: Vec<chrono::DateTime<chrono::Utc>> = ws
        .events("machines")
        .iter()
        .filter(|e| e["kind"] == "machine_metrics")
        .map(|e| e["committed_at"].as_str().unwrap().parse().unwrap())
        .collect();
    for pair in committed[seen..].windows(2) {
        let gap = (pair[1] - pair[0]).to_std().unwrap();
        assert!(
            gap < interval * 4,
            "metrics gap {gap:?} for interval {interval:?}"
        );
    }
    let sample = ws.payloads("machines", "machine_metrics").pop().unwrap();
    assert_eq!(sample["machine_id"], p.machine.to_string());
    assert!(sample["cpu_cores"].as_u64().unwrap() > 0);
    assert!(sample["mem_total_bytes"].as_u64().unwrap() > 0);
    assert!(!sample["disks"].as_array().unwrap().is_empty());
    let machine = p.get(&format!("/api/v1/machines/{}", p.machine)).await;
    assert!(machine["metrics"].is_object());
    assert!(
        !machine["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w == "metrics_stale")
    );

    let id = p.create("synthetic", false).await;
    let chat = format!("conversation:{id}");
    ws.subscribe("chat", &chat).await;
    let submitted = p
        .submit(
            id,
            &script(json!([{"delta": "думаю"}, {"hang": true}])),
            &[],
        )
        .await;
    let input = uuid(&submitted["input_id"]);
    let command = uuid(&submitted["command_id"]);
    p.accepted(input).await;
    let cli = p.cli(id).await.expect("CLI of the live execution");
    let queue = async || -> (Value, Value, String) {
        sqlx::query_as::<_, (sqlx::types::Json<Value>, sqlx::types::Json<Value>, String)>(
            "select pause_reasons,hold_reasons,run_slot from queue_state where conversation_id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .map(|(p, h, s)| (p.0, h.0, s))
        .unwrap()
    };
    let before = queue().await;
    for scope in [chat.as_str(), "conversations"] {
        ws.wait_event("stalled over WSS", scope, "derived_changed", |d| {
            d["conversation_id"] == id.to_string() && d["activity"]["state"] == "stalled"
        })
        .await;
    }
    let stalled = ws
        .payloads(&chat, "derived_changed")
        .into_iter()
        .find(|d| d["activity"]["state"] == "stalled")
        .unwrap();
    assert_eq!(stalled["activity"]["evidence"]["phase"], "generating");
    assert_eq!(stalled["activity"]["evidence"]["link"], "available");
    // `stalled` - только подозрение: исполнение, ввод и очередь не тронуты.
    sleep(Duration::from_secs(1)).await;
    assert_eq!(p.detail(id).await["execution"]["state"], "running");
    assert_eq!(p.input_state(input).await, "accepted");
    assert_eq!(queue().await, before);
    let interrupts: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id=$1 and kind in ('interrupt','stop_execution','withdraw_input')",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(interrupts, 0);
    assert_eq!(p.cli(id).await, Some(cli), "the stalled CLI keeps running");

    let stop = p
        .post(&format!("/api/v1/conversations/{id}/interrupt"), None, 200)
        .await;
    assert!(stop["requested"]["interrupt_command_id"].is_string());
    let interrupts: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id=$1 and kind='interrupt'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(interrupts, 1);
    ws.wait_event(
        "interrupt progress over WSS",
        &chat,
        "interrupt_state_changed",
        |_| true,
    )
    .await;
    ws.wait_event("run finished after interrupt", &chat, "run_finished", |r| {
        r["input_command_id"] == command.to_string()
    })
    .await;
    until("activity leaves stalled", async || {
        states(&ws.payloads(&chat, "derived_changed"), id).last() == Some(&"idle".to_owned())
    })
    .await;
    assert_eq!(p.detail(id).await["execution"]["state"], "running");
    assert_eq!(p.cli(id).await, Some(cli), "interrupt keeps the process");
    check_streams(&pool, p.owner, &ws).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn scheduled_message_end_to_end(pool: PgPool) {
    let _permit = e2e_gate::acquire().await;
    let p = Pilot::new(&pool, PilotOptions::default()).await;
    let mut ws = p.ws().await;
    ws.subscribe("list", "conversations").await;
    let id = p.create("synthetic", false).await;
    let chat = format!("conversation:{id}");
    ws.subscribe("chat", &chat).await;

    let run_at = chrono::Utc::now() + chrono::TimeDelta::seconds(2);
    let wire_time = run_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let scheduled = p
        .post(
            &format!("/api/v1/conversations/{id}/scheduled"),
            Some(json!({"run_at": wire_time, "content": [{"type":"text","text":"Отложенное сообщение"}]})),
            201,
        )
        .await;
    let job = uuid(&scheduled["id"]);
    assert_eq!(scheduled["state"], "pending");
    let cancelled = p
        .post(
            &format!("/api/v1/conversations/{id}/scheduled"),
            Some(json!({"run_at": wire_time, "content": [{"type":"text","text":"Не отправится"}]})),
            201,
        )
        .await;
    let cancelled_id = uuid(&cancelled["id"]);
    let response = p
        .call(
            Method::DELETE,
            &format!("/api/v1/scheduled/{cancelled_id}"),
            None,
        )
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json::<Value>()["state"], "cancelled");
    let listed = p
        .get(&format!("/api/v1/conversations/{id}/scheduled"))
        .await;
    let states: BTreeMap<String, String> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["id"].as_str().unwrap().to_owned(),
                m["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(states[&job.to_string()], "pending");
    assert_eq!(states[&cancelled_id.to_string()], "cancelled");
    assert!(p.history(id).await.iter().all(|i| i["kind"] != "input"));

    ws.wait_event("scheduled message fired", &chat, "scheduled_changed", |m| {
        m["id"] == job.to_string() && m["state"] == "fired" && m["input_id"].is_string()
    })
    .await;
    let (input, command, key, created): (Uuid, Uuid, String, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as(
            "select id,command_id,idempotency_key,created_at from inputs where conversation_id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(key, format!("sched:{job}"));
    assert!(created >= run_at - chrono::TimeDelta::milliseconds(1));
    p.accepted(input).await;
    p.answered(id, command, "Отложенное сообщение").await;
    let fired = p
        .get(&format!("/api/v1/conversations/{id}/scheduled"))
        .await;
    let fired = fired
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == job.to_string())
        .unwrap()
        .clone();
    assert_eq!(fired["state"], "fired");
    assert_eq!(fired["input_id"], input.to_string());
    sleep(ms(500)).await;
    let inputs: i64 = sqlx::query_scalar("select count(*) from inputs where conversation_id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(inputs, 1, "the cancelled message never fired");
    assert!(
        ws.payloads(&chat, "scheduled_changed")
            .iter()
            .any(|m| m["id"] == cancelled_id.to_string() && m["state"] == "cancelled")
    );
    check_streams(&pool, p.owner, &ws).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn internal_kinds_never_leave_the_hub_over_wss_in_the_full_flow(pool: PgPool) {
    let _permit = e2e_gate::acquire().await;
    let p = Pilot::new(&pool, PilotOptions::default()).await;
    let mut ws = p.ws().await;
    ws.subscribe("list", "conversations").await;
    ws.subscribe("machines", "machines").await;
    let id = p.create("synthetic", false).await;
    let chat = format!("conversation:{id}");
    ws.subscribe("chat", &chat).await;

    let attachment = p.upload(id, "data.bin", b"pilot attachment").await;
    let first_cli = p.cli(id).await.expect("CLI of the first execution");
    let first = p
        .submit(
            id,
            &script(json!([
                {"delta": "Читаю "},
                {"tool": {"name": "Bash", "summary": "cargo test"}},
                {"subagent": {"name": "Explore", "model": "synthetic-small", "task": "Поиск", "steps": [{"delta": "ищу"}]}},
                {"read_attachment": 0}
            ])),
            &[attachment],
        )
        .await;
    p.accepted(uuid(&first["input_id"])).await;
    until("first run finished", async || {
        ws.payloads(&chat, "run_finished")
            .iter()
            .any(|r| r["input_command_id"] == first["command_id"])
    })
    .await;
    let stop = p
        .post(
            &format!("/api/v1/conversations/{id}/stop"),
            Some(json!({"force": false})),
            200,
        )
        .await;
    assert!(stop["requested"]["command_id"].is_string());
    let first_execution: Uuid = sqlx::query_scalar(
        "select id from executions where conversation_id=$1 order by generation desc limit 1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    until("execution stopped", async || {
        sqlx::query_scalar::<_, String>("select state from executions where id=$1")
            .bind(first_execution)
            .fetch_one(&pool)
            .await
            .unwrap()
            == "stopped"
    })
    .await;
    until("supervisor and CLI exited", async || {
        p.guard.members().is_empty()
    })
    .await;
    until("journal closed recorded", async || {
        sqlx::query_scalar::<_, bool>(
            "select exists(select 1 from events where scope=$1 and kind='journal_closed')",
        )
        .bind(&chat)
        .fetch_one(&pool)
        .await
        .unwrap()
    })
    .await;
    ws.reconnect().await;
    let paused = p.detail(id).await["queue"]["pause_reasons"].clone();
    assert!(
        paused
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["reason"] == "user_stop" || r == "user_stop"),
        "stop pauses the queue: {paused}"
    );
    let resumed = p
        .post(
            &format!("/api/v1/conversations/{id}/resume"),
            Some(json!({"continue_queue": true})),
            201,
        )
        .await;
    assert!(resumed["execution_id"].is_string());
    until("resumed execution running", async || {
        p.detail(id).await["execution"]["state"] == "running"
    })
    .await;
    let second = p.submit(id, "после возобновления", &[]).await;
    p.answered(id, uuid(&second["command_id"]), "после возобновления")
        .await;
    let second_cli = p.cli(id).await.expect("CLI of the resumed execution");
    assert_ne!(second_cli, first_cli);
    let native: Vec<Option<String>> = sqlx::query_scalar(
        "select native_conversation_id from executions where conversation_id=$1 order by generation",
    )
    .bind(id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(native.len(), 2);
    assert!(native[0].is_some());
    assert_eq!(
        native[0], native[1],
        "resume continues the native conversation"
    );

    let hidden = check_streams(&pool, p.owner, &ws).await;
    let internal: Vec<String> =
        sqlx::query_scalar("select distinct kind from events where scope=any($1) order by kind")
            .bind(
                ws.subscriptions
                    .iter()
                    .map(|(_, scope, _)| internal_scope(p.owner, scope))
                    .collect::<Vec<_>>(),
            )
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .filter(|kind: &String| !is_client_visible(kind))
            .collect();
    assert!(
        internal.contains(&"journal_closed".to_owned()),
        "the flow produced internal kinds: {internal:?}"
    );
    assert!(hidden >= internal.len());
    let leaked: Vec<String> = ws
        .frames()
        .iter()
        .filter(|f| f["op"] == "event")
        .flat_map(|f| {
            let text = f.to_string();
            internal
                .iter()
                .filter(move |kind| text.contains(&format!("\"{kind}\"")))
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "internal kinds in WSS frames: {leaked:?}"
    );
    assert_eq!(ws.connections, 2);
}

/// PNG с заголовком IHDR: тип и размеры определяются по сигнатуре и заголовку.
fn png(width: u32, height: u32, tail: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend((0..tail).map(|i| (i % 253) as u8));
    bytes
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn view_image_step_shows_media_on_the_tool_block_end_to_end(pool: PgPool) {
    let _permit = e2e_gate::acquire().await;
    let p = Pilot::new(&pool, PilotOptions::default()).await;
    let mut ws = p.ws().await;
    let id = p.create("synthetic", false).await;
    let chat = format!("conversation:{id}");
    ws.subscribe("chat", &chat).await;
    let bytes = png(3, 2, 70_000);
    fs::write(p.work.join("screen.png"), &bytes).unwrap();
    let input = p
        .submit(
            id,
            &script(json!([{"view_image": "screen.png"}, {"delta": "Вижу картинку"}])),
            &[],
        )
        .await;
    let command = uuid(&input["command_id"]);
    p.answered(id, command, "Вижу картинку").await;
    ws.wait_event(
        "media attached over WSS",
        &chat,
        "block_media_attached",
        |_| true,
    )
    .await;

    // WSS: блок `Read` с путём, затем ссылка на опубликованную картинку того же блока.
    let events = ws.events(&chat);
    let started = events
        .iter()
        .find(|e| {
            e["kind"] == "block_started"
                && e["payload"]["kind"] == "tool_call"
                && e["payload"]["tool_name"] == "Read"
        })
        .expect("tool block started")
        .clone();
    assert_eq!(started["payload"]["summary"], "screen.png");
    assert_eq!(started["payload"]["input_command_id"], command.to_string());
    let block = uuid(&started["payload"]["block_id"]);
    let attached = events
        .iter()
        .find(|e| e["kind"] == "block_media_attached")
        .unwrap()
        .clone();
    assert!(seq(&attached["scope_seq"]) > seq(&started["scope_seq"]));
    let media = uuid(&attached["payload"]["media_id"]);
    assert_eq!(
        attached["payload"],
        json!({"block_id": block, "media_id": media, "mime": "image/png", "size": bytes.len(), "width": 3, "height": 2})
    );
    assert_eq!(
        ws.payloads(&chat, "block_media_attached").len(),
        1,
        "one reference per image"
    );

    // Модели чтения: история и снимок несут картинку на блоке инструмента.
    let expected = json!([{"media_id": media, "mime": "image/png", "size": bytes.len(), "width": 3, "height": 2}]);
    let history = p.history(id).await;
    let tool = history
        .iter()
        .find(|i| i["kind"] == "block" && i["id"] == block.to_string())
        .expect("tool block in history");
    assert_eq!(tool["block_kind"], "tool_call");
    assert_eq!(tool["tool_name"], "Read");
    assert_eq!(tool["summary"], "screen.png");
    assert_eq!(tool["media"], expected);
    let result = history
        .iter()
        .find(|i| i["kind"] == "block" && i["parent_block_id"] == block.to_string())
        .expect("tool result in history");
    assert_eq!(result["content"], format!("{} bytes", bytes.len()));
    assert_eq!(result["media"], json!([]));
    let snapshot = p.get(&format!("/api/v1/conversations/{id}/snapshot")).await;
    let in_snapshot = snapshot["data"]["history"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == block.to_string())
        .unwrap()
        .clone();
    assert_eq!(in_snapshot["media"], expected);

    // Выдача браузеру: проверенный тип, показ на месте, ETag равен SHA-256.
    let response = p
        .api
        .send(Method::GET, &format!("/api/v1/media/{media}"), None, None)
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body.as_ref(), bytes.as_slice());
    assert_eq!(response.headers["content-type"], "image/png");
    assert_eq!(response.headers["x-content-type-options"], "nosniff");
    assert_eq!(response.headers["content-disposition"], "inline");
    assert_eq!(response.headers["etag"], format!("\"{}\"", sha_hex(&bytes)));
    let execution: Uuid = sqlx::query_scalar(
        "select id from executions where conversation_id=$1 order by generation desc limit 1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let copies = p
        .config
        .data_dir
        .join("executions")
        .join(execution.to_string())
        .join("media");
    until("worker copy removed after publish", async || {
        fs::read_dir(&copies).map_or(true, |mut d| d.next().is_none())
    })
    .await;
    check_streams(&pool, p.owner, &ws).await;
}

/// Натуральные PID synthetic-CLI в рабочем каталоге.
fn synthetic_cli_pids(workdir: &std::path::Path) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if !cmdline
            .split(|&b| b == 0)
            .any(|arg| arg == b"synthetic-cli")
        {
            continue;
        }
        let Ok(cwd) = std::fs::read_link(format!("/proc/{pid}/cwd")) else {
            continue;
        };
        if cwd.as_path() != workdir {
            continue;
        }
        out.push(pid);
    }
    out.sort_unstable();
    out
}

/// Ход с echo-инструкциями; возвращает текст финального блока.
async fn echo_turn(pilot: &Pilot, conversation: Uuid, text: &str) -> (Uuid, String) {
    let submitted = pilot
        .post(
            &format!("/api/v1/conversations/{conversation}/inputs"),
            Some(json!({"content":[{"type":"text","text":text}]})),
            201,
        )
        .await;
    let input: Uuid = submitted["input_id"].as_str().unwrap().parse().unwrap();
    let command: Uuid = submitted["command_id"].as_str().unwrap().parse().unwrap();
    pilot.accepted(input).await;
    let mut out = String::new();
    until("echo final block", async || {
        for block in pilot.answer(conversation, command).await {
            if block["state"] == "final"
                && block["content"]
                    .as_str()
                    .is_some_and(|t| t.contains("instructions "))
            {
                out = block["content"].as_str().unwrap().into();
                return true;
            }
        }
        false
    })
    .await;
    (command, out)
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn preset_change_reaches_the_adapter_from_the_next_turn(pool: PgPool) {
    let _permit = e2e_gate::acquire().await;
    let p = Pilot::new(&pool, PilotOptions::default()).await;
    let preset = p
        .post(
            "/api/v1/presets",
            Some(json!({"title": "Исполнитель", "text": "текст A"})),
            201,
        )
        .await;
    let preset_id = preset["id"].as_str().unwrap();
    let created = p
        .post(
            "/api/v1/conversations",
            Some(json!({
                "machine_id": p.machine,
                "harness": "synthetic",
                "workdir": p.work.to_str().unwrap(),
                "model": "synthetic-large",
                "effort": "high",
                "instruction_profile": preset_id,
                "start": true,
            })),
            201,
        )
        .await;
    assert!(created["execution"]["execution_id"].is_string());
    let id: Uuid = created["conversation_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    until("execution running", async || {
        p.detail(id).await["execution"]["state"] == "running"
    })
    .await;
    // Ход 1: адаптер видит пресет, ревизия 1.
    let turn1 = json!({"synthetic": [{"echo_instructions": true}]});
    let (_cmd1, echo1) = echo_turn(&p, id, &turn1.to_string()).await;
    assert!(echo1.contains("mode=preset"), "{echo1}");
    assert!(echo1.contains("revision=1"), "{echo1}");
    let pids_before = synthetic_cli_pids(&p.work);
    assert!(!pids_before.is_empty(), "synthetic CLI runs");
    // Правка пресета без повторного применения: следующий ход держит ревизию 1.
    p.call(
        Method::PATCH,
        &format!("/api/v1/presets/{preset_id}"),
        Some(json!({"text": "текст B", "base_revision": 1})),
    )
    .await;
    let turn2 = json!({"synthetic": [{"echo_instructions": true}]});
    let (_cmd2, echo2) = echo_turn(&p, id, &turn2.to_string()).await;
    assert!(echo2.contains("revision=1"), "{echo2}");
    // Повторное применение той же ссылки: перезапуск с ревизией 2 и новым pid.
    p.call(
        Method::PATCH,
        &format!("/api/v1/conversations/{id}/settings"),
        Some(json!({"instruction_profile": preset_id})),
    )
    .await;
    let turn3 = json!({"synthetic": [{"echo_instructions": true}]});
    let (_cmd3, echo3) = echo_turn(&p, id, &turn3.to_string()).await;
    assert!(echo3.contains("revision=2"), "{echo3}");
    let pids_after = synthetic_cli_pids(&p.work);
    assert!(
        !pids_after.iter().any(|pid| pids_before.contains(pid)),
        "CLI restarted: before={pids_before:?} after={pids_after:?}"
    );
    assert!(!pids_after.is_empty());
    // «Без инструкций»: чистый запуск без текста.
    p.call(
        Method::PATCH,
        &format!("/api/v1/conversations/{id}/settings"),
        Some(json!({"instruction_profile": "clean"})),
    )
    .await;
    let turn4 = json!({"synthetic": [{"echo_instructions": true}]});
    let (_cmd4, echo4) = echo_turn(&p, id, &turn4.to_string()).await;
    assert!(echo4.contains("mode=clean"), "{echo4}");
}

/// Агент через синтетический вызов применяет пресет к своей сессии:
/// вызов `presets.apply` ставит подвешенную смену, следующий ход видит ревизию.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn synthetic_agent_call_applies_a_preset_end_to_end(pool: PgPool) {
    let _permit = e2e_gate::acquire().await;
    let p = Pilot::new(&pool, PilotOptions::default()).await;
    let preset = p
        .post(
            "/api/v1/presets",
            Some(json!({"title": "Агентский", "text": "текст агента"})),
            201,
        )
        .await;
    let preset_id = preset["id"].as_str().unwrap();
    let created = p
        .post(
            "/api/v1/conversations",
            Some(json!({
                "machine_id": p.machine,
                "harness": "synthetic",
                "workdir": p.work.to_str().unwrap(),
                "model": "synthetic-large",
                "effort": "high",
                "start": true,
            })),
            201,
        )
        .await;
    let id: Uuid = created["conversation_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    until("execution running", async || {
        p.detail(id).await["execution"]["state"] == "running"
    })
    .await;
    // Ход 1: агент вызывает presets.apply к своей сессии.
    let call = json!({"synthetic": [{"agent_call": {"tool": "presets.apply", "arguments": {"id": preset_id}}}]});
    let submitted = p
        .post(
            &format!("/api/v1/conversations/{id}/inputs"),
            Some(json!({"content":[{"type":"text","text":call.to_string()}]})),
            201,
        )
        .await;
    let input: Uuid = submitted["input_id"].as_str().unwrap().parse().unwrap();
    p.accepted(input).await;
    // Блок с итогом вызова не привязан к команде ввода.
    until("agent call answered", async || {
        p.history(id).await.iter().any(|item| {
            item["kind"] == "block"
                && item["block_kind"] == "text"
                && item["state"] == "final"
                && item["content"]
                    .as_str()
                    .is_some_and(|t| t.contains("next_turn"))
        })
    })
    .await;
    // Ход 2: подвешенная смена применена, адаптер видит пресет.
    let turn = json!({"synthetic": [{"echo_instructions": true}]});
    let (_cmd, echo) = echo_turn(&p, id, &turn.to_string()).await;
    assert!(echo.contains("mode=preset"), "{echo}");
    assert!(echo.contains("revision=1"), "{echo}");
}
