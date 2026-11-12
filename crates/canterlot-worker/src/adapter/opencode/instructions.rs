//! Каталог конфигурации исполнения. «Как у владельца» работает с его конфигурацией как
//! есть; остальные режимы заменяют глобальный каталог своим: всё содержимое каталога
//! владельца ссылками, кроме его `AGENTS.md`, и `AGENTS.md` пресета, если он есть.
//! Репозиторные `AGENTS.md` при этом действуют. Ссылки, а не `OPENCODE_CONFIG`: тот принимает
//! один файл, а у владельца могут быть `opencode.json` и `opencode.jsonc`, плюс плагины.
//!
//! Остаётся разрыв: `~/AGENTS.md` и `AGENTS.md` выше корня репозитория читаются, если рабочий
//! каталог лежит под домом (решение 4 спеки: принято, у владельца такого файла нет).

use crate::adapter::{InstructionMode, InstructionSpec};
use std::{
    ffi::OsString,
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Write as _},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, symlink},
    path::{Path, PathBuf},
};

const AGENTS: &str = "AGENTS.md";
const TEMPORARY: &str = ".AGENTS.md.tmp";
/// Файлы конфигурации владельца, из которых делегированная сессия берёт всё, кроме `mcp`.
const CONFIG_FILES: [&str; 2] = ["opencode.json", "opencode.jsonc"];

pub fn config_dir(exec_dir: &Path) -> PathBuf {
    exec_dir.join("opencode-config")
}

/// Пустой глобальный каталог конфигурации: настоящий `XDG_CONFIG_HOME` владельца
/// делегированная сессия не видит.
pub fn xdg_dir(exec_dir: &Path) -> PathBuf {
    exec_dir.join("opencode-xdg")
}

fn variable<'a>(env: &'a [(OsString, OsString)], key: &str) -> Option<&'a OsString> {
    env.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
}

/// Каталог конфигурации владельца: явная настройка, затем `$XDG_CONFIG_HOME/opencode`.
pub fn owner_dir(configured: Option<&Path>, env: &[(OsString, OsString)]) -> Option<PathBuf> {
    configured
        .map(Path::to_path_buf)
        .or_else(|| variable(env, "XDG_CONFIG_HOME").map(|dir| Path::new(dir).join("opencode")))
        .or_else(|| variable(env, "HOME").map(|home| Path::new(home).join(".config/opencode")))
}

// Прошлые ссылки снимаются: перезапуск мог сменить набор файлов. Настоящие файлы остаются.
fn link_owner(dir: &Path, owner: &Path, skip: &[&str]) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            fs::remove_file(entry.path())?;
        }
    }
    let entries = match fs::read_dir(owner) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let name = entry?.file_name();
        if skip.iter().any(|skipped| name == *skipped) {
            continue;
        }
        match symlink(owner.join(&name), dir.join(&name)) {
            Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error),
            _ => {}
        }
    }
    Ok(())
}

