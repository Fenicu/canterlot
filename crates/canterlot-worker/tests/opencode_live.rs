#![forbid(unsafe_code)]
#![cfg(feature = "opencode-live")]

//! L2: настоящий `opencode` с изолированными каталогами. Заглушка провайдера живёт в
//! этом процессе на loopback, внешних запросов к моделям нет. Тесты не входят в
//! check.sh, запуск:
//!
//! ```text
//! cargo test -p canterlot-worker --features opencode-live --test opencode_live -- --ignored --test-threads=1
//! ```

use canterlot_proto::requests::{AnswerValue, DecisionPayload, PermissionBehavior};
use canterlot_worker::{
    adapter::{
        Adapter, AdapterEvent, AgentCallResult, BlockKind, CloseMode, CompactOutcome,
        DecisionOutcome, InstructionMode, InstructionSpec, InterruptOutcome, LimitFact,
        LimitWindow, LocalFile, MediaSource, ModelUsage, Phase, SettingsChange, StartSpec,
        TodoItem, TodoStatus, Transmission, TransmitOutcome,
        opencode::{OpenCodeAdapter, VERIFIED_OPENAPI_SHA256, discover},
    },
    config::OpenCodeConfig,
    containment::TreeContainment,
};
use rustix::process::{Pid, Signal, kill_process};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use uuid::Uuid;

#[path = "support/fake_exec.rs"]
mod fake_exec;

#[path = "support/opencode_stub.rs"]
mod stub;
use stub::{Reply, Stub};

const WAIT: Duration = Duration::from_secs(60);

/// Всё, что видит настоящий сервер, лежит в `root`: `HOME` и `XDG_CONFIG_HOME` подменяет
/// обёртка, остальные каталоги задаёт адаптер.
struct Live {
    root: TempDir,
    exec_dir: PathBuf,
    workdir: PathBuf,
    wrapper: PathBuf,
}

fn real_binary() -> String {
    std::env::var("CANTERLOT_OPENCODE_BIN").unwrap_or_else(|_| "/usr/bin/opencode".into())
}

impl Live {
    fn new(config: &str) -> Self {
        let root = TempDir::new().unwrap();
        let home = root.path().join("home");
        let config_home = root.path().join("config");
        let workdir = root.path().join("work");
        let exec_dir = root.path().join("data/executions/one");
        for dir in [&home, &config_home.join("opencode"), &workdir, &exec_dir] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(config_home.join("opencode/opencode.json"), config).unwrap();
        let wrapper = fake_exec::write_executable(
            &root.path().join("opencode"),
            format!(
                "#!/bin/sh\nexport HOME='{}' OPENCODE_DISABLE_MODELS_FETCH=1\n\
                 case \"$XDG_CONFIG_HOME\" in '{}'/*) ;; *) export XDG_CONFIG_HOME='{}' ;; esac\n\
                 exec '{}' \"$@\"\n",
                home.display(),
                root.path().display(),
                config_home.display(),
                real_binary(),
            ),
            0o700,
        );
        Self {
            root,
            exec_dir,
            workdir,
            wrapper,
        }
    }

    fn adapter(&self) -> OpenCodeAdapter {
        OpenCodeAdapter::new(
            OpenCodeConfig {
                executable: self.wrapper.clone(),
                owner_config: Some(self.root.path().join("config/opencode")),
                ..Default::default()
            },
            self.exec_dir.clone(),
            Duration::from_secs(20),
        )
        .with_relay_executable(PathBuf::from(env!("CARGO_BIN_EXE_canterlot-worker")))
    }

    fn spec(&self, model: &str, mode: &str) -> StartSpec {
        StartSpec {
            execution_id: Uuid::now_v7(),
            generation: 1,
            workdir: self.workdir.clone(),
            native_conversation_id: None,
            profile: "opencode".into(),
            model: model.into(),
            effort: String::new(),
            permission_mode: mode.into(),
            instructions: InstructionSpec {
                mode: InstructionMode::Owner,
                text: String::new(),
                preset_id: None,
                revision: 0,
                title: String::new(),
            },
            isolate_mcp: false,
        }
    }

    fn database(&self) -> PathBuf {
        self.root
            .path()
            .join("data/opencode/data/opencode/opencode.db")
    }

    // Только процессы с рабочим каталогом этого теста: чужие серверы OpenCode не трогаем.
    fn servers(&self) -> Vec<u32> {
        fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter_map(|entry| {
                let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
                let cwd = fs::read_link(entry.path().join("cwd")).ok()?;
                let exe = fs::read_link(entry.path().join("exe")).ok()?;
                (cwd.starts_with(self.root.path()) && exe.to_string_lossy().contains("opencode"))
                    .then_some(pid)
            })
            .collect()
    }

    // Только процессы этого теста: `bash -c` запускается в рабочем каталоге.
    fn running(&self, needle: &str) -> bool {
        fs::read_dir("/proc").unwrap().flatten().any(|entry| {
            let matches_cwd = fs::read_link(entry.path().join("cwd"))
                .is_ok_and(|cwd| cwd.starts_with(self.root.path()));
            let cmdline = fs::read(entry.path().join("cmdline")).unwrap_or_default();
            matches_cwd
                && String::from_utf8_lossy(&cmdline)
                    .replace('\0', " ")
                    .contains(needle)
        })
    }

    async fn wait_gone(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.servers().is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("opencode survived: {:?}", self.servers());
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        for pid in self.servers() {
            if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
                let _ = kill_process(pid, Signal::KILL);
            }
        }
    }
}

