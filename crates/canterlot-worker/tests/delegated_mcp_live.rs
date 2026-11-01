#![forbid(unsafe_code)]

//! D24 на настоящих бинарниках Claude Code и Codex. Конфигурация владельца поддельная: каталог
//! в `TempDir` с сервером `nanokvm`, который при запуске создаёт файл-метку. Настоящие `~/.claude`
//! и `~/.codex` не открываются, входа в аккаунт нет, платных запросов нет. Тесты не входят в
//! check.sh, запуск:
//!
//! ```text
//! cargo test -p canterlot-worker --test delegated_mcp_live -- --ignored --test-threads=1
//! ```
//!
//! Бинарники берутся из `CANTERLOT_CLAUDE_BIN` и `CANTERLOT_CODEX_BIN`, иначе `claude` и `codex`
//! из PATH. OpenCode покрывает `opencode_live.rs`.

use canterlot_worker::adapter::{
    InstructionMode, InstructionSpec,
    claude_code::process::{LaunchSpec, SessionArg},
    codex::{
        home::{CodexHome, TempDirPolicy},
        process::{AppServerProcess, LaunchSpec as CodexLaunch, StdoutLine},
    },
};
use canterlot_worker::config::{ClaudeCodeConfig, CodexConfig};
use canterlot_worker::containment::TreeContainment;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tempfile::TempDir;
use tokio::time::sleep;
use uuid::Uuid;

fn marker_server(root: &Path, name: &str) -> String {
    format!("touch '{}/marker-{name}'; sleep 60", root.display())
}

// Список статусов Codex ждёт запуска каждого сервера: у двойника процесс сразу завершается.
fn short_marker_server(root: &Path, name: &str) -> String {
    format!("touch '{}/marker-{name}'", root.display())
}

fn started(root: &Path, name: &str) -> bool {
    root.join(format!("marker-{name}")).exists()
}

fn binary(variable: &str, default: &str) -> String {
    std::env::var(variable).unwrap_or_else(|_| default.into())
}

// --- Claude Code ---

fn claude_command(binary: &str, home: &Path, workdir: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(binary);
    command
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("DISABLE_AUTOUPDATER", "1")
        .current_dir(workdir);
    command
}

