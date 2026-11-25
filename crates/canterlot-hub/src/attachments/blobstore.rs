use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures_util::{StreamExt, stream::BoxStream};
use sha2::{Digest, Sha256};
use tokio::io::AsyncSeekExt;
use uuid::Uuid;

use crate::config::AttachmentsConfig;

/// Чтение и запись содержимого ведутся кусками не больше файлового чанка протокола.
pub const IO_CHUNK_BYTES: usize = canterlot_proto::limits::MAX_FILE_CHUNK_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishStep {
    Verified,
    FileSynced,
    Renamed,
    DirSynced,
}

/// Наблюдатель шагов публикации; `Err` прерывает публикацию как сбой процесса.
#[allow(clippy::result_unit_err)]
pub trait PublishTrace: Send + Sync {
    fn step(&self, _step: PublishStep) -> Result<(), ()> {
        Ok(())
    }
}

pub struct NoTrace;
impl PublishTrace for NoTrace {}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("blob_io")]
    Io(#[from] io::Error),
    #[error("offset_mismatch")]
    OffsetMismatch { current: u64 },
    #[error("too_large")]
    TooLarge { current: u64 },
    #[error("body_interrupted")]
    BodyInterrupted { current: u64 },
    #[error("length_mismatch")]
    LengthMismatch { actual: u64 },
    #[error("checksum_mismatch")]
    ChecksumMismatch,
    #[error("not_found")]
    NotFound,
    #[error("offset_beyond_end")]
    OffsetBeyondEnd,
    #[error("publish_aborted")]
    Aborted,
    #[error("cross_device")]
    CrossDevice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Staging,
    Blob,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredFile {
    pub id: Uuid,
    pub kind: FileKind,
    pub mtime: DateTime<Utc>,
}

#[async_trait::async_trait]
pub trait BlobStore: Send + Sync {
    /// Захват тома до конца I/O; возвращённый драйвер удерживает его и в фоновых операциях.
    fn try_lock(&self, id: Uuid) -> io::Result<Option<Box<dyn BlobStore>>>;
    async fn staging_len(&self, id: Uuid) -> io::Result<Option<u64>>;
    async fn append_staging(
        &self,
        id: Uuid,
        offset: u64,
        body: BoxStream<'static, Result<Bytes, io::Error>>,
        max_total: u64,
    ) -> Result<u64, BlobError>;
    /// Проверка → fsync файла → rename → fsync каталогов; при уже опубликованном совпадающем blob
    /// копирования нет, повторяется только синхронизация каталогов.
    async fn publish(
        &self,
        id: Uuid,
        expected_len: u64,
        expected_sha256: &str,
    ) -> Result<(), BlobError>;
    async fn open(&self, id: Uuid, offset: u64) -> Result<(tokio::fs::File, u64), BlobError>;
    async fn delete_staging(&self, id: Uuid) -> io::Result<()>;
    async fn delete_blob(&self, id: Uuid) -> io::Result<()>;
    async fn scan(&self) -> io::Result<Vec<StoredFile>>;
    /// Lock-файлы тома с временем создания: сборщик убирает те, за которыми ничего не осталось.
    async fn scan_locks(&self) -> io::Result<Vec<(Uuid, DateTime<Utc>)>> {
        Ok(Vec::new())
    }
    /// Удаляет lock-файл, захват которого держит этот драйвер. Следующий захват того же
    /// ID создаст новый файл; захват на удалённом файле `try_lock` не принимает.
    async fn delete_lock(&self) -> io::Result<()> {
        Ok(())
    }
    /// Процесс один владеет томом (`hub.lock`): только тогда сборщик удаляет lock-файлы.
    fn owns_dir(&self) -> io::Result<bool> {
        Ok(false)
    }
}

/// Исключительный `flock` на `blob_dir/hub.lock`, общий для клонов драйвера процесса.
///
/// Удалять lock-файлы безопасно, только если каждый процесс с этим томом после `flock`
/// сверяет inode с путём. Хаб до этой сверки так не делает: он продолжил бы работу на
/// удалённом файле параллельно с новым захватом. Поэтому lock-файлы удаляет только
/// процесс, который держит `hub.lock`; второй процесс хаба на том же томе его не получит
/// и lock-файлы не тронет. Старый хаб `hub.lock` не берёт, и от него защищает порядок
/// выката: прежний хаб останавливается до старта нового (docs/deploy/pilot.md, §11.4).
struct DirOwner {
    path: PathBuf,
    held: std::sync::Mutex<Option<fs::File>>,
}

impl DirOwner {
    fn new(path: PathBuf) -> Self {
        let owner = Self {
            path,
            held: std::sync::Mutex::new(None),
        };
        // Сразу при открытии: второй процесс, открывший том позже, увидит захват.
        if let Err(error) = owner.acquire() {
            tracing::warn!(kind = "attachment_dir_owner_failed", error = %error.kind(), "blob dir ownership not checked");
        }
        owner
    }

    /// Держит ли процесс `hub.lock`; если нет, пробует взять его снова: прежний владелец
    /// мог закрыть том.
    fn acquire(&self) -> io::Result<bool> {
        let mut held = self.held.lock().unwrap_or_else(|error| error.into_inner());
        if held.is_some() {
            return Ok(true);
        }
        let file = fs::File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&self.path)?;
        match file.try_lock() {
            Ok(()) => {
                *held = Some(file);
                Ok(true)
            }
            Err(fs::TryLockError::WouldBlock) => Ok(false),
            Err(fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

/// Локальный том: `staging/<id>.part` и `blobs/<последние 2 hex>/<id>` в одном `blob_dir` на одной ФС.
#[derive(Clone)]
pub struct LocalBlobStore {
    staging: PathBuf,
    blobs: PathBuf,
    trace: Arc<dyn PublishTrace>,
    locks: PathBuf,
    held: Option<(Uuid, Arc<fs::File>)>,
    owner: Arc<DirOwner>,
}

/// Итог попытки захвата на открытом lock-файле.
#[derive(Debug, PartialEq, Eq)]
enum LockAttempt {
    Held,
    /// Захват держит другой драйвер.
    Busy,
    /// Файл успели удалить: по пути его нет или лежит новый.
    Replaced,
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    let path = std::path::absolute(path)?;
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::DirBuilder::new().mode(0o700).create(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && current.is_dir() => {}
            Err(error) => return Err(error),
        }
        // Существующий компонент мог создать ещё не синхронизировавший родителя процесс.
        if let Some(parent) = current.parent() {
            sync_dir(parent)?;
        }
    }
    Ok(())
}

fn sync_dir(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

fn not_found(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Длина и SHA-256 файла потоковым чтением с ограниченным буфером.
fn hash_file(file: &mut fs::File) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; IO_CHUNK_BYTES];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((total, hex(&hasher.finalize())))
}

impl LocalBlobStore {
    pub fn open(
        config: &AttachmentsConfig,
        trace: Arc<dyn PublishTrace>,
    ) -> Result<Self, BlobError> {
        create_private_dir(&config.blob_dir)?;
        let staging = config.blob_dir.join("staging");
        let blobs = config.blob_dir.join("blobs");
        create_private_dir(&staging)?;
        create_private_dir(&blobs)?;
        let locks = config.blob_dir.join("locks");
        create_private_dir(&locks)?;
        // rename между staging и blobs атомарен только внутри одной файловой системы.
        if fs::metadata(&staging)?.dev() != fs::metadata(&blobs)?.dev() {
            return Err(BlobError::CrossDevice);
        }
        sync_dir(&config.blob_dir)?;
        let owner = Arc::new(DirOwner::new(config.blob_dir.join("hub.lock")));
        Ok(Self {
            staging,
            blobs,
            trace,
            locks,
            held: None,
            owner,
        })
    }

    pub fn staging_path(&self, id: Uuid) -> PathBuf {
        self.staging.join(format!("{id}.part"))
    }

    fn shard(&self, id: Uuid) -> PathBuf {
        let name = id.to_string();
        self.blobs.join(&name[name.len() - 2..])
    }

    pub fn blob_path(&self, id: Uuid) -> PathBuf {
        let current = self.shard(id).join(id.to_string());
        let name = id.to_string();
        let legacy = self.blobs.join(&name[..2]).join(&name);
        if !current.exists() && legacy.exists() {
            legacy
        } else {
            current
        }
    }

    fn step(&self, step: PublishStep) -> Result<(), BlobError> {
        self.trace.step(step).map_err(|()| BlobError::Aborted)
    }

    fn publish_blocking(
        &self,
        id: Uuid,
        expected_len: u64,
        expected_sha256: &str,
    ) -> Result<(), BlobError> {
        let blob = self.blob_path(id);
        let shard = blob.parent().expect("blob shard").to_owned();
        match fs::File::open(&blob) {
            Ok(mut file) => {
                // Повтор после сбоя между публикацией и commit: содержимое уже на месте.
                let (len, sha) = hash_file(&mut file)?;
                if len != expected_len || sha != expected_sha256 {
                    return Err(BlobError::ChecksumMismatch);
                }
                self.step(PublishStep::Verified)?;
                sync_dir(&shard)?;
                sync_dir(&self.blobs)?;
                sync_dir(&self.staging)?;
                return self.step(PublishStep::DirSynced);
            }
            Err(error) if not_found(&error) => {}
            Err(error) => return Err(error.into()),
        }
        let staging = self.staging_path(id);
        let mut file = match fs::File::open(&staging) {
            Ok(file) => file,
            // Пустой файл не требует ни одного тела: staging нулевой длины заводится при публикации.
            Err(error) if not_found(&error) && expected_len == 0 => fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(&staging)?,
            Err(error) if not_found(&error) => {
                return Err(BlobError::LengthMismatch { actual: 0 });
            }
            Err(error) => return Err(error.into()),
        };
        let actual = file.metadata()?.len();
        if actual != expected_len {
            return Err(BlobError::LengthMismatch { actual });
        }
        let (len, sha) = hash_file(&mut file)?;
        if len != expected_len {
            return Err(BlobError::LengthMismatch { actual: len });
        }
        if sha != expected_sha256 {
            return Err(BlobError::ChecksumMismatch);
        }
        self.step(PublishStep::Verified)?;
        file.sync_all()?;
        drop(file);
        self.step(PublishStep::FileSynced)?;
        match fs::DirBuilder::new().mode(0o700).create(&shard) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && shard.is_dir() => {}
            Err(error) => return Err(error.into()),
        }
        sync_dir(&self.blobs)?;
        fs::rename(&staging, &blob)?;
        self.step(PublishStep::Renamed)?;
        sync_dir(&shard)?;
        sync_dir(&self.staging)?;
        self.step(PublishStep::DirSynced)
    }

    async fn remove_file(&self, path: PathBuf) -> io::Result<()> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let _store = store;
            match fs::remove_file(path) {
                Err(error) if !not_found(&error) => Err(error),
                _ => Ok(()),
            }
        })
        .await
        .map_err(|_| io::Error::other("blob delete failed"))?
    }

    fn open_lock(&self, id: Uuid) -> io::Result<fs::File> {
        fs::File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.locks.join(id.to_string()))
    }

    /// Захват на уже открытом lock-файле с проверкой, что по пути лежит он же.
    fn lock_opened(&self, id: Uuid, file: &fs::File) -> io::Result<LockAttempt> {
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Ok(LockAttempt::Busy),
            Err(fs::TryLockError::Error(error)) => return Err(error),
        }
        let opened = file.metadata()?;
        match fs::metadata(self.locks.join(id.to_string())) {
            Ok(current) if (current.dev(), current.ino()) == (opened.dev(), opened.ino()) => {
                Ok(LockAttempt::Held)
            }
            Ok(_) => Ok(LockAttempt::Replaced),
            Err(error) if not_found(&error) => Ok(LockAttempt::Replaced),
            Err(error) => Err(error),
        }
    }

    fn scan_blocking(&self) -> io::Result<Vec<StoredFile>> {
        let mut found = Vec::new();
        let mtime = |path: &Path| -> io::Result<DateTime<Utc>> {
            Ok(DateTime::<Utc>::from(fs::metadata(path)?.modified()?))
        };
        for entry in fs::read_dir(&self.staging)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(id) = name
                .to_str()
                .and_then(|n| n.strip_suffix(".part"))
                .and_then(|n| Uuid::parse_str(n).ok())
            else {
                continue;
            };
            match mtime(&entry.path()) {
                Ok(mtime) => found.push(StoredFile {
                    id,
                    kind: FileKind::Staging,
                    mtime,
                }),
                Err(error) if not_found(&error) => {}
                Err(error) => return Err(error),
            }
        }
        for shard in fs::read_dir(&self.blobs)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(shard.path())? {
                let entry = entry?;
                let Some(id) = entry
                    .file_name()
                    .to_str()
                    .and_then(|n| Uuid::parse_str(n).ok())
                else {
                    continue;
                };
                match mtime(&entry.path()) {
                    Ok(mtime) => found.push(StoredFile {
                        id,
                        kind: FileKind::Blob,
                        mtime,
                    }),
                    Err(error) if not_found(&error) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(found)
    }
}

