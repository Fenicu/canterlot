use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Conversations { owner_id: Uuid },
    Conversation { id: Uuid },
}

impl Scope {
    pub fn key(&self) -> String {
        match self {
            Self::Conversations { owner_id } => format!("owner:{owner_id}:conversations"),
            Self::Conversation { id } => format!("conversation:{id}"),
        }
    }
}

#[derive(Debug)]
pub struct NewEvent {
    pub scope: Scope,
    pub kind: &'static str,
    pub conversation_id: Option<Uuid>,
    pub execution_id: Option<Uuid>,
    pub payload: Value,
    pub correlation_id: Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub struct StoredEvent {
    pub id: Uuid,
    pub scope: String,
    pub scope_seq: i64,
    pub kind: String,
    pub conversation_id: Option<Uuid>,
    pub execution_id: Option<Uuid>,
    pub payload: Value,
    pub correlation_id: Uuid,
    pub committed_at: DateTime<Utc>,
}

/// Вызывать в транзакции изменения состояния; блокировка счётчика удерживается до commit.
pub async fn append(conn: &mut PgConnection, ev: NewEvent) -> Result<StoredEvent, sqlx::Error> {
    sqlx::query_as(
        "with next_seq as (
             insert into scope_counters (scope, last_seq) values ($1, 1)
             on conflict (scope) do update set last_seq = scope_counters.last_seq + 1
             returning last_seq
         )
         insert into events (id, scope, scope_seq, kind, conversation_id, execution_id,
                             payload, correlation_id)
         select $2, $1, last_seq, $3, $4, $5, $6, $7 from next_seq
         returning id, scope, scope_seq, kind, conversation_id, execution_id,
                   payload, correlation_id, committed_at",
    )
    .bind(ev.scope.key())
    .bind(Uuid::now_v7())
    .bind(ev.kind)
    .bind(ev.conversation_id)
    .bind(ev.execution_id)
    .bind(ev.payload)
    .bind(ev.correlation_id)
    .fetch_one(conn)
    .await
}

#[derive(Debug)]
pub enum ReadAfter {
    Events(Vec<StoredEvent>),
    ResyncRequired { oldest_available_seq: i64 },
}

pub async fn read_after(
    pool: &PgPool,
    scope: &Scope,
    after_seq: i64,
    limit: i64,
) -> Result<ReadAfter, sqlx::Error> {
    if after_seq < 0 || limit <= 0 {
        return Err(sqlx::Error::InvalidArgument(
            "after_seq must be nonnegative and limit must be positive".into(),
        ));
    }

    let scope = scope.key();
    let mut tx = pool.begin().await?;
    // Retention и выборка событий должны видеть один снимок.
    sqlx::query("set transaction isolation level repeatable read read only")
        .execute(&mut *tx)
        .await?;
    let (oldest, last_seq): (Option<i64>, i64) = sqlx::query_as(
        "select (select min(scope_seq) from events where scope = $1),
                coalesce((select last_seq from scope_counters where scope = $1), 0)",
    )
    .bind(&scope)
    .fetch_one(&mut *tx)
    .await?;
    let resync_from = match oldest {
        Some(oldest) if after_seq < oldest - 1 => Some(oldest),
        None if after_seq < last_seq => Some(
            last_seq
                .checked_add(1)
                .ok_or_else(|| sqlx::Error::Protocol("scope sequence exhausted".into()))?,
        ),
        _ => None,
    };

    let result = if let Some(oldest_available_seq) = resync_from {
        ReadAfter::ResyncRequired {
            oldest_available_seq,
        }
    } else {
        let events = sqlx::query_as(
            "select id, scope, scope_seq, kind, conversation_id, execution_id,
                    payload, correlation_id, committed_at
             from events where scope = $1 and scope_seq > $2 order by scope_seq limit $3",
        )
        .bind(&scope)
        .bind(after_seq)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        ReadAfter::Events(events)
    };
    tx.commit().await?;
    Ok(result)
}

/// Читать вместе с состоянием в одной транзакции REPEATABLE READ.
pub async fn boundary(conn: &mut PgConnection, scope: &Scope) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("select coalesce((select last_seq from scope_counters where scope = $1), 0)")
        .bind(scope.key())
        .fetch_one(conn)
        .await
}

pub async fn purge_before(
    pool: &PgPool,
    scope: &Scope,
    before_seq: i64,
) -> Result<u64, sqlx::Error> {
    Ok(
        sqlx::query("delete from events where scope = $1 and scope_seq < $2")
            .bind(scope.key())
            .bind(before_seq)
            .execute(pool)
            .await?
            .rows_affected(),
    )
}