fn claude_home(root: &Path) -> (PathBuf, PathBuf) {
    let home = root.join("home");
    let workdir = root.join("work");
    let marketplace = root.join("marketplace");
    for dir in [
        home.join(".claude"),
        workdir.clone(),
        marketplace.join(".claude-plugin"),
        marketplace.join("kvmplug/.claude-plugin"),
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    // Сервер уровня пользователя, проекта и плагина.
    let server = |name: &str| json!({"type": "stdio", "command": "sh", "args": ["-c", marker_server(root, name)]});
    fs::write(
        home.join(".claude.json"),
        json!({"hasCompletedOnboarding": true, "mcpServers": {"nanokvm": server("user")}})
            .to_string(),
    )
    .unwrap();
    fs::write(
        workdir.join(".mcp.json"),
        json!({"mcpServers": {"projkvm": server("project")}}).to_string(),
    )
    .unwrap();
    fs::write(
        marketplace.join(".claude-plugin/marketplace.json"),
        json!({"name": "mk", "owner": {"name": "t"}, "plugins": [{"name": "kvmplug", "source": "./kvmplug", "description": "t"}]}).to_string(),
    )
    .unwrap();
    fs::write(
        marketplace.join("kvmplug/.claude-plugin/plugin.json"),
        json!({"name": "kvmplug", "version": "1.0.0", "description": "t"}).to_string(),
    )
    .unwrap();
    fs::write(
        marketplace.join("kvmplug/.mcp.json"),
        json!({"mcpServers": {"plugkvm": server("plugin")}}).to_string(),
    )
    .unwrap();
    (home, workdir)
}

#[tokio::test]
#[ignore = "L2: настоящий claude, см. заголовок файла"]
async fn claude_delegated_session_starts_no_owner_user_project_or_plugin_server() {
    let claude = binary("CANTERLOT_CLAUDE_BIN", "claude");
    for isolate_mcp in [false, true] {
        let root = TempDir::new().unwrap();
        let (home, workdir) = claude_home(root.path());
        for args in [
            vec!["plugin", "marketplace", "add"],
            vec!["plugin", "install", "kvmplug@mk"],
        ] {
            let mut command = claude_command(&claude, &home, &workdir);
            command.args(&args);
            if args[1] == "marketplace" {
                command.arg(root.path().join("marketplace"));
            }
            let status = command
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "{args:?}");
        }
        let exec = root.path().join("exec");
        let instructions = InstructionSpec {
            mode: InstructionMode::Clean,
            text: String::new(),
            preset_id: None,
            revision: 0,
            title: String::new(),
        };
        let spec = LaunchSpec {
            executable: Path::new(&claude),
            exec_dir: &exec,
            workdir: &workdir,
            session: SessionArg::New(Uuid::now_v7()),
            model: "haiku",
            effort: "low",
            permission_mode: "bypass",
            instructions: &instructions,
            isolate_mcp,
        };
        let cfg = ClaudeCodeConfig::default();
        canterlot_worker::adapter::claude_code::process::write_exec_files(
            &spec,
            &cfg,
            Path::new("/bin/true"),
        )
        .unwrap();
        // Те же аргументы и окружение, что у воркера; входа в аккаунт нет.
        let args =
            canterlot_worker::adapter::claude_code::process::build_args(&spec, &cfg).unwrap();
        let mut env = canterlot_worker::adapter::claude_code::process::build_env(
            [("PATH".into(), std::env::var_os("PATH").unwrap_or_default())].into_iter(),
        );
        if isolate_mcp {
            canterlot_worker::adapter::claude_code::process::apply_mcp_isolation(&mut env);
        }
        let mut command = std::process::Command::new(&claude);
        command
            .env_clear()
            .envs(env)
            .env("HOME", &home)
            .args(args)
            .current_dir(&workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        sleep(Duration::from_secs(6)).await;
        let _ = child.kill();
        let _ = child.wait();
        for name in ["user", "project", "plugin"] {
            assert_eq!(
                started(root.path(), name),
                !isolate_mcp,
                "isolate_mcp={isolate_mcp}: {name} server"
            );
        }
    }
}

// --- Codex ---

async fn codex_status(
    process: &mut AppServerProcess,
    workdir: &Path,
) -> (Vec<String>, Vec<String>) {
    let send = |id: i64, method: &str, params: Value| json!({"id": id, "method": method, "params": params});
    process
        .write_message(&send(
            1,
            "initialize",
            json!({"clientInfo": {"name": "canterlot-test", "version": "0"}}),
        ))
        .await
        .unwrap();
    process
        .write_message(&json!({"method": "initialized"}))
        .await
        .unwrap();
    process.flush_out().await.unwrap();
    process
        .write_message(&send(
            2,
            "config/read",
            json!({"cwd": workdir.to_string_lossy(), "includeLayers": false}),
        ))
        .await
        .unwrap();
    process
        .write_message(&send(3, "mcpServerStatus/list", json!({})))
        .await
        .unwrap();
    process.flush_out().await.unwrap();
    let (mut configured, mut listed) = (vec![], vec![]);
    let (mut have_config, mut have_list) = (false, false);
    while !(have_config && have_list) {
        let line = tokio::time::timeout(Duration::from_secs(30), process.next_line())
            .await
            .expect("app-server answer");
        let StdoutLine::Line(line) = line else {
            panic!("app-server exited");
        };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let names = |value: &Value| -> Vec<String> {
            let mut names: Vec<String> = match value {
                Value::Object(map) => map.keys().cloned().collect(),
                Value::Array(items) => items
                    .iter()
                    .filter_map(|item| item["name"].as_str().map(str::to_owned))
                    .collect(),
                _ => vec![],
            };
            names.sort();
            names
        };
        match value["id"].as_i64() {
            Some(2) => {
                configured = names(&value["result"]["config"]["mcp_servers"]);
                have_config = true;
            }
            Some(3) => {
                listed = names(&value["result"]["data"]);
                have_list = true;
            }
            _ => {}
        }
    }
    (configured, listed)
}

#[tokio::test]
#[ignore = "L2: настоящий codex, см. заголовок файла"]
async fn codex_delegated_session_lists_only_canterlot() {
    let codex = binary("CANTERLOT_CODEX_BIN", "codex");
    for isolate_mcp in [false, true] {
        let root = TempDir::new().unwrap();
        let owner = root.path().join("owner-home");
        let workdir = root.path().join("work");
        let exec_dir = root.path().join("data/executions/one");
        let plugin = owner.join("plugins/cache/mk/kvmplug/1.0.0");
        for dir in [&workdir, &exec_dir, &plugin.join(".codex-plugin")] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(
            owner.join("config.toml"),
            format!(
                "[mcp_servers.nanokvm]\ncommand = \"sh\"\nargs = [\"-c\", {:?}]\n\n[plugins.\"kvmplug@mk\"]\nenabled = true\n",
                short_marker_server(root.path(), "user")
            ),
        )
        .unwrap();
        fs::write(
            plugin.join(".codex-plugin/plugin.json"),
            json!({"name": "kvmplug", "version": "1.0.0", "description": "t", "mcpServers": "./.mcp.json"}).to_string(),
        )
        .unwrap();
        fs::write(
            plugin.join(".mcp.json"),
            json!({"mcpServers": {"plugkvm": {"command": "sh", "args": ["-c", short_marker_server(root.path(), "plugin")]}}}).to_string(),
        )
        .unwrap();
        let cfg = CodexConfig {
            executable: PathBuf::from(&codex),
            home_dir: Some(root.path().join("data/codex-home")),
            owner_home: Some(owner.clone()),
            home_policy: TempDirPolicy::Allow,
            ..Default::default()
        };
        let env: Vec<(OsString, OsString)> = vec![];
        let mut home = CodexHome::resolve(&cfg, &exec_dir, &env).unwrap();
        if isolate_mcp {
            home = home.isolated(false);
        }
        home.prepare("block", TempDirPolicy::Allow).unwrap();
        let launch = CodexLaunch {
            executable: Path::new(&codex),
            exec_dir: &exec_dir,
            workdir: &workdir,
            relay_exe: Path::new("/bin/true"),
        };
        let group = TreeContainment::new().unwrap();
        let mut process =
            AppServerProcess::spawn(&launch, &group, InstructionMode::Clean, Some(&home)).unwrap();
        let (configured, listed) = codex_status(&mut process, &workdir).await;
        sleep(Duration::from_secs(2)).await;
        process.close_stdin();
        let _ = tokio::time::timeout(Duration::from_secs(5), process.wait()).await;
        if isolate_mcp {
            assert_eq!(configured, ["canterlot"]);
            assert_eq!(listed, ["canterlot"]);
        } else {
            assert!(configured.contains(&"nanokvm".to_owned()), "{configured:?}");
            assert!(listed.contains(&"nanokvm".to_owned()), "{listed:?}");
        }
        for name in ["user", "plugin"] {
            assert_eq!(
                started(root.path(), name),
                !isolate_mcp,
                "isolate_mcp={isolate_mcp}: {name} server"
            );
        }
    }
}
