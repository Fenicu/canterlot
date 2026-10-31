#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

//! Боевой путь запуска OpenCode: конфигурация воркера из TOML, `ExecutionManager` с
//! `configure_adapters`, настоящий супервизор, спека на диске с `deny_unknown_fields`
//! и `fake-opencode` как CLI. Именно этот путь не исполнялся тестами, когда
//! `exec/manager.rs` падал из-за пропущенной настройки адаптеров.

use canterlot_proto::worker::v1::{
    Command, ContentBlock, DeliverDecision, DeliverInput, Instructions, Interrupt, InterruptState,
    SettingsOverride, StartExecution, StopExecution, TextBlock, command::Kind, content_block,
    record_body::Body,
};
use canterlot_worker::{
    adapter::opencode::replay::{Reply, Script, Step},
    config::WorkerConfig,
    exec::{ExecutionManager, SjRelay},
    session_store::StoreThread,
    store::{Accept, WorkerStore},
};
use rustix::process::{Pid, Signal, kill_process};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    time::Duration,
};
use tempfile::TempDir;
use tokio::{sync::mpsc, time::timeout};
use uuid::Uuid;

#[path = "support/process_guard.rs"]
mod process_guard;
use process_guard::ProcessGuard;

#[path = "support/opencode.rs"]
mod opencode;
use opencode::{Fake, find, path_of};

const WAIT: Duration = Duration::from_secs(20);

