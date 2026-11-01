//! Правила лимитов подписки (pilot.md §8) поверх хранения `limits`: удержания «кранча»,
//! недельный резерв, пробуждение после подтверждённого сброса, переопределение владельцем и опрос.

use std::{fmt, time::Duration};

use canterlot_proto::{
    harness,
    worker::v1::{ControlDown, LimitReached, PollLimits, control_down},
};
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, types::Json};
use uuid::Uuid;

use crate::{
    blocks::ExecutionRef,
    config::LimitsConfig,
    delivery::{ContentBlock, LockedQueue, SubmitError, SubmitInput, submit_input_in_tx},
    dispatcher::Dispatcher,
    events::{self, NewEvent, Scope},
    executions, idempotency,
    limits::{self, Window},
    model::{ExecutionState, HoldReason, PauseReason},
    scheduler::ClaimedJob,
    session::{Availability, CAPABILITY_PILOT_V1, Registry},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimitReachedEffect {
    Paused,
    Held {
        wake_job: Uuid,
        wake_at: DateTime<Utc>,
    },
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct WeeklyEffect {
    pub entered: Vec<Uuid>,
    pub left: Vec<Uuid>,
    pub woken: Vec<Uuid>,
    pub withdrawn: Vec<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpendOutcome {
    Continued,
    AlreadyContinued,
    NotHeld,
}

pub enum CrunchError {
    NotFound,
    IdempotencyConflict,
    Db(sqlx::Error),
}
impl fmt::Debug for CrunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("NotFound"),
            Self::IdempotencyConflict => f.write_str("IdempotencyConflict"),
            Self::Db(e) => f
                .debug_tuple("Db")
                .field(&crate::dberr::describe(e))
                .finish(),
        }
    }
}
impl fmt::Display for CrunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for CrunchError {}
impl From<sqlx::Error> for CrunchError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}
impl From<idempotency::IdempotencyConflict> for CrunchError {
    fn from(_: idempotency::IdempotencyConflict) -> Self {
        Self::IdempotencyConflict
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobOutcome {
    Done(Value),
    Retry { at: DateTime<Utc>, poll: bool },
    Cancelled(&'static str),
}

type Time = Option<DateTime<Utc>>;
/// Показание окна: загрузка, сброс, время наблюдения.
type Reading = (Option<i32>, Time, Time);

fn delta(d: Duration) -> Result<TimeDelta, sqlx::Error> {
    TimeDelta::from_std(d).map_err(|_| sqlx::Error::InvalidArgument("duration out of range".into()))
}
fn millis(at: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(at.timestamp_millis()).unwrap_or(at)
}
fn wire(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}
fn quota_key(provider: &str, window: &str) -> String {
    format!("quota:{provider}:{window}")
}
fn weekly_key(provider: &str) -> String {
    format!("weekly:{provider}")
}
fn is_quota(hold: &HoldReason, provider: &str, window: &str) -> bool {
    matches!(hold, HoldReason::QuotaExhausted { provider: p, window: w, .. } if p == provider && w == window)
}
fn is_weekly(hold: &HoldReason, provider: &str) -> bool {
    matches!(hold, HoldReason::WeeklyReserve { provider: p, .. } if p == provider)
}
fn floor(config: &LimitsConfig, provider: &str) -> Duration {
    config
        .poll_floor
        .get(provider)
        .copied()
        .unwrap_or(config.poll_min_interval)
}

/// Пауза до следующей проверки пробуждения: элементы `verify_backoff`, последний повторяется.
pub fn verify_delay(config: &LimitsConfig, attempt: i32) -> Duration {
    let attempt = usize::try_from(attempt).unwrap_or(0);
    config
        .verify_backoff
        .get(attempt)
        .or(config.verify_backoff.last())
        .copied()
        .unwrap_or(config.poll_min_interval)
}

/// Минимальный интервал опроса для запуска с номером `attempt`: пока идут шаги `verify_backoff`,
/// действует `poll_min_interval`, после них - `poll_floor` провайдера.
pub fn poll_interval(config: &LimitsConfig, provider: &str, attempt: i32) -> Duration {
    if usize::try_from(attempt).unwrap_or(0) <= config.verify_backoff.len() {
        config.poll_min_interval
    } else {
        floor(config, provider)
    }
}

async fn append(
    conn: &mut PgConnection,
    id: Uuid,
    kind: &'static str,
    execution_id: Option<Uuid>,
    payload: Value,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation { id },
            kind,
            conversation_id: Some(id),
            execution_id,
            payload,
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

/// Ставит или переносит ожидающее пробуждение. Задание, захваченное планировщиком, пропускается:
/// обработчик перечитает удержание под блокировкой очереди.
async fn schedule_wake(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    hold_key: &str,
    run_at: DateTime<Utc>,
    payload: &Value,
    reschedule: bool,
) -> Result<Uuid, sqlx::Error> {
    let pending: Option<Uuid> = sqlx::query_scalar("select id from scheduled_jobs where conversation_id=$1 and kind='wake' and state='pending' and payload->>'hold_key'=$2 for update skip locked")
        .bind(conversation_id).bind(hold_key).fetch_optional(&mut *conn).await?;
    if let Some(id) = pending {
        if reschedule {
            sqlx::query("update scheduled_jobs set run_at=$2,attempts=0,failures=0,payload=$3,updated_at=clock_timestamp() where id=$1")
                .bind(id).bind(run_at).bind(payload).execute(&mut *conn).await?;
        }
        return Ok(id);
    }
    let inserted: Option<Uuid> = sqlx::query_scalar("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) select $1,'wake',$2,$3,$4,$5 where not exists(select 1 from scheduled_jobs where conversation_id=$3 and kind='wake' and state='pending' and payload->>'hold_key'=$6) on conflict do nothing returning id")
        .bind(Uuid::now_v7()).bind(owner_id).bind(conversation_id).bind(run_at).bind(payload).bind(hold_key)
        .fetch_optional(&mut *conn).await?;
    match inserted {
        Some(id) => Ok(id),
        None => sqlx::query_scalar("select id from scheduled_jobs where conversation_id=$1 and kind='wake' and state='pending' and payload->>'hold_key'=$2")
            .bind(conversation_id).bind(hold_key).fetch_one(&mut *conn).await,
    }
}

/// Ветка «кранча» `limits::on_limit_reached_in_tx`: идущий ход и процесс не трогаются.
pub async fn hold_quota_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    run: &ExecutionRef,
    fact: &LimitReached,
    config: &LimitsConfig,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<LimitReachedEffect, sqlx::Error> {
    let window = Window::from_proto(fact.window)?;
    let provider = fact.provider.as_str();
    limits::lock_provider(conn, provider).await?;
    let mut queue: LockedQueue = sqlx::query_as("select q.* from queue_state q join conversations c on c.id=q.conversation_id join executions e on e.conversation_id=c.id where e.id=$1 and c.id=$2 and c.machine_id=$3 for update of q")
        .bind(run.execution_id).bind(run.conversation_id).bind(run.machine_id).fetch_one(&mut *conn).await?;
    let resets_at = limits::timestamp(fact.resets_at_ms)?;
    let candidate = millis(resets_at.unwrap_or(now + delta(config.unknown_reset_retry)?));
    let holds = &mut queue.hold_reasons.0;
    let (wake_at, moved) = match holds
        .iter()
        .position(|h| is_quota(h, provider, window.as_str()))
    {
        Some(i) => match &holds[i] {
            HoldReason::QuotaExhausted { wake_at, .. } if *wake_at >= candidate => {
                (*wake_at, false)
            }
            _ => {
                holds[i] = HoldReason::QuotaExhausted {
                    provider: provider.into(),
                    window: window.as_str().into(),
                    wake_at: candidate,
                };
                (candidate, true)
            }
        },
        None => {
            holds.push(HoldReason::QuotaExhausted {
                provider: provider.into(),
                window: window.as_str().into(),
                wake_at: candidate,
            });
            (candidate, true)
        }
    };
    let weekly_reset: Option<DateTime<Utc>> =
        sqlx::query_scalar("select weekly_resets_at from provider_limits where provider=$1")
            .bind(provider)
            .fetch_one(&mut *conn)
            .await?;
    let expires = weekly_reset
        .filter(|at| *at > now)
        .unwrap_or(now + TimeDelta::days(7))
        + TimeDelta::days(7);
    let input_command = if fact.input_command_id.is_empty() {
        None
    } else {
        Some(
            Uuid::parse_str(&fact.input_command_id)
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        )
    };
    let owner_stopped: bool = sqlx::query_scalar("select owner_stop_requested or (owner_stop_input_seq is not null and ($2::uuid is null or coalesce((select input_seq from inputs where command_id=$2 and conversation_id=$3),0)<=owner_stop_input_seq)) from executions where id=$1")
        .bind(run.execution_id).bind(input_command).bind(run.conversation_id).fetch_one(&mut *conn).await?;
    let pending = !owner_stopped && !queue.pause_reasons.0.contains(&PauseReason::UserStop);
    let resume_pending: bool = if pending {
        sqlx::query_scalar("update conversations set resume_pending=true,resume_pending_until=case when $3 or not resume_pending then $2 else coalesce(resume_pending_until,$2) end where id=$1 returning resume_pending")
            .bind(run.conversation_id).bind(expires).bind(moved).fetch_one(&mut *conn).await?
    } else {
        sqlx::query_scalar("select resume_pending from conversations where id=$1")
            .bind(run.conversation_id)
            .fetch_one(&mut *conn)
            .await?
    };
    let hold_key = quota_key(provider, window.as_str());
    let payload = json!({"provider":provider,"window":window,"hold_key":hold_key,"resets_at":resets_at.map(wire)});
    let wake_job = schedule_wake(
        conn,
        owner_id,
        run.conversation_id,
        &hold_key,
        wake_at + delta(config.reset_grace)?,
        &payload,
        moved,
    )
    .await?;
    let slot = queue.slot()?;
    queue
        .save(conn, &slot, Some(run.execution_id), correlation_id)
        .await?;
    limits::limit_reached_event(conn, run, provider, window, resets_at, correlation_id).await?;
    append(
        conn,
        run.conversation_id,
        "crunch_held",
        Some(run.execution_id),
        json!({"provider":provider,"window":window,"reason":"quota_exhausted","wake_at":wire(wake_at),"resume_pending":resume_pending}),
        correlation_id,
    )
    .await?;
    Ok(LimitReachedEffect::Held { wake_job, wake_at })
}

/// Удержания для нового или возобновлённого разговора; вызывающий держит строку провайдера `for share`.
pub async fn initial_holds_in_tx(
    conn: &mut PgConnection,
    provider: &str,
) -> Result<Vec<HoldReason>, sqlx::Error> {
    let row: Option<(bool, Time, Time)> = sqlx::query_as("select weekly_reserve_active,weekly_reserve_resets_at,weekly_resets_at from provider_limits where provider=$1")
        .bind(provider).fetch_optional(conn).await?;
    Ok(match row {
        Some((true, reserve, weekly)) => vec![HoldReason::WeeklyReserve {
            provider: provider.into(),
            resets_at: reserve.or(weekly),
        }],
        _ => Vec::new(),
    })
}

async fn schedule_limit_check(
    conn: &mut PgConnection,
    owner_id: Uuid,
    provider: &str,
    run_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let pending: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as("select id,run_at from scheduled_jobs where kind='limit_check' and state='pending' and payload->>'provider'=$1 for update skip locked")
        .bind(provider).fetch_optional(&mut *conn).await?;
    match pending {
        // Окно резерва сдвинулось: проверка раньше нового сброса бессмысленна.
        Some((id, at)) if at < run_at => {
            sqlx::query("update scheduled_jobs set run_at=$2,attempts=0,failures=0,updated_at=clock_timestamp() where id=$1")
                .bind(id).bind(run_at).execute(&mut *conn).await?;
        }
        Some(_) => {}
        None => {
            sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) select $1,'limit_check',$2,null,$3,jsonb_build_object('provider',$4::text) where not exists(select 1 from scheduled_jobs where kind='limit_check' and state='pending' and payload->>'provider'=$4) on conflict do nothing")
                .bind(Uuid::now_v7()).bind(owner_id).bind(run_at).bind(provider).execute(&mut *conn).await?;
        }
    }
    Ok(())
}

async fn lock_queues(
    conn: &mut PgConnection,
    ids: &[Uuid],
) -> Result<Vec<LockedQueue>, sqlx::Error> {
    sqlx::query_as("select * from queue_state where conversation_id=any($1) order by conversation_id for update")
        .bind(ids).fetch_all(conn).await
}

/// Обработчик задания `limit_rule`; вызывается только планировщиком, никогда из пакета SJ или CJ.
pub async fn apply_weekly_rule_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    provider: &str,
    config: &LimitsConfig,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<WeeklyEffect, sqlx::Error> {
    let mut effect = WeeklyEffect::default();
    if !limits::known_provider(provider) {
        return Ok(effect);
    }
    limits::lock_provider(conn, provider).await?;
    let (active, used, current_resets, reserve_resets, low_count): (bool, Option<i32>, Time, Time, i16) = sqlx::query_as("select weekly_reserve_active,weekly_used_permille,weekly_resets_at,weekly_reserve_resets_at,weekly_low_count from provider_limits where provider=$1")
        .bind(provider).fetch_one(&mut *conn).await?;
    if !active {
        return Ok(effect);
    }
    let threshold = i32::from(config.weekly_reserve_pct) * 10;
    let new_window = matches!((current_resets, reserve_resets), (Some(current), Some(entered)) if current > entered + TimeDelta::hours(1));
    let harnesses = harness::harnesses_of(provider);
    if (new_window && used.is_some_and(|u| u < threshold)) || low_count >= 2 {
        leave(
            conn,
            owner_id,
            provider,
            &harnesses,
            now,
            correlation_id,
            &mut effect,
        )
        .await?;
        return Ok(effect);
    }
    let mut resets_at = reserve_resets.or(current_resets);
    if new_window {
        // Новое окно всё ещё выше порога: резерв продолжается до его сброса.
        sqlx::query("update provider_limits set weekly_reserve_resets_at=$2,weekly_low_count=0,weekly_low_at=null where provider=$1")
            .bind(provider).bind(current_resets).execute(&mut *conn).await?;
        resets_at = current_resets;
    }
    let ids: Vec<Uuid> = sqlx::query_scalar("select c.id from conversations c join queue_state q on q.conversation_id=c.id where c.owner_id=$1 and c.harness=any($2) and not q.closed order by c.id")
        .bind(owner_id).bind(&harnesses).fetch_all(&mut *conn).await?;
    let queues = lock_queues(conn, &ids).await?;
    let overridden: Vec<Uuid> = sqlx::query_scalar(
        "select id from conversations where id=any($1) and reserve_override_until > $2 order by id",
    )
    .bind(&ids)
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    let hold = HoldReason::WeeklyReserve {
        provider: provider.into(),
        resets_at,
    };
    for mut queue in queues {
        let id = queue.conversation_id;
        if queue.closed || overridden.contains(&id) {
            continue;
        }
        let holds = &mut queue.hold_reasons.0;
        let entered = match holds.iter().position(|h| is_weekly(h, provider)) {
            Some(i) if holds[i] == hold => continue,
            Some(i) => {
                holds[i] = hold.clone();
                false
            }
            None => {
                holds.push(hold.clone());
                true
            }
        };
        let slot = queue.slot()?;
        if entered {
            effect.entered.push(id);
            // Ввод, ждущий вложений, не передаётся внутри резерва.
            if crate::delivery::control::withdraw_dispatch(conn, &slot)
                .await?
                .is_some()
            {
                effect.withdrawn.push(id);
            }
        }
        queue.save(conn, &slot, None, correlation_id).await?;
        if entered {
            let pending: bool =
                sqlx::query_scalar("select resume_pending from conversations where id=$1")
                    .bind(id)
                    .fetch_one(&mut *conn)
                    .await?;
            append(
                conn,
                id,
                "crunch_held",
                None,
                json!({"provider":provider,"window":"weekly","reason":"weekly_reserve","wake_at":resets_at.map(wire),"resume_pending":pending}),
                correlation_id,
            )
            .await?;
        }
    }
    let check_at = match resets_at {
        Some(at) => at + delta(config.reset_grace)?,
        None => now + delta(verify_delay(config, 0))?,
    };
    schedule_limit_check(conn, owner_id, provider, check_at).await?;
    if new_window {
        limits::emit_changed(conn, owner_id, provider, Window::Weekly, correlation_id).await?;
    }
    Ok(effect)
}

async fn leave(
    conn: &mut PgConnection,
    owner_id: Uuid,
    provider: &str,
    harnesses: &[&str],
    now: DateTime<Utc>,
    correlation_id: Uuid,
    effect: &mut WeeklyEffect,
) -> Result<(), sqlx::Error> {
    sqlx::query("update provider_limits set weekly_reserve_active=false,weekly_reserve_resets_at=null,weekly_low_count=0,weekly_low_at=null,updated_at=$2 where provider=$1")
        .bind(provider).bind(now).execute(&mut *conn).await?;
    let marker = json!([{"reason":"weekly_reserve","provider":provider}]);
    let ids: Vec<Uuid> = sqlx::query_scalar("select c.id from conversations c join queue_state q on q.conversation_id=c.id where c.owner_id=$1 and c.harness=any($2) and (q.hold_reasons @> $3 or c.reserve_override_until is not null) order by c.id")
        .bind(owner_id).bind(harnesses).bind(&marker).fetch_all(&mut *conn).await?;
    let queues = lock_queues(conn, &ids).await?;
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        owner_id: Uuid,
        crunch_enabled: bool,
        resume_pending: bool,
        reserve_override_until: Option<DateTime<Utc>>,
    }
    let rows: Vec<Row> = sqlx::query_as("select id,owner_id,crunch_enabled,resume_pending,reserve_override_until from conversations where id=any($1) order by id for no key update")
        .bind(&ids).fetch_all(&mut *conn).await?;
    let hold_key = weekly_key(provider);
    let payload =
        json!({"provider":provider,"window":"weekly","hold_key":hold_key,"verified":true});
    for (mut queue, row) in queues.into_iter().zip(rows) {
        let id = queue.conversation_id;
        debug_assert_eq!(id, row.id);
        let before = queue.hold_reasons.0.len();
        queue.hold_reasons.0.retain(|h| !is_weekly(h, provider));
        let had = queue.hold_reasons.0.len() != before;
        if had {
            effect.left.push(id);
        }
        let slot = queue.slot()?;
        queue.save(conn, &slot, None, correlation_id).await?;
        if row.reserve_override_until.is_some() {
            sqlx::query("update conversations set reserve_override_until=null where id=$1")
                .bind(id)
                .execute(&mut *conn)
                .await?;
            append(
                conn,
                id,
                "crunch_changed",
                None,
                json!({"enabled":row.crunch_enabled,"resume_pending":row.resume_pending,"reserve_override_until":null}),
                correlation_id,
            )
            .await?;
        }
        // Будятся только сессии в «кранче», которые остановил лимит.
        if row.crunch_enabled && row.resume_pending && !queue.closed {
            schedule_wake(conn, row.owner_id, id, &hold_key, now, &payload, false).await?;
            effect.woken.push(id);
        }
    }
    limits::emit_changed(conn, owner_id, provider, Window::Weekly, correlation_id).await
}

#[derive(Deserialize)]
struct WakePayload {
    provider: String,
    window: String,
    #[serde(default)]
    verified: bool,
}

/// Что говорят показания о сбросе окна после срока `wake_at`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ResetCheck {
    Verified,
    /// Показание снято после срока, но сброса не показывает: опрос работает, окно не сбросилось.
    NotReset,
    /// После срока показаний нет: сбой опроса не должен держать сессию вечно.
    NoReading,
}

async fn check_reset(
    conn: &mut PgConnection,
    provider: &str,
    window: Window,
    wake_at: DateTime<Utc>,
) -> Result<ResetCheck, sqlx::Error> {
    // Окна больше нет у провайдера: ограничивать им нечему, сброс подтверждён отчётом после срока.
    if window == Window::FiveHour {
        let absent: Option<DateTime<Utc>> =
            sqlx::query_scalar("select five_hour_absent_at from provider_limits where provider=$1")
                .bind(provider)
                .fetch_optional(&mut *conn)
                .await?
                .flatten();
        if absent.is_some_and(|at| at >= wake_at) {
            return Ok(ResetCheck::Verified);
        }
    }
    let row: Option<Reading> = sqlx::query_as(match window {
        Window::Weekly => "select weekly_used_permille,weekly_resets_at,weekly_observed_at from provider_limits where provider=$1",
        Window::FiveHour => "select five_hour_used_permille,five_hour_resets_at,five_hour_observed_at from provider_limits where provider=$1",
    }).bind(provider).fetch_optional(conn).await?;
    // Повтор доналимитного наблюдения не может подтвердить сброс: нужен более поздний сброс,
    // а без времени сброса - наблюдение не раньше ожидаемого.
    Ok(match row {
        Some((Some(used), resets, Some(observed))) if used < 1000 => match resets {
            Some(resets) if resets > wake_at => ResetCheck::Verified,
            None if observed >= wake_at => ResetCheck::Verified,
            _ if observed >= wake_at => ResetCheck::NotReset,
            _ => ResetCheck::NoReading,
        },
        Some((_, _, Some(observed))) if observed >= wake_at => ResetCheck::NotReset,
        _ => ResetCheck::NoReading,
    })
}

/// Пробуждение «кранча»: подтверждение по лимитам, снятие удержания и, если можно, текст продолжения.
pub async fn handle_wake_in_tx(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    config: &LimitsConfig,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<JobOutcome, sqlx::Error> {
    let (Ok(payload), Some(id)) = (
        serde_json::from_value::<WakePayload>(job.payload.clone()),
        job.conversation_id,
    ) else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    let window = match payload.window.as_str() {
        "five_hour" => Window::FiveHour,
        "weekly" => Window::Weekly,
        _ => return Ok(JobOutcome::Cancelled("invalid_payload")),
    };
    let provider = payload.provider.as_str();
    if !limits::known_provider(provider) {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    }
    // Порядок блокировок: задание, строка провайдера, очередь, разговор.
    limits::share_provider(conn, provider).await?;
    let mut by_clock = false;
    if !payload.verified {
        let holds: Option<Json<Vec<HoldReason>>> =
            sqlx::query_scalar("select hold_reasons from queue_state where conversation_id=$1")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?;
        let wake_at = holds.and_then(|h| {
            h.0.into_iter().find_map(|h| match h {
                HoldReason::QuotaExhausted {
                    provider: p,
                    window: w,
                    wake_at,
                } if p == provider && w == window.as_str() => Some(wake_at),
                _ => None,
            })
        });
        let Some(wake_at) = wake_at else {
            return Ok(JobOutcome::Cancelled("hold_gone"));
        };
        let due = wake_at + delta(config.reset_grace)?;
        if now < due {
            return Ok(JobOutcome::Retry {
                at: due,
                poll: false,
            });
        }
        match check_reset(conn, provider, window, wake_at).await? {
            ResetCheck::Verified => {}
            ResetCheck::NoReading if now >= wake_at + delta(config.unverified_wake_after)? => {
                by_clock = true;
                tracing::warn!(
                    kind = "crunch_wake_unverified",
                    provider,
                    window = window.as_str(),
                    "crunch hold released by the clock without a fresh reading"
                );
            }
            ResetCheck::NotReset | ResetCheck::NoReading => {
                return Ok(JobOutcome::Retry {
                    at: now + delta(verify_delay(config, job.attempts))?,
                    poll: true,
                });
            }
        }
    }
    let mut queue: LockedQueue =
        match sqlx::query_as("select * from queue_state where conversation_id=$1 for update")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?
        {
            Some(queue) => queue,
            None => return Ok(JobOutcome::Cancelled("conversation_not_found")),
        };
    clear_resume_in_tx(conn, id, Some(now), correlation_id).await?;
    let (crunch, mut resume_pending, resume_text): (bool, bool, Option<String>) = sqlx::query_as(
        "select crunch_enabled,resume_pending,crunch_resume_text from conversations where id=$1 for no key update",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    if payload.verified {
        // Пробуждение после сброса недели создано правилом, уже снявшим удержание `weekly_reserve`.
        if !crunch || !resume_pending {
            return Ok(JobOutcome::Cancelled("not_resumable"));
        }
    } else {
        let Some(i) = queue
            .hold_reasons
            .0
            .iter()
            .position(|h| is_quota(h, provider, window.as_str()))
        else {
            return Ok(JobOutcome::Cancelled("hold_gone"));
        };
        queue.hold_reasons.0.remove(i);
    }
    let live = matches!(
        executions::live(conn, id).await?,
        Some((
            _,
            _,
            ExecutionState::Created | ExecutionState::Starting | ExecutionState::Running
        ))
    );
    let (resume_queued, owner_queued): (bool, bool) = sqlx::query_as("select exists(select 1 from inputs where conversation_id=$1 and state in ('queued','dispatched','on_machine') and idempotency_key like 'wake:%'), exists(select 1 from inputs where conversation_id=$1 and state in ('queued','dispatched','on_machine') and idempotency_key not like 'wake:%')")
        .bind(id).fetch_one(&mut *conn).await?;
    let reserve: bool = sqlx::query_scalar(
        "select coalesce((select weekly_reserve_active from provider_limits where provider=$1),false)",
    )
    .bind(provider)
    .fetch_one(&mut *conn)
    .await?;
    let skipped = if queue.closed {
        Some("closed")
    } else if !crunch {
        Some("crunch_disabled")
    } else if !live {
        // Мёртвому процессу текст не ставится: исполнения автоматика не запускает (E-7).
        Some("no_live_execution")
    } else if resume_queued {
        resume_pending = false;
        Some("resume_already_queued")
    } else if owner_queued {
        // Продолжением становится уже стоящий ввод владельца.
        resume_pending = false;
        Some("queue_not_empty")
    } else if !queue.pause_reasons.0.is_empty() {
        Some("paused")
    } else if !resume_pending {
        Some("not_pending")
    } else if reserve {
        // Автоматика в резерв не заходит, даже для переопределённой сессии.
        Some("weekly_reserve")
    } else {
        None
    };
    if matches!(skipped, Some("closed" | "crunch_disabled" | "paused")) {
        resume_pending = false;
    }
    let slot = queue.slot()?;
    queue.save(conn, &slot, None, correlation_id).await?;
    let mut input_id = None;
    if skipped.is_none() {
        resume_pending = false;
        sqlx::query("update conversations set resume_pending=false where id=$1")
            .bind(id)
            .execute(&mut *conn)
            .await?;
        let text = resume_text
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| config.default_resume_text.clone());
        let request = SubmitInput {
            owner_id: job.owner_id,
            conversation_id: id,
            idempotency_key: format!("wake:{}", job.id),
            content: vec![ContentBlock::Text { text }],
            received_at: now,
            correlation_id,
            origin: None,
        };
        match submit_input_in_tx(conn, &request).await {
            Ok(submitted) => input_id = Some(submitted.input_id),
            Err(SubmitError::Db(e)) => return Err(e),
            Err(_) => return Err(sqlx::Error::Protocol("wake submit rejected".into())),
        }
    } else {
        sqlx::query(
            "update conversations set resume_pending=$2 where id=$1 and resume_pending<>$2",
        )
        .bind(id)
        .bind(resume_pending)
        .execute(&mut *conn)
        .await?;
    }
    let result = json!({"input_id":input_id,"skipped":skipped,"resume_pending":resume_pending,"by_clock":by_clock});
    append(
        conn,
        id,
        "crunch_woken",
        None,
        json!({"provider":provider,"window":window,"hold_key":job.payload["hold_key"],"input_id":input_id,"skipped":skipped,"resume_pending":resume_pending}),
        correlation_id,
    )
    .await?;
    Ok(JobOutcome::Done(result))
}

/// Проверка на уровне провайдера: замечает сброс недели, когда живых сессий нет.
pub async fn handle_limit_check_in_tx(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    config: &LimitsConfig,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<JobOutcome, sqlx::Error> {
    let Some(provider) = job
        .payload
        .get("provider")
        .and_then(Value::as_str)
        .filter(|p| limits::known_provider(p))
    else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    limits::lock_provider(conn, provider).await?;
    let (active, entered, observed): (bool, Time, Time) = sqlx::query_as(
        "select weekly_reserve_active,weekly_reserve_resets_at,weekly_observed_at from provider_limits where provider=$1",
    )
    .bind(provider)
    .fetch_one(&mut *conn)
    .await?;
    if !active {
        return Ok(JobOutcome::Done(json!({"reserve_active":false})));
    }
    // Резерв дождался сброса недели, а показаний после него нет: сессии не ждут починки опроса.
    if let Some(entered) = entered
        && now >= entered + delta(config.unverified_wake_after)?
        && observed.is_none_or(|at| at < entered)
    {
        let mut effect = WeeklyEffect::default();
        let harnesses = harness::harnesses_of(provider);
        leave(
            conn,
            job.owner_id,
            provider,
            &harnesses,
            now,
            correlation_id,
            &mut effect,
        )
        .await?;
        tracing::warn!(
            kind = "crunch_reserve_left_unverified",
            provider,
            "weekly reserve left by the clock without a fresh reading"
        );
        return serde_json::to_value(&effect)
            .map(JobOutcome::Done)
            .map_err(|e| sqlx::Error::Encode(Box::new(e)));
    }
    let attempt = usize::try_from(job.attempts).unwrap_or(0);
    let wait = config
        .verify_backoff
        .get(attempt)
        .copied()
        .unwrap_or_else(|| floor(config, provider));
    Ok(JobOutcome::Retry {
        at: now + delta(wait)?,
        poll: true,
    })
}

/// Отказ от «кранча»: удержание по лимиту становится паузой `limit_reached`, пробуждения отменяются.
pub(crate) async fn disable_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut queue: LockedQueue =
        sqlx::query_as("select * from queue_state where conversation_id=$1 for update")
            .bind(conversation_id)
            .fetch_one(&mut *conn)
            .await?;
    let holds = std::mem::take(&mut queue.hold_reasons.0);
    for hold in holds {
        let HoldReason::QuotaExhausted {
            provider,
            window,
            wake_at,
        } = hold
        else {
            queue.hold_reasons.0.push(hold);
            continue;
        };
        let known: Option<Option<String>> = sqlx::query_scalar("select payload->>'resets_at' from scheduled_jobs where conversation_id=$1 and kind='wake' and payload->>'hold_key'=$2 order by created_at desc limit 1")
            .bind(conversation_id).bind(quota_key(&provider, &window)).fetch_optional(&mut *conn).await?;
        let resets_at = match known {
            Some(Some(at)) => DateTime::parse_from_rfc3339(&at)
                .ok()
                .map(|at| at.with_timezone(&Utc)),
            Some(None) => None,
            None => Some(wake_at),
        };
        let pause = PauseReason::LimitReached {
            provider: Some(provider),
            window: Some(window),
            resets_at,
        };
        if !queue.pause_reasons.0.contains(&pause) {
            queue.pause_reasons.0.push(pause);
        }
    }
    let jobs: Vec<Uuid> = sqlx::query_scalar("select id from scheduled_jobs where conversation_id=$1 and kind='wake' and state='pending' for update skip locked")
        .bind(conversation_id).fetch_all(&mut *conn).await?;
    sqlx::query("update scheduled_jobs set state='cancelled',result='{\"reason\":\"crunch_disabled\"}',updated_at=clock_timestamp() where id=any($1)")
        .bind(&jobs).execute(&mut *conn).await?;
    sqlx::query("update conversations set resume_pending=false where id=$1")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?;
    let slot = queue.slot()?;
    queue.save(conn, &slot, None, correlation_id).await
}

/// Ручное продолжение одной сессии за счёт резерва; другие сессии сохраняют удержание.
pub async fn spend_reserve(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    key: &str,
    now: DateTime<Utc>,
    dispatcher: &Dispatcher,
    correlation_id: Uuid,
) -> Result<SpendOutcome, CrunchError> {
    let harness: Option<String> =
        sqlx::query_scalar("select harness from conversations where id=$1 and owner_id=$2")
            .bind(conversation_id)
            .bind(owner_id)
            .fetch_optional(pool)
            .await?;
    let Some(harness) = harness else {
        return Err(CrunchError::NotFound);
    };
    let provider = harness::provider_of(&harness);
    let request = json!({"conversation_id": conversation_id});
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "spend_reserve",
        &request,
        Some(conversation_id),
        move |conn| {
            Box::pin(spend_in_tx(
                conn,
                owner_id,
                conversation_id,
                provider,
                now,
                correlation_id,
            ))
        },
    )
    .await?;
    if outcome.value == SpendOutcome::Continued {
        dispatcher.trigger(conversation_id);
    }
    Ok(outcome.value)
}

async fn spend_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    provider: Option<&'static str>,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<SpendOutcome, CrunchError> {
    if let Some(provider) = provider {
        limits::share_provider(conn, provider).await?;
    }
    let mut queue: LockedQueue = sqlx::query_as("select q.* from queue_state q join conversations c on c.id=q.conversation_id where c.id=$1 and c.owner_id=$2 for update of q")
        .bind(id).bind(owner_id).fetch_optional(&mut *conn).await?.ok_or(CrunchError::NotFound)?;
    let (crunch, resume_pending, until): (bool, bool, Option<DateTime<Utc>>) = sqlx::query_as("select crunch_enabled,resume_pending,reserve_override_until from conversations where id=$1 for no key update")
        .bind(id).fetch_one(&mut *conn).await?;
    let Some(provider) = provider else {
        return Ok(SpendOutcome::NotHeld);
    };
    let (active, weekly_resets): (bool, Option<DateTime<Utc>>) = sqlx::query_as(
        "select weekly_reserve_active,weekly_resets_at from provider_limits where provider=$1",
    )
    .bind(provider)
    .fetch_one(&mut *conn)
    .await?;
    let overridden = until.is_some_and(|until| until > now);
    let stored = queue
        .hold_reasons
        .0
        .iter()
        .position(|h| is_weekly(h, provider));
    if stored.is_none() && !(active && !overridden) {
        return Ok(if overridden {
            SpendOutcome::AlreadyContinued
        } else {
            SpendOutcome::NotHeld
        });
    }
    if let Some(i) = stored {
        queue.hold_reasons.0.remove(i);
    }
    let until = weekly_resets
        .filter(|at| *at > now)
        .unwrap_or(now + TimeDelta::days(7));
    sqlx::query("update conversations set reserve_override_until=$2,updated_at=clock_timestamp() where id=$1")
        .bind(id).bind(until).execute(&mut *conn).await?;
    let slot = queue.slot()?;
    queue.save(conn, &slot, None, correlation_id).await?;
    append(
        conn,
        id,
        "crunch_changed",
        None,
        json!({"enabled":crunch,"resume_pending":resume_pending,"reserve_override_until":wire(until)}),
        correlation_id,
    )
    .await?;
    Ok(SpendOutcome::Continued)
}

