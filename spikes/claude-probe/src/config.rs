//! Конфигурация пробы: путь к CLI, модель, каталоги, окружение и базовые флаги.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::mcp_server::{SERVER_NAME, TOOL_NAME};

pub const SCRATCH_ROOT: &str = "/tmp/canterlot-stage0/claude-code";

#[derive(Debug, Clone)]
pub struct Config {
    pub claude: PathBuf,
    pub model: String,
    pub permission_mode: String,
    pub effort: Option<String>,
    pub evidence_dir: PathBuf,
    pub scratch_root: PathBuf,
    pub self_exe: PathBuf,
}

/// Ключи окружения, которые передаются дочернему CLI (остальное очищается).
pub const PASS_THROUGH_KEYS: &[&str] = &["HOME", "USER", "LANG", "TERM"];

/// Документированные переменные, которые проба выставляет сама.
pub const FIXED_ENV: &[(&str, &str)] = &[
    ("DISABLE_AUTOUPDATER", "1"),
    ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
];

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let home = std::env::var("HOME")?;
        let claude = std::env::var_os("CANTERLOT_CLAUDE")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(&home).join(".local/bin/claude"));
        let evidence_dir = std::env::var_os("CANTERLOT_EVIDENCE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/stage0/evidence/claude-code")
            });
        Ok(Self {
            claude,
            model: std::env::var("CANTERLOT_MODEL").unwrap_or_else(|_| "haiku".to_owned()),
            permission_mode: std::env::var("CANTERLOT_PERMISSION_MODE")
                .unwrap_or_else(|_| "manual".to_owned()),
            effort: Some(std::env::var("CANTERLOT_EFFORT").unwrap_or_else(|_| "low".to_owned()))
                .filter(|e| !e.is_empty()),
            evidence_dir,
            scratch_root: PathBuf::from(SCRATCH_ROOT),
            self_exe: std::env::current_exe()?,
        })
    }

    /// PATH ребёнка: каталог CLI и системные каталоги, без пользовательских добавок.
    pub fn child_path(&self) -> String {
        let cli_dir = self
            .claude
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("{cli_dir}:/usr/local/bin:/usr/bin:/bin")
    }

    pub fn child_env(&self, extra: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = PASS_THROUGH_KEYS
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
            .collect();
        if !env.iter().any(|(k, _)| k == "LANG") {
            env.push(("LANG".to_owned(), "C.UTF-8".to_owned()));
        }
        if !env.iter().any(|(k, _)| k == "TERM") {
            env.push(("TERM".to_owned(), "dumb".to_owned()));
        }
        env.push(("PATH".to_owned(), self.child_path()));
        for (k, v) in FIXED_ENV.iter().chain(extra.iter()) {
            env.retain(|(ek, _)| ek != k);
            env.push(((*k).to_owned(), (*v).to_owned()));
        }
        env
    }

    pub fn permission_tool(&self) -> String {
        format!("mcp__{SERVER_NAME}__{TOOL_NAME}")
    }

    pub fn mcp_config(&self, socket: &Path) -> Value {
        json!({"mcpServers": {SERVER_NAME: {
            "type": "stdio",
            "command": self.self_exe,
            "args": ["mcp-permission", "--socket", socket],
            "env": {},
        }}})
    }

    /// Базовые флаги долгоживущего stream-json процесса.
    pub fn base_args(&self, mcp_config_path: &Path) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--replay-user-messages",
            "--setting-sources",
            "project",
            "--strict-mcp-config",
            "--mcp-config",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        args.push(mcp_config_path.to_string_lossy().into_owned());
        args.push("--permission-prompt-tool".to_owned());
        args.push(self.permission_tool());
        args.push("--permission-mode".to_owned());
        args.push(self.permission_mode.clone());
        args.push("--model".to_owned());
        args.push(self.model.clone());
        if let Some(effort) = &self.effort {
            args.push("--effort".to_owned());
            args.push(effort.clone());
        }
        args
    }
}

