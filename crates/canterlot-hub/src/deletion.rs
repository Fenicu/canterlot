//! Удаление разговора (§14.1, R6). Пилот удаляет остановленный разговор одной транзакцией:
//! локальную очистку на машине протокол пока не описывает, поэтому копии воркера остаются.

use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    attachments::BlobStore,
    conversations, events,
    events::{NewEvent, Scope},
    executions, idempotency,
};

#[derive(Debug, Error)]
pub enum DeleteError {
    #[error("conversation not found")]
    NotFound,
    #[error("conversation deleted")]
    Deleted,
    #[error("execution is live")]
    Live,
    #[error("referenced from another conversation")]
    Referenced,
    #[error("idempotency conflict")]
    IdempotencyConflict,
    #[error(transparent)]
    Db(sqlx::Error),
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
        move |conn| Box::pin(delete_in_tx(conn, owner_id, id, correlation_id)),
    )
    .await;
    match outcome {
        Ok(done) if !done.replayed => free_files(blobs, files).await,
        Ok(_) => {}
        Err(DeleteError::NotFound) if tombstoned(pool, owner_id, id).await? => {
            return Err(DeleteError::Deleted);
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

async fn owned_files(pool: &PgPool, owner_id: Uuid, id: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select a.id from attachments a join conversations c on c.id=a.conversation_id
          where c.id=$1 and c.owner_id=$2
         union all
         select m.id from block_media m join conversations c on c.id=m.conversation_id
          where c.id=$1 and c.owner_id=$2",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_all(pool)
    .await
}

/// Файлы адресуются своим ID, поэтому чужих среди них нет. Занятый или неудалённый файл
/// без строки в каталоге позже подберёт сборщик вложений.
async fn free_files(blobs: &dyn BlobStore, files: Vec<Uuid>) {
    for id in files {
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

async fn delete_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    correlation_id: Uuid,
) -> Result<Value, DeleteError> {
    conversations::lock_owner(&mut *conn, owner_id).await?;
    // Порядок ingest: курсор раньше очереди. Встречное исполнение после блокировки очереди
    // либо живое и даёт отказ, либо его курсор удержит внешний ключ.
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
    let machine_id: Uuid = sqlx::query_scalar(
        "select machine_id from conversations where id=$1 and owner_id=$2 for update",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(DeleteError::NotFound)?;
    if executions::live(conn, id).await?.is_some() {
        return Err(DeleteError::Live);
    }
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
    purge_rows(conn, id, &execution_ids).await?;
    purge_events(conn, owner_id, id, &execution_ids).await?;
    announce(conn, owner_id, id, correlation_id).await?;
    Ok(json!({"deleted": true}))
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
        "delete from input_attachments where input_id in
           (select id from inputs where conversation_id=$1)",
        "delete from scheduled_jobs where conversation_id=$1",
        "delete from runs where conversation_id=$1",
        "delete from inputs where conversation_id=$1",
        "delete from blocks where conversation_id=$1",
        "delete from block_media where conversation_id=$1",
        "delete from commands where conversation_id=$1",
        "delete from attachments where conversation_id=$1",
        "delete from queue_state where conversation_id=$1",
        "delete from idempotency_keys where conversation_id=$1",
        "delete from session_subscriptions where subscriber_id=$1 or target_id=$1",
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
