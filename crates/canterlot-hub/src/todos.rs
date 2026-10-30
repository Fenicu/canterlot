//! Список задач агента: зеркало собственного плана харнесса. Хаб хранит последний
//! полный список каждой дорожки; владелец его не редактирует.

use std::fmt;

use canterlot_proto::worker::v1::{TodoStatus as FactStatus, TodoUpdated};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, types::Json};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    blocks::ExecutionRef,
    events::{self, NewEvent, Scope},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TodoItemView {
    pub text: String,
    pub status: TodoStatus,
}

impl fmt::Debug for TodoItemView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TodoItemView")
            .field("text_bytes", &self.text.len())
            .field("status", &self.status)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TodoListView {
    /// Блок вызова субагента; у основного агента `null`.
    pub lane_block_id: Option<Uuid>,
    /// Растёт с каждым принятым обновлением дорожки.
    pub revision: i64,
    pub items: Vec<TodoItemView>,
    /// Сколько пунктов не поместилось в запись воркера.
    pub omitted: i64,
}

#[derive(sqlx::FromRow)]
struct TodoRow {
    lane_block_id: Option<Uuid>,
    revision: i64,
    items: Json<Vec<TodoItemView>>,
    omitted: i64,
}

fn invalid(what: &str) -> sqlx::Error {
    sqlx::Error::InvalidArgument(format!("invalid todo_updated: {what}"))
}

/// Незнакомое состояние и `\0` не должны останавливать журнал: запись повторялась бы вечно.
fn item_view(text: &str, status: i32) -> TodoItemView {
    TodoItemView {
        text: text.replace('\0', ""),
        status: match FactStatus::try_from(status) {
            Ok(FactStatus::InProgress) => TodoStatus::InProgress,
            Ok(FactStatus::Completed) => TodoStatus::Completed,
            _ => TodoStatus::Pending,
        },
    }
}

/// `true`, если запись устарела и ничего не изменила. Пакет SJ уже держит `queue_state`
/// разговора, поэтому строка дорожки меняется без отдельной блокировки.
pub async fn apply(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &TodoUpdated,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let generation = i64::try_from(fact.generation).map_err(|_| invalid("generation"))?;
    let source_revision = i64::try_from(fact.revision).map_err(|_| invalid("revision"))?;
    let lane = (!fact.lane_block_id.is_empty())
        .then(|| Uuid::parse_str(&fact.lane_block_id))
        .transpose()
        .map_err(|_| invalid("lane"))?;
    let items: Vec<TodoItemView> = fact
        .items
        .iter()
        .map(|item| item_view(&item.text, item.status))
        .collect();
    let omitted = i64::from(fact.omitted);
    let revision: Option<i64> = sqlx::query_scalar(
        "insert into agent_todos as t
             (conversation_id, lane_block_id, generation, source_revision, revision, items, omitted)
         values ($1, $2, $3, $4, 1, $5, $6)
         on conflict (conversation_id, lane_block_id) do update
         set generation = excluded.generation,
             source_revision = excluded.source_revision,
             revision = t.revision + 1,
             items = excluded.items,
             omitted = excluded.omitted,
             updated_at = clock_timestamp()
         where (t.generation, t.source_revision) < (excluded.generation, excluded.source_revision)
         returning revision",
    )
    .bind(run.conversation_id)
    .bind(lane)
    .bind(generation)
    .bind(source_revision)
    .bind(Json(&items))
    .bind(omitted)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(revision) = revision else {
        return Ok(true);
    };
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: run.conversation_id,
            },
            kind: "todo_updated",
            conversation_id: Some(run.conversation_id),
            execution_id: Some(run.execution_id),
            payload: json!({
                "lane_block_id": lane,
                "revision": revision,
                "items": items,
                "omitted": omitted,
            }),
            correlation_id,
        },
    )
    .await?;
    Ok(false)
}

/// Дорожки разговора: основной агент первым.
pub async fn list(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Vec<TodoListView>, sqlx::Error> {
    let rows: Vec<TodoRow> = sqlx::query_as(
        "select lane_block_id, revision, items, omitted from agent_todos
         where conversation_id = $1 order by lane_block_id nulls first",
    )
    .bind(conversation_id)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| TodoListView {
            lane_block_id: row.lane_block_id,
            revision: row.revision,
            items: row.items.0,
            omitted: row.omitted,
        })
        .collect())
}
