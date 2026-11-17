//! Версии CLI в отчёте машины: сравнение, минимум из кода, способ установки и
//! последний выпуск из реестра npm. Никакая из этих функций не обновляет CLI.

use std::{
    cmp::Ordering, collections::HashMap, ffi::OsStr, path::Path, sync::Mutex, time::Duration,
};

use serde::{Deserialize, Serialize};

/// Адрес реестра по умолчанию: только публичные пакеты, без учётных данных.
pub const NPM_REGISTRY: &str = "https://registry.npmjs.org";

/// Не чаще одного запроса на пакет за этот срок.
pub const CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
/// Срок одного запроса: сеть не должна задерживать отчёт машины.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarnessVersion {
    pub installed: String,
    pub latest: String,
    pub minimum: String,
    pub update_command: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallMethod {
    NpmGlobal,
    Native,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Cached {
    version: String,
    fetched_at_ms: u64,
}

type DiskCache = HashMap<String, Cached>;

#[derive(Default)]
pub struct LatestVersions {
    memory: Mutex<HashMap<String, Cached>>,
}

fn fresh(entry: &Cached, now_ms: u64) -> bool {
    now_ms.saturating_sub(entry.fetched_at_ms) < CACHE_TTL.as_millis() as u64
}

fn disk_read(cache: &Path) -> DiskCache {
    std::fs::read(cache)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn disk_remember(cache: &Path, package: &str, version: &str, now_ms: u64) {
    let mut entries = disk_read(cache);
    entries.insert(
        package.to_owned(),
        Cached {
            version: version.to_owned(),
            fetched_at_ms: now_ms,
        },
    );
    let Ok(bytes) = serde_json::to_vec(&entries) else {
        return;
    };
    let temporary = cache.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    if std::fs::write(&temporary, bytes).is_ok() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
        let _ = std::fs::rename(&temporary, cache);
    }
}

impl LatestVersions {
    fn remembered(&self, package: &str) -> Option<String> {
        self.memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(package)
            .map(|entry| entry.version.clone())
    }

    fn remember(&self, package: &str, version: &str, now_ms: u64) {
        self.memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                package.to_owned(),
                Cached {
                    version: version.to_owned(),
                    fetched_at_ms: now_ms,
                },
            );
    }

    /// Последний выпуск из `dist-tags.latest`; при сбое остаётся прежнее значение,
    /// а при его отсутствии — пустая строка. Отчёт машины из-за сети не падает.
    pub async fn fetch(&self, package: &str, registry: &str, cache: &Path, now_ms: u64) -> String {
        let memory = self
            .memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(package)
            .cloned();
        if let Some(entry) = memory.filter(|entry| fresh(entry, now_ms)) {
            return entry.version;
        }
        let disk = disk_read(cache).remove(package);
        if let Some(entry) = disk.as_ref().filter(|entry| fresh(entry, now_ms)) {
            self.remember(package, &entry.version, now_ms);
            return entry.version.clone();
        }
        let url = format!(
            "{}/{}",
            registry.trim_end_matches('/'),
            package.replacen('/', "%2F", 1)
        );
        let fetched = tokio::time::timeout(FETCH_TIMEOUT, async {
            let client = reqwest::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .build()
                .ok()?;
            let response = client.get(&url).send().await.ok()?;
            if !response.status().is_success() {
                return None;
            }
            let body: serde_json::Value = response.json().await.ok()?;
            body.pointer("/dist-tags/latest")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .await
        .ok()
        .flatten();
        match fetched {
            Some(version) => {
                self.remember(package, &version, now_ms);
                disk_remember(cache, package, &version, now_ms);
                version
            }
            // Сеть недоступна: не стираем последнее известное значение, даже если
            // его срок уже прошёл. Пустота означает «никогда не знали».
            None => self
                .remembered(package)
                .or_else(|| disk.map(|entry| entry.version))
                .unwrap_or_default(),
        }
    }
}

struct Semver<'a> {
    core: (u64, u64, u64),
    pre: Vec<&'a str>,
}

fn parse_semver(text: &str) -> Option<Semver<'_>> {
    let text = text.split_once('+').map_or(text, |(core, _)| core);
    let (core, pre) = match text.split_once('-') {
        Some((core, pre)) => (core, pre),
        None => (text, ""),
    };
    let mut parts = core.split('.');
    let core = (
        parts.next()?.parse::<u64>().ok()?,
        parts.next()?.parse::<u64>().ok()?,
        parts.next()?.parse::<u64>().ok()?,
    );
    if parts.next().is_some() {
        return None;
    }
    let pre = if pre.is_empty() {
        Vec::new()
    } else {
        pre.split('.').collect()
    };
    Some(Semver { core, pre })
}

