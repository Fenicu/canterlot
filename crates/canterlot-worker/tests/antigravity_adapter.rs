#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

//! Контракт адаптера Antigravity на `fake-agy`: кадры этапа 0, запуск, resume и каталог.

use canterlot_worker::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, CloseMode, InstructionMode, InstructionSpec,
        StartSpec,
        antigravity::{
            AntigravityAdapter, catalog,
            process::ENV_ALLOWLIST,
            replay::{Script, Step, evidence_result, session_list},
        },
    },
    config::AntigravityConfig,
    containment::TreeContainment,
};
use serde_json::Value;
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

#[path = "support/antigravity.rs"]
mod antigravity;
use antigravity::{Fake, calls};

const WAIT: Duration = Duration::from_secs(10);

fn instructions() -> InstructionSpec {
    InstructionSpec {
        mode: InstructionMode::Clean,
        text: "canterlot test append".into(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

fn spec(model: &str, effort: &str, mode: &str) -> StartSpec {
    StartSpec {
        execution_id: Uuid::now_v7(),
        generation: 1,
        workdir: PathBuf::new(),
        native_conversation_id: None,
        profile: "antigravity".into(),
        model: model.into(),
        effort: effort.into(),
        permission_mode: mode.into(),
        instructions: instructions(),
        isolate_mcp: false,
    }
}

fn init_step() -> Step {
    Script::from_evidence("p1-diagnostics.jsonl").steps[0].clone()
}

fn new_step() -> Step {
    Script::from_evidence("p1-diagnostics.jsonl").steps[2].clone()
}

fn set_model_step() -> Step {
    Script::from_evidence("p14-model-effort.jsonl").steps[3].clone()
}

fn set_effort_step() -> Step {
    Script::from_evidence("p14-model-effort.jsonl").steps[7].clone()
}

struct Bench {
    fake: Fake,
    adapter: AntigravityAdapter,
    group: TreeContainment,
    spec: StartSpec,
    exec_dir: PathBuf,
}

impl Bench {
    fn new(script: Script, model: &str, effort: &str, mode: &str) -> Self {
        let fake = Fake::new(script);
        let exec_dir = fake.exec_dir();
        let cfg = AntigravityConfig {
            enabled: true,
            binary: Some(fake.exe.clone()),
            data_dir: Some(fake.data_dir()),
        };
        let adapter =
            AntigravityAdapter::new(cfg, exec_dir.clone(), WAIT).with_ready_deadline(WAIT);
        let mut start = spec(model, effort, mode);
        start.workdir = fake.workdir();
        Self {
            fake,
            adapter,
            group: TreeContainment::new().unwrap(),
            spec: start,
            exec_dir,
        }
    }

    async fn start(&mut self) -> canterlot_worker::adapter::Started {
        self.adapter.start(&self.spec, &self.group).await.unwrap()
    }
}

#[tokio::test]
async fn start_new_uses_the_recorded_session_and_pinned_copy() {
    let script = Script {
        steps: vec![init_step(), new_step(), set_model_step(), set_effort_step()],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "gemini-3.8-flash-low", "low", "bypass");
    let started = bench.start().await;
    assert_eq!(
        started.native_conversation_id,
        "d41d6945-1310-44ef-89e6-42705ee12e27"
    );
    assert!(started.cli_pid > 0);

    let pinned = bench.fake.data_dir().join("antigravity/bin/agy-1.4.12");
    assert!(pinned.is_file(), "pinned copy missing");
    assert_eq!(
        std::fs::read_to_string(&pinned).unwrap(),
        std::fs::read_to_string(&bench.fake.exe).unwrap(),
        "pinned copy is the configured source binary"
    );
    let boot = bench.fake.boot();
    let args: Vec<&str> = boot["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    // Обёртка теста добавляет `--script <файл>` перед аргументами CLI.
    let start = args.iter().position(|arg| *arg == "acp").expect("acp");
    assert_eq!(
        &args[start..],
        [
            "acp",
            "--log-file",
            bench.exec_dir.join("agy.log").to_string_lossy().as_ref(),
            "--disable-telemetry",
            "--dangerously-skip-permissions",
        ]
    );
    assert!(!args.iter().any(|arg| arg.starts_with("--gemini-dir")));
    assert!(!args.iter().any(|arg| arg.starts_with("--gemini_dir")));

    let requests = bench.fake.requests();
    let new = calls(&requests, "session/new");
    assert_eq!(new.len(), 1);
    assert_eq!(
        new[0]["frame"]["params"]["cwd"],
        bench.spec.workdir.to_string_lossy().as_ref()
    );
    assert_eq!(
        new[0]["frame"]["params"]["mcpServers"],
        Value::Array(vec![])
    );
    assert_eq!(
        new[0]["frame"]["params"]["_meta"]["antigravity"]["systemPrompt"]["append"],
        "canterlot test append"
    );
    let model = calls(&requests, "session/set_model");
    assert_eq!(
        model[0]["frame"]["params"]["modelId"],
        "gemini-3.8-flash-low"
    );
    let effort = calls(&requests, "session/set_config_option");
    assert_eq!(effort[0]["frame"]["params"]["configId"], "thought_level");
    assert_eq!(effort[0]["frame"]["params"]["value"], "low");
    assert!(calls(&requests, "session/load").is_empty());
    bench
        .adapter
        .close(CloseMode::Graceful, &bench.group)
        .await
        .unwrap();
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn rejected_model_setting_fails_start_with_the_32602_code() {
    let script = Script {
        steps: vec![
            init_step(),
            new_step(),
            Script::expect_error(
                "session/set_model",
                serde_json::json!({"code": -32602, "message": "Unknown model ID: no-such-model-xyz"}),
            ),
        ],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "no-such-model-xyz", "", "owner");
    let error = bench
        .adapter
        .start(&bench.spec, &bench.group)
        .await
        .unwrap_err();
    assert!(matches!(error, AdapterError::StartFailed("model_rejected")));
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn rejected_effort_setting_fails_start_with_the_32602_code() {
    let script = Script {
        steps: vec![
            init_step(),
            new_step(),
            set_model_step(),
            Script::expect_error(
                "session/set_config_option",
                serde_json::json!({"code": -32602, "message": "Unknown config option: "}),
            ),
        ],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "gemini-3.8-flash-low", "max", "owner");
    let error = bench
        .adapter
        .start(&bench.spec, &bench.group)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AdapterError::StartFailed("effort_rejected")
    ));
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn env_is_an_allowlist_with_forced_file_storage() {
    let script = Script {
        steps: vec![init_step(), new_step()],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    bench.start().await;
    let boot = bench.fake.boot();
    let keys: Vec<&str> = boot["env_keys"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let allowed = ENV_ALLOWLIST
        .iter()
        .copied()
        .chain(["GEMINI_FORCE_FILE_STORAGE"]);
    for key in &keys {
        assert!(
            allowed.clone().any(|allowed| allowed == *key),
            "unexpected environment key leaked: {key}"
        );
    }
    assert_eq!(boot["env"]["GEMINI_FORCE_FILE_STORAGE"], "true");
    assert!(!keys.contains(&"OPENAI_API_KEY"));
    let args: Vec<&str> = boot["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(!args.contains(&"--dangerously-skip-permissions"));
    bench
        .adapter
        .close(CloseMode::Graceful, &bench.group)
        .await
        .unwrap();
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn resume_checks_session_list_and_never_uses_load() {
    let session_id = "7497af9f-8ce0-4bb6-9b42-f14ac3fd1890";
    let script = Script {
        steps: vec![
            init_step(),
            Script::expect("session/list", session_list(&[session_id])),
            Script::expect("session/resume", evidence_result("t13b-resume.jsonl", 2)),
        ],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    bench.spec.native_conversation_id = Some(session_id.into());
    let started = bench.start().await;
    assert_eq!(started.native_conversation_id, session_id);
    let requests = bench.fake.requests();
    assert_eq!(calls(&requests, "session/list").len(), 1);
    assert_eq!(calls(&requests, "session/resume").len(), 1);
    assert!(calls(&requests, "session/load").is_empty());
    bench
        .adapter
        .close(CloseMode::Graceful, &bench.group)
        .await
        .unwrap();
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn missing_session_in_list_fails_start() {
    let script = Script {
        steps: vec![
            init_step(),
            Script::expect("session/list", session_list(&[])),
        ],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    bench.spec.native_conversation_id = Some("7497af9f-8ce0-4bb6-9b42-f14ac3fd1890".into());
    let error = bench
        .adapter
        .start(&bench.spec, &bench.group)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        AdapterError::StartFailed("conversation not found")
    ));
    assert!(calls(&bench.fake.requests(), "session/resume").is_empty());
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn unknown_version_is_allowed_with_a_diagnostic() {
    let mut initialized = evidence_result("p1-diagnostics.jsonl", 1);
    initialized["agentInfo"]["version"] = "1.5.0".into();
    let script = Script {
        version: "agy 1.5.0".into(),
        steps: vec![Script::expect("initialize", initialized), new_step()],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    bench.start().await;
    assert!(matches!(
        bench.adapter.next_event().await,
        AdapterEvent::Diagnostic {
            kind: "untested_cli_version",
            required: false,
            ..
        }
    ));
    assert!(
        bench
            .fake
            .data_dir()
            .join("antigravity/bin/agy-1.5.0")
            .is_file()
    );
    bench
        .adapter
        .close(CloseMode::Graceful, &bench.group)
        .await
        .unwrap();
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn catalog_probe_reads_real_models_and_excludes_the_family_option() {
    let script = Script {
        steps: vec![init_step(), new_step()],
        ..Default::default()
    };
    let fake = Fake::new(script);
    let cfg = AntigravityConfig {
        enabled: true,
        binary: Some(fake.exe.clone()),
        data_dir: Some(fake.data_dir()),
    };
    let catalog = catalog::discover(&cfg).await.unwrap();
    let ids: Vec<&str> = catalog
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "gemini-3.8-flash-high",
            "gemini-3.8-flash-medium",
            "gemini-3.8-flash-low",
        ]
    );
    assert_eq!(
        catalog.current_model.as_deref(),
        Some("gemini-3.8-flash-high")
    );
    assert_eq!(catalog.efforts, vec!["low", "medium", "high"]);
    assert_eq!(catalog.current_effort.as_deref(), Some("high"));
    assert!(
        !ids.contains(&"gemini-3.8-flash"),
        "configOptions.model is not a model id"
    );
    let requests = fake.requests();
    assert_eq!(calls(&requests, "initialize").len(), 1);
    assert_eq!(calls(&requests, "session/new").len(), 1);
    assert!(calls(&requests, "session/prompt").is_empty());
    fake.wait_gone().await;
}

#[test]
fn catalog_parses_the_recorded_p1_frame() {
    let info = catalog::parse_frame(&evidence_result("p1-diagnostics.jsonl", 3)).unwrap();
    assert_eq!(info.session_id, "d41d6945-1310-44ef-89e6-42705ee12e27");
    let ids: Vec<&str> = info.models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "gemini-3.8-flash-high",
            "gemini-3.8-flash-medium",
            "gemini-3.8-flash-low",
        ]
    );
    assert_eq!(info.efforts, vec!["low", "medium", "high"]);
    let entry = catalog::AntigravityCatalog {
        version: canterlot_worker::adapter::antigravity::version::CliVersion::Verified(
            "1.4.12".into(),
        ),
        models: info.models,
        current_model: info.current_model,
        efforts: info.efforts,
        current_effort: info.current_effort,
    }
    .harness();
    assert_eq!(entry.permission_modes, vec!["owner", "ask", "bypass"]);
    assert!(!entry.images);
    assert!(entry.models.iter().all(|model| model.efforts.is_empty()));
}

#[tokio::test]
async fn graceful_close_exits_on_eof() {
    let script = Script {
        steps: vec![init_step(), new_step()],
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    bench.start().await;
    let started = std::time::Instant::now();
    bench
        .adapter
        .close(CloseMode::Graceful, &bench.group)
        .await
        .unwrap();
    assert!(started.elapsed() < WAIT);
    bench.fake.wait_gone().await;
}

#[tokio::test]
async fn force_close_kills_a_process_that_ignores_eof() {
    let script = Script {
        steps: vec![init_step(), new_step()],
        ignore_eof: true,
        ..Default::default()
    };
    let mut bench = Bench::new(script, "", "", "owner");
    bench.start().await;
    bench
        .adapter
        .close(CloseMode::Force, &bench.group)
        .await
        .unwrap();
    bench.fake.wait_gone().await;
}
