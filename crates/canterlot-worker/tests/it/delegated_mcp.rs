//! D24: делегированная сессия не грузит MCP-серверы владельца ни в одном адаптере.
//! Здесь проверки без настоящих CLI; с настоящими бинарниками их повторяет
//! `delegated_mcp_live.rs`.

use canterlot_worker::adapter::claude_code::process::{
    LaunchSpec, SessionArg, apply_mcp_isolation, build_args, build_env, write_exec_files,
};
use canterlot_worker::adapter::{InstructionMode, InstructionSpec};
use canterlot_worker::config::ClaudeCodeConfig;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use uuid::Uuid;

use crate::fake_exec;

fn instructions(mode: InstructionMode) -> InstructionSpec {
    InstructionSpec {
        mode,
        text: "text".into(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

fn claude_args(td: &TempDir, isolate_mcp: bool, mode: InstructionMode) -> Vec<String> {
    let exec_dir = td.path().join("exec");
    let workdir = td.path().join("work");
    fs::create_dir_all(&exec_dir).unwrap();
    fs::create_dir_all(&workdir).unwrap();
    let instructions = instructions(mode);
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "haiku",
        effort: "low",
        permission_mode: "bypass",
        instructions: &instructions,
        isolate_mcp,
        prompt_suggestions: false,
    };
    build_args(&spec, &ClaudeCodeConfig::default())
        .unwrap()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn claude_delegated_session_loads_only_the_canterlot_config() {
    for mode in [
        InstructionMode::Owner,
        InstructionMode::Clean,
        InstructionMode::Preset,
    ] {
        let td = TempDir::new().unwrap();
        let isolated = claude_args(&td, true, mode);
        assert!(
            isolated.iter().any(|arg| arg == "--strict-mcp-config"),
            "{mode:?}"
        );
        assert!(isolated.iter().any(|arg| arg == "--mcp-config"));
        // Плагины, навыки и настройки пресета остаются: флаг трогает только MCP.
        assert!(!isolated.iter().any(|arg| arg == "--bare"));
        assert!(!isolated.iter().any(|arg| arg == "--setting-sources"));
        let owner_started = claude_args(&td, false, mode);
        assert!(
            !owner_started.iter().any(|arg| arg == "--strict-mcp-config"),
            "{mode:?}: an owner-started session changes in no way"
        );
        let without_flag: Vec<_> = isolated
            .iter()
            .filter(|arg| *arg != "--strict-mcp-config")
            .collect();
        assert_eq!(
            without_flag,
            owner_started.iter().collect::<Vec<_>>(),
            "{mode:?}"
        );
    }
}

#[test]
fn claude_mcp_config_names_only_canterlot() {
    let td = TempDir::new().unwrap();
    let exec_dir = td.path().join("exec");
    let workdir = td.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    let instructions = instructions(InstructionMode::Clean);
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "haiku",
        effort: "low",
        permission_mode: "bypass",
        instructions: &instructions,
        isolate_mcp: true,
        prompt_suggestions: false,
    };
    write_exec_files(
        &spec,
        &ClaudeCodeConfig::default(),
        Path::new("/usr/bin/relay"),
        None,
    )
    .unwrap();
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(exec_dir.join("mcp.json")).unwrap()).unwrap();
    let names: Vec<_> = config["mcpServers"].as_object().unwrap().keys().collect();
    assert_eq!(names, ["canterlot"]);
}

#[test]
fn claude_account_connectors_are_switched_off_over_any_inherited_value() {
    let parent = [
        ("PATH", "/usr/bin"),
        ("ENABLE_CLAUDEAI_MCP_SERVERS", "true"),
    ]
    .map(|(key, value)| (OsString::from(key), OsString::from(value)));
    let mut env = build_env(parent.into_iter());
    apply_mcp_isolation(&mut env);
    let values: Vec<_> = env
        .iter()
        .filter(|(key, _)| key == "ENABLE_CLAUDEAI_MCP_SERVERS")
        .collect();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].1, "false");
}

