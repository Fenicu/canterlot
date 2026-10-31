#![forbid(unsafe_code)]
#![cfg(feature = "opencode-live")]

//! L2: настоящий `opencode` с изолированными каталогами. Заглушка провайдера живёт в
//! этом процессе на loopback, внешних запросов к моделям нет. Тесты не входят в
//! check.sh, запуск:
//!
//! ```text
//! cargo test -p canterlot-worker --features opencode-live --test opencode_live -- --ignored --test-threads=1
//! ```

use canterlot_worker::{
    adapter::{
        Adapter, AdapterEvent, CloseMode, InstructionMode, InstructionSpec, ModelUsage, StartSpec,
        Transmission, TransmitOutcome, opencode::OpenCodeAdapter,
    },
    config::OpenCodeConfig,
    containment::TreeContainment,
};
use rustix::process::{Pid, Signal, kill_process};
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
                "#!/bin/sh\nexport HOME='{}' XDG_CONFIG_HOME='{}' OPENCODE_DISABLE_MODELS_FETCH=1\nexec '{}' \"$@\"\n",
                home.display(),
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