fn is_under(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_server_passes_the_start_path() {
    let live = Live::new("{}");
    let group = TreeContainment::new().unwrap();
    let mut adapter = live.adapter();
    let spec = live.spec("", "bypass");
    let started = tokio::time::timeout(WAIT, adapter.start(&spec, &group))
        .await
        .expect("start must finish")
        .expect("start");
    assert!(started.native_conversation_id.starts_with("ses_"));
    assert!(live.database().exists(), "own database");
    assert!(is_under(&live.database(), live.root.path()));
    assert_eq!(live.servers(), vec![started.cli_pid]);
    let quiet = tokio::time::timeout(Duration::from_millis(300), adapter.next_event()).await;
    assert!(
        !matches!(
            quiet,
            Ok(AdapterEvent::Diagnostic {
                kind: "mcp_unavailable",
                ..
            })
        ),
        "relay must connect"
    );
    let closing = Instant::now();
    adapter.close(CloseMode::Graceful, &group).await.unwrap();
    assert!(closing.elapsed() < Duration::from_secs(5));
    live.wait_gone().await;
}

fn input(text: &str) -> Transmission {
    Transmission {
        command_id: Uuid::now_v7(),
        attempt: 1,
        text: text.into(),
        files: vec![],
        settings: None,
    }
}

struct Session {
    live: Live,
    group: TreeContainment,
    adapter: OpenCodeAdapter,
}

impl Session {
    async fn start(stub: &Stub, mode: &str) -> Self {
        let live = Live::new(&stub.config());
        let group = TreeContainment::new().unwrap();
        let mut adapter = live.adapter();
        let spec = live.spec("fake/fake-1", mode);
        tokio::time::timeout(WAIT, adapter.start(&spec, &group))
            .await
            .expect("start must finish")
            .expect("start");
        Self {
            live,
            group,
            adapter,
        }
    }

    async fn start_with(stub: &Stub, mode: &str, prepare: impl FnOnce(&Live)) -> Self {
        let live = Live::new(&stub.config());
        prepare(&live);
        let group = TreeContainment::new().unwrap();
        let mut adapter = live.adapter();
        let spec = live.spec("fake/fake-1", mode);
        tokio::time::timeout(WAIT, adapter.start(&spec, &group))
            .await
            .expect("start must finish")
            .expect("start");
        Self {
            live,
            group,
            adapter,
        }
    }

    async fn turn(&mut self, input: &Transmission) -> Vec<AdapterEvent> {
        match tokio::time::timeout(WAIT, self.adapter.transmit(input, &self.group))
            .await
            .expect("transmit must finish")
        {
            TransmitOutcome::Accepted(proof) => {
                assert_eq!(proof.kind, "opencode_inbox_admitted");
            }
            other => panic!("{other:?}"),
        }
        let mut events = Vec::new();
        loop {
            let event = tokio::time::timeout(WAIT, self.adapter.next_event())
                .await
                .expect("adapter event");
            let done = matches!(
                &event,
                AdapterEvent::RunFinished { input_command_id, .. }
                    if *input_command_id == input.command_id
            );
            events.push(event);
            if done {
                return events;
            }
        }
    }

    async fn close(mut self) {
        self.adapter
            .close(CloseMode::Graceful, &self.group)
            .await
            .unwrap();
        self.live.wait_gone().await;
    }
}

fn finals(events: &[AdapterEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockFinal { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_turn_gives_blocks_usage_and_one_provider_request() {
    let stub = Stub::start(vec![Reply::Text {
        reasoning: "думаю".into(),
        text: "Привет, мир".into(),
    }])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let turn = input("Скажи привет");
    let events = session.turn(&turn).await;
    assert_eq!(finals(&events), ["думаю", "Привет, мир"]);
    let AdapterEvent::RunFinished {
        usage,
        context_tokens,
        context_window_tokens,
        ..
    } = events.last().unwrap()
    else {
        unreachable!();
    };
    assert_eq!(
        usage,
        &vec![ModelUsage {
            model: "fake/fake-1".into(),
            agent: "orchestrator".into(),
            input_tokens: 70,
            output_tokens: 20,
            cache_read_tokens: 30,
            cache_write_tokens: 0,
        }]
    );
    assert_eq!(*context_tokens, 100);
    assert_eq!(*context_window_tokens, 200_000);
    // Заголовок сессии задан: лишнего запроса к модели нет.
    let requests = stub.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(requests[0].to_string().contains("Скажи привет"));
    session.close().await;
}

// Повтор уже доставленного ввода находится в истории, новый ход не начинается.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn delivered_input_is_found_in_the_history() {
    let stub = Stub::start(vec![Reply::text("один")]).await;
    let mut session = Session::start(&stub, "bypass").await;
    let mut turn = input("первый");
    session.turn(&turn).await;
    let mut last = tokio::time::timeout(WAIT, session.adapter.next_event()).await;
    while !matches!(
        last,
        Ok(AdapterEvent::Phase {
            phase: canterlot_worker::adapter::Phase::AwaitingInput,
            ..
        })
    ) {
        last = tokio::time::timeout(WAIT, session.adapter.next_event()).await;
    }
    turn.attempt = 2;
    match session.adapter.transmit(&turn, &session.group).await {
        TransmitOutcome::Accepted(proof) => assert_eq!(proof.kind, "opencode_history"),
        other => panic!("{other:?}"),
    }
    assert_eq!(stub.requests().len(), 1);
    let unknown = input("такого ввода не было");
    let mut retried = input("третий");
    retried.command_id = unknown.command_id;
    retried.attempt = 2;
    let _ = session.adapter.next_event().await;
    let _ = session.adapter.next_event().await;
    assert!(matches!(
        session.adapter.transmit(&retried, &session.group).await,
        TransmitOutcome::Accepted(proof) if proof.kind == "opencode_inbox_admitted"
    ));
    session.close().await;
}

fn summaries(events: &[AdapterEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockFinal { summary, .. } => summary.clone(),
            _ => None,
        })
        .collect()
}

fn marks(events: &[AdapterEvent]) -> Vec<Option<String>> {
    events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockStarted {
                kind: BlockKind::ToolCall,
                approved_by,
                ..
            } => Some(approved_by.clone()),
            _ => None,
        })
        .collect()
}

// Заодно проверка `snapshots: false`: запись файла работает без хранилища снимков.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_tools_report_exit_status_and_write_with_snapshots_off() {
    let stub = Stub::start(vec![
        Reply::tool("shell", json!({"command": "echo привет; exit 3"})),
        Reply::tool(
            "write",
            json!({"path": "заметка.txt", "content": "текст\n"}),
        ),
        Reply::text("готово"),
    ])
    .await;
    let mut session = Session::start(&stub, "owner").await;
    let turn = input("выполни");
    let events = session.turn(&turn).await;
    let summaries = summaries(&events);
    assert!(
        summaries
            .iter()
            .any(|summary| summary.ends_with(", exit 3")),
        "{summaries:?}"
    );
    assert_eq!(
        fs::read_to_string(session.live.workdir.join("заметка.txt")).unwrap(),
        "текст\n"
    );
    assert_eq!(
        marks(&events),
        [Some("cli_policy".to_owned()), Some("cli_policy".to_owned())]
    );
    assert!(finals(&events).iter().any(|text| text.contains("привет")));
    assert!(finals(&events).iter().any(|text| text == "готово"));
    session.close().await;
}

impl Session {
    async fn transmit(&mut self, input: &Transmission) {
        match tokio::time::timeout(WAIT, self.adapter.transmit(input, &self.group))
            .await
            .expect("transmit must finish")
        {
            TransmitOutcome::Accepted(_) => {}
            other => panic!("{other:?}"),
        }
    }

    async fn until(&mut self, mut done: impl FnMut(&AdapterEvent) -> bool) -> Vec<AdapterEvent> {
        let mut events = Vec::new();
        loop {
            let event = tokio::time::timeout(WAIT, self.adapter.next_event())
                .await
                .expect("adapter event");
            let stop = done(&event);
            events.push(event);
            if stop {
                return events;
            }
        }
    }

    async fn until_request(&mut self) -> (String, Value) {
        let events = self
            .until(|event| matches!(event, AdapterEvent::RequestOpened { .. }))
            .await;
        match events.last() {
            Some(AdapterEvent::RequestOpened {
                request_id,
                payload,
                ..
            }) => (request_id.clone(), payload.clone()),
            other => panic!("{other:?}"),
        }
    }
}

fn permission(behavior: PermissionBehavior, message: Option<&str>) -> DecisionPayload {
    DecisionPayload::Permission {
        behavior,
        message: message.map(str::to_owned),
    }
}

// Режим `ask`: запрос приходит настоящий, отказ с сообщением не кончает ход.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_permission_prompt_declined_with_a_message_and_the_turn_goes_on() {
    let stub = Stub::start(vec![
        Reply::tool("write", json!({"path": "нельзя.txt", "content": "x"})),
        Reply::text("понял"),
    ])
    .await;
    let mut session = Session::start(&stub, "ask").await;
    let turn = input("запиши");
    session.transmit(&turn).await;
    let (request, payload) = session.until_request().await;
    assert_eq!(payload["tool_name"], "edit");
    let outcome = session
        .adapter
        .deliver_decision(&request, &permission(PermissionBehavior::Deny, None))
        .await;
    assert!(
        matches!(outcome, DecisionOutcome::Applied(_)),
        "{outcome:?}"
    );
    let command = turn.command_id;
    let events = session
        .until(|event| matches!(event, AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == command))
        .await;
    assert!(
        summaries(&events)
            .iter()
            .any(|summary| summary.ends_with(", declined")),
        "{:?}",
        summaries(&events)
    );
    assert!(!session.live.workdir.join("нельзя.txt").exists());
    assert_eq!(
        stub.requests().len(),
        2,
        "the model saw the refusal and went on"
    );
    session.close().await;
}

// Разрешение на один раз: инструмент отработал, повторное решение получает 404 сервера,
// а правило в базу не записано.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_permission_allowed_once_runs_the_tool_and_saves_no_rule() {
    let stub = Stub::start(vec![
        Reply::tool(
            "shell",
            json!({"command": "echo привет", "description": "x"}),
        ),
        Reply::text("готово"),
    ])
    .await;
    let mut session = Session::start(&stub, "ask").await;
    let turn = input("запусти");
    session.transmit(&turn).await;
    let mut events = session
        .until(|event| matches!(event, AdapterEvent::RequestOpened { .. }))
        .await;
    let Some(AdapterEvent::RequestOpened {
        request_id: request,
        payload,
        ..
    }) = events.last().cloned()
    else {
        unreachable!();
    };
    assert_eq!(payload["command"], "echo привет");
    let outcome = session
        .adapter
        .deliver_decision(&request, &permission(PermissionBehavior::Allow, None))
        .await;
    assert!(
        matches!(outcome, DecisionOutcome::Applied(_)),
        "{outcome:?}"
    );
    let again = session
        .adapter
        .deliver_decision(&request, &permission(PermissionBehavior::Allow, None))
        .await;
    assert!(matches!(again, DecisionOutcome::NotApplied(_)), "{again:?}");
    let command = turn.command_id;
    events.extend(
        session
            .until(|event| matches!(event, AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == command))
            .await,
    );
    assert_eq!(marks(&events), [None]);
    assert!(finals(&events).iter().any(|text| text.contains("привет")));
    session.close().await;
}