// --- `[execution].delegate_mcp_servers`: разрешённые делегату серверы владельца. Ключи и
// значения поддельные; настоящие файлы владельца не открываются. ---

const FAKE_KEY: &str = "ctx7-FAKE-not-a-real-key";

fn worker_toml(extra: &str) -> String {
    let id = Uuid::now_v7();
    format!(
        "hub_url = \"https://hub.example\"\nca_cert = \"/ca\"\ntoken_file = \"/token\"\n\
         machine_id = \"{id}\"\ninstallation_id = \"{id}\"\nrestore_generation = \"{id}\"\n\
         data_dir = \"/data\"\n{extra}"
    )
}

fn load_worker(extra: &str) -> Result<canterlot_worker::config::WorkerConfig, String> {
    let td = TempDir::new().unwrap();
    let path = td.path().join("worker.toml");
    fs::write(&path, worker_toml(extra)).unwrap();
    canterlot_worker::config::load(&path).map_err(|error| error.to_string())
}

#[test]
fn delegate_mcp_servers_defaults_to_empty_and_takes_valid_names() {
    let config = load_worker("").unwrap();
    assert!(config.execution.delegate_mcp_servers.is_empty());
    let config =
        load_worker("[execution]\ndelegate_mcp_servers = [\"context7\", \"docs_2-x\"]\n").unwrap();
    assert_eq!(
        config.execution.delegate_mcp_servers,
        ["context7", "docs_2-x"]
    );
}

#[test]
fn delegate_mcp_servers_refuses_bad_names_and_canterlot() {
    for bad in ["", "canterlot", "a.b", "a b", "ctx/7", "ü"] {
        let error = load_worker(&format!(
            "[execution]\ndelegate_mcp_servers = [\"context7\", {bad:?}]\n"
        ))
        .unwrap_err();
        assert!(
            error.contains("execution.delegate_mcp_servers"),
            "{bad:?}: {error}"
        );
    }
    // Таблица остаётся строгой.
    let error = load_worker("[execution]\ndelegate_mcp = [\"context7\"]\n").unwrap_err();
    assert!(error.contains("invalid worker configuration"), "{error}");
}

#[test]
fn claude_user_config_file_follows_claude_config_dir() {
    use canterlot_worker::config::claude_user_config_file;
    let home = Path::new("/home/owner");
    assert_eq!(
        claude_user_config_file(home, None),
        Path::new("/home/owner/.claude.json")
    );
    assert_eq!(
        claude_user_config_file(home, Some(std::ffi::OsStr::new("/cfg/claude"))),
        Path::new("/cfg/claude/.claude.json")
    );
    assert_eq!(
        claude_user_config_file(home, Some(std::ffi::OsStr::new("relative"))),
        Path::new("/home/owner/.claude.json")
    );
}

