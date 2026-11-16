//! Файлы секретов на машине сессии: tmpfs, 0700/0600 и ровно один срок жизни.
//!
//! Значение приходит одним управляющим сообщением и живёт только здесь, в файле.
//! Поэтому в этом модуле нет журналирования значения: `Debug`, ошибки и логи
//! называют лишь короткую причину, а не текст, путь или размер содержимого.

use std::{
    fmt,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use canterlot_proto::worker::v1::{
    SecretDeliver, SecretDelivered, SecretFailed, SecretManifest, SecretManifests, SecretRemoved,
};
use rustix::fs::{Mode, OFlags};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::SecretConfig;

/// Сколько файл живёт после записи.
pub const FILE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Полный бросок временного файла: такой считается мусором уборки.
const TEMP_MAX_AGE: Duration = Duration::from_secs(60 * 60);

/// Короткий код причины: он уходит в факт и в журнал, но не содержит значения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// `XDG_RUNTIME_DIR` не задан.
    XdgUnset,
    /// Базовый каталог не на tmpfs: файл на диске был бы нарушением гарантии.
    NotTmpfs,
    /// Каталог не 0700, не принадлежит воркеру или является симлинком.
    InsecureDirectory,
    /// Записать или переименовать файл не удалось.
    Io,
    /// Манифест не читается или не совпадает с файлом.
    Manifest,
}

impl Failure {
    /// Причина уходит наружу строкой — только этот код, без путей и данных.
    pub fn code(self) -> &'static str {
        match self {
            Self::XdgUnset => "xdg_unset",
            Self::NotTmpfs => "not_tmpfs",
            Self::InsecureDirectory => "insecure_directory",
            Self::Io => "io",
            Self::Manifest => "manifest",
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// Итог записи: путь и срок — метаданные, которые хаб отдаёт агенту.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: PathBuf,
    pub expires_at_ms: u64,
    pub conversation_id: Uuid,
}

/// Манифест рядом с файлом: только адреса и срок, значения нет.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    attempt_id: Uuid,
    conversation_id: Uuid,
    expires_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    Removed,
    /// Файла уже нет: отзыв повторный, повтор безвреден.
    Missing,
    /// Манифест принадлежит другой попытке: чужой файл не трогаем.
    Stale,
}

impl Removal {
    fn reason(self) -> &'static str {
        match self {
            Self::Removed => "revoked",
            Self::Missing => "revoked",
            Self::Stale => "revoked",
        }
    }
}

fn uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Проверка каталога: настоящий каталог, не симлинк, владелец — воркер, права 0700.
fn secure_directory(path: &Path) -> Result<(), Failure> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| Failure::InsecureDirectory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Failure::InsecureDirectory);
    }
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != uid() || metadata.mode() & 0o777 != 0o700 {
        return Err(Failure::InsecureDirectory);
    }
    Ok(())
}

/// Каталог живёт на tmpfs: `statfs` отвечает за точку монтирования файла.
fn is_tmpfs(path: &Path) -> bool {
    const TMPFS_MAGIC: i64 = 0x0102_1994;
    match rustix::fs::statfs(path) {
        Ok(stat) => stat.f_type == TMPFS_MAGIC,
        Err(_) => false,
    }
}

fn create_private_directory(path: &Path) -> Result<(), Failure> {
    if path.exists() {
        return secure_directory(path);
    }
    let mut builder = std::fs::DirBuilder::new();
    use std::os::unix::fs::DirBuilderExt;
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => secure_directory(path),
        // Каталог мог создать параллельный вызов: проверка решает гонку.
        Err(_) if path.exists() => secure_directory(path),
        Err(_) => Err(Failure::Io),
    }
}