// Запрещённый инструмент режима `read_only` модели просто не предлагается.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn read_only_rules_reach_the_provider_request() {
    let stub = Stub::start(vec![Reply::text("ок")]).await;
    let mut session = Session::start(&stub, "read_only").await;
    session.turn(&input("читай")).await;
    let request = &stub.requests()[0];
    let tools: Vec<_> = request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect();
    assert!(tools.contains(&"read"), "{tools:?}");
    assert!(!tools.contains(&"shell"), "{tools:?}");
    assert!(!tools.contains(&"write"), "{tools:?}");
    session.close().await;
}

// Настоящее прерывание: команда на переднем плане снята, фоновая осталась и показана.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_interrupt_leaves_the_background_shell_and_shows_it() {
    let stub = Stub::start(vec![
        Reply::Tools(vec![
            (
                "shell".into(),
                json!({"command": "sleep 41 # фон", "description": "bg", "background": true}),
            ),
            (
                "shell".into(),
                json!({"command": "sleep 42 # перед", "description": "fg"}),
            ),
        ]),
        Reply::text("не должно прийти"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let turn = input("запусти");
    session.transmit(&turn).await;
    session
        .until(|event| {
            matches!(event, AdapterEvent::Phase { summary: Some(summary), .. }
                if summary.contains("sleep 42"))
        })
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let outcome = session.adapter.interrupt().await;
    assert!(
        matches!(outcome, InterruptOutcome::Acknowledged),
        "{outcome:?}"
    );
    let command = turn.command_id;
    let events = session
        .until(|event| matches!(event, AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == command))
        .await;
    assert!(
        summaries(&events)
            .iter()
            .any(|s| s.ends_with(", interrupted")),
        "{:?}",
        summaries(&events)
    );
    let events = session
        .until(|event| {
            matches!(event, AdapterEvent::BlockFinal { text, .. }
                if text.starts_with("Фоновые команды"))
        })
        .await;
    let notice = finals(&events).pop().unwrap();
    assert!(notice.contains("sleep 41 # фон"), "{notice}");
    assert!(!notice.contains("sleep 42"), "{notice}");
    assert_eq!(stub.requests().len(), 1, "the model was not woken");
    session.close().await;
}

// Фоновая команда закончилась: модель проснулась без ввода, запуск приписан последней команде.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_background_shell_wakes_a_run_attributed_to_the_last_command() {
    let stub = Stub::start(vec![
        Reply::tool(
            "shell",
            json!({"command": "sleep 1 && echo готово", "description": "bg", "background": true}),
        ),
        Reply::text("жду"),
        Reply::text("команда закончилась"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let turn = input("запусти");
    session.turn(&turn).await;
    let command = turn.command_id;
    let events = session
        .until(|event| matches!(event, AdapterEvent::RunFinished { .. }))
        .await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::RunAdopted { input_command_id } if *input_command_id == command
    )));
    let Some(AdapterEvent::RunFinished {
        input_command_id,
        usage,
        ..
    }) = events.last()
    else {
        unreachable!();
    };
    assert_eq!(*input_command_id, command);
    assert_eq!(usage.len(), 1);
    assert!(
        finals(&events)
            .iter()
            .any(|text| text == "команда закончилась")
    );
    assert_eq!(stub.requests().len(), 3);
    session.close().await;
}

// Каталог настоящего сервера: Go в изоляции нет, включённый провайдер виден с вариантами.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_catalog_lists_the_enabled_provider() {
    let stub = Stub::start(Vec::new()).await;
    let live = Live::new(&stub.config());
    let data = live.root.path().join("data");
    let cfg = OpenCodeConfig {
        executable: live.wrapper.clone(),
        providers: vec!["fake".into()],
        ..Default::default()
    };
    let catalog = tokio::time::timeout(WAIT, discover(&cfg, &data))
        .await
        .expect("discover must finish")
        .map_err(|_| "discover failed")
        .unwrap();
    assert_eq!(catalog.models[0].id, "fake/fake-1");
    assert_eq!(catalog.models[0].efforts, ["low", "medium", "high"]);
    assert!(
        catalog
            .models
            .iter()
            .all(|model| model.id.starts_with("fake/"))
    );
    assert_eq!(stub.requests().len(), 0, "no model call");
    live.wait_gone().await;
}

// Смена усилия посреди сессии видна в запросе к провайдеру.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_effort_change_reaches_the_provider_request() {
    let stub = Stub::start(vec![Reply::text("раз"), Reply::text("два")]).await;
    let mut session = Session::start(&stub, "bypass").await;
    session.turn(&input("первый")).await;
    let mut second = input("второй");
    second.settings = Some(SettingsChange {
        effort: Some("high".into()),
        ..Default::default()
    });
    assert_eq!(session.adapter.validate_transmission(&second), Ok(()));
    session.turn(&second).await;
    let requests = stub.requests();
    assert_ne!(requests[0]["reasoning_effort"], "high");
    assert_eq!(requests[1]["reasoning_effort"], "high", "{}", requests[1]);
    session.close().await;
}

fn question_call() -> Reply {
    Reply::tool(
        "question",
        json!({"questions": [
            {"question": "Какой цвет?", "header": "Цвет",
             "options": [{"label": "Синий", "description": "холодный"}]},
            {"question": "Что ещё?", "header": "Ещё", "multiple": true,
             "options": [{"label": "А", "description": "а"}, {"label": "Б", "description": "б"}]},
        ]}),
    )
}

// Настоящая форма: ответ доходит до модели целиком, без "Unanswered".
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_question_is_answered_through_the_card() {
    let stub = Stub::start(vec![question_call(), Reply::text("понял")]).await;
    let mut session = Session::start(&stub, "ask").await;
    let turn = input("спроси");
    session.transmit(&turn).await;
    let (request, payload) = session.until_request().await;
    assert_eq!(payload["questions"][0]["text"], "Какой цвет?");
    let decision = DecisionPayload::Question {
        answers: [
            ("q0".to_owned(), AnswerValue::Single("Свой ответ".into())),
            (
                "q1".to_owned(),
                AnswerValue::Multiple(vec!["А".into(), "Б".into()]),
            ),
        ]
        .into_iter()
        .collect(),
    };
    let outcome = session.adapter.deliver_decision(&request, &decision).await;
    assert!(
        matches!(outcome, DecisionOutcome::Applied(_)),
        "{outcome:?}"
    );
    let command = turn.command_id;
    session
        .until(|event| matches!(event, AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == command))
        .await;
    let seen = stub.requests()[1].to_string();
    assert!(seen.contains("Свой ответ"), "{seen}");
    assert!(!seen.contains("Unanswered"), "{seen}");
    session.close().await;
}

// Отказ с текстом: модель получает его и идёт дальше, ход не обрывается.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_question_refused_with_a_message_lets_the_model_go_on() {
    let stub = Stub::start(vec![question_call(), Reply::text("понял")]).await;
    let mut session = Session::start(&stub, "ask").await;
    let turn = input("спроси");
    session.transmit(&turn).await;
    let (request, _) = session.until_request().await;
    session
        .adapter
        .refuse_request(&request, "Owner is away, continue without it")
        .await;
    let command = turn.command_id;
    session
        .until(|event| matches!(event, AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == command))
        .await;
    assert_eq!(stub.requests().len(), 2, "the model went on");
    assert!(stub.requests()[1].to_string().contains("Owner is away"));
    session.close().await;
}

// Смоук, пункт 41: у OpenCode лимита вопросов нет, форма шире карточки снимается с сообщением,
// и модель идёт дальше.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_form_with_five_fields_is_refused_and_the_model_goes_on() {
    let question = |index: u32| {
        json!({"question": format!("Вопрос {index}?"), "header": format!("В{index}"),
               "options": [{"label": "Да", "description": "да"}]})
    };
    let questions: Vec<Value> = (0..5).map(question).collect();
    let stub = Stub::start(vec![
        Reply::tool("question", json!({"questions": questions})),
        Reply::text("понял"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let events = session.turn(&input("спроси пять раз")).await;
    assert_eq!(stub.requests().len(), 2, "the model went on");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::RequestOpened { .. })),
        "{events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "opencode_form_refused",
                ..
            }
        )),
        "{events:?}"
    );
    session.close().await;
}