/// Поддельный `~/.claude.json`: context7 и nanokvm у пользователя, ещё один сервер только
/// в проекте, прочие ключи с секретами, которые читать нельзя.
fn fake_claude_json(dir: &Path) -> std::path::PathBuf {
    let path = dir.join(".claude.json");
    fs::write(
        &path,
        serde_json::json!({
            "oauthAccount": {"emailAddress": "owner@example.invalid"},
            "mcpServers": {
                "context7": {
                    "type": "http",
                    "url": "https://mcp.context7.example/mcp",
                    "headers": {"Authorization": format!("Bearer {FAKE_KEY}")}
                },
                "nanokvm": {"type": "stdio", "command": "/bin/true"}
            },
            "projects": {"/w": {"mcpServers": {"projonly": {"type": "stdio", "command": "/bin/true"}}}}
        })
        .to_string(),
    )
    .unwrap();
    path
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn claude_reads_only_allowed_user_scope_servers() {
    use canterlot_worker::adapter::claude_code::process::read_owner_mcp_servers;
    let td = TempDir::new().unwrap();
    let path = fake_claude_json(td.path());
    let found = read_owner_mcp_servers(
        Some(&path),
        &names(&["context7", "projonly", "absent", "canterlot"]),
    );
    assert_eq!(found.servers.keys().collect::<Vec<_>>(), ["context7"]);
    assert_eq!(
        found.servers["context7"]["headers"]["Authorization"],
        format!("Bearer {FAKE_KEY}")
    );
    // Проектный сервер не пользовательский; canterlot владелец не подменяет.
    assert_eq!(found.missing, ["projonly", "absent"]);
    let debug = format!("{found:?}");
    assert!(
        !debug.contains(FAKE_KEY) && !debug.contains("context7.example"),
        "{debug}"
    );
    // Нет файла или он битый: всё разрешённое пропущено, без ошибки.
    let none = read_owner_mcp_servers(Some(&td.path().join("nope.json")), &names(&["context7"]));
    assert!(none.servers.is_empty());
    assert_eq!(none.missing, ["context7"]);
    fs::write(td.path().join("broken.json"), "{ broken").unwrap();
    let broken =
        read_owner_mcp_servers(Some(&td.path().join("broken.json")), &names(&["context7"]));
    assert_eq!(broken.missing, ["context7"]);
    let unset = read_owner_mcp_servers(None, &names(&["context7"]));
    assert_eq!(unset.missing, ["context7"]);
}

fn claude_mcp_json(
    td: &TempDir,
    delegates: &serde_json::Map<String, serde_json::Value>,
) -> (std::path::PathBuf, String) {
    use canterlot_worker::adapter::claude_code::process::write_exec_files_with;
    let exec_dir = td.path().join("exec");
    let workdir = td.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    let instructions = instructions(InstructionMode::Clean);
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "haiku",
        effort: "low",
        permission_mode: "bypass",
        instructions: &instructions,
        isolate_mcp: true,
        prompt_suggestions: false,
    };
    write_exec_files_with(
        &spec,
        &ClaudeCodeConfig::default(),
        Path::new("/usr/bin/relay"),
        Some("relay-secret"),
        delegates,
    )
    .unwrap();
    let path = exec_dir.join("mcp.json");
    let text = fs::read_to_string(&path).unwrap();
    (path, text)
}

#[test]
fn claude_mcp_config_adds_the_allowed_server_next_to_canterlot_privately() {
    use canterlot_worker::adapter::claude_code::process::read_owner_mcp_servers;
    use std::os::unix::fs::PermissionsExt;
    let td = TempDir::new().unwrap();
    let owner = fake_claude_json(td.path());
    let found = read_owner_mcp_servers(Some(&owner), &names(&["context7"]));
    let (path, text) = claude_mcp_json(&td, &found.servers);
    let config: serde_json::Value = serde_json::from_str(&text).unwrap();
    let servers: Vec<_> = config["mcpServers"].as_object().unwrap().keys().collect();
    assert_eq!(servers, ["canterlot", "context7"]);
    assert_eq!(config["mcpServers"]["context7"]["type"], "http");
    assert_eq!(config["mcpServers"]["canterlot"]["type"], "stdio");
    assert!(!text.contains("nanokvm") && !text.contains("projonly"));
    assert!(!text.contains("emailAddress"), "nothing else from the file");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Прежний файл с широкими правами тоже становится 0600.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    claude_mcp_json(&td, &found.servers);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Владелец не подменяет canterlot записью со своим именем.
    let mut spoof = serde_json::Map::new();
    spoof.insert(
        "canterlot".into(),
        serde_json::json!({"type": "http", "url": "x"}),
    );
    let (_, text) = claude_mcp_json(&td, &spoof);
    let config: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(config["mcpServers"]["canterlot"]["type"], "stdio");
}

