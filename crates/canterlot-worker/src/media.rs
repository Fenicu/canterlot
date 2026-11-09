//! Картинки, на которые смотрел агент (pilot.md §5.7): копия в `exec_dir/media/` и загрузка на хаб.
//! Байты и пути не попадают ни в журналы, ни в `Debug`.

use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use canterlot_proto::{
    limits::{MAX_FILE_CHUNK_BYTES, MAX_MEDIA_BYTES},
    worker::v1::MediaHeader,
};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use uuid::Uuid;

use crate::adapter::MediaSource;

pub const MEDIA_DIR: &str = "media";
/// Байты начала файла, по которым определяются тип и размеры.
const PREFIX_BYTES: usize = 512 * 1024;
const CONTENT_ATTEMPTS: u32 = 3;

/// Копия картинки, готовая к загрузке.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub media_id: Uuid,
    pub block_id: Uuid,
    pub mime: &'static str,
    pub size: u64,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
}

/// Почему картинка не получена: блок сохраняет путь и размер.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    TooLarge,
    Unsupported,
    Unreadable,
}

impl Skip {
    pub fn reason(self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::Unsupported => "unsupported_type",
            Self::Unreadable => "unreadable",
        }
    }
}

/// Тип только по сигнатуре: объявленному расширению или типу не доверяем.
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(
        bytes.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn le16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(
        bytes.get(at..at + 2)?.try_into().ok()?,
    )))
}

fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at + 3)?;
    Some(u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16)
}

fn jpeg(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    loop {
        while *bytes.get(at)? != 0xff {
            at += 1;
        }
        while *bytes.get(at)? == 0xff {
            at += 1;
        }
        let marker = *bytes.get(at)?;
        at += 1;
        if marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        if marker == 0xd9 || marker == 0xda {
            return None;
        }
        let length = be16(bytes, at)? as usize;
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            return Some((be16(bytes, at + 5)?, be16(bytes, at + 3)?));
        }
        at += length;
    }
}

/// Ширина и высота из заголовка формата; `None`, если начало файла их не содержит.
pub fn dimensions(mime: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    let (width, height) = match mime {
        "image/png" if bytes.get(12..16) == Some(b"IHDR") => (
            u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?),
            u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?),
        ),
        "image/gif" => (le16(bytes, 6)?, le16(bytes, 8)?),
        "image/jpeg" => jpeg(bytes)?,
        "image/webp" => match bytes.get(12..16)? {
            b"VP8 " => (le16(bytes, 26)? & 0x3fff, le16(bytes, 28)? & 0x3fff),
            b"VP8L" if bytes.get(20) == Some(&0x2f) => {
                let bits = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
                ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
            }
            b"VP8X" => (le24(bytes, 24)? + 1, le24(bytes, 27)? + 1),
            _ => return None,
        },
        _ => return None,
    };
    (width > 0 && height > 0).then_some((width, height))
}

