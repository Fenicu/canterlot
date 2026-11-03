//! Открытие по уже проверенному пути без перехода по ссылкам. Путь проверяется
//! раскрытым (`canonicalize`), а открывается от корня по одному компоненту с
//! `O_NOFOLLOW`: каталог, подменённый ссылкой после проверки, даёт ошибку, а не
//! чужое содержимое. Дальше всё идёт от дескриптора каталога.

use std::{
    fs::File,
    os::fd::OwnedFd,
    path::{Component, Path},
};

use rustix::{
    fs::{FileType, Mode, OFlags},
    io::Errno,
};

/// Каталог по абсолютному раскрытому пути. Ссылка в любом компоненте даёт
/// `ELOOP` или `ENOTDIR`, `.` и `..` недопустимы: путь уже раскрыт.
pub(crate) fn open_dir(resolved: &Path) -> Result<OwnedFd, Errno> {
    let mut components = resolved.components();
    if components.next() != Some(Component::RootDir) {
        return Err(Errno::INVAL);
    }
    let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir = rustix::fs::open("/", flags, Mode::empty())?;
    for component in components {
        let Component::Normal(name) = component else {
            return Err(Errno::INVAL);
        };
        dir = rustix::fs::openat(&dir, name, flags, Mode::empty())?;
        // O_PATH с O_NOFOLLOW открыл бы саму ссылку: тип сверяется по дескриптору.
        if FileType::from_raw_mode(rustix::fs::fstat(&dir)?.st_mode) != FileType::Directory {
            return Err(Errno::LOOP);
        }
    }
    Ok(dir)
}

/// Файл `name` в каталоге-дескрипторе: только чтение, без ссылок и без
/// блокировки на FIFO.
pub(crate) fn open_file(dir: &OwnedFd, name: &str) -> Result<File, Errno> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(Errno::INVAL);
    }
    rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
}

/// Ссылка на месте каталога или файла.
pub(crate) fn is_link_error(error: Errno) -> bool {
    error == Errno::LOOP || error == Errno::NOTDIR
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, os::unix::fs::symlink};

    #[test]
    fn opens_a_real_directory_and_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("a/b");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("f.txt"), b"ok").unwrap();
        let resolved = std::fs::canonicalize(&inner).unwrap();
        let fd = open_dir(&resolved).unwrap();
        let mut text = String::new();
        open_file(&fd, "f.txt")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "ok");
    }

    #[test]
    fn a_directory_swapped_for_a_link_after_the_check_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret");
        std::fs::create_dir(&secret).unwrap();
        std::fs::write(secret.join("key"), b"private").unwrap();
        let checked = dir.path().join("d");
        std::fs::create_dir(&checked).unwrap();
        let resolved = std::fs::canonicalize(&checked).unwrap();
        // Проверка прошла по настоящему каталогу, затем его подменили ссылкой.
        std::fs::rename(&checked, dir.path().join("d.old")).unwrap();
        symlink(&secret, &checked).unwrap();
        let error = open_dir(&resolved).unwrap_err();
        assert!(is_link_error(error), "{error:?}");
        let error = open_dir(&resolved.join("inner")).unwrap_err();
        assert!(is_link_error(error), "{error:?}");
    }

    #[test]
    fn a_final_link_and_relative_parts_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::write(resolved.join("real"), b"x").unwrap();
        symlink(resolved.join("real"), resolved.join("link")).unwrap();
        let fd = open_dir(&resolved).unwrap();
        assert!(is_link_error(open_file(&fd, "link").unwrap_err()));
        assert_eq!(open_file(&fd, "..").unwrap_err(), Errno::INVAL);
        assert_eq!(open_dir(Path::new("relative")).unwrap_err(), Errno::INVAL);
        assert_eq!(
            open_dir(&resolved.join("..").join("x")).unwrap_err(),
            Errno::INVAL
        );
    }
}
