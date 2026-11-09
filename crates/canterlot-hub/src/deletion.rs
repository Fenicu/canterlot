//! Удаление разговора (§14.1, R6). Пилот удаляет остановленный разговор одной транзакцией:
//! локальную очистку на машине протокол пока не описывает, поэтому копии воркера остаются.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    attachments::BlobStore,
    conversations::{self, ConversationDetail},
    events,
    events::{NewEvent, Scope},
    executions, idempotency, scheduler,
    session_tools::{origin, subs},
};

#[derive(Debug, Error)]
pub enum DeleteError {
    #[error("conversation not found")]
    NotFound,
    #[error("conversation deleted")]
    Deleted,
    #[error("execution is live")]
    Live,
    #[error("worktree is not removed")]
    WorktreeNotRemoved,
    #[error("referenced from another conversation")]
    Referenced,
    #[error("conversation is not trashed")]
    NotTrashed,
    #[error("idempotency conflict")]
    IdempotencyConflict,
    /// Задание сессии сейчас выполняет планировщик: удаление повторяется позже.
    #[error("conversation jobs are running")]
    Busy,
    #[error(transparent)]
    Db(sqlx::Error),
}

impl crate::db::TransientError for DeleteError {
    fn is_transient(&self) -> bool {
        match self {
            Self::Busy => true,
            Self::Db(error) => crate::db::is_transient(error),
            _ => false,
        }
    }
}

impl From<sqlx::Error> for DeleteError {
    fn from(error: sqlx::Error) -> Self {
        // Ссылка снаружи означает, что данные не принадлежат одному разговору: отказ, а не каскад.
        match &error {
            sqlx::Error::Database(database) if database.code().as_deref() == Some("23503") => {
                Self::Referenced
            }
            _ => Self::Db(error),
        }
    }
}

impl From<idempotency::IdempotencyConflict> for DeleteError {
    fn from(_: idempotency::IdempotencyConflict) -> Self {
        Self::IdempotencyConflict
    }
}

/// Условие кандидата уборки принятых делегаций (`$1` — срок). Одно и то же условие
/// отбирает кандидатов без блокировок и перепроверяется под блокировкой строки
/// разговора: владелец мог закрепить, вернуть из архива или сгруппировать сессию
/// между отбором и удалением.
macro_rules! accepted_retention_where {
    () => {
        "c.accepted_at is not null and c.accepted_at <= $1
           and c.archived_at is not null and c.spawn_depth > 0
           and c.trashed_at is null
           and not c.pinned
           and not exists (select 1 from conversation_group_members gm
                            where gm.conversation_id=c.id)
           and not exists (select 1 from conversation_group_pins gp
                            where gp.conversation_id=c.id)
           and coalesce(c.worktree->'branch_kept','false'::jsonb) <> 'true'::jsonb
           and coalesce(c.worktree->>'state','') not in ('active','removing','dirty','failed')
           and not exists (select 1 from executions e
                            where e.conversation_id=c.id and e.state not in ('stopped','failed'))
           and not exists (select 1 from conversations child
                            where child.parent_conversation_id=c.id)"
    };
}
pub(crate) use accepted_retention_where;

pub async fn tombstoned(pool: &PgPool, owner_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(select 1 from conversation_tombstones
                       where conversation_id=$1 and owner_id=$2)",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_one(pool)
    .await
}

/// Сессия спрятана владельцем в корзину: для агентов и большинства маршрутов она удалена.
pub async fn trashed(pool: &PgPool, owner_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(select 1 from conversations
                       where id=$1 and owner_id=$2 and trashed_at is not null)",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_one(pool)
    .await
}

/// Исполнение удалённого разговора этой машины: его поздние факты не считаются чужими.
pub async fn execution_tombstoned(
    conn: &mut PgConnection,
    machine_id: Uuid,
    execution_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(select 1 from conversation_tombstones
                       where machine_id=$1 and execution_ids @> array[$2::uuid])",
    )
    .bind(machine_id)
    .bind(execution_id)
    .fetch_one(conn)
    .await
}