/// Сервер MCP, который на вызов инструмента спрашивает клиента и отвечает его решением.
const ELICITING_MCP: &str = r#"
import json, sys
def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()
call = None
for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {
            "protocolVersion": message["params"]["protocolVersion"],
            "capabilities": {"tools": {}}, "serverInfo": {"name": "ask", "version": "1"}}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"tools": [{
            "name": "question", "description": "asks the client",
            "inputSchema": {"type": "object", "properties": {}}}]}})
    elif method == "tools/call":
        call = message["id"]
        send({"jsonrpc": "2.0", "id": 9001, "method": "elicitation/create", "params": {
            "message": "Which one?", "requestedSchema": {
                "type": "object", "properties": {"choice": {"type": "string"}}}}})
    elif message.get("id") == 9001:
        action = message.get("result", {}).get("action", "error")
        send({"jsonrpc": "2.0", "id": call, "result": {
            "content": [{"type": "text", "text": "elicitation " + action}]}})
"#;

// Смоук, пункт 40: настоящая форма MCP живёт на сессии `global`. Отказ доходит до сервера MCP,
// вызов не висит до прерывания.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_mcp_elicitation_is_refused_and_the_call_returns() {
    let stub = Stub::start(vec![
        Reply::tool("ask_question", json!({})),
        Reply::text("понял"),
    ])
    .await;
    let mut config: Value = serde_json::from_str(&stub.config()).unwrap();
    let mut session = Session::start_with(&stub, "bypass", |live| {
        let script = live.root.path().join("ask.py");
        fs::write(&script, ELICITING_MCP).unwrap();
        config["mcp"] = json!({"servers": {"ask": {
            "type": "local", "codemode": false,
            "command": ["python3", script.to_string_lossy()],
        }}});
        fs::write(
            live.root.path().join("config/opencode/opencode.json"),
            config.to_string(),
        )
        .unwrap();
    })
    .await;
    let events = session.turn(&input("спроси")).await;
    assert_eq!(stub.requests().len(), 2, "{events:?}");
    let seen = stub.requests()[1].to_string();
    assert!(seen.contains("elicitation cancel"), "{seen}");
    session.close().await;
}

fn offered(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

// Смена режима на ходу: инструменты следующего запроса модели уже по новым правилам, а `owner`
// снимает их совсем.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_mode_change_applies_from_the_next_input() {
    let stub = Stub::start(vec![
        Reply::text("раз"),
        Reply::text("два"),
        Reply::text("три"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    session.turn(&input("первый")).await;
    assert!(offered(&stub.requests()[0]).contains(&"shell".to_owned()));
    let mut second = input("второй");
    second.settings = Some(SettingsChange {
        permission_mode: Some("read_only".into()),
        ..Default::default()
    });
    session.turn(&second).await;
    let tools = offered(&stub.requests()[1]);
    assert!(!tools.contains(&"shell".to_owned()), "{tools:?}");
    assert!(tools.contains(&"read".to_owned()), "{tools:?}");
    let mut third = input("третий");
    third.settings = Some(SettingsChange {
        permission_mode: Some("owner".into()),
        ..Default::default()
    });
    session.turn(&third).await;
    let tools = offered(&stub.requests()[2]);
    for tool in ["shell", "question", "execute"] {
        assert!(tools.contains(&tool.to_owned()), "{tools:?}");
    }
    session.close().await;
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

impl Session {
    async fn with_config(config: &str) -> Self {
        let live = Live::new(config);
        let group = TreeContainment::new().unwrap();
        let mut adapter = live.adapter();
        let spec = live.spec("fake/fake-1", "bypass");
        tokio::time::timeout(WAIT, adapter.start(&spec, &group))
            .await
            .expect("start must finish")
            .expect("start");
        Self {
            live,
            group,
            adapter,
        }
    }

    async fn start_as(
        stub: &Stub,
        instructions: InstructionSpec,
        prepare: impl FnOnce(&Live),
    ) -> Self {
        let live = Live::new(&stub.config());
        prepare(&live);
        let group = TreeContainment::new().unwrap();
        let mut adapter = live.adapter();
        let mut spec = live.spec("fake/fake-1", "bypass");
        spec.instructions = instructions;
        tokio::time::timeout(WAIT, adapter.start(&spec, &group))
            .await
            .expect("start must finish")
            .expect("start");
        Self {
            live,
            group,
            adapter,
        }
    }
}

fn clean() -> InstructionSpec {
    InstructionSpec {
        mode: InstructionMode::Clean,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

// «Без инструкций»: настройки из обоих файлов и плагины владельца на месте, его `AGENTS.md` нет.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_clean_mode_keeps_the_owner_config_and_drops_his_instructions() {
    let stub = Stub::start(vec![Reply::text("ок")]).await;
    let mut session = Session::start_as(&stub, clean(), |live| {
        let owner = live.root.path().join("config/opencode");
        fs::write(owner.join("AGENTS.md"), "CANARY_OWNER").unwrap();
        let extra = live.root.path().join("extra.md");
        fs::write(&extra, "CANARY_JSONC").unwrap();
        fs::write(
            owner.join("opencode.jsonc"),
            format!(
                "// второй файл\n{{\"instructions\": [\"{}\"]}}",
                extra.display()
            ),
        )
        .unwrap();
        fs::create_dir(owner.join("plugins")).unwrap();
        fs::write(
            owner.join("plugins/canary.ts"),
            "import fs from \"fs\"\nfs.writeFileSync(process.env.TMPDIR + \"/plugin_ran\", \"1\")\nexport default async () => ({})\n",
        )
        .unwrap();
    })
    .await;
    session.turn(&input("привет")).await;
    let seen = stub.requests()[0].to_string();
    assert!(!seen.contains("CANARY_OWNER"), "{seen}");
    assert!(
        session.live.exec_dir.join("tmp/plugin_ran").exists(),
        "owner plugin"
    );
    assert!(seen.contains("CANARY_JSONC"), "{seen}");
    session.close().await;
}

// Пресет на ходу: текст доходит до следующего запроса без перезапуска, фоновая команда живёт,
// а возврат к «Без инструкций» отменяет его для модели.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_preset_change_reaches_the_next_provider_request_without_a_restart() {
    let stub = Stub::start(vec![
        Reply::tool(
            "shell",
            json!({"command": "sleep 4711", "description": "bg", "background": true}),
        ),
        Reply::text("раз"),
        Reply::text("два"),
        Reply::text("три"),
        Reply::text("четыре"),
    ])
    .await;
    let mut session = Session::start_as(&stub, clean(), |_| {}).await;
    session.turn(&input("первый")).await;
    let pids = session.live.servers();
    let mut second = input("второй");
    second.settings = Some(SettingsChange {
        instructions: Some(preset("CANARY_PRESET_ONE", 1)),
        ..Default::default()
    });
    session.turn(&second).await;
    assert!(
        stub.requests()[2].to_string().contains("CANARY_PRESET_ONE"),
        "{}",
        stub.requests()[2]
    );
    let mut third = input("третий");
    third.settings = Some(SettingsChange {
        instructions: Some(preset("CANARY_PRESET_TWO", 2)),
        ..Default::default()
    });
    session.turn(&third).await;
    assert!(stub.requests()[3].to_string().contains("CANARY_PRESET_TWO"));
    let mut fourth = input("четвёртый");
    fourth.settings = Some(SettingsChange {
        instructions: Some(clean()),
        ..Default::default()
    });
    session.turn(&fourth).await;
    assert!(
        stub.requests()[4]
            .to_string()
            .contains("Previously loaded instructions no longer apply"),
        "{}",
        stub.requests()[4]
    );
    assert_eq!(session.live.servers(), pids, "no restart");
    assert!(session.live.running("sleep 4711"), "background shell lives");
    session.close().await;
}

// Переход с «Как у владельца»: другой процесс, та же сессия, история на месте.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_switch_from_owner_restarts_with_the_same_session() {
    let stub = Stub::start(vec![Reply::text("раз"), Reply::text("два")]).await;
    let mut session = Session::start(&stub, "bypass").await;
    session.turn(&input("первый")).await;
    let before = session.live.servers();
    let mut second = input("второй");
    second.settings = Some(SettingsChange {
        instructions: Some(preset("CANARY_RESTART", 1)),
        ..Default::default()
    });
    session.turn(&second).await;
    assert_ne!(session.live.servers(), before);
    let seen = stub.requests()[1].to_string();
    assert!(seen.contains("CANARY_RESTART"), "{seen}");
    assert!(seen.contains("первый"), "history survives: {seen}");
    session.close().await;
}

const CANTERLOT_TOOLS: [&str; 4] = [
    "canterlot_canterlot_limits",
    "canterlot_canterlot_presets_list",
    "canterlot_canterlot_preset_save",
    "canterlot_canterlot_preset_apply",
];

// Инструменты MCP видны в первом запросе после холодного старта. Спека требует 20 повторов:
// `connected` публикуется раньше регистрации инструментов, ожидание остаётся эвристикой.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn canterlot_tools_are_in_the_first_provider_request_across_cold_starts() {
    for attempt in 0..20 {
        let stub = Stub::start(vec![Reply::text("ок")]).await;
        let mut session = Session::start(&stub, "bypass").await;
        session.turn(&input("лимиты")).await;
        let tools = offered(&stub.requests()[0]);
        for name in CANTERLOT_TOOLS {
            assert!(
                tools.iter().any(|tool| tool == name),
                "cold start {attempt}: {name} missing in {tools:?}"
            );
        }
        session.close().await;
    }
}

// Список пресетов и лимиты: вызов модели проходит настоящий клиент MCP, реле и брокер, а ответ
// доходит до следующего запроса. Лимиты отвечает адаптер, поэтому событий о них нет.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_preset_and_limit_tools_answer_the_model() {
    let stub = Stub::start(vec![
        Reply::Tools(vec![
            ("canterlot_canterlot_limits".into(), json!({})),
            ("canterlot_canterlot_presets_list".into(), json!({})),
        ]),
        Reply::text("готово"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let turn = input("покажи пресеты");
    session.transmit(&turn).await;
    let events = session
        .until(|event| matches!(event, AdapterEvent::AgentCall { .. }))
        .await;
    let Some(AdapterEvent::AgentCall { call_id, tool, .. }) = events.last().cloned() else {
        unreachable!();
    };
    assert_eq!(tool, "presets.list");
    session
        .adapter
        .agent_call_result(
            call_id,
            AgentCallResult::Ok(json!({"presets": ["CANARY_PRESET"]})),
        )
        .await;
    let command = turn.command_id;
    session
        .until(|event| matches!(event, AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == command))
        .await;
    let seen = stub.requests()[1].to_string();
    assert!(seen.contains("CANARY_PRESET"), "{seen}");
    assert!(seen.contains("no limit data yet"), "{seen}");
    session.close().await;
}

// В `ask` список и лимиты идут без карточки, а применение пресета спрашивает: действует
// разрешение самого CLI (`pilot.md` §4). В `read_only` модели предлагается только чтение.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_modes_treat_the_preset_tools_by_their_effect() {
    let stub = Stub::start(vec![
        Reply::tool("canterlot_canterlot_presets_list", json!({})),
        Reply::tool("canterlot_canterlot_preset_apply", json!({"id": "clean"})),
        Reply::text("готово"),
    ])
    .await;
    let mut session = Session::start(&stub, "ask").await;
    let turn = input("примени пресет");
    session.transmit(&turn).await;
    let events = session
        .until(|event| matches!(event, AdapterEvent::AgentCall { .. }))
        .await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::RequestOpened { .. })),
        "the list asks nothing"
    );
    let Some(AdapterEvent::AgentCall { call_id, .. }) = events.last().cloned() else {
        unreachable!();
    };
    session
        .adapter
        .agent_call_result(call_id, AgentCallResult::Ok(json!({"presets": []})))
        .await;
    let (request, payload) = session.until_request().await;
    assert_eq!(payload["tool_name"], "canterlot_canterlot_preset_apply");
    let outcome = session
        .adapter
        .deliver_decision(&request, &permission(PermissionBehavior::Allow, None))
        .await;
    assert!(
        matches!(outcome, DecisionOutcome::Applied(_)),
        "{outcome:?}"
    );
    let events = session
        .until(|event| matches!(event, AdapterEvent::AgentCall { .. }))
        .await;
    let Some(AdapterEvent::AgentCall { call_id, tool, .. }) = events.last().cloned() else {
        unreachable!();
    };
    assert_eq!(tool, "presets.apply");
    session
        .adapter
        .agent_call_result(call_id, AgentCallResult::Ok(json!({"applied": true})))
        .await;
    let command = turn.command_id;
    session
        .until(|event| matches!(event, AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == command))
        .await;
    session.close().await;
    let stub = Stub::start(vec![Reply::text("ок")]).await;
    let mut session = Session::start(&stub, "read_only").await;
    session.turn(&input("читай")).await;
    let tools = offered(&stub.requests()[0]);
    assert!(
        tools
            .iter()
            .any(|tool| tool == "canterlot_canterlot_limits")
    );
    assert!(
        tools
            .iter()
            .any(|tool| tool == "canterlot_canterlot_presets_list")
    );
    assert!(
        !tools
            .iter()
            .any(|tool| tool == "canterlot_canterlot_preset_apply")
    );
    assert!(
        !tools
            .iter()
            .any(|tool| tool == "canterlot_canterlot_preset_save")
    );
    session.close().await;
}