/// Чем закончилась просьба об опросе лимитов провайдера.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollRequest {
    Sent,
    /// Недавно уже просили: повтор раньше `min_interval` не уходит.
    Throttled,
    /// Нет доступной машины провайдера с `pilot_v1` и свободным местом в очереди команд.
    NoMachine,
}

/// Просит одну доступную машину провайдера с `pilot_v1` опросить лимиты; вызывается только после commit.
pub async fn request_poll(
    pool: &PgPool,
    registry: &Registry,
    provider: &str,
    min_interval: Duration,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    Ok(
        request_poll_outcome(pool, registry, provider, min_interval, false, now).await?
            == PollRequest::Sent,
    )
}

pub async fn request_poll_outcome(
    pool: &PgPool,
    registry: &Registry,
    provider: &str,
    min_interval: Duration,
    owner_refresh: bool,
    now: DateTime<Utc>,
) -> Result<PollRequest, sqlx::Error> {
    let machines: Vec<Uuid> = sqlx::query_scalar(
        "select distinct machine_id from machine_harnesses where provider=$1 order by machine_id",
    )
    .bind(provider)
    .fetch_all(pool)
    .await?;
    let Some(sender) = machines
        .into_iter()
        .filter(|m| {
            registry.has_capability(*m, CAPABILITY_PILOT_V1)
                && registry.availability(*m) == Availability::Available
        })
        .find_map(|m| registry.command_sender(m))
    else {
        return Ok(PollRequest::NoMachine);
    };
    let Ok(permit) = sender.try_reserve() else {
        return Ok(PollRequest::NoMachine);
    };
    let claimed: Option<String> = sqlx::query_scalar("insert into limit_polls(provider,last_requested_at) values($1,$2) on conflict(provider) do update set last_requested_at=excluded.last_requested_at where limit_polls.last_requested_at <= $3 returning provider")
        .bind(provider).bind(now).bind(now - delta(min_interval)?).fetch_optional(pool).await?;
    if claimed.is_none() {
        return Ok(PollRequest::Throttled);
    }
    permit.send(ControlDown {
        msg: Some(control_down::Msg::PollLimits(PollLimits {
            provider: provider.into(),
            owner_refresh,
        })),
    });
    Ok(PollRequest::Sent)
}

