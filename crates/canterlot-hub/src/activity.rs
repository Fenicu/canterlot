use std::{
    collections::{HashMap, HashSet},
    fmt,
    time::Duration,
};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Row, types::Json};
use tokio::{
    sync::{broadcast, watch},
    time::Instant,
};
use uuid::Uuid;

use crate::{
    bus::{self, EventBus, Notice},
    config::{ActivityConfig, MachinesConfig},
    events::{self, NewEvent, Scope},
    model::{ExecutionState, HoldReason, HoldView, PauseReason, RunSlot},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    Idle,
    Working,
    WaitingForUser,
    Stalled,
    QuotaPaused,
    Failed,
    Stopped,
}

impl ActivityState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::WaitingForUser => "waiting_for_user",
            Self::Stalled => "stalled",
            Self::QuotaPaused => "quota_paused",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    Idle {
        #[serde(with = "optional_time")]
        last_turn_at: Option<DateTime<Utc>>,
        background_work: Option<u32>,
    },
    Working {
        phase: String,
        tool_name: Option<String>,
        summary: Option<String>,
        #[serde(with = "optional_time")]
        phase_since: Option<DateTime<Utc>>,
    },
    WaitingForUser {
        phase: String,
        question: Option<String>,
    },
    Stalled {
        #[serde(with = "time")]
        last_event_at: DateTime<Utc>,
        phase: String,
        process_alive: Option<bool>,
        cpu_busy: Option<bool>,
        link: String,
    },
    QuotaPaused {
        reason: String,
        provider: Option<String>,
        window: Option<String>,
        #[serde(with = "optional_time")]
        until: Option<DateTime<Utc>>,
    },
    Failed {
        reason: String,
    },
    Stopped {
        outcome: String,
    },
}

impl fmt::Debug for Evidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Idle { .. } => "idle",
            Self::Working { .. } => "working",
            Self::WaitingForUser { .. } => "waiting_for_user",
            Self::Stalled { .. } => "stalled",
            Self::QuotaPaused { .. } => "quota_paused",
            Self::Failed { .. } => "failed",
            Self::Stopped { .. } => "stopped",
        };
        f.debug_struct("Evidence")
            .field("kind", &kind)
            .finish_non_exhaustive()
    }
}

pub struct Activity {
    pub state: ActivityState,
    pub evidence: Evidence,
}
pub struct Thresholds {
    pub stalled_generating: Duration,
    pub stalled_tool: Duration,
}
impl From<&ActivityConfig> for Thresholds {
    fn from(c: &ActivityConfig) -> Self {
        Self {
            stalled_generating: c.stalled_generating,
            stalled_tool: c.stalled_tool,
        }
    }
}
#[derive(Clone)]
pub struct ExecutionFacts {
    pub state: ExecutionState,
    pub outcome: Option<String>,
    pub phase: String,
    pub phase_at: Option<DateTime<Utc>>,
    pub tool_name: Option<String>,
    pub summary: Option<String>,
}
#[derive(Clone, Debug)]
pub struct QueueFacts {
    pub slot: RunSlot,
    pub queued: u32,
    pub pause_reasons: Vec<PauseReason>,
    pub hold_reasons: Vec<HoldReason>,
}
#[derive(Clone, Copy, Debug)]
pub struct ProcessFacts {
    pub alive: bool,
    pub cpu_permille: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    Available,
    Degraded,
    Unavailable,
}
#[derive(Clone)]
pub struct ActivityInputs {
    pub now: DateTime<Utc>,
    pub execution: Option<ExecutionFacts>,
    pub queue: QueueFacts,
    pub open_requests: u32,
    pub last_event_at: Option<DateTime<Utc>>,
    pub last_turn_at: Option<DateTime<Utc>>,
    pub process: Option<ProcessFacts>,
    pub link: LinkState,
    pub awaiting_attachments: bool,
    pub background_work: Option<u32>,
}

