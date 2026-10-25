//! Изоляция дочернего `codex app-server` от глобальной конфигурации владельца.
//!
//! Глобальный `~/.codex/config.toml` разрешает всё (approval never, full access),
//! подключает MCP (в т.ч. nanokvm с физическим KVM) и плагины. Каждый запуск
//! переопределяет эти ключи через `-c`/`--disable` и проверяется через `config/read`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use serde_json::{Value, json};

pub const CODEX_BIN: &str = "codex";
pub const MODEL: &str = "gpt-6-luna";
pub const EFFORT: &str = "low";
pub const APPROVAL_POLICY: &str = "on-request";
pub const SANDBOX_MODE: &str = "read-only";
pub const SCRATCH_ROOT: &str = "/tmp/canterlot-stage0/codex";

/// Переменные окружения, которые получает дочерний процесс (остальное очищается).
pub const ENV_ALLOWLIST: &[&str] = &["HOME", "USER", "LANG", "TERM", "PATH"];
pub const CHILD_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Сконфигурированные у владельца MCP-серверы и плагины, которые выключаются явно.
pub const KNOWN_MCP_SERVERS: &[&str] = &["nanokvm", "openaiDeveloperDocs"];
pub const KNOWN_PLUGINS: &[&str] = &["antislop@anti-slop"];

/// Фичи, отключаемые для дочернего процесса (`--disable`).
pub const DISABLED_FEATURES: &[&str] = &[
    "plugins",
    "remote_plugin",
    "apps",
    "memories",
    "multi_agent",
    "goals",
    "hooks",
    "computer_use",
    "browser_use",
    "browser_use_external",
    "in_app_browser",
    "image_generation",
    "tool_suggest",
    "skill_mcp_dependency_install",
];

/// Префиксы, которые пользовательский `~/.codex/rules/default.rules` разрешает
/// выполнять вне песочницы без вопроса; проектный слой их запрещает.
pub const GUARD_RULES: &str = r#"# canterlot stage0 guard: forbid prefixes the user layer allows outside the sandbox
prefix_rule(pattern = ["uv"], decision = "forbidden", justification = "canterlot probe guard")
prefix_rule(pattern = ["npm"], decision = "forbidden", justification = "canterlot probe guard")
prefix_rule(pattern = ["sudo"], decision = "forbidden", justification = "canterlot probe guard")
prefix_rule(pattern = ["ssh"], decision = "forbidden", justification = "canterlot probe guard")
prefix_rule(pattern = ["scp"], decision = "forbidden", justification = "canterlot probe guard")
prefix_rule(pattern = ["curl"], decision = "forbidden", justification = "canterlot probe guard")
prefix_rule(pattern = ["codex"], decision = "forbidden", justification = "canterlot probe guard")
prefix_rule(pattern = ["git", ["add", "commit", "push"]], decision = "forbidden", justification = "canterlot probe guard")
"#;

/// Создаёт свежий scratch-каталог сценария с `git init` и guard-правилами.
pub fn fresh_scratch(scenario: &str) -> anyhow::Result<PathBuf> {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let dir = PathBuf::from(SCRATCH_ROOT)
        .join(scenario)
        .join(format!("run-{ts}"));
    let s = dir.to_string_lossy();
    // путь становится сегментом dotted-path в `-c projects.<path>.trust_level`
    if s.contains('.') || s.contains('"') {
        bail!("scratch path must not contain dots or quotes: {s}");
    }
    std::fs::create_dir_all(dir.join(".codex/rules"))?;
    std::fs::write(dir.join(".codex/rules/canterlot-guard.rules"), GUARD_RULES)?;
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .env_clear()
        .env("PATH", CHILD_PATH)
        .status()
        .context("git init")?;
    if !status.success() {
        bail!("git init failed in {s}");
    }
    Ok(dir)
}