const PNG: [u8; 67] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0xf8, 0xcf, 0x50,
    0x0f, 0x00, 0x03, 0x86, 0x01, 0x80, 0x5a, 0x34, 0x7d, 0x6b, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae,
];

// Картинка, на которую посмотрел агент: те же байты, что в файле.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_picture_the_agent_read_becomes_media() {
    use base64::Engine as _;
    let stub = Stub::start(vec![
        Reply::tool("read", json!({"path": "картинка.png"})),
        Reply::text("вижу"),
    ])
    .await;
    let mut session = Session::start_as(&stub, clean(), |live| {
        let mut bytes = PNG.to_vec();
        bytes.extend([0x42, 0x60, 0x82]);
        fs::write(live.workdir.join("картинка.png"), bytes).unwrap();
    })
    .await;
    let events = session.turn(&input("посмотри")).await;
    let source = events
        .iter()
        .find_map(|event| match event {
            AdapterEvent::Media { source, .. } => Some(source.clone()),
            _ => None,
        })
        .expect("media");
    let MediaSource::Base64(encoded) = source else {
        panic!("{source:?}");
    };
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    assert_eq!(
        decoded,
        fs::read(session.live.workdir.join("картинка.png")).unwrap()
    );
    session.close().await;
}

fn attachment(live: &Live, name: &str, mime: &str, image: bool) -> LocalFile {
    let dir = live.exec_dir.join("вложения");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let mut bytes = PNG.to_vec();
    bytes.extend([0x42, 0x60, 0x82]);
    fs::write(&path, &bytes).unwrap();
    LocalFile {
        path,
        name: name.into(),
        mime: mime.into(),
        size: bytes.len() as u64,
        image,
    }
}

// Модель с картинками получает файл вложением, настоящий сервер читает кириллический путь.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_image_attachment_reaches_a_vision_model_as_an_image() {
    let stub = Stub::start(vec![Reply::text("вижу")]).await;
    let mut session = Session::with_config(&stub.config_with_input(&["text", "image"])).await;
    let mut turn = input("посмотри");
    turn.files = vec![attachment(&session.live, "кот.png", "image/png", true)];
    session.turn(&turn).await;
    let seen = stub.requests()[0].to_string();
    assert!(seen.contains("image_url"), "{seen}");
    assert!(seen.contains("data:image/png;base64"), "{seen}");
    session.close().await;
}

// Модель без картинок и двоичный файл: только путь в тексте, вложения серверу нет.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_model_without_pictures_gets_paths() {
    let stub = Stub::start(vec![Reply::text("понял")]).await;
    let mut session = Session::with_config(&stub.config_with_input(&["text"])).await;
    let mut turn = input("посмотри");
    turn.files = vec![
        attachment(&session.live, "кот.png", "image/png", true),
        attachment(
            &session.live,
            "данные.bin",
            "application/octet-stream",
            false,
        ),
    ];
    session.turn(&turn).await;
    let seen = stub.requests()[0].to_string();
    assert!(!seen.contains("image_url"), "{seen}");
    assert!(seen.contains("кот.png"), "{seen}");
    assert!(seen.contains("данные.bin"), "{seen}");
    session.close().await;
}

fn paced(chunks: &[&str], pause_ms: u64) -> Reply {
    Reply::Paced {
        chunks: chunks.iter().map(|chunk| (*chunk).to_owned()).collect(),
        pause_ms,
    }
}

