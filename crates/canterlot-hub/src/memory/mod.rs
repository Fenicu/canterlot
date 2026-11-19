//! Общая память агентов: область (домен и проект) каждого разговора.
//!
//! Здесь только часть про область. Домен ставится при создании строки и не
//! пересчитывается; единственное исключение — проверка запуска, которая обнуляет домен,
//! если настоящий путь рабочего каталога ему противоречит.

pub mod secrets;
pub mod store;

use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::events::{self, NewEvent, Scope};

/// Возможность воркера, без которой домен у разговора пуст.
pub const CAPABILITY_MEMORY_SCOPE_V1: &str = "memory_scope_v1";

/// Событие, которое видит владелец в сведениях о сессии.
pub const EVENT_DOMAIN_CONFLICT: &str = "memory_domain_conflict";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classification {
    Work,
    Personal,
    Neutral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryDomain {
    Work,
    Personal,
}

impl MemoryDomain {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Personal => "personal",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "work" => Some(Self::Work),
            "personal" => Some(Self::Personal),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScopeError {
    /// Каталог потомка не нейтрален и не из домена родителя.
    #[error("domain mismatch")]
    DomainMismatch,
    #[error("database error")]
    Db(#[from] sqlx::Error),
}

/// Путь без хвостовых `/` и `.`; `..` остаётся как есть, и такой путь не считается вложенным.
fn normalize(path: &str) -> PathBuf {
    Path::new(path)
        .components()
        .filter(|c| *c != Component::CurDir)
        .collect()
}

fn under(path: &Path, root: &str) -> bool {
    let root = normalize(root);
    root.is_absolute() && path.starts_with(&root)
}

/// Лексическая классификация: путь равен корню работы или лежит под ним — `Work`; под
/// базой рабочих копий — `Neutral`; иначе `Personal`. Каталог с `..` вложенным не считается
/// ни в какой корень: `Personal`.
pub fn classify(
    workdir: &str,
    work_roots: &[String],
    worktree_root: Option<&str>,
) -> Classification {
    let path = normalize(workdir);
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Classification::Personal;
    }
    if worktree_root.is_some_and(|root| under(&path, root)) {
        return Classification::Neutral;
    }
    if work_roots.iter().any(|root| under(&path, root)) {
        return Classification::Work;
    }
    Classification::Personal
}

/// Корни и база рабочих копий машины; `None`, если воркер не умеет `memory_scope_v1`
/// или ещё не присылал корни.
struct MachineRoots {
    work_roots: Vec<String>,
    worktree_root: Option<String>,
}

async fn machine_roots(
    conn: &mut PgConnection,
    machine_id: Uuid,
) -> Result<Option<MachineRoots>, sqlx::Error> {
    let row: Option<(Vec<String>, Option<Value>, Option<String>)> = sqlx::query_as(
        "select capabilities, memory_work_roots, worktree_root from machines where id=$1",
    )
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((capabilities, roots, worktree_root)) = row else {
        return Ok(None);
    };
    if !capabilities.iter().any(|c| c == CAPABILITY_MEMORY_SCOPE_V1) {
        return Ok(None);
    }
    let Some(roots) = roots.as_ref().and_then(Value::as_array) else {
        return Ok(None);
    };
    Ok(Some(MachineRoots {
        work_roots: roots
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        worktree_root,
    }))
}

pub type Scoped = (Option<MemoryDomain>, Option<String>);

fn project_of(workdir: &str) -> String {
    normalize(workdir).to_string_lossy().into_owned()
}

/// Область корневой сессии: новая, из расписания или продолжение истории. Пусто у старого
/// воркера, у машины без корней и у нейтрального каталога.
pub async fn root_scope(
    conn: &mut PgConnection,
    machine_id: Uuid,
    workdir: &str,
) -> Result<Scoped, sqlx::Error> {
    let Some(roots) = machine_roots(conn, machine_id).await? else {
        return Ok((None, None));
    };
    Ok(
        match classify(workdir, &roots.work_roots, roots.worktree_root.as_deref()) {
            Classification::Work => (Some(MemoryDomain::Work), Some(project_of(workdir))),
            Classification::Personal => (Some(MemoryDomain::Personal), Some(project_of(workdir))),
            Classification::Neutral => (None, None),
        },
    )
}

/// Область потомка (делегат, форк, копия): домен и проект родителя. Родитель без домена
/// даёт потомка без домена. Явный каталог потомка должен быть нейтральным или из домена
/// родителя, иначе `DomainMismatch`.
pub async fn child_scope(
    conn: &mut PgConnection,
    parent_id: Uuid,
    child_workdir: &str,
    machine_id: Uuid,
) -> Result<Scoped, ScopeError> {
    let parent: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("select memory_domain, memory_project from conversations where id=$1")
            .bind(parent_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((Some(domain), project)) = parent else {
        return Ok((None, None));
    };
    let Some(domain) = MemoryDomain::parse(&domain) else {
        return Ok((None, None));
    };
    // Корни неизвестны: проверить каталог нечем, потомок остаётся без памяти.
    let Some(roots) = machine_roots(conn, machine_id).await? else {
        return Ok((None, None));
    };
    match classify(
        child_workdir,
        &roots.work_roots,
        roots.worktree_root.as_deref(),
    ) {
        Classification::Neutral => {}
        Classification::Work if domain == MemoryDomain::Work => {}
        Classification::Personal if domain == MemoryDomain::Personal => {}
        _ => return Err(ScopeError::DomainMismatch),
    }
    Ok((Some(domain), project))
}

/// Проверка запуска: настоящий путь, который прислал воркер, противоречит сохранённому
/// домену. Тогда память разговора выключается, событие остаётся владельцу, а запуск идёт
/// дальше. Нейтральный настоящий путь домену не противоречит. Возвращает `true`, если
/// домен обнулён.
pub async fn recheck_launch(
    conn: &mut PgConnection,
    execution_id: Uuid,
    canonical_workdir: &str,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    if canonical_workdir.is_empty() {
        return Ok(false);
    }
    let row: Option<(Uuid, Uuid, Option<String>)> = sqlx::query_as(
        "select c.id, c.machine_id, c.memory_domain
           from executions e join conversations c on c.id=e.conversation_id
          where e.id=$1 for update of c",
    )
    .bind(execution_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((conversation_id, machine_id, Some(stored))) = row else {
        return Ok(false);
    };
    let Some(stored) = MemoryDomain::parse(&stored) else {
        return Ok(false);
    };
    let Some(roots) = machine_roots(conn, machine_id).await? else {
        return Ok(false);
    };
    let actual = classify(
        canonical_workdir,
        &roots.work_roots,
        roots.worktree_root.as_deref(),
    );
    let contradicts = matches!(
        (actual, stored),
        (Classification::Work, MemoryDomain::Personal)
            | (Classification::Personal, MemoryDomain::Work)
    );
    if !contradicts {
        return Ok(false);
    }
    sqlx::query(
        "update conversations
            set memory_domain=null, memory_domain_conflict_at=clock_timestamp()
          where id=$1",
    )
    .bind(conversation_id)
    .execute(&mut *conn)
    .await?;
    // Путь и корни в событие не кладутся: хватает домена, который был.
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: conversation_id,
            },
            kind: EVENT_DOMAIN_CONFLICT,
            conversation_id: Some(conversation_id),
            execution_id: Some(execution_id),
            payload: json!({"was": stored.as_str(), "now": match actual {
                Classification::Work => "work",
                _ => "personal",
            }}),
            correlation_id,
        },
    )
    .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_lexical() {
        let roots = vec!["/h/work".to_string()];
        let base = Some("/h/data/worktrees");
        assert_eq!(classify("/h/work", &roots, base), Classification::Work);
        assert_eq!(classify("/h/work/x/", &roots, base), Classification::Work);
        assert_eq!(
            classify("/h/workshop", &roots, base),
            Classification::Personal
        );
        assert_eq!(
            classify("/h/work/../x", &roots, base),
            Classification::Personal
        );
        assert_eq!(
            classify("/h/data/worktrees/0190", &roots, base),
            Classification::Neutral
        );
        assert_eq!(classify("relative", &roots, base), Classification::Personal);
    }
}
