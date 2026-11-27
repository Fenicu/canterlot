//! Удаление деревьев от root без перехода по символическим ссылкам. Каталоги данных
//! принадлежат хабу и postgres: подложенная в них ссылка не должна увести удаление наружу.
//! Путь проходится дескрипторами (`openat` с `O_NOFOLLOW`), содержимое удаляется
//! `unlinkat` относительно дескриптора своего каталога.

use std::{
    ffi::{CStr, OsStr},
    os::{fd::OwnedFd, unix::ffi::OsStrExt},
    path::{Component, Path},
};

use rustix::{
    fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, openat, statat, unlinkat},
    io::Errno,
};

use crate::error::CtlError;

fn errno(err: Errno, path: &Path) -> CtlError {
    CtlError::Other(format!("remove_failed: {}: {err}", path.display()))
}

fn open_dir(parent: &OwnedFd, name: &OsStr) -> Result<OwnedFd, Errno> {
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

fn clear_dir(dir: &OwnedFd, path: &Path) -> Result<(), CtlError> {
    let mut names: Vec<Vec<u8>> = Vec::new();
    let reader = Dir::read_from(dir).map_err(|e| errno(e, path))?;
    for entry in reader {
        let entry = entry.map_err(|e| errno(e, path))?;
        let name: &CStr = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        names.push(bytes.to_vec());
    }
    for name in names {
        let name = OsStr::from_bytes(&name);
        remove_entry(dir, name, &path.join(name))?;
    }
    Ok(())
}

fn remove_entry(parent: &OwnedFd, name: &OsStr, path: &Path) -> Result<(), CtlError> {
    let stat = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| errno(e, path))?;
    if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
        let dir = open_dir(parent, name).map_err(|e| errno(e, path))?;
        clear_dir(&dir, path)?;
        unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(|e| errno(e, path))?;
    } else {
        // Ссылка, файл, сокет: удаляется сама запись каталога.
        unlinkat(parent, name, AtFlags::empty()).map_err(|e| errno(e, path))?;
    }
    Ok(())
}

/// Удаляет `path` (лежащий под доверенным `base`) вместе с содержимым. Ссылка на месте
/// `path` удаляется сама, ссылка в промежуточном компоненте — отказ. false — пути уже нет.
pub fn remove_tree(base: &Path, path: &Path) -> Result<bool, CtlError> {
    let rel = path
        .strip_prefix(base)
        .map_err(|_| CtlError::Other(format!("path_outside_base: {}", path.display())))?;
    let parts: Vec<&OsStr> = rel
        .components()
        .map(|c| match c {
            Component::Normal(part) => Ok(part),
            _ => Err(CtlError::Other(format!(
                "path_not_normal: {}",
                path.display()
            ))),
        })
        .collect::<Result<_, _>>()?;
    let Some((last, dirs)) = parts.split_last() else {
        return Err(CtlError::Other(format!(
            "refusing to remove the base itself: {}",
            path.display()
        )));
    };
    let mut current = openat(
        CWD,
        base,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| errno(e, base))?;
    let mut walked = base.to_path_buf();
    for part in dirs {
        walked.push(part);
        current = match open_dir(&current, part) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(false),
            // ELOOP/ENOTDIR: промежуточный компонент — ссылка или не каталог.
            Err(err) => return Err(errno(err, &walked)),
        };
    }
    match statat(&current, *last, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => Ok(false),
        Err(err) => Err(errno(err, path)),
        Ok(_) => {
            remove_entry(&current, last, path)?;
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn remove_tree_keeps_link_targets() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let outside = dir.path().join("outside");
        fs::create_dir_all(base.join("data/deep")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep"), "x").unwrap();
        fs::write(base.join("data/deep/file"), "y").unwrap();
        symlink(&outside, base.join("data/deep/link")).unwrap();
        symlink(outside.join("keep"), base.join("data/filelink")).unwrap();
        assert!(remove_tree(&base, &base.join("data")).unwrap());
        assert!(!base.join("data").exists());
        assert!(outside.join("keep").is_file());
        assert!(!remove_tree(&base, &base.join("data")).unwrap());
    }

    #[test]
    fn remove_tree_refuses_symlinked_parent() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let outside = dir.path().join("outside");
        fs::create_dir_all(outside.join("sub")).unwrap();
        fs::create_dir_all(&base).unwrap();
        symlink(&outside, base.join("link")).unwrap();
        assert!(remove_tree(&base, &base.join("link/sub")).is_err());
        assert!(outside.join("sub").is_dir());
        // Сама ссылка удаляется, цель остаётся.
        assert!(remove_tree(&base, &base.join("link")).unwrap());
        assert!(outside.join("sub").is_dir());
        assert!(remove_tree(&base, &base).is_err());
        assert!(remove_tree(&base, &base.join("../outside")).is_err());
    }
}
