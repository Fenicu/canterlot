//! Фоновые задачи агента: зеркало списка CLI. Хаб хранит последнее состояние каждой задачи;
//! живыми считаются только задачи живого исполнения, с процессом CLI они умирают.

use canterlot_proto::worker::v1::{
    BackgroundTask as FactTask, BackgroundTaskKind, BackgroundTaskState, BackgroundWorkChanged,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    blocks::{self, ExecutionRef},
    events::{self, NewEvent, Scope},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct BackgroundTaskView {
    pub task_id: String,
    /// `agent` или `bash`.
    pub kind: String,
    /// Блок вызова Agent или Bash.
    pub block_id: Option<Uuid>,
    pub parent_task_id: Option<String>,
    pub description: String,
    /// `running`, `completed`, `failed` или `stopped`.
    pub state: String,
    #[serde(with = "crate::model::utc_time")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "crate::model::optional_utc_time")]
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
struct TaskRow {
    task_id: String,
    kind: String,
    block_id: Option<Uuid>,
    parent_task_id: Option<String>,
    description: String,
    state: String,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
}

impl From<TaskRow> for BackgroundTaskView {
    fn from(row: TaskRow) -> Self {
        Self {
            task_id: row.task_id,
            kind: row.kind,
            block_id: row.block_id,
            parent_task_id: row.parent_task_id,
            description: row.description,
            state: row.state,
            started_at: row.started_at,
            ended_at: row.ended_at,
        }
    }
}

fn invalid(what: &str) -> sqlx::Error {
    sqlx::Error::InvalidArgument(format!("invalid background_work_changed: {what}"))
}

/// Незнакомое значение не должно останавливать журнал: запись повторялась бы вечно.
fn state_name(state: i32) -> &'static str {
    match BackgroundTaskState::try_from(state) {
        Ok(BackgroundTaskState::Running) => "running",
        Ok(BackgroundTaskState::Completed) => "completed",
        Ok(BackgroundTaskState::Failed) => "failed",
        _ => "stopped",
    }
}

fn kind_name(kind: i32) -> &'static str {
    match BackgroundTaskKind::try_from(kind) {
        Ok(BackgroundTaskKind::Agent) => "agent",
        _ => "bash",
    }
}

async fn upsert(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &BackgroundWorkChanged,
    task: &FactTask,
    at: DateTime<Utc>,
) -> Result<Option<BackgroundTaskView>, sqlx::Error> {
    let block = (!task.block_id.is_empty())
        .then(|| Uuid::parse_str(&task.block_id))
        .transpose()
        .map_err(|_| invalid("block"))?;
    let state = state_name(task.state);
    let row: Option<TaskRow> = sqlx::query_as(
        "insert into background_tasks as t
             (conversation_id, task_id, execution_id, generation, source_revision, kind,
              block_id, parent_task_id, description, state, started_at, ended_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                 case when $10 = 'running' then null else $11 end)
         on conflict (conversation_id, task_id) do update
         set execution_id = excluded.execution_id,
             generation = excluded.generation,
             source_revision = excluded.source_revision,
             kind = excluded.kind,
             block_id = coalesce(excluded.block_id, t.block_id),
             parent_task_id = coalesce(excluded.parent_task_id, t.parent_task_id),
             description = excluded.description,
             state = excluded.state,
             started_at = case when t.state <> 'running' and excluded.state = 'running'
                               then excluded.started_at else t.started_at end,
             ended_at = case when excluded.state = 'running' then null
                             else coalesce(t.ended_at, $11) end
         where (t.generation, t.source_revision) < (excluded.generation, excluded.source_revision)
           and (t.state, t.block_id, t.description) is distinct from
               (excluded.state, coalesce(excluded.block_id, t.block_id), excluded.description)
         returning task_id, kind, block_id, parent_task_id, description, state, started_at, ended_at",
    )
    .bind(run.conversation_id)
    .bind(blocks::without_nul(&task.task_id).as_ref())
    .bind(run.execution_id)
    .bind(i64::try_from(fact.generation).map_err(|_| invalid("generation"))?)
    .bind(i64::try_from(fact.revision).map_err(|_| invalid("revision"))?)
    .bind(kind_name(task.kind))
    .bind(block)
    .bind(
        (!task.parent_task_id.is_empty())
            .then(|| blocks::without_nul(&task.parent_task_id).into_owned()),
    )
    .bind(blocks::without_nul(&task.description).as_ref())
    .bind(state)
    .bind(at)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(Into::into))
}