pub(crate) async fn clear_resume_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
    expired_at: Option<DateTime<Utc>>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let changed: Option<(bool, Option<DateTime<Utc>>)> = sqlx::query_as("update conversations set resume_pending=false,resume_pending_until=null where id=$1 and resume_pending and ($2::timestamptz is null or resume_pending_until <= $2) returning crunch_enabled,reserve_override_until")
        .bind(id).bind(expired_at).fetch_optional(&mut *conn).await?;
    if let Some((enabled, until)) = changed {
        append(conn,id,"crunch_changed",None,json!({"enabled":enabled,"resume_pending":false,"reserve_override_until":until.map(wire)}),correlation_id).await?;
    }
    Ok(())
}

pub(crate) async fn expire_pending(pool: &PgPool, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    let ids: Vec<Uuid> = sqlx::query_scalar("select id from conversations where resume_pending and resume_pending_until <= $1 order by id").bind(now).fetch_all(pool).await?;
    for id in ids {
        crate::db::in_tx(pool, move |conn| {
            Box::pin(async move {
                let locked: Option<Uuid> = sqlx::query_scalar("select conversation_id from queue_state where conversation_id=$1 for update skip locked").bind(id).fetch_optional(&mut *conn).await?;
                if locked.is_some() {
                    clear_resume_in_tx(&mut *conn, id, Some(now), Uuid::now_v7()).await?;
                }
                Ok::<_, sqlx::Error>(())
            })
        })
        .await?;
    }
    Ok(())
}
