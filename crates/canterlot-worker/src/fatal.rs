//! Файл `<data_dir>/fatal`: одно слово о том, почему воркер вышел и сам не оправится.
//!
//! Воркер раннера живёт без службы и без человека рядом; его носитель читает слово, чтобы
//! выбрать между запросом нового токена и правкой конфига, а не перезапускать вслепую.
//! Обычный запуск файл убирает.

use std::{fs, io, path::Path};

pub const FILE_NAME: &str = "fatal";

/// Токен отозван или не принят: нужен новый.
pub const UNAUTHENTICATED: &str = "unauthenticated";
/// Поколение восстановления изменилось: нужен новый токен со сбросом локального состояния.
pub const RESTORE_GENERATION: &str = "restore_generation";
/// Конфигурация негодна: носитель пишет её заново.
pub const CONFIG: &str = "config";

/// Записывает слово причины; сбой записи не мешает выходу.
pub fn write(data_dir: &Path, reason: &str) {
    let result = (|| -> io::Result<()> {
        fs::create_dir_all(data_dir)?;
        let temp = data_dir.join(format!("{FILE_NAME}.new"));
        fs::write(&temp, reason)?;
        fs::rename(temp, data_dir.join(FILE_NAME))
    })();
    if let Err(error) = result {
        tracing::warn!(kind = ?error.kind(), "fatal reason could not be written");
    }
}

/// Убирает файл при обычном запуске.
pub fn clear(data_dir: &Path) {
    match fs::remove_file(data_dir.join(FILE_NAME)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(kind = ?error.kind(), "stale fatal file could not be removed"),
    }
}

/// Каталог данных из конфига, который не разобрался целиком: нужен, чтобы оставить слово и
/// для негодной конфигурации.
pub fn data_dir_hint(config_text: &str) -> Option<std::path::PathBuf> {
    let table: toml::Table = toml::from_str(config_text).ok()?;
    let dir = table.get("data_dir")?.as_str()?;
    let path = std::path::PathBuf::from(dir);
    path.is_absolute().then_some(path)
}
