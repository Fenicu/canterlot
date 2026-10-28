use std::{
    fs,
    io::{self, Read},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures_util::{StreamExt, stream::BoxStream};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
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
}

/// Локальный том: `staging/<id>.part` и `blobs/<id[0..2]>/<id>` в одном `blob_dir` на одной ФС.
#[derive(Clone)]
pub struct LocalBlobStore {
    staging: PathBuf,
    blobs: PathBuf,
    trace: Arc<dyn PublishTrace>,
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
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
        let staging = config.blob_dir.join("staging");
        let blobs = config.blob_dir.join("blobs");
        create_private_dir(&staging)?;
        create_private_dir(&blobs)?;
        // rename между staging и blobs атомарен только внутри одной файловой системы.
        if fs::metadata(&staging)?.dev() != fs::metadata(&blobs)?.dev() {
            return Err(BlobError::CrossDevice);
        }
        sync_dir(&config.blob_dir)?;
        Ok(Self {
            staging,
            blobs,
            trace,
        })
    }

    pub fn staging_path(&self, id: Uuid) -> PathBuf {
        self.staging.join(format!("{id}.part"))
    }

    fn shard(&self, id: Uuid) -> PathBuf {
        let name = id.to_string();
        self.blobs.join(&name[..2])
    }

    pub fn blob_path(&self, id: Uuid) -> PathBuf {
        self.shard(id).join(id.to_string())
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
        let shard = self.shard(id);
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
        let fresh_shard = !shard.exists();
        create_private_dir(&shard)?;
        if fresh_shard {
            sync_dir(&self.blobs)?;
        }
        fs::rename(&staging, &blob)?;
        self.step(PublishStep::Renamed)?;
        sync_dir(&shard)?;
        sync_dir(&self.staging)?;
        self.step(PublishStep::DirSynced)
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

async fn remove(path: PathBuf) -> io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(error) if !not_found(&error) => Err(error),
        _ => Ok(()),
    }
}

#[async_trait::async_trait]
impl BlobStore for LocalBlobStore {
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
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.staging_path(id))
            .await?;
        let mut written = file.metadata().await?.len();
        if written != offset {
            return Err(BlobError::OffsetMismatch { current: written });
        }
        // Смещение приёма равно длине файла: всё записанное до обрыва остаётся.
        let outcome = loop {
            let Some(frame) = body.next().await else {
                break Ok(written);
            };
            let Ok(frame) = frame else {
                break Err(BlobError::BodyInterrupted { current: written });
            };
            let mut over = false;
            for piece in frame.chunks(IO_CHUNK_BYTES) {
                let room = max_total.saturating_sub(written);
                let take = (piece.len() as u64).min(room) as usize;
                if take > 0 {
                    file.write_all(&piece[..take]).await?;
                    written += take as u64;
                }
                if take < piece.len() {
                    over = true;
                    break;
                }
            }
            if over {
                break Err(BlobError::TooLarge { current: written });
            }
        };
        file.flush().await?;
        outcome
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
        remove(self.staging_path(id)).await
    }

    async fn delete_blob(&self, id: Uuid) -> io::Result<()> {
        remove(self.blob_path(id)).await
    }

    async fn scan(&self) -> io::Result<Vec<StoredFile>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.scan_blocking())
            .await
            .map_err(|_| io::Error::other("blob scan failed"))?
    }
}
