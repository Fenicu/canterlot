use std::{
    collections::HashMap,
    fs, io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use canterlot_proto::{
    limits::{MAX_ATTACHMENT_BYTES, MAX_FILE_CHUNK_BYTES},
    worker::v1::{AttachmentChunk, AttachmentRef},
};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Semaphore, watch},
};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait AttachmentSource: Send + Sync {
    async fn open(&self, attachment_id: Uuid, offset: u64) -> Result<ChunkStream, FetchError>;
}
pub type ChunkStream =
    std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<AttachmentChunk, FetchError>> + Send>>;

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq, Hash)]
pub enum FetchError {
    #[error("transport")]
    Transport,
    #[error("not_found")]
    NotFound,
    #[error("corrupt")]
    Corrupt,
    #[error("size_mismatch")]
    SizeMismatch,
    #[error("io")]
    Io,
}
impl From<io::Error> for FetchError {
    fn from(_: io::Error) -> Self {
        Self::Io
    }
}
impl FetchError {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Transport => "hub_unreachable",
            Self::NotFound => "not_found",
            Self::Corrupt => "checksum_mismatch",
            Self::SizeMismatch => "size_mismatch",
            Self::Io => "io",
        }
    }
}

pub struct Published {
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub image: bool,
}

pub fn safe_file_name(attachment_id: Uuid, user_name: &str) -> String {
    let slug: String = user_name
        .bytes()
        .filter(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
        .map(char::from)
        .collect();
    let slug = slug.trim_start_matches('.');
    let slug = if slug.is_empty() {
        "file"
    } else {
        &slug[..slug.len().min(64)]
    };
    format!("{attachment_id}-{slug}")
}

#[derive(Clone)]
pub struct RetryPolicy {
    pub content_attempts: u32,
    pub retry: Duration,
    pub backoff_cap: Duration,
}
pub type SourceSlot = watch::Receiver<Option<Arc<dyn AttachmentSource>>>;

pub struct AttachmentStore {
    directory: PathBuf,
    slots: Arc<Semaphore>,
    locks: Mutex<HashMap<Uuid, Weak<tokio::sync::Mutex<()>>>>,
}

fn private_directory(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(io::Error::other("invalid attachment directory"));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

impl AttachmentStore {
    /// `exec_dir` уже надёжно закреплён в `executions/` подготовкой исполнения.
    pub fn new(exec_dir: &Path, parallelism: usize) -> io::Result<Self> {
        Self::with_slots(exec_dir, Arc::new(Semaphore::new(parallelism.clamp(1, 2))))
    }
    pub(crate) fn with_slots(exec_dir: &Path, slots: Arc<Semaphore>) -> io::Result<Self> {
        let directory = exec_dir.join("attachments");
        private_directory(&directory)?;
        private_directory(&directory.join(".tmp"))?;
        fs::File::open(exec_dir)?.sync_all()?;
        fs::File::open(&directory)?.sync_all()?;
        let directory = fs::canonicalize(directory)?;
        Ok(Self {
            directory,
            slots,
            locks: Mutex::default(),
        })
    }
    fn temp(&self, id: Uuid) -> PathBuf {
        self.directory.join(".tmp").join(format!("{id}.part"))
    }
    /// Временный файл, который сейчас пишет другая загрузка того же вложения, не трогается.
    pub fn remove_temp(&self, attachment_id: Uuid) {
        let locks = self.locks.lock().expect("attachment locks");
        let lock = locks.get(&attachment_id).and_then(Weak::upgrade);
        let _guard = match &lock {
            Some(lock) => match lock.try_lock() {
                Ok(guard) => Some(guard),
                Err(_) => return,
            },
            None => None,
        };
        self.unlink_temp(attachment_id);
    }
    fn unlink_temp(&self, attachment_id: Uuid) {
        let _ = fs::remove_file(self.temp(attachment_id));
    }
    pub fn remove_all(&self) {
        let _ = fs::remove_dir_all(&self.directory);
    }

    pub async fn ensure_published(
        &self,
        source: SourceSlot,
        reference: &AttachmentRef,
        policy: &RetryPolicy,
        cancel: CancellationToken,
    ) -> Result<Published, FetchError> {
        self.ensure_reporting(source, reference, policy, cancel, |_, _| {})
            .await
    }

    pub(crate) async fn ensure_reporting(
        &self,
        source: SourceSlot,
        reference: &AttachmentRef,
        policy: &RetryPolicy,
        cancel: CancellationToken,
        report: impl FnMut(FetchError, bool) + Send,
    ) -> Result<Published, FetchError> {
        let id = Uuid::parse_str(&reference.attachment_id).map_err(|_| FetchError::NotFound)?;
        let lock = {
            let mut locks = self.locks.lock().expect("attachment locks");
            locks.retain(|_, lock| lock.strong_count() > 0);
            let lock = locks
                .get(&id)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(())));
            locks.insert(id, Arc::downgrade(&lock));
            lock
        };
        let _lock = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                if let Ok(_lock) = lock.try_lock() {
                    self.unlink_temp(id);
                }
                return Err(FetchError::Transport);
            }
            lock = lock.lock() => lock,
        };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                self.unlink_temp(id);
                return Err(FetchError::Transport);
            }
            result = self.ensure_inner(id, source, reference, policy, report) => result,
        };
        if result.is_err() {
            self.unlink_temp(id);
        }
        result
    }

    async fn ensure_inner(
        &self,
        id: Uuid,
        mut source: SourceSlot,
        reference: &AttachmentRef,
        policy: &RetryPolicy,
        mut report: impl FnMut(FetchError, bool) + Send,
    ) -> Result<Published, FetchError> {
        let _slot = self.slots.acquire().await.map_err(|_| FetchError::Io)?;
        if reference.size > MAX_ATTACHMENT_BYTES {
            return Err(FetchError::SizeMismatch);
        }
        let path = self.directory.join(safe_file_name(id, &reference.name));
        let published = || Published {
            path: path.clone(),
            name: reference.name.clone(),
            mime: reference.mime.clone(),
            size: reference.size,
            image: reference.image,
        };
        if path.exists() {
            let (size, hash) = hash_file(&path).await?;
            if size == reference.size && hex(&hash.finalize()) == reference.sha256 {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
                tokio::fs::File::open(&path).await?.sync_all().await?;
                fs::File::open(&self.directory)?.sync_all()?;
                return Ok(published());
            }
            fs::remove_file(&path)?;
        }
        let temporary = self.temp(id);
        let mut failures = 0;
        let mut outage = false;
        let mut reasons = std::collections::HashSet::new();
        let mut backoff = Duration::from_millis(250).min(policy.backoff_cap);
        loop {
            let current = source.borrow_and_update().clone();
            let Some(current) = current else {
                if !outage {
                    report(FetchError::Transport, true);
                    outage = true;
                }
                if source.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
                continue;
            };
            // Смена источника (разрыв или новое подключение) прерывает поток; продолжение с длины файла.
            let result = tokio::select! {
                biased;
                _ = source.changed(), if source.has_changed().is_ok() => Err(FetchError::Transport),
                result = self.fetch_once(
                    &temporary,
                    &path,
                    reference,
                    current.as_ref(),
                    &mut outage,
                    &mut backoff,
                ) => result,
            };
            match result {
                Ok(()) => return Ok(published()),
                Err(FetchError::Transport) => {
                    if !outage {
                        report(FetchError::Transport, true);
                        outage = true;
                    }
                    backoff = backoff.min(policy.backoff_cap);
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = source.changed(), if source.has_changed().is_ok() => {}
                    }
                    backoff = backoff.saturating_mul(2).min(policy.backoff_cap);
                }
                Err(FetchError::Corrupt) => {
                    self.unlink_temp(id);
                    failures += 1;
                    if failures >= policy.content_attempts.max(1) {
                        return Err(FetchError::Corrupt);
                    }
                    if reasons.insert(FetchError::Corrupt) {
                        report(FetchError::Corrupt, true);
                    }
                    tokio::time::sleep(policy.retry).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn fetch_once(
        &self,
        temporary: &Path,
        path: &Path,
        reference: &AttachmentRef,
        source: &dyn AttachmentSource,
        outage: &mut bool,
        backoff: &mut Duration,
    ) -> Result<(), FetchError> {
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(temporary)?;
        if !file.metadata()?.is_file() {
            return Err(FetchError::Io);
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let mut file = tokio::fs::File::from_std(file);
        let (mut offset, mut hash) = hash_file(temporary).await?;
        if offset > reference.size {
            return Err(FetchError::SizeMismatch);
        }
        let mut stream = source
            .open(
                Uuid::parse_str(&reference.attachment_id).map_err(|_| FetchError::NotFound)?,
                offset,
            )
            .await?;
        loop {
            let chunk = stream.next().await.ok_or(FetchError::Transport)??;
            if chunk.data.len() > MAX_FILE_CHUNK_BYTES || chunk.offset != offset {
                return Err(FetchError::Corrupt);
            }
            let next = offset
                .checked_add(chunk.data.len() as u64)
                .ok_or(FetchError::SizeMismatch)?;
            if next > reference.size || next > MAX_ATTACHMENT_BYTES {
                return Err(FetchError::SizeMismatch);
            }
            file.write_all(&chunk.data).await?;
            file.flush().await?;
            hash.update(&chunk.data);
            offset = next;
            *outage = false;
            *backoff = Duration::from_millis(250);
            if chunk.last {
                if offset != reference.size || hex(&hash.finalize()) != reference.sha256 {
                    return Err(FetchError::Corrupt);
                }
                file.sync_all().await?;
                fs::rename(temporary, path)?;
                fs::File::open(&self.directory)?.sync_all()?;
                fs::File::open(self.directory.join(".tmp"))?.sync_all()?;
                return Ok(());
            }
            if chunk.data.is_empty() {
                return Err(FetchError::Corrupt);
            }
        }
    }
}

async fn hash_file(path: &Path) -> Result<(u64, Sha256), FetchError> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(FetchError::Io);
    }
    let mut file = tokio::fs::File::from_std(file);
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut bytes = vec![0; MAX_FILE_CHUNK_BYTES];
    loop {
        let n = file.read(&mut bytes).await?;
        if n == 0 {
            return Ok((size, hash));
        }
        hash.update(&bytes[..n]);
        size += n as u64;
        if size > MAX_ATTACHMENT_BYTES {
            return Err(FetchError::SizeMismatch);
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}
