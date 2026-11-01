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
    session_tools::summary,
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
}

pub async fn run_in_tx(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<JobOutcome, SubmitError> {
    let Some(subscriber) = job.conversation_id else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    match job.payload.get("action").and_then(Value::as_str) {
        Some("event") => {
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
        Some("note") => {
            let Ok(note) = serde_json::from_value::<NoteJob>(job.payload.clone()) else {
                return Ok(JobOutcome::Cancelled("invalid_payload"));
            };
            deliver(conn, job, subscriber, note.text, None, now, correlation_id).await
        }
        Some(_) => Ok(JobOutcome::Cancelled("unsupported")),
        None => Ok(JobOutcome::Cancelled("invalid_payload")),
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
            let origin = json!({"kind":"event","from":id,"title":summary.title,"harness":summary.harness,"model":summary.model,"chain_depth":depth});
            (summary::render(&summary), origin)
        }
        None => (
            format!("[canterlot] Session {id} was deleted."),
            json!({"kind":"event","from":id,"chain_depth":depth}),
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
        },
    )
    .await?;
    if let Some(origin) = origin {
        sqlx::query("update inputs set origin=$2 where id=$1")
            .bind(submitted.input_id)
            .bind(origin)
            .execute(&mut *conn)
            .await?;
    }
    Ok(JobOutcome::Done(json!({"input_id":submitted.input_id})))
}
