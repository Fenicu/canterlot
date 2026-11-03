//! Задание `session_task`: в его транзакции живёт только очередь адресата, поэтому
//! две очереди вместе никогда не блокируются.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{
    crunch::JobOutcome,
    delivery::{ContentBlock, SubmitError, SubmitInput, submit_input_in_tx},
    scheduler::ClaimedJob,
    session::Registry,
    session_tools::{accept, origin, spawn, summary},
};

#[derive(Deserialize)]
struct EventJob {
    target_id: Uuid,
    state: String,
    chain_depth: i32,
    #[serde(default)]
    armed_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct NoteJob {
    text: String,
    #[serde(default)]
    origin: Option<Value>,
}

/// Действие задания `session_task`. Разбор исчерпывающий: новое действие обязано
/// получить обработчик в `run_in_tx` и `abandon_in_tx`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Event,
    Note,
    Spawn,
    SpawnClose,
    Cleanup,
}

impl Action {
    #[cfg(test)]
    const ALL: [Self; 5] = [
        Self::Event,
        Self::Note,
        Self::Spawn,
        Self::SpawnClose,
        Self::Cleanup,
    ];

    fn parse(value: &str) -> Option<Self> {
        match value {
            "event" => Some(Self::Event),
            "note" => Some(Self::Note),
            "spawn" => Some(Self::Spawn),
            "spawn_close" => Some(Self::SpawnClose),
            "cleanup" => Some(Self::Cleanup),
            _ => None,
        }
    }
}

pub async fn run_in_tx(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    now: DateTime<Utc>,
    correlation_id: Uuid,
    registry: Option<&Registry>,
) -> Result<JobOutcome, SubmitError> {
    let Some(subscriber) = job.conversation_id else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    let Some(name) = job.payload.get("action").and_then(Value::as_str) else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    let Some(action) = Action::parse(name) else {
        return Ok(JobOutcome::Cancelled("unsupported"));
    };
    match action {
        Action::Event => {
            let Ok(event) = serde_json::from_value::<EventJob>(job.payload.clone()) else {
                return Ok(JobOutcome::Cancelled("invalid_payload"));
            };
            let (text, origin) = event_message(conn, job.owner_id, &event).await?;
            deliver(
                conn,
                job,
                subscriber,
                text,
                Some(origin),
                now,
                correlation_id,
            )
            .await
        }
        Action::Note => {
            let Ok(note) = serde_json::from_value::<NoteJob>(job.payload.clone()) else {
                return Ok(JobOutcome::Cancelled("invalid_payload"));
            };
            deliver(
                conn,
                job,
                subscriber,
                note.text,
                note.origin,
                now,
                correlation_id,
            )
            .await
        }
        Action::Spawn => spawn::run_job(conn, job, subscriber, registry, now, correlation_id).await,
        Action::SpawnClose => spawn::close_job(conn, job, subscriber, correlation_id).await,
        Action::Cleanup => {
            Ok(accept::cleanup_job(conn, job, subscriber, now, correlation_id).await?)
        }
    }
}

/// Цели уже нет, если её удалили: и своё событие об удалении, и ждавшее до него говорят одно.
async fn event_message(
    conn: &mut PgConnection,
    owner_id: Uuid,
    event: &EventJob,
) -> Result<(String, Value), sqlx::Error> {
    let id = event.target_id;
    let depth = event.chain_depth;
    let summary = summary::build(conn, owner_id, id, &event.state, event.armed_at).await?;
    Ok(match summary {
        Some(summary) => {
            let event = summary.hub_event();
            let origin = json!({"kind":"event","from":id,"title":summary.title,"harness":summary.harness,"model":summary.model,"chain_depth":depth,"event":event});
            (summary::render(&summary), origin)
        }
        None => (
            format!("[canterlot] Session {id} was deleted."),
            json!({"kind":"event","from":id,"chain_depth":depth,"event":origin::HubEvent::deleted(id)}),
        ),
    })
}

async fn deliver(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    subscriber: Uuid,
    text: String,
    origin: Option<Value>,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<JobOutcome, SubmitError> {
    let key = format!("sub:{}", job.id);
    let existing: Option<Uuid> =
        sqlx::query_scalar("select id from inputs where owner_id=$1 and idempotency_key=$2")
            .bind(job.owner_id)
            .bind(&key)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some(input_id) = existing {
        return Ok(JobOutcome::Done(json!({"input_id":input_id})));
    }
    let submitted = submit_input_in_tx(
        conn,
        &SubmitInput {
            owner_id: job.owner_id,
            conversation_id: subscriber,
            idempotency_key: key,
            content: vec![ContentBlock::Text { text }],
            received_at: now,
            correlation_id,
            origin,
        },
    )
    .await?;
    Ok(JobOutcome::Done(json!({"input_id":submitted.input_id})))
}

/// Задание исчерпало попытки или признано негодным: то, что оно должно было закрыть, закрывается
/// здесь, в той же транзакции. Остальным видам задания закрывать нечего.
pub async fn abandon_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Option<Uuid>,
    payload: &Value,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let Some(conversation) = conversation_id else {
        return Ok(());
    };
    let correlation_id = Uuid::now_v7();
    let Some(name) = payload.get("action").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(action) = Action::parse(name) else {
        return Ok(());
    };
    match action {
        Action::Event | Action::Note => Ok(()),
        Action::Spawn => {
            spawn::abandon_spawn_in_tx(conn, conversation, payload, now, correlation_id).await
        }
        Action::SpawnClose => {
            spawn::abandon_close_in_tx(conn, conversation, payload, correlation_id).await
        }
        Action::Cleanup => {
            accept::abandon_cleanup_in_tx(conn, owner_id, conversation, now, correlation_id).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Разбор действия исчерпывающий в `abandon_in_tx`: новое действие без обработчика
    /// не собирается. Тест перечисляет те же варианты без `_`, чтобы это заметить.
    #[sqlx::test(migrations = "./migrations")]
    async fn every_action_kind_has_an_abandon_handler(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let seed = crate::testkit::seed(&pool).await;
        let mut conn = pool.acquire().await?;
        for action in Action::ALL {
            let payload = match action {
                Action::Event => json!({"action": "event"}),
                Action::Note => json!({"action": "note"}),
                Action::Spawn => json!({"action": "spawn", "request_id": Uuid::now_v7()}),
                Action::SpawnClose => json!({
                    "action": "spawn_close",
                    "request_id": Uuid::now_v7(),
                    "state": "decision_applied",
                    "reason": "job_failed",
                }),
                Action::Cleanup => json!({"action": "cleanup"}),
            };
            abandon_in_tx(
                &mut conn,
                seed.owner_id,
                Some(seed.conversation_id),
                &payload,
                Utc::now(),
            )
            .await?;
        }
        Ok(())
    }
}
