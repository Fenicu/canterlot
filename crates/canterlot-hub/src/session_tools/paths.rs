//! Путь файла условия: относительный разрешается от рабочего каталога сессии,
//! после нормализации `..` не остаётся. Секретные имена и символические ссылки
//! проверяет воркер той же защитой, что у `share_file`: здесь только то, что видно
//! без файловой системы машины.

use std::path::{Component, Path};

use sqlx::PgConnection;
use uuid::Uuid;

use super::{ToolError, ToolErrorCode};

pub const MAX_PATH_BYTES: usize = canterlot_proto::limits::MAX_PROBE_PATH_BYTES;

fn not_allowed(message: &'static str) -> ToolError {
    ToolError::with_message(ToolErrorCode::PathNotAllowed, message)
}

/// Абсолютный путь без `..` и пустых компонентов. Относительный склеивается
/// с рабочим каталогом сессии; без каталога это `path_not_allowed`.
pub fn resolve(raw: &str, workdir: Option<&str>) -> Result<String, ToolError> {
    if raw.is_empty() || raw.len() > MAX_PATH_BYTES || raw.contains('\0') {
        return Err(not_allowed("The path is empty or too long"));
    }
    if raw.chars().any(char::is_control) {
        return Err(not_allowed("The path contains control characters"));
    }
    let raw = Path::new(raw);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        let workdir = workdir
            .filter(|workdir| !workdir.is_empty())
            .ok_or_else(|| not_allowed("A relative path needs the session workdir"))?;
        let workdir = Path::new(workdir);
        if !workdir.is_absolute() {
            return Err(not_allowed("The session workdir is not absolute"));
        }
        workdir.join(raw)
    };
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for component in joined.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => {}
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(not_allowed("The path must not contain .."));
            }
            Component::Normal(part) => parts.push(part),
        }
    }
    let Some((name, directories)) = parts.split_last() else {
        return Err(not_allowed("The path names no file"));
    };
    if *name == "." || *name == ".." {
        return Err(not_allowed("The path names no file"));
    }
    let mut path = std::path::PathBuf::from("/");
    path.extend(directories);
    path.push(name);
    Ok(path.to_string_lossy().into_owned())
}

/// Рабочий каталог сессии вызывающего: относительный путь считается от него.
pub async fn workdir_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("select workdir from conversations where id=$1")
        .bind(conversation_id)
        .fetch_optional(&mut *conn)
        .await
        .map(Option::flatten)
}

/// То же без открытой транзакции: RPC пробы идёт до неё.
pub async fn workdir(
    pool: &sqlx::PgPool,
    conversation_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("select workdir from conversations where id=$1")
        .bind(conversation_id)
        .fetch_optional(pool)
        .await
        .map(Option::flatten)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_path_is_normalized_without_parent_steps() {
        assert_eq!(
            resolve("/tmp/gate.rc", None).unwrap(),
            "/tmp/gate.rc".to_string()
        );
        assert_eq!(
            resolve("/tmp//./gate.rc", None).unwrap(),
            "/tmp/gate.rc".to_string()
        );
        assert_eq!(
            resolve("/tmp/../gate.rc", None).unwrap_err().code,
            ToolErrorCode::PathNotAllowed
        );
    }

    #[test]
    fn a_relative_path_needs_the_session_workdir() {
        assert_eq!(
            resolve("reports/out.md", Some("/srv/work")).unwrap(),
            "/srv/work/reports/out.md".to_string()
        );
        assert_eq!(
            resolve("out.md", None).unwrap_err().code,
            ToolErrorCode::PathNotAllowed
        );
        assert_eq!(
            resolve("out.md", Some("work")).unwrap_err().code,
            ToolErrorCode::PathNotAllowed
        );
    }

    #[test]
    fn an_escape_after_joining_is_refused() {
        assert_eq!(
            resolve("../etc/passwd", Some("/srv/work")).unwrap_err().code,
            ToolErrorCode::PathNotAllowed
        );
    }

    #[test]
    fn empty_control_and_overlong_paths_are_refused() {
        for path in ["", "/tmp/ga\0te", "/tmp/ga\nte", "/"] {
            assert_eq!(
                resolve(path, None).unwrap_err().code,
                ToolErrorCode::PathNotAllowed,
                "{path:?}"
            );
        }
        let long = format!("/{}", "a".repeat(MAX_PATH_BYTES));
        assert_eq!(
            resolve(&long, None).unwrap_err().code,
            ToolErrorCode::PathNotAllowed
        );
    }
}