/// Сравнение semver с предварительными сборками: `1.2.3-rc.2 < 1.2.3`.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (Some(left), Some(right)) = (parse_semver(a), parse_semver(b)) else {
        return a.cmp(b);
    };
    left.core
        .cmp(&right.core)
        .then_with(|| match (left.pre.is_empty(), right.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (x, y) in left.pre.iter().zip(&right.pre) {
                    let ordering = match (x.parse::<u64>(), y.parse::<u64>()) {
                        (Ok(x), Ok(y)) => x.cmp(&y),
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => x.cmp(y),
                    };
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                }
                left.pre.len().cmp(&right.pre.len())
            }
        })
}

/// Правило определения способа установки по каноническому пути бинарника
/// (тот же путь, что проба `--version` использует для своего кэша).
/// Глобальная установка npm кладёт симлинк `<prefix>/bin/<cli>` на файл внутри
/// `<prefix>/lib/node_modules/<пакет>/...`, поэтому после разрешения симлинков
/// путь содержит `node_modules`. В остальных случаях это нативный установщик
/// (`~/.local/bin`, `~/.claude/local`, `~/.opencode/bin`) со своей командой обновления.
pub fn install_method(resolved: &Path) -> InstallMethod {
    if resolved
        .components()
        .any(|part| part.as_os_str() == OsStr::new("node_modules"))
    {
        InstallMethod::NpmGlobal
    } else {
        InstallMethod::Native
    }
}

/// Команда обновления для способа установки. Codex распространяется через npm
/// и его собственный установщик тоже ставит пакет npm, поэтому команда одна.
pub fn update_command(harness: &str, method: InstallMethod) -> String {
    let package = npm_package(harness);
    if !package.is_empty() && method == InstallMethod::NpmGlobal {
        return format!("npm i -g {package}@latest");
    }
    match harness {
        "claude_code" => "claude update".into(),
        "codex" => "npm i -g @openai/codex@latest".into(),
        "opencode" => "opencode upgrade".into(),
        _ => String::new(),
    }
}

/// Пакет npm, из которого берётся `dist-tags.latest`.
pub fn npm_package(harness: &str) -> &'static str {
    match harness {
        "claude_code" => "@anthropic-ai/claude-code",
        "codex" => "@openai/codex",
        "opencode" => "opencode-ai",
        _ => "",
    }
}

