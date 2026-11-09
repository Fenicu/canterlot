#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

//! Адаптер OpenCode на двойнике `fake-opencode` (L1): настоящий процесс, HTTP и SSE.
//! Сценарии собраны из записей этапа 0.

use canterlot_proto::requests::{AnswerValue, DecisionPayload, PermissionBehavior};
use canterlot_worker::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, AgentCallResult, BlockKind, CloseMode, CompactOutcome,
        DecisionOutcome, InstructionMode, InstructionSpec, InterruptOutcome, LimitFact,
        LimitWindow, Phase, SettingsChange, StartSpec, Started, SteerOutcome, Transmission,
        TransmitOutcome,
        opencode::{
            DiscoverError, OpenCodeAdapter,
            catalog::ModelEntry,
            discover, discover_within,
            replay::{Reply, Script, Step, evidence_path},
        },
    },
    config::OpenCodeConfig,
    containment::TreeContainment,
};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt as _, path::PathBuf, time::Duration};
use uuid::Uuid;

#[path = "support/opencode.rs"]
mod opencode;
use opencode::{Fake, find, path_of};

const WAIT: Duration = Duration::from_secs(15);

fn instructions(mode: InstructionMode, text: &str) -> InstructionSpec {
    InstructionSpec {
        mode,
        text: text.into(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

struct Bench {
    fake: Fake,
    adapter: OpenCodeAdapter,
    group: TreeContainment,
    spec: StartSpec,
    exec_dir: PathBuf,
}

impl Bench {
    fn new(script: Script) -> Self {
        Self::with_config(script, |_| {})
    }

    fn with_config(script: Script, change: impl FnOnce(&mut OpenCodeConfig)) -> Self {
        let fake = Fake::new(script);
        let exec_dir = fake.dir.path().join("data/executions/one");
        std::fs::create_dir_all(&exec_dir).unwrap();
        let mut cfg = OpenCodeConfig {
            executable: fake.exe.clone(),
            owner_config: Some(fake.dir.path().join("owner-config")),
            ..Default::default()
        };
        change(&mut cfg);
        let adapter = OpenCodeAdapter::new(cfg, exec_dir.clone(), Duration::from_millis(700))
            .with_relay_executable(PathBuf::from("/opt/relay"));
        let spec = StartSpec {
            execution_id: Uuid::now_v7(),
            generation: 1,
            workdir: opencode::workdir(&fake),
            native_conversation_id: None,
            profile: "opencode".into(),
            model: "fake/fake-1".into(),
            effort: "high".into(),
            permission_mode: "bypass".into(),
            instructions: instructions(InstructionMode::Owner, ""),
            isolate_mcp: false,
        };
        Self {
            fake,
            adapter,
            group: TreeContainment::new().unwrap(),
            spec,
            exec_dir,
        }
    }

    async fn start(&mut self) -> Result<Started, AdapterError> {
        tokio::time::timeout(WAIT, self.adapter.start(&self.spec, &self.group))
            .await
            .expect("start must finish")
    }

    async fn start_ok(&mut self) -> Started {
        self.start().await.expect("start")
    }

    async fn fail_reason(&mut self) -> &'static str {
        match self.start().await {
            Err(AdapterError::StartFailed(reason)) => reason,
            other => panic!("start must fail: {other:?}"),
        }
    }

    async fn event(&mut self) -> AdapterEvent {
        tokio::time::timeout(WAIT, self.adapter.next_event())
            .await
            .expect("adapter event")
    }

    async fn close(&mut self, mode: CloseMode) {
        tokio::time::timeout(WAIT, self.adapter.close(mode, &self.group))
            .await
            .expect("close must finish")
            .expect("close");
    }

    fn input(&self, text: &str) -> Transmission {
        Transmission {
            command_id: Uuid::now_v7(),
            attempt: 1,
            text: text.into(),
            files: vec![],
            settings: None,
        }
    }

    async fn transmit(&mut self, input: &Transmission) -> TransmitOutcome {
        tokio::time::timeout(WAIT, self.adapter.transmit(input, &self.group))
            .await
            .expect("transmit must finish")
    }

    async fn accepted(&mut self, input: &Transmission) {
        match self.transmit(input).await {
            TransmitOutcome::Accepted(proof) => {
                assert_eq!(proof.command_id, input.command_id);
                assert_eq!(proof.kind, "opencode_inbox_admitted");
            }
            other => panic!("{other:?}"),
        }
    }

    async fn until_finished(&mut self, command: Uuid) -> Vec<AdapterEvent> {
        let mut events = Vec::new();
        loop {
            let event = self.event().await;
            let done = matches!(
                &event,
                AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == command
            );
            events.push(event);
            if done {
                return events;
            }
        }
    }

    fn env(&self, key: &str) -> Option<String> {
        self.fake.boot()["env"][key].as_str().map(str::to_owned)
    }

    /// События до открытия запроса: сам запрос последним.
    async fn until_request(&mut self) -> (String, Value, Vec<AdapterEvent>) {
        let mut events = Vec::new();
        loop {
            let event = self.event().await;
            if let AdapterEvent::RequestOpened {
                request_id,
                payload,
                ..
            } = &event
            {
                let opened = (request_id.clone(), payload.clone());
                events.push(event);
                return (opened.0, opened.1, events);
            }
            events.push(event);
        }
    }

    async fn deliver(&mut self, request: &str, decision: &DecisionPayload) -> DecisionOutcome {
        tokio::time::timeout(WAIT, self.adapter.deliver_decision(request, decision))
            .await
            .expect("decision must finish")
    }

    fn replies(&self) -> Vec<Value> {
        self.fake
            .requests()
            .into_iter()
            .filter(|request| path_of(request).ends_with("/reply"))
            .collect()
    }
}

trait Applied {
    fn is_applied(&self) -> bool;
}

impl Applied for DecisionOutcome {
    fn is_applied(&self) -> bool {
        matches!(self, DecisionOutcome::Applied(_))
    }
}

fn allow() -> DecisionPayload {
    DecisionPayload::Permission {
        behavior: PermissionBehavior::Allow,
        message: None,
    }
}

fn deny(message: Option<&str>) -> DecisionPayload {
    DecisionPayload::Permission {
        behavior: PermissionBehavior::Deny,
        message: message.map(str::to_owned),
    }
}

/// Первый ход записи: запрос разрешения и ответ, повторы отказа сняты.
fn permission_turn() -> Script {
    Script::from_capture("permissions", None)
        .first_run()
        .without_replies(404)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents()
}

fn ready() -> Script {
    Script::from_capture("start_stream", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
}

/// Запись `resume`: два хода подряд в одном процессе.
fn two_turns() -> Script {
    let mut script = Script::from_capture("resume", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models();
    let third = script
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .nth(2)
        .map(|(index, _)| index)
        .unwrap();
    script.steps.truncate(third);
    script.with_recorded_agents()
}

fn tools() -> Script {
    Script::from_capture("tools", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents()
}

struct Call {
    tool: String,
    approved_by: Option<String>,
}

fn calls(events: &[AdapterEvent]) -> Vec<Call> {
    events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockStarted {
                kind: BlockKind::ToolCall,
                tool_name,
                approved_by,
                ..
            } => Some(Call {
                tool: tool_name.clone().unwrap_or_default(),
                approved_by: approved_by.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn session_of(script: &Script) -> String {
    script
        .steps
        .iter()
        .find_map(|step| match step {
            Step::Expect { reply, .. } => reply
                .body
                .as_ref()?
                .pointer("/data/id")?
                .as_str()
                .map(str::to_owned),
            _ => None,
        })
        .unwrap()
}

/// Сценарий с записанным началом вызова: шаг, вход и запрос разрешения.
fn tool_prefix(script: &Script) -> Vec<Value> {
    let events: Vec<Value> = script
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Emit { events } => Some(events.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    let position = events
        .iter()
        .position(|event| event["type"] == "session.tool.called")
        .unwrap();
    events[..=position].to_vec()
}

fn tool_events(script: &Script, kinds: &[&str]) -> Vec<Value> {
    script
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Emit { events } => Some(events.clone()),
            _ => None,
        })
        .flatten()
        .filter(|event| kinds.contains(&event["type"].as_str().unwrap_or_default()))
        .collect()
}

// Шаги записи до первого `tool.called`, дальше события подаёт тест.
fn replace_events_after_prompt(script: &mut Script, events: Vec<Step>) {
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(position + 1);
    script.steps.extend(events);
}

fn finish(session: &str) -> Value {
    json!({"type": "session.execution.succeeded", "data": {"sessionID": session}})
}

fn reply_204() -> Reply {
    Reply {
        status: 204,
        ..Default::default()
    }
}

fn text_of(events: &[AdapterEvent], kind: &str) -> Vec<String> {
    let ids: Vec<Uuid> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockStarted {
                block_id,
                kind: found,
                ..
            } if format!("{found:?}") == kind => Some(*block_id),
            _ => None,
        })
        .collect();
    ids.iter()
        .filter_map(|id| {
            events.iter().find_map(|event| match event {
                AdapterEvent::BlockFinal { block_id, text, .. } if block_id == id => {
                    Some(text.clone())
                }
                _ => None,
            })
        })
        .collect()
}

fn set_prompt_reply(script: &mut Script, change: impl FnOnce(&mut Reply)) {
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    let Step::Expect { reply, .. } = &mut script.steps[position] else {
        unreachable!();
    };
    change(reply);
}

/// Без `/prompt`: запись дальше шага создания сессии не нужна.
fn session_only() -> Script {
    let mut script = ready();
    script.steps.truncate(
        script
            .steps
            .iter()
            .position(|step| step.expected_path().is_some())
            .unwrap()
            + 1,
    );
    script
}

#[tokio::test]
async fn start_path_runs_in_the_recorded_order() {
    let mut bench = Bench::new(session_only());
    let started = bench.start_ok().await;
    assert!(started.native_conversation_id.starts_with("ses_"));
    assert_eq!(
        started.native_conversation_id.len(),
        "ses_".len() + 32,
        "{}",
        started.native_conversation_id
    );
    assert_eq!(bench.fake.pids(), vec![started.cli_pid]);
    assert!(started.cli_start_ticks > 0);
    let requests = bench.fake.requests();
    assert_eq!(
        path_of(&requests[0]),
        "/api/event",
        "SSE first: {requests:?}"
    );
    assert!(requests.iter().all(|request| request["authorized"] == true));
    let position = |tail: &str| {
        requests
            .iter()
            .position(|request| path_of(request) == tail)
            .unwrap_or_else(|| panic!("{tail}: {requests:?}"))
    };
    assert!(position("/api/info") < position("/api/session"));
    assert!(position("/api/session") < position("/api/mcp"));
    assert!(position("/api/mcp") < position("/api/agent"));
    let create = find(&requests, "POST", "/api/session").unwrap();
    assert_eq!(create["body"]["id"], started.native_conversation_id);
    assert!(
        create["body"]["title"]
            .as_str()
            .is_some_and(|t| !t.is_empty())
    );
    assert_eq!(
        create["body"]["location"]["directory"],
        bench.spec.workdir.to_str().unwrap()
    );
    assert_eq!(
        create["body"]["model"],
        json!({"id": "fake-1", "providerID": "fake", "variant": "high"})
    );
    assert_eq!(
        create["body"]["permissions"],
        json!([{"action": "*", "resource": "*", "effect": "allow"}])
    );
    assert_eq!(
        Value::from(bench.fake.boot()["cwd"].as_str().unwrap()),
        Value::from(bench.spec.workdir.to_str().unwrap())
    );
    bench.close(CloseMode::Graceful).await;
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn server_gets_its_own_data_and_the_canterlot_layer() {
    let mut bench = Bench::new(session_only());
    bench.start_ok().await;
    let data = bench.fake.dir.path().join("data/opencode");
    assert_eq!(bench.fake.boot()["password_set"], true);
    assert_eq!(
        bench.env("XDG_DATA_HOME").unwrap(),
        data.join("data").to_str().unwrap()
    );
    assert_eq!(
        bench.env("XDG_STATE_HOME").unwrap(),
        data.join("state").to_str().unwrap()
    );
    assert_eq!(
        bench.env("XDG_CACHE_HOME").unwrap(),
        data.join("cache").to_str().unwrap()
    );
    assert_eq!(
        bench.env("TMPDIR").unwrap(),
        bench.exec_dir.join("tmp").to_str().unwrap()
    );
    assert_eq!(
        bench.env("OPENCODE_DISABLE_AUTOUPDATE").as_deref(),
        Some("1")
    );
    assert_eq!(bench.env("OPENCODE_CONFIG_DIR"), None);
    let content: Value =
        serde_json::from_str(&bench.env("OPENCODE_CONFIG_CONTENT").unwrap()).unwrap();
    assert_eq!(content["update"], "disable");
    assert_eq!(content["share"], "disabled");
    assert_eq!(content["snapshots"], false);
    assert_eq!(content["experimental"]["todo"], true);
    let server = &content["mcp"]["servers"]["canterlot"];
    assert_eq!(server["type"], "local");
    assert_eq!(server["codemode"], false);
    assert_eq!(
        server["command"],
        json!([
            "/opt/relay",
            "mcp-relay",
            "--socket",
            bench.exec_dir.join("mcp.sock").to_str().unwrap()
        ])
    );
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn preset_text_goes_to_the_execution_config_dir() {
    let mut bench = Bench::new(session_only());
    bench.spec.instructions = instructions(InstructionMode::Preset, "Отвечай кратко");
    bench.start_ok().await;
    let dir = bench.exec_dir.join("opencode-config");
    assert_eq!(
        bench.env("OPENCODE_CONFIG_DIR").unwrap(),
        dir.to_str().unwrap()
    );
    assert_eq!(
        bench.env("OPENCODE_DISABLE_CLAUDE_CODE").as_deref(),
        Some("1")
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(),
        "Отвечай кратко"
    );
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn clean_mode_replaces_the_config_dir_with_an_empty_one() {
    let mut bench = Bench::new(session_only());
    bench.spec.instructions = instructions(InstructionMode::Clean, "");
    bench.start_ok().await;
    let dir = bench.exec_dir.join("opencode-config");
    assert_eq!(
        bench.env("OPENCODE_CONFIG_DIR").unwrap(),
        dir.to_str().unwrap()
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    bench.close(CloseMode::Graceful).await;
}

fn owner_config(bench: &Bench) -> PathBuf {
    let owner = bench.fake.dir.path().join("owner-config");
    std::fs::create_dir_all(owner.join("plugins")).unwrap();
    std::fs::write(owner.join("opencode.json"), "{}").unwrap();
    std::fs::write(owner.join("opencode.jsonc"), "{}").unwrap();
    std::fs::write(owner.join("AGENTS.md"), "инструкции владельца").unwrap();
    owner
}

// Настройки, вход и плагины владельца остаются, его инструкции нет: оба файла конфигурации
// читаются только из каталога, `OPENCODE_CONFIG` принял бы один.
#[tokio::test]
async fn clean_mode_links_the_owner_config_without_his_instructions() {
    let mut bench = Bench::new(session_only());
    let owner = owner_config(&bench);
    bench.spec.instructions = instructions(InstructionMode::Clean, "");
    bench.start_ok().await;
    let dir = bench.exec_dir.join("opencode-config");
    for name in ["opencode.json", "opencode.jsonc", "plugins"] {
        assert_eq!(
            std::fs::read_link(dir.join(name)).unwrap(),
            owner.join(name),
            "{name}"
        );
    }
    assert!(!dir.join("AGENTS.md").exists());
    assert_eq!(bench.env("OPENCODE_CONFIG"), None);
    bench.close(CloseMode::Graceful).await;
}

fn preset(text: &str, revision: u64) -> InstructionSpec {
    InstructionSpec {
        mode: InstructionMode::Preset,
        text: text.into(),
        preset_id: Some(Uuid::from_u128(7)),
        revision,
        title: "Пресет".into(),
    }
}

fn instructions_change(next: InstructionSpec) -> SettingsChange {
    SettingsChange {
        instructions: Some(next),
        ..Default::default()
    }
}

fn agents_file(bench: &Bench) -> PathBuf {
    bench.exec_dir.join("opencode-config/AGENTS.md")
}

fn boots(bench: &Bench) -> usize {
    bench
        .fake
        .entries()
        .iter()
        .filter(|entry| entry["boot"] == true)
        .count()
}

// Смена текста приходит модели сообщением `<system-update>` на следующем шаге: перезапуск не
// нужен, файл меняется целиком и только для владельца.
#[tokio::test]
async fn preset_change_rewrites_the_file_and_keeps_the_server() {
    use std::os::unix::fs::PermissionsExt as _;
    let mut bench = Bench::new(two_turns());
    bench.spec.instructions = instructions(InstructionMode::Clean, "");
    let started = bench.start_ok().await;
    let mut first = bench.input("первый");
    first.settings = Some(instructions_change(preset("Новый текст", 2)));
    assert_eq!(bench.adapter.validate_transmission(&first), Ok(()));
    bench.accepted(&first).await;
    assert_eq!(
        std::fs::read_to_string(agents_file(&bench)).unwrap(),
        "Новый текст"
    );
    let mode = std::fs::metadata(agents_file(&bench)).unwrap();
    assert_eq!(mode.permissions().mode() & 0o777, 0o600);
    bench.until_finished(first.command_id).await;
    let mut second = bench.input("второй");
    second.settings = Some(instructions_change(instructions(
        InstructionMode::Clean,
        "",
    )));
    bench.accepted(&second).await;
    assert!(!agents_file(&bench).exists());
    assert_eq!(boots(&bench), 1, "no restart");
    assert_eq!(bench.fake.pids(), vec![started.cli_pid]);
    bench.close(CloseMode::Graceful).await;
}

// Хаб настроек не применил: перед следующим вводом файл возвращается к прежнему тексту.
#[tokio::test]
async fn refused_input_brings_the_previous_instructions_back() {
    let mut script = two_turns();
    let session = session_of(&script);
    script.steps.retain(|step| {
        !step
            .expected_path()
            .is_some_and(|path| path.ends_with("/prompt"))
    });
    script.set_route(
        "POST",
        &format!("/api/session/{session}/prompt"),
        vec![
            Reply {
                status: 400,
                body: Some(json!({"_tag": "InvalidRequestError"})),
                ..Default::default()
            },
            Reply {
                status: 400,
                body: Some(json!({"_tag": "InvalidRequestError"})),
                ..Default::default()
            },
        ],
    );
    let mut bench = Bench::new(script);
    bench.spec.instructions = preset("Было", 1);
    bench.start_ok().await;
    let mut input = bench.input("x");
    input.settings = Some(instructions_change(preset("Стало", 2)));
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Rejected(reason) if reason == "InvalidRequestError"
    ));
    assert_eq!(
        std::fs::read_to_string(agents_file(&bench)).unwrap(),
        "Стало"
    );
    let next = bench.input("y");
    assert!(matches!(
        bench.transmit(&next).await,
        TransmitOutcome::Rejected(_)
    ));
    assert_eq!(
        std::fs::read_to_string(agents_file(&bench)).unwrap(),
        "Было"
    );
    bench.close(CloseMode::Graceful).await;
}

// «Как у владельца» и остальные режимы различаются окружением запуска: только такая смена
// перезапускает сервер, а сессия та же.
#[tokio::test]
async fn switch_from_owner_restarts_the_server_with_the_same_session() {
    let (script, session) = resumable(two_turns());
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(session.clone());
    bench.spec.instructions = instructions(InstructionMode::Owner, "");
    let started = bench.start_ok().await;
    assert_eq!(bench.env("OPENCODE_CONFIG_DIR"), None);
    let mut input = bench.input("x");
    input.settings = Some(instructions_change(preset("Текст", 1)));
    let outcome = bench.transmit(&input).await;
    assert!(
        !matches!(outcome, TransmitOutcome::Undetermined),
        "{outcome:?}"
    );
    assert_eq!(boots(&bench), 2);
    let pids = bench.fake.pids();
    assert_eq!(pids.len(), 1, "{pids:?}");
    assert_ne!(pids[0], started.cli_pid);
    let boot = bench
        .fake
        .entries()
        .into_iter()
        .rfind(|entry| entry["boot"] == true)
        .unwrap();
    assert_eq!(
        boot["env"]["OPENCODE_CONFIG_DIR"],
        bench.exec_dir.join("opencode-config").to_str().unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(agents_file(&bench)).unwrap(),
        "Текст"
    );
    let event = bench.event().await;
    assert!(
        matches!(event, AdapterEvent::CliRestarted { cli_pid, .. } if cli_pid == pids[0]),
        "{event:?}"
    );
    let creates = bench
        .fake
        .requests()
        .iter()
        .filter(|request| request["method"] == "POST" && path_of(request) == "/api/session")
        .count();
    assert_eq!(creates, 0, "the session survives the restart");
    bench.close(CloseMode::Graceful).await;
}

// Каталог не готов: работающий сервер не тронут, ввод отклонён.
#[tokio::test]
async fn restart_that_cannot_prepare_leaves_the_server_running() {
    let mut bench = Bench::new(ready());
    bench.spec.instructions = instructions(InstructionMode::Owner, "");
    let started = bench.start_ok().await;
    std::fs::write(bench.exec_dir.join("opencode-config"), "не каталог").unwrap();
    let mut input = bench.input("x");
    input.settings = Some(instructions_change(preset("Текст", 1)));
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"
    ));
    assert_eq!(bench.fake.pids(), vec![started.cli_pid]);
    bench.close(CloseMode::Graceful).await;
}

fn resumable(mut script: Script) -> (Script, String) {
    let session = session_of(&script);
    script.set_route(
        "GET",
        &format!("/api/session/{session}"),
        vec![Reply {
            body: Some(json!({"data": {"id": session}})),
            ..Default::default()
        }],
    );
    script.set_route(
        "PATCH",
        &format!("/api/session/{session}"),
        vec![reply_204()],
    );
    script.set_route(
        "POST",
        &format!("/api/session/{session}/model"),
        vec![reply_204()],
    );
    (script, session)
}

// Новый запуск упал: возвращается прежняя конфигурация, ввод отклонён, сессия живёт.
#[tokio::test]
async fn failed_restart_respawns_the_previous_server_and_rejects_the_input() {
    let (script, session) = resumable(ready());
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(session);
    bench.spec.instructions = instructions(InstructionMode::Owner, "");
    let started = bench.start_ok().await;
    let flag = bench.fake.dir.path().join("fail-next");
    let wrapper = std::fs::read_to_string(&bench.fake.exe).unwrap();
    std::fs::write(
        &bench.fake.exe,
        wrapper.replacen(
            "exec ",
            &format!(
                "if [ -f '{0}' ]; then rm '{0}'; exit 3; fi\nexec ",
                flag.display()
            ),
            1,
        ),
    )
    .unwrap();
    std::fs::write(&flag, "").unwrap();
    let mut input = bench.input("x");
    input.settings = Some(instructions_change(preset("Текст", 1)));
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Rejected(reason) if reason == "instructions_change_failed"
    ));
    let pids = bench.fake.pids();
    assert_eq!(pids.len(), 1, "{pids:?}");
    assert_ne!(pids[0], started.cli_pid);
    assert!(!agents_file(&bench).exists(), "previous configuration");
    loop {
        if let AdapterEvent::CliRestarted { cli_pid, .. } = bench.event().await {
            assert_eq!(cli_pid, pids[0]);
            break;
        }
    }
    bench.close(CloseMode::Graceful).await;
}

#[test]
fn preset_text_stays_out_of_debug_output() {
    let change = instructions_change(preset("тайный текст", 1));
    let input = Transmission {
        command_id: Uuid::now_v7(),
        attempt: 1,
        text: String::new(),
        files: Vec::new(),
        settings: Some(change.clone()),
    };
    for shown in [format!("{change:?}"), format!("{input:?}")] {
        assert!(!shown.contains("тайный"), "{shown}");
    }
}

#[tokio::test]
async fn modes_become_session_rules() {
    for (mode, rules) in [
        ("owner", Value::Null),
        (
            "read_only",
            json!([
                {"action": "*", "resource": "*", "effect": "deny"},
                {"action": "read", "resource": "*", "effect": "allow"},
                {"action": "glob", "resource": "*", "effect": "allow"},
                {"action": "grep", "resource": "*", "effect": "allow"},
                {"action": "question", "resource": "*", "effect": "allow"},
                {"action": "subagent", "resource": "*", "effect": "allow"},
                {"action": "todowrite", "resource": "*", "effect": "allow"},
                {"action": "canterlot_canterlot_limits", "resource": "*", "effect": "allow"},
                {"action": "canterlot_canterlot_presets_list", "resource": "*", "effect": "allow"},
                {"action": "canterlot_sessions_list", "resource": "*", "effect": "allow"},
                {"action": "canterlot_session_inspect", "resource": "*", "effect": "allow"},
                {"action": "canterlot_session_subscribe", "resource": "*", "effect": "allow"},
                {"action": "canterlot_session_unsubscribe", "resource": "*", "effect": "allow"},
                {"action": "canterlot_session_wait", "resource": "*", "effect": "allow"},
                {"action": "canterlot_alarm_set", "resource": "*", "effect": "allow"},
                {"action": "canterlot_alarm_cancel", "resource": "*", "effect": "allow"},
                {"action": "canterlot_alarm_list", "resource": "*", "effect": "allow"},
                {"action": "canterlot_route_preview", "resource": "*", "effect": "allow"},
                {"action": "canterlot_plan_list", "resource": "*", "effect": "allow"},
                {"action": "canterlot_plan_add", "resource": "*", "effect": "allow"},
                {"action": "canterlot_plan_update", "resource": "*", "effect": "allow"},
                {"action": "canterlot_history_search", "resource": "*", "effect": "allow"},
            ]),
        ),
        (
            "ask",
            json!([
                {"action": "*", "resource": "*", "effect": "ask"},
                {"action": "question", "resource": "*", "effect": "allow"},
                {"action": "todowrite", "resource": "*", "effect": "allow"},
                {"action": "canterlot_canterlot_limits", "resource": "*", "effect": "allow"},
                {"action": "canterlot_canterlot_presets_list", "resource": "*", "effect": "allow"},
                {"action": "canterlot_sessions_list", "resource": "*", "effect": "allow"},
                {"action": "canterlot_session_inspect", "resource": "*", "effect": "allow"},
                {"action": "canterlot_session_subscribe", "resource": "*", "effect": "allow"},
                {"action": "canterlot_session_unsubscribe", "resource": "*", "effect": "allow"},
                {"action": "canterlot_session_wait", "resource": "*", "effect": "allow"},
                {"action": "canterlot_alarm_set", "resource": "*", "effect": "allow"},
                {"action": "canterlot_alarm_cancel", "resource": "*", "effect": "allow"},
                {"action": "canterlot_alarm_list", "resource": "*", "effect": "allow"},
                {"action": "canterlot_route_preview", "resource": "*", "effect": "allow"},
                {"action": "canterlot_plan_list", "resource": "*", "effect": "allow"},
                {"action": "canterlot_plan_add", "resource": "*", "effect": "allow"},
                {"action": "canterlot_plan_update", "resource": "*", "effect": "allow"},
                {"action": "canterlot_history_search", "resource": "*", "effect": "allow"},
            ]),
        ),
    ] {
        let mut bench = Bench::new(session_only());
        bench.spec.permission_mode = mode.into();
        bench.start_ok().await;
        let requests = bench.fake.requests();
        let create = find(&requests, "POST", "/api/session").unwrap();
        assert_eq!(create["body"]["permissions"], rules, "{mode}");
        bench.close(CloseMode::Graceful).await;
    }
}

#[tokio::test]
async fn session_tools_follow_the_mode() {
    for (mode, default, read_like) in [("ask", "ask", "allow"), ("read_only", "deny", "allow")] {
        let mut bench = Bench::new(session_only());
        bench.spec.permission_mode = mode.into();
        bench.start_ok().await;
        let requests = bench.fake.requests();
        let create = find(&requests, "POST", "/api/session").unwrap();
        let rules = create["body"]["permissions"].as_array().unwrap().clone();
        // В OpenCode действует последнее совпавшее правило.
        let effect = |tool: &str| {
            rules
                .iter()
                .rev()
                .find(|rule| rule["action"] == tool || rule["action"] == "*")
                .unwrap()["effect"]
                .clone()
        };
        for tool in [
            "canterlot_sessions_list",
            "canterlot_session_inspect",
            "canterlot_session_subscribe",
            "canterlot_session_unsubscribe",
            "canterlot_session_wait",
            "canterlot_plan_list",
            "canterlot_plan_add",
            "canterlot_plan_update",
            "canterlot_history_search",
        ] {
            assert_eq!(effect(tool), read_like, "{mode} {tool}");
        }
        for tool in [
            "canterlot_session_send",
            "canterlot_session_spawn",
            "canterlot_session_accept",
            "canterlot_session_interrupt",
            "canterlot_session_stop",
            "canterlot_spawn_cancel",
            "canterlot_plan_adopt",
        ] {
            assert_eq!(effect(tool), default, "{mode} {tool}");
        }
        bench.close(CloseMode::Graceful).await;
    }
}

#[tokio::test]
async fn unknown_mode_stops_before_the_cli_starts() {
    let mut bench = Bench::new(session_only());
    bench.spec.permission_mode = "yolo".into();
    assert_eq!(bench.fail_reason().await, "unknown_permission_mode");
    assert!(bench.fake.requests().is_empty());
}

#[tokio::test]
async fn database_of_the_owner_is_refused() {
    let home = std::env::var("HOME").unwrap();
    let root = format!("{home}/.local/share/opencode/canterlot-never-created");
    let mut bench = Bench::with_config(session_only(), |cfg| {
        cfg.data_root = Some(root.clone().into());
    });
    assert_eq!(bench.fail_reason().await, "data_root_refused");
    assert!(bench.fake.requests().is_empty());
    assert!(!std::path::Path::new(&root).exists());
}

#[tokio::test]
async fn old_cli_is_incompatible() {
    let mut script = session_only();
    script.version = "opencode v2.1.3".into();
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "cli_too_old");
}

#[tokio::test]
async fn missing_cli_is_named() {
    let mut bench = Bench::with_config(session_only(), |cfg| {
        cfg.executable = "/nonexistent/opencode".into();
    });
    assert_eq!(bench.fail_reason().await, "cli_not_found");
}

#[tokio::test]
async fn non_loopback_ready_line_is_refused() {
    for line in [
        r#"{"url":"http://10.0.0.1:4096"}"#,
        r#"{"url":"https://127.0.0.1:4096"}"#,
        r#"{"url":"http://evil.example:4096"}"#,
        r#"{"url":"http://user:pw@127.0.0.1:4096"}"#,
    ] {
        let mut script = session_only();
        script.ready.line = Some(line.into());
        let mut bench = Bench::new(script);
        assert_eq!(bench.fail_reason().await, "server_not_loopback", "{line}");
        bench.fake.wait_gone().await;
    }
}

#[tokio::test]
async fn garbled_ready_line_fails_the_start() {
    let mut script = session_only();
    script.ready.line = Some("Update available".into());
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "server_not_ready");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn silent_server_hits_the_ready_deadline() {
    let mut script = session_only();
    script.ready.silent = true;
    let mut bench = Bench::new(script);
    bench.adapter.set_ready_wait(Duration::from_millis(300));
    assert_eq!(bench.fail_reason().await, "server_not_ready");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn server_dying_before_the_ready_line_fails_the_start() {
    let mut script = session_only();
    script.ready.exit = Some(7);
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "server_not_ready");
}

// Строка готовности старого процесса: порт уже никто не слушает.
#[tokio::test]
async fn stale_ready_line_fails_to_connect() {
    let unused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = unused.local_addr().unwrap().port();
    drop(unused);
    let mut script = session_only();
    script.ready.line = Some(format!(r#"{{"url":"http://127.0.0.1:{port}"}}"#));
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "server_unreachable");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn other_server_on_the_port_is_refused_by_pid() {
    let mut script = session_only();
    script.set_route(
        "GET",
        "/api/info",
        vec![Reply {
            body: Some(json!({"version": "2.1.4", "pid": 1})),
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "server_identity_mismatch");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn version_of_the_api_must_match_the_cli() {
    let mut script = session_only();
    script.set_route(
        "GET",
        "/api/info",
        vec![Reply {
            body: Some(json!({"version": "2.1.9", "pid": "$PID"})),
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "server_identity_mismatch");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn untested_version_runs_with_diagnostics() {
    let mut script = session_only();
    script.version = "opencode v2.2.0".into();
    script.set_route(
        "GET",
        "/api/info",
        vec![Reply {
            body: Some(json!({"version": "2.2.0", "pid": "$PID"})),
            ..Default::default()
        }],
    );
    script.set_route(
        "GET",
        "/openapi.json",
        vec![Reply {
            body: Some(json!({"openapi": "changed"})),
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let mut kinds = Vec::new();
    for _ in 0..2 {
        match bench.event().await {
            AdapterEvent::Diagnostic { kind, required, .. } => {
                assert!(!required);
                kinds.push(kind);
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(kinds, ["untested_cli_version", "openapi_changed"]);
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn failed_mcp_is_a_diagnostic_not_a_failed_start() {
    let script = Script::from_capture("start_stream", None).with_canterlot_mcp("failed");
    let mut script = script;
    script.steps.truncate(
        script
            .steps
            .iter()
            .position(|step| step.expected_path().is_some())
            .unwrap()
            + 1,
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    match bench.event().await {
        AdapterEvent::Diagnostic { kind, required, .. } => {
            assert_eq!(kind, "mcp_unavailable");
            assert!(!required);
        }
        other => panic!("{other:?}"),
    }
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn pending_mcp_past_the_deadline_is_a_diagnostic() {
    let mut bench = Bench::new({
        let mut script = session_only();
        let pending = script.route("GET", "/api/mcp").unwrap().replies[0].clone();
        script.set_route("GET", "/api/mcp", vec![pending]);
        script
    });
    bench.adapter.set_ready_wait(Duration::from_millis(300));
    bench.start_ok().await;
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Diagnostic {
            kind: "mcp_unavailable",
            required: false,
            ..
        }
    ));
    bench.close(CloseMode::Graceful).await;
}

/// Делегированная сессия с заданным ответом `/api/mcp` и коротким сроком ожидания.
fn isolated(replies: Vec<Reply>) -> Bench {
    let mut script = session_only();
    script.set_route("GET", "/api/mcp", replies);
    let mut bench = Bench::new(script);
    bench.spec.isolate_mcp = true;
    bench.adapter.set_ready_wait(Duration::from_millis(300));
    bench
}

fn mcp_reply(body: Value) -> Reply {
    Reply {
        body: Some(body),
        ..Reply::default()
    }
}

#[tokio::test]
async fn isolated_start_fails_closed_without_proof_of_the_server_list() {
    let refused = Reply {
        status: 500,
        body: Some(json!({"_tag": "Failure"})),
        ..Reply::default()
    };
    for (name, reply) in [
        ("the list is refused", refused),
        ("unknown shape", mcp_reply(json!({"servers": []}))),
        (
            "data is no list",
            mcp_reply(json!({"data": {"canterlot": {}}})),
        ),
        ("the list stays empty", mcp_reply(json!({"data": []}))),
    ] {
        let mut bench = isolated(vec![reply]);
        assert_eq!(bench.fail_reason().await, "mcp_isolation_failed", "{name}");
    }
}

#[tokio::test]
async fn isolated_start_refuses_a_foreign_server() {
    let mut bench = isolated(vec![mcp_reply(json!({"data": [
        {"name": "canterlot", "status": {"status": "connected"}},
        {"name": "nanokvm", "status": {"status": "pending"}},
    ]}))]);
    assert_eq!(bench.fail_reason().await, "mcp_isolation_failed");
}

#[tokio::test]
async fn isolated_start_goes_on_when_the_list_proves_there_is_only_canterlot() {
    let mut bench = isolated(vec![mcp_reply(json!({"data": [
        {"name": "canterlot", "status": {"status": "pending"}},
    ]}))]);
    bench.start_ok().await;
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Diagnostic {
            kind: "mcp_unavailable",
            required: false,
            ..
        }
    ));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn owner_session_still_starts_when_the_server_list_is_refused() {
    let mut script = session_only();
    let refused = Reply {
        status: 500,
        body: Some(json!({"_tag": "Failure"})),
        ..Reply::default()
    };
    script.set_route("GET", "/api/mcp", vec![refused]);
    let mut bench = Bench::new(script);
    bench.adapter.set_ready_wait(Duration::from_millis(300));
    bench.start_ok().await;
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Diagnostic {
            kind: "mcp_unavailable",
            required: false,
            ..
        }
    ));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn agents_are_polled_until_the_location_is_up() {
    let mut bench = Bench::new(session_only());
    bench.start_ok().await;
    let polls = bench
        .fake
        .requests()
        .iter()
        .filter(|request| path_of(request) == "/api/agent")
        .count();
    assert!(polls >= 2, "the capture starts with an empty list: {polls}");
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn empty_agent_list_past_the_deadline_fails_the_start() {
    let mut script = session_only();
    let empty = script.route("GET", "/api/agent").unwrap().replies[0].clone();
    script.set_route("GET", "/api/agent", vec![empty]);
    let mut bench = Bench::new(script);
    bench.adapter.set_ready_wait(Duration::from_millis(300));
    assert_eq!(bench.fail_reason().await, "agents_unavailable");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn refused_session_fails_the_start() {
    let mut script = session_only();
    let last = script.steps.len() - 1;
    if let Step::Expect { reply, .. } = &mut script.steps[last] {
        *reply = Reply {
            status: 400,
            body: Some(json!({"_tag": "InvalidRequestError", "message": "секрет"})),
            ..Default::default()
        };
    }
    let mut bench = Bench::new(script);
    assert_eq!(bench.fail_reason().await, "session_create_failed");
    bench.fake.wait_gone().await;
}

fn resume_script(found: bool) -> (Script, String) {
    let id = "ses_11112222333344445555666677778888".to_owned();
    let mut script = ready();
    script.steps.clear();
    let session = if found {
        Reply {
            body: Some(json!({"data": {"id": id, "title": "canterlot"}})),
            ..Default::default()
        }
    } else {
        Reply {
            status: 404,
            body: Some(json!({"_tag": "SessionNotFoundError", "sessionID": id})),
            ..Default::default()
        }
    };
    script.set_route("GET", &format!("/api/session/{id}"), vec![session]);
    for (method, path) in [
        ("PATCH", format!("/api/session/{id}")),
        ("POST", format!("/api/session/{id}/model")),
    ] {
        script.set_route(
            method,
            &path,
            vec![Reply {
                status: 204,
                ..Default::default()
            }],
        );
    }
    (script, id)
}

#[tokio::test]
async fn resume_reapplies_rules_and_model_to_the_same_session() {
    let (script, id) = resume_script(true);
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(id.clone());
    bench.spec.permission_mode = "read_only".into();
    let started = bench.start_ok().await;
    assert_eq!(started.native_conversation_id, id);
    let requests = bench.fake.requests();
    assert!(find(&requests, "POST", "/api/session").is_none());
    let patch = find(&requests, "PATCH", &id).unwrap();
    assert_eq!(patch["body"]["permissions"][0]["effect"], "deny");
    let model = find(&requests, "POST", "/model").unwrap();
    assert_eq!(
        model["body"]["model"],
        json!({"id": "fake-1", "providerID": "fake", "variant": "high"})
    );
    bench.close(CloseMode::Graceful).await;
}

// Правила прошлого режима лежат в базе: `owner` без явной очистки унаследовал бы их.
#[tokio::test]
async fn resume_in_owner_mode_clears_the_rules_of_the_session() {
    let (script, id) = resume_script(true);
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(id.clone());
    bench.spec.permission_mode = "owner".into();
    bench.start_ok().await;
    let requests = bench.fake.requests();
    let patch = find(&requests, "PATCH", &id).expect("rules");
    assert_eq!(patch["body"], json!({"permissions": []}));
    bench.close(CloseMode::Graceful).await;
}

// Потомок копирует правила один раз, при создании, а субагент продолжает его по `sessionID`:
// без этого запрос после возобновления обошёл бы `read_only` по старым правилам потомка.
#[tokio::test]
async fn resume_reapplies_rules_to_old_children() {
    let (mut script, id) = resume_script(true);
    script.set_route(
        "GET",
        "/api/session",
        vec![Reply {
            body: Some(json!({"data": [{"id": "ses_child"}, {"id": "ses_gone"}]})),
            ..Default::default()
        }],
    );
    script.set_route("PATCH", "/api/session/ses_child", vec![reply_204()]);
    script.set_route(
        "PATCH",
        "/api/session/ses_gone",
        vec![Reply {
            status: 404,
            body: Some(json!({"_tag": "SessionNotFoundError"})),
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(id);
    bench.spec.permission_mode = "read_only".into();
    bench.start_ok().await;
    let requests = bench.fake.requests();
    let patch = find(&requests, "PATCH", "ses_child").expect("rules of the child");
    assert_eq!(patch["body"]["permissions"][0]["effect"], "deny");
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn resume_fails_when_a_child_keeps_its_old_rules() {
    let (mut script, id) = resume_script(true);
    script.set_route(
        "GET",
        "/api/session",
        vec![Reply {
            body: Some(json!({"data": [{"id": "ses_child"}]})),
            ..Default::default()
        }],
    );
    script.set_route(
        "PATCH",
        "/api/session/ses_child",
        vec![Reply {
            status: 500,
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(id);
    assert_eq!(bench.fail_reason().await, "resume_failed");
    bench.fake.wait_gone().await;
}

// У корневой сессии могут быть сотни потомков, а старый продолжается и с дальней страницы.
#[tokio::test]
async fn children_are_read_past_the_first_page() {
    let (mut script, id) = resume_script(true);
    let first: Vec<Value> = (0..100)
        .map(|n| json!({"id": format!("ses_{n}")}))
        .collect();
    script.set_route(
        "GET",
        "/api/session",
        vec![
            Reply {
                body: Some(json!({"data": first, "cursor": {"next": "page2"}})),
                ..Default::default()
            },
            Reply {
                body: Some(json!({"data": [{"id": "ses_far"}], "cursor": {"next": "page3"}})),
                ..Default::default()
            },
        ],
    );
    script.set_route("PATCH", "/api/session/ses_far", vec![reply_204()]);
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(id);
    bench.start_ok().await;
    let requests = bench.fake.requests();
    let queries: Vec<&str> = requests
        .iter()
        .filter(|request| request["method"] == "GET" && path_of(request) == "/api/session")
        .map(|request| request["query"].as_str().unwrap())
        .collect();
    assert_eq!(queries.len(), 2, "{queries:?}");
    assert_eq!(queries[1], "limit=100&cursor=page2");
    assert!(find(&requests, "PATCH", "ses_far").is_some());
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn resume_of_a_lost_session_fails_the_start() {
    let (script, id) = resume_script(false);
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(id);
    assert_eq!(bench.fail_reason().await, "resume_failed");
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn graceful_close_uses_the_stdin_lease() {
    let mut bench = Bench::new(session_only());
    bench.start_ok().await;
    assert!(!bench.fake.pids().is_empty());
    let started = std::time::Instant::now();
    bench.close(CloseMode::Graceful).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn graceful_close_escalates_to_term_then_kill() {
    let mut script = session_only();
    script.ignore_eof = true;
    let mut bench = Bench::new(script);
    bench.adapter.set_stop_grace(Duration::from_millis(100));
    bench.start_ok().await;
    bench.close(CloseMode::Graceful).await;
    bench.fake.wait_gone().await;

    let mut script = session_only();
    script.ignore_eof = true;
    script.ignore_term = true;
    let mut bench = Bench::new(script);
    bench.adapter.set_stop_grace(Duration::from_millis(100));
    bench.start_ok().await;
    bench.close(CloseMode::Graceful).await;
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn force_close_kills_at_once() {
    let mut script = session_only();
    script.ignore_eof = true;
    script.ignore_term = true;
    let mut bench = Bench::new(script);
    bench.adapter.set_stop_grace(Duration::from_secs(30));
    bench.start_ok().await;
    let started = std::time::Instant::now();
    bench.close(CloseMode::Force).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn dropped_adapter_leaves_no_server_behind() {
    let mut bench = Bench::new(session_only());
    bench.start_ok().await;
    let Bench { fake, adapter, .. } = bench;
    drop(adapter);
    fake.wait_gone().await;
}

#[tokio::test]
async fn cancelled_start_leaves_no_server_behind() {
    let mut script = session_only();
    script.set_route(
        "GET",
        "/api/mcp",
        vec![Reply {
            stall_ms: 30_000,
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    let cut = tokio::time::timeout(
        Duration::from_millis(500),
        bench.adapter.start(&bench.spec, &bench.group),
    )
    .await;
    assert!(cut.is_err());
    let Bench { fake, adapter, .. } = bench;
    drop(adapter);
    fake.wait_gone().await;
}

#[tokio::test]
async fn server_exit_reaches_the_supervisor() {
    let mut script = session_only();
    script.steps.push(Step::Stall { ms: 1500 });
    script.steps.push(Step::Die {
        code: 9,
        kill: false,
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    loop {
        match bench.event().await {
            AdapterEvent::ProcessExited { code, signal } => {
                assert_eq!((code, signal), (Some(9), None));
                break;
            }
            AdapterEvent::Diagnostic { .. } => {}
            other => panic!("{other:?}"),
        }
    }
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn second_start_is_refused() {
    let mut bench = Bench::new(session_only());
    bench.start_ok().await;
    assert!(matches!(
        bench.start().await,
        Err(AdapterError::AlreadyStarted)
    ));
    bench.close(CloseMode::Graceful).await;
}

#[test]
fn registry_builds_the_adapter_from_context_with_supervisor_paths() {
    use canterlot_worker::adapter::{AdapterContext, registry_with};
    let context = AdapterContext {
        transmit_deadline: Duration::from_secs(1),
        ready_deadline: Duration::from_secs(1),
        exec_dir: PathBuf::from("/tmp/x"),
        claude_code: None,
        codex: None,
        opencode: Some(OpenCodeConfig::default()),
        antigravity: None,
    };
    let adapter = registry_with("opencode", &context).unwrap();
    assert!(adapter.capabilities().interrupt_keeps_process);
}

#[tokio::test]
async fn first_turn_runs_from_prompt_to_run_finished() {
    let mut bench = Bench::new(ready());
    let started = bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert_eq!(text_of(&events, "Reasoning"), ["think more"]);
    assert_eq!(text_of(&events, "Text"), ["Hello world"]);
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            ..
        }
    ));
    let AdapterEvent::RunFinished {
        usage,
        context_tokens,
        context_window_tokens,
        ..
    } = events.last().unwrap()
    else {
        unreachable!();
    };
    assert_eq!(usage[0].input_tokens, 70);
    assert_eq!(usage[0].output_tokens, 20);
    assert_eq!(*context_tokens, 100);
    assert_eq!(*context_window_tokens, 200_000);
    let requests = bench.fake.requests();
    let prompt = find(&requests, "POST", "/prompt").unwrap();
    assert_eq!(
        path_of(prompt),
        format!("/api/session/{}/prompt", started.native_conversation_id)
    );
    assert_eq!(
        prompt["body"],
        json!({
            "id": format!("msg_{}", input.command_id.simple()),
            "text": "say hello",
            "delivery": "queue",
            "metadata": {"canterlot_command": input.command_id.to_string()},
        })
    );
    bench.close(CloseMode::Graceful).await;
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn two_turns_in_one_process_each_get_their_end() {
    let mut bench = Bench::new(two_turns());
    bench.start_ok().await;
    let first = bench.input("remember");
    bench.accepted(&first).await;
    let events = bench.until_finished(first.command_id).await;
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AdapterEvent::RunFinished { .. }))
            .count(),
        1
    );
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            ..
        }
    ));
    let second = bench.input("what did I say");
    bench.accepted(&second).await;
    bench.until_finished(second.command_id).await;
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn second_input_during_a_turn_is_rejected_not_steered() {
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .rposition(|step| matches!(step, Step::Emit { .. }))
        .unwrap();
    script.steps.insert(position, Step::Stall { ms: 300 });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let first = bench.input("one");
    bench.accepted(&first).await;
    let second = bench.input("two");
    assert!(matches!(
        bench.transmit(&second).await,
        TransmitOutcome::Rejected(reason) if reason == "turn_active"
    ));
    bench.until_finished(first.command_id).await;
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn refused_prompt_names_the_error_and_leaves_the_session_usable() {
    for (status, tag) in [
        (400, "InvalidRequestError"),
        (409, "ConflictError"),
        (404, "SessionNotFoundError"),
    ] {
        let mut script = ready();
        set_prompt_reply(&mut script, |reply| {
            *reply = Reply {
                status,
                body: Some(json!({"_tag": tag, "message": "текст для модели"})),
                ..Default::default()
            };
        });
        let position = script
            .steps
            .iter()
            .position(|step| {
                step.expected_path()
                    .is_some_and(|path| path.ends_with("/prompt"))
            })
            .unwrap();
        script.steps.truncate(position + 1);
        let mut bench = Bench::new(script);
        bench.start_ok().await;
        let input = bench.input("x");
        match bench.transmit(&input).await {
            TransmitOutcome::Rejected(reason) => assert_eq!(reason, tag),
            other => panic!("{other:?}"),
        }
        let next = bench.input("y");
        assert!(!matches!(
            bench.transmit(&next).await,
            TransmitOutcome::Rejected(reason) if reason == "turn_active"
        ));
        bench.close(CloseMode::Graceful).await;
    }
}

#[tokio::test]
async fn foreign_id_in_the_reply_is_not_a_proof() {
    let mut script = ready();
    set_prompt_reply(&mut script, |reply| {
        reply.body = Some(json!({"data": {"id": "msg_someone_else"}}));
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Undetermined
    ));
    bench.close(CloseMode::Force).await;
}

// 500 ничего не доказывает, а автоматического повтора нет (D-6): запись не найдена,
// исполнение освобождается, ввод остаётся неподтверждённым.
#[tokio::test]
async fn server_error_on_prompt_stays_unconfirmed_and_frees_the_execution() {
    let mut script = ready();
    set_prompt_reply(&mut script, |reply| {
        *reply = Reply {
            status: 500,
            body: Some(json!({"_tag": "UnknownError"})),
            ..Default::default()
        };
    });
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(position + 1);
    let mut bench = Bench::new(script);
    let started = bench.start_ok().await;
    let id = started.native_conversation_id;
    let input = bench.input("x");
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Undetermined
    ));
    let events = bench.until_finished(input.command_id).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::InputAccepted { .. }))
    );
    let requests = bench.fake.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| path_of(request).ends_with("/prompt"))
            .count(),
        1,
        "no automatic resend"
    );
    assert!(find(&requests, "GET", &format!("{id}/inbox")).is_some());
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn timed_out_prompt_is_proven_late_by_the_event_stream() {
    let mut script = ready();
    set_prompt_reply(&mut script, |reply| reply.stall_ms = 30_000);
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Undetermined
    ));
    let events = bench.until_finished(input.command_id).await;
    let accepted = events
        .iter()
        .filter(|event| {
            matches!(event, AdapterEvent::InputAccepted { command_id } if *command_id == input.command_id)
        })
        .count();
    assert_eq!(accepted, 1, "{events:?}");
    bench.close(CloseMode::Force).await;
}

// Потока нет, доказательство только в проекции.
#[tokio::test]
async fn timed_out_prompt_is_proven_late_by_the_inbox_projection() {
    let mut script = ready();
    set_prompt_reply(&mut script, |reply| reply.stall_ms = 30_000);
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(position + 1);
    let mut bench = Bench::new(script);
    let started = bench.start_ok().await;
    let input = bench.input("x");
    bench_route_inbox(&bench, &started.native_conversation_id, &input);
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Undetermined
    ));
    assert!(matches!(
        bench.event().await,
        AdapterEvent::InputAccepted { command_id } if command_id == input.command_id
    ));
    bench.close(CloseMode::Force).await;
}

fn bench_route_inbox(bench: &Bench, session: &str, input: &Transmission) {
    // Маршрут пишется в журнал двойника файлом: сценарий уже загружен.
    let id = format!("msg_{}", input.command_id.simple());
    let body = json!({"data": [{"id": id, "sessionID": session, "type": "user"}]});
    bench
        .fake
        .write_override("GET", &format!("/api/session/{session}/inbox"), &body);
}

#[tokio::test]
async fn repeated_attempt_checks_history_and_never_resends() {
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(position);
    let mut bench = Bench::new(script);
    let started = bench.start_ok().await;
    let mut input = bench.input("x");
    input.attempt = 2;
    let session = &started.native_conversation_id;
    let id = format!("msg_{}", input.command_id.simple());
    bench.fake.write_override(
        "GET",
        &format!("/api/session/{session}/message/{id}"),
        &json!({"data": {"id": id, "type": "user"}}),
    );
    match bench.transmit(&input).await {
        TransmitOutcome::Accepted(proof) => assert_eq!(proof.kind, "opencode_history"),
        other => panic!("{other:?}"),
    }
    bench.until_finished(input.command_id).await;
    let requests = bench.fake.requests();
    assert!(find(&requests, "POST", "/prompt").is_none());
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn repeated_attempt_without_history_is_sent_once() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let mut input = bench.input("say hello");
    input.attempt = 2;
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn cyrillic_text_survives_the_stream_and_the_input() {
    let mut script = ready();
    for step in &mut script.steps {
        let Step::Emit { events } = step else {
            continue;
        };
        for event in events {
            if let Some(delta) = event["data"].get_mut("delta")
                && let Some(value) = delta.as_str()
            {
                *delta = value.replace("Hello ", "Привет, ").into();
            }
            if let Some(text) = event["data"].get_mut("text")
                && let Some(value) = text.as_str()
            {
                *text = value.replace("Hello ", "Привет, ").into();
            }
        }
    }
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("Скажи привет");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert_eq!(text_of(&events, "Text"), ["Привет, world"]);
    let requests = bench.fake.requests();
    assert_eq!(
        find(&requests, "POST", "/prompt").unwrap()["body"]["text"],
        "Скажи привет"
    );
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn attachments_go_as_paths_in_the_text_until_their_slice() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let mut input = bench.input("смотри");
    input.files = vec![canterlot_worker::adapter::LocalFile {
        path: "/tmp/отчёт.pdf".into(),
        name: "отчёт.pdf".into(),
        mime: "application/pdf".into(),
        size: 5,
        image: false,
    }];
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    let requests = bench.fake.requests();
    let body = &find(&requests, "POST", "/prompt").unwrap()["body"];
    assert!(body["text"].as_str().unwrap().contains("/tmp/отчёт.pdf"));
    assert!(body.get("files").is_none());
    bench.close(CloseMode::Graceful).await;
}

fn with_model_route(mut script: Script, replies: Vec<Reply>) -> Script {
    let session = session_of(&script);
    script.set_route("POST", &format!("/api/session/{session}/model"), replies);
    script
}

fn model_change(model: Option<&str>, effort: Option<&str>) -> SettingsChange {
    SettingsChange {
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        ..Default::default()
    }
}

// Сервер принимает неизвестный вариант с 204 и падает только на ходе: проверка раньше него.
#[tokio::test]
async fn unknown_model_and_variant_are_refused_before_the_server() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    for (model, effort) in [
        (Some("fake/other"), None),
        (None, Some("ultra")),
        (Some("opencode/big-pickle"), Some("high")),
    ] {
        let mut input = bench.input("x");
        input.settings = Some(model_change(model, effort));
        assert_eq!(
            bench.adapter.validate_transmission(&input),
            Err("settings_unavailable"),
            "{model:?} {effort:?}"
        );
    }
    let mut same = bench.input("x");
    same.settings = Some(SettingsChange {
        model: Some("fake/fake-1".into()),
        effort: Some("high".into()),
        permission_mode: Some("bypass".into()),
        instructions: None,
    });
    assert_eq!(bench.adapter.validate_transmission(&same), Ok(()));
    let mut known = bench.input("x");
    known.settings = Some(model_change(Some("opencode/flint-40b-free"), Some("max")));
    assert_eq!(bench.adapter.validate_transmission(&known), Ok(()));
    let mut plain = bench.input("x");
    plain.settings = Some(model_change(Some("opencode/big-pickle"), Some("default")));
    assert_eq!(bench.adapter.validate_transmission(&plain), Ok(()));
    bench.close(CloseMode::Graceful).await;
}

// Смена модели и усилия: запрос раньше ввода, вариант вместе с моделью.
#[tokio::test]
async fn model_change_goes_before_the_input() {
    let mut bench = Bench::new(with_model_route(ready(), vec![reply_204()]));
    bench.start_ok().await;
    let mut input = bench.input("x");
    input.settings = Some(model_change(Some("opencode/flint-40b-free"), Some("max")));
    bench.accepted(&input).await;
    let requests = bench.fake.requests();
    let model = position(&requests, "POST", "/model");
    assert!(model < position(&requests, "POST", "/prompt"));
    assert_eq!(
        requests[model]["body"],
        json!({"model": {"id": "flint-40b-free", "providerID": "opencode", "variant": "max"}})
    );
    bench.close(CloseMode::Graceful).await;
}

// Только усилие: модель остаётся текущей.
#[tokio::test]
async fn effort_change_keeps_the_model() {
    let mut bench = Bench::new(with_model_route(ready(), vec![reply_204()]));
    bench.start_ok().await;
    let mut input = bench.input("x");
    input.settings = Some(model_change(None, Some("low")));
    bench.accepted(&input).await;
    let requests = bench.fake.requests();
    let model = position(&requests, "POST", "/model");
    assert_eq!(
        requests[model]["body"],
        json!({"model": {"id": "fake-1", "providerID": "fake", "variant": "low"}})
    );
    bench.close(CloseMode::Graceful).await;
}

// У модели без вариантов усилие каталога условное, серверу оно не передаётся.
#[tokio::test]
async fn model_without_variants_goes_without_one() {
    let mut bench = Bench::new(with_model_route(ready(), vec![reply_204()]));
    bench.start_ok().await;
    let mut input = bench.input("x");
    input.settings = Some(model_change(Some("opencode/big-pickle"), Some("default")));
    bench.accepted(&input).await;
    let requests = bench.fake.requests();
    assert_eq!(
        requests[position(&requests, "POST", "/model")]["body"],
        json!({"model": {"id": "big-pickle", "providerID": "opencode"}})
    );
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn same_model_sends_no_change() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let mut input = bench.input("x");
    input.settings = Some(model_change(Some("fake/fake-1"), Some("high")));
    bench.accepted(&input).await;
    assert!(find(&bench.fake.requests(), "POST", "/model").is_none());
    bench.close(CloseMode::Graceful).await;
}

// Отказ в смене: ввод не уходит, следующий ввод возвращает то, что считает текущим хаб.
#[tokio::test]
async fn failed_model_change_stops_the_input_and_returns_the_settings() {
    let failed = Reply {
        status: 500,
        body: Some(json!({"_tag": "InternalError"})),
        ..Default::default()
    };
    let script = with_model_route(
        with_rules_route(ready(), vec![reply_204()]),
        vec![failed, reply_204()],
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let mut refused = bench.input("x");
    refused.settings = Some(SettingsChange {
        permission_mode: Some("ask".into()),
        ..model_change(Some("opencode/flint-40b-free"), Some("max"))
    });
    assert!(matches!(
        bench.transmit(&refused).await,
        TransmitOutcome::Rejected(reason) if reason == "settings_unavailable"
    ));
    let next = bench.input("y");
    bench.accepted(&next).await;
    let requests = bench.fake.requests();
    let models: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "POST" && path_of(request).ends_with("/model"))
        .map(|request| request["body"]["model"]["id"].clone())
        .collect();
    assert_eq!(models, [json!("flint-40b-free"), json!("fake-1")]);
    // Модель идёт первой: при её отказе режим не менялся, открывать его нечему.
    assert!(requests.iter().all(|request| request["method"] != "PATCH"));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn failed_rules_return_the_changed_model() {
    let failed = Reply {
        status: 500,
        body: Some(json!({"_tag": "InternalError"})),
        ..Default::default()
    };
    let script = with_model_route(
        with_rules_route(ready(), vec![failed, reply_204()]),
        vec![reply_204()],
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let mut refused = bench.input("x");
    refused.settings = Some(SettingsChange {
        permission_mode: Some("ask".into()),
        ..model_change(Some("opencode/flint-40b-free"), Some("max"))
    });
    assert!(matches!(
        bench.transmit(&refused).await,
        TransmitOutcome::Rejected(reason) if reason == "settings_unavailable"
    ));
    let next = bench.input("y");
    bench.accepted(&next).await;
    let requests = bench.fake.requests();
    let models: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "POST" && path_of(request).ends_with("/model"))
        .map(|request| request["body"]["model"]["id"].clone())
        .collect();
    assert_eq!(models, [json!("flint-40b-free"), json!("fake-1")]);
    let effects: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "PATCH")
        .map(|request| request["body"]["permissions"][0]["effect"].clone())
        .collect();
    assert_eq!(effects, [json!("ask"), json!("allow")]);
    bench.close(CloseMode::Graceful).await;
}

fn catalog_reply(models: Value) -> Reply {
    Reply {
        body: Some(json!({"data": models})),
        ..Default::default()
    }
}

/// Модели записи `models` и настоящие записи Go: `evidence/opencode/live/catalog.jsonl`.
fn recorded_catalog() -> Vec<Value> {
    let mut models: Vec<Value> = Script::default()
        .with_recorded_models()
        .route("GET", "/api/model")
        .unwrap()
        .replies[0]
        .body
        .as_ref()
        .unwrap()["data"]
        .as_array()
        .unwrap()
        .clone();
    let go = std::fs::read_to_string(evidence_path("live/catalog")).unwrap();
    models.extend(go.lines().filter_map(|line| {
        let record: Value = serde_json::from_str(line).unwrap();
        (record["dir"] == "http_in").then(|| record["data"].clone())
    }));
    models
}

fn verified_cli() -> Script {
    Script {
        version: "opencode v2.1.4".into(),
        ..Default::default()
    }
}

async fn discovered(script: Script, providers: &[&str]) -> Result<Vec<ModelEntry>, DiscoverError> {
    let fake = Fake::new(script);
    let data = fake.dir.path().join("data");
    let cfg = OpenCodeConfig {
        executable: fake.exe.clone(),
        providers: providers.iter().map(|name| (*name).to_owned()).collect(),
        ..Default::default()
    };
    let found = tokio::time::timeout(WAIT, discover(&cfg, &data))
        .await
        .expect("discover must finish");
    fake.wait_gone().await;
    found.map(|catalog| catalog.models)
}

// Решение 2: в каталоге Go и явно включённые провайдеры, идентификатор с провайдером.
#[tokio::test]
async fn discover_lists_go_and_the_enabled_providers() {
    let mut script = verified_cli();
    script.set_route(
        "GET",
        "/api/model",
        vec![catalog_reply(Value::Array(recorded_catalog()))],
    );
    script.set_route(
        "GET",
        "/api/model/default",
        vec![Reply {
            body: Some(json!({"data": {"id": "fake-1", "providerID": "fake"}})),
            ..Default::default()
        }],
    );
    let models = discovered(script.clone(), &["fake"]).await.unwrap();
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "fake/fake-1",
            "opencode-go/mimo-v2.6-pro",
            "opencode-go/muse-spark-1.3-contributor",
            "opencode-go/glm-5.3",
        ]
    );
    assert_eq!(models[0].efforts, ["low", "medium", "high"]);
    assert_eq!(models[1].efforts, ["default"], "a model without variants");
    assert_eq!(models[3].images, Some(false));
    let muse = &models[2];
    assert_eq!(muse.title, "Muse Spark 1.3 Contributor");
    assert_eq!(muse.efforts, ["minimal", "low", "medium", "high", "xhigh"]);
    assert_eq!(muse.images, Some(true));
    assert_eq!(muse.context_window, Some(1_048_576));
    let only_go = discovered(script, &[]).await.unwrap();
    assert_eq!(only_go.len(), 3);
    assert!(
        only_go
            .iter()
            .all(|model| model.id.starts_with("opencode-go/"))
    );
}

/// Настоящий ответ `GET /api/model`, пока плагины владельца не активировались:
/// `evidence/opencode/live/plugins_503.jsonl`. Сервер ждёт их 5 с и отвечает этим.
fn plugins_activating() -> Reply {
    Reply {
        status: 503,
        body: Some(json!({
            "_tag": "ServiceUnavailableError",
            "message": "Location plugins did not finish activating",
            "service": "plugin",
        })),
        ..Default::default()
    }
}

// Смоук: перезапуск воркера с плагином владельца убрал харнесс из каталога до следующего старта.
#[tokio::test]
async fn discover_waits_out_the_activation_of_plugins() {
    let mut script = verified_cli();
    script.set_route(
        "GET",
        "/api/model",
        vec![
            plugins_activating(),
            catalog_reply(Value::Array(recorded_catalog())),
        ],
    );
    let models = discovered(script, &[]).await.unwrap();
    assert_eq!(models.len(), 3);
}

// Иначе ход идёт без окна контекста, вариантов и признака картинок, и молча.
#[tokio::test]
async fn start_waits_out_the_activation_of_plugins_for_the_model_list() {
    let mut script = ready();
    let models = script.route("GET", "/api/model").unwrap().replies.clone();
    script.set_route(
        "GET",
        "/api/model",
        [vec![plugins_activating()], models].concat(),
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    let AdapterEvent::RunFinished {
        context_window_tokens,
        ..
    } = events.last().unwrap()
    else {
        unreachable!();
    };
    assert_eq!(*context_window_tokens, 200_000);
    bench.close(CloseMode::Graceful).await;
}

// Без подписки каталог пуст, но харнесс виден: `[opencode]` настроен.
#[tokio::test]
async fn discover_without_available_models_is_not_a_failure() {
    let mut script = verified_cli();
    script.set_route("GET", "/api/model", vec![catalog_reply(json!([]))]);
    assert_eq!(discovered(script, &[]).await.unwrap(), []);
}

#[tokio::test]
async fn discover_of_a_dead_server_hides_the_harness() {
    let mut script = verified_cli();
    script.ready.exit = Some(1);
    assert!(matches!(
        discovered(script, &[]).await,
        Err(DiscoverError::Catalog)
    ));
}

// Проверка при старте воркера пишет только в каталоги canterlot.
#[tokio::test]
async fn discover_keeps_the_probe_inside_canterlot_data() {
    let mut script = verified_cli();
    script.set_route("GET", "/api/model", vec![catalog_reply(json!([]))]);
    let fake = Fake::new(script);
    let data = fake.dir.path().join("data");
    // `--version` тоже заводит каталоги и журнал: обёртка видит окружение обоих запусков.
    let seen = fake.dir.path().join("seen");
    let wrapper = fake.dir.path().join("wrapper");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\necho \"$XDG_DATA_HOME\" >> '{}'\nexec '{}' \"$@\"\n",
            seen.display(),
            fake.exe.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cfg = OpenCodeConfig {
        executable: wrapper,
        ..Default::default()
    };
    discover(&cfg, &data).await.unwrap();
    fake.wait_gone().await;
    let expected = data.join("opencode/data");
    let runs: Vec<String> = std::fs::read_to_string(seen)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        runs,
        [expected.to_str().unwrap(), expected.to_str().unwrap()]
    );
    let boot = fake.boot();
    assert_eq!(
        boot["env"]["XDG_DATA_HOME"],
        data.join("opencode/data").to_str().unwrap()
    );
    assert_eq!(
        boot["env"]["XDG_STATE_HOME"],
        data.join("opencode/state").to_str().unwrap()
    );
    assert_eq!(boot["password_set"], true);
    assert!(
        fake.requests()
            .iter()
            .all(|request| request["authorized"] == true)
    );
}

fn with_rules_route(mut script: Script, replies: Vec<Reply>) -> Script {
    let session = session_of(&script);
    script.set_route("PATCH", &format!("/api/session/{session}"), replies);
    script
}

fn mode_change(mode: &str) -> SettingsChange {
    SettingsChange {
        permission_mode: Some(mode.into()),
        ..Default::default()
    }
}

fn position(requests: &[Value], method: &str, tail: &str) -> usize {
    requests
        .iter()
        .position(|request| request["method"] == method && path_of(request).ends_with(tail))
        .unwrap_or_else(|| panic!("{method} {tail}: {requests:?}"))
}

// Смена режима на ходу: новые правила раньше ввода, режим отметок меняется вместе с ними.
#[tokio::test]
async fn mode_change_goes_before_the_input() {
    let mut bench = Bench::new(with_rules_route(two_turns(), vec![reply_204()]));
    bench.start_ok().await;
    let first = bench.input("раз");
    bench.accepted(&first).await;
    bench.until_finished(first.command_id).await;
    let mut second = bench.input("два");
    second.settings = Some(mode_change("read_only"));
    assert_eq!(bench.adapter.validate_transmission(&second), Ok(()));
    bench.accepted(&second).await;
    let requests = bench.fake.requests();
    let patches: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "PATCH")
        .collect();
    assert_eq!(patches.len(), 1, "{requests:?}");
    assert_eq!(patches[0]["body"]["permissions"][0]["effect"], "deny");
    let prompts: Vec<usize> = requests
        .iter()
        .enumerate()
        .filter(|(_, request)| path_of(request).ends_with("/prompt"))
        .map(|(index, _)| index)
        .collect();
    assert!(position(&requests, "PATCH", "") < prompts[1]);
    assert!(prompts[0] < position(&requests, "PATCH", ""));
    bench.close(CloseMode::Graceful).await;
}

// `null` сервер игнорирует: снять правила можно только пустым списком.
#[tokio::test]
async fn switching_to_owner_clears_the_session_rules() {
    let mut bench = Bench::new(with_rules_route(ready(), vec![reply_204()]));
    bench.start_ok().await;
    let mut input = bench.input("x");
    input.settings = Some(mode_change("owner"));
    bench.accepted(&input).await;
    let requests = bench.fake.requests();
    let patch = &requests[position(&requests, "PATCH", "")];
    assert_eq!(patch["body"], json!({"permissions": []}));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn same_mode_sends_no_rules() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let mut input = bench.input("x");
    input.settings = Some(mode_change("bypass"));
    bench.accepted(&input).await;
    assert!(
        bench
            .fake
            .requests()
            .iter()
            .all(|request| request["method"] != "PATCH")
    );
    bench.close(CloseMode::Graceful).await;
}

fn prompt_texts(requests: &[Value]) -> Vec<String> {
    requests
        .iter()
        .filter(|request| request["method"] == "POST" && path_of(request).ends_with("/prompt"))
        .map(|request| request["body"]["text"].as_str().unwrap().to_owned())
        .collect()
}

// Сервер при выходе из `read_only` сам присылает модели `<system-update>` с Code Mode, и она
// перестаёт звать `shell` напрямую: первый ввод после выхода несёт напоминание, один раз.
#[tokio::test]
async fn leaving_read_only_adds_a_note_to_the_first_input_only() {
    let mut bench = Bench::new(with_rules_route(two_turns(), vec![reply_204()]));
    bench.spec.permission_mode = "read_only".into();
    bench.start_ok().await;
    let mut first = bench.input("раз");
    first.settings = Some(mode_change("bypass"));
    bench.accepted(&first).await;
    bench.until_finished(first.command_id).await;
    let second = bench.input("два");
    bench.accepted(&second).await;
    let texts = prompt_texts(&bench.fake.requests());
    assert!(texts[0].ends_with("\n\nраз"), "{}", texts[0]);
    assert!(texts[0].contains("`shell`") && texts[0].contains("`question`"));
    assert!(texts[0].starts_with("<system-reminder>"), "{}", texts[0]);
    assert_eq!(texts[1], "два");
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn other_mode_changes_leave_the_input_as_typed() {
    let mut bench = Bench::new(with_rules_route(
        two_turns(),
        vec![reply_204(), reply_204()],
    ));
    bench.start_ok().await;
    let mut first = bench.input("раз");
    first.settings = Some(mode_change("read_only"));
    bench.accepted(&first).await;
    bench.until_finished(first.command_id).await;
    let second = bench.input("два");
    bench.accepted(&second).await;
    assert_eq!(prompt_texts(&bench.fake.requests()), ["раз", "два"]);
    bench.close(CloseMode::Graceful).await;
}

// Хаб нового режима не принял: выход из `read_only` не состоялся, напоминание ждёт повтора.
#[tokio::test]
async fn refused_input_keeps_the_note_owed_to_the_next_one() {
    let mut script = with_rules_route(ready(), vec![reply_204(), reply_204(), reply_204()]);
    set_prompt_reply(&mut script, |reply| {
        *reply = Reply {
            status: 400,
            body: Some(json!({"_tag": "InvalidRequestError"})),
            ..Default::default()
        };
    });
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "read_only".into();
    bench.start_ok().await;
    let mut input = bench.input("раз");
    input.settings = Some(mode_change("bypass"));
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Rejected(_)
    ));
    let mut again = bench.input("раз");
    again.settings = Some(mode_change("bypass"));
    let _ = bench.transmit(&again).await;
    let texts = prompt_texts(&bench.fake.requests());
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(
        texts.iter().all(|text| text.contains("`shell`")),
        "{texts:?}"
    );
    bench.close(CloseMode::Graceful).await;
}

// Потомок получает копию правил при создании: `read_only` родителя его не трогает сам.
#[tokio::test]
async fn mode_change_reaches_the_live_children() {
    let capture = two_turns();
    let session = session_of(&capture);
    let child = "ses_child00000000000000000000000".to_owned();
    let mut script = with_rules_route(capture, vec![reply_204()]);
    script.set_route("PATCH", &format!("/api/session/{child}"), vec![reply_204()]);
    let created = script
        .steps
        .iter()
        .position(|step| matches!(step, Step::Emit { .. }))
        .unwrap();
    script.steps.insert(
        created,
        Step::Emit {
            events: vec![json!({"type": "session.created",
                "data": {"sessionID": child, "parentID": session}})],
        },
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let first = bench.input("раз");
    bench.accepted(&first).await;
    bench.until_finished(first.command_id).await;
    let mut second = bench.input("два");
    second.settings = Some(mode_change("read_only"));
    bench.accepted(&second).await;
    let requests = bench.fake.requests();
    let patched: Vec<&str> = requests
        .iter()
        .filter(|request| request["method"] == "PATCH")
        .map(path_of)
        .collect();
    assert_eq!(patched.len(), 2, "{requests:?}");
    assert!(patched.iter().any(|path| path.ends_with(&child)));
    bench.close(CloseMode::Graceful).await;
}

// Правила не применились: ввод не уходит, а следующий ввод применяет их снова.
#[tokio::test]
async fn failed_mode_change_stops_the_input_and_is_applied_again() {
    let failed = Reply {
        status: 500,
        body: Some(json!({"_tag": "InternalError"})),
        ..Default::default()
    };
    let mut bench = Bench::new(with_rules_route(ready(), vec![failed, reply_204()]));
    bench.start_ok().await;
    let mut refused = bench.input("x");
    refused.settings = Some(mode_change("ask"));
    assert!(matches!(
        bench.transmit(&refused).await,
        TransmitOutcome::Rejected(reason) if reason == "settings_unavailable"
    ));
    assert!(find(&bench.fake.requests(), "POST", "/prompt").is_none());
    let next = bench.input("y");
    bench.accepted(&next).await;
    let requests = bench.fake.requests();
    let patches: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "PATCH")
        .collect();
    assert_eq!(patches.len(), 2, "{requests:?}");
    // Режим остался прежним: на сервере могли остаться новые правила, их надо вернуть.
    assert_eq!(patches[1]["body"]["permissions"][0]["effect"], "allow");
    assert!(position(&requests, "PATCH", "") < position(&requests, "POST", "/prompt"));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn unknown_mode_in_a_settings_change_is_refused() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let mut input = bench.input("x");
    input.settings = Some(mode_change("yolo"));
    assert_eq!(
        bench.adapter.validate_transmission(&input),
        Err("settings_unavailable")
    );
    bench.close(CloseMode::Graceful).await;
}

// Отказ в приёме ввода оставляет прежний режим: хаб нового не применил.
#[tokio::test]
async fn refused_input_returns_the_previous_mode() {
    let mut script = with_rules_route(ready(), vec![reply_204()]);
    set_prompt_reply(&mut script, |reply| {
        *reply = Reply {
            status: 400,
            body: Some(json!({"_tag": "InvalidRequestError"})),
            ..Default::default()
        };
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let mut input = bench.input("x");
    input.settings = Some(mode_change("ask"));
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Rejected(reason) if reason == "InvalidRequestError"
    ));
    let next = bench.input("y");
    let _ = bench.transmit(&next).await;
    let requests = bench.fake.requests();
    let effects: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "PATCH")
        .map(|request| request["body"]["permissions"][0]["effect"].clone())
        .collect();
    assert_eq!(effects, [json!("ask"), json!("allow")]);
    bench.close(CloseMode::Graceful).await;
}

// Период без ввода может начаться раньше следующего ввода: `bypass`, который хаб не принял,
// не должен дождаться его на сервере.
#[tokio::test]
async fn refused_input_returns_the_previous_mode_at_once() {
    let mut script = with_rules_route(ready(), vec![reply_204()]);
    set_prompt_reply(&mut script, |reply| {
        *reply = Reply {
            status: 400,
            body: Some(json!({"_tag": "InvalidRequestError"})),
            ..Default::default()
        };
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let mut input = bench.input("x");
    input.settings = Some(mode_change("read_only"));
    assert!(matches!(
        bench.transmit(&input).await,
        TransmitOutcome::Rejected(reason) if reason == "InvalidRequestError"
    ));
    let requests = bench.fake.requests();
    let effects: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "PATCH")
        .map(|request| request["body"]["permissions"][0]["effect"].clone())
        .collect();
    assert_eq!(effects, [json!("deny"), json!("allow")]);
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn a_locked_turn_resumes_and_finishes_without_an_error_block() {
    let mut script = ready();
    script_with_locked_runs(&mut script, 1);
    let mut bench = Bench::new(script);
    bench
        .adapter
        .set_db_lock_delays(vec![Duration::from_millis(20), Duration::from_millis(40)]);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert!(
        text_of(&events, "Error").is_empty(),
        "a transient lock must not become an error block"
    );
    let notices = text_of(&events, "Warning");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("база занята"), "{}", notices[0]);
    let prompts: Vec<_> = bench
        .fake
        .requests()
        .into_iter()
        .filter(|request| path_of(request).ends_with("/prompt"))
        .collect();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_eq!(prompts[1]["body"]["text"], json!("continue"));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn a_locked_turn_is_retried_no_more_than_twice() {
    let mut script = ready();
    script_with_locked_runs(&mut script, 3);
    let mut bench = Bench::new(script);
    bench
        .adapter
        .set_db_lock_delays(vec![Duration::from_millis(20), Duration::from_millis(40)]);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    let errors = text_of(&events, "Error");
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("Failed to execute statement"));
    let prompts = bench
        .fake
        .requests()
        .into_iter()
        .filter(|request| path_of(request).ends_with("/prompt"))
        .count();
    assert_eq!(prompts, 3, "first prompt and two resumes");
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn a_locked_session_create_is_retried_until_the_server_answers() {
    let mut script = ready();
    let locked = Reply {
        status: 500,
        body: Some(json!({"_tag": "UnknownError", "message": "Failed to execute statement"})),
        ..Default::default()
    };
    let position = script
        .steps
        .iter()
        .position(|step| {
            matches!(step, Step::Expect { method, path, .. }
                if method == "POST" && path == "/api/session")
        })
        .unwrap();
    for _ in 0..2 {
        script.steps.insert(
            position,
            Step::Expect {
                method: "POST".into(),
                path: "/api/session".into(),
                reply: locked.clone(),
                bind: Default::default(),
            },
        );
    }
    let mut bench = Bench::new(script);
    bench
        .adapter
        .set_db_lock_delays(vec![Duration::from_millis(20), Duration::from_millis(40)]);
    bench.start_ok().await;
    let creates = bench
        .fake
        .requests()
        .into_iter()
        .filter(|request| request["method"] == "POST" && path_of(request) == "/api/session")
        .count();
    assert_eq!(creates, 3, "two refusals and the real create");
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn a_locked_session_create_stops_after_three_attempts() {
    let mut script = ready();
    let locked = Reply {
        status: 500,
        body: Some(json!({"_tag": "UnknownError", "message": "Failed to execute statement"})),
        ..Default::default()
    };
    let position = script
        .steps
        .iter()
        .position(|step| {
            matches!(step, Step::Expect { method, path, .. }
                if method == "POST" && path == "/api/session")
        })
        .unwrap();
    for _ in 0..3 {
        script.steps.insert(
            position,
            Step::Expect {
                method: "POST".into(),
                path: "/api/session".into(),
                reply: locked.clone(),
                bind: Default::default(),
            },
        );
    }
    let mut bench = Bench::new(script);
    bench
        .adapter
        .set_db_lock_delays(vec![Duration::from_millis(20), Duration::from_millis(40)]);
    assert_eq!(bench.fail_reason().await, "session_create_failed");
    let creates = bench
        .fake
        .requests()
        .into_iter()
        .filter(|request| request["method"] == "POST" && path_of(request) == "/api/session")
        .count();
    assert_eq!(creates, 3);
}

/// Переписывает первый ход: `runs - 1` падений на замке базы, между ними `continue`,
/// и успешный конец. Хвост последнего отказа ошибкой становится только после всех повторов.
fn script_with_locked_runs(script: &mut Script, runs: usize) {
    let (started, succeeded) = script
        .steps
        .iter()
        .find_map(|step| {
            let Step::Emit { events } = step else {
                return None;
            };
            let started = events
                .iter()
                .find(|event| event["type"] == "session.execution.started")?
                .clone();
            let succeeded = events
                .iter()
                .find(|event| event["type"] == "session.execution.succeeded")?
                .clone();
            Some((started, succeeded))
        })
        .expect("recorded started and succeeded");
    let mut failed = succeeded.clone();
    failed["type"] = json!("session.execution.failed");
    failed["data"]["error"] = json!({"type": "unknown", "message": "Failed to execute statement"});
    let position = script
        .steps
        .iter()
        .position(|step| {
            let Step::Emit { events } = step else {
                return false;
            };
            events
                .iter()
                .any(|event| event["type"] == "session.execution.succeeded")
        })
        .unwrap();
    let Step::Emit { events } = &mut script.steps[position] else {
        unreachable!();
    };
    for event in events {
        if event["type"] == "session.execution.succeeded" {
            *event = failed.clone();
        }
    }
    let session = started["data"]["sessionID"].as_str().unwrap().to_owned();
    let mut insert_at = position + 1;
    for _ in 1..runs {
        script
            .steps
            .insert(insert_at, locked_prompt_expect(&session, "msg_retry"));
        script.steps.insert(
            insert_at + 1,
            Step::Emit {
                events: vec![started.clone(), failed.clone()],
            },
        );
        insert_at += 2;
    }
    script
        .steps
        .insert(insert_at, locked_prompt_expect(&session, "msg_done"));
    script.steps.insert(
        insert_at + 1,
        Step::Emit {
            events: vec![started, succeeded],
        },
    );
}

fn locked_prompt_expect(session: &str, id: &str) -> Step {
    Step::Expect {
        method: "POST".into(),
        path: format!("/api/session/{session}/prompt"),
        reply: Reply {
            status: 200,
            body: Some(json!({"data": {"id": id}})),
            ..Default::default()
        },
        bind: [(id.to_owned(), "/id".to_owned())].into_iter().collect(),
    }
}

#[tokio::test]
async fn failed_run_shows_an_error_block_and_still_finishes() {
    let mut script = ready();
    for step in &mut script.steps {
        let Step::Emit { events } = step else {
            continue;
        };
        for event in events {
            if event["type"] == "session.execution.succeeded" {
                event["type"] = "session.execution.failed".into();
                event["data"]["error"] =
                    json!({"type": "provider.internal", "message": "упал провайдер"});
            }
        }
    }
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    let errors = text_of(&events, "Error");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("упал провайдер"));
    assert!(errors[0].contains("provider.internal"));
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockStarted { kind: BlockKind::Error, input_command_id, .. }
            if *input_command_id == Some(input.command_id)
    )));
    let prompts = bench
        .fake
        .requests()
        .into_iter()
        .filter(|request| path_of(request).ends_with("/prompt"))
        .count();
    assert_eq!(prompts, 1, "only the lock failure is retried");
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn unknown_event_does_not_stop_the_turn() {
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .rposition(|step| matches!(step, Step::Emit { .. }))
        .unwrap();
    script.steps.insert(
        position,
        Step::Emit {
            events: vec![
                json!({"type": "session.brand.new", "data": {"sessionID": "ses_x"}}),
                json!({"type": "brand.new", "data": {}}),
            ],
        },
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic { kind: "unknown_event", required: false, event_type, .. }
            if event_type == "brand.new"
    )));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn garbled_terminal_frame_stops_the_execution() {
    let mut script = ready();
    script.steps.push(Step::Frame {
        raw: "data: {\"type\":\"session.execution.succ\n\n".into(),
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    loop {
        match bench.event().await {
            AdapterEvent::Diagnostic { kind, required, .. } => {
                assert_eq!(kind, "unparsed_event");
                assert!(required);
                break;
            }
            AdapterEvent::Phase { .. } => {}
            other => panic!("{other:?}"),
        }
    }
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn lost_stream_stops_the_server_instead_of_waiting_forever() {
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(position + 1);
    script.steps.push(Step::Stall { ms: 100 });
    script.refuse_events_from = Some(2);
    script.steps.push(Step::DropSse);
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let mut lost = false;
    loop {
        match bench.event().await {
            AdapterEvent::Diagnostic {
                kind: "event_stream_lost",
                ..
            } => lost = true,
            // Закрытый stdin и SIGTERM идут подряд, сервер выходит по первому из них.
            AdapterEvent::ProcessExited { code, .. } => {
                assert!(matches!(code, Some(0 | 130)), "{code:?}");
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(lost);
    assert!(matches!(
        bench.transmit(&bench.input("y")).await,
        TransmitOutcome::Undetermined
    ));
    bench.close(CloseMode::Graceful).await;
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn lost_stream_escalates_when_the_server_ignores_the_signal() {
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(position + 1);
    script.steps.push(Step::Stall { ms: 100 });
    script.refuse_events_from = Some(2);
    script.steps.push(Step::DropSse);
    script.ignore_eof = true;
    script.ignore_term = true;
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    loop {
        if let AdapterEvent::ProcessExited { signal, .. } = bench.event().await {
            assert_eq!(signal, Some(9));
            break;
        }
    }
    bench.close(CloseMode::Graceful).await;
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn server_death_during_a_turn_keeps_the_last_events() {
    let mut script = ready();
    script.steps.push(Step::Stall { ms: 100 });
    script.steps.push(Step::Die {
        code: 3,
        kill: true,
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    let mut finished = false;
    loop {
        match bench.event().await {
            AdapterEvent::RunFinished { .. } => finished = true,
            AdapterEvent::ProcessExited { signal, .. } => {
                assert_eq!(signal, Some(9));
                break;
            }
            _ => {}
        }
    }
    assert!(
        finished,
        "the stream tail reaches the journal before the exit"
    );
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn next_event_is_cancel_safe() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    let mut events = Vec::new();
    while !events
        .iter()
        .any(|event| matches!(event, AdapterEvent::RunFinished { .. }))
    {
        if let Ok(event) =
            tokio::time::timeout(Duration::from_millis(1), bench.adapter.next_event()).await
        {
            events.push(event);
        }
    }
    assert_eq!(text_of(&events, "Text"), ["Hello world"]);
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn debug_of_events_hides_the_conversation() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let input = bench.input("say hello");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    let shown = format!("{events:?}");
    assert!(!shown.contains("Hello"), "{shown}");
    assert!(!shown.contains("think more"), "{shown}");
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn bypass_shows_tools_without_a_mark() {
    let mut bench = Bench::new(tools());
    bench.start_ok().await;
    let input = bench.input("run tools");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    let calls = calls(&events);
    assert_eq!(calls.len(), 7);
    assert!(calls.iter().all(|call| call.approved_by.is_none()));
    assert_eq!(text_of(&events, "ToolResult").len(), 7);
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn owner_mode_marks_tools_that_ran_without_a_prompt() {
    let mut bench = Bench::new(tools());
    bench.spec.permission_mode = "owner".into();
    bench.start_ok().await;
    let input = bench.input("run tools");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    let calls = calls(&events);
    assert_eq!(calls.len(), 7);
    for call in &calls {
        assert_eq!(
            call.approved_by.as_deref(),
            Some("cli_policy"),
            "{}",
            call.tool
        );
    }
    bench.close(CloseMode::Graceful).await;
}

// Запрос разрешения снимает удержание без пометки: решал не CLI.
#[tokio::test]
async fn tool_with_a_permission_prompt_gets_no_mark() {
    let mut bench = Bench::new(permission_turn());
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (request, _, mut events) = bench.until_request().await;
    assert!(matches!(
        bench.deliver(&request, &allow()).await,
        DecisionOutcome::Applied(_)
    ));
    events.extend(bench.until_finished(input.command_id).await);
    let calls = calls(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].approved_by, None);
    bench.close(CloseMode::Graceful).await;
}

// Режим `ask`: запрос доходит карточкой, ход ждёт решения владельца.
#[tokio::test]
async fn permission_request_reaches_the_owner_as_a_card() {
    let mut bench = Bench::new(permission_turn());
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (request, payload, _) = bench.until_request().await;
    assert!(request.starts_with("per_"), "{request}");
    assert_eq!(payload["tool_name"], "shell");
    assert_eq!(payload["command"], "echo one > one.txt");
    assert_eq!(payload["agent"], "orchestrator");
    assert_eq!(payload["deny_also_interrupts"], false);
    assert!(
        payload["tool_use_id"]
            .as_str()
            .unwrap()
            .starts_with("call_")
    );
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Phase {
            phase: Phase::AwaitingPermission,
            ..
        }
    ));
    assert!(bench.replies().is_empty(), "no answer before the owner");
    assert!(matches!(
        bench.deliver(&request, &allow()).await,
        DecisionOutcome::Applied(_)
    ));
    // `always` записал бы правило на весь проект, только `once`.
    assert_eq!(bench.replies()[0]["body"], json!({"decision": "once"}));
    let events = bench.until_finished(input.command_id).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AdapterEvent::Phase {
                phase: Phase::RunningTool,
                ..
            }
        )),
        "the card is gone: {events:?}"
    );
    bench.close(CloseMode::Graceful).await;
}

// Отказ без сообщения кончает ход, поэтому сообщение есть всегда.
#[tokio::test]
async fn deny_always_carries_a_message() {
    for (given, sent) in [
        (None, None),
        (Some("  "), None),
        (Some("Не трогай этот файл"), Some("Не трогай этот файл")),
    ] {
        let mut bench = Bench::new(permission_turn());
        bench.spec.permission_mode = "ask".into();
        bench.start_ok().await;
        let input = bench.input("x");
        bench.accepted(&input).await;
        let (request, ..) = bench.until_request().await;
        assert!(matches!(
            bench.deliver(&request, &deny(given)).await,
            DecisionOutcome::Applied(_)
        ));
        let reply = bench.replies().remove(0);
        assert_eq!(reply["body"]["decision"], "reject");
        let message = reply["body"]["message"].as_str().unwrap();
        assert!(!message.trim().is_empty());
        if let Some(sent) = sent {
            assert_eq!(message, sent);
        }
        bench.close(CloseMode::Graceful).await;
    }
}

// Повтор решения доходит до сервера один раз: запрос живёт только в его памяти.
#[tokio::test]
async fn repeated_decision_reaches_the_server_once() {
    let mut bench = Bench::new(permission_turn());
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (request, ..) = bench.until_request().await;
    assert!(matches!(
        bench.deliver(&request, &allow()).await,
        DecisionOutcome::Applied(_)
    ));
    let DecisionOutcome::NotApplied(reason) = bench.deliver(&request, &allow()).await else {
        panic!("second decision");
    };
    assert_eq!(reason, "request_not_pending");
    assert_eq!(bench.replies().len(), 1);
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn decision_on_a_settled_request_is_not_applied() {
    let mut script = permission_turn();
    let position = script
        .steps
        .iter()
        .position(|step| step.expected_path().is_some_and(|p| p.ends_with("/reply")))
        .unwrap();
    let Step::Expect { reply, .. } = &mut script.steps[position] else {
        unreachable!();
    };
    *reply = Reply {
        status: 404,
        body: Some(json!({"_tag": "PermissionNotFoundError", "message": "gone"})),
        ..Default::default()
    };
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (request, ..) = bench.until_request().await;
    let DecisionOutcome::NotApplied(reason) = bench.deliver(&request, &allow()).await else {
        panic!("404 is a proven miss");
    };
    assert_eq!(reason, "request_not_pending");
    bench.close(CloseMode::Graceful).await;
}

// Доставку не доказать: исход неизвестен, повторной отправки нет.
#[tokio::test]
async fn lost_answer_is_undetermined_and_never_resent() {
    for status in [500, 503] {
        let mut script = permission_turn();
        let position = script
            .steps
            .iter()
            .position(|step| step.expected_path().is_some_and(|p| p.ends_with("/reply")))
            .unwrap();
        let Step::Expect { reply, .. } = &mut script.steps[position] else {
            unreachable!();
        };
        reply.status = status;
        reply.body = Some(json!({"_tag": "InternalError"}));
        let mut bench = Bench::new(script);
        bench.spec.permission_mode = "ask".into();
        bench.start_ok().await;
        let input = bench.input("x");
        bench.accepted(&input).await;
        let (request, ..) = bench.until_request().await;
        assert!(matches!(
            bench.deliver(&request, &allow()).await,
            DecisionOutcome::Undetermined
        ));
        assert!(matches!(
            bench.deliver(&request, &allow()).await,
            DecisionOutcome::NotApplied(_)
        ));
        bench.adapter.refuse_request(&request, "turn ended").await;
        assert_eq!(bench.replies().len(), 1);
        bench.close(CloseMode::Graceful).await;
    }
}

// Запрос живёт в памяти сервера: новый адаптер на той же сессии его не знает.
#[tokio::test]
async fn request_of_a_previous_process_is_never_answered() {
    let (script, id) = resume_script(true);
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(id);
    bench.start_ok().await;
    let DecisionOutcome::NotApplied(reason) = bench
        .deliver("per_18f9febb6001B7zsfPCgwOzqXy", &allow())
        .await
    else {
        panic!("unknown request");
    };
    assert_eq!(reason, "request_not_pending");
    assert!(bench.replies().is_empty());
    bench.close(CloseMode::Graceful).await;
}

/// Записанная пачка из трёх запросов одного шага; отказ снимает всю пачку.
fn sibling_requests() -> Script {
    let capture = Script::from_capture("permissions", None);
    // Пачка записана во второй сессии прогона, сценарий ведёт её в первой.
    let session = session_of(&capture);
    let own = |mut event: Value| {
        event["data"]["sessionID"] = session.clone().into();
        event
    };
    let batch: Vec<Value> = tool_events(&capture, &["permission.asked"])
        .into_iter()
        .filter(|event| event["data"]["action"] != "shell")
        .map(own)
        .collect();
    assert_eq!(batch.len(), 3);
    let first = batch[0]["data"]["id"].as_str().unwrap().to_owned();
    let replied: Vec<Value> = tool_events(&capture, &["permission.replied"])
        .into_iter()
        .filter(|event| {
            batch
                .iter()
                .any(|asked| asked["data"]["id"] == event["data"]["requestID"])
        })
        .map(own)
        .collect();
    assert_eq!(replied.len(), 3);
    let mut script = capture
        .clone()
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    replace_events_after_prompt(
        &mut script,
        vec![
            Step::Emit { events: batch },
            Step::Expect {
                method: "POST".into(),
                path: format!("/api/session/{session}/permission/{first}/reply"),
                reply: reply_204(),
                bind: Default::default(),
            },
            Step::Emit { events: replied },
            Step::Emit {
                events: vec![finish(&session)],
            },
        ],
    );
    script
}

#[tokio::test]
async fn deny_closes_the_siblings_from_the_replied_events() {
    let mut bench = Bench::new(sibling_requests());
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (first, payload, _) = bench.until_request().await;
    assert_eq!(payload["tool_name"], "read");
    assert_eq!(payload["input"]["file_path"], "one.txt");
    let (second, payload, _) = bench.until_request().await;
    assert_eq!(payload["tool_name"], "edit");
    assert_eq!(payload["changes"][0]["path"], "w.txt");
    assert_eq!(payload["changes"][0]["kind"], "add");
    assert!(
        payload["changes"][0]["diff"]
            .as_str()
            .unwrap()
            .contains("+w")
    );
    let (third, payload, _) = bench.until_request().await;
    assert_eq!(payload["tool_name"], "external_directory");
    assert_eq!(payload["grant_root"], "/etc/*");
    assert!(matches!(
        bench.deliver(&first, &deny(Some("нельзя"))).await,
        DecisionOutcome::Applied(_)
    ));
    let events = bench.until_finished(input.command_id).await;
    let mut closed: Vec<(String, String)> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::RequestClosed { request_id, reason } => {
                Some((request_id.clone(), reason.clone()))
            }
            _ => None,
        })
        .collect();
    closed.sort();
    let mut wanted = vec![
        (second, "cli_cancelled".to_owned()),
        (third, "cli_cancelled".to_owned()),
    ];
    wanted.sort();
    assert_eq!(closed, wanted);
    bench.close(CloseMode::Graceful).await;
}

// Карточка не показана или уже закрыта: сервер получает отказ с текстом, ход идёт дальше.
#[tokio::test]
async fn refusal_without_the_owner_carries_a_message() {
    let mut bench = Bench::new(permission_turn());
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (request, ..) = bench.until_request().await;
    bench
        .adapter
        .refuse_request(&request, "Request is too large to show to the owner")
        .await;
    bench.until_finished(input.command_id).await;
    let replies = bench.replies();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["body"]["decision"], "reject");
    assert_eq!(
        replies[0]["body"]["message"],
        "Request is too large to show to the owner"
    );
    bench.close(CloseMode::Graceful).await;
}

// Вне хода карточку показать некому: отказ сразу, иначе вызов ждал бы вечно.
#[tokio::test]
async fn request_outside_a_turn_is_refused_at_once() {
    let capture = Script::from_capture("permissions", None);
    let asked = tool_events(&capture, &["permission.asked"]).remove(0);
    let session = asked["data"]["sessionID"].as_str().unwrap().to_owned();
    let id = asked["data"]["id"].as_str().unwrap().to_owned();
    let mut script = capture
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    let create = script
        .steps
        .iter()
        .position(|step| step.expected_path().is_some())
        .unwrap();
    script.steps.truncate(create + 1);
    script.steps.push(Step::Stall { ms: 400 });
    script.steps.push(Step::Emit {
        events: vec![asked],
    });
    script.set_route(
        "POST",
        &format!("/api/session/{session}/permission/{id}/reply"),
        vec![reply_204()],
    );
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let diagnostic = bench.event().await;
    assert!(
        matches!(
            diagnostic,
            AdapterEvent::Diagnostic {
                kind: "request_outside_turn",
                required: false,
                ..
            }
        ),
        "{diagnostic:?}"
    );
    tokio::time::timeout(WAIT, async {
        while bench.replies().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("refusal");
    let reply = bench.replies().remove(0);
    assert_eq!(reply["body"]["decision"], "reject");
    assert!(
        reply["body"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty())
    );
    bench.close(CloseMode::Graceful).await;
}

// Прерывание снимает запрос без `permission.replied`, виден только конец инструмента.
#[tokio::test]
async fn aborted_tool_closes_its_request() {
    let capture = Script::from_capture("permissions", None);
    let session = session_of(&capture);
    let mut prefix = tool_prefix(&capture);
    let asked = tool_events(&capture, &["permission.asked"]).remove(0);
    let called = prefix.last().unwrap().clone();
    prefix.push(asked.clone());
    let failed = json!({"type": "session.tool.failed", "data": {
        "sessionID": session,
        "assistantMessageID": called["data"]["assistantMessageID"],
        "id": called["data"]["id"],
        "error": {"type": "aborted", "message": "Tool execution interrupted"},
        "executed": false,
    }});
    let mut script = capture
        .clone()
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    replace_events_after_prompt(
        &mut script,
        vec![
            Step::Emit { events: prefix },
            Step::Stall { ms: 200 },
            Step::Emit {
                events: vec![
                    failed,
                    json!({"type": "session.execution.interrupted",
                        "data": {"sessionID": session, "reason": "user"}}),
                ],
            },
        ],
    );
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (request, ..) = bench.until_request().await;
    let events = bench.until_finished(input.command_id).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::RequestClosed { request_id, reason }
            if *request_id == request && reason == "cli_cancelled"
    )));
    assert!(bench.replies().is_empty());
    bench.close(CloseMode::Graceful).await;
}

// Команда пошла без запроса: вызов виден сразу, с пометкой, а не после конца команды.
#[tokio::test]
async fn progress_releases_a_long_tool_before_it_ends() {
    let capture = Script::from_capture("tools", None);
    let session = session_of(&capture);
    let prefix = tool_prefix(&capture);
    let progress = tool_events(&capture, &["session.tool.progress"]);
    let mut script = tools();
    replace_events_after_prompt(
        &mut script,
        vec![
            Step::Emit { events: prefix },
            Step::Emit {
                events: progress[..1].to_vec(),
            },
            Step::Stall { ms: 3000 },
            Step::Emit {
                events: vec![finish(&session)],
            },
        ],
    );
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "owner".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let started = std::time::Instant::now();
    loop {
        if let AdapterEvent::BlockStarted {
            kind: BlockKind::ToolCall,
            approved_by,
            ..
        } = bench.event().await
        {
            assert_eq!(approved_by.as_deref(), Some("cli_policy"));
            break;
        }
    }
    assert!(started.elapsed() < Duration::from_secs(2));
    bench.close(CloseMode::Force).await;
}

// Ход кончился без решения: вызов не теряется и остаётся без пометки.
#[tokio::test]
async fn end_of_a_turn_releases_held_tools_without_a_mark() {
    let capture = Script::from_capture("tools", None);
    let session = session_of(&capture);
    let prefix = tool_prefix(&capture);
    let mut script = tools();
    replace_events_after_prompt(
        &mut script,
        vec![
            Step::Emit { events: prefix },
            Step::Emit {
                events: vec![finish(&session)],
            },
        ],
    );
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "owner".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    let calls = calls(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].approved_by, None);
    bench.close(CloseMode::Graceful).await;
}

// Без ответа ход висел бы вечно: форма снимается с сообщением, ход продолжается.
/// Первый ход записи вопросов: два вопроса, ответ, конец хода.
fn question_turn() -> Script {
    Script::from_capture("question", None)
        .first_run()
        .without_replies(409)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents()
}

fn answers(items: &[(&str, AnswerValue)]) -> DecisionPayload {
    DecisionPayload::Question {
        answers: items
            .iter()
            .map(|(id, value)| ((*id).to_owned(), value.clone()))
            .collect(),
    }
}

fn single(text: &str) -> AnswerValue {
    AnswerValue::Single(text.into())
}

fn multiple(values: &[&str]) -> AnswerValue {
    AnswerValue::Multiple(values.iter().map(|value| (*value).to_owned()).collect())
}

fn edit_form(script: &mut Script, change: impl Fn(&mut Value)) {
    for step in &mut script.steps {
        if let Step::Emit { events } = step {
            for event in events {
                if event["type"] == "form.created" {
                    change(&mut event["data"]["form"]);
                }
            }
        }
    }
}

// Вопрос доходит карточкой, а ответ идёт по ключам формы.
#[tokio::test]
async fn question_reaches_the_owner_and_the_answer_goes_by_keys() {
    let mut bench = Bench::new(question_turn());
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (request, payload, _) = bench.until_request().await;
    assert!(request.starts_with("frm_"), "{request}");
    assert_eq!(payload["agent"], "orchestrator");
    assert!(
        payload["tool_use_id"]
            .as_str()
            .unwrap()
            .starts_with("call_")
    );
    let questions = payload["questions"].as_array().unwrap();
    assert_eq!(questions.len(), 2);
    assert_eq!(questions[0]["id"], "q0");
    assert_eq!(questions[0]["text"], "Pick a colour?");
    assert_eq!(questions[0]["header"], "Colour");
    assert_eq!(
        questions[0]["options"],
        json!([
            {"label": "Blue", "description": "cold"},
            {"label": "Red", "description": "warm"},
        ])
    );
    assert_eq!(questions[0]["multi_select"], false);
    assert_eq!(questions[0]["allow_custom"], true);
    assert_eq!(questions[1]["multi_select"], true);
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Phase {
            phase: Phase::AwaitingAnswer,
            ..
        }
    ));
    let decision = answers(&[("q0", single("Blue")), ("q1", multiple(&["A", "B"]))]);
    assert!(matches!(
        bench.deliver(&request, &decision).await,
        DecisionOutcome::Applied(_)
    ));
    assert_eq!(
        bench.replies()[0]["body"],
        json!({"answer": {"q0": "Blue", "q1": ["A", "B"]}})
    );
    bench.until_finished(input.command_id).await;
    let DecisionOutcome::NotApplied(reason) = bench.deliver(&request, &decision).await else {
        panic!("second answer");
    };
    assert_eq!(reason, "request_not_pending");
    assert_eq!(bench.replies().len(), 1);
    bench.close(CloseMode::Graceful).await;
}

// Сервер принял бы неполный ответ и подставил модели "Unanswered".
#[tokio::test]
async fn partial_answer_stops_before_the_server() {
    let mut bench = Bench::new(question_turn());
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (request, ..) = bench.until_request().await;
    let DecisionOutcome::NotApplied(reason) = bench
        .deliver(&request, &answers(&[("q0", single("Blue"))]))
        .await
    else {
        panic!("partial answer");
    };
    assert_eq!(reason, "decision_invalid");
    assert!(bench.replies().is_empty());
    let DecisionOutcome::NotApplied(reason) = bench.deliver(&request, &allow()).await else {
        panic!("permission on a question");
    };
    assert_eq!(reason, "decision_invalid");
    assert!(matches!(
        bench
            .deliver(
                &request,
                &answers(&[("q0", single("Свой ответ")), ("q1", multiple(&["Другое"]))]),
            )
            .await,
        DecisionOutcome::Applied(_)
    ));
    assert_eq!(
        bench.replies()[0]["body"],
        json!({"answer": {"q0": "Свой ответ", "q1": ["Другое"]}})
    );
    bench.close(CloseMode::Graceful).await;
}

// Подпись видит владелец, значение ждёт сервер. Кадр выдуман: инструмент `question` кладёт
// в оба поля одно и то же, различать их разрешает схема `Form.Option`.
#[tokio::test]
async fn answer_is_sent_as_the_value_of_the_option() {
    let mut script = question_turn();
    edit_form(&mut script, |form| {
        form["fields"][0]["options"][0]["label"] = "Синий".into();
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (request, payload, _) = bench.until_request().await;
    assert_eq!(payload["questions"][0]["options"][0]["label"], "Синий");
    let decision = answers(&[("q0", single("Синий")), ("q1", multiple(&["A"]))]);
    assert!(matches!(
        bench.deliver(&request, &decision).await,
        DecisionOutcome::Applied(_)
    ));
    assert_eq!(bench.replies()[0]["body"]["answer"]["q0"], "Blue");
    bench.close(CloseMode::Graceful).await;
}

async fn cancelled_form(script: Script) -> (Bench, Vec<AdapterEvent>) {
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    (bench, events)
}

fn form_cancel_script(change: impl Fn(&mut Value)) -> (Script, String) {
    let capture = Script::from_capture("question", None);
    let mut form = tool_events(&capture, &["form.created"]).remove(0);
    change(&mut form["data"]["form"]);
    let session = session_of(&capture);
    let owner = form["data"]["form"]["sessionID"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = form["data"]["form"]["id"].as_str().unwrap().to_owned();
    let mut script = capture
        .clone()
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    replace_events_after_prompt(
        &mut script,
        vec![
            Step::Emit { events: vec![form] },
            Step::Stall { ms: 200 },
            Step::Emit {
                events: vec![finish(&session)],
            },
        ],
    );
    let path = format!("/api/session/{owner}/form/{id}");
    script.set_route("DELETE", &path, vec![reply_204()]);
    (script, path)
}

fn assert_cancelled(bench: &Bench, events: &[AdapterEvent], tail: &str) {
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, AdapterEvent::RequestOpened { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "opencode_form_refused",
            required: false,
            ..
        }
    )));
    let requests = bench.fake.requests();
    let cancel = find(&requests, "DELETE", tail).expect("form cancel");
    assert!(cancel["query"].as_str().unwrap().contains("message="));
}

// Больше четырёх вопросов карточка не показывает, без ответа ход завис бы. Настоящую форму
// проверяет L2 `real_form_with_five_fields_is_refused_and_the_model_goes_on`.
#[tokio::test]
async fn form_with_too_many_questions_is_cancelled_with_a_message() {
    let (script, path) = form_cancel_script(|form| {
        let field = form["fields"][0].clone();
        let list = form["fields"].as_array_mut().unwrap();
        for index in 2..5 {
            let mut field = field.clone();
            field["key"] = format!("q{index}").into();
            list.push(field);
        }
    });
    let (mut bench, events) = cancelled_form(script).await;
    assert_cancelled(&bench, &events, path.rsplit('/').next().unwrap());
    bench.close(CloseMode::Graceful).await;
}

// Формы MCP не вопросы: полей карточка не соберёт. Настоящую форму от настоящего сервера MCP
// проверяет L2 `real_mcp_elicitation_is_refused_and_the_call_returns`.
#[tokio::test]
async fn elicitation_form_is_cancelled_with_a_message() {
    let (script, path) = form_cancel_script(|form| {
        form["sessionID"] = "global".into();
        form["metadata"] =
            json!({"kind": "mcp-elicitation", "server": "ask", "message": "Which one?"});
    });
    let (mut bench, events) = cancelled_form(script).await;
    assert!(path.contains("/session/global/form/"));
    assert_cancelled(&bench, &events, path.rsplit('/').next().unwrap());
    bench.close(CloseMode::Graceful).await;
}

// Отказ без текста кончил бы ход: `DELETE` всегда с сообщением.
#[tokio::test]
async fn refused_question_carries_a_message() {
    let (mut script, path) = form_cancel_script(|_| {});
    let tail = path.rsplit('/').next().unwrap().to_owned();
    script
        .steps
        .retain(|step| !matches!(step, Step::Stall { .. }));
    let finish = script.steps.pop().unwrap();
    script.steps.push(Step::Stall { ms: 300 });
    script.steps.push(finish);
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (request, ..) = bench.until_request().await;
    bench
        .adapter
        .refuse_request(&request, "The turn has ended")
        .await;
    bench.until_finished(input.command_id).await;
    let requests = bench.fake.requests();
    let cancel = find(&requests, "DELETE", &tail).expect("form cancel");
    assert!(
        cancel["query"]
            .as_str()
            .unwrap()
            .contains("message=The+turn+has+ended")
    );
    assert!(bench.replies().is_empty());
    bench.close(CloseMode::Graceful).await;
}

// Сервер снял форму сам: карточка закрывается по `form.cancelled`.
#[tokio::test]
async fn form_cancelled_by_the_server_closes_the_card() {
    let capture = Script::from_capture("question", None);
    let form = tool_events(&capture, &["form.created"]).remove(0);
    let session = session_of(&capture);
    let id = form["data"]["form"]["id"].as_str().unwrap().to_owned();
    let mut script = capture
        .clone()
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    replace_events_after_prompt(
        &mut script,
        vec![
            Step::Emit { events: vec![form] },
            Step::Stall { ms: 200 },
            Step::Emit {
                events: vec![
                    json!({"type": "form.cancelled", "data": {"id": id, "sessionID": session}}),
                    finish(&session),
                ],
            },
        ],
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (request, ..) = bench.until_request().await;
    let events = bench.until_finished(input.command_id).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::RequestClosed { request_id, reason }
            if *request_id == request && reason == "cli_cancelled"
    )));
    bench.close(CloseMode::Graceful).await;
}

// Сервер отверг ответ: без отмены инструмент ждал бы вечно.
#[tokio::test]
async fn invalid_answer_cancels_the_form_with_a_message() {
    let mut script = question_turn();
    let position = script
        .steps
        .iter()
        .position(|step| step.expected_path().is_some_and(|p| p.ends_with("/reply")))
        .unwrap();
    let Step::Expect { reply, path, .. } = &mut script.steps[position] else {
        unreachable!();
    };
    *reply = Reply {
        status: 400,
        body: Some(json!({"_tag": "FormInvalidAnswerError", "message": "bad"})),
        ..Default::default()
    };
    let cancel = path.trim_end_matches("/reply").to_owned();
    script.set_route("DELETE", &cancel, vec![reply_204()]);
    let tail = cancel.rsplit('/').next().unwrap().to_owned();
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (request, ..) = bench.until_request().await;
    let decision = answers(&[("q0", single("Blue")), ("q1", multiple(&["A"]))]);
    let DecisionOutcome::NotApplied(reason) = bench.deliver(&request, &decision).await else {
        panic!("400 is a proven miss");
    };
    assert_eq!(reason, "decision_invalid");
    tokio::time::timeout(WAIT, async {
        while find(&bench.fake.requests(), "DELETE", &tail).is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("form cancel");
    bench.close(CloseMode::Graceful).await;
}

/// Первый ход записи прерывания: модель зависла, прерывание снимает её.
fn hanging_turn() -> Script {
    Script::from_capture("interrupt", None)
        .first_run()
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents()
}

fn set_interrupt_reply(script: &mut Script, change: impl FnOnce(&mut Reply)) {
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/interrupt"))
        })
        .unwrap();
    let Step::Expect { reply, .. } = &mut script.steps[position] else {
        unreachable!();
    };
    change(reply);
}

impl Bench {
    async fn interrupt(&mut self) -> InterruptOutcome {
        tokio::time::timeout(WAIT, self.adapter.interrupt())
            .await
            .expect("interrupt must finish")
    }
}

// Прерывание: подтверждение сервера, конец хода приходит обычным событием.
#[tokio::test]
async fn interrupt_is_acknowledged_and_the_turn_ends() {
    let mut bench = Bench::new(hanging_turn());
    bench.start_ok().await;
    let input = bench.input("hang");
    bench.accepted(&input).await;
    assert!(matches!(
        bench.interrupt().await,
        InterruptOutcome::Acknowledged
    ));
    bench.until_finished(input.command_id).await;
    let requests = bench.fake.requests();
    let sent = find(&requests, "POST", "/interrupt").expect("interrupt");
    assert_eq!(sent["body"], Value::Null);
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            ..
        }
    ));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn interrupt_with_nothing_running_is_not_applicable() {
    let mut script = hanging_turn();
    set_interrupt_reply(&mut script, |reply| {
        reply.body = Some(json!({"interrupted": false}));
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("hang");
    bench.accepted(&input).await;
    assert!(matches!(
        bench.interrupt().await,
        InterruptOutcome::NotApplicable
    ));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn interrupt_without_an_answer_is_undetermined() {
    let mut script = hanging_turn();
    set_interrupt_reply(&mut script, |reply| {
        reply.status = 500;
        reply.body = Some(json!({"_tag": "InternalError"}));
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("hang");
    bench.accepted(&input).await;
    assert!(matches!(
        bench.interrupt().await,
        InterruptOutcome::Undetermined
    ));
    bench.close(CloseMode::Graceful).await;
}

/// Запрос потомка и прерывание, которое ничего не сняло: запрос жив на сервере.
fn child_request_with_a_refused_interrupt() -> (Script, String, String) {
    let capture = Script::from_capture("permissions", None);
    let session = session_of(&capture);
    let child = "ses_child00000000000000000000000".to_owned();
    let mut asked = tool_events(&capture, &["permission.asked"]).remove(0);
    asked["data"]["sessionID"] = child.clone().into();
    let request = asked["data"]["id"].as_str().unwrap().to_owned();
    let mut script = capture
        .clone()
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    replace_events_after_prompt(
        &mut script,
        vec![
            Step::Emit {
                events: vec![
                    json!({"type": "session.created",
                        "data": {"sessionID": child, "parentID": session}}),
                    asked,
                ],
            },
            Step::Expect {
                method: "POST".into(),
                path: format!("/api/session/{session}/interrupt"),
                reply: Reply {
                    body: Some(json!({"interrupted": false})),
                    ..Default::default()
                },
                bind: Default::default(),
            },
        ],
    );
    script.set_route(
        "POST",
        &format!("/api/session/{child}/permission/{request}/reply"),
        vec![reply_204()],
    );
    (script, child, request)
}

// Сервер ничего не прервал, а супервизор оставляет ход и карточки как были: запрос должен
// остаться отвечаемым, иначе решение владельца упрётся в `request_not_pending`. Отказ
// оставшимся даёт конец хода (`on_run_ended`).
#[tokio::test]
async fn interrupt_that_stopped_nothing_keeps_the_open_requests_answerable() {
    let (script, child, request) = child_request_with_a_refused_interrupt();
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    bench.until_request().await;
    assert!(matches!(
        bench.interrupt().await,
        InterruptOutcome::NotApplicable
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(bench.replies().is_empty(), "{:?}", bench.replies());
    let allow = DecisionPayload::Permission {
        behavior: PermissionBehavior::Allow,
        message: None,
    };
    assert!(bench.deliver(&request, &allow).await.is_applied());
    tokio::time::timeout(WAIT, async {
        while bench.replies().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("owner decision");
    let reply = bench.replies().remove(0);
    assert!(path_of(&reply).ends_with(&format!("{child}/permission/{request}/reply")));
    assert_eq!(reply["body"]["decision"], "once");
    bench.close(CloseMode::Graceful).await;
}

// Подтверждённое прерывание сняло запросы само: ответы на них были бы лишними 404.
#[tokio::test]
async fn acknowledged_interrupt_sends_no_refusals() {
    let capture = Script::from_capture("permissions", None);
    let session = session_of(&capture);
    let mut prefix = tool_prefix(&capture);
    let called = prefix.last().unwrap().clone();
    prefix.push(tool_events(&capture, &["permission.asked"]).remove(0));
    let failed = json!({"type": "session.tool.failed", "data": {
        "sessionID": session,
        "assistantMessageID": called["data"]["assistantMessageID"],
        "id": called["data"]["id"],
        "error": {"type": "aborted", "message": "Tool execution interrupted"},
        "executed": false,
    }});
    let mut script = capture
        .clone()
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    replace_events_after_prompt(
        &mut script,
        vec![
            Step::Emit { events: prefix },
            Step::Expect {
                method: "POST".into(),
                path: format!("/api/session/{session}/interrupt"),
                reply: Reply {
                    body: Some(json!({"interrupted": true})),
                    ..Default::default()
                },
                bind: Default::default(),
            },
            Step::Emit {
                events: vec![
                    failed,
                    json!({"type": "session.execution.interrupted",
                        "data": {"sessionID": session, "reason": "user"}}),
                ],
            },
        ],
    );
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    bench.until_request().await;
    assert!(matches!(
        bench.interrupt().await,
        InterruptOutcome::Acknowledged
    ));
    let events = bench.until_finished(input.command_id).await;
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, AdapterEvent::RequestClosed { .. })),
        "the supervisor has closed the cards: {events:?}"
    );
    assert!(bench.replies().is_empty());
    bench.close(CloseMode::Graceful).await;
}

fn shell_list(session: &str) -> Value {
    json!({"data": [
        {"id": "sh_1", "status": "running", "command": "sleep 171717 # фон",
         "metadata": {"sessionID": session}},
        {"id": "sh_2", "status": "killed", "command": "sleep 181818",
         "metadata": {"sessionID": session}},
        {"id": "sh_3", "status": "running", "command": "чужая команда",
         "metadata": {"sessionID": "ses_stranger"}},
    ]})
}

// Решение 9: фоновые команды прерывание переживают, владелец их видит, снятия нет.
#[tokio::test]
async fn background_shells_survive_the_interrupt_and_are_shown() {
    let mut script = hanging_turn();
    let session = session_of(&script);
    script.set_route(
        "GET",
        "/api/shell",
        vec![Reply {
            body: Some(shell_list(&session)),
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("hang");
    bench.accepted(&input).await;
    assert!(matches!(
        bench.interrupt().await,
        InterruptOutcome::Acknowledged
    ));
    bench.until_finished(input.command_id).await;
    let mut warning = None;
    let mut blocks = Vec::new();
    while warning.is_none() {
        let event = bench.event().await;
        if let AdapterEvent::BlockFinal { block_id, text, .. } = &event
            && blocks.contains(block_id)
        {
            warning = Some(text.clone());
        }
        if let AdapterEvent::BlockStarted {
            block_id,
            kind: BlockKind::Warning,
            input_command_id,
            ..
        } = &event
        {
            assert_eq!(*input_command_id, Some(input.command_id));
            blocks.push(*block_id);
        }
    }
    let warning = warning.unwrap();
    assert!(warning.contains("sleep 171717 # фон"), "{warning}");
    assert!(!warning.contains("sleep 181818"), "{warning}");
    assert!(!warning.contains("чужая команда"), "{warning}");
    let requests = bench.fake.requests();
    assert!(requests.iter().all(|request| request["method"] != "DELETE"));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn interrupt_without_background_shells_shows_nothing() {
    let mut script = hanging_turn();
    script.set_route(
        "GET",
        "/api/shell",
        vec![Reply {
            body: Some(json!({"data": []})),
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("hang");
    bench.accepted(&input).await;
    bench.interrupt().await;
    bench.until_finished(input.command_id).await;
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            ..
        }
    ));
    tokio::time::timeout(WAIT, async {
        while find(&bench.fake.requests(), "GET", "/api/shell").is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("shell list");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), bench.adapter.next_event())
            .await
            .is_err()
    );
    bench.close(CloseMode::Graceful).await;
}

fn first_terminal(script: &Script) -> usize {
    script
        .steps
        .iter()
        .position(|step| {
            matches!(step, Step::Emit { events } if events.iter().any(|event| {
                event["type"] == "session.execution.succeeded"
            }))
        })
        .unwrap()
}

/// Запись субагента: фоновый потомок закончил, родитель запустился без ввода.
fn background_child() -> Script {
    Script::from_capture("subagent", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents()
}

// Решение 8: запуск без ввода принят и приписан последней команде, расход не потерян.
#[tokio::test]
async fn autonomous_run_is_attributed_to_the_last_command() {
    let mut bench = Bench::new(background_child());
    bench.start_ok().await;
    let input = bench.input("delegate");
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    let mut events = Vec::new();
    loop {
        let event = bench.event().await;
        let done = matches!(event, AdapterEvent::RunFinished { .. });
        events.push(event);
        if done {
            break;
        }
    }
    assert!(
        matches!(
            events.iter().find(|event| matches!(event, AdapterEvent::RunAdopted { .. })),
            Some(AdapterEvent::RunAdopted { input_command_id }) if *input_command_id == input.command_id
        ),
        "{events:?}"
    );
    let adopted = events
        .iter()
        .position(|event| matches!(event, AdapterEvent::RunAdopted { .. }))
        .unwrap();
    // До запуска родителя работает только фоновый потомок: его блоки в дорожке вызова.
    assert!(
        events[..adopted]
            .iter()
            .all(|event| !matches!(event, AdapterEvent::BlockStarted { parent: None, .. })),
        "{events:?}"
    );
    for event in &events {
        if let AdapterEvent::BlockStarted {
            input_command_id, ..
        } = event
        {
            assert_eq!(*input_command_id, Some(input.command_id));
        }
    }
    let Some(AdapterEvent::RunFinished {
        input_command_id,
        usage,
        ..
    }) = events.last()
    else {
        unreachable!();
    };
    assert_eq!(*input_command_id, input.command_id);
    assert!(!usage.is_empty(), "the autonomous run spent tokens");
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            ..
        }
    ));
    bench.close(CloseMode::Graceful).await;
}

/// Запуск без ввода: ход записи, затем начало периода и пауза на вмешательство теста.
fn autonomous_start(steps: Vec<Step>) -> (Script, String) {
    let mut script = two_turns();
    let session = session_of(&script);
    let terminal = first_terminal(&script);
    let Step::Emit { events } = &mut script.steps[terminal] else {
        unreachable!();
    };
    let end = events
        .iter()
        .position(|event| event["type"] == "session.execution.succeeded")
        .unwrap();
    events.truncate(end + 1);
    script.steps.truncate(terminal + 1);
    script.steps.push(Step::Emit {
        events: vec![
            json!({"type": "session.inbox.enqueued",
                "data": {"inboxID": "msg_synthetic", "sessionID": session,
                         "item": {"type": "synthetic"}}}),
            json!({"type": "session.execution.started", "data": {"sessionID": session}}),
        ],
    });
    script.steps.extend(steps);
    (script, session)
}

fn step_usage(session: &str, message: &str, input: u64) -> Vec<Value> {
    vec![
        json!({"type": "session.step.started", "data": {
            "sessionID": session, "assistantMessageID": message, "agent": "build",
            "model": {"id": "fake-1", "providerID": "fake"}}}),
        json!({"type": "session.step.ended", "data": {
            "sessionID": session, "assistantMessageID": message, "finish": "stop",
            "tokens": {"input": input, "output": 1, "reasoning": 0,
                       "cache": {"read": 0, "write": 0}}}}),
    ]
}

// Ввод обогнал конец запуска без ввода: период один, конец один, расход весь у нового ввода.
// Кадры периода выдуманы из записанных: запись такой гонки нет.
#[tokio::test]
async fn input_during_an_autonomous_run_shares_its_end() {
    let (mut script, session) = autonomous_start(Vec::new());
    let mut merged = step_usage(&session, "msg_auto", 10);
    merged.push(finish(&session));
    script.steps.push(Step::Expect {
        method: "POST".into(),
        path: format!("/api/session/{session}/prompt"),
        reply: Reply {
            body: Some(json!({"data": {"id": "$ID"}})),
            ..Default::default()
        },
        bind: [("$ID".to_owned(), "/id".to_owned())].into_iter().collect(),
    });
    script.steps.push(Step::Emit { events: merged });
    let mut steps = step_usage(&session, "msg_second", 5);
    steps.insert(
        0,
        json!({"type": "session.inbox.enqueued",
            "data": {"inboxID": "$ID", "sessionID": session}}),
    );
    steps.insert(
        1,
        json!({"type": "session.execution.started", "data": {"sessionID": session}}),
    );
    steps.insert(
        2,
        json!({"type": "session.inbox.delivered",
            "data": {"inboxID": "$ID", "sessionID": session}}),
    );
    steps.push(finish(&session));
    script.steps.push(Step::Emit { events: steps });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let first = bench.input("раз");
    bench.accepted(&first).await;
    bench.until_finished(first.command_id).await;
    loop {
        if matches!(bench.event().await, AdapterEvent::RunAdopted { .. }) {
            break;
        }
    }
    let second = bench.input("два");
    bench.accepted(&second).await;
    let events = bench.until_finished(second.command_id).await;
    let finished: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::RunFinished {
                input_command_id,
                usage,
                ..
            } => Some((*input_command_id, usage.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(finished.len(), 1, "{events:?}");
    assert_eq!(finished[0].0, second.command_id);
    assert_eq!(finished[0].1[0].input_tokens, 15, "both periods counted");
    bench.close(CloseMode::Graceful).await;
}

/// Настоящие кадры `live/merged_autonomous.jsonl`, начиная с этого ввода команды.
fn live_period(session: &str, command: &str, input: &str) -> Vec<Value> {
    let file = std::fs::read_to_string(evidence_path("live/merged_autonomous")).unwrap();
    file.lines()
        .filter_map(|line| {
            let record: Value = serde_json::from_str(line).unwrap();
            (record["dir"] == "sse").then(|| record["data"].clone())
        })
        .skip_while(|frame| frame["data"]["inboxID"] != command)
        .map(|frame| {
            let text = frame
                .to_string()
                .replace("ses_01a1934a953571f98f2ea7ff099453bc", session)
                .replace(command, input);
            serde_json::from_str(&text).unwrap()
        })
        .collect()
}

// Смоук, пункт 33: фоновая команда кончилась посреди хода, её `synthetic` доставлен в тот же
// период. Конец один и приписан вводу, второго хода нет.
#[tokio::test]
async fn background_shell_that_ends_during_a_turn_stays_inside_it() {
    let mut script = ready();
    let session = session_of(&script);
    let prompt = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(prompt);
    script.steps.push(Step::Expect {
        method: "POST".into(),
        path: format!("/api/session/{session}/prompt"),
        reply: Reply {
            body: Some(json!({"data": {"id": "$ID"}})),
            ..Default::default()
        },
        bind: [("$ID".to_owned(), "/id".to_owned())].into_iter().collect(),
    });
    script.steps.push(Step::Emit {
        events: live_period(&session, "msg_01a193536e71715888e52b9b5dd1d236", "$ID"),
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("дальше");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::RunAdopted { .. })),
        "{events:?}"
    );
    let AdapterEvent::RunFinished { usage, .. } = events.last().unwrap() else {
        unreachable!();
    };
    assert_eq!(usage[0].input_tokens, 402, "both steps are counted");
    assert_eq!(usage[0].output_tokens, 300);
    assert_eq!(usage[0].cache_read_tokens, 23_010);
    assert_eq!(text_of(&events, "Text").last().unwrap(), "finished");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), bench.adapter.next_event())
            .await
            .is_ok_and(|event| matches!(
                event,
                AdapterEvent::Phase {
                    phase: Phase::AwaitingInput,
                    ..
                }
            ))
    );
    bench.close(CloseMode::Graceful).await;
}

// Запрос в запуске без ввода показать есть кому: ход приписан последней команде.
#[tokio::test]
async fn request_during_an_autonomous_run_reaches_the_owner() {
    let capture = Script::from_capture("permissions", None);
    let asked = tool_events(&capture, &["permission.asked"]).remove(0);
    let mut own = asked.clone();
    let (mut script, session) = autonomous_start(Vec::new());
    own["data"]["sessionID"] = session.clone().into();
    let id = own["data"]["id"].as_str().unwrap().to_owned();
    script.steps.push(Step::Emit { events: vec![own] });
    script.set_route(
        "POST",
        &format!("/api/session/{session}/permission/{id}/reply"),
        vec![reply_204()],
    );
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let first = bench.input("раз");
    bench.accepted(&first).await;
    bench.until_finished(first.command_id).await;
    let (request, ..) = bench.until_request().await;
    assert!(matches!(
        bench.deliver(&request, &allow()).await,
        DecisionOutcome::Applied(_)
    ));
    bench.close(CloseMode::Graceful).await;
}

// Запуск без ввода и без единой команды: приписать некому, но ход не теряется.
#[tokio::test]
async fn autonomous_run_with_no_command_ends_without_a_run() {
    let mut script = ready();
    let session = session_of(&script);
    let prompt = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(prompt);
    script.steps.push(Step::Stall { ms: 300 });
    script.steps.push(Step::Emit {
        events: vec![
            json!({"type": "session.execution.started", "data": {"sessionID": session}}),
            finish(&session),
        ],
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Diagnostic {
            kind: "autonomous_run_unattributed",
            required: false,
            ..
        }
    ));
    assert!(matches!(
        bench.event().await,
        AdapterEvent::Phase {
            phase: Phase::AwaitingInput,
            ..
        }
    ));
    bench.close(CloseMode::Graceful).await;
}

// Запрос потомка отвечается на его путь.
#[tokio::test]
async fn request_of_a_child_session_goes_to_the_child_path() {
    let capture = Script::from_capture("permissions", None);
    let session = session_of(&capture);
    let child = "ses_child00000000000000000000000".to_owned();
    let mut asked = tool_events(&capture, &["permission.asked"]).remove(0);
    asked["data"]["sessionID"] = child.clone().into();
    let request = asked["data"]["id"].as_str().unwrap().to_owned();
    let mut script = capture
        .clone()
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    replace_events_after_prompt(
        &mut script,
        vec![
            Step::Emit {
                events: vec![
                    json!({"type": "session.created",
                        "data": {"sessionID": child, "parentID": session}}),
                    asked,
                ],
            },
            Step::Stall { ms: 200 },
            Step::Emit {
                events: vec![finish(&session)],
            },
        ],
    );
    script.set_route(
        "POST",
        &format!("/api/session/{child}/permission/{request}/reply"),
        vec![reply_204()],
    );
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let (opened, payload, _) = bench.until_request().await;
    assert_eq!(payload["agent"], "subagent");
    assert!(matches!(
        bench.deliver(&opened, &allow()).await,
        DecisionOutcome::Applied(_)
    ));
    bench.until_finished(input.command_id).await;
    let requests = bench.fake.requests();
    assert!(
        find(
            &requests,
            "POST",
            &format!("{child}/permission/{request}/reply")
        )
        .is_some()
    );
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn refused_request_outside_our_sessions_is_ignored() {
    let capture = Script::from_capture("permissions", None);
    let session = session_of(&capture);
    let mut asked = tool_events(&capture, &["permission.asked"]).remove(0);
    asked["data"]["sessionID"] = "ses_stranger".into();
    let mut script = capture
        .clone()
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents();
    replace_events_after_prompt(
        &mut script,
        vec![
            Step::Emit {
                events: vec![asked],
            },
            Step::Emit {
                events: vec![finish(&session)],
            },
        ],
    );
    let mut bench = Bench::new(script);
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    assert!(find(&bench.fake.requests(), "POST", "/reply").is_none());
    bench.close(CloseMode::Graceful).await;
}

/// Соединение реле: сервер OpenCode говорит с брокером через тот же сокет.
struct Relay {
    reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl Relay {
    async fn connect(bench: &Bench) -> Self {
        use tokio::io::AsyncWriteExt as _;
        let stream = tokio::net::UnixStream::connect(bench.exec_dir.join("mcp.sock"))
            .await
            .unwrap();
        let (read, mut writer) = stream.into_split();
        let content: Value =
            serde_json::from_str(&bench.env("OPENCODE_CONFIG_CONTENT").unwrap()).unwrap();
        let secret = content["mcp"]["servers"]["canterlot"]["environment"]["CANTERLOT_MCP_SECRET"]
            .as_str()
            .expect("the relay gets the socket secret")
            .to_owned();
        writer
            .write_all(canterlot_worker::adapter::canterlot_mcp::relay_hello(&secret).as_bytes())
            .await
            .unwrap();
        Self {
            reader: tokio::io::BufReader::new(read),
            writer,
        }
    }

    async fn send(&mut self, message: Value) {
        use tokio::io::AsyncWriteExt as _;
        let mut line = message.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.unwrap();
    }

    async fn reply(&mut self) -> Value {
        use tokio::io::AsyncBufReadExt as _;
        let mut line = String::new();
        tokio::time::timeout(WAIT, self.reader.read_line(&mut line))
            .await
            .expect("relay reply")
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn call(&mut self, id: u64, name: &str) {
        self.send(json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": {}},
        }))
        .await;
    }
}

// Набор инструментов и текст бюджета: сервер получает их при подключении реле.
#[tokio::test]
async fn relay_sees_the_preset_tools_and_the_opencode_budget() {
    let mut bench = Bench::new(session_only());
    bench.start_ok().await;
    let mut relay = Relay::connect(&bench).await;
    relay
        .send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        .await;
    let list = relay.reply().await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "canterlot_limits",
            "share_file",
            "canterlot_presets_list",
            "canterlot_preset_save",
            "canterlot_preset_apply",
            "sessions_list",
            "session_inspect",
            "session_send",
            "session_spawn",
            "session_subscribe",
            "session_unsubscribe",
            "session_wait",
            "session_accept",
            "session_interrupt",
            "session_stop",
            "alarm_set",
            "alarm_cancel",
            "alarm_list",
            "nudge_set",
            "nudge_clear",
            "route_preview",
            "spawn_cancel",
            "plan_list",
            "plan_add",
            "plan_update",
            "plan_adopt",
            "history_search"
        ]
    );
    bench.close(CloseMode::Graceful).await;
}

// Лимиты и пресеты идут через хаб и возвращаются результатом инструмента.
#[tokio::test]
async fn limits_and_preset_calls_go_through_the_broker() {
    let mut bench = Bench::new(session_only());
    bench.start_ok().await;
    let mut relay = Relay::connect(&bench).await;
    relay.call(1, "canterlot_limits").await;
    let event = bench.event().await;
    let AdapterEvent::Limits { call_id } = event else {
        panic!("{event:?}");
    };
    bench
        .adapter
        .agent_call_result(
            call_id,
            AgentCallResult::Ok(json!({"text": "no limit data yet in this session"})),
        )
        .await;
    let answer = relay.reply().await;
    assert_eq!(
        answer["result"]["content"][0]["text"],
        "no limit data yet in this session"
    );
    relay.call(2, "canterlot_presets_list").await;
    let event = bench.event().await;
    let AdapterEvent::AgentCall { call_id, tool, .. } = event else {
        panic!("{event:?}");
    };
    assert_eq!(tool, "presets.list");
    bench
        .adapter
        .agent_call_result(call_id, AgentCallResult::Ok(json!({"presets": []})))
        .await;
    let answer = relay.reply().await;
    assert_eq!(answer["id"], 2);
    assert!(
        answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("presets"),
        "{answer}"
    );
    bench.close(CloseMode::Graceful).await;
}

/// Запись настоящего сервера с разрывом потока или перезапуском (`recovery.jsonl`).
fn recorded(scenario: &str) -> Script {
    Script::from_capture("recovery", Some(scenario))
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents()
}

const RECORDED_SESSION: &str = "ses_0190cafe000070008000000000000001";

#[derive(Debug, Default)]
struct Ledger {
    started: usize,
    deltas: String,
    finals: Vec<String>,
}

/// Что собрал бы хаб: повторный старт или второй финал блока здесь видны.
fn ledger(events: &[AdapterEvent]) -> std::collections::BTreeMap<Uuid, Ledger> {
    let mut blocks = std::collections::BTreeMap::<Uuid, Ledger>::new();
    for event in events {
        match event {
            AdapterEvent::BlockStarted { block_id, .. } => {
                blocks.entry(*block_id).or_default().started += 1;
            }
            AdapterEvent::BlockDelta { block_id, append } => {
                blocks.entry(*block_id).or_default().deltas.push_str(append);
            }
            AdapterEvent::BlockFinal { block_id, text, .. } => {
                blocks
                    .entry(*block_id)
                    .or_default()
                    .finals
                    .push(text.clone());
            }
            _ => {}
        }
    }
    blocks
}

fn assert_each_block_once(events: &[AdapterEvent]) -> Vec<String> {
    let blocks = ledger(events);
    for (id, block) in &blocks {
        assert_eq!(block.started, 1, "{id}: {blocks:?}");
        assert_eq!(block.finals.len(), 1, "{id}: {blocks:?}");
        assert!(
            block.deltas.is_empty() || block.deltas == block.finals[0],
            "the hub checksum would not match: {id}: {blocks:?}"
        );
    }
    let mut texts: Vec<String> = blocks
        .into_values()
        .flat_map(|block| block.finals)
        .collect();
    texts.sort();
    texts
}

fn usage_of(events: &[AdapterEvent]) -> (u64, u64) {
    events
        .iter()
        .find_map(|event| match event {
            AdapterEvent::RunFinished { usage, .. } => Some((
                usage.iter().map(|model| model.input_tokens).sum(),
                usage.iter().map(|model| model.output_tokens).sum(),
            )),
            _ => None,
        })
        .unwrap()
}

fn prompts(bench: &Bench) -> usize {
    bench
        .fake
        .requests()
        .iter()
        .filter(|request| request["method"] == "POST" && path_of(request).ends_with("/prompt"))
        .count()
}

fn subscriptions(bench: &Bench) -> usize {
    bench
        .fake
        .requests()
        .iter()
        .filter(|request| path_of(request) == "/api/event")
        .count()
}

// Разрыв посреди хода: конец текста, вызов и его результат ушли в разрыв, второй шаг идёт
// уже по новому потоку. Каждый блок один раз, текст целиком, расход без задвоения.
#[tokio::test]
async fn stream_cut_loses_no_block_and_duplicates_none() {
    let mut bench = Bench::new(recorded("cut_midturn"));
    bench.start_ok().await;
    let input = bench.input("начни");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert_eq!(
        assert_each_block_once(&events),
        [
            "echo из разрыва",
            "думаю ",
            "из разрыва\n",
            "первый кусок",
            "после разрыва"
        ]
    );
    assert_eq!(usage_of(&events), (200, 40));
    assert_eq!(prompts(&bench), 1, "no prompt is sent twice");
    assert_eq!(subscriptions(&bench), 2);
    bench.close(CloseMode::Graceful).await;
}

// Конец хода целиком в разрыве: живого терминала не будет, ход закрывает проекция.
#[tokio::test]
async fn stream_cut_over_the_end_of_the_turn_finishes_from_the_projection() {
    let mut bench = Bench::new(recorded("cut_terminal_lost"));
    bench.start_ok().await;
    let input = bench.input("начни");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert_eq!(assert_each_block_once(&events), ["начало конец"]);
    assert_eq!(usage_of(&events), (100, 20));
    let next = bench.event().await;
    assert!(
        matches!(
            next,
            AdapterEvent::Phase {
                phase: Phase::AwaitingInput,
                ..
            }
        ),
        "{next:?}"
    );
    assert_eq!(prompts(&bench), 1);
    bench.close(CloseMode::Graceful).await;
}

// Терминал пришёл уже по новому потоку, а проекция его видела: следующий ввод им не закрыть.
#[tokio::test]
async fn late_terminal_of_a_reconciled_turn_does_not_end_the_next_one() {
    let mut script = recorded("cut_terminal_lost");
    let session = session_of(&script);
    let terminal = json!({
        "type": "session.execution.succeeded",
        "data": {"sessionID": session},
    });
    // Придуманный порядок: этот же кадр настоящий сервер шлёт раньше подписки либо позже.
    script.steps.push(Step::Stall { ms: 300 });
    script.steps.push(Step::Emit {
        events: vec![terminal],
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("начни");
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let drained = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            let event = bench.adapter.next_event().await;
            assert!(
                !matches!(event, AdapterEvent::RunFinished { .. }),
                "{event:?}"
            );
        }
    })
    .await;
    assert!(drained.is_err());
    bench.close(CloseMode::Graceful).await;
}

// Запрос родился в разрыве: карточка открывается по проекции один раз, решение доходит.
#[tokio::test]
async fn permission_asked_in_the_gap_reopens_from_the_projection() {
    let mut bench = Bench::new(recorded("cut_permission"));
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("начни");
    bench.accepted(&input).await;
    let (request, payload, events) = bench.until_request().await;
    assert_eq!(payload["command"], "echo разрешение");
    let outcome = bench.deliver(&request, &allow()).await;
    assert!(
        outcome.is_applied(),
        "{outcome:?}: {:?}",
        bench.fake.requests().last()
    );
    let mut events = events;
    events.extend(bench.until_finished(input.command_id).await);
    let opened = events
        .iter()
        .filter(|event| matches!(event, AdapterEvent::RequestOpened { .. }))
        .count();
    assert_eq!(opened, 1);
    assert_each_block_once(&events);
    assert_eq!(prompts(&bench), 1);
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn question_asked_in_the_gap_reopens_from_the_projection() {
    let mut bench = Bench::new(recorded("cut_form"));
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("начни");
    bench.accepted(&input).await;
    let (request, payload, _) = bench.until_request().await;
    assert_eq!(payload["questions"][0]["text"], "Какой цвет?");
    let answer = DecisionPayload::Question {
        answers: [("q0".to_owned(), AnswerValue::Single("Синий".into()))]
            .into_iter()
            .collect(),
    };
    assert!(bench.deliver(&request, &answer).await.is_applied());
    bench.until_finished(input.command_id).await;
    let replies = bench.replies();
    assert_eq!(replies[0]["body"], json!({"answer": {"q0": "Синий"}}));
    bench.close(CloseMode::Graceful).await;
}

// Потомок родился в разрыве: о нём знает только список сессий, ответ идёт на его путь.
#[tokio::test]
async fn permission_of_a_child_born_in_the_gap_is_found() {
    let mut bench = Bench::new(recorded("cut_child_permission"));
    bench.spec.permission_mode = "ask".into();
    bench.start_ok().await;
    let input = bench.input("начни");
    bench.accepted(&input).await;
    let (request, payload, _) = bench.until_request().await;
    assert_eq!(payload["agent"], "subagent");
    assert!(bench.deliver(&request, &allow()).await.is_applied());
    let replies = bench.replies();
    assert!(
        path_of(&replies[0]).contains("ses_e6f9fe28"),
        "{:?}",
        replies[0]
    );
    bench.close(CloseMode::Graceful).await;
}

// Сервер умер вместе с потоком: это выход, а не разрыв, и повторной подписки не ждём.
#[tokio::test]
async fn dead_server_is_an_exit_not_a_gap() {
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    script.steps.truncate(position + 1);
    script.steps.push(Step::Stall { ms: 100 });
    script.steps.push(Step::Die {
        code: 9,
        kill: true,
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    loop {
        if let AdapterEvent::ProcessExited { signal, .. } = bench.event().await {
            assert_eq!(signal, Some(9));
            break;
        }
    }
    assert_eq!(subscriptions(&bench), 1);
    bench.close(CloseMode::Graceful).await;
}

async fn resumed(scenario: &str) -> (Bench, Vec<AdapterEvent>) {
    let mut script = recorded(scenario);
    script.set_route(
        "PATCH",
        &format!("/api/session/{RECORDED_SESSION}"),
        vec![reply_204()],
    );
    script.set_route(
        "POST",
        &format!("/api/session/{RECORDED_SESSION}/model"),
        vec![reply_204()],
    );
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(RECORDED_SESSION.to_owned());
    bench.start_ok().await;
    let mut events = Vec::new();
    while let Ok(event) =
        tokio::time::timeout(Duration::from_millis(300), bench.adapter.next_event()).await
    {
        events.push(event);
    }
    (bench, events)
}

// После SIGKILL инструмент в проекции `running`, а исполнения нет: это сирота. Текст до него
// находится, расход к чужому ходу не приписывается, ввод не повторяется.
#[tokio::test]
async fn restart_closes_the_orphan_tool_and_finds_what_was_lost() {
    let (mut bench, events) = resumed("kill_mid_tool").await;
    let texts = assert_each_block_once(&events);
    assert!(texts.contains(&"до команды".to_owned()), "{texts:?}");
    assert!(
        texts.iter().any(|text| text.contains("прерван")),
        "{texts:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockFinal { summary: Some(summary), .. }
                if summary.ends_with(", interrupted")
        )),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::RunFinished { .. }))
    );
    assert_eq!(prompts(&bench), 0, "no prompt is sent on resume");
    // Смоук: чтение истории оставляло фазу `generating` у сессии, которая ждёт ввода.
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::Phase { .. })),
        "{events:?}"
    );
    // Следующий ввод: сервер сам закрывает тот же вызов, блок уже закрыт.
    let input = bench.input("дальше");
    bench.accepted(&input).await;
    let next = bench.until_finished(input.command_id).await;
    assert!(
        !next.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockFinal { summary: Some(summary), .. }
                if summary.contains("shell")
        )),
        "{next:?}"
    );
    assert_eq!(assert_each_block_once(&next), ["дальше"]);
    assert_eq!(usage_of(&next), (100, 20), "only the new step");
    bench.close(CloseMode::Graceful).await;
}

// EOF stdin: сервер успел закрыть инструмент сам, сирот нет, но итог надо показать.
#[tokio::test]
async fn restart_after_the_stdin_lease_shows_the_closed_tool() {
    let (mut bench, events) = resumed("eof_mid_tool").await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockFinal { summary: Some(summary), .. }
                if summary.ends_with(", interrupted")
        )),
        "{events:?}"
    );
    assert_each_block_once(&events);
    bench.close(CloseMode::Graceful).await;
}

// SIGKILL посреди текста: проекция пуста, показывать нечего, блоков не появляется.
#[tokio::test]
async fn restart_after_a_kill_in_the_middle_of_a_text_adds_nothing() {
    let (mut bench, events) = resumed("kill_mid_text").await;
    assert!(ledger(&events).is_empty(), "{events:?}");
    bench.close(CloseMode::Graceful).await;
}

// Ввод пережил рестарт в очереди: хаб считает его неопределённым, и следующий ввод не должен
// исполнить его вместе с собой.
#[tokio::test]
async fn restart_cancels_the_input_left_in_the_queue() {
    let (mut bench, events) = resumed("queued_restart").await;
    let requests = bench.fake.requests();
    let cancel = requests
        .iter()
        .find(|request| request["method"] == "DELETE")
        .expect("inbox cancel");
    assert!(path_of(cancel).contains("/inbox/msg_"), "{cancel}");
    assert!(
        events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockFinal { text, .. } if text.contains("очереди")
        )),
        "{events:?}"
    );
    bench.close(CloseMode::Graceful).await;
}

// Проекция с картинками больше потолка ответа: сверка не падает, а читает страницами мельче.
#[tokio::test]
async fn projection_over_the_body_cap_is_read_in_smaller_pages() {
    let mut script = recorded("kill_mid_tool");
    let path = format!("/api/session/{RECORDED_SESSION}/message");
    let recorded = script.route("GET", &path).expect("recorded").replies[0].clone();
    script.set_route(
        "GET",
        &path,
        vec![
            Reply {
                raw: Some(" ".repeat(65 * 1024 * 1024)),
                ..Default::default()
            },
            recorded,
        ],
    );
    script.set_route(
        "PATCH",
        &format!("/api/session/{RECORDED_SESSION}"),
        vec![reply_204()],
    );
    script.set_route(
        "POST",
        &format!("/api/session/{RECORDED_SESSION}/model"),
        vec![reply_204()],
    );
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(RECORDED_SESSION.to_owned());
    bench.start_ok().await;
    let reads: Vec<String> = bench
        .fake
        .requests()
        .iter()
        .filter(|request| path_of(request).ends_with("/message"))
        .map(|request| request["query"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(reads, ["limit=100&order=desc", "limit=25&order=desc"]);
    bench.close(CloseMode::Graceful).await;
}

fn resume_with_history(replies: Vec<Reply>) -> Script {
    let mut script = recorded("kill_mid_tool");
    let path = format!("/api/session/{RECORDED_SESSION}/message");
    script.set_route("GET", &path, replies);
    for (method, tail) in [("PATCH", ""), ("POST", "/model")] {
        script.set_route(
            method,
            &format!("/api/session/{RECORDED_SESSION}{tail}"),
            vec![reply_204()],
        );
    }
    script
}

fn history_reads(bench: &Bench) -> Vec<String> {
    bench
        .fake
        .requests()
        .iter()
        .filter(|request| path_of(request).ends_with("/message"))
        .map(|request| request["query"].as_str().unwrap().to_owned())
        .collect()
}

async fn sees_unreadable_history(bench: &mut Bench) -> bool {
    for _ in 0..8 {
        let event = bench.event().await;
        if matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "history_unreadable",
                required: false,
                ..
            }
        ) {
            return true;
        }
    }
    false
}

// Сообщение крупнее потолка не прочитать и страницей из одного: иначе resume_failed навсегда,
// ход не запустить, и оно не уйдёт из хвоста.
#[tokio::test]
async fn message_over_the_body_cap_does_not_stop_a_resume() {
    let script = resume_with_history(vec![Reply {
        raw: Some(" ".repeat(65 * 1024 * 1024)),
        ..Default::default()
    }]);
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(RECORDED_SESSION.to_owned());
    bench.start_ok().await;
    assert_eq!(
        history_reads(&bench),
        [
            "limit=100&order=desc",
            "limit=25&order=desc",
            "limit=6&order=desc",
            "limit=1&order=desc"
        ]
    );
    assert!(sees_unreadable_history(&mut bench).await);
    bench.close(CloseMode::Graceful).await;
}

// То же с сообщением, которое не разбирается: например, глубже предела рекурсии разбора.
#[tokio::test]
async fn message_that_does_not_parse_does_not_stop_a_resume() {
    let script = resume_with_history(vec![Reply {
        raw: Some("{".to_owned()),
        ..Default::default()
    }]);
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(RECORDED_SESSION.to_owned());
    bench.start_ok().await;
    assert_eq!(history_reads(&bench).len(), 4);
    assert!(sees_unreadable_history(&mut bench).await);
    bench.close(CloseMode::Graceful).await;
}

// Разрыв потока с таким сообщением в хвосте не снимает сервер посреди хода.
#[tokio::test]
async fn unreadable_message_does_not_end_a_stream_recovery() {
    let mut script = ready();
    let session = session_of(&script);
    script.set_route(
        "GET",
        &format!("/api/session/{session}/message"),
        vec![Reply {
            raw: Some("{".to_owned()),
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    bench.adapter.cut_stream();
    let input = bench.input("Привет");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "event_stream_lost",
                ..
            }
        )),
        "{events:?}"
    );
    assert_eq!(subscriptions(&bench), 2);
    bench.close(CloseMode::Graceful).await;
}