pub fn compute(i: &ActivityInputs, t: &Thresholds) -> Activity {
    use ActivityState as S;
    let idle = || Activity {
        state: S::Idle,
        evidence: Evidence::Idle {
            last_turn_at: i.last_turn_at,
            background_work: i.background_work,
        },
    };
    let Some(e) = &i.execution else {
        return idle();
    };
    match e.state {
        ExecutionState::Stopped => {
            return Activity {
                state: S::Stopped,
                evidence: Evidence::Stopped {
                    outcome: e.outcome.clone().unwrap_or_else(|| "stopped".into()),
                },
            };
        }
        ExecutionState::Failed | ExecutionState::OwnerLost => {
            return Activity {
                state: S::Failed,
                evidence: Evidence::Failed {
                    reason: if e.state == ExecutionState::OwnerLost {
                        "owner_lost".into()
                    } else {
                        e.outcome.clone().unwrap_or_else(|| "failed".into())
                    },
                },
            };
        }
        _ => {}
    }
    let working = |phase: &str| Activity {
        state: S::Working,
        evidence: Evidence::Working {
            phase: phase.into(),
            tool_name: e.tool_name.clone(),
            summary: e.summary.clone(),
            phase_since: e.phase_at,
        },
    };
    if matches!(e.state, ExecutionState::Created | ExecutionState::Starting) {
        return working("starting");
    }
    if e.state == ExecutionState::Stopping {
        return working("stopping");
    }
    if !matches!(i.queue.slot, RunSlot::Free) {
        if i.open_requests > 0
            || matches!(e.phase.as_str(), "awaiting_permission" | "awaiting_answer")
        {
            return Activity {
                state: S::WaitingForUser,
                evidence: Evidence::WaitingForUser {
                    phase: e.phase.clone(),
                    question: None,
                },
            };
        }
        if i.link != LinkState::Available {
            return working("no_link");
        }
        if i.awaiting_attachments {
            return working("delivering_attachments");
        }
        let last = i.last_event_at.max(e.phase_at);
        let threshold = if matches!(e.phase.as_str(), "running_tool" | "context_maintenance") {
            t.stalled_tool
        } else {
            t.stalled_generating
        };
        if let Some(last) = last
            && (i.now - last)
                .to_std()
                .is_ok_and(|silence| silence > threshold)
        {
            return Activity {
                state: S::Stalled,
                evidence: Evidence::Stalled {
                    last_event_at: i.last_event_at.unwrap_or(last),
                    phase: e.phase.clone(),
                    process_alive: i.process.map(|p| p.alive),
                    cpu_busy: i.process.map(|p| p.cpu_permille >= 50),
                    link: "available".into(),
                },
            };
        }
        return working(&e.phase);
    }
    for hold in &i.queue.hold_reasons {
        let fields = match hold {
            HoldReason::QuotaExhausted {
                provider,
                window,
                wake_at,
            } => Some((
                "quota_exhausted",
                Some(provider.clone()),
                Some(window.clone()),
                Some(*wake_at),
            )),
            HoldReason::WeeklyReserve {
                provider,
                resets_at,
            } => Some((
                "weekly_reserve",
                Some(provider.clone()),
                Some("weekly".into()),
                *resets_at,
            )),
            _ => None,
        };
        if let Some((reason, provider, window, until)) = fields {
            return Activity {
                state: S::QuotaPaused,
                evidence: Evidence::QuotaPaused {
                    reason: reason.into(),
                    provider,
                    window,
                    until,
                },
            };
        }
    }
    for pause in &i.queue.pause_reasons {
        if let PauseReason::LimitReached {
            provider,
            window,
            resets_at,
        } = pause
        {
            return Activity {
                state: S::QuotaPaused,
                evidence: Evidence::QuotaPaused {
                    reason: "limit_reached".into(),
                    provider: provider.clone(),
                    window: window.clone(),
                    until: *resets_at,
                },
            };
        }
    }
    idle()
}