/// JSONC без комментариев и висячих запятых: так OpenCode читает конфигурацию.
fn strip_jsonc(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut in_string = false;
    while at < chars.len() {
        let c = chars[at];
        if in_string {
            out.push(c);
            if c == '\\' && at + 1 < chars.len() {
                out.push(chars[at + 1]);
                at += 1;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if c == '/' && chars.get(at + 1) == Some(&'/') {
            while at < chars.len() && chars[at] != '\n' {
                at += 1;
            }
            continue;
        } else if c == '/' && chars.get(at + 1) == Some(&'*') {
            at += 2;
            while at + 1 < chars.len() && !(chars[at] == '*' && chars[at + 1] == '/') {
                at += 1;
            }
            at += 2;
            continue;
        } else if c == ',' {
            let next = chars[at + 1..].iter().find(|c| !c.is_whitespace()).copied();
            if !matches!(next, Some('}' | ']')) {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        at += 1;
    }
    out
}

/// Конфигурация владельца без `mcp` (D24). Не разобралась - ошибка: целиком копировать нельзя.
#[cfg(test)]
fn without_mcp(text: &str) -> io::Result<String> {
    without_mcp_except(text, &[]).map(|(text, _)| text)
}

/// Как `without_mcp`, но серверы `allowed` остаются: `mcp.servers.<имя>` (вид 2.x) и
/// `mcp.<имя>` (вид 1.x). Прочие ключи `mcp` уходят. Возвращает и оставленные имена.
fn without_mcp_except(text: &str, allowed: &[String]) -> io::Result<(String, Vec<String>)> {
    use serde_json::Value;
    let invalid = |_| io::Error::new(io::ErrorKind::InvalidData, "owner config");
    let mut config: Value = serde_json::from_str(&strip_jsonc(text)).map_err(invalid)?;
    let object = config
        .as_object_mut()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "owner config"))?;
    let mut owned = match object.remove("mcp") {
        Some(Value::Object(mcp)) => mcp,
        _ => serde_json::Map::new(),
    };
    let mut servers = match owned.remove("servers") {
        Some(Value::Object(servers)) => servers,
        _ => serde_json::Map::new(),
    };
    let mut kept_mcp = serde_json::Map::new();
    let mut kept_servers = serde_json::Map::new();
    let mut kept = Vec::new();
    for name in crate::adapter::delegate_mcp_names(allowed) {
        let mut found = false;
        if let Some(server @ Value::Object(_)) = servers.remove(&name) {
            kept_servers.insert(name.clone(), server);
            found = true;
        }
        if name != "servers"
            && let Some(server @ Value::Object(_)) = owned.remove(&name)
        {
            kept_mcp.insert(name.clone(), server);
            found = true;
        }
        if found {
            kept.push(name);
        }
    }
    if !kept_servers.is_empty() {
        kept_mcp.insert("servers".into(), Value::Object(kept_servers));
    }
    if !kept_mcp.is_empty() {
        object.insert("mcp".into(), Value::Object(kept_mcp));
    }
    let text = serde_json::to_string(&config).map_err(invalid)?;
    Ok((text, kept))
}

// Копии лежат настоящими файлами рядом со ссылками; исчезнувший у владельца файл исчезает и тут.
// Возвращает разрешённые серверы, оставленные хотя бы в одной копии.
fn copy_config_without_mcp(
    dir: &Path,
    owner: Option<&Path>,
    allowed: &[String],
) -> io::Result<Vec<String>> {
    let mut kept_names: Vec<String> = Vec::new();
    for name in CONFIG_FILES {
        let target = dir.join(name);
        let text = match owner.map(|owner| fs::read_to_string(owner.join(name))) {
            Some(Ok(text)) => text,
            Some(Err(error)) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {
                match fs::remove_file(&target) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                    _ => {}
                }
                continue;
            }
        };
        let temporary = dir.join(format!(".{name}.tmp"));
        match fs::remove_file(&temporary) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let (copy, kept) = without_mcp_except(&text, allowed)?;
        // Копия может нести секреты разрешённых серверов: 0600 с момента создания.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(copy.as_bytes())?;
        fs::rename(temporary, target)?;
        for server in kept {
            if !kept_names.contains(&server) {
                kept_names.push(server);
            }
        }
    }
    Ok(kept_names)
}

// Подмена целиком: сервер следит за файлом и прочитал бы половину текста.
fn write_text(dir: &Path, spec: &InstructionSpec) -> io::Result<()> {
    let agents = dir.join(AGENTS);
    if spec.mode != InstructionMode::Preset {
        return match fs::remove_file(agents) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    let temporary = dir.join(TEMPORARY);
    match fs::remove_file(&temporary) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(spec.text.as_bytes())?;
    fs::rename(temporary, agents)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)?
}

/// Итог подготовки каталога: переменные запуска и судьба разрешённых серверов MCP.
#[derive(Debug, Default)]
pub struct Prepared {
    pub env: Vec<(OsString, OsString)>,
    /// Разрешённые делегату серверы, оставленные в копиях конфигурации.
    pub mcp_kept: Vec<String>,
    /// Разрешённые серверы, которых нет в конфигурации владельца.
    pub mcp_missing: Vec<String>,
}

/// Готовит каталог и возвращает переменные запуска. Делегированная сессия (`isolate_mcp`)
/// всегда получает каталог с копией конфигурации без `mcp`, пустой глобальный каталог и
/// запрет репозиторной конфигурации: серверов владельца и проекта в ней нет.
pub async fn prepare(
    exec_dir: &Path,
    owner: Option<PathBuf>,
    spec: &InstructionSpec,
    isolate_mcp: bool,
) -> io::Result<Vec<(OsString, OsString)>> {
    prepare_with(exec_dir, owner, spec, isolate_mcp, &[])
        .await
        .map(|prepared| prepared.env)
}

/// `prepare`, у делегированной сессии копии сохраняют серверы `delegate_mcp`.
pub async fn prepare_with(
    exec_dir: &Path,
    owner: Option<PathBuf>,
    spec: &InstructionSpec,
    isolate_mcp: bool,
    delegate_mcp: &[String],
) -> io::Result<Prepared> {
    if spec.mode == InstructionMode::Owner && !isolate_mcp {
        rewrite(exec_dir, spec).await?;
        return Ok(Prepared::default());
    }
    let allowed: Vec<String> = if isolate_mcp {
        crate::adapter::delegate_mcp_names(delegate_mcp)
    } else {
        Vec::new()
    };
    let wanted = allowed.clone();
    let dir = config_dir(exec_dir);
    let target = dir.clone();
    let xdg = xdg_dir(exec_dir);
    let spec = spec.clone();
    // «Как у владельца» оставляет его `AGENTS.md` ссылкой, остальные режимы подменяют его.
    let owner_text = spec.mode == InstructionMode::Owner;
    let mcp_kept = blocking(move || {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&target)?;
        let mut skip = Vec::new();
        if !owner_text {
            skip.push(AGENTS);
        }
        let mut kept = Vec::new();
        if isolate_mcp {
            skip.extend(CONFIG_FILES);
            DirBuilder::new().recursive(true).mode(0o700).create(&xdg)?;
            kept = copy_config_without_mcp(&target, owner.as_deref(), &allowed)?;
        }
        if let Some(owner) = &owner {
            link_owner(&target, owner, &skip)?;
        }
        if !owner_text {
            write_text(&target, &spec)?;
        }
        Ok(kept)
    })
    .await?;
    let mcp_missing = wanted
        .into_iter()
        .filter(|name| !mcp_kept.contains(name))
        .collect();
    let mut env: Vec<(OsString, OsString)> =
        vec![("OPENCODE_CONFIG_DIR".into(), dir.into_os_string())];
    if !owner_text {
        env.push(("OPENCODE_DISABLE_CLAUDE_CODE".into(), "1".into()));
    }
    if isolate_mcp {
        env.push(("XDG_CONFIG_HOME".into(), xdg_dir(exec_dir).into_os_string()));
        env.push(("OPENCODE_DISABLE_PROJECT_CONFIG".into(), "1".into()));
    }
    Ok(Prepared {
        env,
        mcp_kept,
        mcp_missing,
    })
}