// Страница мельче после картинки не должна остаться мелкой: двадцать страниц по одному
// сообщению не дошли бы до начала хода.
#[tokio::test]
async fn history_follows_the_cursor_and_the_page_grows_back() {
    let page = |count: usize, next: &str| Reply {
        body: Some(json!({
            "data": (0..count)
                .map(|n| json!({"type": "user", "id": format!("msg_{next}_{n}")}))
                .collect::<Vec<_>>(),
            "cursor": {"next": next},
        })),
        ..Default::default()
    };
    let script = resume_with_history(vec![
        Reply {
            raw: Some(" ".repeat(65 * 1024 * 1024)),
            ..Default::default()
        },
        page(25, "second"),
        page(3, "end"),
    ]);
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(RECORDED_SESSION.to_owned());
    bench.start_ok().await;
    assert_eq!(
        history_reads(&bench),
        [
            "limit=100&order=desc",
            "limit=25&order=desc",
            "limit=100&cursor=second"
        ]
    );
    bench.close(CloseMode::Graceful).await;
}

// Кадр больше потолка потерян, как при разрыве: инструмент закрывается по проекции.
#[tokio::test]
async fn oversized_frame_is_recovered_from_the_projection() {
    let mut script = recorded("cut_midturn");
    let drop_at = script
        .steps
        .iter()
        .position(|step| matches!(step, Step::DropSse))
        .unwrap();
    script.steps[drop_at] = Step::Frame {
        raw: format!("data: {}\n\n", "x".repeat(65 * 1024 * 1024)),
    };
    script.steps.remove(drop_at + 1);
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("начни");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "oversized_frame",
                ..
            }
        )),
        "{events:?}"
    );
    assert_eq!(subscriptions(&bench), 1, "the stream stays");
    assert_each_block_once(&events);
    bench.close(CloseMode::Graceful).await;
}