fn private_directory(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(io::Error::other("invalid media directory"));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

pub fn media_path(exec_dir: &Path, media_id: Uuid) -> PathBuf {
    exec_dir.join(MEDIA_DIR).join(media_id.to_string())
}

/// Читает источник сразу (файл мог измениться позже), проверяет тип и размер, считает SHA-256
/// и размеры и пишет копию `exec_dir/media/<media_id>` с правами 0600. Вызывать в spawn_blocking.
pub fn prepare(
    exec_dir: &Path,
    media_id: Uuid,
    block_id: Uuid,
    source: MediaSource,
) -> Result<Prepared, Skip> {
    let mut reader: Box<dyn Read> = match source {
        MediaSource::Bytes(bytes) => {
            if bytes.len() as u64 > MAX_MEDIA_BYTES {
                return Err(Skip::TooLarge);
            }
            Box::new(io::Cursor::new(bytes))
        }
        MediaSource::Base64(text) => {
            use base64::Engine as _;
            // Размер виден по длине: большую строку незачем декодировать.
            if text.len() as u64 / 4 * 3 > MAX_MEDIA_BYTES + 3 {
                return Err(Skip::TooLarge);
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(|_| Skip::Unsupported)?;
            if bytes.len() as u64 > MAX_MEDIA_BYTES {
                return Err(Skip::TooLarge);
            }
            Box::new(io::Cursor::new(bytes))
        }
        MediaSource::Path(path) => {
            // FIFO или устройство не должны блокировать чтение: принимаем только обычный файл.
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
                .open(path)
                .map_err(|_| Skip::Unreadable)?;
            let meta = file.metadata().map_err(|_| Skip::Unreadable)?;
            if !meta.is_file() {
                return Err(Skip::Unreadable);
            }
            if meta.len() > MAX_MEDIA_BYTES {
                return Err(Skip::TooLarge);
            }
            Box::new(file.take(MAX_MEDIA_BYTES + 1))
        }
    };
    let mut prefix = Vec::new();
    let mut buffer = vec![0; MAX_FILE_CHUNK_BYTES];
    let first = read_some(&mut reader, &mut buffer)?;
    let mime = sniff(&buffer[..first]).ok_or(Skip::Unsupported)?;
    let directory = exec_dir.join(MEDIA_DIR);
    private_directory(&directory).map_err(|_| Skip::Unreadable)?;
    let temporary = directory.join(format!(".{media_id}.part"));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| Skip::Unreadable)?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut chunk = first;
        while chunk > 0 {
            let bytes = &buffer[..chunk];
            size += chunk as u64;
            if size > MAX_MEDIA_BYTES {
                return Err(Skip::TooLarge);
            }
            hash.update(bytes);
            if prefix.len() < PREFIX_BYTES {
                prefix.extend_from_slice(&bytes[..chunk.min(PREFIX_BYTES - prefix.len())]);
            }
            file.write_all(bytes).map_err(|_| Skip::Unreadable)?;
            chunk = read_some(&mut reader, &mut buffer)?;
        }
        fs::rename(&temporary, media_path(exec_dir, media_id)).map_err(|_| Skip::Unreadable)?;
        let (width, height) = dimensions(mime, &prefix).unwrap_or((0, 0));
        Ok(Prepared {
            media_id,
            block_id,
            mime,
            size,
            sha256: hex(&hash.finalize()),
            width,
            height,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_some(reader: &mut dyn Read, buffer: &mut [u8]) -> Result<usize, Skip> {
    loop {
        match reader.read(buffer) {
            Ok(n) => return Ok(n),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(Skip::Unreadable),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PutError {
    /// Сеть, перезапуск хаба, `UNAVAILABLE`, `RESOURCE_EXHAUSTED`: повтор без ограничения.
    #[error("transport")]
    Transport,
    /// `DATA_LOSS`: содержимое не совпало с checksum; ограниченное число повторов.
    #[error("corrupt")]
    Corrupt,
    #[error("conflict")]
    Conflict,
    /// Хаб отказал без известной причины; так отвечает и хаб без поддержки `share`.
    #[error("rejected")]
    Rejected,
    /// Хаб назвал причину отказа (`media_too_large` и т.п.): повтор не поможет.
    #[error("too_large")]
    TooLarge,
    #[error("invalid_name")]
    InvalidName,
    #[error("invalid_type")]
    InvalidType,
    #[error("invalid_text")]
    InvalidText,
    #[error("io")]
    Io,
    /// Файл пришёл без идентификатора блока: блок нельзя связать с лентой.
    #[error("share_block_missing")]
    NoBlock,
}

impl PutError {
    /// Отказ `INVALID_ARGUMENT` хаба: сообщение статуса — стабильный код причины.
    pub fn from_invalid_argument(message: &str) -> Self {
        match message {
            "media_too_large" => Self::TooLarge,
            "media_invalid_name" => Self::InvalidName,
            "media_invalid_type" => Self::InvalidType,
            "media_invalid_text" => Self::InvalidText,
            _ => Self::Rejected,
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Self::Transport => "hub_unreachable",
            Self::Corrupt => "checksum_mismatch",
            Self::Conflict => "conflict",
            Self::Rejected => "rejected",
            Self::TooLarge => "too_large",
            Self::InvalidName => "invalid_name",
            Self::InvalidType => "invalid_type",
            Self::InvalidText => "invalid_text",
            Self::Io => "io",
            Self::NoBlock => "share_block_missing",
        }
    }
}

/// Канал загрузки картинок; ставится при каждом подключении с `pilot_v1`, снимается при разрыве.
#[async_trait::async_trait]
pub trait MediaSink: Send + Sync {
    async fn put(&self, header: MediaHeader, file: tokio::fs::File) -> Result<(), PutError>;
}

pub type SinkSlot = watch::Receiver<Option<Arc<dyn MediaSink>>>;

fn open_copy(path: &Path) -> Result<tokio::fs::File, PutError> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(|_| PutError::Io)?;
    if !file.metadata().map_err(|_| PutError::Io)?.is_file() {
        return Err(PutError::Io);
    }
    Ok(tokio::fs::File::from_std(file))
}

/// Загружает копию до окончательного ответа хаба. Транспортные ошибки ждут нового канала
/// или повторяются с backoff 250 мс → `backoff_cap`; ни поток SJ, ни управление не ждут загрузку.
pub async fn upload(
    mut sink: SinkSlot,
    path: &Path,
    header: MediaHeader,
    backoff_cap: Duration,
) -> Result<(), PutError> {
    let floor = Duration::from_millis(250).min(backoff_cap);
    let mut backoff = floor;
    let mut corrupt = 0;
    loop {
        let current = sink.borrow_and_update().clone();
        let Some(current) = current else {
            if sink.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
            continue;
        };
        let file = open_copy(path)?;
        // Смена канала (разрыв или новое подключение) прерывает поток; загрузка начинается заново.
        let result = tokio::select! {
            biased;
            _ = sink.changed(), if sink.has_changed().is_ok() => Err(PutError::Transport),
            result = current.put(header.clone(), file) => result,
        };
        match result {
            Ok(()) => return Ok(()),
            Err(PutError::Transport) => {}
            Err(PutError::Corrupt) => {
                corrupt += 1;
                if corrupt >= CONTENT_ATTEMPTS {
                    return Err(PutError::Corrupt);
                }
            }
            Err(error) => return Err(error),
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = sink.changed(), if sink.has_changed().is_ok() => {}
        }
        backoff = backoff.saturating_mul(2).min(backoff_cap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        bytes
    }

    // Картинка из результата инструмента: размер виден до декодирования.
    #[test]
    fn encoded_picture_is_bounded_before_decoding() {
        use base64::Engine as _;
        let dir = tempfile::TempDir::new().unwrap();
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let source = MediaSource::Base64(encode(&png(3, 2)));
        assert!(format!("{source:?}").contains("Base64"));
        let prepared = prepare(dir.path(), Uuid::now_v7(), Uuid::now_v7(), source).unwrap();
        assert_eq!(prepared.mime, "image/png");
        assert_eq!((prepared.width, prepared.height), (3, 2));
        let broken = MediaSource::Base64("не base64".into());
        assert_eq!(
            prepare(dir.path(), Uuid::now_v7(), Uuid::now_v7(), broken),
            Err(Skip::Unsupported)
        );
        let too_long = "A".repeat(usize::try_from(MAX_MEDIA_BYTES / 3 * 4).unwrap() + 8);
        assert_eq!(
            prepare(
                dir.path(),
                Uuid::now_v7(),
                Uuid::now_v7(),
                MediaSource::Base64(too_long)
            ),
            Err(Skip::TooLarge)
        );
    }

    #[test]
    fn signature_decides_type_and_header_gives_dimensions() {
        assert_eq!(sniff(&png(3, 2)), Some("image/png"));
        assert_eq!(dimensions("image/png", &png(3, 2)), Some((3, 2)));
        let gif = b"GIF89a\x0a\x00\x05\x00\x00\x00\x00".to_vec();
        assert_eq!(sniff(&gif), Some("image/gif"));
        assert_eq!(dimensions("image/gif", &gif), Some((10, 5)));
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend([0xff, 0xc0, 0x00, 0x11, 0x08, 0x01, 0xe0, 0x02, 0x80, 0x03]);
        assert_eq!(sniff(&jpeg), Some("image/jpeg"));
        assert_eq!(dimensions("image/jpeg", &jpeg), Some((640, 480)));
        let mut webp = b"RIFF\x00\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00\x00\x00\x00\x00".to_vec();
        webp.extend([0x3f, 0x01, 0x00, 0xef, 0x00, 0x00]);
        assert_eq!(sniff(&webp), Some("image/webp"));
        assert_eq!(dimensions("image/webp", &webp), Some((320, 240)));
        let mut lossless = b"RIFF\x00\x00\x00\x00WEBPVP8L\x00\x00\x00\x00\x2f".to_vec();
        lossless.extend((99u32 | (49 << 14)).to_le_bytes());
        assert_eq!(dimensions("image/webp", &lossless), Some((100, 50)));
        for other in [&b"<svg xmlns=\"\"/>"[..], b"%PDF-1.7", b"", b"\x89PNG"] {
            assert_eq!(sniff(other), None);
        }
        assert_eq!(dimensions("image/png", b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(dimensions("image/jpeg", &[0xff, 0xd8, 0xff, 0xd9]), None);
    }

    #[test]
    fn prepare_copies_privately_and_skips_large_foreign_or_special_files() {
        let root = tempfile::tempdir().unwrap();
        let exec = root.path().join("exec");
        fs::create_dir(&exec).unwrap();
        let image = root.path().join("image.png");
        let mut bytes = png(7, 9);
        bytes.extend(vec![1; 3 * MAX_FILE_CHUNK_BYTES]);
        fs::write(&image, &bytes).unwrap();
        let (id, block) = (Uuid::now_v7(), Uuid::now_v7());
        let prepared = prepare(&exec, id, block, MediaSource::Path(image.clone())).unwrap();
        assert_eq!(
            prepared,
            Prepared {
                media_id: id,
                block_id: block,
                mime: "image/png",
                size: bytes.len() as u64,
                sha256: hex(&Sha256::digest(&bytes)),
                width: 7,
                height: 9,
            }
        );
        let copy = media_path(&exec, id);
        assert_eq!(fs::read(&copy).unwrap(), bytes);
        assert_eq!(
            fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(exec.join(MEDIA_DIR))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        // Последующая правка исходного файла не меняет уже увиденное.
        fs::write(&image, b"changed").unwrap();
        assert_eq!(fs::read(&copy).unwrap(), bytes);

        let from_bytes =
            prepare(&exec, Uuid::now_v7(), block, MediaSource::Bytes(png(1, 1))).unwrap();
        assert_eq!((from_bytes.width, from_bytes.height), (1, 1));

        let large = root.path().join("large.png");
        let file = fs::File::create(&large).unwrap();
        file.set_len(MAX_MEDIA_BYTES + 1).unwrap();
        let mut head = fs::OpenOptions::new().write(true).open(&large).unwrap();
        head.write_all(&png(1, 1)).unwrap();
        let text = root.path().join("notes.txt");
        fs::write(&text, b"just text").unwrap();
        let fifo = root.path().join("pipe");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        for (source, skip) in [
            (MediaSource::Path(large), Skip::TooLarge),
            (
                MediaSource::Bytes(vec![0x89; MAX_MEDIA_BYTES as usize + 1]),
                Skip::TooLarge,
            ),
            (MediaSource::Path(text), Skip::Unsupported),
            (MediaSource::Bytes(Vec::new()), Skip::Unsupported),
            (MediaSource::Path(fifo), Skip::Unreadable),
            (
                MediaSource::Path(root.path().join("missing")),
                Skip::Unreadable,
            ),
            (
                MediaSource::Path(root.path().to_path_buf()),
                Skip::Unreadable,
            ),
        ] {
            let id = Uuid::now_v7();
            assert_eq!(prepare(&exec, id, block, source), Err(skip));
            assert!(!media_path(&exec, id).exists());
        }
        let left: Vec<_> = fs::read_dir(exec.join(MEDIA_DIR))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "only the two prepared copies remain");
    }
}