fn ready() -> Script {
    Script::from_capture("start_stream", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
}

struct Env {
    guard: ProcessGuard,
    fake: Option<Fake>,
    root: TempDir,
    work: PathBuf,
    thread: Option<StoreThread>,
    manager: Option<ExecutionManager>,
    relay: mpsc::Receiver<SjRelay>,
    bodies: Vec<Body>,
    seen: HashSet<(Uuid, u64)>,
    committed: HashMap<Uuid, u64>,
    execution: Uuid,
    conversation: Uuid,
}

impl Drop for Env {
    fn drop(&mut self) {
        drop(self.manager.take());
    }
}

fn worker_toml(root: &std::path::Path, work: &std::path::Path, table: &str) -> String {
    let id = Uuid::now_v7();
    format!(
        r#"
hub_url = "https://hub.test"
ca_cert = "{root}/ca.pem"
token_file = "{root}/token"
machine_id = "{id}"
installation_id = "{id}"
restore_generation = "{id}"
data_dir = "{root}/store"

[execution]
allowed_workdir_roots = ["{work}"]
supervisor_exe = "{exe}"
spawn_timeout = "10s"

[execution.supervisor]
stop_grace = "1s"
stop_retry = "200ms"

{table}
"#,
        root = root.display(),
        work = work.display(),
        exe = env!("CARGO_BIN_EXE_canterlot-worker"),
    )
}

impl Env {
    async fn new(script: Option<Script>) -> Self {
        let root = TempDir::new().unwrap();
        let work = root.path().join("work");
        fs::create_dir(&work).unwrap();
        let fake = script.map(Fake::new);
        let table = fake.as_ref().map_or(String::new(), |fake| {
            format!(
                "[opencode]\nexecutable = \"{}\"\nowner_config = \"{}\"\n",
                fake.exe.display(),
                root.path().join("owner-config").display()
            )
        });
        let config: WorkerConfig =
            toml::from_str(&worker_toml(root.path(), &work, &table)).unwrap();
        let thread = StoreThread::new(WorkerStore::open(&root.path().join("store")).unwrap());
        let (sender, relay) = mpsc::channel(64);
        let manager = ExecutionManager::new(config.execution.clone(), thread.client(), sender);
        manager.configure_adapters(config.claude_code, config.codex, config.opencode);
        Self {
            guard: ProcessGuard::new(root.path()),
            fake,
            root,
            work,
            thread: Some(thread),
            manager: Some(manager),
            relay,
            bodies: Vec::new(),
            seen: HashSet::new(),
            committed: HashMap::new(),
            execution: Uuid::now_v7(),
            conversation: Uuid::now_v7(),
        }
    }

    fn fake(&self) -> &Fake {
        self.fake.as_ref().unwrap()
    }

    fn manager(&self) -> &ExecutionManager {
        self.manager.as_ref().unwrap()
    }

    fn command(&self, id: Uuid, kind: Kind) -> Command {
        Command {
            command_id: id.to_string(),
            dispatch_attempt: 1,
            execution_id: self.execution.to_string(),
            execution_generation: 1,
            kind: Some(kind),
        }
    }

    async fn send(&self, command: Command) {
        let copy = command.clone();
        let accept = self
            .thread
            .as_ref()
            .unwrap()
            .call(move |store| Ok(store.as_mut().unwrap().accept_command(&copy)?))
            .await
            .unwrap();
        assert!(matches!(accept, Accept::Stored { .. }), "{accept:?}");
        self.manager().handle(command, accept).await.unwrap();
    }

    fn start_command(&self, mode: &str) -> Command {
        self.command(
            Uuid::now_v7(),
            Kind::StartExecution(StartExecution {
                conversation_id: self.conversation.to_string(),
                harness: "opencode".into(),
                workdir: self.work.to_string_lossy().into_owned(),
                model: "fake/fake-1".into(),
                effort: "high".into(),
                permission_mode: mode.into(),
                instructions: Some(Instructions {
                    mode: "clean".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        )
    }

    async fn start(&mut self, mode: &str) {
        self.manager().on_hub_session(self.committed.clone());
        self.send(self.start_command(mode)).await;
    }

    async fn deliver(&self, text: &str) -> Uuid {
        self.deliver_with(text, None).await
    }

    async fn deliver_with(&self, text: &str, settings: Option<SettingsOverride>) -> Uuid {
        let id = Uuid::now_v7();
        self.send(self.command(
            id,
            Kind::DeliverInput(DeliverInput {
                input_id: Uuid::now_v7().to_string(),
                conversation_id: self.conversation.to_string(),
                content: vec![ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock { text: text.into() })),
                }],
                settings,
            }),
        ))
        .await;
        id
    }

    async fn pump(&mut self, within: Duration) -> bool {
        match timeout(within, self.relay.recv()).await {
            Ok(Some(batch)) => {
                let mut last = 0;
                for record in batch.records {
                    last = last.max(record.seq);
                    if self.seen.insert((batch.journal_id, record.seq)) {
                        self.bodies.push(record.body.unwrap().body.unwrap());
                    }
                }
                let entry = self.committed.entry(batch.journal_id).or_default();
                *entry = (*entry).max(last);
                self.manager().on_sj_ack(batch.journal_id, last);
                true
            }
            _ => false,
        }
    }

    async fn wait(&mut self, what: &str, wanted: impl Fn(&[Body]) -> bool) {
        let outcome = timeout(WAIT, async {
            loop {
                if wanted(&self.bodies) {
                    return true;
                }
                if !self.pump(Duration::from_secs(5)).await {
                    return wanted(&self.bodies);
                }
            }
        })
        .await;
        assert!(
            outcome.unwrap_or(false),
            "timed out waiting for: {what}; bodies: {:?}; logs: {}",
            self.bodies.iter().map(describe).collect::<Vec<_>>(),
            self.logs()
        );
    }

    fn logs(&self) -> String {
        let mut text = String::new();
        if let Ok(entries) = fs::read_dir(self.root.path().join("store/executions")) {
            for entry in entries.flatten() {
                text +=
                    &fs::read_to_string(entry.path().join("supervisor.log")).unwrap_or_default();
            }
        }
        text
    }

    async fn wait_running(&mut self) {
        self.wait("running", |bodies| {
            bodies
                .iter()
                .any(|body| matches!(body, Body::ExecutionState(f) if f.state == "running"))
        })
        .await;
    }

    async fn wait_finished(&mut self, command: Uuid) {
        self.wait("run finished", |bodies| {
            bodies.iter().any(|body| {
                matches!(body, Body::RunFinished(f) if f.input_command_id == command.to_string())
            })
        })
        .await;
    }

    fn exec_dir(&self) -> PathBuf {
        self.root
            .path()
            .join("store/executions")
            .join(self.execution.to_string())
    }

    fn decide(&self, request: &str, kind: &str, payload: &Value) -> Command {
        self.command(
            Uuid::now_v7(),
            Kind::DeliverDecision(DeliverDecision {
                request_id: request.to_owned(),
                decision_id: Uuid::now_v7().to_string(),
                generation: 1,
                kind: kind.to_owned(),
                decision_payload_json: payload.to_string().into_bytes(),
            }),
        )
    }

    async fn wait_request(&mut self) -> (String, Value) {
        self.wait("request opened", |bodies| {
            bodies
                .iter()
                .any(|body| matches!(body, Body::RequestOpened(_)))
        })
        .await;
        self.bodies
            .iter()
            .find_map(|body| match body {
                Body::RequestOpened(f) => Some((
                    f.request_id.clone(),
                    serde_json::from_slice(&f.payload_json).unwrap(),
                )),
                _ => None,
            })
            .unwrap()
    }

    fn replies(&self) -> Vec<Value> {
        self.fake()
            .requests()
            .into_iter()
            .filter(|request| path_of(request).ends_with("/reply"))
            .collect()
    }

    async fn stop(&mut self) {
        let stop = self.command(
            Uuid::now_v7(),
            Kind::StopExecution(StopExecution { force: false }),
        );
        self.send(stop).await;
        self.wait("closed", |bodies| {
            bodies
                .iter()
                .any(|body| matches!(body, Body::JournalClosed(_)))
        })
        .await;
    }
}

fn describe(body: &Body) -> String {
    match body {
        Body::ExecutionState(f) => format!("execution_state({},{})", f.state, f.outcome),
        Body::AcceptanceUnconfirmed(f) => format!("acceptance_unconfirmed({})", f.reason),
        Body::AdapterDiagnostic(f) => format!("adapter_diagnostic({})", f.event_type),
        Body::PhaseChanged(f) => format!("phase({})", f.phase),
        Body::BlockStarted(f) => format!("block_started({})", f.kind),
        Body::BlockFinal(_) => "block_final".into(),
        Body::BlockDelta(_) => "block_delta".into(),
        Body::RunFinished(_) => "run_finished".into(),
        Body::RequestOpened(f) => format!("request_opened({})", f.kind),
        Body::RequestClosed(f) => format!("request_closed({})", f.reason),
        Body::DecisionApplied(_) => "decision_applied".into(),
        Body::DecisionNotApplied(f) => format!("decision_not_applied({})", f.reason),
        Body::DecisionUnconfirmed(_) => "decision_unconfirmed".into(),
        Body::InputAccepted(_) => "input_accepted".into(),
        Body::InputRejected(f) => format!("input_rejected({})", f.reason),
        Body::JournalClosed(_) => "journal_closed".into(),
        _ => "other".into(),
    }
}

#[tokio::test]
async fn start_without_the_table_is_refused_by_the_worker() {
    let mut env = Env::new(None).await;
    env.start("bypass").await;
    let thread = env.thread.as_ref().unwrap();
    timeout(WAIT, async {
        loop {
            let rejected = thread
                .call(|store| {
                    Ok(store
                        .as_ref()
                        .unwrap()
                        .cj_records_from(1, usize::MAX)?
                        .into_iter()
                        .filter_map(|record| record.body.and_then(|body| body.body))
                        .any(|body| {
                            matches!(body, Body::CommandRejected(f) if f.reason == "harness_unavailable")
                        }))
                })
                .await
                .unwrap();
            if rejected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("harness_unavailable");
    env.guard.cleanup();
}

#[tokio::test]
async fn start_execution_reaches_running_through_a_real_supervisor() {
    let mut script = ready();
    let prompt = script
        .steps
        .iter()
        .rposition(|step| step.expected_path().is_some())
        .unwrap();
    script.steps.truncate(prompt);
    let mut env = Env::new(Some(script)).await;
    env.start("bypass").await;
    env.wait_running().await;
    let spec = fs::read_to_string(env.exec_dir().join("spec.toml")).unwrap();
    assert!(spec.contains("[opencode]"), "{spec}");
    let requests = env.fake().requests();
    let create = find(&requests, "POST", "/api/session").expect("session.create");
    assert_eq!(
        create["body"]["location"]["directory"],
        env.work.to_str().unwrap()
    );
    assert_eq!(path_of(&requests[0]), "/api/event");
    assert_eq!(
        env.fake().boot()["env"]["OPENCODE_CONFIG_DIR"],
        env.exec_dir().join("opencode-config").to_str().unwrap()
    );
    env.stop().await;
    env.fake().wait_gone().await;
}

// Смена пресета идёт через настоящий супервизор: файл меняется, сервер тот же, а текст
// пресета не попадает ни в журнал сопровождения, ни в спеку на диске.
#[tokio::test]
async fn preset_change_through_the_supervisor_keeps_the_server_and_the_text_private() {
    let mut env = Env::new(Some(ready())).await;
    env.start("bypass").await;
    env.wait_running().await;
    let pids = env.fake().pids();
    let settings = SettingsOverride {
        instructions: Some(Instructions {
            mode: "preset".into(),
            text: "тайный текст пресета".into(),
            preset_id: Uuid::now_v7().to_string(),
            revision: 3,
            title: "Пресет".into(),
        }),
        ..Default::default()
    };
    let command = env.deliver_with("Привет", Some(settings)).await;
    env.wait_finished(command).await;
    let agents = env.exec_dir().join("opencode-config/AGENTS.md");
    assert_eq!(fs::read_to_string(agents).unwrap(), "тайный текст пресета");
    assert_eq!(env.fake().pids(), pids, "no restart");
    assert!(!env.logs().contains("тайный"));
    let spec = fs::read_to_string(env.exec_dir().join("spec.toml")).unwrap();
    assert!(!spec.contains("тайный"));
    env.stop().await;
    env.fake().wait_gone().await;
}

// Команда реле из конфигурации запуска — настоящий `mcp-relay` воркера, а брокер живёт в
// супервизоре: именно это соединение открывает сервер OpenCode.
#[tokio::test]
async fn relay_from_the_boot_config_reaches_the_broker_of_the_supervisor() {
    use std::{
        io::{BufRead as _, BufReader, Write as _},
        process::{Command as Process, Stdio},
    };
    let mut env = Env::new(Some(ready())).await;
    env.start("bypass").await;
    env.wait_running().await;
    let boot = env.fake().boot();
    let content: Value =
        serde_json::from_str(boot["env"]["OPENCODE_CONFIG_CONTENT"].as_str().unwrap()).unwrap();
    let server = &content["mcp"]["servers"]["canterlot"];
    assert_eq!(server["codemode"], false);
    let command: Vec<&str> = server["command"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mut relay = Process::new(command[0])
        .args(&command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = relay.stdin.take().unwrap();
    let call = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "canterlot_limits", "arguments": {}},
    });
    writeln!(stdin, "{call}").unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    let stdout = relay.stdout.take().unwrap();
    let answer = tokio::task::spawn_blocking(move || {
        BufReader::new(stdout).read_line(&mut line).unwrap();
        line
    });
    let answer: Value =
        serde_json::from_str(&timeout(WAIT, answer).await.unwrap().unwrap()).unwrap();
    assert_eq!(
        answer["result"]["content"][0]["text"],
        "no limit data yet in this session"
    );
    drop(stdin);
    relay.wait().unwrap();
    env.stop().await;
    env.fake().wait_gone().await;
}

// Смерть супервизора закрывает stdin, и сервер уходит сам.
#[tokio::test]
async fn dead_supervisor_releases_the_server_through_the_stdin_lease() {
    let mut script = ready();
    let prompt = script
        .steps
        .iter()
        .rposition(|step| step.expected_path().is_some())
        .unwrap();
    script.steps.truncate(prompt);
    let mut env = Env::new(Some(script)).await;
    env.start("bypass").await;
    env.wait_running().await;
    assert!(!env.fake().pids().is_empty());
    for member in env.guard.members() {
        kill_process(
            Pid::from_raw(i32::try_from(member.0).unwrap()).unwrap(),
            Signal::KILL,
        )
        .unwrap();
    }
    env.fake().wait_gone().await;
}

// Сквозной ход по боевому пути: start_execution, ввод на кириллице, блоки и конец хода.
#[tokio::test]
async fn start_execution_to_a_finished_turn() {
    let mut env = Env::new(Some(ready())).await;
    env.start("bypass").await;
    env.wait_running().await;
    let input = env.deliver("Скажи привет").await;
    env.wait_finished(input).await;
    let text: Vec<_> = env
        .bodies
        .iter()
        .filter_map(|body| match body {
            Body::BlockFinal(f) if f.text == "Hello world" => Some(f.block_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text.len(),
        1,
        "{:?}",
        env.bodies.iter().map(describe).collect::<Vec<_>>()
    );
    assert!(
        env.bodies.iter().any(
            |body| matches!(body, Body::InputAccepted(f) if f.command_id == input.to_string())
        )
    );
    let Some(Body::RunFinished(run)) = env
        .bodies
        .iter()
        .find(|body| matches!(body, Body::RunFinished(_)))
    else {
        unreachable!();
    };
    assert_eq!(run.usage[0].model, "fake/fake-1");
    assert_eq!(run.usage[0].agent, "orchestrator");
    assert_eq!(run.usage[0].input_tokens, 70);
    assert_eq!(run.usage[0].output_tokens, 20);
    assert_eq!(run.usage[0].cache_read_tokens, 30);
    assert_eq!(run.context_tokens, 100);
    assert_eq!(run.context_window_tokens, 200_000);
    let requests = env.fake().requests();
    let prompt = find(&requests, "POST", "/prompt").unwrap();
    assert_eq!(prompt["body"]["text"], "Скажи привет");
    assert_eq!(prompt["body"]["delivery"], "queue");
    env.stop().await;
    env.fake().wait_gone().await;
}

// Инструменты по боевому пути: блоки, статус выхода и пометка в журнале.
#[tokio::test]
async fn tools_reach_the_journal_with_exit_status_and_mark() {
    let script = Script::from_capture("tools", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    let mut env = Env::new(Some(script)).await;
    env.start("owner").await;
    env.wait_running().await;
    let input = env.deliver("run tools").await;
    env.wait_finished(input).await;
    let tools: Vec<_> = env
        .bodies
        .iter()
        .filter_map(|body| match body {
            Body::BlockStarted(f) if f.kind == "tool_call" => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(tools.len(), 7);
    assert!(tools.iter().all(|tool| tool.approved_by == "cli_policy"));
    assert!(env.bodies.iter().any(|body| matches!(
        body,
        Body::BlockFinal(f) if f.summary == "shell: exit 3, exit 3"
    )));
    env.stop().await;
    env.fake().wait_gone().await;
}

fn permission_turn() -> Script {
    Script::from_capture("permissions", None)
        .first_run()
        .without_replies(404)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents()
}

fn allow() -> Value {
    json!({"kind": "permission", "behavior": "allow", "message": null})
}

// Режим `ask` по боевому пути: карточка в журнале, решение владельца, один ответ серверу.
#[tokio::test]
async fn ask_reaches_the_owner_and_the_decision_goes_once() {
    let mut env = Env::new(Some(permission_turn())).await;
    env.start("ask").await;
    env.wait_running().await;
    let input = env.deliver("запусти").await;
    let (request, payload) = env.wait_request().await;
    assert_eq!(payload["command"], "echo one > one.txt");
    assert!(env.replies().is_empty());
    let decision = env.decide(&request, "permission", &allow());
    env.send(decision.clone()).await;
    env.wait("decision applied", |bodies| {
        bodies
            .iter()
            .any(|body| matches!(body, Body::DecisionApplied(_)))
    })
    .await;
    // Хаб не получил подтверждения и шлёт ту же команду ещё раз.
    let copy = decision.clone();
    let accept = env
        .thread
        .as_ref()
        .unwrap()
        .call(move |store| Ok(store.as_mut().unwrap().accept_command(&copy)?))
        .await
        .unwrap();
    assert!(matches!(accept, Accept::Duplicate), "{accept:?}");
    env.manager().handle(decision, accept).await.unwrap();
    // Другая команда с тем же запросом: второе решение владельца.
    env.send(env.decide(&request, "permission", &allow())).await;
    env.wait("second decision refused", |bodies| {
        bodies.iter().any(
            |body| matches!(body, Body::DecisionNotApplied(f) if f.reason == "already_transmitted"),
        )
    })
    .await;
    env.wait_finished(input).await;
    let replies = env.replies();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0]["body"], json!({"decision": "once"}));
    let applied = env
        .bodies
        .iter()
        .find_map(|body| match body {
            Body::DecisionApplied(f) => Some(f),
            _ => None,
        })
        .unwrap();
    assert_eq!(applied.proof_kind, "opencode_reply_accepted");
    env.stop().await;
    env.fake().wait_gone().await;
}

// Карточка больше потолка журнала: владелец видит превью, решение идёт по настоящему запросу.
#[tokio::test]
async fn oversized_request_works_through_the_truncated_preview() {
    let mut script = permission_turn();
    let huge = "щ".repeat(300_000);
    for step in &mut script.steps {
        if let Step::Emit { events } = step {
            for event in events {
                if event["type"] == "permission.asked" {
                    event["data"]["action"] = "edit".into();
                    event["data"]["metadata"] = json!({"files": [
                        {"file": "большой.txt", "patch": huge, "status": "modified"},
                    ]});
                }
            }
        }
    }
    let mut env = Env::new(Some(script)).await;
    env.start("ask").await;
    env.wait_running().await;
    let input = env.deliver("правка").await;
    let (request, payload) = env.wait_request().await;
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["changes"][0]["path"], "большой.txt");
    assert!(payload["changes"][0]["diff"].as_str().unwrap().len() < huge.len());
    env.send(env.decide(&request, "permission", &allow())).await;
    env.wait_finished(input).await;
    assert_eq!(env.replies().len(), 1);
    env.stop().await;
    env.fake().wait_gone().await;
}

// Запрос, который супервизор не журналирует: отказ с текстом, ход идёт дальше.
#[tokio::test]
async fn request_the_journal_cannot_hold_is_refused_with_a_message() {
    let mut script = permission_turn();
    let long = format!("per_{}", "a".repeat(1100));
    for step in &mut script.steps {
        match step {
            Step::Emit { events } => {
                for event in events {
                    if event["type"] == "permission.asked" {
                        event["data"]["id"] = long.clone().into();
                    }
                }
            }
            Step::Expect { path, reply, .. } if path.ends_with("/reply") => {
                let session = path.split("/permission/").next().unwrap().to_owned();
                *path = format!("{session}/permission/{long}/reply");
                *reply = Reply {
                    status: 204,
                    ..Default::default()
                };
            }
            _ => {}
        }
    }
    let mut env = Env::new(Some(script)).await;
    env.start("ask").await;
    env.wait_running().await;
    let input = env.deliver("запусти").await;
    env.wait_finished(input).await;
    let replies = env.replies();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0]["body"]["decision"], "reject");
    assert!(
        replies[0]["body"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
    assert!(
        !env.bodies
            .iter()
            .any(|body| matches!(body, Body::RequestOpened(_)))
    );
    env.stop().await;
    env.fake().wait_gone().await;
}

// Смена режима приходит с вводом хаба и уходит на сервер раньше него.
#[tokio::test]
async fn mode_change_from_the_hub_reaches_the_server_before_the_input() {
    let mut script = ready();
    let session = script
        .steps
        .iter()
        .find_map(|step| match step {
            Step::Expect { reply, .. } => reply
                .body
                .as_ref()
                .and_then(|body| body.pointer("/data/id"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        })
        .unwrap();
    script.set_route(
        "PATCH",
        &format!("/api/session/{session}"),
        vec![Reply {
            status: 204,
            ..Default::default()
        }],
    );
    let mut env = Env::new(Some(script)).await;
    env.start("bypass").await;
    env.wait_running().await;
    let input = env
        .deliver_with(
            "дальше только чтение",
            Some(SettingsOverride {
                permission_mode: "read_only".into(),
                ..Default::default()
            }),
        )
        .await;
    env.wait_finished(input).await;
    let requests = env.fake().requests();
    let rules = requests
        .iter()
        .position(|request| request["method"] == "PATCH")
        .expect("rules");
    let prompt = requests
        .iter()
        .position(|request| path_of(request).ends_with("/prompt"))
        .unwrap();
    assert!(rules < prompt, "{requests:?}");
    assert_eq!(requests[rules]["body"]["permissions"][0]["effect"], "deny");
    env.stop().await;
    env.fake().wait_gone().await;
}

fn question_turn() -> Script {
    Script::from_capture("question", None)
        .first_run()
        .without_replies(409)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents()
}

fn answer() -> Value {
    json!({"kind": "question", "answers": {"q0": "Blue", "q1": ["A", "B"]}})
}

// Вопрос по боевому пути: карточка, ответ и фаза ожидания в журнале.
#[tokio::test]
async fn question_goes_through_the_supervisor_once() {
    let mut env = Env::new(Some(question_turn())).await;
    env.start("bypass").await;
    env.wait_running().await;
    let input = env.deliver("спроси").await;
    let (request, payload) = env.wait_request().await;
    assert_eq!(payload["questions"][1]["multi_select"], true);
    env.wait("awaiting answer", |bodies| {
        bodies
            .iter()
            .any(|body| matches!(body, Body::PhaseChanged(f) if f.phase == "awaiting_answer"))
    })
    .await;
    env.send(env.decide(&request, "question", &answer())).await;
    env.wait_finished(input).await;
    env.send(env.decide(&request, "question", &answer())).await;
    env.wait("second answer refused", |bodies| {
        bodies.iter().any(|body| {
            matches!(body, Body::DecisionNotApplied(f)
                if f.reason == "already_transmitted" || f.reason == "request_closed")
        })
    })
    .await;
    let replies = env.replies();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(
        replies[0]["body"],
        json!({"answer": {"q0": "Blue", "q1": ["A", "B"]}})
    );
    env.stop().await;
    env.fake().wait_gone().await;
}

// Вопрос больше потолка журнала: подписи вариантов целы, ответ сверяется по ним.
#[tokio::test]
async fn oversized_question_keeps_labels_in_the_preview() {
    let mut script = question_turn();
    let huge = "щ".repeat(300_000);
    for step in &mut script.steps {
        if let Step::Emit { events } = step {
            for event in events {
                if event["type"] == "form.created" {
                    event["data"]["form"]["fields"][0]["description"] = huge.clone().into();
                }
            }
        }
    }
    let mut env = Env::new(Some(script)).await;
    env.start("bypass").await;
    env.wait_running().await;
    let input = env.deliver("спроси").await;
    let (request, payload) = env.wait_request().await;
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["questions"][0]["options"][0]["label"], "Blue");
    env.send(env.decide(&request, "question", &answer())).await;
    env.wait_finished(input).await;
    assert_eq!(env.replies().len(), 1);
    env.stop().await;
    env.fake().wait_gone().await;
}

fn session_id(script: &Script) -> String {
    script
        .steps
        .iter()
        .find_map(|step| match step {
            Step::Expect { reply, .. } => reply
                .body
                .as_ref()
                .and_then(|body| body.pointer("/data/id"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        })
        .unwrap()
}

fn interrupt_state(body: &Body, command: Uuid, wanted: InterruptState) -> bool {
    matches!(body, Body::InterruptStatus(f)
        if f.command_id == command.to_string() && f.state == i32::from(wanted))
}

// Прерывание по боевому пути: карточка закрыта, ход закончен, статус дошёл до конца.
#[tokio::test]
async fn interrupt_goes_through_the_supervisor() {
    let script = Script::from_capture("interrupt", None)
        .first_run()
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    let mut env = Env::new(Some(script)).await;
    env.start("bypass").await;
    env.wait_running().await;
    let input = env.deliver("hang").await;
    env.wait("accepted", |bodies| {
        bodies
            .iter()
            .any(|body| matches!(body, Body::InputAccepted(_)))
    })
    .await;
    let stop = Uuid::now_v7();
    env.send(env.command(stop, Kind::Interrupt(Interrupt {})))
        .await;
    env.wait("interrupt completed", |bodies| {
        bodies
            .iter()
            .any(|body| interrupt_state(body, stop, InterruptState::Completed))
    })
    .await;
    assert!(env.bodies.iter().any(|body| matches!(
        body,
        Body::RunFinished(f) if f.input_command_id == input.to_string()
    )));
    env.stop().await;
    env.fake().wait_gone().await;
}

/// Ход, затем запуск без ввода, который ждёт прерывания.
fn autonomous_script() -> Script {
    let mut script = Script::from_capture("start_stream", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models();
    let session = session_id(&script);
    script.steps.push(Step::Stall { ms: 100 });
    script.steps.push(Step::Emit {
        events: vec![
            json!({"type": "session.execution.started", "data": {"sessionID": session}}),
            json!({"type": "session.step.started", "data": {
                "sessionID": session, "assistantMessageID": "msg_auto", "agent": "build",
                "model": {"id": "fake-1", "providerID": "fake"}}}),
            json!({"type": "session.step.ended", "data": {
                "sessionID": session, "assistantMessageID": "msg_auto", "finish": "stop",
                "tokens": {"input": 7, "output": 2, "reasoning": 0,
                           "cache": {"read": 0, "write": 0}}}}),
        ],
    });
    script.steps.push(Step::Expect {
        method: "POST".into(),
        path: format!("/api/session/{session}/interrupt"),
        reply: Reply {
            body: Some(json!({"interrupted": true})),
            ..Default::default()
        },
        bind: Default::default(),
    });
    script.steps.push(Step::Emit {
        events: vec![json!({"type": "session.execution.interrupted",
            "data": {"sessionID": session, "reason": "user"}})],
    });
    script
}

// Запуск без ввода тратит лимит: для супервизора он ход, значит его можно прервать, а
// следующий ввод ждёт его конца.
#[tokio::test]
async fn autonomous_run_is_a_turn_for_the_supervisor() {
    let mut env = Env::new(Some(autonomous_script())).await;
    env.start("bypass").await;
    env.wait_running().await;
    let input = env.deliver("первый").await;
    env.wait_finished(input).await;
    env.wait("second phase", |bodies| {
        bodies
            .iter()
            .filter(|body| matches!(body, Body::PhaseChanged(f) if f.phase == "generating"))
            .count()
            >= 2
    })
    .await;
    let stop = Uuid::now_v7();
    env.send(env.command(stop, Kind::Interrupt(Interrupt {})))
        .await;
    env.wait("interrupt completed", |bodies| {
        bodies
            .iter()
            .any(|body| interrupt_state(body, stop, InterruptState::Completed))
    })
    .await;
    let finished: Vec<_> = env
        .bodies
        .iter()
        .filter_map(|body| match body {
            Body::RunFinished(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(finished.len(), 2);
    assert_eq!(finished[1].input_command_id, input.to_string());
    assert!(!finished[0].adopted);
    assert!(finished[1].adopted);
    assert_eq!(finished[1].usage[0].input_tokens, 7);
    env.stop().await;
    env.fake().wait_gone().await;
}

// Смена модели и усилия с вводом хаба: запрос раньше ввода, в формате `Model.Ref`.
#[tokio::test]
async fn model_change_from_the_hub_reaches_the_server_before_the_input() {
    let mut script = ready();
    let session = session_id(&script);
    script.set_route(
        "POST",
        &format!("/api/session/{session}/model"),
        vec![Reply {
            status: 204,
            ..Default::default()
        }],
    );
    let mut env = Env::new(Some(script)).await;
    env.start("bypass").await;
    env.wait_running().await;
    let input = env
        .deliver_with(
            "другой моделью",
            Some(SettingsOverride {
                model: "opencode/flint-40b-free".into(),
                effort: "max".into(),
                ..Default::default()
            }),
        )
        .await;
    env.wait_finished(input).await;
    let requests = env.fake().requests();
    let model = requests
        .iter()
        .position(|request| request["method"] == "POST" && path_of(request).ends_with("/model"))
        .expect("model change");
    let prompt = requests
        .iter()
        .position(|request| path_of(request).ends_with("/prompt"))
        .unwrap();
    assert!(model < prompt, "{requests:?}");
    assert_eq!(
        requests[model]["body"],
        json!({"model": {"id": "flint-40b-free", "providerID": "opencode", "variant": "max"}})
    );
    env.stop().await;
    env.fake().wait_gone().await;
}