/// Базовый каталог секретов: `$XDG_RUNTIME_DIR/canterlot-secrets` либо явный
/// тестовый путь. Каталог создаётся 0700, проверяется и обязан быть на tmpfs.
pub fn base_dir(config: &SecretConfig) -> Result<PathBuf, Failure> {
    let base = match &config.base_dir {
        Some(path) => path.clone(),
        None => {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or(Failure::XdgUnset)?;
            PathBuf::from(runtime).join("canterlot-secrets")
        }
    };
    create_private_directory(&base)?;
    if !is_tmpfs(&base) {
        return Err(Failure::NotTmpfs);
    }
    Ok(base)
}

/// Каталог разговора: `0700`, владелец — воркер, не симлинк.
fn conversation_dir(config: &SecretConfig, conversation_id: Uuid) -> Result<PathBuf, Failure> {
    let base = base_dir(config)?;
    let dir = base.join(conversation_id.to_string());
    create_private_directory(&dir)?;
    // Вложенный каталог не должен уводить за базовый даже через симлинк родителя.
    let canonical = std::fs::canonicalize(&dir).map_err(|_| Failure::InsecureDirectory)?;
    let canonical_base = std::fs::canonicalize(&base).map_err(|_| Failure::InsecureDirectory)?;
    if !canonical.starts_with(&canonical_base) {
        return Err(Failure::InsecureDirectory);
    }
    Ok(dir)
}

fn manifest_path(dir: &Path, request_id: Uuid) -> PathBuf {
    dir.join(format!("{request_id}.json"))
}

fn file_path(dir: &Path, request_id: Uuid) -> PathBuf {
    dir.join(request_id.to_string())
}

fn read_manifest(dir: &Path, request_id: Uuid) -> Option<Manifest> {
    let bytes = std::fs::read(manifest_path(dir, request_id)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Пишет файл и манифест. Та же попытка не переписывает файл и не сдвигает срок.
pub fn write(config: &SecretConfig, request: &SecretDeliver) -> Result<Written, Failure> {
    let request_id = Uuid::parse_str(&request.request_id).map_err(|_| Failure::Manifest)?;
    let attempt_id = Uuid::parse_str(&request.attempt_id).map_err(|_| Failure::Manifest)?;
    let conversation_id =
        Uuid::parse_str(&request.conversation_id).map_err(|_| Failure::Manifest)?;
    let dir = conversation_dir(config, conversation_id)?;

    // Повтор той же попытки: файл уже на месте, срок не сбрасывается.
    if let Some(manifest) = read_manifest(&dir, request_id)
        && manifest.attempt_id == attempt_id
    {
        let path = file_path(&dir, request_id);
        if path.exists() {
            return Ok(Written {
                path,
                expires_at_ms: manifest.expires_at_ms,
                conversation_id,
            });
        }
    }

    let expires_at_ms = u64::try_from(
        chrono::Utc::now()
            .checked_add_signed(chrono::TimeDelta::seconds(FILE_TTL.as_secs() as i64))
            .unwrap_or_else(chrono::Utc::now)
            .timestamp_millis(),
    )
    .unwrap_or(0);
    let temp = dir.join(format!(".{request_id}.tmp-{attempt_id}"));
    write_private(&temp, &request.value)?;
    let path = file_path(&dir, request_id);
    if std::fs::rename(&temp, &path).is_err() {
        let _ = std::fs::remove_file(&temp);
        return Err(Failure::Io);
    }
    let manifest = Manifest {
        attempt_id,
        conversation_id,
        expires_at_ms,
    };
    let encoded = serde_json::to_vec(&manifest).map_err(|_| Failure::Manifest)?;
    let temp_manifest = dir.join(format!(".{request_id}.json.tmp-{attempt_id}"));
    write_private(&temp_manifest, &encoded)?;
    if std::fs::rename(&temp_manifest, manifest_path(&dir, request_id)).is_err() {
        let _ = std::fs::remove_file(&temp_manifest);
        return Err(Failure::Io);
    }
    Ok(Written {
        path,
        expires_at_ms,
        conversation_id,
    })
}

/// `O_CREAT|O_EXCL|O_NOFOLLOW` и 0600: чужой файл не подменить симлинком.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    let flags = OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd =
        rustix::fs::open(path, flags, Mode::from_bits_truncate(0o600)).map_err(|_| Failure::Io)?;
    let mut file = std::fs::File::from(fd);
    file.write_all(bytes).map_err(|_| Failure::Io)?;
    file.sync_all().map_err(|_| Failure::Io)?;
    Ok(())
}

/// Удаляет файл и манифест, если манифест принадлежит этой попытке.
pub fn remove(config: &SecretConfig, request_id: Uuid, attempt_id: Uuid) -> Removal {
    let Ok(base) = base_dir(config) else {
        return Removal::Missing;
    };
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Removal::Missing;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(manifest) = read_manifest(&dir, request_id) else {
            continue;
        };
        if manifest.attempt_id != attempt_id {
            return Removal::Stale;
        }
        let path = file_path(&dir, request_id);
        let file_gone = !path.exists();
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(manifest_path(&dir, request_id));
        return if file_gone {
            Removal::Missing
        } else {
            Removal::Removed
        };
    }
    Removal::Missing
}

