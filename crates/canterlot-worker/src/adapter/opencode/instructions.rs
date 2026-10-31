//! Каталог конфигурации исполнения. «Как у владельца» работает с его конфигурацией как
//! есть; остальные режимы заменяют глобальный каталог своим, с `AGENTS.md` пресета или
//! пустым. Репозиторные `AGENTS.md` при этом действуют.

use crate::adapter::{InstructionMode, InstructionSpec};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub fn config_dir(exec_dir: &Path) -> PathBuf {
    exec_dir.join("opencode-config")
}

/// Готовит каталог и возвращает переменные запуска.
pub async fn prepare(
    exec_dir: &Path,
    spec: &InstructionSpec,
) -> std::io::Result<Vec<(OsString, OsString)>> {
    if spec.mode == InstructionMode::Owner {
        return Ok(Vec::new());
    }
    let dir = config_dir(exec_dir);
    tokio::fs::create_dir_all(&dir).await?;
    let agents = dir.join("AGENTS.md");
    match spec.mode {
        InstructionMode::Preset => tokio::fs::write(&agents, &spec.text).await?,
        _ => match tokio::fs::remove_file(&agents).await {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        },
    }
    Ok(vec![
        ("OPENCODE_CONFIG_DIR".into(), dir.into_os_string()),
        ("OPENCODE_DISABLE_CLAUDE_CODE".into(), "1".into()),
    ])
}
