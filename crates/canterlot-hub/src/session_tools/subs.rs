//! Подписки на осевшее состояние другой сессии. Оценка идёт в транзакции проектора цели и
//! не трогает очередь подписчика: событие доходит до неё заданием `session_task`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, types::Json};
use uuid::Uuid;

use super::completion;
use super::settle::{self, SettleConfig, SettleFacts, Settled};
use crate::activity::{self, Activity, ActivityState, Evidence, QueueFacts};

/// Отпечаток события об удалении: цель исчезла, повторно оно не возникнет.
const DELETED: &str = "deleted";

async fn upsert(
    conn: &mut PgConnection,
    owner_id: Uuid,
    subscriber_id: Uuid,
    target_id: Uuid,
    chain_depth: i32,
    fingerprint: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into session_subscriptions(id,owner_id,subscriber_id,target_id,chain_depth,last_fingerprint)
         values($1,$2,$3,$4,$5,$6)
         on conflict(subscriber_id,target_id) do update set
           chain_depth=excluded.chain_depth,last_fingerprint=excluded.last_fingerprint,
           armed_at=clock_timestamp(),closed_at=null,closed_reason=null",
    )
    .bind(Uuid::now_v7())
    .bind(owner_id)
    .bind(subscriber_id)
    .bind(target_id)
    .bind(chain_depth)
    .bind(fingerprint)
    .execute(conn)
    .await?;
    Ok(())
}

/// `session_send` и `session_spawn`: следующий осевший результат придёт, даже если совпадёт с прошлым.
pub async fn subscribe_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    subscriber_id: Uuid,
    target_id: Uuid,
    chain_depth: i32,
) -> Result<(), sqlx::Error> {
    upsert(conn, owner_id, subscriber_id, target_id, chain_depth, None).await
}

/// Явная подписка: об уже осевшем состоянии не сообщает, его видно в ответе инструмента.
pub async fn subscribe_explicit_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    subscriber_id: Uuid,
    target_id: Uuid,
    chain_depth: i32,
    now: DateTime<Utc>,
    config: &SettleConfig,
) -> Result<Option<Settled>, sqlx::Error> {
    let current = current_in_tx(conn, target_id, now, config).await?;
    let fingerprint = current.as_ref().map(|value| value.fingerprint.as_str());
    upsert(
        conn,
        owner_id,
        subscriber_id,
        target_id,
        chain_depth,
        fingerprint,
    )
    .await?;
    Ok(current.map(|value| value.state))
}

/// Отписка забирает и событие, которое уже ждёт в заданиях: подписчик больше о нём не просил.
/// Сначала подписка: оценка держит её строку, поэтому её задание к этому моменту уже видно.
pub async fn unsubscribe_in_tx(
    conn: &mut PgConnection,
    subscriber_id: Uuid,
    target_id: Uuid,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update session_subscriptions
         set closed_at=coalesce(closed_at,clock_timestamp()),closed_reason=$3
         where subscriber_id=$1 and target_id=$2
           and (closed_at is null or closed_reason=$4)",
    )
    .bind(subscriber_id)
    .bind(target_id)
    .bind(reason)
    .bind(Settled::Idle.as_str())
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "update scheduled_jobs set state='cancelled',result=$3,updated_at=clock_timestamp()
         where kind='session_task' and state='pending' and conversation_id=$1
           and payload->>'action'='event' and payload->>'target_id'=$2",
    )
    .bind(subscriber_id)
    .bind(target_id.to_string())
    .bind(json!({"reason": reason}))
    .execute(conn)
    .await?;
    Ok(())
}