/// Сверка по тому, что собрал бы хаб: один старт и один финал, дельты равны финалу.
fn assert_each_block_once(events: &[AdapterEvent]) -> Vec<String> {
    use std::collections::BTreeMap;
    let mut blocks: BTreeMap<Uuid, (usize, String, Vec<String>)> = BTreeMap::new();
    for event in events {
        match event {
            AdapterEvent::BlockStarted { block_id, .. } => {
                blocks.entry(*block_id).or_default().0 += 1;
            }
            AdapterEvent::BlockDelta { block_id, append } => {
                blocks.entry(*block_id).or_default().1.push_str(append);
            }
            AdapterEvent::BlockFinal { block_id, text, .. } => {
                blocks.entry(*block_id).or_default().2.push(text.clone());
            }
            _ => {}
        }
    }
    for (id, (started, deltas, finals)) in &blocks {
        assert_eq!(*started, 1, "{id}: {blocks:?}");
        assert_eq!(finals.len(), 1, "{id}: {blocks:?}");
        assert!(
            deltas.is_empty() || *deltas == finals[0],
            "{id}: {blocks:?}"
        );
    }
    blocks.into_values().flat_map(|block| block.2).collect()
}

// Настоящий сервер, настоящий разрыв: дельты посреди текста потеряны, конец хода может быть
// в разрыве или после него. Блоки целы и не задвоены, провайдер получил один запрос на шаг.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_stream_cut_loses_no_block_and_duplicates_none() {
    let stub = Stub::start(vec![
        paced(&["первый ", "второй ", "третий ", "четвёртый"], 250),
        Reply::text("хвост"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let turn = input("начни");
    session.transmit(&turn).await;
    let mut events = session
        .until(|event| matches!(event, AdapterEvent::BlockDelta { .. }))
        .await;
    session.adapter.cut_stream();
    let command = turn.command_id;
    events.extend(
        session
            .until(|event| matches!(event, AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == command))
            .await,
    );
    let texts = assert_each_block_once(&events);
    assert_eq!(texts, ["первый второй третий четвёртый"]);
    assert_eq!(stub.requests().len(), 1);
    let usage = events.iter().find_map(|event| match event {
        AdapterEvent::RunFinished { usage, .. } => Some(usage.clone()),
        _ => None,
    });
    assert_eq!(usage.unwrap()[0].input_tokens, 70, "counted once");
    let next = input("дальше");
    session.turn(&next).await;
    assert_eq!(stub.requests().len(), 2, "the input after the cut works");
    session.close().await;
}

// Весь конец хода в разрыве: живого терминала нет, ход закрывает проекция настоящего сервера.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_stream_cut_over_the_end_of_the_turn_finishes_it() {
    let stub = Stub::start(vec![
        paced(&["начало ", "конец"], 50),
        Reply::text("второй"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let turn = input("начни");
    session.transmit(&turn).await;
    let mut events = session
        .until(|event| matches!(event, AdapterEvent::BlockDelta { .. }))
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    session.adapter.cut_stream();
    let command = turn.command_id;
    events.extend(
        session
            .until(|event| matches!(event, AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == command))
            .await,
    );
    assert_each_block_once(&events);
    session.turn(&input("дальше")).await;
    session.close().await;
}

// Поток рвётся сразу после приёма ввода: запрос рождается в разрыве или сразу за ним. Карточка
// одна, откуда бы она ни пришла, решение доходит, инструмент работает.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_permission_around_a_cut_reaches_the_owner_once() {
    let stub = Stub::start(vec![
        Reply::tool(
            "shell",
            json!({"command": "echo разрешено", "description": "x"}),
        ),
        Reply::text("готово"),
    ])
    .await;
    let mut session = Session::start(&stub, "ask").await;
    let turn = input("запусти");
    session.transmit(&turn).await;
    session.adapter.cut_stream();
    let (request, payload) = session.until_request().await;
    assert_eq!(payload["command"], "echo разрешено");
    let outcome = session
        .adapter
        .deliver_decision(&request, &permission(PermissionBehavior::Allow, None))
        .await;
    assert!(
        matches!(outcome, DecisionOutcome::Applied(_)),
        "{outcome:?}"
    );
    let command = turn.command_id;
    let events = session
        .until(|event| matches!(event, AdapterEvent::RunFinished { input_command_id, .. } if *input_command_id == command))
        .await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AdapterEvent::RequestOpened { .. })),
        "{events:?}"
    );
    assert!(
        finals(&events)
            .iter()
            .any(|text| text.contains("разрешено"))
    );
    assert_eq!(stub.requests().len(), 2);
    session.close().await;
}

impl Live {
    // Новое исполнение той же базы: свой каталог, та же сессия.
    fn second_execution(&self) -> OpenCodeAdapter {
        let exec_dir = self.root.path().join("data/executions/two");
        fs::create_dir_all(&exec_dir).unwrap();
        OpenCodeAdapter::new(
            OpenCodeConfig {
                executable: self.wrapper.clone(),
                owner_config: Some(self.root.path().join("config/opencode")),
                ..Default::default()
            },
            exec_dir,
            Duration::from_secs(20),
        )
        .with_relay_executable(PathBuf::from(env!("CARGO_BIN_EXE_canterlot-worker")))
    }

    fn shell_journal(&self, native: &str) -> PathBuf {
        self.root
            .path()
            .join("data/opencode/shells")
            .join(format!("{native}.json"))
    }

    async fn wait_for(&self, what: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !what() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn kill(pid: u32) {
    let pid = Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    kill_process(pid, Signal::KILL).unwrap();
}

async fn drain(adapter: &mut OpenCodeAdapter) -> Vec<AdapterEvent> {
    let mut events = Vec::new();
    while let Ok(event) =
        tokio::time::timeout(Duration::from_millis(500), adapter.next_event()).await
    {
        events.push(event);
    }
    events
}

// SIGKILL посреди команды: новый запуск находит текст до неё, закрывает сироту, снимает её
// процесс и не шлёт ввода. Следующий ввод настоящий сервер закрывает тем же вызовом: блок уже
// закрыт. Модель получает старый ввод один раз.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_server_kill_recovers_on_resume_and_stops_the_orphan() {
    let stub = Stub::start(vec![
        Reply::Tools(vec![(
            "shell".into(),
            json!({"command": "sleep 4712", "description": "x"}),
        )]),
        Reply::text("дальше"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let native = session.adapter.native_session().to_owned();
    let turn = input("запусти");
    session.transmit(&turn).await;
    session
        .until(|event| {
            matches!(event, AdapterEvent::Phase { summary: Some(summary), .. }
            if summary.contains("sleep 4712"))
        })
        .await;
    let journal = session.live.shell_journal(&native);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !journal.exists() {
        assert!(Instant::now() < deadline, "journal of the running shell");
        let _ = tokio::time::timeout(Duration::from_millis(50), session.adapter.next_event()).await;
    }
    assert!(session.live.running("sleep 4712"));
    for pid in session.live.servers() {
        kill(pid);
    }
    let Session {
        live,
        group,
        adapter,
    } = session;
    drop(adapter);
    assert!(live.running("sleep 4712"), "the orphan outlives the server");
    let mut second = live.second_execution();
    let mut spec = live.spec("fake/fake-1", "bypass");
    spec.native_conversation_id = Some(native.clone());
    let started = tokio::time::timeout(WAIT, second.start(&spec, &group))
        .await
        .expect("start must finish")
        .expect("resume");
    assert_eq!(started.native_conversation_id, native);
    let events = drain(&mut second).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "opencode_orphans_stopped",
            ..
        }
    )));
    assert!(
        summaries(&events)
            .iter()
            .any(|summary| summary.ends_with(", interrupted")),
        "{:?}",
        summaries(&events)
    );
    live.wait_for(|| !live.running("sleep 4712")).await;
    assert_eq!(stub.requests().len(), 1, "nothing was sent on resume");
    let mut session = Session {
        live,
        group,
        adapter: second,
    };
    let next = session.turn(&input("продолжай")).await;
    assert!(
        !summaries(&next)
            .iter()
            .any(|summary| summary.contains("sleep 4712")),
        "{:?}",
        summaries(&next)
    );
    let seen = stub.requests()[1].to_string();
    assert_eq!(seen.matches("запусти").count(), 1, "{seen}");
    session.close().await;
}

