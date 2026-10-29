use std::fmt;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Conversations { owner_id: Uuid },
    Conversation { id: Uuid },
    Machines { owner_id: Uuid },
}

impl Scope {
    pub fn key(&self) -> String {
        match self {
            Self::Conversations { owner_id } => format!("owner:{owner_id}:conversations"),
            Self::Machines { owner_id } => format!("owner:{owner_id}:machines"),
            Self::Conversation { id } => format!("conversation:{id}"),
        }
    }
}

pub struct NewEvent {
    pub scope: Scope,
    pub kind: &'static str,
    pub conversation_id: Option<Uuid>,
    pub execution_id: Option<Uuid>,
    pub payload: Value,
    pub correlation_id: Uuid,
}

#[derive(sqlx::FromRow)]
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
    let stored: StoredEvent = sqlx::query_as(
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
    .fetch_one(&mut *conn)
    .await?;
    crate::bus::appended(conn, &stored.scope, &stored.kind, stored.scope_seq).await?;
    Ok(stored)
}

pub async fn purge_older_than(pool: &PgPool, age: std::time::Duration) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "delete from events where committed_at < clock_timestamp() - $1 * interval '1 second'",
    )
    .bind(age.as_secs_f64())
    .execute(pool)
    .await?
    .rows_affected())
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
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            // Retention и выборка событий должны видеть один снимок.
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            let (oldest, last_seq): (Option<i64>, i64) = sqlx::query_as(
                "select (select min(scope_seq) from events where scope = $1),
                coalesce((select last_seq from scope_counters where scope = $1), 0)",
            )
            .bind(&scope)
            .fetch_one(&mut *conn)
            .await?;
            let resync_from =
                if after_seq > last_seq {
                    Some(oldest.unwrap_or(
                        last_seq.checked_add(1).ok_or_else(|| {
                            sqlx::Error::Protocol("scope sequence exhausted".into())
                        })?,
                    ))
                } else {
                    match oldest {
                        Some(oldest) if after_seq < oldest - 1 => Some(oldest),
                        None if after_seq < last_seq => {
                            Some(last_seq.checked_add(1).ok_or_else(|| {
                                sqlx::Error::Protocol("scope sequence exhausted".into())
                            })?)
                        }
                        _ => None,
                    }
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
                .fetch_all(&mut *conn)
                .await?;
                ReadAfter::Events(events)
            };
            Ok(result)
        })
    })
    .await
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

fn redacted_payload(value: &Value) -> Value {
    match value {
        Value::String(text) => serde_json::json!({"bytes": text.len()}),
        Value::Array(values) => Value::Array(values.iter().map(redacted_payload).collect()),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), redacted_payload(value)))
                .collect(),
        ),
        value => value.clone(),
    }
}

impl fmt::Debug for NewEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewEvent")
            .field("scope", &self.scope)
            .field("kind", &self.kind)
            .field("conversation_id", &self.conversation_id)
            .field("execution_id", &self.execution_id)
            .field("payload", &redacted_payload(&self.payload))
            .field("correlation_id", &self.correlation_id)
            .finish()
    }
}

impl fmt::Debug for StoredEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredEvent")
            .field("id", &self.id)
            .field("scope", &self.scope)
            .field("scope_seq", &self.scope_seq)
            .field("kind", &self.kind)
            .field("conversation_id", &self.conversation_id)
            .field("execution_id", &self.execution_id)
            .field("payload", &redacted_payload(&self.payload))
            .field("correlation_id", &self.correlation_id)
            .field("committed_at", &self.committed_at)
            .finish()
    }
}

pub const CLIENT_KINDS: &[&str] = &[
    "input_queued",
    "input_state_changed",
    "queue_state_changed",
    "queue_paused",
    "queue_resumed",
    "execution_state_changed",
    "phase_changed",
    "run_finished",
    "interrupt_state_changed",
    "block_started",
    "block_delta",
    "block_final",
    "block_incomplete",
    "adapter_diagnostic",
    "state_divergence",
    "block_revision_gap",
    "block_checksum_mismatch",
    "block_conflicting_final",
    "block_media_attached",
    "conversation_created",
    "conversation_updated",
    "conversation_settings_changed",
    "crunch_changed",
    "crunch_held",
    "crunch_woken",
    "derived_changed",
    "group_changed",
    "instruction_preset_changed",
    "scheduled_changed",
    "limits_changed",
    "limit_reached",
    "attachment_state_changed",
    "request_opened",
    "request_closed",
    "request_decided",
    "request_transmitting",
    "machine_availability_changed",
    "machine_report_updated",
    "machine_metrics",
    "hidden",
    "oversized",
];

pub fn is_client_visible(kind: &str) -> bool {
    CLIENT_KINDS.contains(&kind)
}

