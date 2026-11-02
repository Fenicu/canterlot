//! Чтение файла, который агент отдаёт владельцу. Путь проверяется один раз:
//! открытие с `O_NOFOLLOW` и `O_NONBLOCK`, дальше только дескриптор (`fstat`):
//! обычный файл, одна жёсткая ссылка, владелец-процесс. Секретные имена общие
//! с инструкциями владельца; каталог `.ssh` и файлы больше 50 МиБ запрещены.

use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::Path,
};

use canterlot_proto::limits::MAX_SHARE_BYTES;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::owner_instructions::denied_name;
use crate::media;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareError {
    Symlink,
    HardLink,
    NotRegular,
    Denied,
    TooLarge,
    Unreadable,
    NotOwned,
}

impl ShareError {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Symlink => "share_symlink",
            Self::HardLink => "share_hard_link",
            Self::NotRegular => "share_not_regular",
            Self::Denied => "share_denied",
            Self::TooLarge => "share_too_large",
            Self::Unreadable => "share_unreadable",
            Self::NotOwned => "share_not_owned",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Symlink => "The file is a symbolic link",
            Self::HardLink => "The file has more than one hard link",
            Self::NotRegular => "Only a regular file can be shared",
            Self::Denied => "This file is on the secret deny list",
            Self::TooLarge => "The file is larger than 50 MiB",
            Self::Unreadable => "The file cannot be read",
            Self::NotOwned => "The file belongs to another user",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub id: Uuid,
    pub name: String,
    pub title: String,
    pub note: String,
    pub mime: String,
    pub size: u64,
    pub sha256: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

fn denied_path(path: &Path) -> bool {
    path.components().any(|part| part.as_os_str() == ".ssh")
        || path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(denied_name)
}

fn valid_title(title: &str) -> bool {
    title.len() <= canterlot_proto::limits::MAX_SHARE_TITLE_BYTES
        && !title.chars().any(char::is_control)
}

fn name_of(path: &Path) -> Result<String, ShareError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 255
                && !name.chars().any(char::is_control)
                && *name != "."
                && *name != ".."
        })
        .ok_or(ShareError::Unreadable)?;
    Ok(name.to_owned())
}

fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Тип по сигнатуре, затем по расширению; неизвестное отдаётся как поток байтов.
pub fn mime_for(name: &str, head: &[u8]) -> String {
    if let Some(image) = media::sniff(head) {
        return image.to_owned();
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        return "video/mp4".into();
    }
    if head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return "video/webm".into();
    }
    match extension(name).as_str() {
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
    .into()
}