// Сервер убит, супервизор жив: команды сервера остаются его потомками через subreaper, поэтому
// их снимает обычное дерево процессов, как у Codex.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_orphan_of_a_killed_server_dies_with_the_process_tree() {
    use canterlot_worker::containment::Containment as _;
    let stub = Stub::start(vec![Reply::Tools(vec![(
        "shell".into(),
        json!({"command": "sleep 4713", "description": "x"}),
    )])])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let turn = input("запусти");
    session.transmit(&turn).await;
    session
        .until(|event| {
            matches!(event, AdapterEvent::Phase { summary: Some(summary), .. }
            if summary.contains("sleep 4713"))
        })
        .await;
    session
        .live
        .wait_for(|| session.live.running("sleep 4713"))
        .await;
    for pid in session.live.servers() {
        kill(pid);
    }
    session
        .until(|event| matches!(event, AdapterEvent::ProcessExited { .. }))
        .await;
    assert!(session.live.running("sleep 4713"), "the orphan is alive");
    session
        .group
        .terminate(Duration::from_secs(2))
        .await
        .unwrap();
    session
        .live
        .wait_for(|| !session.live.running("sleep 4713"))
        .await;
}

// Смерть супервизора в живом потоке: сервер уходит по EOF stdin и сам дописывает проекцию.
// Новое исполнение показывает то, что журнал не получил, а незавершённый ввод не шлёт.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_stdin_lease_keeps_the_partial_text_for_the_next_execution() {
    let stub = Stub::start(vec![paced(&["уцелевший ", "кусок", " никогда"], 700)]).await;
    let mut session = Session::start(&stub, "bypass").await;
    let native = session.adapter.native_session().to_owned();
    let turn = input("пиши");
    session.transmit(&turn).await;
    session
        .until(|event| matches!(event, AdapterEvent::BlockDelta { .. }))
        .await;
    let Session {
        live,
        group,
        adapter,
    } = session;
    drop(adapter);
    live.wait_gone().await;
    let mut second = live.second_execution();
    let mut spec = live.spec("fake/fake-1", "bypass");
    spec.native_conversation_id = Some(native);
    tokio::time::timeout(WAIT, second.start(&spec, &group))
        .await
        .expect("start must finish")
        .expect("resume");
    let events = drain(&mut second).await;
    let texts = assert_each_block_once(&events);
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(texts[0].starts_with("уцелевший"), "{texts:?}");
    assert_eq!(stub.requests().len(), 1);
    second.close(CloseMode::Graceful, &group).await.unwrap();
    live.wait_gone().await;
}