/// Приём результата закрывает подписки в обе стороны: сама принятая сессия ни о ком не ждёт,
/// а принявший родитель о ней больше не спрашивает. Ожидающие события для них отменяются,
/// чтобы остановка при приёме не вернулась родителю как новый итог. Чужие подписчики и их
/// ожидающие события не трогаются: возвращается список закрытых чужих подписок, им надо
/// сообщить о приёме отдельно. Задания в работе пропускаются: порядок блокировок
/// у них другой, а доставка одного события безвредна.
pub async fn close_for_accept_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    parent: Option<Uuid>,
    reason: &str,
) -> Result<Vec<ClosedSubscriber>, sqlx::Error> {
    let closed: Vec<(Uuid, Uuid, Uuid, i32)> = sqlx::query_as(
        "update session_subscriptions set closed_at=clock_timestamp(),closed_reason=$2
         where (target_id=$1 or subscriber_id=$1) and closed_at is null
         returning owner_id,subscriber_id,target_id,chain_depth",
    )
    .bind(conversation_id)
    .bind(reason)
    .fetch_all(&mut *conn)
    .await?;
    sqlx::query(
        "update session_subscriptions set closed_reason=$2 where target_id=$1 and closed_reason=$3",
    )
    .bind(conversation_id)
    .bind(reason)
    .bind(Settled::Idle.as_str())
    .execute(&mut *conn)
    .await?;
    let pending: Vec<Uuid> = sqlx::query_scalar(
        "select id from scheduled_jobs
         where kind='session_task' and state='pending' and payload->>'action'='event'
           and (conversation_id=$1
                or (payload->>'target_id'=$2 and conversation_id is not distinct from $3))
         for update skip locked",
    )
    .bind(conversation_id)
    .bind(conversation_id.to_string())
    .bind(parent)
    .fetch_all(&mut *conn)
    .await?;
    sqlx::query(
        "update scheduled_jobs set state='cancelled',result=$2,updated_at=clock_timestamp()
         where id=any($1)",
    )
    .bind(&pending)
    .bind(json!({"reason": reason}))
    .execute(conn)
    .await?;
    Ok(closed
        .into_iter()
        .filter(|(_, subscriber, target, _)| {
            *target == conversation_id
                && *subscriber != conversation_id
                && Some(*subscriber) != parent
        })
        .map(
            |(owner_id, subscriber_id, _, chain_depth)| ClosedSubscriber {
                owner_id,
                subscriber_id,
                chain_depth,
            },
        )
        .collect())
}

/// Подписчик принятой сессии, не принимавший её сам.
pub struct ClosedSubscriber {
    pub owner_id: Uuid,
    pub subscriber_id: Uuid,
    pub chain_depth: i32,
}

/// Цель работает дальше без нового ввода: подписчик услышал `idle`, но это был не конец.
/// Подписка открывается снова со своей глубиной и окном, следующий итог дойдёт до него.
pub async fn reopen_for_wake_turn_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update session_subscriptions set closed_at=null,closed_reason=null
         where target_id=$1 and closed_reason=$2",
    )
    .bind(target_id)
    .bind(Settled::Idle.as_str())
    .execute(conn)
    .await?;
    Ok(())
}

/// Осевшее состояние цели на момент чтения.
pub struct Current {
    pub state: Settled,
    pub fingerprint: String,
    pub closes: bool,
}