#[test]
fn claude_mcp_config_with_no_allowed_server_is_exactly_as_before() {
    let td = TempDir::new().unwrap();
    let (_, with_empty) = claude_mcp_json(&td, &serde_json::Map::new());
    let exec_dir = td.path().join("exec");
    let instructions = instructions(InstructionMode::Clean);
    let workdir = td.path().join("work");
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "haiku",
        effort: "low",
        permission_mode: "bypass",
        instructions: &instructions,
        isolate_mcp: true,
        prompt_suggestions: false,
    };
    write_exec_files(
        &spec,
        &ClaudeCodeConfig::default(),
        Path::new("/usr/bin/relay"),
        Some("relay-secret"),
    )
    .unwrap();
    let plain = fs::read_to_string(exec_dir.join("mcp.json")).unwrap();
    assert_eq!(with_empty, plain);
    let expected = serde_json::json!({"mcpServers": {"canterlot": {
        "type": "stdio",
        "command": "/usr/bin/relay",
        "args": ["mcp-relay", "--socket", exec_dir.join("mcp.sock").display().to_string()],
        "env": {"CANTERLOT_MCP_SECRET": "relay-secret"}
    }}});
    assert_eq!(plain, expected.to_string());
}

// --- Codex: двойник app-server читает CODEX_HOME/config.toml и флаги так, как настоящий, и
// записывает, какие MCP-серверы он поднял бы при запуске треда. ---

mod codex {
    use canterlot_worker::adapter::{
        Adapter, AdapterContext, AdapterEvent, InstructionMode, InstructionSpec, SettingsChange,
        StartSpec, Transmission, TransmitOutcome, codex::home::TempDirPolicy,
    };
    use canterlot_worker::config::CodexConfig;
    use canterlot_worker::containment::TreeContainment;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::fake_exec;

    const WAIT: Duration = Duration::from_secs(20);

    const EMULATING_FAKE: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, sys, tomllib
d = pathlib.Path(__file__).parent
if '--version' in sys.argv:
    print('codex-cli 0.164.0'); sys.exit(0)
home = pathlib.Path(os.environ.get('CODEX_HOME') or d / 'owner-home')
config = {}
if (home / 'config.toml').exists():
    config = tomllib.loads((home / 'config.toml').read_text())
servers = {name: dict(server) for name, server in config.get('mcp_servers', {}).items()}
features = {'apps': True, 'plugins': True}
args = sys.argv[1:]
for n, arg in enumerate(args):
    if arg == '-c' and args[n + 1].startswith('mcp_servers.canterlot.'):
        servers.setdefault('canterlot', {})
    if arg == '--disable' and args[n + 1] in features and not (d / 'ignore-disable').exists():
        features[args[n + 1]] = False
def emit(obj):
    print(json.dumps(obj), flush=True)
def started():
    names = sorted(n for n, s in servers.items() if s.get('enabled', True))
    with (d / 'started.log').open('a') as log:
        log.write(json.dumps({'home': str(home), 'servers': names, 'args': args}) + '\n')
thread = {"thread": {"id": "thread-1", "cliVersion": "0.164.0"}, "model": "gpt-6-luna",
          "approvalPolicy": "on-request", "sandbox": {"type": "readOnly"}, "instructionSources": []}
models = {"data": [{"id": "gpt-6-luna", "model": "gpt-6-luna", "displayName": "gpt-6-luna",
    "description": "d", "hidden": False, "isDefault": True, "defaultReasoningEffort": "low",
    "supportedReasoningEfforts": [{"reasoningEffort": "low", "description": "low"}],
    "inputModalities": ["text"]}], "nextCursor": None}
turn_no = 0
for line in sys.stdin:
    try: msg = json.loads(line)
    except Exception: continue
    m = msg.get('method'); i = msg.get('id')
    if m == 'initialize': emit({'id': i, 'result': {'codexHome': str(home)}})
    elif m == 'account/read': emit({'id': i, 'result': {'account': {'type': 'chatgpt'}}})
    elif m == 'config/read':
        emit({'id': i, 'result': {'config': {'mcp_servers': servers, 'features': features}}})
    elif m == 'model/list': emit({'id': i, 'result': models})
    elif m in ('thread/start', 'thread/resume'):
        started(); emit({'id': i, 'result': thread})
    elif m == 'turn/start':
        turn_no += 1
        tid = f'turn-{os.getpid()}-{turn_no}'
        t = (msg.get('params') or {}).get('threadId')
        cid = (msg.get('params') or {}).get('clientUserMessageId')
        emit({'id': i, 'result': {'turn': {'id': tid, 'status': 'inProgress'}}})
        emit({'method': 'item/started', 'params': {'threadId': t, 'turnId': tid,
            'item': {'id': 'u', 'type': 'userMessage', 'clientId': cid, 'content': []}}})
        emit({'method': 'item/completed', 'params': {'threadId': t, 'turnId': tid, 'completedAtMs': 1,
            'item': {'id': 'u', 'type': 'userMessage', 'clientId': cid, 'content': []}}})
        emit({'method': 'item/started', 'params': {'threadId': t, 'turnId': tid,
            'item': {'id': 'a', 'type': 'agentMessage', 'text': 'ok'}}})
        emit({'method': 'item/completed', 'params': {'threadId': t, 'turnId': tid, 'completedAtMs': 2,
            'item': {'id': 'a', 'type': 'agentMessage', 'text': 'ok'}}})
        emit({'method': 'turn/completed', 'params': {'threadId': t,
            'turn': {'id': tid, 'status': 'completed', 'items': []}}})
"#;

