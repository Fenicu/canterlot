pub use crate::crunch::LimitReachedEffect;
pub use crate::limits_forecast::Forecast;
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

/// Допустимое опережение часов машины: более позднее показание прижимается к now,
/// иначе один сдвиг часов вперёд делал бы несвежими все настоящие показания.
const FUTURE_TOLERANCE: chrono::TimeDelta = chrono::TimeDelta::minutes(2);

fn clamp_to_now(observed_at: DateTime<Utc>, now: DateTime<Utc>) -> DateTime<Utc> {
    if observed_at > now + FUTURE_TOLERANCE {
        now
    } else {
        observed_at
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Window {
    FiveHour,
    Weekly,
    Monthly,
    WeeklyFable,
}
impl Window {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FiveHour => "five_hour",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::WeeklyFable => "weekly_fable",
        }
    }
    pub fn from_proto(value: i32) -> Result<Self, sqlx::Error> {
        match LimitWindow::try_from(value) {
            Ok(LimitWindow::FiveHour) => Ok(Self::FiveHour),
            Ok(LimitWindow::Weekly) => Ok(Self::Weekly),
            Ok(LimitWindow::Monthly) => Ok(Self::Monthly),
            Ok(LimitWindow::WeeklyFable) => Ok(Self::WeeklyFable),
            _ => Err(sqlx::Error::InvalidArgument("invalid limit window".into())),
        }
    }
    fn proto(self) -> i32 {
        match self {
            Self::FiveHour => LimitWindow::FiveHour as i32,
            Self::Weekly => LimitWindow::Weekly as i32,
            Self::Monthly => LimitWindow::Monthly as i32,
            Self::WeeklyFable => LimitWindow::WeeklyFable as i32,
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
    /// Учётная запись провайдера, которой принадлежит показание; ставит воркер.
    pub account: String,
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
    /// Учётная запись провайдера: у одного провайдера на машинах владельца их может быть несколько.
    pub account: String,
    pub weekly: Option<WindowState>,
    pub five_hour: Option<WindowState>,
    pub monthly: Option<WindowState>,
    pub weekly_fable: Option<WindowState>,
    pub weekly_reserve_active: bool,
    pub weekly_reserve_resets_at: Option<DateTime<Utc>>,
    /// У провайдера нет пятичасового окна: клиенты прячут счётчик, история остаётся.
    pub five_hour_absent: bool,
    pub five_hour_absent_at: Option<DateTime<Utc>>,
    /// Точность последнего показания; старые записи без поля считаются точными.
    pub data_confidence: DataConfidence,
    /// Конец входа провайдера на машинах этой учётной записи (самый ранний): у провайдера
    /// нет окна для этой даты, она приходит из кадров `result`. `None` — дата неизвестна.
    /// Поле для клиентов, которые ещё не читают `machine_credentials`.
    pub credential_expires_at: Option<DateTime<Utc>>,
    /// Сбросы лимитов с самой свежей машины этой учётной записи; `None` — провайдер о них молчит.
    /// Поле для клиентов, которые ещё не читают `machine_credits`.
    pub credits: Option<Credits>,
    /// Конец входа по машинам: у каждой машины он свой, и в одну строку не сливается.
    pub machine_credentials: Vec<MachineCredential>,
    /// Сбросы лимитов по машинам.
    pub machine_credits: Vec<MachineCredits>,
    /// Публичный статус провайдера; `None`, пока хаб его не опрашивал.
    #[serde(skip)]
    pub status: Option<crate::provider_status::Status>,
}
#[derive(Debug, Serialize)]
pub struct WindowState {
    pub used_permille: u16,
    pub resets_at: Option<DateTime<Utc>>,
    pub observed_at: DateTime<Utc>,
    /// Прогноз по темпу; `None` — данных мало или окно без времени сброса.
    pub forecast: Option<crate::limits_forecast::Forecast>,
}

/// Машина `pc.machine_id` относится к учётной записи `$3` провайдера `pc.provider`: её харнесс
/// этого провайдера держит такую метку. Машина без харнесса провайдера (вход или сбросы
/// пришли раньше отчёта машины) — `owner`.
macro_rules! account_of_machine {
    () => {
        "(exists(select 1 from machine_harnesses h where h.machine_id=pc.machine_id and h.provider=pc.provider and h.account=$3) or ($3='owner' and not exists(select 1 from machine_harnesses h where h.machine_id=pc.machine_id and h.provider=pc.provider)))"
    };
}

/// Конец входа на одной машине.
#[derive(Debug, Clone, Serialize)]
pub struct MachineCredential {
    pub machine_id: Uuid,
    pub machine_name: String,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Сбросы лимитов, о которых сообщила одна машина.
#[derive(Debug, Clone, Serialize)]
pub struct MachineCredits {
    pub machine_id: Uuid,
    pub machine_name: String,
    #[serde(flatten)]
    pub credits: Credits,
}

/// Строка состояния пары (провайдер, учётная запись). Блокировки таких строк берутся
/// в отсортированном порядке пар, как и раньше строк провайдеров.
pub(crate) async fn lock_provider(
    conn: &mut PgConnection,
    provider: &str,
    account: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into provider_limits(provider,account) values($1,$2) on conflict do nothing",
    )
    .bind(provider)
    .bind(account)
    .execute(&mut *conn)
    .await?;
    sqlx::query("select provider from provider_limits where provider=$1 and account=$2 for update")
        .bind(provider)
        .bind(account)
        .fetch_one(conn)
        .await?;
    Ok(())
}
/// Разделяемая блокировка строки пары: создание и возобновление сессии против правила резерва.
pub(crate) async fn share_provider(
    conn: &mut PgConnection,
    provider: &str,
    account: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into provider_limits(provider,account) values($1,$2) on conflict do nothing",
    )
    .bind(provider)
    .bind(account)
    .execute(&mut *conn)
    .await?;
    sqlx::query("select provider from provider_limits where provider=$1 and account=$2 for share")
        .bind(provider)
        .bind(account)
        .fetch_one(conn)
        .await?;
    Ok(())
}

/// Учётная запись харнесса машины по его строке в каталоге; нет строки — `owner`.
pub async fn harness_account(
    conn: &mut PgConnection,
    machine_id: Uuid,
    harness: &str,
) -> Result<String, sqlx::Error> {
    let account: Option<String> = sqlx::query_scalar(
        "select account from machine_harnesses where machine_id=$1 and harness=$2",
    )
    .bind(machine_id)
    .bind(harness)
    .fetch_optional(conn)
    .await?;
    Ok(account.unwrap_or_else(crate::model::owner_account))
}

/// Учётные записи харнессов провайдера на машине по её каталогу; харнесса нет — `owner`.
pub async fn machine_accounts(
    conn: &mut PgConnection,
    machine_id: Uuid,
    provider: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let mut accounts: Vec<String> = sqlx::query_scalar(
        "select distinct account from machine_harnesses where machine_id=$1 and provider=$2 order by account",
    )
    .bind(machine_id)
    .bind(provider)
    .fetch_all(conn)
    .await?;
    if accounts.is_empty() {
        accounts.push(crate::model::owner_account());
    }
    Ok(accounts)
}

/// Учётная запись факта лимита. Воркер ставит её сам, и хаб при приёме её не ищет, поэтому
/// порядок отчётов и повтор журнала показание не перепутают. Факт без метки (старый воркер)
/// считается `owner`, если сохранённые строки харнессов машины не называют для провайдера
/// другую: тогда его владелец неизвестен, и факт отбрасывается (`None`).
pub async fn fact_account(
    conn: &mut PgConnection,
    machine_id: Uuid,
    provider: &str,
    stamped: &str,
) -> Result<Option<String>, sqlx::Error> {
    if !stamped.is_empty() && !canterlot_proto::limits::valid_account(stamped) {
        return Err(sqlx::Error::InvalidArgument("invalid limit account".into()));
    }
    let account = fact_account_quiet(conn, machine_id, provider, stamped).await?;
    if account.is_none() {
        tracing::warn!(
            kind = "limit_fact_unstamped_dropped",
            provider,
            "limit fact without an account dropped: the machine declares another account for the provider"
        );
    }
    Ok(account)
}

/// То же без предупреждения и без ошибки на негодной метке (`None`): им блокируют строки пар
/// до разбора записей, а разбор уже называет причину.
pub async fn fact_account_quiet(
    conn: &mut PgConnection,
    machine_id: Uuid,
    provider: &str,
    stamped: &str,
) -> Result<Option<String>, sqlx::Error> {
    if !stamped.is_empty() {
        return Ok(canterlot_proto::limits::valid_account(stamped).then(|| stamped.to_owned()));
    }
    let foreign: bool = sqlx::query_scalar(
        "select exists(select 1 from machine_harnesses where machine_id=$1 and provider=$2 and account<>'owner')",
    )
    .bind(machine_id)
    .bind(provider)
    .fetch_one(conn)
    .await?;
    Ok((!foreign).then(crate::model::owner_account))
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
        emit_changed(
            conn,
            owner_id,
            &o.provider,
            &o.account,
            o.window,
            correlation_id,
        )
        .await?;
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
    if o.used_permille > 1000
        || !known_provider(&o.provider)
        || !canterlot_proto::limits::valid_account(&o.account)
    {
        return Err(sqlx::Error::InvalidArgument(
            "invalid limit observation".into(),
        ));
    }
    lock_provider(conn, &o.provider, &o.account).await?;
    // PostgreSQL хранит микросекунды: сравнение идёт с тем же округлением.
    let observed_at = DateTime::from_timestamp_micros(o.observed_at.timestamp_micros())
        .ok_or_else(|| sqlx::Error::InvalidArgument("invalid observation time".into()))?;
    let observed_at = clamp_to_now(observed_at, now);
    let window = o.window.as_str();
    let previous:Option<(i32,Option<DateTime<Utc>>,DateTime<Utc>)>=sqlx::query_as("select used_permille,resets_at,observed_at from limit_snapshots where provider=$1 and account=$3 and window_kind=$2 order by id desc limit 1").bind(&o.provider).bind(window).bind(&o.account).fetch_optional(&mut *conn).await?;
    let interval = chrono::Duration::from_std(config.snapshot_min_interval)
        .map_err(|_| sqlx::Error::InvalidArgument("snapshot interval overflow".into()))?;
    let store = previous.is_none_or(|(used, reset, seen)| {
        used != i32::from(o.used_permille) || reset != o.resets_at || observed_at - seen >= interval
    });
    if store {
        sqlx::query("insert into limit_snapshots(provider,window_kind,used_permille,resets_at,observed_at,received_at,source,machine_id,execution_id,account) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
 .bind(&o.provider).bind(window).bind(i32::from(o.used_permille)).bind(o.resets_at).bind(observed_at).bind(now).bind(match o.source{ObservationSource::Session=>"session",ObservationSource::Poll=>"poll"}).bind(o.machine_id).bind(o.execution_id).bind(&o.account).execute(&mut *conn).await?;
    }
    let current: Option<StoredReading> = sqlx::query_as(match o.window {
        Window::Weekly => "select weekly_used_permille,weekly_resets_at,weekly_observed_at,data_confidence from provider_limits where provider=$1 and account=$2",
        Window::FiveHour => "select five_hour_used_permille,five_hour_resets_at,five_hour_observed_at,data_confidence from provider_limits where provider=$1 and account=$2",
        Window::Monthly => "select monthly_used_permille,monthly_resets_at,monthly_observed_at,data_confidence from provider_limits where provider=$1 and account=$2",
        Window::WeeklyFable => "select weekly_fable_used_permille,weekly_fable_resets_at,weekly_fable_observed_at,data_confidence from provider_limits where provider=$1 and account=$2",
    }).bind(&o.provider).bind(&o.account).fetch_optional(&mut *conn).await?;
    let (used, reset, seen, stored_confidence) =
        current.unwrap_or((None, None, None, DataConfidence::Exact.as_str().into()));
    let stored_confidence = DataConfidence::from_stored(&stored_confidence);
    let absent_at: Option<DateTime<Utc>> = if o.window == Window::FiveHour {
        sqlx::query_scalar(
            "select five_hour_absent_at from provider_limits where provider=$1 and account=$2",
        )
        .bind(&o.provider)
        .bind(&o.account)
        .fetch_one(&mut *conn)
        .await?
    } else {
        None
    };
    // Отметка из будущего (старая версия или сдвинутые часы) не должна отбрасывать
    // верные показания до тех пор, пока реальное время её не догонит.
    let future = now + FUTURE_TOLERANCE;
    let newer = mapped
        && seen
            .filter(|seen| *seen <= future)
            .is_none_or(|seen| seen < observed_at)
        && absent_at
            .filter(|absent| *absent <= future)
            .is_none_or(|absent| absent < observed_at);
    if newer {
        sqlx::query(match o.window {
            Window::Weekly => "update provider_limits set weekly_used_permille=$2,weekly_resets_at=$3,weekly_observed_at=$4,data_confidence=$5,updated_at=$6 where provider=$1 and account=$7",
            Window::FiveHour => "update provider_limits set five_hour_used_permille=$2,five_hour_resets_at=$3,five_hour_observed_at=$4,five_hour_absent_at=null,data_confidence=$5,updated_at=$6 where provider=$1 and account=$7",
            Window::Monthly => "update provider_limits set monthly_used_permille=$2,monthly_resets_at=$3,monthly_observed_at=$4,data_confidence=$5,updated_at=$6 where provider=$1 and account=$7",
            Window::WeeklyFable => "update provider_limits set weekly_fable_used_permille=$2,weekly_fable_resets_at=$3,weekly_fable_observed_at=$4,data_confidence=$5,updated_at=$6 where provider=$1 and account=$7",
        }).bind(&o.provider).bind(i32::from(o.used_permille)).bind(o.resets_at).bind(observed_at).bind(o.data_confidence.as_str()).bind(now).bind(&o.account).execute(&mut *conn).await?;
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
            &o.account,
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
    ) = sqlx::query_as("select weekly_reserve_active,weekly_reserve_resets_at,weekly_low_count,weekly_low_at from provider_limits where provider=$1 and account=$2")
        .bind(&o.provider)
        .bind(&o.account)
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
    sqlx::query("update provider_limits set weekly_reserve_active=$2,weekly_reserve_resets_at=$3,weekly_low_count=$4,weekly_low_at=$5 where provider=$1 and account=$6 and (weekly_reserve_active,weekly_reserve_resets_at,weekly_low_count,weekly_low_at) is distinct from ($2,$3,$4,$5)")
        .bind(&o.provider).bind(active).bind(if active { reserve_resets } else { None }).bind(low_count).bind(low_at).bind(&o.account)
        .execute(&mut *conn).await?;
    if !active {
        return Ok(());
    }
    // Вставляет только держатель строки пары, поэтому проверка без ожидания чужих транзакций.
    // Для `owner` нагрузка прежняя, без ключа `account`.
    sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) select $1,'limit_rule',m.owner_id,null,$3,case when $5='owner' then jsonb_build_object('provider',$4::text) else jsonb_build_object('provider',$4::text,'account',$5::text) end from machines m where m.id=$2 and not exists(select 1 from scheduled_jobs where kind='limit_rule' and state='pending' and payload->>'provider'=$4 and coalesce(payload->>'account','owner')=$5) on conflict do nothing")
        .bind(Uuid::now_v7()).bind(o.machine_id).bind(now).bind(&o.provider).bind(&o.account)
        .execute(&mut *conn).await?;
    Ok(())
}
pub(crate) async fn emit_changed(
    conn: &mut PgConnection,
    owner_id: Uuid,
    provider: &str,
    account: &str,
    window: Window,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    // Пара, которой не предлагает ни одна машина владельца, клиентам не показывается.
    if !offered_accounts(&mut *conn, owner_id)
        .await?
        .iter()
        .any(|(p, a)| p == provider && a == account)
    {
        return Ok(());
    }
    let states = current_for(conn, Some((provider, account))).await?;
    if let Some(p) = states.into_iter().next() {
        let state = match window {
            Window::Weekly => p.weekly,
            Window::FiveHour => p.five_hour,
            Window::Monthly => p.monthly,
            Window::WeeklyFable => p.weekly_fable,
        };
        let wire = |at: DateTime<Utc>| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let reserve = p.weekly_reserve_resets_at.map(wire);
        // Дата конца входа меняется без смены окна: её несёт каждое событие лимитов.
        let credential = credential_expires_at(&mut *conn, owner_id, provider, account)
            .await?
            .map(wire);
        // Сбросы меняются без смены окна: их несёт каждое событие лимитов.
        let credits = credits(&mut *conn, owner_id, provider, account)
            .await
            .and_then(|credits| {
                serde_json::to_value(credits).map_err(|error| sqlx::Error::Encode(Box::new(error)))
            })?;
        // Вход и сбросы по машинам: у каждой машины они свои и не сливаются в одну дату.
        let machine_credentials = serde_json::to_value(
            machine_credentials(&mut *conn, owner_id, provider, account).await?,
        )
        .map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
        let machine_credits =
            serde_json::to_value(machine_credits(&mut *conn, owner_id, provider, account).await?)
                .map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
        let forecast = state
            .as_ref()
            .and_then(|s| s.forecast.as_ref())
            .and_then(|f| {
                serde_json::to_value(crate::http::limits::ForecastView::from(f.clone())).ok()
            });
        // Отсутствующее окно уходит клиентам событием без значения: `used_permille` и `resets_at` равны null.
        let payload = match (state, p.five_hour_absent_at) {
            (Some(s), _) => Some(
                json!({"provider":provider,"account":account,"window":window,"used_permille":s.used_permille,"resets_at":s.resets_at.map(wire),"observed_at":wire(s.observed_at),"weekly_reserve_active":p.weekly_reserve_active,"weekly_reserve_resets_at":reserve,"data_confidence":p.data_confidence.as_str(),"forecast":forecast,"credential_expires_at":credential,"credits":credits,"machine_credentials":machine_credentials,"machine_credits":machine_credits}),
            ),
            (None, Some(absent)) if window == Window::FiveHour => Some(
                json!({"provider":provider,"account":account,"window":window,"used_permille":null,"resets_at":null,"observed_at":wire(absent),"weekly_reserve_active":p.weekly_reserve_active,"weekly_reserve_resets_at":reserve,"data_confidence":p.data_confidence.as_str(),"forecast":forecast,"credential_expires_at":credential,"credits":credits,"machine_credentials":machine_credentials,"machine_credits":machine_credits}),
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
    account: &str,
    window: Window,
    observed_at: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    if window != Window::FiveHour
        || !known_provider(provider)
        || !canterlot_proto::limits::valid_account(account)
    {
        return Err(sqlx::Error::InvalidArgument("invalid limit absence".into()));
    }
    let observed_at = DateTime::from_timestamp_micros(observed_at.timestamp_micros())
        .ok_or_else(|| sqlx::Error::InvalidArgument("invalid observation time".into()))?;
    let observed_at = clamp_to_now(observed_at, Utc::now());
    lock_provider(conn, provider, account).await?;
    let (seen, absent): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as(
        "select five_hour_observed_at,five_hour_absent_at from provider_limits where provider=$1 and account=$2",
    )
    .bind(provider)
    .bind(account)
    .fetch_one(&mut *conn)
    .await?;
    // Более позднее показание окна сильнее устаревшего отчёта об отсутствии.
    // Отметка из будущего недействительна и не блокирует верный отчёт.
    let future = Utc::now() + FUTURE_TOLERANCE;
    if seen
        .filter(|seen| *seen <= future)
        .is_some_and(|seen| seen >= observed_at)
        || absent
            .filter(|at| *at <= future)
            .is_some_and(|at| at >= observed_at)
    {
        return Ok(false);
    }
    sqlx::query("update provider_limits set five_hour_used_permille=null,five_hour_resets_at=null,five_hour_observed_at=null,five_hour_absent_at=$2,updated_at=clock_timestamp() where provider=$1 and account=$3")
        .bind(provider)
        .bind(observed_at)
        .bind(account)
        .execute(&mut *conn)
        .await?;
    let first = absent.is_none();
    if first {
        emit_changed(conn, owner_id, provider, account, window, correlation_id).await?;
    }
    // Исчезнувшее окно — тоже признак сброса: условие будильника проверяется здесь.
    crate::session_tools::alarms::evaluate_limit_in_tx(
        conn,
        provider,
        account,
        window,
        observed_at,
        correlation_id,
    )
    .await?;
    Ok(first)
}
/// Совпадает ли учётная запись факта с текущей меткой харнесса разговора на его машине.
/// Харнесс без строки в каталоге машины ничего не оспаривает.
async fn account_is_current(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    account: &str,
) -> Result<bool, sqlx::Error> {
    let current: Option<String> = sqlx::query_scalar(
        "select h.account from conversations c join machine_harnesses h on h.machine_id=c.machine_id and h.harness=c.harness where c.id=$1",
    )
    .bind(conversation_id)
    .fetch_optional(conn)
    .await?;
    Ok(current.is_none_or(|current| current == account))
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
    // Учётную запись называет воркер; факт без метки при чужой метке харнесса машины
    // сюда не доходит: его отбрасывает приём журнала.
    let account = canterlot_proto::limits::account_or_owner(&fact.account);
    if !canterlot_proto::limits::valid_account(account) {
        return Err(sqlx::Error::InvalidArgument("invalid limit account".into()));
    }
    // Строка пары раньше очереди: общий порядок блокировок хаба.
    lock_provider(conn, &fact.provider, account).await?;
    // Под замком пары: факт чужой учётной записи (повтор журнала после смены метки) в очередь
    // разговора не попадает, ни удержанием, ни паузой.
    if !account_is_current(conn, run.conversation_id, account).await? {
        tracing::info!(
            kind = "limit_reached_stale_account",
            provider = fact.provider.as_str(),
            "limit fact of an account the harness no longer has is not applied to the queue"
        );
        return Ok(LimitReachedEffect::Stale);
    }
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
    // Одноразовое продолжение уже держит это окно: повторный факт лимита не добавляет
    // вторую, обычную паузу под удержанием.
    let one_shot_pending: bool = sqlx::query_scalar(
        "select exists(select 1 from scheduled_jobs where conversation_id=$1 and kind='wake'
           and state='pending' and payload->>'one_shot'='true'
           and payload->>'provider'=$2 and payload->>'window'=$3
           and coalesce(payload->>'account','owner')=$4)",
    )
    .bind(run.conversation_id)
    .bind(&fact.provider)
    .bind(window.as_str())
    .bind(account)
    .fetch_one(&mut *conn)
    .await?;
    if one_shot_pending {
        return Ok(LimitReachedEffect::Paused);
    }
    // Пока пауза по этому окну стоит, повторный факт ничего не добавляет.
    if queue.pause_reasons.0.iter().any(|reason| {
        matches!(reason, PauseReason::LimitReached { provider, account: stored_account, window: stored, .. }
            if provider.as_deref() == Some(fact.provider.as_str()) && stored_account == account && stored.as_deref() == Some(window.as_str()))
    }) {
        return Ok(LimitReachedEffect::Paused);
    }
    let resets_at = timestamp(fact.resets_at_ms)?;
    queue.pause_reasons.0.push(PauseReason::LimitReached {
        provider: Some(fact.provider.clone()),
        account: account.to_owned(),
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
async fn window_state(
    conn: &mut PgConnection,
    provider: &str,
    account: &str,
    prefix: &str,
    window: Window,
    row: &sqlx::postgres::PgRow,
    now: DateTime<Utc>,
) -> Result<Option<WindowState>, sqlx::Error> {
    let used: Option<i32> = row.try_get(format!("{prefix}_used_permille").as_str())?;
    let observed_at: Option<DateTime<Utc>> =
        row.try_get(format!("{prefix}_observed_at").as_str())?;
    let (Some(used), Some(observed_at)) = (used, observed_at) else {
        return Ok(None);
    };
    let used_permille =
        u16::try_from(used).map_err(|_| sqlx::Error::Protocol("invalid stored limit".into()))?;
    let resets_at: Option<DateTime<Utc>> = row.try_get(format!("{prefix}_resets_at").as_str())?;
    let current = crate::limits_forecast::Reading {
        used_permille,
        resets_at,
        observed_at,
    };
    let forecast =
        crate::limits_forecast::load(&mut *conn, provider, account, window, current, now).await?;
    Ok(Some(WindowState {
        used_permille,
        resets_at,
        observed_at,
        forecast,
    }))
}

pub async fn current(conn: &mut PgConnection) -> Result<Vec<ProviderLimits>, sqlx::Error> {
    current_for(conn, None).await
}

/// Состояние всех пар или одной. Строки идут по провайдеру, затем по учётной записи.
pub async fn current_for(
    conn: &mut PgConnection,
    only: Option<(&str, &str)>,
) -> Result<Vec<ProviderLimits>, sqlx::Error> {
    let now = Utc::now();
    let rows = sqlx::query(
        "select * from provider_limits
          where ($1::text is null or (provider=$1 and account=$2))
          order by provider, account",
    )
    .bind(only.map(|(provider, _)| provider))
    .bind(only.map(|(_, account)| account))
    .fetch_all(&mut *conn)
    .await?;
    let statuses = crate::provider_status::load_all(&mut *conn, now).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let provider: String = row.try_get("provider")?;
        let account: String = row.try_get("account")?;
        // Публичный статус общий на провайдера, поэтому его получает каждая учётная запись.
        let status = statuses.get(&provider).cloned();
        out.push(ProviderLimits {
            weekly: window_state(
                &mut *conn,
                &provider,
                &account,
                "weekly",
                Window::Weekly,
                row,
                now,
            )
            .await?,
            five_hour: window_state(
                &mut *conn,
                &provider,
                &account,
                "five_hour",
                Window::FiveHour,
                row,
                now,
            )
            .await?,
            monthly: window_state(
                &mut *conn,
                &provider,
                &account,
                "monthly",
                Window::Monthly,
                row,
                now,
            )
            .await?,
            weekly_fable: window_state(
                &mut *conn,
                &provider,
                &account,
                "weekly_fable",
                Window::WeeklyFable,
                row,
                now,
            )
            .await?,
            provider,
            account,
            weekly_reserve_active: row.try_get("weekly_reserve_active")?,
            weekly_reserve_resets_at: row.try_get("weekly_reserve_resets_at")?,
            five_hour_absent: row
                .try_get::<Option<DateTime<Utc>>, _>("five_hour_absent_at")?
                .is_some(),
            five_hour_absent_at: row.try_get("five_hour_absent_at")?,
            data_confidence: DataConfidence::from_stored(
                row.try_get::<Option<String>, _>("data_confidence")?
                    .as_deref()
                    .unwrap_or("exact"),
            ),
            credential_expires_at: None,
            credits: None,
            machine_credentials: Vec::new(),
            machine_credits: Vec::new(),
            status,
        });
    }
    Ok(out)
}
/// Пары (провайдер, учётная запись), чей харнесс сейчас есть в каталоге хотя бы одной
/// машины владельца.
pub async fn offered_accounts(
    conn: &mut PgConnection,
    owner_id: Uuid,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as(
        "select distinct h.provider, h.account from machine_harnesses h join machines m on m.id=h.machine_id where m.owner_id=$1 order by h.provider, h.account",
    )
    .bind(owner_id)
    .fetch_all(conn)
    .await
}
/// Провайдеры, чей харнесс сейчас есть в каталоге хотя бы одной машины владельца.
pub async fn offered_providers(
    conn: &mut PgConnection,
    owner_id: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "select distinct h.provider from machine_harnesses h join machines m on m.id=h.machine_id where m.owner_id=$1 order by h.provider",
    )
    .bind(owner_id)
    .fetch_all(conn)
    .await
}
/// Текущие лимиты для владельца: только провайдеры из каталога его машин.
/// Строки остальных провайдеров и их история остаются в базе и возвращаются вместе с харнессом.
pub async fn shown(
    conn: &mut PgConnection,
    owner_id: Uuid,
) -> Result<Vec<ProviderLimits>, sqlx::Error> {
    let offered = offered_accounts(&mut *conn, owner_id).await?;
    let mut rows: Vec<ProviderLimits> = current(conn)
        .await?
        .into_iter()
        .filter(|p| {
            offered
                .iter()
                .any(|(provider, account)| *provider == p.provider && *account == p.account)
        })
        .collect();
    for row in &mut rows {
        row.credential_expires_at =
            credential_expires_at(&mut *conn, owner_id, &row.provider, &row.account).await?;
        row.credits = credits(&mut *conn, owner_id, &row.provider, &row.account).await?;
        row.machine_credentials =
            machine_credentials(&mut *conn, owner_id, &row.provider, &row.account).await?;
        row.machine_credits =
            machine_credits(&mut *conn, owner_id, &row.provider, &row.account).await?;
    }
    Ok(rows)
}

/// Самый ранний конец входа провайдера среди машин владельца с этой учётной записью:
/// вход свой на каждой машине, и дата другой учётной записи в строку не попадает.
pub async fn credential_expires_at(
    conn: &mut PgConnection,
    owner_id: Uuid,
    provider: &str,
    account: &str,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar(concat!(
        "select min(pc.expires_at) from provider_credentials pc
         join machines m on m.id=pc.machine_id
         where m.owner_id=$1 and pc.provider=$2 and ",
        account_of_machine!()
    ))
    .bind(owner_id)
    .bind(provider)
    .bind(account)
    .fetch_one(conn)
    .await
}

/// Конец входа по машинам учётной записи: каждая машина называется своим именем.
pub async fn machine_credentials(
    conn: &mut PgConnection,
    owner_id: Uuid,
    provider: &str,
    account: &str,
) -> Result<Vec<MachineCredential>, sqlx::Error> {
    let rows: Vec<(Uuid, String, Option<DateTime<Utc>>)> = sqlx::query_as(concat!(
        "select m.id,m.name,pc.expires_at from provider_credentials pc
         join machines m on m.id=pc.machine_id
         where m.owner_id=$1 and pc.provider=$2 and ",
        account_of_machine!(),
        " order by m.name, m.id"
    ))
    .bind(owner_id)
    .bind(provider)
    .bind(account)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(machine_id, machine_name, expires_at)| MachineCredential {
            machine_id,
            machine_name,
            expires_at,
        })
        .collect())
}

/// Дата конца входа из кадра `result` Claude Code. Дата заменяется только более поздним
/// наблюдением: пересланная старая запись после повторного входа её не откатывает.
pub async fn record_credential_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    provider: &str,
    expires_at: DateTime<Utc>,
    observed_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    if !known_provider(provider) {
        return Err(sqlx::Error::InvalidArgument(
            "unknown limit provider".into(),
        ));
    }
    let observed_at = DateTime::from_timestamp_micros(observed_at.timestamp_micros())
        .ok_or_else(|| sqlx::Error::InvalidArgument("invalid credential time".into()))?;
    let observed_at = clamp_to_now(observed_at, now);
    let stored: Option<DateTime<Utc>> = sqlx::query_scalar(
        "insert into provider_credentials(machine_id,provider,expires_at,observed_at)
         values($1,$2,$3,$4)
         on conflict(machine_id,provider) do update
           set expires_at=excluded.expires_at,observed_at=excluded.observed_at
           where provider_credentials.observed_at < excluded.observed_at
         returning expires_at",
    )
    .bind(machine_id)
    .bind(provider)
    .bind(expires_at)
    .bind(observed_at)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(stored.is_some())
}

/// Сбросы лимитов провайдера с самой свежей машины владельца.
#[derive(Clone, Debug, Serialize)]
pub struct Credits {
    /// Сколько сбросов доступно; `None` — провайдер их ещё не называл.
    pub reset_count: Option<u32>,
    pub resets: Vec<Reset>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Reset {
    /// Заголовок от провайдера; пусто — «Сброс лимитов».
    pub title: String,
    /// `None` — сброс не сгорает.
    pub expires_at: Option<DateTime<Utc>>,
    pub granted_at: Option<DateTime<Utc>>,
}

/// Самые свежие сбросы провайдера среди машин владельца с этой учётной записью.
pub async fn credits(
    conn: &mut PgConnection,
    owner_id: Uuid,
    provider: &str,
    account: &str,
) -> Result<Option<Credits>, sqlx::Error> {
    let row = sqlx::query(concat!(
        "select pc.reset_count,pc.resets,pc.observed_at from provider_credits pc
         join machines m on m.id=pc.machine_id
         where m.owner_id=$1 and pc.provider=$2 and ",
        account_of_machine!(),
        " order by pc.observed_at desc limit 1"
    ))
    .bind(owner_id)
    .bind(provider)
    .bind(account)
    .fetch_optional(conn)
    .await?;
    row.as_ref().map(credits_from_row).transpose()
}

/// Сбросы лимитов по машинам учётной записи: каждая машина называется своим именем.
pub async fn machine_credits(
    conn: &mut PgConnection,
    owner_id: Uuid,
    provider: &str,
    account: &str,
) -> Result<Vec<MachineCredits>, sqlx::Error> {
    let rows = sqlx::query(concat!(
        "select m.id as machine_id,m.name as machine_name,pc.reset_count,pc.resets,pc.observed_at
         from provider_credits pc
         join machines m on m.id=pc.machine_id
         where m.owner_id=$1 and pc.provider=$2 and ",
        account_of_machine!(),
        " order by m.name, m.id"
    ))
    .bind(owner_id)
    .bind(provider)
    .bind(account)
    .fetch_all(conn)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(MachineCredits {
                machine_id: row.try_get("machine_id")?,
                machine_name: row.try_get("machine_name")?,
                credits: credits_from_row(row)?,
            })
        })
        .collect()
}