/// Минимальная поддерживаемая версия берётся из кода адаптера, не из конфигурации.
pub fn minimum(harness: &str) -> String {
    let triple = match harness {
        "claude_code" => crate::adapter::claude_code::version::MIN_SUPPORTED_VERSION,
        "codex" => crate::adapter::codex::version::MIN_SUPPORTED_VERSION,
        "opencode" => crate::adapter::opencode::version::MIN_SUPPORTED_VERSION,
        _ => return String::new(),
    };
    format!("{}.{}.{}", triple.0, triple.1, triple.2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const HOUR_MS: u64 = 3_600_000;

    #[test]
    fn semver_compares_cores_first_and_prereleases_below_the_release() {
        assert_eq!(compare("2.1.311", "2.1.312"), Ordering::Less);
        assert_eq!(compare("2.1.311", "2.2.0"), Ordering::Less);
        assert_eq!(compare("0.167.0", "0.164.0"), Ordering::Greater);
        assert_eq!(compare("2.1.311", "2.1.311"), Ordering::Equal);
        assert_eq!(compare("2.1.311-rc.1", "2.1.311"), Ordering::Less);
        assert_eq!(compare("0.164.0-alpha.2", "0.164.0"), Ordering::Less);
        assert_eq!(
            compare("0.164.0-alpha.2", "0.164.0-alpha.10"),
            Ordering::Less
        );
        assert_eq!(compare("2.2.6-beta.1", "2.2.6-beta.2"), Ordering::Less);
        assert_eq!(compare("2.1.311+Build.7", "2.1.311"), Ordering::Equal);
    }

    #[test]
    fn install_method_distinguishes_npm_global_prefix_from_native_installer() {
        assert_eq!(
            install_method(Path::new(
                "/usr/local/lib/node_modules/@openai/codex/bin/codex.js"
            )),
            InstallMethod::NpmGlobal
        );
        assert_eq!(
            install_method(Path::new(
                "/home/owner/.nvm/versions/node/v22/lib/node_modules/opencode-ai/bin/opencode"
            )),
            InstallMethod::NpmGlobal
        );
        assert_eq!(
            install_method(Path::new("/home/owner/.local/bin/claude")),
            InstallMethod::Native
        );
        assert_eq!(
            install_method(Path::new("/usr/bin/opencode")),
            InstallMethod::Native
        );
    }

    #[test]
    fn update_command_and_minimum_follow_the_harness_and_install_method() {
        assert_eq!(
            update_command("claude_code", InstallMethod::Native),
            "claude update"
        );
        assert_eq!(
            update_command("claude_code", InstallMethod::NpmGlobal),
            "npm i -g @anthropic-ai/claude-code@latest"
        );
        assert_eq!(
            update_command("codex", InstallMethod::NpmGlobal),
            "npm i -g @openai/codex@latest"
        );
        assert_eq!(
            update_command("codex", InstallMethod::Native),
            "npm i -g @openai/codex@latest"
        );
        assert_eq!(
            update_command("opencode", InstallMethod::Native),
            "opencode upgrade"
        );
        assert_eq!(
            update_command("opencode", InstallMethod::NpmGlobal),
            "npm i -g opencode-ai@latest"
        );
        assert_eq!(npm_package("claude_code"), "@anthropic-ai/claude-code");
        assert_eq!(npm_package("codex"), "@openai/codex");
        assert_eq!(npm_package("opencode"), "opencode-ai");
        assert_eq!(minimum("claude_code"), "2.1.311");
        assert_eq!(minimum("codex"), "0.164.0");
        assert_eq!(minimum("opencode"), "2.1.4");
        assert_eq!(minimum("synthetic"), "");
    }

    async fn fake_registry(
        calls: Arc<AtomicUsize>,
        body: Arc<Mutex<Result<String, u16>>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                calls.fetch_add(1, AtomicOrdering::SeqCst);
                let mut request = [0u8; 4096];
                let _ = socket.read(&mut request).await;
                let response = body.lock().unwrap().clone();
                let (status, payload) = match response {
                    Ok(version) => (
                        "200 OK",
                        format!("{{\"dist-tags\":{{\"latest\":\"{version}\"}}}}"),
                    ),
                    Err(code) => (
                        if code == 500 {
                            "500 Internal Server Error"
                        } else {
                            "404 Not Found"
                        },
                        String::new(),
                    ),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn fetch_asks_the_registry_at_most_once_per_six_hours_and_caches_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join(".npm-latest.json");
        let calls = Arc::new(AtomicUsize::new(0));
        let body = Arc::new(Mutex::new(Ok("2.2.6".to_owned())));
        let (registry, server) = fake_registry(calls.clone(), body.clone()).await;
        let versions = LatestVersions::default();
        let now = 1_700_000_000_000u64;
        assert_eq!(
            versions
                .fetch("@openai/codex", &registry, &cache, now)
                .await,
            "2.2.6"
        );
        assert_eq!(
            versions
                .fetch("@openai/codex", &registry, &cache, now + 10_000)
                .await,
            "2.2.6"
        );
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
        // Новый экземпляр: память холодная, но диск хранит то же значение.
        let restarted = LatestVersions::default();
        assert_eq!(
            restarted
                .fetch("@openai/codex", &registry, &cache, now + HOUR_MS)
                .await,
            "2.2.6"
        );
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
        // Через 6 часов срок прошёл: запрос повторяется и берёт новый выпуск.
        *body.lock().unwrap() = Ok("2.2.7".to_owned());
        assert_eq!(
            restarted
                .fetch("@openai/codex", &registry, &cache, now + 7 * HOUR_MS)
                .await,
            "2.2.7"
        );
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn failed_fetch_keeps_the_last_known_value_and_never_fails_the_report() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join(".npm-latest.json");
        let calls = Arc::new(AtomicUsize::new(0));
        let body = Arc::new(Mutex::new(Ok("2.1.311".to_owned())));
        let (registry, server) = fake_registry(calls.clone(), body.clone()).await;
        let versions = LatestVersions::default();
        let now = 1_700_000_000_000u64;
        assert_eq!(
            versions
                .fetch("@anthropic-ai/claude-code", &registry, &cache, now)
                .await,
            "2.1.311"
        );
        *body.lock().unwrap() = Err(500);
        assert_eq!(
            versions
                .fetch(
                    "@anthropic-ai/claude-code",
                    &registry,
                    &cache,
                    now + 7 * HOUR_MS
                )
                .await,
            "2.1.311"
        );
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
        // Пустой кэш и недоступный реестр дают пустую строку, а не ошибку.
        let empty = dir.path().join(".missing.json");
        let cold = LatestVersions::default();
        assert_eq!(cold.fetch("opencode-ai", &registry, &empty, now).await, "");
        server.abort();
    }
}