/// Значения окружения для дочернего процесса из allowlist.
pub fn child_env() -> Vec<(String, String)> {
    let mut env = Vec::new();
    for key in ENV_ALLOWLIST {
        let val = match *key {
            "PATH" => Some(CHILD_PATH.to_string()),
            "LANG" => Some("C.UTF-8".to_string()),
            "TERM" => Some("dumb".to_string()),
            other => std::env::var(other).ok(),
        };
        if let Some(v) = val {
            env.push(((*key).to_string(), v));
        }
    }
    env
}

/// Аргументы `codex app-server` с изолирующими переопределениями.
pub fn app_server_args(scratch: &Path, extra: &[String]) -> Vec<String> {
    let mut a: Vec<String> = vec!["app-server".into(), "--listen".into(), "stdio://".into()];
    let mut c = |kv: String| {
        a.push("-c".into());
        a.push(kv);
    };
    c(format!("approval_policy=\"{APPROVAL_POLICY}\""));
    c(format!("sandbox_mode=\"{SANDBOX_MODE}\""));
    c("sandbox_workspace_write.network_access=false".into());
    c("sandbox_workspace_write.writable_roots=[]".into());
    c(format!("model=\"{MODEL}\""));
    c(format!("model_reasoning_effort=\"{EFFORT}\""));
    for s in KNOWN_MCP_SERVERS {
        c(format!("mcp_servers.{s}.enabled=false"));
    }
    for p in KNOWN_PLUGINS {
        c(format!("plugins.{p}.enabled=false"));
    }
    c("web_search=\"disabled\"".into());
    c("project_doc_max_bytes=0".into());
    c("check_for_update_on_startup=false".into());
    c("analytics.enabled=false".into());
    c("history.persistence=\"none\"".into());
    c(format!(
        "projects.{}.trust_level=\"trusted\"",
        scratch.to_string_lossy()
    ));
    for f in DISABLED_FEATURES {
        a.push("--disable".into());
        a.push((*f).to_string());
    }
    a.extend(extra.iter().cloned());
    a
}

/// Параметры `thread/start`, дублирующие изоляцию на уровне треда.
pub fn thread_start_params(scratch: &Path, ephemeral: bool) -> Value {
    json!({
        "cwd": scratch.to_string_lossy(),
        "model": MODEL,
        "approvalPolicy": APPROVAL_POLICY,
        "approvalsReviewer": "user",
        "sandbox": SANDBOX_MODE,
        "ephemeral": ephemeral,
        "serviceName": "canterlot_stage0_probe",
    })
}

