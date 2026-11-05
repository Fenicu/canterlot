pub use crate::crunch::LimitReachedEffect;
use crate::{
    blocks::ExecutionRef,
    config::LimitsConfig,
    delivery::LockedQueue,
    events::{self, NewEvent, Scope},
    model::PauseReason,
    session::{CAPABILITY_PILOT_V1, Registry},
};
use canterlot_proto::{
    limits::MAX_MESSAGE_BYTES,
    worker::v1::{
        ControlDown, LimitDataConfidence, LimitObservation, LimitReached, LimitWindow,
        LimitsSnapshot, control_down,
    },
};
use chrono::{DateTime, NaiveDate, Utc};
use prost::Message;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Window {
    FiveHour,
    Weekly,
    Monthly,
}
impl Window {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FiveHour => "five_hour",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }
    pub fn from_proto(value: i32) -> Result<Self, sqlx::Error> {
        match LimitWindow::try_from(value) {
            Ok(LimitWindow::FiveHour) => Ok(Self::FiveHour),
            Ok(LimitWindow::Weekly) => Ok(Self::Weekly),
            Ok(LimitWindow::Monthly) => Ok(Self::Monthly),
            _ => Err(sqlx::Error::InvalidArgument("invalid limit window".into())),
        }
    }
    fn proto(self) -> i32 {
        match self {
            Self::FiveHour => LimitWindow::FiveHour as i32,
            Self::Weekly => LimitWindow::Weekly as i32,
            Self::Monthly => LimitWindow::Monthly as i32,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationSource {
    Session,
    Poll,
}
/// Точность показаний провайдера. `estimated` приходит от CodexBar, когда он считает
/// расход по локальным журналам, а не по данным провайдера.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DataConfidence {
    Exact,
    Estimated,
}
impl DataConfidence {
    pub fn from_proto(value: i32) -> Self {
        match LimitDataConfidence::try_from(value) {
            Ok(LimitDataConfidence::Estimated) => Self::Estimated,
            _ => Self::Exact,
        }
    }
    pub fn from_stored(value: &str) -> Self {
        if value == "estimated" {
            Self::Estimated
        } else {
            Self::Exact
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Estimated => "estimated",
        }
    }
    fn proto(self) -> i32 {
        match self {
            Self::Exact => LimitDataConfidence::Exact as i32,
            Self::Estimated => LimitDataConfidence::Estimated as i32,
        }
    }
}
pub struct Observation {
    pub provider: String,
    pub window: Window,
    pub used_permille: u16,
    pub resets_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
    pub source: ObservationSource,
    pub data_confidence: DataConfidence,
    pub machine_id: Uuid,
    pub execution_id: Option<Uuid>,
}
pub struct Recorded {
    pub snapshot_stored: bool,
    pub provider_changed: bool,
    /// Равное показание поверх давно не обновлявшегося: клиенту нужна новая отметка времени.
    pub refreshed: bool,
    pub stale: bool,
}
/// Равные показания чаще этого срока событие не порождают, иначе опрос каждые несколько минут забил бы ленту.
const REFRESH_EVENT_GAP: chrono::Duration = chrono::Duration::seconds(120);
#[derive(Debug, Serialize)]
pub struct ProviderLimits {
    pub provider: String,
    pub weekly: Option<WindowState>,
    pub five_hour: Option<WindowState>,
    pub monthly: Option<WindowState>,
    pub weekly_reserve_active: bool,
    pub weekly_reserve_resets_at: Option<DateTime<Utc>>,
    /// У провайдера нет пятичасового окна: клиенты прячут счётчик, история остаётся.
    pub five_hour_absent: bool,
    pub five_hour_absent_at: Option<DateTime<Utc>>,
    /// Точность последнего показания; старые записи без поля считаются точными.
    pub data_confidence: DataConfidence,
}
#[derive(Debug, Serialize)]
pub struct WindowState {
    pub used_permille: u16,
    pub resets_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
}

pub(crate) async fn lock_provider(
    conn: &mut PgConnection,
    provider: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("insert into provider_limits(provider) values($1) on conflict do nothing")
        .bind(provider)
        .execute(&mut *conn)
        .await?;
    sqlx::query("select provider from provider_limits where provider=$1 for update")
        .bind(provider)
        .fetch_one(conn)
        .await?;
    Ok(())
}
/// Разделяемая блокировка строки провайдера: создание и возобновление сессии против правила резерва.
pub(crate) async fn share_provider(
    conn: &mut PgConnection,
    provider: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("insert into provider_limits(provider) values($1) on conflict do nothing")
        .bind(provider)
        .execute(&mut *conn)
        .await?;
    sqlx::query("select provider from provider_limits where provider=$1 for share")
        .bind(provider)
        .fetch_one(conn)
        .await?;
    Ok(())
}
pub async fn record_observation_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    o: &Observation,
    now: DateTime<Utc>,
    config: &LimitsConfig,
    correlation_id: Uuid,
) -> Result<Recorded, sqlx::Error> {
    let result = record_in_tx(conn, owner_id, o, now, config, true, correlation_id).await?;
    if result.provider_changed || result.refreshed {
        emit_changed(conn, owner_id, &o.provider, o.window, correlation_id).await?;
    }
    Ok(result)
}
type StoredReading = (
    Option<i32>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    String,
);

