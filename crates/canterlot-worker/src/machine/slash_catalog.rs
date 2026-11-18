//! Каталог команд и навыков, которые CLI реально может выполнить в сессии.
//! Разбор ответов проб, кэш по пути бинарника и версии, фоновые пробы без хода модели.
//! Ответы CLI не журналируются: из них берутся только имя, описание и подсказка аргумента.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

/// Вид записи каталога: slash-команда или навык.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    Command,
    Skill,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Skill => "skill",
        }
    }
}

/// Одна строка каталога для PWA.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    pub description: String,
    pub kind: EntryKind,
    pub argument_hint: String,
}

const MAX_NAME: usize = 64;
const MAX_DESCRIPTION: usize = 240;
const MAX_HINT: usize = 80;

fn cut(text: &str, limit: usize) -> String {
    let cut = text.floor_char_boundary(limit);
    let mut out = text[..cut].trim().to_owned();
    if cut < text.len() {
        out.push('…');
    }
    out
}

fn entry(name: &str, description: &str, kind: EntryKind, hint: &str) -> Option<CatalogEntry> {
    let name = name.trim().trim_start_matches('/');
    if name.is_empty() || name.len() > MAX_NAME || name.contains(char::is_whitespace) {
        return None;
    }
    Some(CatalogEntry {
        name: name.to_owned(),
        description: cut(description, MAX_DESCRIPTION),
        kind,
        argument_hint: cut(hint, MAX_HINT),
    })
}

/// Claude Code отдаёт kick `--version` одной строкой `control_response` в потоке stream-json;
/// `commands` содержит и команды, и навыки (их описание помечено `(user)`, `(project)` и т.п.).
pub fn parse_claude(output: &str) -> Vec<CatalogEntry> {
    let mut out = Vec::new();
    for line in output.lines() {
        let Ok(frame) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if frame.get("type").and_then(Value::as_str) != Some("control_response") {
            continue;
        }
        let commands = frame
            .pointer("/response/response/commands")
            .and_then(Value::as_array);
        let Some(commands) = commands else { continue };
        for command in commands {
            let Some(name) = command.get("name").and_then(Value::as_str) else {
                continue;
            };
            let description = command
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            let hint = command
                .get("argumentHint")
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(row) = entry(name, description, EntryKind::Command, hint) {
                out.push(row);
            }
        }
    }
    out
}

/// Codex `skills/list`: `data[].skills[]`, отключённые навыки в меню не попадают.
pub fn parse_codex(reply: &Value) -> Vec<CatalogEntry> {
    // JSON-RPC сырая строка несёт `result`, зонд адаптера отдаёт уже его содержимое.
    let reply = reply.get("result").unwrap_or(reply);
    let mut out = Vec::new();
    for group in reply.get("data").and_then(Value::as_array).into_iter().flatten() {
        for skill in group.get("skills").and_then(Value::as_array).into_iter().flatten() {
            if skill.get("enabled").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            let Some(name) = skill.get("name").and_then(Value::as_str) else {
                continue;
            };
            let description = skill
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(row) = entry(name, description, EntryKind::Skill, "") {
                out.push(row);
            }
        }
    }
    out
}

/// OpenCode `GET /api/command`: `{location, data: [{name, description}]}`.
pub fn parse_opencode_commands(reply: &Value) -> Vec<CatalogEntry> {
    parse_opencode(reply, EntryKind::Command)
}

/// OpenCode `GET /api/skill`: `{location, data: [{id, name, description, ...}]}`.
pub fn parse_opencode_skills(reply: &Value) -> Vec<CatalogEntry> {
    parse_opencode(reply, EntryKind::Skill)
}

/// Ключ кэша: канонический путь бинарника и его версия.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    pub path: String,
    pub version: String,
}

/// Кэш каталога: `entries: None` означает известный отказ пробы («каталога нет»).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cached {
    pub entries: Option<Vec<CatalogEntry>>,
    pub at_ms: u64,
}

/// Срок, через который известный отказ пробы можно повторить.
pub const FAILURE_TTL_MS: u64 = 60 * 60 * 1000;

/// Кэш в памяти процесса; общий файл obligatory для нескольких процессов воркера.
#[derive(Default)]
pub struct CatalogCache {
    memory: Mutex<HashMap<CacheKey, Cached>>,
}