pub async fn load_inputs(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    now: DateTime<Utc>,
    stale_after: Duration,
) -> Result<ActivityInputs, sqlx::Error> {
    let q=sqlx::query("select q.*,m.availability from queue_state q join conversations c on c.id=q.conversation_id join machines m on m.id=c.machine_id where q.conversation_id=$1")
        .bind(conversation_id).fetch_one(&mut *conn).await?;
    let slot = RunSlot::from_columns(
        q.try_get("run_slot")?,
        q.try_get("run_slot_input_id")?,
        q.try_get("run_slot_attempt")?,
    )
    .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
    let queued:i64=sqlx::query_scalar("select count(*) from inputs where conversation_id=$1 and state in ('queued','dispatched','on_machine')").bind(conversation_id).fetch_one(&mut *conn).await?;
    let latest=sqlx::query("select * from executions where conversation_id=$1 order by (state in ('created','starting','running','stopping')) desc,generation desc limit 1").bind(conversation_id).fetch_optional(&mut *conn).await?;
    let mut process = None;
    let mut last_event_at = None;
    let execution = if let Some(e) = latest {
        let id: Uuid = e.try_get("id")?;
        last_event_at = e
            .try_get::<Option<DateTime<Utc>>, _>("last_event_at")?
            .or(Some(e.try_get("updated_at")?));
        let detail: Option<Value> = e.try_get("phase_detail")?;
        let metrics:Option<Value>=sqlx::query_scalar("select mm.executions from machine_metrics mm join conversations c on c.machine_id=mm.machine_id where c.id=$1 and mm.observed_at <= $2 and mm.observed_at > $2 - $3 * interval '1 second'")
            .bind(conversation_id).bind(now).bind(stale_after.as_secs_f64()).fetch_optional(&mut *conn).await?;
        if let Some(metrics) = metrics
            && let Some(found) = metrics.as_array().and_then(|items| {
                items
                    .iter()
                    .find(|m| m["execution_id"].as_str() == Some(id.to_string().as_str()))
            })
            && let (Some(alive), Some(cpu)) =
                (found["alive"].as_bool(), found["cpu_permille"].as_u64())
        {
            process = Some(ProcessFacts {
                alive,
                cpu_permille: cpu.min(1000) as u32,
            });
        }
        let state: String = e.try_get("state")?;
        Some(ExecutionFacts {
            state: state
                .parse()
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            outcome: e.try_get("outcome")?,
            phase: e
                .try_get::<Option<String>, _>("phase")?
                .unwrap_or_else(|| "unknown".into()),
            phase_at: e.try_get("phase_at")?,
            tool_name: detail
                .as_ref()
                .and_then(|d| d["tool_name"].as_str())
                .map(str::to_owned),
            summary: detail
                .as_ref()
                .and_then(|d| d["summary"].as_str())
                .map(str::to_owned),
        })
    } else {
        None
    };
    let last_turn_at =
        sqlx::query_scalar("select max(finished_at) from runs where conversation_id=$1")
            .bind(conversation_id)
            .fetch_one(&mut *conn)
            .await?;
    let availability: &str = q.try_get("availability")?;
    // Выданный ввод ждёт публикации файлов на машине, пока его вложения не готовы.
    let awaiting_attachments = match &slot {
        RunSlot::Free => false,
        RunSlot::Dispatching { input_id, .. } | RunSlot::Running { input_id } => {
            sqlx::query_scalar(
                "select exists(select 1 from input_attachments ia join inputs i on i.id = ia.input_id
                 where ia.input_id = $1 and i.state in ('dispatched','on_machine') and ia.machine_state <> 'ready')",
            )
            .bind(input_id)
            .fetch_one(&mut *conn)
            .await?
        }
    };
    Ok(ActivityInputs {
        now,
        execution,
        queue: QueueFacts {
            slot,
            queued: queued as u32,
            pause_reasons: q.try_get::<Json<Vec<PauseReason>>, _>("pause_reasons")?.0,
            hold_reasons: q.try_get::<Json<Vec<HoldReason>>, _>("hold_reasons")?.0,
        },
        open_requests: 0,
        last_event_at,
        last_turn_at,
        process,
        link: match availability {
            "available" => LinkState::Available,
            "degraded" => LinkState::Degraded,
            _ => LinkState::Unavailable,
        },
        awaiting_attachments,
        background_work: None,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refresh {
    Unchanged,
    Changed,
}

pub async fn refresh(
    pool: &PgPool,
    conversation_id: Uuid,
    now: DateTime<Utc>,
    thresholds: &Thresholds,
    correlation_id: Uuid,
) -> Result<Refresh, sqlx::Error> {
    refresh_with_metrics(
        pool,
        conversation_id,
        now,
        thresholds,
        MachinesConfig::default().metrics_stale_after,
        correlation_id,
    )
    .await
}

pub async fn refresh_with_metrics(
    pool: &PgPool,
    conversation_id: Uuid,
    now: DateTime<Utc>,
    thresholds: &Thresholds,
    stale_after: Duration,
    correlation_id: Uuid,
) -> Result<Refresh, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
        .bind(conversation_id)
        .fetch_one(&mut *tx)
        .await?;
    let (owner_id, read_seq, model, effort, permission, crunch): (Uuid, i64, String, String, String, bool) =
        sqlx::query_as("select owner_id,last_read_seq,model,effort,permission_mode,crunch_enabled from conversations where id=$1 for no key update")
            .bind(conversation_id)
            .fetch_one(&mut *tx)
            .await?;
    let inputs = load_inputs(&mut tx, conversation_id, now, stale_after).await?;
    let activity = compute(&inputs, thresholds);
    let evidence =
        serde_json::to_value(&activity.evidence).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let unread:i64=sqlx::query_scalar("select count(*) from blocks where conversation_id=$1 and kind in ('text','warning','error') and state in ('streaming','final') and created_seq>$2").bind(conversation_id).bind(read_seq).fetch_one(&mut *tx).await?;
    let last_activity:Option<DateTime<Utc>>=sqlx::query_scalar("select date_trunc('minute',greatest((select max(committed_at) from inputs where conversation_id=$1),(select max(created_at) from blocks where conversation_id=$1)))").bind(conversation_id).fetch_one(&mut *tx).await?;
    let old = sqlx::query("select * from conversation_derived where conversation_id=$1 for update")
        .bind(conversation_id)
        .fetch_optional(&mut *tx)
        .await?;
    let pauses = json!(inputs.queue.pause_reasons);
    let mut holds: Vec<HoldView> = inputs
        .queue
        .hold_reasons
        .iter()
        .cloned()
        .map(HoldView::from)
        .collect();
    for (input_id, reason) in
        crate::conversations::attachments_pending(&mut tx, conversation_id).await?
    {
        holds.push(HoldView::AttachmentsPending { input_id, reason });
    }
    let holds = json!(holds);
    let mut since = now;
    if let Some(old) = old {
        let old_state: String = old.try_get("activity_state")?;
        if old_state == activity.state.as_str() {
            since = old.try_get("activity_since")?;
        }
        if old_state == activity.state.as_str()
            && old.try_get::<Value, _>("activity_evidence")? == evidence
            && old.try_get::<i32, _>("unread_count")? == unread as i32
            && old.try_get::<i32, _>("queue_queued")? == inputs.queue.queued as i32
            && old.try_get::<Value, _>("pause_reasons")? == pauses
            && old.try_get::<Value, _>("hold_reasons")? == holds
            && old.try_get::<Option<DateTime<Utc>>, _>("last_activity_at")? == last_activity
            && old.try_get::<String, _>("effective_model")? == model
            && old.try_get::<String, _>("effective_effort")? == effort
            && old.try_get::<String, _>("effective_permission_mode")? == permission
            && old.try_get::<bool, _>("crunch_enabled")? == crunch
        {
            sqlx::query("update conversation_derived d set updated_at=clock_timestamp() where d.conversation_id=$1 and d.updated_at < (select max(e.committed_at) from events e where e.scope='conversation:'||d.conversation_id::text and e.kind not in ('block_delta','derived_changed','scheduled_changed'))")
                .bind(conversation_id).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(Refresh::Unchanged);
        }
    }
    sqlx::query("update conversations set last_activity_at=$2 where id=$1")
        .bind(conversation_id)
        .bind(last_activity)
        .execute(&mut *tx)
        .await?;
    sqlx::query("insert into conversation_derived(conversation_id,activity_state,activity_evidence,activity_since,unread_count,queue_queued,pause_reasons,hold_reasons,last_activity_at,effective_model,effective_effort,effective_permission_mode,crunch_enabled) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) on conflict(conversation_id) do update set activity_state=excluded.activity_state,activity_evidence=excluded.activity_evidence,activity_since=excluded.activity_since,unread_count=excluded.unread_count,queue_queued=excluded.queue_queued,pause_reasons=excluded.pause_reasons,hold_reasons=excluded.hold_reasons,last_activity_at=excluded.last_activity_at,effective_model=excluded.effective_model,effective_effort=excluded.effective_effort,effective_permission_mode=excluded.effective_permission_mode,crunch_enabled=excluded.crunch_enabled,updated_at=clock_timestamp()")
        .bind(conversation_id).bind(activity.state.as_str()).bind(&evidence).bind(since).bind(unread as i32).bind(inputs.queue.queued as i32).bind(&pauses).bind(&holds).bind(last_activity).bind(&model).bind(&effort).bind(&permission).bind(crunch).execute(&mut *tx).await?;
    let payload = json!({"conversation_id":conversation_id,"activity":{"state":activity.state,"evidence":evidence,"since":since.to_rfc3339_opts(SecondsFormat::Millis,true)},"unread_count":unread,"queue":{"queued":inputs.queue.queued,"pause_reasons":pauses,"hold_reasons":holds},"last_activity_at":last_activity.map(|at|at.to_rfc3339_opts(SecondsFormat::Millis,true)),"settings":{"model":model,"effort":effort,"permission_mode":permission},"crunch":{"enabled":crunch}});
    for scope in [
        Scope::Conversation {
            id: conversation_id,
        },
        Scope::Conversations { owner_id },
    ] {
        events::append(
            &mut tx,
            NewEvent {
                scope,
                kind: "derived_changed",
                conversation_id: Some(conversation_id),
                execution_id: None,
                payload: payload.clone(),
                correlation_id,
            },
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Refresh::Changed)
}

async fn sweep(pool: &PgPool) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar("select c.id from conversations c left join conversation_derived d on d.conversation_id=c.id where d.conversation_id is null or d.activity_state in ('working','stalled','waiting_for_user') or exists(select 1 from executions e where e.conversation_id=c.id and e.state in ('created','starting','running','stopping')) or d.updated_at < (select max(e.committed_at) from events e where e.scope='conversation:'||c.id::text and e.kind not in ('block_delta','derived_changed','scheduled_changed')) order by c.id").fetch_all(pool).await
}

pub async fn run(
    pool: PgPool,
    bus: EventBus,
    config: ActivityConfig,
    machines: MachinesConfig,
    shutdown: watch::Receiver<bool>,
) {
    crate::singleton::supervise(&pool, "activity", shutdown, || async {
        let mut notices = bus.subscribe();
        let mut tick = tokio::time::interval(config.tick.max(Duration::from_millis(1)));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut pending: HashMap<Uuid, (Instant, Option<Notice>)> = HashMap::new();
        let mut sweep_at = None;
        let thresholds = Thresholds::from(&config);
        loop {
            let next = pending
                .values()
                .map(|(at, _)| *at)
                .chain(sweep_at)
                .min()
                .unwrap_or_else(|| Instant::now() + config.tick.max(Duration::from_millis(1)));
            tokio::select! {
                _=tick.tick()=>sweep_at=Some(Instant::now()),
                notice=notices.recv()=>match notice {
                    Ok(n) if n.scope=="*"=>sweep_at=Some(Instant::now()),
                    Ok(n) if !matches!(n.kind.as_str(),"block_delta"|"derived_changed"|"scheduled_changed")=>{
                        if let Some(id)=n.scope.strip_prefix("conversation:").and_then(|s|Uuid::parse_str(s).ok()) {
                            pending.insert(id,(Instant::now()+config.debounce,Some((*n).clone())));
                        }
                        else if n.scope.ends_with(":machines") {sweep_at.get_or_insert(Instant::now()+config.debounce);}
                    },
                    Err(broadcast::error::RecvError::Lagged(_))=>sweep_at=Some(Instant::now()),
                    Err(broadcast::error::RecvError::Closed)=>break,
                    _=>{},
                },
                _=tokio::time::sleep_until(next)=>{},
            }
            if sweep_at.is_some_and(|at| at <= Instant::now()) {
                sweep_at = None;
                match sweep(&pool).await {
                    Ok(ids) => {
                        for id in ids {
                            pending.insert(id, (Instant::now(), None));
                        }
                    }
                    Err(e) => log_error(&e),
                }
            }
            let ready: Vec<_> = pending
                .iter()
                .filter(|(_, (at, _))| *at <= Instant::now())
                .map(|(id, _)| *id)
                .collect();
            if ready.is_empty() {
                continue;
            }
            let owned: HashSet<Uuid> = match sqlx::query_scalar::<_, Uuid>(
                "select id from conversations where id=any($1)",
            )
            .bind(&ready)
            .fetch_all(&pool)
            .await
            {
                Ok(ids) => ids.into_iter().collect(),
                Err(e) => {
                    log_error(&e);
                    continue;
                }
            };
            for id in ready {
                let notice = pending.remove(&id).and_then(|(_, notice)| notice);
                if !owned.contains(&id) {
                    continue;
                }
                if let Some(notice) = notice {
                    match bus::wait_visible(
                        &pool,
                        &notice,
                        crate::config::WsConfig::default().poll_interval,
                    )
                    .await
                    {
                        Ok(true) => {}
                        Ok(false) => continue,
                        Err(e) => {
                            log_error(&e);
                            continue;
                        }
                    }
                }
                if let Err(e) = refresh_with_metrics(
                    &pool,
                    id,
                    Utc::now(),
                    &thresholds,
                    machines.metrics_stale_after,
                    Uuid::now_v7(),
                )
                .await
                {
                    log_error(&e);
                }
            }
        }
    }).await;
}

fn log_error(error: &sqlx::Error) {
    tracing::error!(kind="activity_db_error",error=%crate::dberr::describe(error),"activity refresh failed");
}

pub(crate) mod time {
    use super::*;
    pub fn serialize<S: serde::Serializer>(value: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        value
            .to_rfc3339_opts(SecondsFormat::Millis, true)
            .serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        DateTime::<Utc>::deserialize(d)
    }
}
mod optional_time {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &Option<DateTime<Utc>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .map(|v| v.to_rfc3339_opts(SecondsFormat::Millis, true))
            .serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Option<DateTime<Utc>>, D::Error> {
        Option::<DateTime<Utc>>::deserialize(d)
    }
}