// Кадр не разобран, как и слишком большой: результат инструмента читается из проекции.
#[tokio::test]
async fn undecodable_frame_is_recovered_from_the_projection() {
    let mut script = recorded("cut_midturn");
    let drop_at = script
        .steps
        .iter()
        .position(|step| matches!(step, Step::DropSse))
        .unwrap();
    script.steps[drop_at] = Step::Frame {
        raw: "data: {\"type\":\"session.tool.succ\n\n".to_owned(),
    };
    script.steps.remove(drop_at + 1);
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("начни");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "unparsed_event",
                ..
            }
        )),
        "{events:?}"
    );
    assert_eq!(subscriptions(&bench), 1, "the stream stays");
    assert_each_block_once(&events);
    bench.close(CloseMode::Graceful).await;
}

// Сервер режет строки по UTF-16: половина эмодзи в истории не должна закрыть возобновление.
#[tokio::test]
async fn lone_surrogate_in_history_does_not_stop_a_resume() {
    let mut script = recorded("kill_mid_tool");
    let path = format!("/api/session/{RECORDED_SESSION}/message");
    let recorded = script.route("GET", &path).expect("recorded").replies[0].clone();
    let raw = recorded.body.clone().unwrap().to_string().replacen(
        "\"data\":[{",
        "\"data\":[{\"broken\":\"\\ud83d\",",
        1,
    );
    assert!(raw.contains("\\ud83d"));
    script.set_route(
        "GET",
        &path,
        vec![Reply {
            raw: Some(raw),
            ..Default::default()
        }],
    );
    for (method, tail) in [("PATCH", ""), ("POST", "/model")] {
        script.set_route(
            method,
            &format!("/api/session/{RECORDED_SESSION}{tail}"),
            vec![reply_204()],
        );
    }
    let mut bench = Bench::new(script);
    bench.spec.native_conversation_id = Some(RECORDED_SESSION.to_owned());
    bench.start_ok().await;
    bench.close(CloseMode::Graceful).await;
}