    struct Fixture {
        dir: TempDir,
        owner: PathBuf,
        adapter: Box<dyn Adapter>,
        group: TreeContainment,
    }

    fn instructions(mode: InstructionMode) -> InstructionSpec {
        InstructionSpec {
            mode,
            text: if mode == InstructionMode::Preset {
                "preset".into()
            } else {
                String::new()
            },
            preset_id: (mode == InstructionMode::Preset).then(Uuid::now_v7),
            revision: 1,
            title: "t".into(),
        }
    }

    impl Fixture {
        fn new(setup: impl FnOnce(&Path)) -> Self {
            Self::with_delegates(setup, None, &[])
        }

        /// `owner_config` заменяет конфиг владельца, `allowed` — `delegate_mcp_servers`.
        fn with_delegates(
            setup: impl FnOnce(&Path),
            owner_config: Option<&str>,
            allowed: &[&str],
        ) -> Self {
            let dir = TempDir::new().unwrap();
            setup(dir.path());
            let exe = dir.path().join("fake-codex");
            fake_exec::write_executable(&exe, EMULATING_FAKE, 0o700);
            // Дом владельца с сервером nanokvm; настоящий ~/.codex не открывается.
            let owner = dir.path().join("owner-home");
            std::fs::create_dir_all(&owner).unwrap();
            std::fs::write(
                owner.join("config.toml"),
                "model = \"x\"\n\n[mcp_servers.nanokvm]\ncommand = \"/bin/true\"\n\n[mcp_servers.other]\ncommand = \"/bin/true\"\nenabled = true\n",
            )
            .unwrap();
            if let Some(text) = owner_config {
                std::fs::write(owner.join("config.toml"), text).unwrap();
            }
            std::fs::write(owner.join("AGENTS.md"), "owner instructions").unwrap();
            let context = AdapterContext {
                transmit_deadline: WAIT,
                ready_deadline: WAIT,
                exec_dir: dir.path().join("exec"),
                claude_code: None,
                codex: Some(CodexConfig {
                    executable: exe,
                    home_dir: Some(dir.path().join("codex-home")),
                    owner_home: Some(owner.clone()),
                    home_policy: TempDirPolicy::Allow,
                    ..Default::default()
                }),
                opencode: None,
                antigravity: None,
                delegate_mcp_servers: allowed.iter().map(|name| (*name).to_owned()).collect(),
            };
            let adapter = canterlot_worker::adapter::registry_with("codex", &context).unwrap();
            Self {
                dir,
                owner,
                adapter,
                group: TreeContainment::new().unwrap(),
            }
        }