fn credits_from_row(row: &sqlx::postgres::PgRow) -> Result<Credits, sqlx::Error> {
    let resets: Vec<Reset> = row
        .try_get::<serde_json::Value, _>("resets")?
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|reset| Reset {
                    title: reset["title"].as_str().unwrap_or_default().to_owned(),
                    expires_at: reset["expires_at_ms"]
                        .as_u64()
                        .and_then(|ms| timestamp(ms).ok().flatten()),
                    granted_at: reset["granted_at_ms"]
                        .as_u64()
                        .and_then(|ms| timestamp(ms).ok().flatten()),
                })
                .collect()
        })
        .unwrap_or_default();
    let count: Option<i32> = row.try_get("reset_count")?;
    Ok(Credits {
        reset_count: count.and_then(|count| u32::try_from(count).ok()),
        resets,
        observed_at: row.try_get("observed_at")?,
    })
}

/// Сбросы из отчёта машины. Что машина не знает, не затирается: прежняя строка хранит
/// свои сбросы, пока не придёт знающее наблюдение.
pub async fn record_credits_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    provider: &str,
    credits: &canterlot_proto::worker::v1::ProviderCredits,
    observed_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    if !known_provider(provider) {
        return Err(sqlx::Error::InvalidArgument(
            "unknown limit provider".into(),
        ));
    }
    if !credits.resets_known {
        return Ok(false);
    }
    let observed_at = DateTime::from_timestamp_micros(observed_at.timestamp_micros())
        .ok_or_else(|| sqlx::Error::InvalidArgument("invalid credits time".into()))?;
    let observed_at = clamp_to_now(observed_at, now);
    // Список может быть короче счёта: хаб показывает счёт и то, что провайдер назвал.
    let resets: serde_json::Value = credits
        .resets
        .iter()
        .take(64)
        .map(|reset| {
            json!({
                "title": reset.title.chars().take(200).collect::<String>(),
                "expires_at_ms": reset.expires_at_ms,
                "granted_at_ms": reset.granted_at_ms,
            })
        })
        .collect();
    let stored: Option<DateTime<Utc>> = sqlx::query_scalar(
        "insert into provider_credits(machine_id,provider,reset_count,resets,observed_at)
         values($1,$2,$3,$4,$5)
         on conflict(machine_id,provider) do update
           set reset_count=excluded.reset_count,resets=excluded.resets,observed_at=excluded.observed_at
           where provider_credits.observed_at < excluded.observed_at
         returning observed_at",
    )
    .bind(machine_id)
    .bind(provider)
    .bind(i64::from(credits.reset_count))
    .bind(resets)
    .bind(observed_at)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(stored.is_some())
}
/// Каталог машин владельца изменил набор провайдеров: клиенты убирают ушедших,
/// а вернувшиеся получают свои окна событиями `limits_changed`.
pub(crate) async fn emit_offered_changed(
    conn: &mut PgConnection,
    owner_id: Uuid,
    before: &[(String, String)],
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let after = offered_accounts(&mut *conn, owner_id).await?;
    if after == before {
        return Ok(());
    }
    // `providers` читают клиенты без учётных записей, `accounts` — клиенты с ними.
    let providers: Vec<&str> = {
        let mut names: Vec<&str> = after
            .iter()
            .map(|(provider, _)| provider.as_str())
            .collect();
        names.dedup();
        names
    };
    let accounts: Vec<serde_json::Value> = after
        .iter()
        .map(|(provider, account)| json!({"provider": provider, "account": account}))
        .collect();
    events::append(
        &mut *conn,
        NewEvent {
            scope: Scope::Conversations { owner_id },
            kind: "limits_providers_changed",
            conversation_id: None,
            execution_id: None,
            payload: json!({ "providers": providers, "accounts": accounts }),
            correlation_id,
        },
    )
    .await?;
    for (provider, account) in after.iter().filter(|pair| !before.contains(pair)) {
        for window in [
            Window::FiveHour,
            Window::Weekly,
            Window::Monthly,
            Window::WeeklyFable,
        ] {
            emit_changed(
                &mut *conn,
                owner_id,
                provider,
                account,
                window,
                correlation_id,
            )
            .await?;
        }
    }
    Ok(())
}
async fn stored_provider(
    pool: &PgPool,
    provider: &str,
    account: &str,
) -> Result<Option<ProviderLimits>, sqlx::Error> {
    Ok(
        current_for(&mut *pool.acquire().await?, Some((provider, account)))
            .await?
            .into_iter()
            .next(),
    )
}
// Время наблюдения уходит в часах машины-получателя: часы хаба минус смещение сеанса.
fn snapshot_message(
    provider: &str,
    account: &str,
    stored: Option<&ProviderLimits>,
    offset: Option<chrono::Duration>,
) -> ControlDown {
    let mut observations = Vec::new();
    if let Some(p) = stored {
        for (window, state) in [
            (Window::FiveHour, &p.five_hour),
            (Window::Weekly, &p.weekly),
            (Window::Monthly, &p.monthly),
            (Window::WeeklyFable, &p.weekly_fable),
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
                    account: account.into(),
                });
            }
        }
    }
    ControlDown {
        msg: Some(control_down::Msg::LimitsSnapshot(LimitsSnapshot {
            provider: provider.into(),
            current: observations,
            weekly_reserve_active: stored.is_some_and(|p| p.weekly_reserve_active),
            account: account.into(),
        })),
    }
}
/// Снимок состояния пары уходит только машинам, чей харнесс этого провайдера
/// держит эту учётную запись.
pub async fn push_limits_snapshot(
    pool: &PgPool,
    registry: &Registry,
    provider: &str,
    account: &str,
) -> Result<usize, sqlx::Error> {
    let machines: Vec<Uuid> = sqlx::query_scalar(
        "select distinct machine_id from machine_harnesses where provider=$1 and account=$2 order by machine_id",
    )
    .bind(provider)
    .bind(account)
    .fetch_all(pool)
    .await?;
    let stored = stored_provider(pool, provider, account).await?;
    let mut sent = 0;
    for machine in machines {
        if registry.has_capability(machine, CAPABILITY_PILOT_V1)
            && let Some(sender) = registry.command_sender(machine)
        {
            let message = snapshot_message(
                provider,
                account,
                stored.as_ref(),
                registry.clock_offset(machine),
            );
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
    let pairs: Vec<(String, String)> = sqlx::query_as(
        "select distinct provider, account from machine_harnesses where machine_id=$1 order by provider, account",
    )
    .bind(machine)
    .fetch_all(pool)
    .await?;
    for (provider, account) in pairs {
        let stored = stored_provider(pool, &provider, &account).await?;
        let message = snapshot_message(
            &provider,
            &account,
            stored.as_ref(),
            registry.clock_offset(machine),
        );
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
    /// Учётная запись провайдера, на которой шли ходы.
    pub account: String,
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
    sqlx::query_as("with ranked as (select r.started_at,r.provider,r.account,coalesce(c.side_of,r.conversation_id) conversation_id,r.overlapped,r.exact,r.weekly_before_permille,r.weekly_after_permille,u.model,u.agent,u.input_tokens,u.output_tokens,u.cache_read_tokens,u.cache_write_tokens,row_number() over (partition by r.input_command_id order by u.input_tokens+u.output_tokens+u.cache_write_tokens desc,u.model,u.agent) rn from runs r join conversations c on c.id=r.conversation_id join run_model_usage u using(input_command_id) where c.owner_id=$1 and c.trashed_at is null and ($2::text is null or r.provider=$2) and r.started_at >= $3 and r.started_at < $4 and r.provider is not null) select date_trunc('day',started_at at time zone $5)::date as day,provider,account,conversation_id,model,agent,count(*)::bigint as runs,sum(input_tokens)::bigint as input_tokens,sum(output_tokens)::bigint as output_tokens,sum(cache_read_tokens)::bigint as cache_read_tokens,sum(cache_write_tokens)::bigint as cache_write_tokens,coalesce(sum(case when rn=1 and not overlapped and exact and weekly_after_permille>=weekly_before_permille then weekly_after_permille-weekly_before_permille else 0 end),0)::bigint as weekly_delta_permille,count(*) filter(where overlapped or not exact)::bigint as overlapped_runs from ranked group by day,provider,account,conversation_id,model,agent order by day,provider,account,conversation_id,model,agent")
 .bind(owner_id).bind(provider).bind(from).bind(to).bind(tz).fetch_all(pool).await
}
pub async fn series(
    pool: &PgPool,
    provider: &str,
    account: &str,
    window: Window,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<(DateTime<Utc>, u16)>, sqlx::Error> {
    let rows:Vec<(DateTime<Utc>,i32)>=sqlx::query_as("select observed_at,used_permille from limit_snapshots where provider=$1 and account=$5 and window_kind=$2 and observed_at >= $3 and observed_at < $4 order by observed_at,id").bind(provider).bind(window.as_str()).bind(from).bind(to).bind(account).fetch_all(pool).await?;
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