pub fn wire_scope(scope: &Scope) -> String {
    match scope {
        Scope::Conversations { .. } => "conversations".into(),
        Scope::Machines { .. } => "machines".into(),
        Scope::Conversation { id } => format!("conversation:{id}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequestOpenedPayload {
    pub request_id: Uuid,
    pub kind: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequestDecidedPayload {
    pub request_id: Uuid,
    pub winner_decision_id: Uuid,
    pub decision: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequestTransmittingPayload {
    pub request_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequestClosedPayload {
    pub request_id: Uuid,
    pub reason: String,
}

#[derive(Clone, serde::Serialize, utoipa::ToSchema)]
pub struct WireEvent {
    pub v: u8,
    pub id: Uuid,
    pub scope: String,
    pub scope_seq: String,
    pub scope_seq_from: String,
    pub committed_at: String,
    pub committed_at_from: String,
    pub kind: String,
    pub conversation_id: Option<Uuid>,
    pub execution_id: Option<Uuid>,
    pub payload: Value,
    pub correlation_id: Uuid,
}

impl fmt::Debug for WireEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WireEvent")
            .field("id", &self.id)
            .field("scope", &self.scope)
            .field("scope_seq", &self.scope_seq)
            .field("kind", &self.kind)
            .field("payload", &redacted_payload(&self.payload))
            .finish()
    }
}

fn payload_seqs(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if (key == "seq" || key.ends_with("_seq")) && value.is_number() {
                    *value = Value::String(value.to_string());
                } else {
                    payload_seqs(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(payload_seqs),
        _ => {}
    }
}

pub fn project_for_client(events: Vec<StoredEvent>) -> Vec<WireEvent> {
    let mut result: Vec<WireEvent> = Vec::with_capacity(events.len());
    let mut previous_scope = String::new();
    let mut previous_seq = None;
    for e in events {
        let hidden = e.kind == "hidden" || !is_client_visible(&e.kind);
        let at = crate::conversations::time(e.committed_at);
        if hidden
            && previous_scope == e.scope
            && previous_seq == e.scope_seq.checked_sub(1)
            && let Some(last) = result.last_mut().filter(|last| last.kind == "hidden")
        {
            last.scope_seq = e.scope_seq.to_string();
            last.committed_at = at;
            previous_seq = Some(e.scope_seq);
            continue;
        }
        let scope = if e.scope.starts_with("conversation:") {
            e.scope.clone()
        } else if e.scope.ends_with(":machines") {
            "machines".into()
        } else {
            "conversations".into()
        };
        previous_scope = e.scope;
        previous_seq = Some(e.scope_seq);
        let mut payload = if hidden {
            serde_json::json!({})
        } else {
            e.payload
        };
        payload_seqs(&mut payload);
        result.push(WireEvent {
            v: 1,
            id: e.id,
            scope,
            scope_seq: e.scope_seq.to_string(),
            scope_seq_from: e.scope_seq.to_string(),
            committed_at: at.clone(),
            committed_at_from: at,
            kind: if hidden { "hidden".into() } else { e.kind },
            conversation_id: if hidden { None } else { e.conversation_id },
            execution_id: if hidden { None } else { e.execution_id },
            payload,
            correlation_id: e.correlation_id,
        });
    }
    result
}

pub struct EventRange {
    pub head: i64,
    pub result: ReadAfter,
    pub cursor_ahead: bool,
}

pub async fn range(
    pool: &PgPool,
    scope: &Scope,
    after: i64,
    limit: i64,
) -> Result<EventRange, sqlx::Error> {
    if after < 0 || !(1..=256).contains(&limit) {
        return Err(sqlx::Error::InvalidArgument("invalid event range".into()));
    }
    let scope = *scope;
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            let head = boundary(&mut *conn, &scope).await?;
            let oldest: Option<i64> =
                sqlx::query_scalar("select min(scope_seq) from events where scope=$1")
                    .bind(scope.key())
                    .fetch_one(&mut *conn)
                    .await?;
            let cursor_ahead = after > head;
            let oldest = oldest.unwrap_or(head.saturating_add(1));
            let result = if cursor_ahead || after < oldest - 1 {
                ReadAfter::ResyncRequired {
                    oldest_available_seq: oldest,
                }
            } else {
                ReadAfter::Events(
            sqlx::query_as(
                "select * from events where scope=$1 and scope_seq>$2 order by scope_seq limit $3",
            )
            .bind(scope.key())
            .bind(after)
            .bind(limit)
            .fetch_all(&mut *conn)
            .await?,
        )
            };
            Ok(EventRange {
                head,
                result,
                cursor_ahead,
            })
        })
    })
    .await
}