pub(crate) fn known_provider(provider: &str) -> bool {
    !canterlot_proto::harness::harnesses_of(provider).is_empty()
}
pub(crate) async fn record_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    o: &Observation,
    now: DateTime<Utc>,
    config: &LimitsConfig,
    mapped: bool,
    correlation_id: Uuid,
) -> Result<Recorded, sqlx::Error> {
    if o.used_permille > 1000 || !known_provider(&o.provider) {
        return Err(sqlx::Error::InvalidArgument(
            "invalid limit observation".into(),
        ));
    }
    lock_provider(conn, &o.provider).await?;
    // PostgreSQL хранит микросекунды: сравнение идёт с тем же округлением.
    let observed_at = DateTime::from_timestamp_micros(o.observed_at.timestamp_micros())
        .ok_or_else(|| sqlx::Error::InvalidArgument("invalid observation time".into()))?;
    let window = o.window.as_str();
    let previous:Option<(i32,Option<DateTime<Utc>>,DateTime<Utc>)>=sqlx::query_as("select used_permille,resets_at,observed_at from limit_snapshots where provider=$1 and window_kind=$2 order by id desc limit 1").bind(&o.provider).bind(window).fetch_optional(&mut *conn).await?;
    let interval = chrono::Duration::from_std(config.snapshot_min_interval)
        .map_err(|_| sqlx::Error::InvalidArgument("snapshot interval overflow".into()))?;
    let store = previous.is_none_or(|(used, reset, seen)| {
        used != i32::from(o.used_permille) || reset != o.resets_at || observed_at - seen >= interval
    });
    if store {
        sqlx::query("insert into limit_snapshots(provider,window_kind,used_permille,resets_at,observed_at,received_at,source,machine_id,execution_id) values($1,$2,$3,$4,$5,$6,$7,$8,$9)")
 .bind(&o.provider).bind(window).bind(i32::from(o.used_permille)).bind(o.resets_at).bind(observed_at).bind(now).bind(match o.source{ObservationSource::Session=>"session",ObservationSource::Poll=>"poll"}).bind(o.machine_id).bind(o.execution_id).execute(&mut *conn).await?;
    }
    let current: Option<StoredReading> = sqlx::query_as(match o.window {
        Window::Weekly => "select weekly_used_permille,weekly_resets_at,weekly_observed_at,data_confidence from provider_limits where provider=$1",
        Window::FiveHour => "select five_hour_used_permille,five_hour_resets_at,five_hour_observed_at,data_confidence from provider_limits where provider=$1",
        Window::Monthly => "select monthly_used_permille,monthly_resets_at,monthly_observed_at,data_confidence from provider_limits where provider=$1",
    }).bind(&o.provider).fetch_optional(&mut *conn).await?;
    let (used, reset, seen, stored_confidence) =
        current.unwrap_or((None, None, None, DataConfidence::Exact.as_str().into()));
    let stored_confidence = DataConfidence::from_stored(&stored_confidence);
    let absent_at: Option<DateTime<Utc>> = if o.window == Window::FiveHour {
        sqlx::query_scalar("select five_hour_absent_at from provider_limits where provider=$1")
            .bind(&o.provider)
            .fetch_one(&mut *conn)
            .await?
    } else {
        None
    };
    let newer = mapped
        && seen.is_none_or(|seen| seen < observed_at)
        && absent_at.is_none_or(|absent| absent < observed_at);
    if newer {
        sqlx::query(match o.window {
            Window::Weekly => "update provider_limits set weekly_used_permille=$2,weekly_resets_at=$3,weekly_observed_at=$4,data_confidence=$5,updated_at=$6 where provider=$1",
            Window::FiveHour => "update provider_limits set five_hour_used_permille=$2,five_hour_resets_at=$3,five_hour_observed_at=$4,five_hour_absent_at=null,data_confidence=$5,updated_at=$6 where provider=$1",
            Window::Monthly => "update provider_limits set monthly_used_permille=$2,monthly_resets_at=$3,monthly_observed_at=$4,data_confidence=$5,updated_at=$6 where provider=$1",
        }).bind(&o.provider).bind(i32::from(o.used_permille)).bind(o.resets_at).bind(observed_at).bind(o.data_confidence.as_str()).bind(now).execute(&mut *conn).await?;
    }
    if newer && o.window == Window::Weekly {
        weekly_stage(conn, owner_id, o, observed_at, now, config).await?;
    }
    // Свежее показание сперва попадает в `provider_limits`, поэтому условие будильника
    // оценивается уже по новой строке. Строка провайдера заперта выше: порядок блокировок
    // «провайдер → будильники» соблюдён.
    if newer {
        crate::session_tools::alarms::evaluate_limit_in_tx(
            conn,
            &o.provider,
            o.window,
            now,
            correlation_id,
        )
        .await?;
    }
    // Возвращение окна после отметки об отсутствии и смена точности клиентам тоже нужны.
    let changed = used != Some(i32::from(o.used_permille))
        || reset != o.resets_at
        || absent_at.is_some()
        || stored_confidence != o.data_confidence;
    Ok(Recorded {
        snapshot_stored: store,
        provider_changed: newer && changed,
        refreshed: newer
            && !changed
            && seen.is_some_and(|seen| observed_at - seen >= REFRESH_EVENT_GAP),
        stale: !newer,
    })
}
/// Стадия наблюдения недельного резерва; сами удержания ставит задание `limit_rule`.
async fn weekly_stage(
    conn: &mut PgConnection,
    owner_id: Uuid,
    o: &Observation,
    observed_at: DateTime<Utc>,
    now: DateTime<Utc>,
    config: &LimitsConfig,
) -> Result<(), sqlx::Error> {
    let settings = crate::limit_settings::effective(conn, owner_id, config).await?;
    let Some(threshold) = settings.reserve_threshold_permille() else {
        // Резерв выключен: действующий флаг снимет задание правила, новых не ставим.
        return Ok(());
    };
    let (active, mut reserve_resets, mut low_count, mut low_at): (
        bool,
        Option<DateTime<Utc>>,
        i16,
        Option<DateTime<Utc>>,
    ) = sqlx::query_as("select weekly_reserve_active,weekly_reserve_resets_at,weekly_low_count,weekly_low_at from provider_limits where provider=$1")
        .bind(&o.provider)
        .fetch_one(&mut *conn)
        .await?;
    let exit = threshold - i32::from(config.reserve_exit_hysteresis_permille);
    let used = i32::from(o.used_permille);
    let interval = chrono::Duration::from_std(config.snapshot_min_interval)
        .map_err(|_| sqlx::Error::InvalidArgument("snapshot interval overflow".into()))?;
    let entered = !active && used >= threshold;
    if entered {
        reserve_resets = o.resets_at;
        (low_count, low_at) = (0, None);
    } else if active {
        reserve_resets = reserve_resets.or(o.resets_at);
        if used > exit {
            (low_count, low_at) = (0, None);
        } else if low_count == 0 {
            (low_count, low_at) = (1, Some(observed_at));
        } else if low_at.is_none_or(|at| observed_at - at >= interval) {
            (low_count, low_at) = (low_count.saturating_add(1), Some(observed_at));
        }
    } else {
        (low_count, low_at) = (0, None);
    }
    let active = active || entered;
    sqlx::query("update provider_limits set weekly_reserve_active=$2,weekly_reserve_resets_at=$3,weekly_low_count=$4,weekly_low_at=$5 where provider=$1 and (weekly_reserve_active,weekly_reserve_resets_at,weekly_low_count,weekly_low_at) is distinct from ($2,$3,$4,$5)")
        .bind(&o.provider).bind(active).bind(if active { reserve_resets } else { None }).bind(low_count).bind(low_at)
        .execute(&mut *conn).await?;
    if !active {
        return Ok(());
    }
    // Вставляет только держатель строки провайдера, поэтому проверка без ожидания чужих транзакций.
    sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) select $1,'limit_rule',m.owner_id,null,$3,jsonb_build_object('provider',$4::text) from machines m where m.id=$2 and not exists(select 1 from scheduled_jobs where kind='limit_rule' and state='pending' and payload->>'provider'=$4) on conflict do nothing")
        .bind(Uuid::now_v7()).bind(o.machine_id).bind(now).bind(&o.provider)
        .execute(&mut *conn).await?;
    Ok(())
}
pub(crate) async fn emit_changed(
    conn: &mut PgConnection,
    owner_id: Uuid,
    provider: &str,
    window: Window,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let states = current(conn).await?;
    if let Some(p) = states.into_iter().find(|p| p.provider == provider) {
        let state = match window {
            Window::Weekly => p.weekly,
            Window::FiveHour => p.five_hour,
            Window::Monthly => p.monthly,
        };
        let wire = |at: DateTime<Utc>| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let reserve = p.weekly_reserve_resets_at.map(wire);
        // Отсутствующее окно уходит клиентам событием без значения: `used_permille` и `resets_at` равны null.
        let payload = match (state, p.five_hour_absent_at) {
            (Some(s), _) => Some(
                json!({"provider":provider,"window":window,"used_permille":s.used_permille,"resets_at":s.resets_at.map(wire),"observed_at":wire(s.observed_at),"weekly_reserve_active":p.weekly_reserve_active,"weekly_reserve_resets_at":reserve,"data_confidence":p.data_confidence.as_str()}),
            ),
            (None, Some(absent)) if window == Window::FiveHour => Some(
                json!({"provider":provider,"window":window,"used_permille":null,"resets_at":null,"observed_at":wire(absent),"weekly_reserve_active":p.weekly_reserve_active,"weekly_reserve_resets_at":reserve,"data_confidence":p.data_confidence.as_str()}),
            ),
            _ => None,
        };
        if let Some(payload) = payload {
            events::append(
                conn,
                NewEvent {
                    scope: Scope::Conversations { owner_id },
                    kind: "limits_changed",
                    conversation_id: None,
                    execution_id: None,
                    payload,
                    correlation_id,
                },
            )
            .await?;
        }
    }
    Ok(())
}
/// Записывает, что у провайдера нет пятичасового окна. Возвращает `true`, если клиентам нужно событие.
/// История снимков не трогается; показание окна, наблюдённое позже, снимает отметку.
pub async fn record_absence_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    provider: &str,
    window: Window,
    observed_at: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    if window != Window::FiveHour || !known_provider(provider) {
        return Err(sqlx::Error::InvalidArgument("invalid limit absence".into()));
    }
    let observed_at = DateTime::from_timestamp_micros(observed_at.timestamp_micros())
        .ok_or_else(|| sqlx::Error::InvalidArgument("invalid observation time".into()))?;
    lock_provider(conn, provider).await?;
    let (seen, absent): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as(
        "select five_hour_observed_at,five_hour_absent_at from provider_limits where provider=$1",
    )
    .bind(provider)
    .fetch_one(&mut *conn)
    .await?;
    // Более позднее показание окна сильнее устаревшего отчёта об отсутствии.
    if seen.is_some_and(|seen| seen >= observed_at) || absent.is_some_and(|at| at >= observed_at) {
        return Ok(false);
    }
    sqlx::query("update provider_limits set five_hour_used_permille=null,five_hour_resets_at=null,five_hour_observed_at=null,five_hour_absent_at=$2,updated_at=clock_timestamp() where provider=$1")
        .bind(provider)
        .bind(observed_at)
        .execute(&mut *conn)
        .await?;
    let first = absent.is_none();
    if first {
        emit_changed(conn, owner_id, provider, window, correlation_id).await?;
    }
    // Исчезнувшее окно — тоже признак сброса: условие будильника проверяется здесь.
    crate::session_tools::alarms::evaluate_limit_in_tx(
        conn,
        provider,
        window,
        observed_at,
        correlation_id,
    )
    .await?;
    Ok(first)
}
pub async fn on_limit_reached_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    run: &ExecutionRef,
    fact: &LimitReached,
    config: &LimitsConfig,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<LimitReachedEffect, sqlx::Error> {
    let window = Window::from_proto(fact.window)?;
    if !known_provider(&fact.provider) {
        return Err(sqlx::Error::InvalidArgument(
            "unknown limit provider".into(),
        ));
    }
    // Строка провайдера раньше очереди: общий порядок блокировок хаба.
    lock_provider(conn, &fact.provider).await?;
    let mut queue:LockedQueue=sqlx::query_as("select q.* from queue_state q join conversations c on c.id=q.conversation_id join executions e on e.conversation_id=c.id where e.id=$1 and c.id=$2 and c.machine_id=$3 for update of q").bind(run.execution_id).bind(run.conversation_id).bind(run.machine_id).fetch_one(&mut *conn).await?;
    let crunch: bool = sqlx::query_scalar("select crunch_enabled from conversations where id=$1")
        .bind(run.conversation_id)
        .fetch_one(&mut *conn)
        .await?;
    if crunch {
        return crate::crunch::hold_quota_in_tx(
            conn,
            owner_id,
            run,
            fact,
            config,
            now,
            correlation_id,
        )
        .await;
    }
    // Пока пауза по этому окну стоит, повторный факт ничего не добавляет.
    if queue.pause_reasons.0.iter().any(|reason| {
        matches!(reason, PauseReason::LimitReached { provider, window: stored, .. }
            if provider.as_deref() == Some(fact.provider.as_str()) && stored.as_deref() == Some(window.as_str()))
    }) {
        return Ok(LimitReachedEffect::Paused);
    }
    let resets_at = timestamp(fact.resets_at_ms)?;
    queue.pause_reasons.0.push(PauseReason::LimitReached {
        provider: Some(fact.provider.clone()),
        window: Some(window.as_str().into()),
        resets_at,
    });
    let slot = queue.slot()?;
    queue
        .save(conn, &slot, Some(run.execution_id), correlation_id)
        .await?;
    limit_reached_event(conn, run, &fact.provider, window, resets_at, correlation_id).await?;
    Ok(LimitReachedEffect::Paused)
}
pub(crate) async fn limit_reached_event(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    provider: &str,
    window: Window,
    resets_at: Option<DateTime<Utc>>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(conn,NewEvent{scope:Scope::Conversation{id:run.conversation_id},kind:"limit_reached",conversation_id:Some(run.conversation_id),execution_id:Some(run.execution_id),payload:json!({"provider":provider,"window":window,"resets_at":resets_at.map(|at|at.to_rfc3339_opts(chrono::SecondsFormat::Millis,true))}),correlation_id}).await?;
    Ok(())
}
pub(crate) fn timestamp(ms: u64) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    if ms == 0 {
        return Ok(None);
    }
    i64::try_from(ms)
        .ok()
        .and_then(DateTime::from_timestamp_millis)
        .map(Some)
        .ok_or_else(|| sqlx::Error::InvalidArgument("invalid limit timestamp".into()))
}
pub async fn current(conn: &mut PgConnection) -> Result<Vec<ProviderLimits>, sqlx::Error> {
    let rows = sqlx::query("select * from provider_limits order by provider")
        .fetch_all(conn)
        .await?;
    rows.into_iter()
        .map(|r| {
            let state = |prefix: &str| -> Result<Option<WindowState>, sqlx::Error> {
                let used: Option<i32> = r.try_get(format!("{prefix}_used_permille").as_str())?;
                let at: Option<DateTime<Utc>> =
                    r.try_get(format!("{prefix}_observed_at").as_str())?;
                match (used, at) {
                    (Some(used), Some(observed_at)) => Ok(Some(WindowState {
                        used_permille: u16::try_from(used)
                            .map_err(|_| sqlx::Error::Protocol("invalid stored limit".into()))?,
                        resets_at: r.try_get(format!("{prefix}_resets_at").as_str())?,
                        observed_at,
                    })),
                    _ => Ok(None),
                }
            };
            Ok(ProviderLimits {
                provider: r.try_get("provider")?,
                weekly: state("weekly")?,
                five_hour: state("five_hour")?,
                monthly: state("monthly")?,
                weekly_reserve_active: r.try_get("weekly_reserve_active")?,
                weekly_reserve_resets_at: r.try_get("weekly_reserve_resets_at")?,
                five_hour_absent: r
                    .try_get::<Option<DateTime<Utc>>, _>("five_hour_absent_at")?
                    .is_some(),
                five_hour_absent_at: r.try_get("five_hour_absent_at")?,
                data_confidence: DataConfidence::from_stored(
                    r.try_get::<Option<String>, _>("data_confidence")?
                        .as_deref()
                        .unwrap_or("exact"),
                ),
            })
        })
        .collect()
}
async fn stored_provider(
    pool: &PgPool,
    provider: &str,
) -> Result<Option<ProviderLimits>, sqlx::Error> {
    Ok(current(&mut *pool.acquire().await?)
        .await?
        .into_iter()
        .find(|p| p.provider == provider))
}
// Время наблюдения уходит в часах машины-получателя: часы хаба минус смещение сеанса.
fn snapshot_message(
    provider: &str,
    stored: Option<&ProviderLimits>,
    offset: Option<chrono::Duration>,
) -> ControlDown {
    let mut observations = Vec::new();
    if let Some(p) = stored {
        for (window, state) in [
            (Window::FiveHour, &p.five_hour),
            (Window::Weekly, &p.weekly),
            (Window::Monthly, &p.monthly),
        ] {
            if let Some(s) = state {
                let seen = s.observed_at - offset.unwrap_or_default();
                observations.push(LimitObservation {
                    provider: provider.into(),
                    window: window.proto(),
                    used_permille: u32::from(s.used_permille),
                    resets_at_ms: s
                        .resets_at
                        .map_or(0, |at| at.timestamp_millis().max(0) as u64),
                    observed_machine_ms: seen.timestamp_millis().max(0) as u64,
                    data_confidence: p.data_confidence.proto(),
                });
            }
        }
    }
    ControlDown {
        msg: Some(control_down::Msg::LimitsSnapshot(LimitsSnapshot {
            provider: provider.into(),
            current: observations,
            weekly_reserve_active: stored.is_some_and(|p| p.weekly_reserve_active),
        })),
    }
}
pub async fn push_limits_snapshot(
    pool: &PgPool,
    registry: &Registry,
    provider: &str,
) -> Result<usize, sqlx::Error> {
    let machines: Vec<Uuid> = sqlx::query_scalar(
        "select distinct machine_id from machine_harnesses where provider=$1 order by machine_id",
    )
    .bind(provider)
    .fetch_all(pool)
    .await?;
    let stored = stored_provider(pool, provider).await?;
    let mut sent = 0;
    for machine in machines {
        if registry.has_capability(machine, CAPABILITY_PILOT_V1)
            && let Some(sender) = registry.command_sender(machine)
        {
            let message =
                snapshot_message(provider, stored.as_ref(), registry.clock_offset(machine));
            if message.encoded_len() > MAX_MESSAGE_BYTES {
                return Err(sqlx::Error::InvalidArgument("limits snapshot bound".into()));
            }
            match sender.try_send(message) {
                Ok(()) => sent += 1,
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    if let Some(info) = registry.session_info(machine) {
                        registry.revoke(machine, info.connection_id);
                    }
                }
                Err(_) => {}
            }
        }
    }
    Ok(sent)
}
pub(crate) async fn push_machine_snapshot(
    pool: &PgPool,
    registry: &Registry,
    machine: Uuid,
) -> Result<(), sqlx::Error> {
    if !registry.has_capability(machine, CAPABILITY_PILOT_V1) {
        return Ok(());
    }
    let providers: Vec<String> = sqlx::query_scalar(
        "select distinct provider from machine_harnesses where machine_id=$1 order by provider",
    )
    .bind(machine)
    .fetch_all(pool)
    .await?;
    for provider in providers {
        let stored = stored_provider(pool, &provider).await?;
        let message = snapshot_message(&provider, stored.as_ref(), registry.clock_offset(machine));
        if let Some(sender) = registry.command_sender(machine)
            && (message.encoded_len() > MAX_MESSAGE_BYTES || sender.try_send(message).is_err())
        {
            return Err(sqlx::Error::Protocol("limits snapshot unavailable".into()));
        }
    }
    Ok(())
}
#[derive(Debug, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct UsageDay {
    pub day: NaiveDate,
    pub provider: String,
    pub conversation_id: Uuid,
    pub model: String,
    pub agent: String,
    pub runs: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub weekly_delta_permille: i64,
    pub overlapped_runs: i64,
}
pub async fn usage_by_day(
    pool: &PgPool,
    owner_id: Uuid,
    provider: Option<&str>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    tz: &str,
) -> Result<Vec<UsageDay>, sqlx::Error> {
    let valid: bool =
        sqlx::query_scalar("select exists(select 1 from pg_timezone_names where name=$1)")
            .bind(tz)
            .fetch_one(pool)
            .await?;
    if !valid {
        return Err(sqlx::Error::InvalidArgument("invalid timezone".into()));
    }
    // Прирост недели хода засчитывается один раз, в строке с наибольшим расходом токенов.
    sqlx::query_as("with ranked as (select r.started_at,r.provider,r.conversation_id,r.overlapped,r.exact,r.weekly_before_permille,r.weekly_after_permille,u.model,u.agent,u.input_tokens,u.output_tokens,u.cache_read_tokens,u.cache_write_tokens,row_number() over (partition by r.input_command_id order by u.input_tokens+u.output_tokens+u.cache_write_tokens desc,u.model,u.agent) rn from runs r join conversations c on c.id=r.conversation_id join run_model_usage u using(input_command_id) where c.owner_id=$1 and ($2::text is null or r.provider=$2) and r.started_at >= $3 and r.started_at < $4 and r.provider is not null) select date_trunc('day',started_at at time zone $5)::date as day,provider,conversation_id,model,agent,count(*)::bigint as runs,sum(input_tokens)::bigint as input_tokens,sum(output_tokens)::bigint as output_tokens,sum(cache_read_tokens)::bigint as cache_read_tokens,sum(cache_write_tokens)::bigint as cache_write_tokens,coalesce(sum(case when rn=1 and not overlapped and exact and weekly_after_permille>=weekly_before_permille then weekly_after_permille-weekly_before_permille else 0 end),0)::bigint as weekly_delta_permille,count(*) filter(where overlapped or not exact)::bigint as overlapped_runs from ranked group by day,provider,conversation_id,model,agent order by day,provider,conversation_id,model,agent")
 .bind(owner_id).bind(provider).bind(from).bind(to).bind(tz).fetch_all(pool).await
}
pub async fn series(
    pool: &PgPool,
    provider: &str,
    window: Window,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<(DateTime<Utc>, u16)>, sqlx::Error> {
    let rows:Vec<(DateTime<Utc>,i32)>=sqlx::query_as("select observed_at,used_permille from limit_snapshots where provider=$1 and window_kind=$2 and observed_at >= $3 and observed_at < $4 order by observed_at,id").bind(provider).bind(window.as_str()).bind(from).bind(to).fetch_all(pool).await?;
    rows.into_iter()
        .map(|(at, used)| {
            Ok((
                at,
                u16::try_from(used)
                    .map_err(|_| sqlx::Error::Protocol("invalid stored limit".into()))?,
            ))
        })
        .collect()
}