/// Успех не стареет: смена версии меняет ключ. Отказ повторяем не раньше срока.
fn fresh(cached: &Cached, now_ms: u64) -> bool {
    match &cached.entries {
        Some(_) => true,
        None => now_ms.saturating_sub(cached.at_ms) < FAILURE_TTL_MS,
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct DiskRow {
    key: CacheKey,
    cached: Cached,
}

fn disk_read(path: &Path) -> Vec<DiskRow> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

impl CatalogCache {
    pub fn cached(&self, key: &CacheKey, now_ms: u64) -> Option<Option<Vec<CatalogEntry>>> {
        self.memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .filter(|cached| fresh(cached, now_ms))
            .map(|cached| cached.entries.clone())
    }

    pub fn remember(&self, key: &CacheKey, entries: Option<Vec<CatalogEntry>>, now_ms: u64) {
        self.memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                key.clone(),
                Cached {
                    entries,
                    at_ms: now_ms,
                },
            );
    }

    pub fn from_disk(path: &Path, key: &CacheKey, now_ms: u64) -> Option<Option<Vec<CatalogEntry>>> {
        disk_read(path)
            .into_iter()
            .find(|row| &row.key == key)
            .map(|row| row.cached)
            .filter(|cached| fresh(cached, now_ms))
            .map(|cached| cached.entries)
    }

    pub fn to_disk(path: &Path, key: &CacheKey, entries: Option<Vec<CatalogEntry>>, now_ms: u64) {
        let mut rows = disk_read(path);
        rows.retain(|row| &row.key != key);
        rows.push(DiskRow {
            key: key.clone(),
            cached: Cached {
                entries,
                at_ms: now_ms,
            },
        });
        let Ok(bytes) = serde_json::to_vec(&rows) else {
            return;
        };
        let temporary = path.with_extension(format!(
            "{}.{}.tmp",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        if std::fs::write(&temporary, bytes).is_ok() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
            let _ = std::fs::rename(&temporary, path);
        }
    }
}

/// Что и как запускать: флаги и окружение живут в адаптерах, здесь только выбор харнесса.
#[derive(Clone, Debug)]
pub enum Source {
    Claude {
        executable: PathBuf,
    },
    Codex {
        executable: PathBuf,
    },
    OpenCode {
        cfg: crate::config::OpenCodeConfig,
        data_dir: PathBuf,
    },
}

impl Source {
    pub fn harness(&self) -> &'static str {
        match self {
            Self::Claude { .. } => "claude_code",
            Self::Codex { .. } => "codex",
            Self::OpenCode { .. } => "opencode",
        }
    }

    fn executable(&self) -> &Path {
        match self {
            Self::Claude { executable } | Self::Codex { executable } => executable,
            Self::OpenCode { cfg, .. } => &cfg.executable,
        }
    }

    async fn run(&self) -> Result<Vec<CatalogEntry>, String> {
        match self {
            Self::Claude { executable } => {
                crate::adapter::claude_code::catalog::catalog_probe(executable).await
            }
            Self::Codex { executable } => {
                let reply = crate::adapter::codex::probe::probe(
                    executable,
                    "skills/list",
                    serde_json::json!({}),
                    Duration::from_secs(15),
                )
                .await
                .map_err(|error| format!("{error:?}"))?;
                Ok(parse_codex(&reply))
            }
            Self::OpenCode { cfg, data_dir } => {
                crate::adapter::opencode::catalog_probe(cfg, data_dir)
                    .await
                    .map_err(|_| "opencode_catalog".to_owned())
            }
        }
    }
}

/// Каталог из кэша или фоновой пробы. Отказ пробы — «каталога нет» (`None`) без ошибки.
pub async fn load(
    source: &Source,
    version: &str,
    cache: &CatalogCache,
    data_dir: &Path,
) -> Option<Vec<CatalogEntry>> {
    let path = crate::adapter::cli_version::resolve(source.executable())
        .unwrap_or_else(|| source.executable().to_owned());
    let key = CacheKey {
        path: path.to_string_lossy().into_owned(),
        version: version.to_owned(),
    };
    let now = crate::machine::now_ms();
    if let Some(found) = cache.cached(&key, now) {
        return found;
    }
    let disk = data_dir.join(".slash-catalog.json");
    if let Some(found) = CatalogCache::from_disk(&disk, &key, now) {
        cache.remember(&key, found.clone(), now);
        return found;
    }
    let result = source.run().await.ok();
    cache.remember(&key, result.clone(), now);
    CatalogCache::to_disk(&disk, &key, result.clone(), now);
    result
}

fn parse_opencode(reply: &Value, kind: EntryKind) -> Vec<CatalogEntry> {
    reply
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let name = row.get("name").and_then(Value::as_str)?;
            let description = row
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            entry(name, description, kind, "")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_parses_commands_from_the_recorded_control_response() {
        let raw = include_str!("catalog_fixtures/claude_initialize.ndjson");
        let entries = parse_claude(raw);
        assert_eq!(entries.len(), 5, "got {}", entries.len());
        assert!(entries.iter().all(|e| e.kind == EntryKind::Command));
        assert!(entries.iter().all(|e| !e.name.is_empty()));
        let first = entries
            .iter()
            .find(|e| e.name == "deploy-notes")
            .expect("fixture command");
        assert!(first.description.contains("Example"));
        assert!(first.description.contains("(user)"));
        assert_eq!(first.argument_hint, "");
    }

    #[test]
    fn claude_ignores_hook_noise_without_a_control_response() {
        let raw = "{\"type\":\"system\",\"subtype\":\"init\"}\n{\"type\":\"result\"}\n";
        assert!(parse_claude(raw).is_empty());
    }

    #[test]
    fn codex_parses_enabled_skills_and_drops_disabled() {
        let raw = include_str!("catalog_fixtures/codex_skills_list.json");
        let reply: Value = serde_json::from_str(raw).unwrap();
        let entries = parse_codex(&reply);
        assert!(!entries.is_empty());
        assert!(entries.iter().all(|e| e.kind == EntryKind::Skill));
        assert!(entries.iter().any(|e| e.name == "example:deploy-notes"));
        let disabled = serde_json::json!({"data":[{"skills":[
            {"name":"on","description":"yes","enabled":true},
            {"name":"off","description":"no","enabled":false},
            {"name":"default","description":"implicit"}
        ]}]});
        let names: Vec<_> = parse_codex(&disabled)
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["on", "default"]);
    }

    #[test]
    fn opencode_parses_commands_and_skills_from_recorded_replies() {
        let commands: Value =
            serde_json::from_str(include_str!("catalog_fixtures/opencode_commands.json")).unwrap();
        let skills: Value =
            serde_json::from_str(include_str!("catalog_fixtures/opencode_skills.json")).unwrap();
        let cmd = parse_opencode_commands(&commands);
        let skill = parse_opencode_skills(&skills);
        assert!(cmd.iter().all(|e| e.kind == EntryKind::Command));
        assert!(cmd.iter().any(|e| e.name == "review" && !e.description.is_empty()));
        assert!(skill.iter().all(|e| e.kind == EntryKind::Skill));
        assert!(skill.iter().any(|e| e.name == "format-docs"));
        assert!(skill.iter().all(|e| e.name.len() <= MAX_NAME));
    }

    #[test]
    fn cache_is_keyed_by_binary_path_and_version() {
        let cache = CatalogCache::default();
        let key = CacheKey {
            path: "/usr/bin/codex".into(),
            version: "0.169.0".into(),
        };
        let entries = vec![entry("review", "Review", EntryKind::Command, "").unwrap()];
        assert_eq!(cache.cached(&key, 1000), None);
        cache.remember(&key, Some(entries.clone()), 1000);
        assert_eq!(cache.cached(&key, 2000), Some(Some(entries.clone())));
        let upgraded = CacheKey {
            version: "0.170.0".into(),
            ..key.clone()
        };
        assert_eq!(cache.cached(&upgraded, 2000), None);
        let other = CacheKey {
            path: "/opt/codex".into(),
            version: key.version.clone(),
        };
        assert_eq!(cache.cached(&other, 2000), None);
    }

    #[test]
    fn cache_remembers_a_failure_only_for_its_ttl() {
        let cache = CatalogCache::default();
        let key = CacheKey {
            path: "/usr/bin/claude".into(),
            version: "2.1.326".into(),
        };
        cache.remember(&key, None, 1000);
        assert_eq!(cache.cached(&key, 1000 + FAILURE_TTL_MS - 1), Some(None));
        assert_eq!(cache.cached(&key, 1000 + FAILURE_TTL_MS), None);
    }

    #[test]
    fn disk_cache_survives_a_cold_memory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".slash-catalog.json");
        let key = CacheKey {
            path: "/usr/bin/opencode".into(),
            version: "2.2.6".into(),
        };
        let entries = vec![entry("init", "Setup", EntryKind::Command, "").unwrap()];
        assert_eq!(CatalogCache::from_disk(&file, &key, 1000), None);
        CatalogCache::to_disk(&file, &key, Some(entries.clone()), 1000);
        assert_eq!(
            CatalogCache::from_disk(&file, &key, 2000),
            Some(Some(entries))
        );
        // Отказ тоже сохраняется, чтобы не дёргать CLI на каждом старте.
        CatalogCache::to_disk(&file, &key, None, 3000);
        assert_eq!(CatalogCache::from_disk(&file, &key, 3100), Some(None));
    }

    #[test]
    fn entries_cut_long_text_on_a_character_boundary_and_require_a_name() {
        let long = "x".repeat(500);
        let entries = vec![
            entry("ok", &long, EntryKind::Command, &long).unwrap(),
            entry(" /slash ", "опис", EntryKind::Skill, "арг").unwrap(),
        ];
        assert_eq!(entries[0].description.chars().count(), MAX_DESCRIPTION + 1);
        assert_eq!(entries[0].argument_hint.chars().count(), MAX_HINT + 1);
        assert_eq!(entries[1].name, "slash");
        assert!(entry("имя с пробелом", "", EntryKind::Command, "").is_none());
        assert!(entry("", "", EntryKind::Command, "").is_none());
    }
}
