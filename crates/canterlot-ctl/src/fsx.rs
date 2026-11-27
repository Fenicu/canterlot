//! Запись файлов установки. Утилита работает от root и пишет в том числе в домашний
//! каталог учётной записи воркера, поэтому ни один путь не проходит через символическую ссылку.

use std::{
    fmt::Write as _,
    fs,
    io::{self, Write as _},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::error::CtlError;

/// Создаёт недостающие каталоги от `base` до `path` с `mode` и возвращает созданные
/// (сверху вниз). `base` доверенный (корень установки или домашний каталог); символическая
/// ссылка ниже него — отказ: root не должен идти по ссылке, подложенной владельцем каталога.
pub fn ensure_dir(base: &Path, path: &Path, mode: u32) -> Result<Vec<PathBuf>, CtlError> {
    let rel = path
        .strip_prefix(base)
        .map_err(|_| CtlError::Other(format!("path_outside_base: {}", path.display())))?;
    let mut created = Vec::new();
    let mut current = base.to_path_buf();
    for part in rel.components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(CtlError::Other(format!(
                    "symlink_refused: {}",
                    current.display()
                )));
            }
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(CtlError::Other(format!(
                    "not_a_directory: {}",
                    current.display()
                )));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                fs::DirBuilder::new().mode(mode).create(&current)?;
                fs::set_permissions(&current, fs::Permissions::from_mode(mode))?;
                created.push(current.clone());
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(created)
}

/// Атомарная запись с правами с момента создания: секрет ни на миг не читается другими.
pub fn write_atomic_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<(), CtlError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let tmp = parent.join(format!(".{name}.{}.tmp", Uuid::now_v7()));
    let mut file = fs::File::options()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    // rename заменяет саму ссылку, если она там была, и не идёт по ней.
    fs::rename(&tmp, path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Записывает файл, только если его ещё нет; существующий не трогает и возвращает false.
pub fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<bool, CtlError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(CtlError::Other(format!(
            "symlink_refused: {}",
            path.display()
        ))),
        Ok(_) => Ok(false),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            write_atomic_mode(path, bytes, mode)?;
            Ok(true)
        }
        Err(err) => Err(err.into()),
    }
}

/// 32 случайных байта в hex: пароль базы, секрет прокси.
pub fn random_hex() -> Result<String, CtlError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| CtlError::Other(format!("random_failed: {err}")))?;
    let mut out = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}

/// Относительная ссылка `link -> target`, заменяемая атомарно.
pub fn replace_symlink(target: &Path, link: &Path) -> Result<(), CtlError> {
    let parent = link.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(".link.{}.tmp", Uuid::now_v7()));
    std::os::unix::fs::symlink(target, &tmp)?;
    fs::rename(&tmp, link)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_dir_refuses_planted_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let home = dir.path().join("home");
        fs::create_dir(&home).unwrap();
        std::os::unix::fs::symlink(&outside, home.join(".config")).unwrap();
        assert!(ensure_dir(&home, &home.join(".config/canterlot"), 0o700).is_err());
        assert!(!outside.join("canterlot").exists());
    }

    #[test]
    fn write_new_keeps_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        assert!(write_new(&path, b"one", 0o600).unwrap());
        assert!(!write_new(&path, b"two", 0o600).unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"one");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