// Ввод, лёгший между чтениями снимка, виден в истории, но не в первом ответе `active`:
// без второго чтения идущий ход закрылся бы как прерванный.
#[tokio::test]
async fn snapshot_reads_the_active_sessions_again_after_the_history() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    bench.adapter.cut_stream();
    let input = bench.input("Привет");
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    let requests = bench.fake.requests();
    let reads: Vec<&str> = requests
        .iter()
        .map(path_of)
        .filter(|path| path.ends_with("/active") || path.ends_with("/message"))
        .collect();
    assert!(
        reads.len() >= 2 && reads[reads.len() - 1].ends_with("/active"),
        "{reads:?}"
    );
    bench.close(CloseMode::Graceful).await;
}

// Ввод, пришедший в разрыв, не ждёт и не пропадает: сервер жив, события добирает сверка.
#[tokio::test]
async fn input_during_a_reconnect_goes_through() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    bench.adapter.cut_stream();
    let input = bench.input("Привет");
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    assert_eq!(prompts(&bench), 1);
    assert_eq!(subscriptions(&bench), 2);
    bench.close(CloseMode::Graceful).await;
}

// Сервер и супервизор умерли вместе: команду прошлого сервера снимает новый запуск по журналу.
#[tokio::test]
async fn resume_stops_the_shell_journalled_by_the_dead_server() {
    use std::os::unix::process::CommandExt as _;
    let mut orphan = std::process::Command::new("sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .unwrap();
    let mut script = ready();
    let session = session_of(&script);
    script.set_route(
        "GET",
        &format!("/api/session/{session}"),
        vec![Reply {
            body: Some(json!({"data": {"id": session}})),
            ..Default::default()
        }],
    );
    script.set_route(
        "PATCH",
        &format!("/api/session/{session}"),
        vec![reply_204()],
    );
    script.set_route(
        "POST",
        &format!("/api/session/{session}/model"),
        vec![reply_204()],
    );
    script.set_route(
        "GET",
        &format!("/api/session/{session}/message"),
        vec![Reply {
            body: Some(json!({"data": []})),
            ..Default::default()
        }],
    );
    let mut bench = Bench::new(script);
    let journal = bench
        .fake
        .dir
        .path()
        .join("data/opencode/shells")
        .join(format!("{session}.json"));
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let ticks = canterlot_worker::containment::start_ticks(orphan.id()).unwrap();
    std::fs::write(
        &journal,
        json!([{"pid": orphan.id(), "start_ticks": ticks}]).to_string(),
    )
    .unwrap();
    bench.spec.native_conversation_id = Some(session);
    bench.start_ok().await;
    // Статус забирает уборка соседних тестов: смотрим на сам процесс.
    let pid = orphan.id();
    let stopped = tokio::time::timeout(WAIT, async {
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            let state = stat
                .rfind(')')
                .and_then(|end| stat[end + 1..].trim_start().chars().next());
            if matches!(state, None | Some('Z' | 'X')) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    stopped.await.expect("the orphan is stopped");
    let _ = orphan.wait();
    assert!(!journal.exists());
    bench.close(CloseMode::Graceful).await;
}

/// Записанный ход, конец которого заменён отказом.
fn failing_turn(error: Value) -> Script {
    let mut script = ready();
    for step in &mut script.steps {
        let Step::Emit { events } = step else {
            continue;
        };
        for event in events {
            if event["type"] == "session.execution.succeeded" {
                event["type"] = "session.execution.failed".into();
                event["data"]["error"] = error.clone();
            }
        }
    }
    script
}

async fn failed_turn(error: Value) -> (Bench, Vec<AdapterEvent>) {
    let mut bench = Bench::new(failing_turn(error));
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    (bench, events)
}

impl Bench {
    async fn until(&mut self, wanted: impl Fn(&AdapterEvent) -> bool) -> Vec<AdapterEvent> {
        let mut events = Vec::new();
        loop {
            let event = self.event().await;
            let done = wanted(&event);
            events.push(event);
            if done {
                return events;
            }
        }
    }
}

impl Bench {
    async fn compact(&mut self, command: Uuid) -> CompactOutcome {
        tokio::time::timeout(WAIT, self.adapter.compact(command))
            .await
            .expect("compact must finish")
    }
}

fn limit_of(events: &[AdapterEvent]) -> Option<LimitFact> {
    events.iter().find_map(|event| match event {
        AdapterEvent::Limit(fact) => Some(fact.clone()),
        _ => None,
    })
}

fn reset_in(fact: &LimitFact, wait: u64) -> bool {
    let LimitFact::Reached {
        resets_at_ms: Some(at),
        ..
    } = fact
    else {
        return false;
    };
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    at.abs_diff(now + wait) < 60_000
}

#[tokio::test]
async fn go_quota_is_a_reached_limit_recorded_inside_the_turn() {
    let (mut bench, events) = failed_turn(json!({
        "type": "provider.quota", "message": "Go usage limit reached", "status": 429,
        "retryAfterMs": 3_600_000}))
    .await;
    let fact = limit_of(&events).expect("limit fact");
    assert!(matches!(
        fact,
        LimitFact::Reached {
            window: LimitWindow::FiveHour,
            ..
        }
    ));
    assert!(reset_in(&fact, 3_600_000), "{fact:?}");
    assert_eq!(text_of(&events, "Error").len(), 1);
    bench.close(CloseMode::Graceful).await;
}

// Тот же отказ Go в 2.2.5 и 2.2.6: свой тип вместо `provider.quota` (живой кадр 09.11.2026).
#[tokio::test]
async fn go_limit_of_newer_servers_is_the_same_reached_limit() {
    let (mut bench, events) = failed_turn(json!({
        "type": "provider.go-limit",
        "message": "OpenCode Go usage limit reached. Switch to a free model, or turn on Extra Usage on the Go page in OpenCode Console to keep going.",
        "status": 429, "retryAfterMs": 3_600_000}))
    .await;
    let fact = limit_of(&events).expect("limit fact");
    assert!(reset_in(&fact, 3_600_000), "{fact:?}");
    assert_eq!(text_of(&events, "Error").len(), 1);
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn free_limit_waits_in_the_long_window() {
    let (mut bench, events) = failed_turn(json!({
        "type": "provider.free-limit", "message": "Free usage limit reached",
        "status": 429, "retryAfterMs": 80_814_000}))
    .await;
    assert!(matches!(
        limit_of(&events),
        Some(LimitFact::Reached {
            window: LimitWindow::Weekly,
            ..
        })
    ));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn other_failures_are_no_limit_and_name_only_their_kind() {
    let (mut bench, events) = failed_turn(json!({
        "type": "provider.internal", "message": "секрет провайдера"}))
    .await;
    assert_eq!(limit_of(&events), None);
    let more = bench
        .until(|event| matches!(event, AdapterEvent::Diagnostic { kind, .. } if *kind == "provider_failure"))
        .await;
    let Some(AdapterEvent::Diagnostic {
        event_type,
        required,
        ..
    }) = more.last()
    else {
        panic!("diagnostic");
    };
    assert_eq!(event_type, "provider.internal");
    assert!(!required);
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn rejected_credentials_stop_the_session_with_their_reason() {
    let (mut bench, events) = failed_turn(json!({
        "type": "provider.auth", "message": "invalid api key", "status": 401}))
    .await;
    assert_eq!(
        text_of(&events, "Error").len(),
        1,
        "the owner sees the text"
    );
    let more = bench
        .until(|event| matches!(event, AdapterEvent::Diagnostic { kind, .. } if *kind == "auth_required"))
        .await;
    assert!(matches!(
        more.last(),
        Some(AdapterEvent::Diagnostic { required: true, .. })
    ));
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn overflow_the_server_could_not_recover_is_context_exhausted() {
    let (mut bench, _) = failed_turn(json!({
        "type": "provider.invalid-request", "message": "prompt is too long", "status": 400}))
    .await;
    bench
        .until(|event| matches!(event, AdapterEvent::ContextExhausted))
        .await;
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn failed_compaction_with_an_overflow_exhausts_the_context_after_the_turn() {
    let mut script = failing_turn(json!({"type": "unknown", "message": "после сжатия"}));
    let position = script
        .steps
        .iter()
        .rposition(|step| matches!(step, Step::Emit { .. }))
        .unwrap();
    let session = session_of(&script);
    if let Step::Emit { events } = &mut script.steps[position] {
        // Придуманный кадр: отказа автоматического сжатия нет в записях.
        let last = events.len() - 1;
        events.insert(
            last,
            json!({"type": "session.compaction.failed", "data": {
                "sessionID": session, "reason": "auto",
                "error": {"type": "provider.invalid-request", "message": "prompt is too long"}}}),
        );
    }
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::ContextExhausted))
    );
    bench
        .until(|event| matches!(event, AdapterEvent::ContextExhausted))
        .await;
    bench.close(CloseMode::Graceful).await;
}

// Запрос к модели повторяет сервер: со стороны адаптера это фаза, ввод уходит один раз.
#[tokio::test]
async fn retry_of_the_server_sends_no_second_prompt() {
    let mut script = ready();
    let session = session_of(&script);
    let position = script
        .steps
        .iter()
        .rposition(|step| matches!(step, Step::Emit { .. }))
        .unwrap();
    if let Step::Emit { events } = &mut script.steps[position] {
        let end = events
            .iter()
            .position(|event| event["type"] == "session.execution.succeeded")
            .unwrap();
        events.insert(
            end,
            json!({"type": "session.retry.scheduled", "data": {
                "sessionID": session, "assistantMessageID": "msg_1", "attempt": 2, "at": 1,
                "error": {"type": "provider.rate-limit", "message": "slow down", "status": 429}}}),
        );
    }
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    let events = bench.until_finished(input.command_id).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Phase { summary: Some(summary), .. } if summary == "retry 2"
    )));
    assert!(text_of(&events, "Error").is_empty());
    let prompts = bench
        .fake
        .requests()
        .iter()
        .filter(|request| path_of(request).ends_with("/prompt"))
        .count();
    assert_eq!(prompts, 1);
    bench.close(CloseMode::Graceful).await;
}

fn stuck_session() -> Script {
    Script::from_capture("stuck_invalid_request", None)
        .with_canterlot_mcp("connected")
        .with_recorded_models()
        .with_recorded_agents()
}

// Запись целиком: два отказа подряд, сжатие, следующий ввод проходит.
#[tokio::test]
async fn same_refusal_twice_offers_compaction_and_compaction_clears_it() {
    let mut bench = Bench::new(stuck_session());
    bench.start_ok().await;
    let one = bench.input("one");
    bench.accepted(&one).await;
    let first = bench.until_finished(one.command_id).await;
    assert!(
        !first
            .iter()
            .any(|event| matches!(event, AdapterEvent::SessionStuck))
    );
    let two = bench.input("two");
    bench.accepted(&two).await;
    bench.until_finished(two.command_id).await;
    bench
        .until(|event| matches!(event, AdapterEvent::SessionStuck))
        .await;
    assert_eq!(
        bench.compact(Uuid::now_v7()).await,
        CompactOutcome::Accepted
    );
    let compaction = bench
        .until(|event| matches!(event, AdapterEvent::Compacted { .. }))
        .await;
    assert!(matches!(
        compaction.last(),
        Some(AdapterEvent::Compacted { ok: true })
    ));
    assert!(compaction.iter().any(|event| matches!(
        event,
        AdapterEvent::Phase {
            phase: Phase::ContextMaintenance,
            ..
        }
    )));
    bench.until_finished(two.command_id).await;
    let three = bench.input("three");
    bench.accepted(&three).await;
    let events = bench.until_finished(three.command_id).await;
    assert!(text_of(&events, "Error").is_empty());
    let requests = bench.fake.requests();
    let compact = find(&requests, "POST", "/compact").expect("compaction request");
    assert_eq!(compact["body"]["delivery"], "queue");
    assert!(
        compact["body"]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("msg_"))
    );
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn compaction_is_refused_while_a_turn_runs() {
    let mut bench = Bench::new(hanging_turn());
    bench.start_ok().await;
    let input = bench.input("hang");
    bench.accepted(&input).await;
    assert_eq!(
        bench.compact(Uuid::now_v7()).await,
        CompactOutcome::NotApplicable
    );
    assert!(find(&bench.fake.requests(), "POST", "/compact").is_none());
    bench.close(CloseMode::Force).await;
}

#[tokio::test]
async fn server_refusal_of_compaction_is_reported_at_once() {
    let mut script = ready();
    script.push_route(
        "POST",
        &format!("/api/session/{}/compact", session_of(&script)),
        Reply {
            status: 409,
            body: Some(json!({"_tag": "ConflictError"})),
            ..Default::default()
        },
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    assert_eq!(bench.compact(Uuid::now_v7()).await, CompactOutcome::Failed);
    bench.close(CloseMode::Force).await;
}

// Настоящая последовательность `live/invalid_request_stuck.jsonl`: отказ сжатия закрывает период
// как успешный, блока ошибки хода нет, текст провайдера показывает только сжатие.
#[tokio::test]
async fn failed_manual_compaction_is_reported_and_the_session_stays() {
    let mut script = ready();
    let session = session_of(&script);
    let recorded = "msg_0190cafe000070008000000000000099";
    script.steps.push(Step::Expect {
        method: "POST".into(),
        path: format!("/api/session/{session}/compact"),
        reply: Reply {
            body: Some(json!({"data": {"id": recorded, "type": "compaction"}})),
            ..Default::default()
        },
        bind: [(recorded.to_owned(), "/id".to_owned())].into(),
    });
    script.steps.push(Step::Emit {
        events: vec![
            json!({"type": "session.execution.started", "data": {"sessionID": session}}),
            json!({"type": "session.compaction.started", "data": {
                "sessionID": session, "reason": "manual", "inputID": recorded}}),
            json!({"type": "session.compaction.failed", "data": {
                "sessionID": session, "reason": "manual", "inputID": recorded,
                "error": {
                    "type": "provider.invalid-request",
                    "message": "Upstream request failed: Model is unavailable.",
                    "status": 400,
                    "response": {"body": "{\"error\":{\"type\":\"server_error\",\"message\":\"Upstream request failed: Model is unavailable.\"}}"},
                }}}),
            json!({"type": "session.execution.succeeded", "data": {"sessionID": session}}),
        ],
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    assert_eq!(
        bench.compact(Uuid::now_v7()).await,
        CompactOutcome::Accepted
    );
    let mut events = bench
        .until(|event| matches!(event, AdapterEvent::Compacted { .. }))
        .await;
    assert!(matches!(
        events.last(),
        Some(AdapterEvent::Compacted { ok: false })
    ));
    events.extend(bench.until_finished(input.command_id).await);
    assert_eq!(
        text_of(&events, "Error"),
        [
            "Ошибка OpenCode (provider.invalid-request): Upstream request failed: Model is unavailable."
        ]
    );
    bench.close(CloseMode::Graceful).await;
}

// Зависший `opencode` не держит старт воркера: сопровождения Claude и Codex ждут за ним.
#[tokio::test]
async fn hung_server_does_not_hold_the_worker_past_the_deadline() {
    let mut script = ready();
    script.ready.silent = true;
    let fake = Fake::new(script);
    let data = fake.dir.path().join("worker");
    let cfg = OpenCodeConfig {
        executable: fake.exe.clone(),
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let found = discover_within(&cfg, &data, Duration::from_millis(300)).await;
    assert!(matches!(found, Err(DiscoverError::Timeout)));
    assert!(started.elapsed() < Duration::from_secs(5));
    fake.wait_gone().await;
}

// Правила потомкам уходят разом: зависший ответ одного не умножается на их число.
#[tokio::test]
async fn children_get_their_rules_at_once() {
    let capture = two_turns();
    let session = session_of(&capture);
    let mut script = with_rules_route(capture, vec![reply_204()]);
    let children: Vec<String> = (0..3).map(|n| format!("ses_child{n}")).collect();
    let slow = Reply {
        status: 204,
        stall_ms: 400,
        ..Default::default()
    };
    for child in &children {
        script.set_route(
            "PATCH",
            &format!("/api/session/{child}"),
            vec![slow.clone()],
        );
    }
    let created = script
        .steps
        .iter()
        .position(|step| matches!(step, Step::Emit { .. }))
        .unwrap();
    script.steps.insert(
        created,
        Step::Emit {
            events: children
                .iter()
                .map(|child| {
                    json!({"type": "session.created",
                        "data": {"sessionID": child, "parentID": session}})
                })
                .collect(),
        },
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let first = bench.input("раз");
    bench.accepted(&first).await;
    bench.until_finished(first.command_id).await;
    let mut second = bench.input("два");
    second.settings = Some(mode_change("read_only"));
    let started = std::time::Instant::now();
    bench.accepted(&second).await;
    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "{:?}",
        started.elapsed()
    );
    let patched = bench
        .fake
        .requests()
        .iter()
        .filter(|request| request["method"] == "PATCH")
        .count();
    assert_eq!(patched, 4, "the session and every child");
    bench.close(CloseMode::Graceful).await;
}

// Настоящая запись `live/compaction.jsonl`: прерывание даёт отказ `aborted`, затем терминал.
// Это решение владельца, а не ошибка: блока нет, карточка сообщает итог.
#[tokio::test]
async fn interrupted_compaction_is_reported_as_failed() {
    let mut script = ready();
    let session = session_of(&script);
    let recorded = "msg_0190cafe000070008000000000000098";
    script.steps.push(Step::Expect {
        method: "POST".into(),
        path: format!("/api/session/{session}/compact"),
        reply: Reply {
            body: Some(json!({"data": {"id": recorded, "type": "compaction"}})),
            ..Default::default()
        },
        bind: [(recorded.to_owned(), "/id".to_owned())].into(),
    });
    script.steps.push(Step::Emit {
        events: vec![
            json!({"type": "session.execution.started", "data": {"sessionID": session}}),
            json!({"type": "session.compaction.started", "data": {
                "sessionID": session, "reason": "manual", "inputID": recorded}}),
            json!({"type": "session.compaction.failed", "data": {
                "sessionID": session, "reason": "manual", "inputID": recorded,
                "error": {"type": "aborted", "message": "Compaction cancelled"}}}),
            json!({"type": "session.execution.interrupted", "data": {
                "sessionID": session, "reason": "user"}}),
        ],
    });
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let input = bench.input("x");
    bench.accepted(&input).await;
    bench.until_finished(input.command_id).await;
    assert_eq!(
        bench.compact(Uuid::now_v7()).await,
        CompactOutcome::Accepted
    );
    let mut events = bench
        .until(|event| matches!(event, AdapterEvent::Compacted { .. }))
        .await;
    assert!(matches!(
        events.last(),
        Some(AdapterEvent::Compacted { ok: false })
    ));
    events.extend(bench.until_finished(input.command_id).await);
    assert!(text_of(&events, "Error").is_empty());
    bench.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn steer_during_a_turn_posts_steer_delivery_and_is_accepted() {
    // Вливание в идущий период: второй POST на `/prompt` с `delivery: "steer"`,
    // приём — тот же 200 с нашим id. Шаг при этом прерывается (живой прогон),
    // так что подпись в PWA предупреждает об этом.
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    let path = script.steps[position].expected_path().unwrap().to_owned();
    // Ответ-приём со своим плейсхолдером: bind подставит id запроса, иначе
    // ответ унаследовал бы id первого хода из таблицы подстановок.
    script.steps.insert(
        position + 1,
        Step::Expect {
            method: "POST".into(),
            path,
            reply: Reply {
                status: 200,
                body: Some(json!({"data": {"id": "msg_steer_bind"}})),
                ..Default::default()
            },
            bind: [("msg_steer_bind".to_owned(), "/id".to_owned())]
                .into_iter()
                .collect(),
        },
    );
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let first = bench.input("one");
    bench.accepted(&first).await;
    let second = bench.input("send now");
    let outcome = tokio::time::timeout(WAIT, bench.adapter.steer(&second))
        .await
        .expect("steer must finish");
    assert!(
        matches!(
            outcome,
            SteerOutcome::Accepted(ref proof) if proof.command_id == second.command_id
        ),
        "steer proof: {outcome:?}"
    );
    let requests = bench.fake.requests();
    let prompts: Vec<_> = requests
        .iter()
        .filter(|request| path_of(request).ends_with("/prompt"))
        .collect();
    assert_eq!(prompts.len(), 2, "normal prompt plus the steer");
    assert_eq!(prompts[0]["body"]["delivery"], "queue");
    assert_eq!(prompts[1]["body"]["delivery"], "steer");
    assert_eq!(
        prompts[1]["body"]["id"],
        format!("msg_{}", second.command_id.simple())
    );
    bench.close(CloseMode::Graceful).await;
}

/// Неуверенное вливание сверяется отдельно от обычного ввода: провал сверки
/// не трогает чужой ход — ни `delivery.active`, ни его конец.
#[tokio::test]
async fn failed_steer_reconciliation_leaves_the_running_turn_alone() {
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    let path = script.steps[position].expected_path().unwrap().to_owned();
    script.steps.insert(
        position + 1,
        Step::Expect {
            method: "POST".into(),
            path,
            reply: Reply {
                status: 500,
                body: Some(json!({"_tag": "UnknownError"})),
                ..Default::default()
            },
            bind: Default::default(),
        },
    );
    // После вливания проекций нет: сверка ничего не найдёт.
    script.steps.truncate(position + 2);
    let mut bench = Bench::new(script);
    bench.start_ok().await;
    let first = bench.input("one");
    bench.accepted(&first).await;
    let second = bench.input("send now");
    let outcome = tokio::time::timeout(WAIT, bench.adapter.steer(&second))
        .await
        .expect("steer must finish");
    assert!(
        matches!(outcome, SteerOutcome::Undetermined),
        "failed POST reconciles later: {outcome:?}"
    );
    // Гоняем события, пока сверка не уляжется: конца чужого хода быть не должно.
    let deadline = std::time::Instant::now() + Duration::from_secs(12);
    let mut stray_finish = false;
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match tokio::time::timeout(remaining, bench.adapter.next_event()).await {
            Ok(event) => {
                if matches!(
                    &event,
                    AdapterEvent::RunFinished { input_command_id, .. }
                    if *input_command_id == second.command_id
                ) {
                    stray_finish = true;
                    break;
                }
                if matches!(
                    &event,
                    AdapterEvent::RunFinished { input_command_id, .. }
                    if *input_command_id == first.command_id
                ) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    assert!(!stray_finish, "uncertain steer must not finish any turn");
    let requests = bench.fake.requests();
    assert!(
        requests
            .iter()
            .any(|request| path_of(request).ends_with("/inbox")),
        "the steer was reconciled through projections"
    );
    // Исходный ход жив: новый ввод отклоняется занятостью.
    assert!(
        matches!(
            bench.transmit(&bench.input("third")).await,
            TransmitOutcome::Rejected(reason) if reason == "turn_active"
        ),
        "the original turn must still run"
    );
    bench.close(CloseMode::Force).await;
}

/// Два неуверенных вливания в один ход сверяются независимо: ответы потеряны,
/// первое подтверждает очередь проекций, второе — событие очереди сервера.
#[tokio::test]
async fn two_uncertain_steers_are_both_confirmed_and_leave_the_turn_alone() {
    let mut script = ready();
    let position = script
        .steps
        .iter()
        .position(|step| {
            step.expected_path()
                .is_some_and(|path| path.ends_with("/prompt"))
        })
        .unwrap();
    let path = script.steps[position].expected_path().unwrap().to_owned();
    for (offset, placeholder) in ["msg_steer_first", "msg_steer_second"]
        .into_iter()
        .enumerate()
    {
        script.steps.insert(
            position + 1 + offset,
            Step::Expect {
                method: "POST".into(),
                path: path.clone(),
                reply: Reply {
                    status: 500,
                    body: Some(json!({"_tag": "UnknownError"})),
                    ..Default::default()
                },
                bind: [(placeholder.to_owned(), "/id".to_owned())]
                    .into_iter()
                    .collect(),
            },
        );
    }
    script.steps.insert(
        position + 3,
        Step::Emit {
            events: vec![json!({
                "type": "session.inbox.enqueued",
                "data": {"inboxID": "msg_steer_second"},
            })],
        },
    );
    // Исходный ход не завершается: проверяем, что вливания его не трогают.
    script.steps.truncate(position + 4);
    let mut bench = Bench::new(script);
    let started = bench.start_ok().await;
    let first = bench.input("one");
    bench.accepted(&first).await;
    let second = bench.input("send now one");
    let third = bench.input("send now two");
    // Только первое вливание видно в проекции очереди; второе подтверждает событие.
    bench.fake.write_override(
        "GET",
        &format!("/api/session/{}/inbox", started.native_conversation_id),
        &json!({"data": [{"id": format!("msg_{}", second.command_id.simple())}]}),
    );
    for input in [&second, &third] {
        let outcome = tokio::time::timeout(WAIT, bench.adapter.steer(input))
            .await
            .expect("steer must finish");
        assert!(
            matches!(outcome, SteerOutcome::Undetermined),
            "lost response reconciles later: {outcome:?}"
        );
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(12);
    let mut confirmed = Vec::new();
    while confirmed.len() < 2 && std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let Ok(event) = tokio::time::timeout(remaining, bench.adapter.next_event()).await else {
            break;
        };
        if let AdapterEvent::InputAccepted { command_id } = event {
            confirmed.push(command_id);
        }
        assert!(
            !matches!(
                &event,
                AdapterEvent::RunFinished { input_command_id, .. }
                    if *input_command_id == second.command_id
                        || *input_command_id == third.command_id
            ),
            "an uncertain steer must not finish a turn"
        );
    }
    confirmed.sort();
    let mut expected = vec![second.command_id, third.command_id];
    expected.sort();
    assert_eq!(confirmed, expected, "both steers were reconciled");
    // Исходный ход жив: новый ввод отклоняется занятостью.
    assert!(
        matches!(
            bench.transmit(&bench.input("fourth")).await,
            TransmitOutcome::Rejected(reason) if reason == "turn_active"
        ),
        "the original turn must still run"
    );
    bench.close(CloseMode::Force).await;
}

#[tokio::test]
async fn steer_without_a_turn_sends_nothing() {
    let mut bench = Bench::new(ready());
    bench.start_ok().await;
    let input = bench.input("nowhere");
    let outcome = tokio::time::timeout(WAIT, bench.adapter.steer(&input))
        .await
        .expect("steer must finish");
    assert!(
        matches!(&outcome, SteerOutcome::NotApplicable(reason) if reason == "no_turn"),
        "{outcome:?}"
    );
    let requests = bench.fake.requests();
    assert!(
        !requests
            .iter()
            .any(|request| path_of(request).ends_with("/prompt")),
        "no prompt without a running turn"
    );
    bench.close(CloseMode::Graceful).await;
}