        fn spec(mode: InstructionMode, isolate_mcp: bool, workdir: &Path) -> StartSpec {
            StartSpec {
                execution_id: Uuid::now_v7(),
                generation: 1,
                workdir: workdir.into(),
                native_conversation_id: None,
                profile: "codex".into(),
                model: "gpt-6-luna".into(),
                effort: "low".into(),
                permission_mode: "ask".into(),
                instructions: instructions(mode),
                isolate_mcp,
                prompt_suggestions: false,
            }
        }

        async fn start(&mut self, mode: InstructionMode, isolate_mcp: bool) -> Result<(), String> {
            let spec = Self::spec(mode, isolate_mcp, self.dir.path());
            tokio::time::timeout(WAIT, self.adapter.start(&spec, &self.group))
                .await
                .expect("start deadline")
                .map(|_| ())
                .map_err(|error| error.to_string())
        }

        /// Строки журнала двойника: что он запустил и под каким домом.
        fn launches(&self) -> Vec<serde_json::Value> {
            std::fs::read_to_string(self.dir.path().join("started.log"))
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }

        /// Уже готовые события адаптера, не дожидаясь новых.
        async fn queued(&mut self) -> Vec<AdapterEvent> {
            let mut events = Vec::new();
            while let Ok(event) =
                tokio::time::timeout(Duration::from_millis(200), self.adapter.next_event()).await
            {
                events.push(event);
            }
            events
        }

        fn servers(launch: &serde_json::Value) -> Vec<String> {
            launch["servers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|name| name.as_str().unwrap().to_owned())
                .collect()
        }
    }

    #[tokio::test]
    async fn owner_started_session_is_unchanged_and_still_sees_the_owner_servers() {
        for mode in [InstructionMode::Clean, InstructionMode::Owner] {
            let mut f = Fixture::new(|_| {});
            f.start(mode, false).await.unwrap();
            let launches = f.launches();
            assert_eq!(launches.len(), 1);
            assert_eq!(
                Fixture::servers(&launches[0]),
                ["canterlot", "nanokvm", "other"],
                "{mode:?}"
            );
            let args = launches[0]["args"].as_array().unwrap();
            assert!(!args.iter().any(|arg| arg == "apps" || arg == "plugins"));
            if mode == InstructionMode::Owner {
                assert_eq!(
                    launches[0]["home"].as_str().unwrap(),
                    f.owner.to_str().unwrap()
                );
            }
        }
    }