/// Метаданные всех файлов: хаб сверяет `sending` после переподключения.
pub fn manifests(config: &SecretConfig) -> SecretManifests {
    let mut items = Vec::new();
    let Ok(base) = base_dir(config) else {
        return SecretManifests { manifests: items };
    };
    let Ok(entries) = std::fs::read_dir(&base) else {
        return SecretManifests { manifests: items };
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(request) = name.strip_suffix(".json") else {
                continue;
            };
            if request.starts_with('.') {
                continue;
            }
            let Ok(request_id) = Uuid::parse_str(request) else {
                continue;
            };
            let Some(manifest) = read_manifest(&dir, request_id) else {
                continue;
            };
            let path = file_path(&dir, request_id);
            if !path.exists() {
                continue;
            }
            items.push(SecretManifest {
                attempt_id: manifest.attempt_id.to_string(),
                request_id: request_id.to_string(),
                conversation_id: manifest.conversation_id.to_string(),
                path: path.display().to_string(),
                expires_at_ms: manifest.expires_at_ms,
            });
        }
    }
    SecretManifests { manifests: items }
}

/// Что уборка нашла и убрала: каждый известный файл отчитывается отдельно.
#[derive(Debug, Default)]
pub struct Swept {
    pub removed: Vec<SecretRemoved>,
    pub corrupt: u64,
    pub orphans: u64,
}

/// Уборка: просроченные файлы, сироты, брошенные временные файлы, испорченные
/// манифесты и каталоги разговоров, которых воркер больше не ведёт.
pub fn sweep(config: &SecretConfig, live: &[Uuid], now_ms: u64) -> Swept {
    let mut swept = Swept::default();
    let Ok(base) = base_dir(config) else {
        return swept;
    };
    let Ok(entries) = std::fs::read_dir(&base) else {
        return swept;
    };
    let live: std::collections::HashSet<Uuid> = live.iter().copied().collect();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let conversation = entry
            .file_name()
            .to_str()
            .and_then(|name| Uuid::parse_str(name).ok());
        // Каталог сессии, которую воркер больше не ведёт, убирается целиком.
        if conversation.is_none_or(|id| !live.contains(&id)) {
            swept.orphans += remove_tree(&dir);
            continue;
        }
        let Ok(files) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<String> = files
            .flatten()
            .filter_map(|file| file.file_name().to_str().map(str::to_owned))
            .collect();
        names.sort();
        for name in names {
            if name.starts_with('.') {
                let path = dir.join(&name);
                let old = std::fs::metadata(&path)
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .and_then(|time| time.elapsed().ok())
                    .is_some_and(|age| age > TEMP_MAX_AGE);
                if old {
                    let _ = std::fs::remove_file(&path);
                    swept.orphans += 1;
                }
                continue;
            }
            let Some(request) = name.strip_suffix(".json") else {
                // Файл без манифеста: его писал прерванный вызов, срок неизвестен.
                let Ok(request_id) = Uuid::parse_str(&name) else {
                    continue;
                };
                if read_manifest(&dir, request_id).is_none() {
                    let _ = std::fs::remove_file(dir.join(&name));
                    swept.orphans += 1;
                }
                continue;
            };
            let Ok(request_id) = Uuid::parse_str(request) else {
                let _ = std::fs::remove_file(dir.join(&name));
                swept.corrupt += 1;
                continue;
            };
            let Some(manifest) = read_manifest(&dir, request_id) else {
                let _ = std::fs::remove_file(dir.join(&name));
                let _ = std::fs::remove_file(file_path(&dir, request_id));
                swept.corrupt += 1;
                continue;
            };
            let path = file_path(&dir, request_id);
            if !path.exists() {
                let _ = std::fs::remove_file(manifest_path(&dir, request_id));
                swept.orphans += 1;
                continue;
            }
            if manifest.expires_at_ms <= now_ms {
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(manifest_path(&dir, request_id));
                swept.removed.push(SecretRemoved {
                    attempt_id: manifest.attempt_id.to_string(),
                    reason: "expired".into(),
                });
            }
        }
    }
    swept
}