/// Смена текста без перезапуска: сервер следит за файлом.
pub async fn rewrite(exec_dir: &Path, spec: &InstructionSpec) -> io::Result<()> {
    let dir = config_dir(exec_dir);
    let spec = spec.clone();
    blocking(move || write_text(&dir, &spec)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> InstructionSpec {
        InstructionSpec {
            mode: InstructionMode::Clean,
            text: String::new(),
            preset_id: None,
            revision: 0,
            title: String::new(),
        }
    }

    fn owner_config(dir: &Path) {
        fs::create_dir_all(dir.join("plugins")).unwrap();
        fs::write(dir.join("AGENTS.md"), "owner text").unwrap();
        fs::write(dir.join("auth-helper.json"), "{}").unwrap();
        fs::write(
            dir.join("opencode.jsonc"),
            "// owner\n{\n  \"model\": \"a/b\", /* inline */\n  \"provider\": {\"p\": {\"options\": {\"url\": \"http://x//y\",},},},\n  \"mcp\": {\"servers\": {\"nanokvm\": {\"type\": \"local\", \"command\": [\"/bin/true\"]}}},\n}\n",
        )
        .unwrap();
        fs::write(
            dir.join("opencode.json"),
            r#"{"mcp": {"legacy": {"type": "local"}}, "theme": "t"}"#,
        )
        .unwrap();
    }

    fn get(env: &[(OsString, OsString)], key: &str) -> Option<OsString> {
        env.iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_go_but_strings_stay() {
        let text =
            "{ // c\n \"a\": \"x // not a comment\", /* b */ \"b\": [1, 2,], \"c\": \"q\\\"//\", }";
        let value: serde_json::Value = serde_json::from_str(&strip_jsonc(text)).unwrap();
        assert_eq!(value["a"], "x // not a comment");
        assert_eq!(value["b"], serde_json::json!([1, 2]));
        assert_eq!(value["c"], "q\"//");
        assert!(without_mcp("[1]").is_err());
        assert!(without_mcp("{ broken").is_err());
    }

    #[tokio::test]
    async fn isolated_prepare_copies_the_config_without_mcp_and_closes_other_sources() {
        let root = tempfile::tempdir().unwrap();
        let owner = root.path().join("owner");
        owner_config(&owner);
        let exec = root.path().join("exec");
        let env = prepare(&exec, Some(owner.clone()), &clean(), true)
            .await
            .unwrap();
        let dir = config_dir(&exec);
        assert_eq!(
            get(&env, "OPENCODE_CONFIG_DIR"),
            Some(dir.clone().into_os_string())
        );
        assert_eq!(
            get(&env, "XDG_CONFIG_HOME"),
            Some(xdg_dir(&exec).into_os_string())
        );
        assert_eq!(
            get(&env, "OPENCODE_DISABLE_PROJECT_CONFIG"),
            Some("1".into())
        );
        assert_eq!(get(&env, "OPENCODE_DISABLE_CLAUDE_CODE"), Some("1".into()));
        assert!(xdg_dir(&exec).is_dir());

        for name in CONFIG_FILES {
            let path = dir.join(name);
            assert!(!path.is_symlink(), "{name} is a copy, not a link");
            let text = fs::read_to_string(&path).unwrap();
            assert!(!text.contains("mcp") && !text.contains("nanokvm"), "{text}");
        }
        let jsonc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("opencode.jsonc")).unwrap()).unwrap();
        assert_eq!(jsonc["model"], "a/b");
        assert_eq!(jsonc["provider"]["p"]["options"]["url"], "http://x//y");
        // Остальное по ссылке; свой AGENTS.md не от владельца.
        assert!(dir.join("plugins").is_symlink());
        assert!(dir.join("auth-helper.json").is_symlink());
        assert!(!dir.join("AGENTS.md").exists());
        // Файлы владельца не тронуты.
        assert!(
            fs::read_to_string(owner.join("opencode.jsonc"))
                .unwrap()
                .contains("nanokvm")
        );
    }

    #[tokio::test]
    async fn isolated_owner_mode_keeps_the_owner_instructions() {
        let root = tempfile::tempdir().unwrap();
        let owner = root.path().join("owner");
        owner_config(&owner);
        let exec = root.path().join("exec");
        let mut spec = clean();
        spec.mode = InstructionMode::Owner;
        let env = prepare(&exec, Some(owner), &spec, true).await.unwrap();
        let dir = config_dir(&exec);
        assert!(dir.join("AGENTS.md").is_symlink());
        assert_eq!(get(&env, "OPENCODE_DISABLE_CLAUDE_CODE"), None);
        assert_eq!(
            get(&env, "OPENCODE_DISABLE_PROJECT_CONFIG"),
            Some("1".into())
        );
        assert!(
            !fs::read_to_string(dir.join("opencode.json"))
                .unwrap()
                .contains("legacy")
        );
    }

    #[tokio::test]
    async fn isolated_prepare_follows_the_owner_and_refuses_a_broken_config() {
        let root = tempfile::tempdir().unwrap();
        let owner = root.path().join("owner");
        owner_config(&owner);
        let exec = root.path().join("exec");
        prepare(&exec, Some(owner.clone()), &clean(), true)
            .await
            .unwrap();
        fs::remove_file(owner.join("opencode.json")).unwrap();
        prepare(&exec, Some(owner.clone()), &clean(), true)
            .await
            .unwrap();
        assert!(!config_dir(&exec).join("opencode.json").exists());
        fs::write(owner.join("opencode.json"), "{ broken").unwrap();
        assert!(prepare(&exec, Some(owner), &clean(), true).await.is_err());
    }

    #[tokio::test]
    async fn owner_started_prepare_is_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let owner = root.path().join("owner");
        owner_config(&owner);
        let exec = root.path().join("exec");
        let env = prepare(&exec, Some(owner.clone()), &clean(), false)
            .await
            .unwrap();
        let dir = config_dir(&exec);
        assert!(dir.join("opencode.json").is_symlink(), "linked as before");
        assert_eq!(get(&env, "XDG_CONFIG_HOME"), None);
        assert_eq!(get(&env, "OPENCODE_DISABLE_PROJECT_CONFIG"), None);
        let mut spec = clean();
        spec.mode = InstructionMode::Owner;
        assert!(
            prepare(&exec, Some(owner), &spec, false)
                .await
                .unwrap()
                .is_empty()
        );
    }

    const FAKE_KEY: &str = "ctx7-FAKE-not-a-real-key";

    fn owner_with_context7(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("opencode.json"),
            serde_json::json!({
                "theme": "t",
                "mcp": {
                    "servers": {
                        "context7": {"type": "remote", "url": "https://mcp.context7.example/mcp",
                            "headers": {"Authorization": format!("Bearer {FAKE_KEY}")}},
                        "nanokvm": {"type": "local", "command": ["/bin/true"]}
                    },
                    "other_setting": true
                }
            })
            .to_string(),
        )
        .unwrap();
        // Вид 1.x: сервер прямо под `mcp`.
        fs::write(
            dir.join("opencode.jsonc"),
            "// owner\n{\"mcp\": {\"legacy7\": {\"type\": \"remote\", \"url\": \"https://x\"}, \"nanokvm\": {\"type\": \"local\"}},}\n",
        )
        .unwrap();
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[tokio::test]
    async fn isolated_prepare_keeps_only_allowed_servers_in_both_shapes() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let owner = root.path().join("owner");
        owner_with_context7(&owner);
        let exec = root.path().join("exec");
        let prepared = prepare_with(
            &exec,
            Some(owner.clone()),
            &clean(),
            true,
            &names(&["context7", "legacy7", "absent", "canterlot"]),
        )
        .await
        .unwrap();
        assert_eq!(prepared.mcp_kept, ["context7", "legacy7"]);
        assert_eq!(prepared.mcp_missing, ["absent"]);
        assert!(!format!("{prepared:?}").contains(FAKE_KEY));
        let dir = config_dir(&exec);
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("opencode.json")).unwrap()).unwrap();
        assert_eq!(
            json["mcp"],
            serde_json::json!({"servers": {"context7": {"type": "remote",
                "url": "https://mcp.context7.example/mcp",
                "headers": {"Authorization": format!("Bearer {FAKE_KEY}")}}}})
        );
        assert_eq!(json["theme"], "t");
        let jsonc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("opencode.jsonc")).unwrap()).unwrap();
        assert_eq!(
            jsonc["mcp"],
            serde_json::json!({"legacy7": {"type": "remote", "url": "https://x"}})
        );
        for name in CONFIG_FILES {
            let meta = fs::metadata(dir.join(name)).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600, "{name}");
            assert!(
                !fs::read_to_string(dir.join(name))
                    .unwrap()
                    .contains("nanokvm")
            );
        }
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[tokio::test]
    async fn isolated_prepare_with_no_allowed_server_is_exactly_as_before() {
        let root = tempfile::tempdir().unwrap();
        let owner = root.path().join("owner");
        owner_with_context7(&owner);
        let exec = root.path().join("exec");
        prepare(&exec, Some(owner.clone()), &clean(), true)
            .await
            .unwrap();
        let dir = config_dir(&exec);
        let before: Vec<String> = CONFIG_FILES
            .iter()
            .map(|name| fs::read_to_string(dir.join(name)).unwrap())
            .collect();
        for text in &before {
            assert!(!text.contains("mcp") && !text.contains(FAKE_KEY), "{text}");
        }
        let prepared = prepare_with(&exec, Some(owner), &clean(), true, &[])
            .await
            .unwrap();
        assert!(prepared.mcp_kept.is_empty() && prepared.mcp_missing.is_empty());
        let after: Vec<String> = CONFIG_FILES
            .iter()
            .map(|name| fs::read_to_string(dir.join(name)).unwrap())
            .collect();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn owner_started_prepare_ignores_the_allow_list() {
        let root = tempfile::tempdir().unwrap();
        let owner = root.path().join("owner");
        owner_with_context7(&owner);
        let exec = root.path().join("exec");
        let prepared = prepare_with(&exec, Some(owner), &clean(), false, &names(&["context7"]))
            .await
            .unwrap();
        assert!(prepared.mcp_kept.is_empty() && prepared.mcp_missing.is_empty());
        assert!(config_dir(&exec).join("opencode.json").is_symlink());
    }
}