pub async fn delete(
    pool: &PgPool,
    blobs: &dyn BlobStore,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    correlation_id: Uuid,
) -> Result<(), DeleteError> {
    let files = owned_files(pool, owner_id, id).await?;
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "delete_conversation",
        &json!({"id": id}),
        None,
        move |conn| {
            Box::pin(delete_in_tx(
                conn,
                owner_id,
                id,
                correlation_id,
                DeleteMode::Manual,
                TrashGuard::Forbid,
            ))
        },
    )
    .await;
    match outcome {
        Ok(done) if !done.replayed => {
            scheduler::notify_pool(pool);
            free_files(pool, blobs, files).await;
        }
        Ok(_) => {}
        Err(DeleteError::NotFound) if tombstoned(pool, owner_id, id).await? => {
            return Err(DeleteError::Deleted);
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

/// Ручное удаление владельца: сессия уходит в корзину, строки остаются до срока хранения.
/// Предварительные условия те же, что у окончательной уборки: живого исполнения нет,
/// рабочая копия уже убрана. Повтор по другому ключу ничего не делает повторно.
pub async fn trash(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    correlation_id: Uuid,
) -> Result<(), DeleteError> {
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "trash_conversation",
        &json!({"id": id}),
        Some(id),
        move |conn| Box::pin(trash_in_tx(conn, owner_id, id, correlation_id)),
    )
    .await;
    match outcome {
        Ok(done) if !done.replayed => {
            scheduler::notify_pool(pool);
        }
        Ok(_) => {}
        Err(DeleteError::NotFound) if tombstoned(pool, owner_id, id).await? => {
            return Err(DeleteError::Deleted);
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

/// Возврат из корзины: снимает отметку и ничего не воскрешает. Закрытые подписки и
/// сработавшие будильники остаются закрытыми — агентам уже сказали об удалении.
pub async fn restore(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    correlation_id: Uuid,
) -> Result<ConversationDetail, DeleteError> {
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "restore_conversation",
        &json!({"id": id}),
        Some(id),
        move |conn| Box::pin(restore_in_tx(conn, owner_id, id, correlation_id)),
    )
    .await;
    match outcome {
        Ok(done) => {
            if !done.replayed {
                scheduler::notify_pool(pool);
            }
            Ok(done.value)
        }
        Err(DeleteError::NotFound) if tombstoned(pool, owner_id, id).await? => {
            Err(DeleteError::Deleted)
        }
        Err(error) => Err(error),
    }
}

/// Окончательная уборка из корзины по кнопке владельца: только уже спрятанную сессию.
pub async fn purge(
    pool: &PgPool,
    blobs: &dyn BlobStore,
    owner_id: Uuid,
    id: Uuid,
    cutoff: DateTime<Utc>,
    key: &str,
    correlation_id: Uuid,
) -> Result<(), DeleteError> {
    let files = owned_files(pool, owner_id, id).await?;
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "purge_conversation",
        &json!({"id": id}),
        None,
        move |conn| {
            Box::pin(delete_in_tx(
                conn,
                owner_id,
                id,
                correlation_id,
                DeleteMode::Retention,
                TrashGuard::Before(cutoff),
            ))
        },
    )
    .await;
    match outcome {
        Ok(done) if !done.replayed => {
            scheduler::notify_pool(pool);
            free_files(pool, blobs, files).await;
        }
        Ok(_) => {}
        Err(DeleteError::NotFound) if tombstoned(pool, owner_id, id).await? => {
            return Err(DeleteError::Deleted);
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

/// Уборка корзины по сроку: тот же окончательный путь, но без ключа идемпотентности.
pub async fn purge_retained(
    pool: &PgPool,
    blobs: Option<&dyn BlobStore>,
    owner_id: Uuid,
    id: Uuid,
    cutoff: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), DeleteError> {
    let files = owned_files(pool, owner_id, id).await?;
    let mut tx = crate::db::begin(pool).await?;
    match delete_in_tx(
        &mut tx,
        owner_id,
        id,
        correlation_id,
        DeleteMode::Retention,
        TrashGuard::Before(cutoff),
    )
    .await
    {
        Ok(_) => {
            tx.commit().await?;
            scheduler::notify_pool(pool);
            if let Some(blobs) = blobs {
                free_files(pool, blobs, files).await;
            }
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// Уборка принятой делегации по сроку хранения: тот же путь, что и ручное удаление, но без
/// ключа идемпотентности и без уведомлений подписчикам. Приём уже закрыл их и сообщил итог,
/// поэтому `deleted` в очередь никто не получает.
pub async fn delete_retained(
    pool: &PgPool,
    blobs: Option<&dyn BlobStore>,
    owner_id: Uuid,
    id: Uuid,
    correlation_id: Uuid,
) -> Result<(), DeleteError> {
    let files = owned_files(pool, owner_id, id).await?;
    let mut tx = crate::db::begin(pool).await?;
    match delete_in_tx(
        &mut tx,
        owner_id,
        id,
        correlation_id,
        DeleteMode::Retention,
        TrashGuard::Forbid,
    )
    .await
    {
        Ok(_) => {
            tx.commit().await?;
            scheduler::notify_pool(pool);
            if let Some(blobs) = blobs {
                free_files(pool, blobs, files).await;
            }
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

/// Уборка принятой делегации планировщиком: условие отбора кандидатов перепроверяется
/// под блокировкой строки разговора; сессия, которую владелец успел оставить, даёт
/// `NotTrashed` и не удаляется.
pub async fn delete_accepted_retained(
    pool: &PgPool,
    blobs: Option<&dyn BlobStore>,
    owner_id: Uuid,
    id: Uuid,
    cutoff: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), DeleteError> {
    let files = owned_files(pool, owner_id, id).await?;
    let mut tx = crate::db::begin(pool).await?;
    match delete_in_tx(
        &mut tx,
        owner_id,
        id,
        correlation_id,
        DeleteMode::Retention,
        TrashGuard::AcceptedBefore(cutoff),
    )
    .await
    {
        Ok(_) => {
            tx.commit().await?;
            scheduler::notify_pool(pool);
            if let Some(blobs) = blobs {
                free_files(pool, blobs, files).await;
            }
            Ok(())
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

async fn owned_files(pool: &PgPool, owner_id: Uuid, id: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select a.id from attachments a join conversations c on c.id=a.conversation_id
          where c.id=$1 and c.owner_id=$2
         union all
         select m.id from block_media m join conversations c on c.id=m.conversation_id
          where c.id=$1 and c.owner_id=$2
         union all
         select f.id from block_files f join conversations c on c.id=f.conversation_id
          where c.id=$1 and c.owner_id=$2",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_all(pool)
    .await
}

/// Файлы адресуются своим ID, поэтому чужих среди них нет. Занятый или неудалённый файл
/// без строки в каталоге позже подберёт сборщик вложений. Список собран до транзакции,
/// поэтому после commit файл освобождается, только если его строки больше нет: вложение,
/// перешедшее к получателю пересылки, остаётся вместе со своим blob.
async fn free_files(pool: &PgPool, blobs: &dyn BlobStore, files: Vec<Uuid>) {
    let gone: Vec<Uuid> = match sqlx::query_scalar(
        "select f.id from unnest($1::uuid[]) as f(id)
          where not exists(select 1 from attachments a where a.id=f.id)
            and not exists(select 1 from block_media m where m.id=f.id)
            and not exists(select 1 from block_files b where b.id=f.id)",
    )
    .bind(&files)
    .fetch_all(pool)
    .await
    {
        Ok(gone) => gone,
        Err(error) => {
            tracing::warn!(kind = "conversation_delete_file_failed", error = %error, "files left for the orphan sweep");
            return;
        }
    };
    for id in gone {
        let locked = match blobs.try_lock(id) {
            Ok(Some(locked)) => locked,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(kind = "conversation_delete_file_failed", file_id = %id, error = %error.kind(), "left for the orphan sweep");
                continue;
            }
        };
        for result in [
            locked.delete_blob(id).await,
            locked.delete_staging(id).await,
        ] {
            if let Err(error) = result {
                tracing::warn!(kind = "conversation_delete_file_failed", file_id = %id, error = %error.kind(), "left for the orphan sweep");
            }
        }
    }
}

/// Кто просит удаление: ручное удаление уведомляет открытые подписки, уборка по сроку
/// хранения молчит — приём уже закрыл подписки и сообщил итог.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeleteMode {
    Manual,
    Retention,
}

/// Что делать с отметкой корзины при окончательной уборке.
#[derive(Clone, Copy)]
enum TrashGuard {
    /// Принятая делегация удаляется только вне корзины; под блокировкой это перепроверяется.
    Forbid,
    /// Корзина удаляется только после своего срока.
    Before(DateTime<Utc>),
    /// Принятая делегация после срока хранения: всё условие отбора кандидатов
    /// перепроверяется под блокировкой.
    AcceptedBefore(DateTime<Utc>),
}

async fn delete_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    correlation_id: Uuid,
    mode: DeleteMode,
    trash_guard: TrashGuard,
) -> Result<Value, DeleteError> {
    conversations::lock_owner(&mut *conn, owner_id).await?;
    // Задания разговора берутся до очереди и без ожидания: планировщик держит строку
    // задания и потом ждёт очередь, а под блокировкой очереди ждать задание нельзя.
    // Запертое задание уже выполняется; удаление откатывается и повторяется позже.
    let free: Vec<Uuid> = sqlx::query_scalar(
        "select id from scheduled_jobs where conversation_id=$1 order by id for update skip locked",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let busy: bool = sqlx::query_scalar(
        "select exists(select 1 from scheduled_jobs where conversation_id=$1 and id<>all($2))",
    )
    .bind(id)
    .bind(&free)
    .fetch_one(&mut *conn)
    .await?;
    if busy {
        return Err(DeleteError::Busy);
    }
    // Порядок ingest: курсор раньше очереди; очередь раньше строки разговора.
    // Встречное исполнение после блокировки очереди либо живое и даёт отказ, либо его
    // курсор удержит внешний ключ.
    let known: Vec<Uuid> = sqlx::query_scalar("select id from executions where conversation_id=$1")
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    sqlx::query(
        "select journal_id from journal_cursors where execution_id=any($1)
         order by journal_id for update",
    )
    .bind(&known)
    .execute(&mut *conn)
    .await?;
    sqlx::query_scalar::<_, Uuid>(
        "select q.conversation_id from queue_state q join conversations c on c.id=q.conversation_id
         where c.id=$1 and c.owner_id=$2 for update of q",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(DeleteError::NotFound)?;
    let (machine_id, trashed_at): (Uuid, Option<DateTime<Utc>>) = sqlx::query_as(
        "select machine_id,trashed_at from conversations
         where id=$1 and owner_id=$2 for update",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(DeleteError::NotFound)?;
    // Окончательная уборка не трогает сессию, которую успели вернуть: отметка снята
    // или обновлена после отбора кандидатов. Владелец блокировки решает гонку целиком.
    match trash_guard {
        TrashGuard::Forbid if trashed_at.is_some() => return Err(DeleteError::NotTrashed),
        TrashGuard::Before(cutoff) if !matches!(trashed_at, Some(at) if at <= cutoff) => {
            return Err(DeleteError::NotTrashed);
        }
        TrashGuard::AcceptedBefore(cutoff) => {
            let candidate: bool = sqlx::query_scalar(concat!(
                "select exists(select 1 from conversations c where c.id=$2 and ",
                accepted_retention_where!(),
                ")"
            ))
            .bind(cutoff)
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
            if !candidate {
                return Err(DeleteError::NotTrashed);
            }
        }
        _ => {}
    }
    if executions::live(conn, id).await?.is_some() {
        return Err(DeleteError::Live);
    }
    if holds_worktree(conn, id).await? {
        return Err(DeleteError::WorktreeNotRemoved);
    }
    // До очистки runs: расходы уходят родителю одной строкой и переживают уборку.
    crate::session_usage::fold_in_tx(&mut *conn, id).await?;
    let (generation, execution_ids): (i64, Vec<Uuid>) = sqlx::query_as(
        "select coalesce(max(generation),0), coalesce(array_agg(id),'{}') from executions
         where conversation_id=$1",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query(
        "insert into conversation_tombstones(conversation_id,owner_id,machine_id,max_generation,execution_ids)
         values($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(owner_id)
    .bind(machine_id)
    .bind(generation)
    .bind(&execution_ids)
    .execute(&mut *conn)
    .await?;
    crate::session_tools::spawn_queue::cancel_for_parent_in_tx(
        conn,
        owner_id,
        id,
        "parent_trashed",
        correlation_id,
        Utc::now(),
        false,
    )
    .await?;
    crate::session_tools::alarms::on_target_deleted_in_tx(conn, id, Utc::now()).await?;
    if mode == DeleteMode::Manual {
        subs::on_target_deleted_in_tx(conn, id, Utc::now()).await?;
    }
    origin::scrub_sender_in_tx(conn, id).await?;
    // Внешний ключ обнулит связь задач плана молча: запомнить их до удаления.
    let plan_tasks = crate::session_tools::plan::linked_to_in_tx(conn, id).await?;
    // Удаление строк разговора берёт строки плана (обнуление связи исполнителя, каскад
    // плана оркестратора), а событие потом идёт в общий журнал владельца. Писатели плана
    // берут журнал раньше строк плана, поэтому здесь журнал тоже берётся до них.
    if crate::session_tools::plan::touches_in_tx(conn, id).await? {
        crate::events::lock_scope(conn, &Scope::Conversations { owner_id }).await?;
    }
    purge_rows(conn, id, &execution_ids).await?;
    purge_events(conn, owner_id, id, &execution_ids).await?;
    // После чистки событий: прежние события задач с ID исполнителя скрыты.
    crate::session_tools::plan::on_executor_deleted_in_tx(conn, &plan_tasks, correlation_id)
        .await?;
    announce(conn, owner_id, id, correlation_id).await?;
    Ok(json!({"deleted": true}))
}

/// Мягкое удаление в тех же блокировках, что и окончательное: владелец, курсоры журнала,
/// очередь, строка разговора. Строка берётся `FOR NO KEY UPDATE`: подписка держит на ней
/// `FOR KEY SHARE`, и обновление неключевой отметки корзины не должно с ней конфликтовать.
/// Ни одна строка сессии не удаляется.
async fn trash_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    correlation_id: Uuid,
) -> Result<Value, DeleteError> {
    conversations::lock_owner(&mut *conn, owner_id).await?;
    let known: Vec<Uuid> = sqlx::query_scalar("select id from executions where conversation_id=$1")
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    sqlx::query(
        "select journal_id from journal_cursors where execution_id=any($1)
         order by journal_id for update",
    )
    .bind(&known)
    .execute(&mut *conn)
    .await?;
    sqlx::query_scalar::<_, Uuid>(
        "select q.conversation_id from queue_state q join conversations c on c.id=q.conversation_id
         where c.id=$1 and c.owner_id=$2 for update of q",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(DeleteError::NotFound)?;
    let trashed_at: Option<Option<DateTime<Utc>>> = sqlx::query_scalar(
        "select trashed_at from conversations where id=$1 and owner_id=$2 for no key update",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(trashed_at) = trashed_at else {
        return Err(DeleteError::NotFound);
    };
    if trashed_at.is_some() {
        // Повторное удаление не дублирует ни события, ни задания подписчикам.
        return Ok(json!({"trashed": true}));
    }
    if executions::live(conn, id).await?.is_some() {
        return Err(DeleteError::Live);
    }
    if holds_worktree(conn, id).await? {
        return Err(DeleteError::WorktreeNotRemoved);
    }
    sqlx::query(
        "update conversations set trashed_at=clock_timestamp(),updated_at=clock_timestamp()
         where id=$1",
    )
    .bind(id)
    .execute(&mut *conn)
    .await?;
    crate::session_tools::spawn_queue::cancel_for_parent_in_tx(
        conn,
        owner_id,
        id,
        "parent_trashed",
        correlation_id,
        Utc::now(),
        false,
    )
    .await?;
    crate::session_tools::alarms::on_target_deleted_in_tx(conn, id, Utc::now()).await?;
    subs::on_target_deleted_in_tx(conn, id, Utc::now()).await?;
    origin::scrub_sender_in_tx(conn, id).await?;
    crate::notify::cancel_for_conversation_in_tx(conn, id).await?;
    // Список и открытый чат узнают о корзине тем же обновлением атрибутов.
    conversations::emit_attributes(conn, owner_id, id, correlation_id).await?;
    Ok(json!({"trashed": true}))
}

async fn restore_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    correlation_id: Uuid,
) -> Result<ConversationDetail, DeleteError> {
    conversations::lock_owner(&mut *conn, owner_id).await?;
    let trashed_at: Option<Option<DateTime<Utc>>> = sqlx::query_scalar(
        "select trashed_at from conversations where id=$1 and owner_id=$2 for update",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(trashed_at) = trashed_at else {
        return Err(DeleteError::NotFound);
    };
    if trashed_at.is_some() {
        sqlx::query(
            "update conversations set trashed_at=null,updated_at=clock_timestamp() where id=$1",
        )
        .bind(id)
        .execute(&mut *conn)
        .await?;
        conversations::emit_attributes(conn, owner_id, id, correlation_id).await?;
    }
    conversations::detail(&mut *conn, owner_id, id)
        .await?
        .ok_or(DeleteError::NotFound)
}

/// Только строка разговора знает путь и ветку копии, а машине об удалении никто не скажет:
/// без неё каталог и ветка остались бы навсегда. Убирает их приём результата. Сбой уборки и
/// недоступная машина удаление не держат: иначе сессию нельзя было бы удалить вообще.
async fn holds_worktree(conn: &mut PgConnection, id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(select 1 from conversations c join machines m on m.id=c.machine_id
                       where c.id=$1 and c.worktree->>'state' in ('active','dirty','removing')
                         and m.availability<>'unavailable')",
    )
    .bind(id)
    .fetch_one(conn)
    .await
}

/// Без каскадов от разговора: каждая строка выбрана по нему, а незнакомая ссылка снаружи
/// остановит транзакцию внешним ключом вместо того, чтобы удалить чужие данные.
async fn purge_rows(
    conn: &mut PgConnection,
    id: Uuid,
    execution_ids: &[Uuid],
) -> Result<(), sqlx::Error> {
    const BY_CONVERSATION: &[&str] = &[
        "delete from requests where conversation_id=$1",
        "delete from owner_pushes where conversation_id=$1",
        "delete from input_attachments where input_id in
           (select id from inputs where conversation_id=$1)",
        "delete from scheduled_jobs where conversation_id=$1",
        "delete from runs where conversation_id=$1",
        "delete from inputs where conversation_id=$1",
        "delete from blocks where conversation_id=$1",
        "delete from block_media where conversation_id=$1",
        "delete from block_files where conversation_id=$1",
        "delete from commands where conversation_id=$1",
        // Вложение, пересланное в ввод другой сессии (`sessions.send`, бриф `spawn`),
        // переходит к получателю: его ввод ссылается на файл и живёт дольше отправителя.
        // Ссылок из других разговоров может быть несколько; берётся наименьший ID, а при
        // его удалении вложение снова уйдёт следующему. После последней ссылки вложение
        // удаляется вместе с последним разговором, и висячих строк или файлов не остаётся.
        "update attachments a set conversation_id=r.conversation_id
           from (select ia.attachment_id,
                        (array_agg(i.conversation_id order by i.conversation_id))[1] as conversation_id
                   from input_attachments ia join inputs i on i.id=ia.input_id
                  where i.conversation_id<>$1
                  group by ia.attachment_id
                 union all
                 select sa.attachment_id,
                        (array_agg(j.conversation_id order by j.conversation_id))[1]
                   from scheduled_job_attachments sa join scheduled_jobs j on j.id=sa.job_id
                  where j.conversation_id<>$1
                  group by sa.attachment_id) r
          where a.conversation_id=$1 and a.id=r.attachment_id",
        "delete from attachments where conversation_id=$1",
        "delete from conversation_group_pins where conversation_id=$1",
        "delete from queue_state where conversation_id=$1",
        "delete from idempotency_keys where conversation_id=$1",
        "delete from session_subscriptions where subscriber_id=$1 or target_id=$1",
        "delete from machine_event_marks where conversation_id=$1",
    ];
    for statement in BY_CONVERSATION.iter().copied() {
        sqlx::query(statement).bind(id).execute(&mut *conn).await?;
    }
    // Потомки переживают родителя (D21): остаются сессиями без родителя.
    sqlx::query(
        "update conversations set parent_conversation_id=null where parent_conversation_id=$1",
    )
    .bind(id)
    .execute(&mut *conn)
    .await?;
    sqlx::query("update limit_snapshots set execution_id=null where execution_id=any($1)")
        .bind(execution_ids)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "update journal_cursors set execution_id=null, closed=true, discarded=true
         where execution_id=any($1)",
    )
    .bind(execution_ids)
    .execute(&mut *conn)
    .await?;
    sqlx::query("delete from executions where conversation_id=$1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("delete from conversations where id=$1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Журнал области разговора исчезает целиком. Общие журналы владельца нумерованы без пропусков,
/// поэтому там строка теряет содержимое и становится скрытой.
async fn purge_events(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    execution_ids: &[Uuid],
) -> Result<(), sqlx::Error> {
    let scope = Scope::Conversation { id }.key();
    sqlx::query("delete from events where scope=$1")
        .bind(&scope)
        .execute(&mut *conn)
        .await?;
    sqlx::query("delete from scope_counters where scope=$1")
        .bind(&scope)
        .execute(&mut *conn)
        .await?;
    let shared = [
        Scope::Conversations { owner_id }.key(),
        Scope::Machines { owner_id }.key(),
    ];
    let needles: Vec<String> = std::iter::once(id)
        .chain(execution_ids.iter().copied())
        .map(|value| value.to_string())
        .collect();
    sqlx::query(
        "update events set kind='hidden', conversation_id=null, execution_id=null, payload='{}'
         where scope=any($1) and (conversation_id=$2 or execution_id=any($3)
           or exists(select 1 from unnest($4::text[]) needle where payload::text like '%'||needle||'%'))",
    )
    .bind(&shared)
    .bind(id)
    .bind(execution_ids)
    .bind(&needles)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn announce(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let scope = Scope::Conversations { owner_id };
    sqlx::query("insert into scope_counters(scope,last_seq) values($1,0) on conflict do nothing")
        .bind(scope.key())
        .execute(&mut *conn)
        .await?;
    sqlx::query("select scope from scope_counters where scope=$1 for update")
        .bind(scope.key())
        .execute(&mut *conn)
        .await?;
    let counts = conversations::counts(&mut *conn, owner_id).await?;
    events::append(
        conn,
        NewEvent {
            scope,
            kind: "conversation_deleted",
            conversation_id: Some(id),
            execution_id: None,
            payload: json!({"id": id, "counts": counts}),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}
