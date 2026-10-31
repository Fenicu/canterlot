//! Каталог конфигурации исполнения. «Как у владельца» работает с его конфигурацией как
//! есть; остальные режимы заменяют глобальный каталог своим: всё содержимое каталога
//! владельца ссылками, кроме его `AGENTS.md`, и `AGENTS.md` пресета, если он есть.
//! Репозиторные `AGENTS.md` при этом действуют. Ссылки, а не `OPENCODE_CONFIG`: тот принимает
//! один файл, а у владельца могут быть `opencode.json` и `opencode.jsonc`, плюс плагины.
//!
//! Остаётся разрыв: `~/AGENTS.md` и `AGENTS.md` выше корня репозитория читаются, если рабочий
//! каталог лежит под домом (решение 4 спеки: принято, у владельца такого файла нет).

use crate::adapter::{InstructionMode, InstructionSpec};
use std::{
    ffi::OsString,
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Write as _},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, symlink},
    path::{Path, PathBuf},
};

const AGENTS: &str = "AGENTS.md";
const TEMPORARY: &str = ".AGENTS.md.tmp";

pub fn config_dir(exec_dir: &Path) -> PathBuf {
    exec_dir.join("opencode-config")
}

fn variable<'a>(env: &'a [(OsString, OsString)], key: &str) -> Option<&'a OsString> {
    env.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
}

/// Каталог конфигурации владельца: явная настройка, затем `$XDG_CONFIG_HOME/opencode`.
pub fn owner_dir(configured: Option<&Path>, env: &[(OsString, OsString)]) -> Option<PathBuf> {
    configured
        .map(Path::to_path_buf)
        .or_else(|| variable(env, "XDG_CONFIG_HOME").map(|dir| Path::new(dir).join("opencode")))
        .or_else(|| variable(env, "HOME").map(|home| Path::new(home).join(".config/opencode")))
}

// Прошлые ссылки снимаются: перезапуск мог сменить набор файлов. Настоящие файлы остаются.
fn link_owner(dir: &Path, owner: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            fs::remove_file(entry.path())?;
        }
    }
    let entries = match fs::read_dir(owner) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let name = entry?.file_name();
        if name == AGENTS {
            continue;
        }
        match symlink(owner.join(&name), dir.join(&name)) {
            Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error),
            _ => {}
        }
    }
    Ok(())
}

// Подмена целиком: сервер следит за файлом и прочитал бы половину текста.
fn write_text(dir: &Path, spec: &InstructionSpec) -> io::Result<()> {
    let agents = dir.join(AGENTS);
    if spec.mode != InstructionMode::Preset {
        return match fs::remove_file(agents) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    let temporary = dir.join(TEMPORARY);
    match fs::remove_file(&temporary) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(spec.text.as_bytes())?;
    fs::rename(temporary, agents)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)?
}

/// Готовит каталог и возвращает переменные запуска.
pub async fn prepare(
    exec_dir: &Path,
    owner: Option<PathBuf>,
    spec: &InstructionSpec,
) -> io::Result<Vec<(OsString, OsString)>> {
    if spec.mode == InstructionMode::Owner {
        rewrite(exec_dir, spec).await?;
        return Ok(Vec::new());
    }
    let dir = config_dir(exec_dir);
    let target = dir.clone();
    let spec = spec.clone();
    blocking(move || {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&target)?;
        if let Some(owner) = owner {
            link_owner(&target, &owner)?;
        }
        write_text(&target, &spec)
    })
    .await?;
    Ok(vec![
        ("OPENCODE_CONFIG_DIR".into(), dir.into_os_string()),
        ("OPENCODE_DISABLE_CLAUDE_CODE".into(), "1".into()),
    ])
}

/// Смена текста без перезапуска: сервер следит за файлом.
pub async fn rewrite(exec_dir: &Path, spec: &InstructionSpec) -> io::Result<()> {
    let dir = config_dir(exec_dir);
    let spec = spec.clone();
    blocking(move || write_text(&dir, &spec)).await
}