/// Скопировать проверенный файл в `exec_dir/media/<id>` и посчитать SHA-256.
pub fn prepare(
    exec_dir: &Path,
    id: Uuid,
    path: &Path,
    title: String,
    note: String,
) -> Result<Prepared, ShareError> {
    if !valid_title(&title) || note.len() > canterlot_proto::limits::MAX_SHARE_NOTE_BYTES {
        return Err(ShareError::Unreadable);
    }
    let name = name_of(path)?;
    if denied_path(path) {
        return Err(ShareError::Denied);
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
        .map_err(|error| {
            if error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
                ShareError::Symlink
            } else {
                ShareError::Unreadable
            }
        })?;
    let meta = file.metadata().map_err(|_| ShareError::Unreadable)?;
    if meta.file_type().is_symlink() {
        return Err(ShareError::Symlink);
    }
    if !meta.is_file() {
        return Err(ShareError::NotRegular);
    }
    if meta.nlink() != 1 {
        return Err(ShareError::HardLink);
    }
    if meta.uid() != rustix::process::getuid().as_raw() {
        return Err(ShareError::NotOwned);
    }
    if meta.len() > MAX_SHARE_BYTES {
        return Err(ShareError::TooLarge);
    }
    let directory = exec_dir.join(media::MEDIA_DIR);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .map_err(|_| ShareError::Unreadable)?;
    let temporary = directory.join(format!(".{id}.share"));
    let result = (|| {
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| ShareError::Unreadable)?;
        let mut reader = file.take(MAX_SHARE_BYTES + 1);
        let mut hash = Sha256::new();
        let mut head = Vec::with_capacity(512);
        let mut buffer = vec![0; 128 * 1024];
        let mut size = 0u64;
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|_| ShareError::Unreadable)?;
            if read == 0 {
                break;
            }
            size += read as u64;
            if size > MAX_SHARE_BYTES {
                return Err(ShareError::TooLarge);
            }
            hash.update(&buffer[..read]);
            if head.len() < 512 {
                let take = read.min(512 - head.len());
                head.extend_from_slice(&buffer[..take]);
            }
            out.write_all(&buffer[..read])
                .map_err(|_| ShareError::Unreadable)?;
        }
        out.sync_all().map_err(|_| ShareError::Unreadable)?;
        fs::rename(&temporary, media::media_path(exec_dir, id))
            .map_err(|_| ShareError::Unreadable)?;
        let mime = mime_for(&name, &head);
        let (width, height) =
            media::dimensions(&mime, &head).map_or((None, None), |(w, h)| (Some(w), Some(h)));
        Ok(Prepared {
            id,
            name,
            title,
            note,
            mime,
            size,
            sha256: hash
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            width,
            height,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(media::media_path(exec_dir, id));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn refuses_symlink() {
        let dir = dir();
        let target = dir.path().join("real.txt");
        fs::write(&target, b"secret").unwrap();
        let link = dir.path().join("link.txt");
        symlink(&target, &link).unwrap();
        assert_eq!(
            prepare(
                dir.path(),
                Uuid::now_v7(),
                &link,
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::Symlink
        );
    }

    #[test]
    fn refuses_hard_link() {
        let dir = dir();
        let first = dir.path().join("one.txt");
        fs::write(&first, b"secret").unwrap();
        let second = dir.path().join("two.txt");
        fs::hard_link(&first, &second).unwrap();
        assert_eq!(
            prepare(
                dir.path(),
                Uuid::now_v7(),
                &first,
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::HardLink
        );
    }

    #[test]
    fn refuses_fifo() {
        let dir = dir();
        let fifo = dir.path().join("pipe");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_bits_truncate(0o644),
            0,
        )
        .unwrap();
        assert_eq!(
            prepare(
                dir.path(),
                Uuid::now_v7(),
                &fifo,
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::NotRegular
        );
    }

    #[test]
    fn refuses_denied_names_and_ssh() {
        let dir = dir();
        for name in [".env", "auth.json", ".credentials.json"] {
            let path = dir.path().join(name);
            fs::write(&path, b"secret").unwrap();
            assert_eq!(
                prepare(
                    dir.path(),
                    Uuid::now_v7(),
                    &path,
                    String::new(),
                    String::new()
                )
                .unwrap_err(),
                ShareError::Denied
            );
        }
        let ssh = dir.path().join(".ssh");
        fs::create_dir(&ssh).unwrap();
        let key = ssh.join("id_ed25519");
        fs::write(&key, b"secret").unwrap();
        assert_eq!(
            prepare(
                dir.path(),
                Uuid::now_v7(),
                &key,
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::Denied
        );
    }

    #[test]
    fn refuses_too_large() {
        let dir = dir();
        let path = dir.path().join("big.bin");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_SHARE_BYTES + 1).unwrap();
        assert_eq!(
            prepare(
                dir.path(),
                Uuid::now_v7(),
                &path,
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::TooLarge
        );
    }

    #[test]
    fn copies_a_regular_file_and_hashes_it() {
        let dir = dir();
        let path = dir.path().join("note.txt");
        fs::write(&path, b"hello").unwrap();
        let prepared = prepare(
            dir.path(),
            Uuid::now_v7(),
            &path,
            "Title".into(),
            "Note".into(),
        )
        .unwrap();
        assert_eq!(prepared.name, "note.txt");
        assert_eq!(prepared.mime, "text/plain");
        assert_eq!(prepared.size, 5);
        assert!(media::media_path(dir.path(), prepared.id).exists());
        assert_eq!(
            fs::metadata(media::media_path(dir.path(), prepared.id))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
    }
}
