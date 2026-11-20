//! Рабочий каталог задачи: проверка и закрепление.
//!
//! Каталог проверяется дважды: воркером до запуска оболочки и самой оболочкой после выдачи
//! аренды (ожидание в очереди может длиться сколько угодно, за это время путь могли подменить).
//! Проверка идёт по открытому дескриптору, а не по имени, и тот же дескриптор становится
//! рабочим каталогом команды, поэтому между проверкой и запуском подменить нечего.

use std::{
    fs, io,
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
};

use rustix::fs::{Mode, OFlags};

/// Каталог открыт и проверен: абсолютный путь совпадает со своим каноническим видом (ни
/// ссылок, ни `..`, ни лишних слэшей) и лежит внутри разрешённых корней. Возвращённый
/// дескриптор указывает на тот самый каталог, который проверен.
pub fn pin(workdir: &str, roots: &[PathBuf]) -> Result<OwnedFd, Refused> {
    let path = Path::new(workdir);
    if !path.is_absolute() {
        return Err(Refused);
    }
    // `NOFOLLOW` отказывает ссылке в последнем звене; ссылки в середине пути ловит сверка ниже.
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| Refused)?;
    // Ядро называет путь открытого каталога без ссылок: расхождение с заданным — подмена.
    let real = fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd())).map_err(|_| Refused)?;
    // Сравнение строк, а не компонентов: `Path` считает `/a//b` и `/a/b` равными.
    if real.as_os_str() != path.as_os_str() {
        return Err(Refused);
    }
    let inside = roots
        .iter()
        .any(|root| root.canonicalize().is_ok_and(|root| real.starts_with(root)));
    if inside { Ok(fd) } else { Err(Refused) }
}

/// Каталог не прошёл проверку: подробности наружу не идут.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused;

/// Делает каталог рабочим каталогом этого процесса; потомки наследуют его без обращения к пути.
pub fn enter(fd: &OwnedFd) -> io::Result<()> {
    rustix::process::fchdir(fd).map_err(io::Error::from)
}