// Субагент на настоящем сервере: связь с вызовом, имя агента и расход потомка. Последний
// ответ заглушки уходит родителю, потому что потомок завершает свой шаг раньше.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_subagent_runs_in_the_lane_of_its_call() {
    let stub = Stub::start(vec![
        Reply::tool(
            "subagent",
            json!({"agent": "general", "description": "проба", "prompt": "сделай"}),
        ),
        Reply::text("готово у потомка"),
        Reply::text("готово"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let events = session.turn(&input("делегируй")).await;
    let call = events
        .iter()
        .find_map(|event| match event {
            AdapterEvent::BlockStarted {
                block_id,
                tool_name: Some(name),
                ..
            } if name == "subagent" => Some(*block_id),
            _ => None,
        })
        .expect("subagent call");
    let lane: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockStarted {
                parent,
                agent,
                kind: BlockKind::Text,
                ..
            } => Some((*parent, agent.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        lane,
        vec![
            (Some(call), "general".to_owned()),
            (None, "orchestrator".to_owned())
        ]
    );
    let Some(AdapterEvent::RunFinished { usage, .. }) = events.last() else {
        panic!("run must finish");
    };
    assert!(
        usage.iter().any(|line| line.agent == "general"),
        "{usage:?}"
    );
    assert!(
        usage.iter().any(|line| line.agent == "orchestrator"),
        "{usage:?}"
    );
    session.close().await;
}

// Флаг `experimental.todo` на настоящем сервере: инструмент есть и отдаёт полный список.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_todo_list_comes_whole_with_a_cancelled_item() {
    let stub = Stub::start(vec![
        Reply::tool(
            "todowrite",
            json!({"todos": [
                {"content": "сделано", "status": "completed"},
                {"content": "снято", "status": "cancelled"},
            ]}),
        ),
        Reply::text("готово"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let events = session.turn(&input("составь план")).await;
    let lists: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::TodoUpdated { lane, items } => Some((*lane, items.clone())),
            _ => None,
        })
        .collect();
    let todo = |text: &str, status| TodoItem {
        text: text.into(),
        status,
    };
    assert_eq!(
        lists,
        vec![(
            None,
            vec![
                todo("сделано", TodoStatus::Completed),
                todo("снято", TodoStatus::Cancelled)
            ]
        )]
    );
    session.close().await;
}

const OBSERVED_REFUSAL: &str = "The request contains invalid parameters. Check the request body for any errors or inconsistencies.";

/// Сводка по обязательному шаблону OpenCode, запись этапа 0.
const SUMMARY: &str = "## Objective\n- probe\n\n## Requirements\n- (none)\n\n## Decisions\n- (none)\n\n## Work State\n### Completed\n- (none)\n\n### Active\n- (none)\n\n### Blocked\n- (none)\n\n## Next Move\n1. (none)\n\n## Relevant Files\n- (none)\n\n## Important Context\n- (none)\n";

fn refusal(message: &str) -> Reply {
    Reply::Status {
        status: 400,
        body: json!({"model": "fake-1", "error": {
            "code": null, "message": message, "param": null,
            "type": "invalid_request_error"}}),
    }
}

impl Session {
    async fn compact(&mut self) -> CompactOutcome {
        tokio::time::timeout(WAIT, self.adapter.compact(Uuid::now_v7()))
            .await
            .expect("compact must finish")
    }

    async fn refused_turn(&mut self, text: &str) {
        let events = self.turn(&input(text)).await;
        assert!(
            events.iter().any(|event| matches!(
                event,
                AdapterEvent::BlockStarted {
                    kind: BlockKind::Error,
                    ..
                }
            )),
            "{events:?}"
        );
    }
}

// Запись `stuck_invalid_request` на настоящем сервере: отказ, отказ, сжатие, ответ.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_same_refusal_twice_offers_compaction_and_it_recovers() {
    let stub = Stub::start(vec![
        refusal(OBSERVED_REFUSAL),
        refusal(OBSERVED_REFUSAL),
        Reply::text(SUMMARY),
        Reply::text("снова работает"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    session.refused_turn("один").await;
    session.refused_turn("два").await;
    session
        .until(|event| matches!(event, AdapterEvent::SessionStuck))
        .await;
    assert_eq!(session.compact().await, CompactOutcome::Accepted);
    let events = session
        .until(|event| matches!(event, AdapterEvent::Compacted { .. }))
        .await;
    assert!(matches!(
        events.last(),
        Some(AdapterEvent::Compacted { ok: true })
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Phase {
            phase: Phase::ContextMaintenance,
            ..
        }
    )));
    session
        .until(|event| matches!(event, AdapterEvent::RunFinished { .. }))
        .await;
    let events = session.turn(&input("три")).await;
    assert_eq!(finals(&events), ["снова работает"]);
    assert_eq!(stub.requests().len(), 4, "one model call per input");
    session.close().await;
}

// Отказ сжатия снимается с настоящего сервера: в записях этапа 0 такого кадра нет.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_compaction_without_a_summary_is_reported() {
    let stub = Stub::start(vec![
        refusal(OBSERVED_REFUSAL),
        refusal(OBSERVED_REFUSAL),
        Reply::text("без шаблона"),
        Reply::text("снова без шаблона"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    session.refused_turn("один").await;
    session.refused_turn("два").await;
    session
        .until(|event| matches!(event, AdapterEvent::SessionStuck))
        .await;
    assert_eq!(session.compact().await, CompactOutcome::Accepted);
    let events = session
        .until(|event| matches!(event, AdapterEvent::Compacted { .. }))
        .await;
    assert!(
        matches!(events.last(), Some(AdapterEvent::Compacted { ok: false })),
        "{events:?}"
    );
    session
        .until(|event| matches!(event, AdapterEvent::RunFinished { .. }))
        .await;
    session.close().await;
}

// Второе переполнение: сервер сжал и снова получил отказ, чинить ему больше нечем.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_overflow_after_compaction_exhausts_the_context() {
    let overflow = "prompt is too long: 900000 tokens > 200000 maximum";
    let stub = Stub::start(vec![
        Reply::text("первый ответ"),
        refusal(overflow),
        Reply::text(SUMMARY),
        refusal(overflow),
        refusal(overflow),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    session.turn(&input("один")).await;
    session.refused_turn("два").await;
    session
        .until(|event| matches!(event, AdapterEvent::ContextExhausted))
        .await;
    session.close().await;
}

#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_quota_refusal_is_a_reached_limit_with_the_wait() {
    let stub = Stub::start(vec![Reply::Limited {
        body: json!({"error": {"message": "Go usage limit reached", "type": "GoUsageLimitError"}}),
        retry_after_s: 3600,
    }])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let events = session.turn(&input("x")).await;
    let fact = events.iter().find_map(|event| match event {
        AdapterEvent::Limit(fact) => Some(fact.clone()),
        _ => None,
    });
    assert!(
        matches!(
            fact,
            Some(LimitFact::Reached {
                window: LimitWindow::FiveHour,
                resets_at_ms: Some(_),
            })
        ),
        "{fact:?}"
    );
    session.close().await;
}

// Повтор запроса к модели на настоящем сервере: фаза есть, вторая запись ввода не нужна.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_retry_of_a_rate_limit_finishes_the_same_turn() {
    let stub = Stub::start(vec![
        Reply::Status {
            status: 429,
            body: json!({"error": {"message": "slow down", "type": "rate_limit_error"}}),
        },
        Reply::text("после повтора"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let events = session.turn(&input("x")).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Phase { summary: Some(summary), .. } if summary == "retry 2"
    )));
    assert_eq!(finals(&events), ["после повтора"]);
    assert_eq!(
        stub.requests()
            .iter()
            .filter(|request| request["messages"].to_string().contains('x'))
            .count(),
        2,
        "the model is called again, the input stays one"
    );
    session.close().await;
}

// Живьём у потомка `todowrite` на этапе 0 не снимался: L1 держит придуманный кадр, здесь он
// сверяется с настоящим сервером.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_child_todo_list_goes_to_the_lane_of_its_call() {
    let stub = Stub::start(vec![
        Reply::tool(
            "subagent",
            json!({"agent": "general", "description": "проба", "prompt": "сделай"}),
        ),
        Reply::tool(
            "todowrite",
            json!({"todos": [{"content": "дочерний пункт", "status": "in_progress"}]}),
        ),
        Reply::text("готово у потомка"),
        Reply::text("готово"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    let events = session.turn(&input("делегируй")).await;
    let call = events
        .iter()
        .find_map(|event| match event {
            AdapterEvent::BlockStarted {
                block_id,
                tool_name: Some(name),
                ..
            } if name == "subagent" => Some(*block_id),
            _ => None,
        })
        .expect("subagent call");
    let lists: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::TodoUpdated { lane, .. } => Some(*lane),
            _ => None,
        })
        .collect();
    assert_eq!(lists, vec![Some(call)], "{events:?}");
    session.close().await;
}

// Договор с проверенной версией: схема настоящего сервера совпадает с манифестом.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_server_schema_matches_the_manifest() {
    use sha2::{Digest, Sha256};
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
    };
    let stub = Stub::start(Vec::new()).await;
    let live = Live::new(&stub.config());
    let password = "manifest-check";
    let data = live.root.path().join("manifest");
    let mut child = Command::new(&live.wrapper)
        .args(["serve", "--stdio", "--port", "0"])
        .env("OPENCODE_PASSWORD", password)
        .env("XDG_DATA_HOME", data.join("data"))
        .env("XDG_STATE_HOME", data.join("state"))
        .env("XDG_CACHE_HOME", data.join("cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let ready: Value = serde_json::from_str(&line).unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let bytes = client
        .get(format!("{}/openapi.json", ready["url"].as_str().unwrap()))
        .basic_auth("opencode", Some(password))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    drop(child.stdin.take());
    child.wait().unwrap();
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        digest, VERIFIED_OPENAPI_SHA256,
        "scripts/opencode-schema-check.sh"
    );
}

fn user_messages(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "user")
        .map(|message| message["content"].to_string())
        .collect()
}

fn mode_input(text: &str, mode: &str) -> Transmission {
    let mut next = input(text);
    next.settings = Some(SettingsChange {
        permission_mode: Some(mode.into()),
        ..Default::default()
    });
    next
}

// Сервер сам вставляет в историю `<system-update>` с Code Mode при возврате инструментов;
// напоминание о прямых вызовах идёт в первом вводе после выхода и больше нигде.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_leaving_read_only_reminds_the_model_once() {
    let stub = Stub::start(vec![
        Reply::text("раз"),
        Reply::text("два"),
        Reply::text("три"),
        Reply::text("четыре"),
    ])
    .await;
    let mut session = Session::start(&stub, "bypass").await;
    session.turn(&input("первый")).await;
    session.turn(&mode_input("второй", "read_only")).await;
    session.turn(&mode_input("третий", "owner")).await;
    session.turn(&input("четвёртый")).await;
    let requests = stub.requests();
    let noted = |index: usize| {
        user_messages(&requests[index])
            .iter()
            .filter(|text| text.contains("Read-only mode is over"))
            .count()
    };
    assert_eq!(noted(1), 0);
    assert_eq!(noted(2), 1, "{:?}", user_messages(&requests[2]));
    assert_eq!(
        noted(3),
        1,
        "the note stays in history, but is not repeated"
    );
    session.close().await;
}

// Режим мог смениться, пока исполнение стояло: заметно по правилам, оставшимся в сессии.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn real_resume_out_of_read_only_reminds_the_model() {
    let stub = Stub::start(vec![Reply::text("раз"), Reply::text("два")]).await;
    let mut session = Session::start(&stub, "read_only").await;
    session.turn(&input("первый")).await;
    let native = session.adapter.native_session().to_owned();
    let Session {
        live,
        group,
        mut adapter,
    } = session;
    adapter.close(CloseMode::Graceful, &group).await.unwrap();
    live.wait_gone().await;
    let mut second = live.second_execution();
    let mut spec = live.spec("fake/fake-1", "owner");
    spec.native_conversation_id = Some(native);
    tokio::time::timeout(WAIT, second.start(&spec, &group))
        .await
        .expect("start must finish")
        .expect("resume");
    let mut session = Session {
        live,
        group,
        adapter: second,
    };
    session.turn(&input("второй")).await;
    let texts = user_messages(&stub.requests()[1]);
    assert!(
        texts
            .iter()
            .any(|text| text.contains("Read-only mode is over")),
        "{texts:?}"
    );
    session.close().await;
}

// D24: сервер с именем nanokvm в конфигурации владельца и в репозитории. Настоящий opencode
// запускает его при старте сессии владельца и не запускает при делегированной.
#[tokio::test]
#[ignore = "L2: настоящий opencode, см. заголовок файла"]
async fn delegated_session_never_starts_owner_or_project_servers() {
    for (isolate, mode) in [
        (false, InstructionMode::Owner),
        (true, InstructionMode::Owner),
        (true, InstructionMode::Clean),
    ] {
        let live = Live::new("{}");
        let server = |name: &str| {
            json!({"type": "local", "environment": {}, "command": [
                "sh", "-c", format!("touch '{}/marker-{name}'; sleep 60", live.root.path().display()),
            ]})
        };
        fs::write(
            live.root.path().join("config/opencode/opencode.json"),
            json!({"mcp": {"servers": {"nanokvm": server("nanokvm")}}}).to_string(),
        )
        .unwrap();
        fs::write(
            live.workdir.join("opencode.json"),
            json!({"mcp": {"servers": {"projkvm": server("projkvm")}}}).to_string(),
        )
        .unwrap();
        let group = TreeContainment::new().unwrap();
        let mut adapter = live.adapter();
        let mut spec = live.spec("", "bypass");
        spec.isolate_mcp = isolate;
        spec.instructions.mode = mode;
        tokio::time::timeout(WAIT, adapter.start(&spec, &group))
            .await
            .expect("start must finish")
            .expect("start");
        tokio::time::sleep(Duration::from_secs(2)).await;
        let started = |name: &str| live.root.path().join(format!("marker-{name}")).exists();
        assert_eq!(started("nanokvm"), !isolate, "isolate={isolate} {mode:?}");
        assert_eq!(started("projkvm"), !isolate, "isolate={isolate} {mode:?}");
        adapter.close(CloseMode::Graceful, &group).await.unwrap();
        live.wait_gone().await;
    }
}
