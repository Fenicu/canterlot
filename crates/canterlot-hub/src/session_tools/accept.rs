//! `session_accept`: остановить делегированную сессию, убрать её рабочую копию и архивировать.
//!
//! Приём состоит из транзакции (останов, закрытие подписок, состояние `removing`, задание
//! `cleanup`) и самого задания: пока исполнение живо, оно перепроверяет раз в 5 секунд,
//! затем отдаёт воркеру `remove_worktree`. Итог воркера разбирает `apply_removal_in_tx`.

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{
    Caller, ToolContext, ToolError, ToolErrorCode, access, origin, parse_arguments, send, subs,
};
use crate::{
    conversations::{self, Attributes, UpdateError},
    crunch::JobOutcome,
    delivery::control::{self, ControlError},
    executions,
    idempotency::{self, IdempotencyConflict},
    scheduler::ClaimedJob,
};

const CLOSED_BY_ACCEPT: &str = "accepted";
const RECHECK: TimeDelta = TimeDelta::seconds(5);
/// После этого срока живое исполнение останавливают принудительно.
const FORCE_AFTER: TimeDelta = TimeDelta::minutes(10);
/// Исполнение, которое не гаснет сутки (машина не вернулась), уборку не держит вечно.
const GIVE_UP_AFTER: TimeDelta = TimeDelta::hours(24);
const MAX_DETAIL_CHARS: usize = 64;

const HINT_STOPPING: &str = "The session is being stopped; its worktree is removed afterwards. A [canterlot] message tells you how it ended. Do not poll.";
const HINT_REMOVING: &str =
    "The worktree is being removed. A [canterlot] message tells you how it ended. Do not poll.";
const HINT_DONE: &str = "The session is stopped and archived.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AcceptState {
    Stopping,
    Removing,
    Done,
}