/// Проверяет эффективную конфигурацию из `config/read` и возвращает сводку без секретов.
pub fn check_effective_config(read: &Value, scratch: &Path) -> anyhow::Result<Value> {
    let cfg = &read["config"];
    let mut problems = Vec::new();
    let expect = |key: &str, want: &str, problems: &mut Vec<String>| {
        if cfg[key].as_str() != Some(want) {
            problems.push(format!("{key}={} (want {want})", cfg[key]));
        }
    };
    expect("approval_policy", APPROVAL_POLICY, &mut problems);
    expect("sandbox_mode", SANDBOX_MODE, &mut problems);
    expect("model", MODEL, &mut problems);
    expect("model_reasoning_effort", EFFORT, &mut problems);
    expect("web_search", "disabled", &mut problems);

    let mut mcp = serde_json::Map::new();
    if let Some(servers) = cfg["mcp_servers"].as_object() {
        for (name, s) in servers {
            let enabled = s["enabled"].as_bool().unwrap_or(true);
            if enabled {
                problems.push(format!("mcp server {name} enabled"));
            }
            mcp.insert(name.clone(), json!({ "enabled": enabled }));
        }
    }
    let mut plugins = serde_json::Map::new();
    if let Some(ps) = cfg["plugins"].as_object() {
        for (name, p) in ps {
            let enabled = p["enabled"].as_bool().unwrap_or(true);
            if enabled {
                problems.push(format!("plugin {name} enabled"));
            }
            plugins.insert(name.clone(), json!({ "enabled": enabled }));
        }
    }
    let mut features = serde_json::Map::new();
    for f in DISABLED_FEATURES {
        let v = cfg["features"][*f].as_bool();
        if v != Some(false) {
            problems.push(format!("feature {f}={v:?}"));
        }
        features.insert((*f).to_string(), json!(v));
    }
    let scratch_s = scratch.to_string_lossy().to_string();
    let mut project_layer = Value::Null;
    if let Some(layers) = read["layers"].as_array() {
        for l in layers {
            if l["name"]["type"] == "project" {
                project_layer = json!({
                    "dotCodexFolder": l["name"]["dotCodexFolder"],
                    "disabledReason": l.get("disabledReason").cloned().unwrap_or(Value::Null),
                });
            }
        }
    }
    let project_ok = project_layer["dotCodexFolder"]
        .as_str()
        .is_some_and(|p| p.starts_with(&scratch_s))
        && project_layer["disabledReason"].is_null();
    if !project_ok {
        problems.push(format!(
            "project layer with guard rules not active: {project_layer}"
        ));
    }
    let layer_names: Vec<Value> = read["layers"]
        .as_array()
        .map(|ls| ls.iter().map(|l| l["name"].clone()).collect())
        .unwrap_or_default();

    let summary = json!({
        "approval_policy": cfg["approval_policy"],
        "sandbox_mode": cfg["sandbox_mode"],
        "sandbox_workspace_write": cfg["sandbox_workspace_write"],
        "model": cfg["model"],
        "model_reasoning_effort": cfg["model_reasoning_effort"],
        "web_search": cfg["web_search"],
        "project_doc_max_bytes": cfg["project_doc_max_bytes"],
        "mcp_servers": mcp,
        "plugins": plugins,
        "disabled_features": features,
        "project_layer": project_layer,
        "layers": layer_names,
        "problems": problems,
    });
    if !problems.is_empty() {
        bail!("isolation check failed: {summary}");
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_disable_every_known_server_and_plugin() {
        let args = app_server_args(Path::new("/tmp/canterlot-stage0/codex/x/run-1"), &[]);
        let joined = args.join(" ");
        assert!(joined.contains("mcp_servers.nanokvm.enabled=false"));
        assert!(joined.contains("plugins.antislop@anti-slop.enabled=false"));
        assert!(
            joined.contains("projects./tmp/canterlot-stage0/codex/x/run-1.trust_level=\"trusted\"")
        );
        assert!(!joined.contains("danger"));
        assert!(!joined.contains("ws://"));
    }

    #[test]
    fn effective_config_check_flags_enabled_mcp() {
        let scratch = Path::new("/tmp/canterlot-stage0/codex/x/run-1");
        let mut features = serde_json::Map::new();
        for f in DISABLED_FEATURES {
            features.insert((*f).to_string(), json!(false));
        }
        let mut read = json!({
            "config": {
                "approval_policy": "on-request", "sandbox_mode": "read-only",
                "model": MODEL, "model_reasoning_effort": EFFORT, "web_search": "disabled",
                "mcp_servers": {"nanokvm": {"enabled": false, "url": "http://x"}},
                "plugins": {"antislop@anti-slop": {"enabled": false}},
                "features": features,
            },
            "layers": [{"name": {"type": "project", "dotCodexFolder": "/tmp/canterlot-stage0/codex/x/run-1/.codex"}}]
        });
        assert!(check_effective_config(&read, scratch).is_ok());
        read["config"]["mcp_servers"]["nanokvm"]["enabled"] = json!(true);
        assert!(check_effective_config(&read, scratch).is_err());
        read["config"]["mcp_servers"]["nanokvm"]["enabled"] = json!(false);
        read["layers"][0]["disabledReason"] = json!("untrusted");
        assert!(check_effective_config(&read, scratch).is_err());
    }
}
