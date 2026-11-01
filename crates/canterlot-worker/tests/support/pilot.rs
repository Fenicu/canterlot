//! Стенд сквозных сценариев: настоящий хаб и воркер, браузер через HTTP и WSS.
#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fs,
    net::SocketAddr,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
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

use crate::process_guard::ProcessGuard;

pub const WAIT: Duration = Duration::from_secs(40);

pub fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

pub async fn until(what: &str, mut condition: impl AsyncFnMut() -> bool) {
    timeout(WAIT, async {
        while !condition().await {
            sleep(ms(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"));
}

pub fn seq(value: &Value) -> i64 {
    value
        .as_str()
        .expect("seq is a decimal string")
        .parse()
        .expect("decimal seq")
}

pub fn new_key() -> String {
    Uuid::now_v7().simple().to_string()
}

pub fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn uuid(value: &Value) -> Uuid {
    value.as_str().expect("uuid string").parse().expect("uuid")
}

pub fn script(steps: Value) -> String {
    json!({ "synthetic": steps }).to_string()
}

// ---------------------------------------------------------------- WSS

/// Одно соединение WSS: кадры складываются в общий журнал записи по порядку прихода.
pub struct Connection {
    pub ops: mpsc::UnboundedSender<Value>,
    pub task: JoinHandle<()>,
}

/// Клиент WSS, переживающий переподключение: подписки возобновляются с курсора последнего кадра.
pub struct Recorder {
    pub addr: SocketAddr,
    pub frames: Arc<Mutex<Vec<Value>>>,
    /// Подписки: id, область и курсор, с которого подписка началась.
    pub subscriptions: Vec<(String, String, i64)>,
    pub connection: Option<Connection>,
    pub connections: usize,
}

impl Recorder {
    pub async fn connect(addr: SocketAddr) -> Self {
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

    pub async fn open(&mut self) {
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

    pub fn send(&self, op: Value) {
        self.connection
            .as_ref()
            .expect("open connection")
            .ops
            .send(op)
            .expect("WSS task alive");
    }

    /// Подписка с начала журнала области: сверка с базой видит каждый номер.
    pub async fn subscribe(&mut self, id: &str, scope: &str) {
        self.subscribe_after(id, scope, 0).await;
    }

    /// Подписка после границы снимка, как у PWA.
    pub async fn subscribe_after(&mut self, id: &str, scope: &str, after: i64) {
        self.subscriptions.push((id.into(), scope.into(), after));
        self.send(json!({"op":"subscribe","id":id,"scope":scope,"after":after.to_string()}));
        self.wait(&format!("subscribed {id}"), |frames| {
            frames
                .iter()
                .any(|f| f["op"] == "subscribed" && f["id"] == id)
        })
        .await;
    }

    pub fn frames(&self) -> Vec<Value> {
        self.frames.lock().unwrap().clone()
    }

    pub fn cursor(&self, id: &str) -> i64 {
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
    pub fn events(&self, scope: &str) -> Vec<Value> {
        self.frames()
            .into_iter()
            .filter(|f| f["op"] == "event" && f["event"]["scope"] == scope)
            .map(|f| f["event"].clone())
            .collect()
    }

    pub fn payloads(&self, scope: &str, kind: &str) -> Vec<Value> {
        self.events(scope)
            .into_iter()
            .filter(|e| e["kind"] == kind)
            .map(|e| e["payload"].clone())
            .collect()
    }

    pub async fn wait(&self, what: &str, condition: impl Fn(&[Value]) -> bool) {
        until(what, async || condition(&self.frames())).await;
    }

    pub async fn wait_event(
        &self,
        what: &str,
        scope: &str,
        kind: &str,
        check: impl Fn(&Value) -> bool,
    ) {
        until(what, async || self.payloads(scope, kind).iter().any(&check)).await;
    }

    pub async fn disconnect(&mut self) {
        let connection = self.connection.take().expect("open connection");
        drop(connection.ops);
        timeout(WAIT, connection.task)
            .await
            .expect("WSS task stops")
            .expect("WSS task");
    }

    /// Новое соединение и подписки с курсоров, достигнутых до обрыва.
    pub async fn reconnect(&mut self) {
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

pub fn server_op_validator() -> jsonschema::Validator {
    let mut schema: Value =
        serde_json::from_str(include_str!("../../../../docs/api/events.schema.json")).unwrap();
    schema["$ref"] = json!("#/$defs/ServerOp");
    schema.as_object_mut().unwrap().remove("oneOf");
    jsonschema::validator_for(&schema).unwrap()
}

pub fn internal_scope(owner: Uuid, scope: &str) -> String {
    match scope {
        "conversations" => format!("owner:{owner}:conversations"),
        "machines" => format!("owner:{owner}:machines"),
        other => other.to_owned(),
    }
}

/// Поток WSS против журнала: только клиентские виды и заглушки, номера каждой подписки
/// непрерывны от её начала и покрывают каждое событие базы ровно один раз; внутренние события
/// приходят только заглушками без payload. Возвращает число внутренних событий под заглушками.
pub async fn check_streams(pool: &PgPool, owner: Uuid, recorder: &Recorder) -> usize {
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

pub struct PilotOptions {
    pub limits: LimitsConfig,
    pub activity: ActivityConfig,
    pub metrics_interval: Duration,
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

pub type RunningWorker = (watch::Sender<bool>, JoinHandle<Result<(), WorkerError>>);

/// Настоящий хаб (HTTP и TLS gRPC), настоящая управляющая служба воркера, процессы
/// сопровождения и синтетический CLI; браузер - HTTP и WSS через сокет loopback.
pub struct Pilot {
    pub pool: PgPool,
    pub hub: Option<TestHub>,
    pub api: TestClient,
    pub owner: Uuid,
    pub machine: Uuid,
    pub config: WorkerConfig,
    pub work: PathBuf,
    pub worker: Option<RunningWorker>,
    pub guard: ProcessGuard,
    pub _root: TempDir,
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
    pub async fn new(pool: &PgPool, options: PilotOptions) -> Self {
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
                ..Default::default()
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
            opencode: None,
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

    pub fn hub(&self) -> &TestHub {
        self.hub.as_ref().unwrap()
    }

    pub async fn start_worker(&mut self) {
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

    pub async fn ws(&self) -> Recorder {
        Recorder::connect(self.hub().http_addr()).await
    }

    pub async fn get(&self, path: &str) -> Value {
        let response = self.api.send(Method::GET, path, None, None).await;
        assert_eq!(response.status, 200, "GET {path}");
        response.json()
    }

    pub async fn call(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        self.api.send(method, path, Some(&new_key()), body).await
    }

    pub async fn post(&self, path: &str, body: Option<Value>, status: u16) -> Value {
        let response = self.call(Method::POST, path, body).await;
        assert_eq!(response.status, status, "POST {path}: {:?}", response.body);
        if response.body.is_empty() {
            Value::Null
        } else {
            response.json()
        }
    }

    /// Сессия с запуском: живое исполнение в состоянии `running`.
    pub async fn create(&self, harness: &str, crunch: bool) -> Uuid {
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

    pub async fn detail(&self, conversation: Uuid) -> Value {
        self.get(&format!("/api/v1/conversations/{conversation}"))
            .await
    }

    pub async fn submit(&self, conversation: Uuid, text: &str, attachments: &[Uuid]) -> Value {
        self.post(
            &format!("/api/v1/conversations/{conversation}/inputs"),
            Some(json!({"content":[{"type":"text","text":text}],"attachment_ids":attachments})),
            201,
        )
        .await
    }

    pub async fn input_state(&self, input: Uuid) -> String {
        sqlx::query_scalar("select state from inputs where id=$1")
            .bind(input)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    pub async fn accepted(&self, input: Uuid) {
        until("input accepted", async || {
            self.input_state(input).await == "accepted"
        })
        .await;
    }

    pub async fn history(&self, conversation: Uuid) -> Vec<Value> {
        self.get(&format!(
            "/api/v1/conversations/{conversation}/history?limit=200"
        ))
        .await["items"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// Окончательные текстовые блоки ответа на ввод с данной командой.
    pub async fn answer(&self, conversation: Uuid, command: Uuid) -> Vec<Value> {
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

    pub async fn answered(&self, conversation: Uuid, command: Uuid, text: &str) {
        until("answer text final", async || {
            self.answer(conversation, command)
                .await
                .iter()
                .any(|b| b["state"] == "final" && b["content"] == text)
        })
        .await;
    }

    pub async fn upload(&self, conversation: Uuid, name: &str, bytes: &[u8]) -> Uuid {
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

    pub fn limits_file(&self, provider: &str, value: Value) {
        let directory = self.config.data_dir.join("synthetic-limits");
        fs::create_dir_all(&directory).unwrap();
        let temporary = directory.join(format!(".{provider}.tmp"));
        fs::write(&temporary, value.to_string()).unwrap();
        fs::rename(temporary, directory.join(format!("{provider}.json"))).unwrap();
    }

    /// Процесс синтетического CLI живого исполнения разговора: (pid, start ticks) потомка
    /// процесса сопровождения этого исполнения.
    pub async fn cli(&self, conversation: Uuid) -> Option<(u32, u64)> {
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

    pub async fn wake_inputs(&self, conversation: Uuid) -> Vec<(Uuid, Uuid, String)> {
        sqlx::query_as(
            "select id,command_id,idempotency_key from inputs where conversation_id=$1 and idempotency_key like 'wake:%' order by created_at",
        )
        .bind(conversation)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

pub fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis() as u64
}

pub fn states(payloads: &[Value], conversation: Uuid) -> Vec<String> {
    payloads
        .iter()
        .filter(|p| p["conversation_id"] == conversation.to_string())
        .map(|p| p["activity"]["state"].as_str().unwrap().to_owned())
        .collect()
}