/// `true`, если запись устарела и ничего не изменила. Пакет SJ уже держит `queue_state`
/// разговора, поэтому строки меняются без отдельной блокировки.
pub async fn apply(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &BackgroundWorkChanged,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let at = i64::try_from(fact.machine_time_ms)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .ok_or_else(|| invalid("machine time"))?;
    let revision = i64::try_from(fact.revision).map_err(|_| invalid("revision"))?;
    let mut changed = Vec::with_capacity(fact.tasks.len());
    for task in &fact.tasks {
        changed.extend(upsert(conn, run, fact, task, at).await?);
    }
    // Список полный: задача, которой в нём нет, не работает. Обрезанный список этого не значит.
    if fact.omitted == 0 {
        let listed: Vec<String> = fact
            .tasks
            .iter()
            .map(|task| blocks::without_nul(&task.task_id).into_owned())
            .collect();
        let swept: Vec<TaskRow> = sqlx::query_as(
            "update background_tasks set state = 'stopped', ended_at = $3, source_revision = $4
             where conversation_id = $1 and execution_id = $2 and state = 'running'
               and source_revision < $4 and task_id <> all($5)
             returning task_id, kind, block_id, parent_task_id, description, state, started_at, ended_at",
        )
        .bind(run.conversation_id)
        .bind(run.execution_id)
        .bind(at)
        .bind(revision)
        .bind(&listed)
        .fetch_all(&mut *conn)
        .await?;
        changed.extend(swept.into_iter().map(BackgroundTaskView::from));
    }
    let wake =
        sqlx::query("update executions set wake_turn = $2 where id = $1 and wake_turn <> $2")
            .bind(run.execution_id)
            .bind(fact.wake_turn)
            .execute(&mut *conn)
            .await?
            .rows_affected();
    if changed.is_empty() && wake == 0 {
        return Ok(true);
    }
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: run.conversation_id,
            },
            kind: "background_work_changed",
            conversation_id: Some(run.conversation_id),
            execution_id: Some(run.execution_id),
            payload: json!({"tasks": changed}),
            correlation_id,
        },
    )
    .await?;
    Ok(false)
}

/// Законченных задач в снимке: долгая сессия копит их без конца.
const RECENT_TASKS: i64 = 50;

/// Работающие и последние законченные задачи разговора, свежие в конце.
/// Со смертью процесса работающая задача прервана.
pub async fn list(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Vec<BackgroundTaskView>, sqlx::Error> {
    let rows: Vec<TaskRow> = sqlx::query_as(
        "select t.task_id, t.kind, t.block_id, t.parent_task_id, t.description,
                case when t.state = 'running' and e.state not in ('created','starting','running','stopping') then 'stopped'
                     else t.state end as state,
                t.started_at, t.ended_at
         from background_tasks t join executions e on e.id = t.execution_id
         where t.conversation_id = $1
           and ((t.state = 'running' and e.state in ('created','starting','running','stopping'))
                or t.task_id in (select r.task_id from background_tasks r where r.conversation_id = $1
                                 order by r.started_at desc, r.task_id desc limit $2))
         order by t.started_at, t.task_id",
    )
    .bind(conversation_id)
    .bind(RECENT_TASKS)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Сколько фоновых задач работает сейчас.
pub async fn running(conn: &mut PgConnection, conversation_id: Uuid) -> Result<u32, sqlx::Error> {
    let count: i64 = sqlx::query_scalar(
        "select count(*) from background_tasks t join executions e on e.id = t.execution_id
         where t.conversation_id = $1 and t.state = 'running' and e.state in ('created','starting','running','stopping')",
    )
    .bind(conversation_id)
    .fetch_one(conn)
    .await?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}