fn remove_tree(dir: &Path) -> u64 {
    let mut removed = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                removed += remove_tree(&path);
            } else if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
    removed
}

/// Факт доставки: только метаданные.
pub fn delivered_fact(request: &SecretDeliver, written: &Written) -> SecretDelivered {
    SecretDelivered {
        attempt_id: request.attempt_id.clone(),
        request_id: request.request_id.clone(),
        path: written.path.display().to_string(),
        expires_at_ms: written.expires_at_ms,
    }
}

/// Факт отказа: короткая причина без значения и без пути.
pub fn failed_fact(request: &SecretDeliver, failure: Failure) -> SecretFailed {
    SecretFailed {
        attempt_id: request.attempt_id.clone(),
        reason: failure.code().into(),
    }
}

/// Факт удаления: тот же `attempt_id`, который назвал отзыв.
pub fn removed_fact(attempt_id: Uuid, removal: Removal) -> SecretRemoved {
    SecretRemoved {
        attempt_id: attempt_id.to_string(),
        reason: removal.reason().into(),
    }
}

/// Убирает файлы разговоров, которые воркер больше не ведёт, и просроченные файлы.
/// Запускается при старте и раз в 10 минут.
pub(crate) async fn run_sweeper(
    config: SecretConfig,
    shared: std::sync::Arc<crate::exec::Shared>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(600));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let swept = {
            let live: Vec<Uuid> = shared
                .infos
                .lock()
                .map(|infos| infos.values().map(|info| info.conversation_id).collect())
                .unwrap_or_default();
            let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0);
            sweep(&config, &live, now_ms)
        };
        if !swept.removed.is_empty() || swept.corrupt > 0 || swept.orphans > 0 {
            let facts = swept.removed.clone();
            let _ = shared
                .store
                .call(move |slot| {
                    let store = crate::exec::open(slot)?;
                    for fact in facts {
                        store.append_cj_fact(
                            canterlot_proto::worker::v1::record_body::Body::SecretRemoved(fact),
                        )?;
                    }
                    Ok(())
                })
                .await;
            tracing::info!(
                kind = "secret_sweep",
                removed = swept.removed.len(),
                corrupt = swept.corrupt,
                orphans = swept.orphans,
                "secret files swept"
            );
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow_and_update() {
                    return;
                }
            }
            _ = ticker.tick() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    /// Базовый каталог на настоящем tmpfs: `$XDG_RUNTIME_DIR` не трогаем.
    struct TestBase(PathBuf);

    impl TestBase {
        fn new() -> Self {
            let path =
                PathBuf::from("/dev/shm").join(format!("canterlot-secret-{}", Uuid::now_v7()));
            Self(path)
        }

        fn config(&self) -> SecretConfig {
            SecretConfig {
                base_dir: Some(self.0.clone()),
            }
        }
    }

    impl Drop for TestBase {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn request(conversation: Uuid, request: Uuid, attempt: Uuid, value: &str) -> SecretDeliver {
        SecretDeliver {
            attempt_id: attempt.to_string(),
            request_id: request.to_string(),
            conversation_id: conversation.to_string(),
            value: value.as_bytes().to_vec(),
        }
    }

    #[test]
    fn writes_exact_bytes_into_a_private_file_with_a_metadata_only_manifest() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let value = "canterlot-secret-PROBE-строка\n";
        let written =
            write(&config, &request(conversation, request_id, attempt, value)).expect("write");
        let bytes = std::fs::read(&written.path).expect("file");
        assert_eq!(bytes, value.as_bytes(), "byte-exact, newline preserved");
        let file = std::fs::metadata(&written.path).expect("file metadata");
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let dir = std::fs::metadata(written.path.parent().unwrap()).expect("dir metadata");
        assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        assert_eq!(dir.uid(), uid());
        assert_eq!(written.conversation_id, conversation);

        let manifest = written
            .path
            .parent()
            .unwrap()
            .join(format!("{request_id}.json"));
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).expect("manifest")).unwrap();
        let mut keys: Vec<&String> = raw.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["attempt_id", "conversation_id", "expires_at_ms"],
            "the manifest never carries the value"
        );
        assert!(
            !std::fs::read_to_string(&manifest)
                .unwrap()
                .contains("PROBE")
        );
        let expires = written.expires_at_ms;
        assert!(expires > 0);
    }

    #[test]
    fn a_replayed_attempt_re_reports_the_file_without_rewriting_it() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let first = write(
            &config,
            &request(conversation, request_id, attempt, "first\n"),
        )
        .unwrap();
        let again = write(
            &config,
            &request(conversation, request_id, attempt, "second\n"),
        )
        .unwrap();
        assert_eq!(first.path, again.path);
        assert_eq!(first.expires_at_ms, again.expires_at_ms, "the expiry stays");
        assert_eq!(std::fs::read(&first.path).unwrap(), b"first\n");
    }

    #[test]
    fn a_new_attempt_replaces_the_old_value() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let first = write(
            &config,
            &request(conversation, request_id, Uuid::now_v7(), "old\n"),
        )
        .unwrap();
        let second = write(
            &config,
            &request(conversation, request_id, Uuid::now_v7(), "new\n"),
        )
        .unwrap();
        assert_eq!(first.path, second.path);
        assert_eq!(std::fs::read(&second.path).unwrap(), b"new\n");
    }

    #[test]
    fn removal_deletes_the_file_and_ignores_a_stale_attempt() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let written = write(
            &config,
            &request(conversation, request_id, attempt, "value\n"),
        )
        .unwrap();
        assert_eq!(remove(&config, request_id, Uuid::now_v7()), Removal::Stale);
        assert!(
            written.path.exists(),
            "another attempt leaves the file alone"
        );
        assert_eq!(remove(&config, request_id, attempt), Removal::Removed);
        assert!(!written.path.exists());
        assert!(
            !written
                .path
                .parent()
                .unwrap()
                .join(format!("{request_id}.json"))
                .exists()
        );
        assert_eq!(remove(&config, request_id, attempt), Removal::Missing);
    }

    #[test]
    fn manifests_list_only_files_with_a_manifest() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let written = write(
            &config,
            &request(conversation, request_id, attempt, "value\n"),
        )
        .unwrap();
        let listed = manifests(&config);
        assert_eq!(listed.manifests.len(), 1);
        let manifest = &listed.manifests[0];
        assert_eq!(manifest.attempt_id, attempt.to_string());
        assert_eq!(manifest.request_id, request_id.to_string());
        assert_eq!(manifest.path, written.path.display().to_string());
        assert_eq!(manifest.expires_at_ms, written.expires_at_ms);
    }

    #[test]
    fn a_symlinked_conversation_directory_is_refused() {
        let base = TestBase::new();
        let config = base.config();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(&base.0).unwrap();
        std::fs::set_permissions(&base.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        let conversation = Uuid::now_v7();
        std::os::unix::fs::symlink(elsewhere.path(), base.0.join(conversation.to_string()))
            .unwrap();
        let failure = write(
            &config,
            &request(conversation, Uuid::now_v7(), Uuid::now_v7(), "value\n"),
        )
        .unwrap_err();
        assert_eq!(failure, Failure::InsecureDirectory);
        assert_eq!(failure.code(), "insecure_directory");
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_base_outside_tmpfs_is_refused() {
        // Каталог репозитория лежит на диске, а не на tmpfs: годной базой он быть не может.
        let directory = tempfile::Builder::new()
            .prefix("canterlot-secret-on-disk-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .unwrap();
        let config = SecretConfig {
            base_dir: Some(directory.path().join("secrets")),
        };
        assert_eq!(base_dir(&config).unwrap_err(), Failure::NotTmpfs);
        assert!(!directory.path().join("secrets").join(".tmp").exists());
    }

    #[test]
    fn sweep_removes_expired_orphans_corrupt_manifests_and_dead_conversations() {
        let base = TestBase::new();
        let config = base.config();
        let live = Uuid::now_v7();
        let dead = Uuid::now_v7();
        let live_ok = write(
            &config,
            &request(live, Uuid::now_v7(), Uuid::now_v7(), "live\n"),
        )
        .unwrap();
        let expired_request = Uuid::now_v7();
        let expired = write(
            &config,
            &request(live, expired_request, Uuid::now_v7(), "old\n"),
        )
        .unwrap();
        let dead_written = write(
            &config,
            &request(dead, Uuid::now_v7(), Uuid::now_v7(), "dead\n"),
        )
        .unwrap();
        // Просроченный манифест: срок в прошлом.
        let manifest_path = expired
            .path
            .parent()
            .unwrap()
            .join(format!("{expired_request}.json"));
        let mut manifest: Manifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest.expires_at_ms = 1;
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        // Испорченный манифест и файл-сирота без манифеста.
        let corrupt = live_ok
            .path
            .parent()
            .unwrap()
            .join(format!("{}.json", Uuid::now_v7()));
        std::fs::write(&corrupt, b"{not json").unwrap();
        let orphan = live_ok
            .path
            .parent()
            .unwrap()
            .join(Uuid::now_v7().to_string());
        std::fs::write(&orphan, b"orphan").unwrap();
        // Брошенный временный файл.
        let temp = live_ok.path.parent().unwrap().join(".tmp-test");
        std::fs::write(&temp, b"temp").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&temp)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(7200))
            .unwrap();

        let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
        let swept = sweep(&config, &[live], now_ms);
        assert!(live_ok.path.exists(), "a live file stays");
        assert!(!expired.path.exists(), "expired file removed");
        assert!(!manifest_path.exists());
        assert!(!dead_written.path.exists(), "dead conversation removed");
        assert!(!corrupt.exists());
        assert!(!orphan.exists());
        assert!(!temp.exists());
        assert_eq!(swept.removed.len(), 1);
        assert_eq!(swept.removed[0].reason, "expired");
        assert_eq!(swept.corrupt, 1);
        assert!(swept.orphans >= 1);
    }

    #[test]
    fn the_tmpfs_message_never_holds_the_value() {
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let deliver = request(conversation, request_id, attempt, "canterlot-secret-PROBE");
        let failed = failed_fact(&deliver, Failure::NotTmpfs);
        assert!(!format!("{failed:?}").contains("PROBE"));
        assert_eq!(failed.reason, "not_tmpfs");
    }
}
