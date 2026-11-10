//! Атомарная запись исполняемых двойников для тестов.
//!
//! Прямая перезапись исполняемого файла, который уже выполняется, даёт
//! ETXTBSY; окно между записью тела и `chmod` — отказ в доступе при
//! конкурентном запуске. Запись во временный файл в том же каталоге и
//! `rename` убирают оба класса: новый запуск видит либо старый файл целиком,
//! либо новый целиком и уже с готовыми правами.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Записать исполняемый файл двойника атомарно: временный файл в том же
/// каталоге (уникальное имя на процесс, поток и счётчик), права, затем
/// `rename` поверх назначения. Паника при любой ошибке: тесты падают громко.
pub fn write_executable(path: &Path, contents: impl AsRef<[u8]>, mode: u32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).unwrap();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(
        ".fake-exec-{}-{:?}-{n}.tmp",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&tmp, contents).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode)).unwrap();
    std::fs::rename(&tmp, path).unwrap();
    path.to_path_buf()
}
