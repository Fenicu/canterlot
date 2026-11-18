//! Цель разговора: последний полный снимок от адаптера и живые события владельцу.

use canterlot_proto::goals::{GoalState, GoalStatus, MAX_OBJECTIVE_BYTES};
use canterlot_proto::worker::v1::GoalChanged;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    blocks::ExecutionRef,
    events::{self, NewEvent, Scope},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema, sqlx::FromRow)]
pub struct GoalView {
    pub objective: String,
    pub status: String,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub token_budget: Option<i64>,
    pub turns_used: Option<i64>,
    #[serde(with = "crate::model::utc_time")]
    pub updated_at: DateTime<Utc>,
}

fn bound(text: &str) -> String {
    let mut end = text.len().min(MAX_OBJECTIVE_BYTES);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn millis(ms: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(ms.clamp(0, i64::MAX))
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp_millis(0).expect("epoch"))
}

/// Применяет запись SJ. Возвращает `true`, если запись устарела и событий не было.
pub async fn apply(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &GoalChanged,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let generation = i64::try_from(fact.generation)
        .map_err(|_| sqlx::Error::InvalidArgument("invalid goal_changed: generation".into()))?;
    let Some((owner_id, harness)): Option<(Uuid, String)> =
        sqlx::query_as("select owner_id, harness from conversations where id = $1")
            .bind(run.conversation_id)
            .fetch_optional(&mut *conn)
            .await?
    else {
        return Ok(true);
    };
    if harness != "codex" && harness != "claude_code" {
        return Ok(true);
    }

    let stored: Option<GoalView> = match &fact.goal {
        Some(proto) => {
            let status = GoalStatus::parse(&proto.status).unwrap_or_default();
            let updated_at = millis(proto.updated_at_ms);
            sqlx::query_as(
                "insert into conversation_goals as g
                   (conversation_id, generation, objective, status, tokens_used,
                    time_used_seconds, token_budget, turns_used, updated_at)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 on conflict (conversation_id) do update set
                   generation = excluded.generation, objective = excluded.objective,
                   status = excluded.status, tokens_used = excluded.tokens_used,
                   time_used_seconds = excluded.time_used_seconds,
                   token_budget = excluded.token_budget, turns_used = excluded.turns_used,
                   updated_at = excluded.updated_at
                 where g.generation <= excluded.generation
                 returning objective, status, tokens_used, time_used_seconds,
                           token_budget, turns_used, updated_at",
            )
            .bind(run.conversation_id)
            .bind(generation)
            .bind(bound(&proto.objective))
            .bind(status.as_str())
            .bind(proto.tokens_used.clamp(0, i64::MAX))
            .bind(proto.time_used_seconds.clamp(0, i64::MAX))
            .bind(proto.token_budget)
            .bind(proto.turns_used)
            .bind(updated_at)
            .fetch_optional(&mut *conn)
            .await?
        }
        None => {
            sqlx::query_as(
                "delete from conversation_goals where conversation_id = $1 and generation <= $2
                 returning objective, status, tokens_used, time_used_seconds,
                           token_budget, turns_used, updated_at",
            )
            .bind(run.conversation_id)
            .bind(generation)
            .fetch_optional(&mut *conn)
            .await?
        }
    };
    let published = if fact.goal.is_some() {
        stored.as_ref()
    } else {
        None
    };
    publish(
        conn,
        owner_id,
        run.conversation_id,
        published,
        correlation_id,
    )
    .await?;
    Ok(stored.is_none())
}

pub async fn in_snapshot(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Option<GoalView>, sqlx::Error> {
    sqlx::query_as(
        "select objective, status, tokens_used, time_used_seconds,
                token_budget, turns_used, updated_at
         from conversation_goals where conversation_id = $1",
    )
    .bind(conversation_id)
    .fetch_optional(conn)
    .await
}

pub async fn clear_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    owner_id: Uuid,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let deleted = sqlx::query("delete from conversation_goals where conversation_id = $1")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    if deleted == 0 {
        return Ok(false);
    }
    publish(conn, owner_id, conversation_id, None, correlation_id).await?;
    Ok(true)
}

pub async fn publish(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    goal: Option<&GoalView>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let payload = json!({"conversation_id": conversation_id, "goal": goal});
    // Список владельца обновляет шапку на всех устройствах; область разговора
    // доставляет то же состояние открытому чату без перехода через список.
    for scope in [
        Scope::Conversations { owner_id },
        Scope::Conversation {
            id: conversation_id,
        },
    ] {
        events::append(
            conn,
            NewEvent {
                scope,
                kind: "goal_changed",
                conversation_id: Some(conversation_id),
                execution_id: None,
                payload: payload.clone(),
                correlation_id,
            },
        )
        .await?;
    }
    Ok(())
}

/// Преобразование для тестов и снимков: из общей модели адаптера в представление API.
pub fn view(goal: GoalState) -> GoalView {
    GoalView {
        objective: bound(&goal.objective),
        status: goal.status.as_str().into(),
        tokens_used: goal.tokens_used,
        time_used_seconds: goal.time_used_seconds,
        token_budget: goal.token_budget,
        turns_used: goal.turns_used,
        updated_at: millis(goal.updated_at_ms),
    }
}