    #[tokio::test]
    async fn delegated_session_starts_only_canterlot_in_every_instruction_mode() {
        for (mode, suffix) in [
            (InstructionMode::Clean, "codex-home-isolated"),
            (InstructionMode::Preset, "codex-home-isolated"),
            (InstructionMode::Owner, "codex-home-isolated-owner"),
        ] {
            let mut f = Fixture::new(|_| {});
            f.start(mode, true).await.unwrap();
            let launches = f.launches();
            assert_eq!(launches.len(), 1, "{mode:?}");
            assert_eq!(Fixture::servers(&launches[0]), ["canterlot"], "{mode:?}");
            assert!(
                launches[0]["home"].as_str().unwrap().ends_with(suffix),
                "{mode:?}: {}",
                launches[0]["home"]
            );
            let args: Vec<_> = launches[0]["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| arg.as_str().unwrap())
                .collect();
            for feature in ["apps", "plugins"] {
                assert!(
                    args.windows(2).any(|pair| pair == ["--disable", feature]),
                    "{mode:?}: {feature}"
                );
            }
            // Файл владельца на месте и не изменён.
            assert!(
                std::fs::read_to_string(f.owner.join("config.toml"))
                    .unwrap()
                    .contains("nanokvm")
            );
        }
    }

    #[tokio::test]
    async fn start_fails_closed_when_the_cli_would_still_load_a_foreign_feature() {
        let mut f = Fixture::new(|dir| std::fs::write(dir.join("ignore-disable"), "1").unwrap());
        let error = f.start(InstructionMode::Clean, true).await.unwrap_err();
        assert_eq!(error, "mcp_isolation_failed");
        assert!(
            f.launches().is_empty(),
            "no thread was started, so no server was launched"
        );
    }

    #[tokio::test]
    async fn isolation_survives_the_restart_for_an_instruction_change() {
        let mut f = Fixture::new(|_| {});
        f.start(InstructionMode::Clean, true).await.unwrap();
        for next in [InstructionMode::Preset, InstructionMode::Owner] {
            let change = SettingsChange {
                instructions: Some(instructions(next)),
                ..Default::default()
            };
            let input = Transmission {
                command_id: Uuid::now_v7(),
                attempt: 1,
                text: "go".into(),
                files: vec![],
                settings: Some(change),
            };
            let outcome = tokio::time::timeout(WAIT, f.adapter.transmit(&input, &f.group))
                .await
                .expect("transmit deadline");
            assert!(
                matches!(outcome, TransmitOutcome::Accepted(_)),
                "{outcome:?}"
            );
            loop {
                let event = tokio::time::timeout(WAIT, f.adapter.next_event())
                    .await
                    .expect("event deadline");
                if matches!(event, AdapterEvent::RunFinished { .. }) {
                    break;
                }
            }
        }
        let launches = f.launches();
        assert!(launches.len() >= 3, "{launches:?}");
        for launch in &launches {
            assert_eq!(Fixture::servers(launch), ["canterlot"], "{launch}");
        }
    }

    const FAKE_KEY: &str = "ctx7-FAKE-not-a-real-key";

    fn context7_config() -> String {
        format!(
            "model = \"x\"\n\n[mcp_servers.nanokvm]\ncommand = \"/bin/true\"\n\n\
             [mcp_servers.context7]\nurl = \"https://mcp.context7.example/mcp\"\n\n\
             [mcp_servers.context7.http_headers]\nAuthorization = \"Bearer {FAKE_KEY}\"\n"
        )
    }

    fn missing(events: &[AdapterEvent]) -> Vec<Vec<String>> {
        events
            .iter()
            .filter_map(|event| match event {
                AdapterEvent::Diagnostic {
                    kind: "delegate_mcp_missing",
                    required: false,
                    field_names,
                    ..
                } => Some(field_names.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn delegated_session_also_starts_the_allowed_owner_server_and_nothing_else() {
        for mode in [InstructionMode::Clean, InstructionMode::Owner] {
            let config = context7_config();
            let mut f = Fixture::with_delegates(|_| {}, Some(&config), &["context7"]);
            f.start(mode, true).await.unwrap();
            let launches = f.launches();
            assert_eq!(launches.len(), 1, "{mode:?}");
            assert_eq!(
                Fixture::servers(&launches[0]),
                ["canterlot", "context7"],
                "{mode:?}"
            );
            let events = f.queued().await;
            assert!(missing(&events).is_empty(), "{events:?}");
            assert!(!format!("{events:?}").contains(FAKE_KEY));
        }
    }

    #[tokio::test]
    async fn owner_started_session_ignores_the_allow_list() {
        let config = context7_config();
        let mut f = Fixture::with_delegates(|_| {}, Some(&config), &["context7"]);
        f.start(InstructionMode::Clean, false).await.unwrap();
        assert_eq!(
            Fixture::servers(&f.launches()[0]),
            ["canterlot", "context7", "nanokvm"]
        );
    }

    #[tokio::test]
    async fn a_missing_allowed_server_is_skipped_with_one_named_diagnostic() {
        let mut f = Fixture::with_delegates(|_| {}, None, &["context7"]);
        f.start(InstructionMode::Clean, true).await.unwrap();
        assert_eq!(Fixture::servers(&f.launches()[0]), ["canterlot"]);
        let events = f.queued().await;
        assert_eq!(missing(&events), [vec!["context7".to_owned()]]);
        let debug = format!("{events:?}");
        assert!(!debug.contains("nanokvm"), "{debug}");
    }
}
