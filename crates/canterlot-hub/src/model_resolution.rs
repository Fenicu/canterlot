//! Конкретная модель за псевдонимом. Claude Code принимает `fable[1m]` или `opus`
//! и сам выбирает версию: её называет `system/init` и каждое сообщение ассистента,
//! а воркер кладёт её в `BlockStarted.model`. Codex и OpenCode называют модель
//! полностью, у них просьба и результат совпадают.
//!
//! Первый блок основного агента в исполнении записывает пару «просили — получили»
//! в `executions`, а для псевдонима ещё и последнее наблюдение на машине
//! (`model_resolutions`) с прежним значением, если оно сменилось.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, Row};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::blocks::ExecutionRef;

/// Семейства Claude: псевдоним семейства не может разрешиться в чужое семейство.
const CLAUDE_FAMILIES: &[&str] = &["opus", "sonnet", "haiku", "fable"];

fn base(model: &str) -> &str {
    model.strip_suffix("[1m]").unwrap_or(model)
}

/// Пара «просили — получили» правдоподобна. Модель сессии меняется между ходами,
/// а блок прежней модели может прийти уже после смены: такой блок ничего не
/// записывает. `opusplan` разрешается в разные модели по режиму и не записывается.
pub fn consistent(requested: &str, resolved: &str) -> bool {
    let requested = base(requested);
    let resolved = base(resolved);
    if resolved.is_empty() || resolved == "<synthetic>" || requested == "opusplan" {
        return false;
    }
    match CLAUDE_FAMILIES.iter().find(|family| **family == requested) {
        Some(family) => resolved.contains(family),
        None => true,
    }
}

/// Просьба — псевдоним, а не полное имя: ему есть смысл помнить разрешение.
fn is_alias(requested: &str, resolved: &str) -> bool {
    !requested.is_empty() && base(requested) != base(resolved)
}

/// Первый блок основного агента с моделью записывает разрешение исполнения.
/// Следующие блоки того же исполнения с той же парой ничего не пишут.
pub async fn observe(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    resolved: &str,
) -> Result<(), sqlx::Error> {
    let Some(row) = sqlx::query(
        "select c.model, c.harness, e.model_requested, e.model_resolved from executions e \
         join conversations c on c.id = e.conversation_id where e.id = $1",
    )
    .bind(run.execution_id)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(());
    };
    let requested: String = row.try_get("model")?;
    let harness: String = row.try_get("harness")?;
    let stored_requested: Option<String> = row.try_get("model_requested")?;
    let stored_resolved: Option<String> = row.try_get("model_resolved")?;
    if !consistent(&requested, resolved)
        || (stored_requested.as_deref() == Some(requested.as_str())
            && stored_resolved.as_deref() == Some(resolved))
    {
        return Ok(());
    }
    sqlx::query("update executions set model_requested = $2, model_resolved = $3 where id = $1")
        .bind(run.execution_id)
        .bind(&requested)
        .bind(resolved)
        .execute(&mut *conn)
        .await?;
    if is_alias(&requested, resolved) {
        sqlx::query(
            "insert into model_resolutions (machine_id, harness, alias, resolved, observed_at) \
             values ($1, $2, $3, $4, clock_timestamp()) \
             on conflict (machine_id, harness, alias) do update set \
             previous = case when model_resolutions.resolved <> excluded.resolved \
               then model_resolutions.resolved else model_resolutions.previous end, \
             changed_at = case when model_resolutions.resolved <> excluded.resolved \
               then excluded.observed_at else model_resolutions.changed_at end, \
             resolved = excluded.resolved, observed_at = excluded.observed_at",
        )
        .bind(run.machine_id)
        .bind(&harness)
        .bind(&requested)
        .bind(resolved)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Конкретная модель сессии для шапки и списка: последнее исполнение на текущей
/// модели сессии, а до первого такого хода — последнее наблюдение псевдонима на
/// машине. Пусто — неизвестно.
pub async fn conversation_resolved(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    requested: &str,
) -> Result<String, sqlx::Error> {
    let resolved: Option<String> = sqlx::query_scalar(
        "select coalesce( \
           (select e.model_resolved from executions e where e.conversation_id = c.id \
              and e.model_requested = $2 and e.model_resolved is not null \
              order by e.generation desc limit 1), \
           (select r.resolved from model_resolutions r where r.machine_id = c.machine_id \
              and r.harness = c.harness and r.alias = $2)) \
         from conversations c where c.id = $1",
    )
    .bind(conversation_id)
    .bind(requested)
    .fetch_optional(&mut *conn)
    .await?
    .flatten();
    Ok(resolved.unwrap_or_default())
}

/// Последнее разрешение псевдонима на машине.
#[derive(Serialize, ToSchema, Clone, Debug, PartialEq)]
pub struct ResolutionView {
    /// Конкретная модель, например `claude-fable-5-2`.
    pub model: String,
    pub observed_at: String,
    /// Прежнее разрешение, если псевдоним его сменил.
    pub previous: Option<String>,
    /// Когда псевдоним сменил разрешение.
    pub changed_at: Option<String>,
}

/// Разрешения псевдонимов машины по `(harness, alias)`.
pub async fn machine_resolutions(
    conn: &mut PgConnection,
    machine_id: Uuid,
) -> Result<HashMap<(String, String), ResolutionView>, sqlx::Error> {
    let rows = sqlx::query(
        "select harness, alias, resolved, observed_at, previous, changed_at \
         from model_resolutions where machine_id = $1",
    )
    .bind(machine_id)
    .fetch_all(&mut *conn)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                (row.try_get("harness")?, row.try_get("alias")?),
                ResolutionView {
                    model: row.try_get("resolved")?,
                    observed_at: crate::conversations::time(row.try_get("observed_at")?),
                    previous: row.try_get("previous")?,
                    changed_at: row
                        .try_get::<Option<DateTime<Utc>>, _>("changed_at")?
                        .map(crate::conversations::time),
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_alias_must_resolve_to_its_family() {
        assert!(consistent("fable[1m]", "claude-fable-5-2"));
        assert!(consistent("opus", "claude-opus-5-5"));
        assert!(!consistent("sonnet[1m]", "claude-opus-5-5"));
        assert!(!consistent("opusplan", "claude-opus-5-5"));
        assert!(!consistent("opus", "<synthetic>"));
        assert!(!consistent("opus", ""));
        assert!(consistent("gpt-6-luna", "gpt-6-luna"));
        assert!(consistent("", "claude-opus-5-5"));
    }

    #[test]
    fn full_names_are_not_aliases() {
        assert!(is_alias("fable[1m]", "claude-fable-5-2"));
        assert!(!is_alias("claude-opus-5-5[1m]", "claude-opus-5-5"));
        assert!(!is_alias("gpt-6-luna", "gpt-6-luna"));
        assert!(!is_alias("", "claude-opus-5-5"));
    }
}