/// Проверка изоляции по `system/init`: только наш MCP-сервер, ни одного плагина,
/// ни одного пользовательского навыка/команды и ни одного чужого MCP-инструмента.
pub fn check_isolation(init: &Value, user_skill_names: &[String]) -> (bool, Value) {
    let mut problems: Vec<String> = Vec::new();
    let servers = init["mcp_servers"].as_array().cloned().unwrap_or_default();
    for s in &servers {
        let name = s["name"].as_str().unwrap_or_default();
        if name != SERVER_NAME {
            problems.push(format!("foreign mcp server: {name}"));
        }
    }
    if !servers
        .iter()
        .any(|s| s["name"] == SERVER_NAME && s["status"] == "connected")
    {
        problems.push("own permission server not connected".to_owned());
    }
    // Встроенные в CLI плагины (path "builtin") допустимы; пользовательские нет.
    for p in init["plugins"].as_array().into_iter().flatten() {
        let builtin = p["path"] == "builtin"
            || p["source"]
                .as_str()
                .is_some_and(|s| s.ends_with("@builtin"));
        if !builtin {
            problems.push(format!("non-builtin plugin: {}", p["name"]));
        }
    }
    let own_prefix = format!("mcp__{SERVER_NAME}__");
    for t in init["tools"].as_array().into_iter().flatten() {
        let t = t.as_str().unwrap_or_default();
        if t.starts_with("mcp__") && !t.starts_with(&own_prefix) {
            problems.push(format!("foreign mcp tool: {t}"));
        }
    }
    for key in ["skills", "slash_commands", "agents"] {
        for item in init[key].as_array().into_iter().flatten() {
            let item = item.as_str().unwrap_or_default();
            if item.contains(':') {
                problems.push(format!("plugin-scoped {key}: {item}"));
            }
            if user_skill_names.iter().any(|u| u == item) {
                problems.push(format!("user {key}: {item}"));
            }
        }
    }
    let details = json!({
        "mcp_servers": init["mcp_servers"],
        "plugins": init["plugins"],
        "tools": init["tools"],
        "skills": init["skills"],
        "slash_commands": init["slash_commands"],
        "agents": init["agents"],
        "permissionMode": init["permissionMode"],
        "model": init["model"],
        "apiKeySource": init["apiKeySource"],
        "capabilities": init["capabilities"],
        "problems": problems,
    });
    (problems.is_empty(), details)
}

/// Имена пользовательских навыков (только имена каталогов, без чтения содержимого).
pub fn user_skill_names() -> Vec<String> {
    let Ok(home) = std::env::var("HOME") else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for sub in [".claude/skills", ".claude/commands", ".claude/agents"] {
        if let Ok(rd) = std::fs::read_dir(Path::new(&home).join(sub)) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                names.push(n.trim_end_matches(".md").to_owned());
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean_init() -> Value {
        json!({
            "type": "system", "subtype": "init",
            "mcp_servers": [{"name": "canterlot", "status": "connected"}],
            "plugins": [{"name": "cc-plugin-agents-md", "path": "builtin", "source": "cc-plugin-agents-md@builtin"}],
            "tools": ["Bash", "Read", "mcp__canterlot__approve"],
            "skills": ["simplify"],
            "slash_commands": ["compact", "simplify"],
        })
    }

    #[test]
    fn clean_init_passes() {
        let (ok, details) = check_isolation(&clean_init(), &["my-skill".to_owned()]);
        assert!(ok, "{details}");
    }

    #[test]
    fn foreign_servers_plugins_tools_and_user_skills_fail() {
        let mut init = clean_init();
        init["mcp_servers"] = json!([{"name": "canterlot", "status": "connected"}, {"name": "taiga", "status": "connected"}]);
        init["plugins"] = json!([{"name": "superpowers", "path": "/x"}]);
        init["tools"] = json!(["Bash", "mcp__taiga__list"]);
        init["skills"] = json!(["my-skill", "superpowers:brainstorming"]);
        let (ok, details) = check_isolation(&init, &["my-skill".to_owned()]);
        assert!(!ok);
        assert_eq!(details["problems"].as_array().map(Vec::len), Some(5));
    }

    #[test]
    fn missing_own_server_fails() {
        let mut init = clean_init();
        init["mcp_servers"] = json!([{"name": "canterlot", "status": "failed"}]);
        let (ok, _) = check_isolation(&init, &[]);
        assert!(!ok);
    }

    #[test]
    fn child_env_is_allowlist_plus_fixed() {
        let cfg = Config {
            claude: PathBuf::from("/opt/cli/claude"),
            model: "haiku".into(),
            permission_mode: "manual".into(),
            effort: None,
            evidence_dir: PathBuf::from("/tmp/e"),
            scratch_root: PathBuf::from("/tmp/s"),
            self_exe: PathBuf::from("/x/probe"),
        };
        let env = cfg.child_env(&[("HTTPS_PROXY", "http://127.0.0.1:9")]);
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        for k in &keys {
            assert!(
                PASS_THROUGH_KEYS.contains(k)
                    || *k == "PATH"
                    || FIXED_ENV.iter().any(|(f, _)| f == k)
                    || *k == "HTTPS_PROXY",
                "unexpected key {k}"
            );
        }
        assert!(env.contains(&(
            "PATH".to_owned(),
            "/opt/cli:/usr/local/bin:/usr/bin:/bin".to_owned()
        )));
        assert!(
            !keys.iter().any(|k| k.starts_with("CLAUDE_CODE_")
                && *k != "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC")
        );
    }
}