async fn blocking<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, BlobError> + Send + 'static,
) -> Result<T, BlobError> {
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|_| BlobError::Io(io::Error::other("blob task failed")))?
}

#[async_trait::async_trait]
impl BlobStore for LocalBlobStore {
    fn try_lock(&self, id: Uuid) -> io::Result<Option<Box<dyn BlobStore>>> {
        if self.held.is_some() {
            return Err(io::Error::other("blob store already locked"));
        }
        // Сборщик удаляет lock-файлы, за которыми ничего не осталось. Файл, открытый до
        // такого удаления, — уже не тот, что лежит по пути: захват на нём никого не
        // исключает. Тогда открываем заново; три подряд удаления под рукой не бывает.
        for _ in 0..3 {
            let file = self.open_lock(id)?;
            match self.lock_opened(id, &file)? {
                LockAttempt::Held => {
                    let mut store = self.clone();
                    store.held = Some((id, Arc::new(file)));
                    return Ok(Some(Box::new(store)));
                }
                LockAttempt::Busy => return Ok(None),
                LockAttempt::Replaced => {}
            }
        }
        Ok(None)
    }
    async fn scan_locks(&self) -> io::Result<Vec<(Uuid, DateTime<Utc>)>> {
        let locks = self.locks.clone();
        tokio::task::spawn_blocking(move || {
            let mut found = Vec::new();
            for entry in fs::read_dir(&locks)? {
                let entry = entry?;
                let Some(id) = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| Uuid::parse_str(name).ok())
                else {
                    continue;
                };
                // Lock-файл не пишется, поэтому время изменения — время его создания.
                match entry.metadata().and_then(|meta| meta.modified()) {
                    Ok(created) => found.push((id, DateTime::<Utc>::from(created))),
                    Err(error) if not_found(&error) => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(found)
        })
        .await
        .map_err(|_| io::Error::other("lock scan failed"))?
    }
    async fn delete_lock(&self) -> io::Result<()> {
        let Some((id, _)) = &self.held else {
            return Err(io::Error::other("blob store is not locked"));
        };
        self.remove_file(self.locks.join(id.to_string())).await
    }
    fn owns_dir(&self) -> io::Result<bool> {
        self.owner.acquire()
    }
    async fn staging_len(&self, id: Uuid) -> io::Result<Option<u64>> {
        match tokio::fs::metadata(self.staging_path(id)).await {
            Ok(meta) => Ok(Some(meta.len())),
            Err(error) if not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn append_staging(
        &self,
        id: Uuid,
        offset: u64,
        mut body: BoxStream<'static, Result<Bytes, io::Error>>,
        max_total: u64,
    ) -> Result<u64, BlobError> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.staging_path(id))?;
        let mut written = file.metadata()?.len();
        if written != offset {
            return Err(BlobError::OffsetMismatch { current: written });
        }
        while let Some(frame) = body.next().await {
            let frame = frame.map_err(|_| BlobError::BodyInterrupted { current: written })?;
            let store = self.clone();
            let (next_file, next_written, over) = blocking(move || {
                let _store = store;
                let mut over = false;
                for piece in frame.chunks(IO_CHUNK_BYTES) {
                    let take = (piece.len() as u64).min(max_total.saturating_sub(written)) as usize;
                    file.write_all(&piece[..take])?;
                    written += take as u64;
                    if take < piece.len() {
                        over = true;
                        break;
                    }
                }
                Ok((file, written, over))
            })
            .await?;
            file = next_file;
            written = next_written;
            if over {
                return Err(BlobError::TooLarge { current: written });
            }
        }
        Ok(written)
    }

    async fn publish(
        &self,
        id: Uuid,
        expected_len: u64,
        expected_sha256: &str,
    ) -> Result<(), BlobError> {
        let store = self.clone();
        let sha = expected_sha256.to_owned();
        blocking(move || store.publish_blocking(id, expected_len, &sha)).await
    }

    async fn open(&self, id: Uuid, offset: u64) -> Result<(tokio::fs::File, u64), BlobError> {
        let mut file = match tokio::fs::File::open(self.blob_path(id)).await {
            Ok(file) => file,
            Err(error) if not_found(&error) => return Err(BlobError::NotFound),
            Err(error) => return Err(error.into()),
        };
        let len = file.metadata().await?.len();
        if offset > len {
            return Err(BlobError::OffsetBeyondEnd);
        }
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        Ok((file, len))
    }

    async fn delete_staging(&self, id: Uuid) -> io::Result<()> {
        self.remove_file(self.staging_path(id)).await
    }

    async fn delete_blob(&self, id: Uuid) -> io::Result<()> {
        self.remove_file(self.blob_path(id)).await
    }

    async fn scan(&self) -> io::Result<Vec<StoredFile>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.scan_blocking())
            .await
            .map_err(|_| io::Error::other("blob scan failed"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &tempfile::TempDir) -> LocalBlobStore {
        let config = AttachmentsConfig {
            blob_dir: dir.path().join("blobs"),
            ..Default::default()
        };
        LocalBlobStore::open(&config, Arc::new(NoTrace)).unwrap()
    }

    /// Процесс открыл lock-файл, а сборщик успел его удалить и отпустить. Захват на таком
    /// файле не принимается: другой процесс уже взял бы захват на новом файле того же ID.
    #[tokio::test]
    async fn a_lock_on_a_removed_lock_file_is_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        let id = Uuid::now_v7();
        let opened_before = store.open_lock(id).unwrap();
        let collector = store.try_lock(id).unwrap().expect("free lock");
        assert_eq!(
            store.lock_opened(id, &opened_before).unwrap(),
            LockAttempt::Busy
        );
        collector.delete_lock().await.unwrap();
        drop(collector);
        assert!(store.scan_locks().await.unwrap().is_empty());
        // Пути нет вовсе.
        assert_eq!(
            store.lock_opened(id, &opened_before).unwrap(),
            LockAttempt::Replaced
        );
        opened_before.unlock().unwrap();
        // По пути уже новый файл: его захват и захват на старом друг друга не исключают,
        // поэтому старый обязан быть отвергнут.
        let fresh = store.try_lock(id).unwrap().expect("a new lock file");
        assert_eq!(
            store.lock_opened(id, &opened_before).unwrap(),
            LockAttempt::Replaced
        );
        assert!(
            store.try_lock(id).unwrap().is_none(),
            "the new lock is held"
        );
        drop(fresh);
        assert!(store.try_lock(id).unwrap().is_some());
        let listed = store.scan_locks().await.unwrap();
        assert_eq!(listed.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [id]);
    }

    #[tokio::test]
    async fn only_a_locked_driver_deletes_its_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir);
        assert!(store.delete_lock().await.is_err());
    }
}