/// Осевшее состояние по проекции: отпечаток должен совпасть с тем, что посчитает проектор.
pub async fn current_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    now: DateTime<Utc>,
    config: &SettleConfig,
) -> Result<Option<Current>, sqlx::Error> {
    let derived: Option<(Json<ActivityState>, Json<Evidence>, DateTime<Utc>)> = sqlx::query_as(
        "select to_jsonb(activity_state),activity_evidence,activity_since
         from conversation_derived where conversation_id=$1",
    )
    .bind(target_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((Json(state), Json(evidence), since)) = derived else {
        return Ok(None);
    };
    let inputs = activity::load_inputs(conn, target_id, now, Duration::ZERO).await?;
    let facts = SettleFacts {
        now,
        since,
        last_turn_at: inputs.last_turn_at,
        last_event_at: inputs.last_event_at,
        phase_since: inputs
            .execution
            .as_ref()
            .and_then(|execution| execution.phase_at),
    };
    let activity = Activity { state, evidence };
    Ok(
        settle::settled(&activity, &inputs.queue, &facts, config).map(|state| Current {
            state,
            fingerprint: settle::fingerprint(state, since, inputs.last_turn_at, facts.phase_since),
            closes: state.closes_subscription(inputs.queue.queued),
        }),
    )
}

fn event_job(
    subscription_id: Uuid,
    target_id: Uuid,
    state: &str,
    fingerprint: &str,
    armed_at: DateTime<Utc>,
    chain_depth: i32,
) -> Value {
    json!({
        "action": "event",
        "subscription_id": subscription_id,
        "target_id": target_id,
        "state": state,
        "fingerprint": fingerprint,
        "armed_at": armed_at,
        "chain_depth": chain_depth,
    })
}

pub(super) async fn insert_job(
    conn: &mut PgConnection,
    owner_id: Uuid,
    subscriber_id: Uuid,
    run_at: DateTime<Utc>,
    payload: &Value,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'session_task',$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(owner_id)
    .bind(subscriber_id)
    .bind(run_at)
    .bind(payload)
    .execute(conn)
    .await?;
    Ok(id)
}

/// Вызывается проектором цели под её очередью. Возвращает, создано ли задание:
/// после commit планировщик надо разбудить.
pub async fn evaluate_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    activity: &Activity,
    queue: &QueueFacts,
    facts: &SettleFacts,
    config: &SettleConfig,
) -> Result<bool, sqlx::Error> {
    let Some(state) = settle::settled(activity, queue, facts, config) else {
        return Ok(false);
    };
    if !completion::is_completion(state) {
        return Ok(false);
    }
    let fingerprint =
        settle::fingerprint(state, facts.since, facts.last_turn_at, facts.phase_since);
    // Удаление подписчика держит его строку и потом удаляет подписку: без блокировки строки
    // разговора здесь вставка задания ждала бы его, а оно ждало бы подписку.
    let open: Vec<(Uuid, Uuid, Uuid, i32, DateTime<Utc>)> = sqlx::query_as(
        "select s.id,s.owner_id,s.subscriber_id,s.chain_depth,s.armed_at
         from session_subscriptions s join conversations c on c.id=s.subscriber_id
         where s.target_id=$1 and s.closed_at is null and s.last_fingerprint is distinct from $2
         for update of s for key share of c skip locked",
    )
    .bind(target_id)
    .bind(&fingerprint)
    .fetch_all(&mut *conn)
    .await?;
    let closes = state.closes_subscription(queue.queued);
    for (id, owner_id, subscriber_id, chain_depth, armed_at) in &open {
        if matches!(state, Settled::Idle | Settled::Stopped)
            && crate::session_tools::alarms::hold_for_batch_in_tx(
                conn,
                *subscriber_id,
                target_id,
                state.as_str(),
                &fingerprint,
                *id,
                *armed_at,
                *chain_depth,
                facts.now,
            )
            .await?
        {
            // Пачка сама сдвигает отпечаток, но подписку не закрывает: отпускание
            // удержанного проверяет, что она жива и того же поколения.
            sqlx::query("update session_subscriptions set last_fingerprint=$2 where id=$1")
                .bind(id)
                .bind(&fingerprint)
                .execute(&mut *conn)
                .await?;
            continue;
        }
        sqlx::query(
            "update session_subscriptions set last_fingerprint=$2,
               closed_at=case when $3 then clock_timestamp() end,
               closed_reason=case when $3 then $4 end
             where id=$1",
        )
        .bind(id)
        .bind(&fingerprint)
        .bind(closes)
        .bind(state.as_str())
        .execute(&mut *conn)
        .await?;
        let payload = event_job(
            *id,
            target_id,
            state.as_str(),
            &fingerprint,
            *armed_at,
            *chain_depth,
        );
        insert_job(conn, *owner_id, *subscriber_id, facts.now, &payload).await?;
    }
    Ok(!open.is_empty())
}

/// Отпускание итога, удержанного пачкой: подписка должна быть жива и того же поколения.
/// Устаревший итог отбрасывается, чтобы отписка, приём или переподписка не воскрешали его.
#[allow(clippy::too_many_arguments)]
pub(super) async fn release_held_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    subscriber_id: Uuid,
    target_id: Uuid,
    subscription_id: Uuid,
    armed_at: DateTime<Utc>,
    state: &str,
    fingerprint: &str,
    chain_depth: i32,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let alive: Option<Uuid> = sqlx::query_scalar(
        "select id from session_subscriptions
         where id=$1 and subscriber_id=$2 and target_id=$3 and armed_at=$4 and closed_at is null",
    )
    .bind(subscription_id)
    .bind(subscriber_id)
    .bind(target_id)
    .bind(armed_at)
    .fetch_optional(&mut *conn)
    .await?;
    if alive.is_none() {
        return Ok(false);
    }
    let payload = event_job(
        subscription_id,
        target_id,
        state,
        fingerprint,
        armed_at,
        chain_depth,
    );
    insert_job(conn, owner_id, subscriber_id, now, &payload).await?;
    Ok(true)
}

/// Удаление цели: сами подписки уйдут вместе с разговором. В задании только идентификатор,
/// название удалённой сессии дальше не живёт.
pub async fn on_target_deleted_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let open: Vec<(Uuid, Uuid, Uuid, i32, DateTime<Utc>)> = sqlx::query_as(
        "select id,owner_id,subscriber_id,chain_depth,armed_at from session_subscriptions
         where target_id=$1 and subscriber_id<>$1 and closed_at is null",
    )
    .bind(target_id)
    .fetch_all(&mut *conn)
    .await?;
    for (id, owner_id, subscriber_id, chain_depth, armed_at) in open {
        let payload = event_job(id, target_id, DELETED, DELETED, armed_at, chain_depth);
        insert_job(conn, owner_id, subscriber_id, now, &payload).await?;
    }
    Ok(())
}