impl AcceptState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopping => "stopping",
            Self::Removing => "removing",
            Self::Done => "done",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Stopping => HINT_STOPPING,
            Self::Removing => HINT_REMOVING,
            Self::Done => HINT_DONE,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AcceptError {
    #[error("session not found")]
    NotFound,
    #[error("session was not started by an agent")]
    NotDelegated,
    #[error("only the parent may accept")]
    NotParent,
    #[error("idempotency conflict")]
    IdempotencyConflict,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl From<IdempotencyConflict> for AcceptError {
    fn from(_: IdempotencyConflict) -> Self {
        Self::IdempotencyConflict
    }
}

fn control_error(error: ControlError) -> sqlx::Error {
    match error {
        ControlError::Db(error) => error,
        _ => sqlx::Error::RowNotFound,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptArgs {
    session_id: Uuid,
    #[serde(default)]
    force: bool,
}

/// Сессия глазами приёма: кто родитель и что с рабочей копией.
struct Child {
    id: Uuid,
    owner_id: Uuid,
    machine_id: Uuid,
    parent: Option<Uuid>,
    title: String,
    harness: String,
    model: String,
    worktree: Option<Value>,
    archived: bool,
}

impl Child {
    fn sender(&self) -> origin::Sender {
        origin::Sender {
            id: self.id,
            title: self.title.clone(),
            harness: self.harness.clone(),
            model: self.model.clone(),
        }
    }

    fn worktree_state(&self) -> Option<&str> {
        self.worktree.as_ref()?["state"].as_str()
    }

    fn branch(&self) -> &str {
        self.worktree
            .as_ref()
            .and_then(|worktree| worktree["branch"].as_str())
            .unwrap_or_default()
    }

    fn label(&self) -> String {
        format!("\"{}\" (id {})", origin::clean_title(&self.title), self.id)
    }
}

type ChildRow = (
    Uuid,
    Option<Uuid>,
    String,
    String,
    String,
    Option<Value>,
    bool,
);

async fn load_child(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Option<Child>, sqlx::Error> {
    let row: Option<ChildRow> = sqlx::query_as(
        "select machine_id,parent_conversation_id,title,harness,model,worktree,
                archived_at is not null
         from conversations where id=$1 and owner_id=$2",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(
        |(machine_id, parent, title, harness, model, worktree, archived)| Child {
            id,
            owner_id,
            machine_id,
            parent,
            title,
            harness,
            model,
            worktree,
            archived,
        },
    ))
}

/// `None`, если сессии нет; `Some(None)`, если у неё нет родителя.
async fn parent_of(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Option<Option<Uuid>>, sqlx::Error> {
    sqlx::query_scalar(
        "select parent_conversation_id from conversations where id=$1 and owner_id=$2",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(conn)
    .await
}

/// Принять результат может только прямой родитель; сессию без родителя принимать нечем.
fn check_parent(parent: Option<Uuid>, caller: Uuid) -> Result<(), AcceptError> {
    match parent {
        None => Err(AcceptError::NotDelegated),
        Some(parent) if parent != caller => Err(AcceptError::NotParent),
        Some(_) => Ok(()),
    }
}

fn tool_error(error: AcceptError) -> ToolError {
    match error {
        AcceptError::NotFound => ToolError::new(ToolErrorCode::SessionNotFound),
        AcceptError::NotDelegated => ToolError::new(ToolErrorCode::NotDelegated),
        AcceptError::NotParent => ToolError::new(ToolErrorCode::NotParent),
        AcceptError::IdempotencyConflict => ToolError::new(ToolErrorCode::InvalidArguments),
        AcceptError::Db(error) => error.into(),
    }
}

/// `sessions.accept`: вызывает прямой родитель.
pub async fn accept(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: AcceptArgs = parse_arguments(arguments)?;
    let mut tx = context.pool.begin().await?;
    // Владелец раньше строк разговоров: тот же порядок у создания, удаления и архивации.
    conversations::lock_owner(&mut tx, caller.owner_id).await?;
    let me = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    let target = access::target_in_tx(
        &mut tx,
        &context.config,
        &me,
        caller.owner_id,
        args.session_id,
    )
    .await?;
    let parent = parent_of(&mut tx, caller.owner_id, target.id)
        .await?
        .ok_or_else(|| ToolError::new(ToolErrorCode::SessionNotFound))?;
    check_parent(parent, me.id).map_err(tool_error)?;
    access::check_accept(&me, &target, args.force)?;
    let state = begin_in_tx(
        &mut tx,
        caller.owner_id,
        target.id,
        args.force,
        caller.call_id,
    )
    .await?;
    tx.commit().await?;
    wake(context.dispatcher.as_ref(), &context.pool, target.id);
    Ok(json!({"status": state.as_str(), "hint": state.hint()}))
}

/// Владелец принимает любую делегированную сессию; повтор с тем же ключом отвечает тем же.
pub async fn accept_for_owner(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    force: bool,
    key: &str,
    correlation_id: Uuid,
) -> Result<AcceptState, AcceptError> {
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "accept_conversation",
        &json!({"conversation_id": id, "force": force}),
        None,
        move |conn| {
            Box::pin(async move {
                conversations::lock_owner(conn, owner_id).await?;
                let parent = parent_of(conn, owner_id, id)
                    .await?
                    .ok_or(AcceptError::NotFound)?;
                if parent.is_none() {
                    return Err(AcceptError::NotDelegated);
                }
                Ok::<_, AcceptError>(begin_in_tx(conn, owner_id, id, force, correlation_id).await?)
            })
        },
    )
    .await?;
    Ok(outcome.value)
}

/// После фиксации: остановку подхватит диспетчер, задание очистки разбудит планировщик.
pub fn wake(dispatcher: Option<&crate::dispatcher::Dispatcher>, pool: &PgPool, id: Uuid) {
    if let Some(dispatcher) = dispatcher {
        dispatcher.trigger(id);
    }
    crate::scheduler::notify_pool(pool);
}

/// Общая часть приёма. Повтор безвреден: стоп и задание заводятся один раз.
pub async fn begin_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    force: bool,
    correlation_id: Uuid,
) -> Result<AcceptState, sqlx::Error> {
    conversations::lock_owner(conn, owner_id).await?;
    send::lock_queue(conn, id).await?;
    let child = load_child(conn, owner_id, id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    let live = executions::live(conn, id).await?.is_some();
    if live {
        control::stop_in_tx(conn, owner_id, id, false, correlation_id)
            .await
            .map_err(control_error)?;
    }
    let others = subs::close_for_accept_in_tx(conn, id, child.parent, CLOSED_BY_ACCEPT).await?;
    let now = Utc::now();
    for other in others {
        let origin = origin::started_note_origin(&child.sender(), other.chain_depth);
        subs::insert_job(
            conn,
            other.owner_id,
            other.subscriber_id,
            now,
            &super::spawn::note_job(subscription_closed_text(&child), origin),
        )
        .await?;
    }
    let has_copy = matches!(
        child.worktree_state(),
        Some("active" | "removing" | "dirty" | "failed")
    );
    if has_copy {
        set_worktree_state(conn, &child, "removing", None, None, correlation_id).await?;
    } else if !live {
        archive_in_tx(conn, &child, correlation_id).await?;
        return Ok(AcceptState::Done);
    }
    ensure_job(conn, owner_id, id, force).await?;
    Ok(if live {
        AcceptState::Stopping
    } else {
        AcceptState::Removing
    })
}

/// Задание уборки одно на сессию; повторный приём с `force` только усиливает его.
/// Задание в работе пропускается: его порядок блокировок обратный, а лишнее задание
/// находит команду в полёте или готовое состояние и ничего не делает.
async fn ensure_job(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    force: bool,
) -> Result<(), sqlx::Error> {
    let queued: Option<Uuid> = sqlx::query_scalar(
        "select id from scheduled_jobs
         where kind='session_task' and state='pending' and conversation_id=$1
           and payload->>'action'='cleanup'
         for update skip locked",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(job) = queued {
        sqlx::query(
            "update scheduled_jobs set
               payload=payload||jsonb_build_object('force',
                 coalesce((payload->>'force')::boolean,false) or $2),
               updated_at=clock_timestamp()
             where id=$1",
        )
        .bind(job)
        .bind(force)
        .execute(conn)
        .await?;
        return Ok(());
    }
    subs::insert_job(
        conn,
        owner_id,
        id,
        Utc::now(),
        &json!({"action": "cleanup", "conversation_id": id, "force": force}),
    )
    .await?;
    Ok(())
}

async fn set_worktree_state(
    conn: &mut PgConnection,
    child: &Child,
    state: &str,
    detail: Option<&str>,
    branch_kept: Option<bool>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let changed = sqlx::query(
        "update conversations set
           worktree=worktree||jsonb_build_object('state',$2::text,'detail',$3::text,'branch_kept',$4::boolean),
           updated_at=clock_timestamp()
         where id=$1 and worktree is not null
           and (worktree->>'state' is distinct from $2
                or worktree->>'detail' is distinct from $3
                or worktree->'branch_kept' is distinct from to_jsonb($4::boolean))",
    )
    .bind(child.id)
    .bind(state)
    .bind(detail)
    .bind(branch_kept)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if changed > 0 {
        conversations::emit_attributes(conn, child.owner_id, child.id, correlation_id).await?;
    }
    Ok(())
}

/// Архив с отменой запланированного и кранча, как при принудительной архивации владельцем.
/// Занятое задание не мешает: сессия всё равно уходит в архив.
async fn archive_in_tx(
    conn: &mut PgConnection,
    child: &Child,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    if child.archived {
        return Ok(());
    }
    let archived = Attributes {
        archived: Some(true),
        force: true,
        ..Attributes::default()
    };
    match conversations::update_attributes_in_tx(
        conn,
        child.owner_id,
        child.id,
        archived,
        correlation_id,
    )
    .await
    {
        Ok(_) | Err(UpdateError::NotFound) => Ok(()),
        Err(UpdateError::Db(error)) => Err(error),
        Err(_) => {
            sqlx::query(
                "update conversations set archived_at=coalesce(archived_at,clock_timestamp()),
                   updated_at=clock_timestamp() where id=$1",
            )
            .bind(child.id)
            .execute(&mut *conn)
            .await?;
            conversations::emit_attributes(conn, child.owner_id, child.id, correlation_id).await
        }
    }
}

/// Сообщение родителю отдельным заданием. Оно несёт данные принятой сессии, поэтому при её
/// удалении вычищается как событие от неё.
async fn notify_parent_in_tx(
    conn: &mut PgConnection,
    child: &Child,
    text: String,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let Some(parent) = child.parent else {
        return Ok(());
    };
    let depth = access::current_depth(conn, parent).await?;
    let origin = origin::started_note_origin(&child.sender(), depth);
    subs::insert_job(
        conn,
        child.owner_id,
        parent,
        now,
        &super::spawn::note_job(text, origin),
    )
    .await?;
    Ok(())
}

fn accepted_text(child: &Child, removed: Option<bool>) -> String {
    let label = child.label();
    match removed {
        None => format!("[canterlot] Session {label} was accepted: it is stopped and archived."),
        Some(true) => format!(
            "[canterlot] Session {label} was accepted: it is stopped, archived, and its worktree is removed. Branch {} holds commits that exist nowhere else and was kept: review or merge it.",
            child.branch()
        ),
        Some(false) => format!(
            "[canterlot] Session {label} was accepted: it is stopped, archived, and its worktree is removed. Its branch {} had no commits of its own and was deleted.",
            child.branch()
        ),
    }
}

fn subscription_closed_text(child: &Child) -> String {
    format!(
        "[canterlot] Session {} was accepted by its parent and closed: you will get no further events from it.",
        child.label()
    )
}

fn dirty_text(child: &Child) -> String {
    format!(
        "[canterlot] Session {} is stopped, but its worktree has uncommitted changes, so nothing was removed and the session is not archived. Call session_accept again with force:true to discard them, or session_send to ask it to commit first.",
        child.label()
    )
}

fn failed_text(child: &Child, detail: &str) -> String {
    format!(
        "[canterlot] The worktree of session {} could not be removed: {detail}. The session is stopped. Call session_accept again to retry.",
        child.label()
    )
}

/// Итог воркера по `remove_worktree`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    Removed { branch_kept: bool },
    Dirty,
    Failed,
}

/// Причина отказа воркера остаётся коротким кодом: в состояние и сообщение идёт только он.
fn clean_detail(detail: &str) -> String {
    let code: String = detail
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(MAX_DETAIL_CHARS)
        .collect();
    if code.is_empty() {
        "unknown".to_owned()
    } else {
        code
    }
}

/// Запись `WorktreeRemoved` из журнала команд. Возвращает, проигнорирована ли она: команда
/// не наша, не `remove_worktree`, другая попытка или уже закрыта.
pub(crate) async fn apply_removal_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    command_id: Uuid,
    dispatch_attempt: i32,
    removal: Removal,
    detail: &str,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let owner: Option<Uuid> = sqlx::query_scalar(
        "select c.owner_id from commands m join conversations c on c.id=m.conversation_id
         where m.id=$1 and m.machine_id=$2 and m.kind='remove_worktree'",
    )
    .bind(command_id)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(owner_id) = owner else {
        return Ok(true);
    };
    // Владелец и очередь раньше строки команды: порядок приёма, архивации и квитанций.
    conversations::lock_owner(conn, owner_id).await?;
    sqlx::query(
        "select q.conversation_id from queue_state q join commands c on c.conversation_id=q.conversation_id
         where c.id=$1 for update of q",
    )
    .bind(command_id)
    .execute(&mut *conn)
    .await?;
    let target: Option<(Uuid, i32, String)> = sqlx::query_as(
        "select conversation_id,dispatch_attempt,delivery_state from commands
         where id=$1 and machine_id=$2 and kind='remove_worktree'",
    )
    .bind(command_id)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((conversation_id, current_attempt, delivery_state)) = target else {
        return Ok(true);
    };
    if current_attempt != dispatch_attempt || delivery_state == "completed" {
        return Ok(true);
    }
    let detail = clean_detail(detail);
    let status = match removal {
        Removal::Removed { .. } => "removed",
        Removal::Dirty => "dirty",
        Removal::Failed => "failed",
    };
    sqlx::query(
        "update commands set delivery_state='completed',completed_at=coalesce(completed_at,now()),
           last_status=$2,result=$3 where id=$1",
    )
    .bind(command_id)
    .bind(status)
    .bind(json!({"outcome": status, "detail": detail}))
    .execute(&mut *conn)
    .await?;
    let Some(child) = load_child(conn, owner_id, conversation_id).await? else {
        return Ok(false);
    };
    // Ответ на устаревшую просьбу: состояние уже решено другим приёмом.
    if child.worktree_state() != Some("removing") {
        return Ok(false);
    }
    let now = Utc::now();
    match removal {
        Removal::Removed { branch_kept } => {
            set_worktree_state(
                conn,
                &child,
                "removed",
                None,
                Some(branch_kept),
                correlation_id,
            )
            .await?;
            archive_in_tx(conn, &child, correlation_id).await?;
            notify_parent_in_tx(conn, &child, accepted_text(&child, Some(branch_kept)), now)
                .await?;
        }
        Removal::Dirty => {
            set_worktree_state(conn, &child, "dirty", None, None, correlation_id).await?;
            notify_parent_in_tx(conn, &child, dirty_text(&child), now).await?;
        }
        Removal::Failed => {
            set_worktree_state(conn, &child, "failed", Some(&detail), None, correlation_id).await?;
            notify_parent_in_tx(conn, &child, failed_text(&child, &detail), now).await?;
        }
    }
    Ok(false)
}

#[derive(Deserialize)]
struct CleanupJob {
    #[serde(default)]
    force: bool,
}

/// Задание `cleanup`: доводит принятую сессию до конца под её очередью.
pub(crate) async fn cleanup_job(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    id: Uuid,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<JobOutcome, sqlx::Error> {
    let Ok(CleanupJob { force }) = serde_json::from_value(job.payload.clone()) else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    conversations::lock_owner(conn, job.owner_id).await?;
    send::lock_queue(conn, id).await?;
    let Some(child) = load_child(conn, job.owner_id, id).await? else {
        return Ok(JobOutcome::Done(json!({"skipped": true})));
    };
    if let Some((execution_id, generation, _)) = executions::live(conn, id).await? {
        let created: DateTime<Utc> =
            sqlx::query_scalar("select created_at from scheduled_jobs where id=$1")
                .bind(job.id)
                .fetch_one(&mut *conn)
                .await?;
        let age = now - created;
        if age >= GIVE_UP_AFTER {
            give_up_in_tx(conn, &child, now, correlation_id).await?;
            return Ok(JobOutcome::Done(json!({"gave_up": true})));
        }
        if age >= FORCE_AFTER {
            force_stop_once(conn, &child, execution_id, generation, correlation_id).await?;
        }
        return Ok(JobOutcome::Retry {
            at: now + RECHECK,
            poll: false,
        });
    }
    match child.worktree_state() {
        None | Some("removed") => {
            archive_in_tx(conn, &child, correlation_id).await?;
            notify_parent_in_tx(conn, &child, accepted_text(&child, None), now).await?;
            Ok(JobOutcome::Done(json!({"archived": true})))
        }
        Some("removing") => {
            let queued = queue_removal(conn, &child, force).await?;
            if queued.is_none() {
                set_worktree_state(
                    conn,
                    &child,
                    "failed",
                    Some("no_execution"),
                    None,
                    correlation_id,
                )
                .await?;
                notify_parent_in_tx(conn, &child, failed_text(&child, "no_execution"), now).await?;
            }
            Ok(JobOutcome::Done(json!({"command_id": queued})))
        }
        Some(_) => Ok(JobOutcome::Done(json!({"skipped": true}))),
    }
}

/// Одна принудительная остановка на исполнение: после неё ждём, а не шлём новые.
async fn force_stop_once(
    conn: &mut PgConnection,
    child: &Child,
    execution_id: Uuid,
    generation: i64,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let asked: bool = sqlx::query_scalar(
        "select exists(select 1 from commands where conversation_id=$1 and kind='stop_execution'
                         and execution_id=$2 and execution_generation=$3
                         and payload->>'force'='true')",
    )
    .bind(child.id)
    .bind(execution_id)
    .bind(generation)
    .fetch_one(&mut *conn)
    .await?;
    if !asked {
        control::stop_in_tx(conn, child.owner_id, child.id, true, correlation_id)
            .await
            .map_err(control_error)?;
    }
    Ok(())
}

async fn give_up_in_tx(
    conn: &mut PgConnection,
    child: &Child,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    if child.worktree_state() == Some("removing") {
        set_worktree_state(
            conn,
            child,
            "failed",
            Some("stop_timeout"),
            None,
            correlation_id,
        )
        .await?;
    }
    notify_parent_in_tx(conn, child, failed_text(child, "stop_timeout"), now).await
}

/// Команда воркеру с последним исполнением сессии: без него команда не уходит. Воркер
/// принимает и пустое, но очередь хаба требует исполнение у каждой команды. Команда в полёте
/// отвечает собой, второй не будет; `None`, если у сессии нет ни одного исполнения.
async fn queue_removal(
    conn: &mut PgConnection,
    child: &Child,
    force: bool,
) -> Result<Option<Uuid>, sqlx::Error> {
    let outstanding: Option<Uuid> = sqlx::query_scalar(
        "select id from commands where conversation_id=$1
           and kind='remove_worktree' and delivery_state<>'completed'
         order by created_at,id limit 1",
    )
    .bind(child.id)
    .fetch_optional(&mut *conn)
    .await?;
    if outstanding.is_some() {
        return Ok(outstanding);
    }
    let last: Option<(Uuid, i64)> = sqlx::query_as(
        "select id,generation from executions where conversation_id=$1
         order by generation desc limit 1",
    )
    .bind(child.id)
    .fetch_optional(&mut *conn)
    .await?;
    let (Some((execution_id, generation)), Some(worktree)) = (last, child.worktree.as_ref()) else {
        return Ok(None);
    };
    let command_id = Uuid::now_v7();
    sqlx::query(
        "insert into commands (id,machine_id,conversation_id,execution_id,execution_generation,
                               kind,payload,delivery_state)
         values ($1,$2,$3,$4,$5,'remove_worktree',$6,'pending')",
    )
    .bind(command_id)
    .bind(child.machine_id)
    .bind(child.id)
    .bind(execution_id)
    .bind(generation)
    .bind(json!({
        "source_workdir": worktree["source_workdir"],
        "path": worktree["path"],
        "branch": worktree["branch"],
        "force": force,
    }))
    .execute(&mut *conn)
    .await?;
    sqlx::query("select pg_notify('canterlot_outbox', $1::text)")
        .bind(child.machine_id.to_string())
        .execute(conn)
        .await?;
    Ok(Some(command_id))
}

/// Задание исчерпало попытки: сессия остаётся остановленной, а родитель узнаёт, что уборка не вышла.
pub(crate) async fn abandon_cleanup_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    conversations::lock_owner(conn, owner_id).await?;
    send::lock_queue(conn, id).await?;
    let Some(child) = load_child(conn, owner_id, id).await? else {
        return Ok(());
    };
    if child.worktree_state() == Some("removing") {
        set_worktree_state(
            conn,
            &child,
            "failed",
            Some("cleanup_failed"),
            None,
            correlation_id,
        )
        .await?;
    }
    notify_parent_in_tx(conn, &child, failed_text(&child, "cleanup_failed"), now).await
}
