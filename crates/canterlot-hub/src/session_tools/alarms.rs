//! Будильники: разбудить сессию по времени или по устоявшемуся состоянию другой сессии.
//!
//! Срабатывание не пишет в чужую очередь напрямую: оно ставит задание `session_task`
//! с действием `alarm_deliver`, а доставка забирает все сработавшие будильники получателя
//! пачкой. Заметки и тексты сообщений — данные уровня сообщения: в журнал и `Debug` не попадают.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{
    Caller, ToolContext, ToolError, ToolErrorCode, access, completion, origin, parse_arguments,
    send, settle, subs, summary,
};
use crate::{
    crunch::ResetCheck,
    delivery::{ContentBlock, SubmitError, SubmitInput, submit_input_in_tx},
    events::{self, NewEvent, Scope},
    limits::{DataConfidence, Window},
    scheduler::ClaimedJob,
};
use canterlot_proto::worker::v1::MachineMetrics;

/// Сколько сработавших будильников получателя сливаются в одно сообщение.
const COALESCE: TimeDelta = TimeDelta::seconds(5);
const MAX_ARMED: i64 = 20;
const MAX_TARGETS: usize = 20;
const MAX_NOTE_BYTES: usize = 2 * 1024;
const MIN_AT_SECONDS: i64 = 30;
const MAX_AT_SECONDS: i64 = 366 * 86400;
const DEFAULT_DEADLINE_SECONDS: i64 = 86400;
const MIN_DEADLINE_SECONDS: i64 = 60;
const MAX_DEADLINE_SECONDS: i64 = 7 * 86400;

/// Сколько ждём свежее показание лимитов после опроса на `resets_at + reset_grace`.
const PROBE_VERIFY_WINDOW: TimeDelta = TimeDelta::minutes(30);
/// Повтор задания `alarm_probe`, пока не истекло окно проверки.
const PROBE_RETRY: TimeDelta = TimeDelta::seconds(30);
/// Показания машины старше этого не доказывают условие: их заменит следующий отчёт.
const MACHINE_METRICS_MAX_AGE: TimeDelta = TimeDelta::seconds(90);
const BYTES_PER_GB: f64 = 1e9;

const DEFAULT_STATES: [&str; 5] = ["idle", "waiting_for_user", "failed", "stopped", "deleted"];
const ALL_STATES: [&str; 7] = [
    "idle",
    "waiting_for_user",
    "stalled",
    "quota_paused",
    "failed",
    "stopped",
    "deleted",
];

const HINT_ARMED: &str = "Alarm armed. End your turn; a [canterlot] message wakes you. Do not use sleep commands or polling for this wait.";
const HINT_SATISFIED: &str = "The condition already holds; nothing was armed.";
const AUTOMATION: &str = "This is canterlot automation, not the owner.";

/// Состояние условия `session_state`: отметки целей снимаются при взводе.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SessionCondition {
    kind: String,
    session_ids: Vec<Uuid>,
    states: Vec<String>,
    #[serde(default)]
    all: bool,
    /// Отпечаток каждой цели на момент взвода. Условие, уже истинное, не взводится.
    #[serde(default)]
    armed: BTreeMap<String, String>,
}

/// Условие `limit_reset`: при взводе запоминаем `resets_at` и расход окна.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct LimitCondition {
    kind: String,
    provider: String,
    window: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    below_percent: Option<f64>,
    armed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resets_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    used_permille: Option<u16>,
    /// Что именно сработало: `reset`, `below_percent` или `by_clock`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trigger: Option<String>,
    /// `false` у срабатывания по часам: свежего показания, подтверждающего сброс, не было.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verified: Option<bool>,
    /// Расход в момент срабатывания: текст сообщения не ходит в лимиты ещё раз.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_used_permille: Option<u16>,
}

type LimitReading = (
    Option<i32>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    String,
);

/// Условие `machine`: все заданные пороги должны выполняться одновременно.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct MachineCondition {
    kind: String,
    machine_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    free_disk_gb_above: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    load_per_core_below: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mem_available_gb_above: Option<f64>,
    armed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_free_disk_gb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_load_per_core: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_mem_available_gb: Option<f64>,
}

/// Что уже собрано по целям: для `all` — состояния, для пачки — удержанные итоги.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Collected {
    #[serde(default)]
    sessions: BTreeMap<String, CollectedSession>,
}

/// Старые записи хранили границу временем: не разбираем её, но и не теряем остальное.
fn boundary_or_none<'de, D>(deserializer: D) -> Result<Option<summary::Boundary>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// Кому принадлежит итог цели: пачке (`held`), обычной подписке (`separate`) или
/// пока никому (`pending`, счёт есть, но уведомление ещё не доставлено).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Ownership {
    /// Состояние засчитано, но владение не разрешено: строку подписки держит чужая
    /// транзакция. Пачка не срабатывает, пока цель не станет `held` или `separate`.
    #[default]
    Pending,
    Held,
    Separate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CollectedSession {
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fingerprint: Option<String>,
    /// Цель условия устоялась в подходящем состоянии. Это отдельный факт от удержания
    /// итога: пачка удерживает только `idle`/`stopped`, остальные состояния приходят
    /// обычной подпиской и здесь только засчитываются.
    #[serde(default)]
    satisfied: bool,
    /// Владение итогом. У старых записей поля нет: выводим из подписки и `satisfied`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ownership: Option<Ownership>,
    /// Цель уже устоялась при взводе пачки: её итог ушёл подпиской раньше, и в сообщении
    /// пачки об этом говорит отдельная строка.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    settled_before_arming: bool,
    /// Пачка хранит, из какой подписки и какого поколения взят итог.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subscription_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    armed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    chain_depth: i32,
    /// Граница завершения, которую покрывает удержанный итог: более поздний ход цели
    /// не должен подменить его при отрисовке. Идентичность хода и блоков хаба, не время.
    /// У старых записей здесь время: такое поле просто отбрасывается, не сбрасывая запись.
    #[serde(
        default,
        deserialize_with = "boundary_or_none",
        skip_serializing_if = "Option::is_none"
    )]
    boundary: Option<summary::Boundary>,
}

impl CollectedSession {
    fn owned(&self) -> Ownership {
        self.ownership.unwrap_or_else(|| {
            if self.subscription_id.is_some() {
                Ownership::Held
            } else if self.satisfied {
                Ownership::Separate
            } else {
                Ownership::Pending
            }
        })
    }
}

impl Collected {
    fn parse(value: &Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }

    fn as_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({"sessions": {}}))
    }

    fn get(&self, id: Uuid) -> Option<&CollectedSession> {
        self.sessions.get(&id.to_string())
    }

    fn has(&self, id: Uuid) -> bool {
        self.get(id).is_some()
    }

    fn insert(&mut self, id: Uuid, entry: CollectedSession) {
        self.sessions.insert(id.to_string(), entry);
    }

    /// Все цели условия засчитаны. Удаление цели закрывает пачку даже тогда, когда
    /// `deleted` не перечислен в `states`: отдельное сообщение об удалении уже ушло
    /// обычной подпиской, пачке остаётся только посчитать его.
    fn complete(&self, condition: &SessionCondition) -> bool {
        condition.session_ids.iter().all(|id| {
            self.get(*id).is_some_and(|entry| {
                entry.satisfied
                    && entry.owned() != Ownership::Pending
                    && (condition.states.contains(&entry.state) || entry.state == "deleted")
            })
        })
    }

    fn any_matching(&self, condition: &SessionCondition) -> bool {
        self.sessions.values().any(|entry| {
            entry.satisfied
                && entry.owned() != Ownership::Pending
                && condition.states.contains(&entry.state)
        })
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WhenArgs {
    kind: String,
    session_ids: Option<Vec<Uuid>>,
    states: Option<Vec<String>>,
    all: Option<bool>,
    provider: Option<String>,
    window: Option<String>,
    below_percent: Option<f64>,
    free_disk_gb_above: Option<f64>,
    load_per_core_below: Option<f64>,
    mem_available_gb_above: Option<f64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SetArgs {
    at: Option<String>,
    in_seconds: Option<i64>,
    when: Option<WhenArgs>,
    deadline_seconds: Option<i64>,
    only_on_timeout: Option<bool>,
    batch: Option<bool>,
    note: Option<String>,
    key: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CancelArgs {
    alarm_id: Option<Uuid>,
    key: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    include_finished: Option<bool>,
}

/// Строка будильника.
#[derive(Clone, sqlx::FromRow)]
pub struct AlarmRow {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub conversation_id: Uuid,
    pub key: Option<String>,
    pub kind: String,
    pub condition: Option<Value>,
    pub fire_at: Option<DateTime<Utc>>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub only_on_timeout: bool,
    pub batch: bool,
    pub collected: Value,
    pub note: Option<String>,
    pub chain_depth: i32,
    pub state: String,
    pub fired_reason: Option<String>,
    pub failure_reason: Option<String>,
    pub call_id: Uuid,
    pub response: Value,
    pub delivered_input_id: Option<Uuid>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub fired_at: Option<DateTime<Utc>>,
}

/// То, что видят PWA и HTTP: без заметки и без внутренних отметок.
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AlarmView {
    pub alarm_id: Uuid,
    pub conversation_id: Uuid,
    pub key: Option<String>,
    pub kind: String,
    pub condition: String,
    /// Разобранное условие `limit_reset` для подписей PWA на русском.
    pub provider: Option<String>,
    pub window: Option<String>,
    pub below_percent: Option<f64>,
    /// Разобранное условие `machine` для подписей PWA на русском.
    pub free_disk_gb_above: Option<f64>,
    pub load_per_core_below: Option<f64>,
    pub mem_available_gb_above: Option<f64>,
    pub session_ids: Vec<Uuid>,
    pub states: Vec<String>,
    pub fires_at: Option<String>,
    pub deadline_at: Option<String>,
    pub only_on_timeout: bool,
    pub batch: bool,
    pub state: String,
    pub fired_reason: Option<String>,
    pub failure_reason: Option<String>,
    pub fired_at: Option<String>,
    pub note: Option<String>,
    pub note_bytes: i64,
    pub created_at: String,
}

fn time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn clock(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M UTC").to_string()
}

fn coarse(duration: TimeDelta) -> String {
    let minutes = duration.num_minutes().max(0);
    if minutes < 60 {
        format!("{minutes} min")
    } else if minutes < 24 * 60 {
        format!("{} h", minutes / 60)
    } else {
        format!("{} d", minutes / (24 * 60))
    }
}

fn name_of(key: Option<&str>) -> String {
    key.filter(|key| !key.is_empty())
        .map(|key| format!("\"{key}\" "))
        .unwrap_or_default()
}

fn states_text(states: &[String]) -> String {
    states.join(", ")
}

fn condition_text(
    condition: &SessionCondition,
    deadline: DateTime<Utc>,
    only_on_timeout: bool,
    batch: bool,
) -> String {
    let ids = condition
        .session_ids
        .iter()
        .map(Uuid::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let scope = match (condition.all, batch) {
        (true, true) => ", all, batched",
        (true, false) => ", all",
        (false, true) => ", batched",
        (false, false) => "",
    };
    let watchdog = if only_on_timeout { "; watchdog" } else { "" };
    format!(
        "sessions {ids} settle in {}{scope}; deadline {}{watchdog}",
        states_text(&condition.states),
        clock(deadline)
    )
}

fn limit_condition_text(
    condition: &LimitCondition,
    deadline: DateTime<Utc>,
    only_on_timeout: bool,
) -> String {
    let below = condition
        .below_percent
        .map(|percent| format!(" or usage below {percent}%"))
        .unwrap_or_default();
    let reset = condition
        .resets_at
        .map(|at| format!(" after {}", clock(at)))
        .unwrap_or_default();
    let watchdog = if only_on_timeout { "; watchdog" } else { "" };
    format!(
        "{} {} window resets{reset}{below}; deadline {}{watchdog}",
        condition.provider,
        condition.window,
        clock(deadline)
    )
}

fn machine_condition_text(
    condition: &MachineCondition,
    deadline: DateTime<Utc>,
    only_on_timeout: bool,
) -> String {
    let mut parts = Vec::new();
    if let Some(value) = condition.free_disk_gb_above {
        parts.push(format!("free disk above {value} GB"));
    }
    if let Some(value) = condition.load_per_core_below {
        parts.push(format!("load below {value} per core"));
    }
    if let Some(value) = condition.mem_available_gb_above {
        parts.push(format!("memory available above {value} GB"));
    }
    let watchdog = if only_on_timeout { "; watchdog" } else { "" };
    format!(
        "machine: {}; deadline {}{watchdog}",
        parts.join(", "),
        clock(deadline)
    )
}

/// Разобранное условие: вид решает, что снимать при взводе и где оценивать.
enum ParsedWhen {
    Session(SessionCondition, Vec<String>),
    Limit {
        provider: String,
        window: Window,
        below_percent: Option<f64>,
    },
    Machine {
        free_disk_gb_above: Option<f64>,
        load_per_core_below: Option<f64>,
        mem_available_gb_above: Option<f64>,
    },
}

fn window_from_str(value: &str) -> Option<Window> {
    match value {
        "five_hour" => Some(Window::FiveHour),
        "weekly" => Some(Window::Weekly),
        "monthly" => Some(Window::Monthly),
        _ => None,
    }
}

fn threshold(value: Option<f64>) -> Result<Option<f64>, ToolError> {
    match value {
        Some(value) if value.is_finite() && value >= 0.0 => Ok(Some(value)),
        Some(_) => Err(ToolError::new(ToolErrorCode::InvalidArguments)),
        None => Ok(None),
    }
}

fn parse_when(when: &WhenArgs) -> Result<ParsedWhen, ToolError> {
    let invalid = || ToolError::new(ToolErrorCode::InvalidArguments);
    let machine_fields = when.free_disk_gb_above.is_some()
        || when.load_per_core_below.is_some()
        || when.mem_available_gb_above.is_some();
    let session_fields = when.session_ids.is_some() || when.states.is_some() || when.all.is_some();
    let limit_fields =
        when.provider.is_some() || when.window.is_some() || when.below_percent.is_some();
    match when.kind.as_str() {
        "session_state" => {
            if limit_fields || machine_fields {
                return Err(invalid());
            }
            let ids = when.session_ids.clone().unwrap_or_default();
            if ids.is_empty() || ids.len() > MAX_TARGETS {
                return Err(ToolError::with_message(
                    ToolErrorCode::InvalidArguments,
                    "session_state needs one to twenty session ids",
                ));
            }
            let mut seen = BTreeSet::new();
            let ids: Vec<Uuid> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
            let states = when
                .states
                .clone()
                .unwrap_or_else(|| DEFAULT_STATES.iter().map(|s| s.to_string()).collect());
            if states.is_empty()
                || states
                    .iter()
                    .any(|state| !ALL_STATES.contains(&state.as_str()))
            {
                return Err(invalid());
            }
            Ok(ParsedWhen::Session(
                SessionCondition {
                    kind: "session_state".into(),
                    session_ids: ids,
                    states: states.clone(),
                    all: when.all.unwrap_or(false),
                    armed: BTreeMap::new(),
                },
                states,
            ))
        }
        "limit_reset" => {
            if session_fields || machine_fields {
                return Err(invalid());
            }
            let provider = when.provider.clone().ok_or_else(invalid)?;
            if !crate::limits::known_provider(&provider) {
                return Err(ToolError::new(ToolErrorCode::InvalidArguments));
            }
            let window = when
                .window
                .as_deref()
                .and_then(window_from_str)
                .ok_or_else(invalid)?;
            let below_percent = match when.below_percent {
                Some(percent) if (0.0..=100.0).contains(&percent) => Some(percent),
                Some(_) => return Err(invalid()),
                None => None,
            };
            Ok(ParsedWhen::Limit {
                provider,
                window,
                below_percent,
            })
        }
        "machine" => {
            if session_fields || limit_fields {
                return Err(invalid());
            }
            let free_disk_gb_above = threshold(when.free_disk_gb_above)?;
            let load_per_core_below = threshold(when.load_per_core_below)?;
            let mem_available_gb_above = threshold(when.mem_available_gb_above)?;
            if free_disk_gb_above.is_none()
                && load_per_core_below.is_none()
                && mem_available_gb_above.is_none()
            {
                return Err(ToolError::with_message(
                    ToolErrorCode::InvalidArguments,
                    "machine needs at least one threshold",
                ));
            }
            Ok(ParsedWhen::Machine {
                free_disk_gb_above,
                load_per_core_below,
                mem_available_gb_above,
            })
        }
        other => Err(ToolError::with_message(
            ToolErrorCode::InvalidArguments,
            format!("Condition {other} is not supported yet"),
        )),
    }
}

fn validate_key(key: Option<&str>) -> Result<(), ToolError> {
    if let Some(key) = key
        && (key.is_empty() || key.len() > 64 || key.chars().any(char::is_control))
    {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }
    Ok(())
}

fn view(row: &AlarmRow) -> AlarmView {
    let kind = row
        .condition
        .as_ref()
        .and_then(|value| value.get("kind"))
        .and_then(Value::as_str);
    let parsed = if kind == Some("session_state") {
        row.condition
            .as_ref()
            .and_then(|value| serde_json::from_value::<SessionCondition>(value.clone()).ok())
    } else {
        None
    };
    let limit = if kind == Some("limit_reset") {
        row.condition
            .as_ref()
            .and_then(|value| serde_json::from_value::<LimitCondition>(value.clone()).ok())
    } else {
        None
    };
    let machine = if kind == Some("machine") {
        row.condition
            .as_ref()
            .and_then(|value| serde_json::from_value::<MachineCondition>(value.clone()).ok())
    } else {
        None
    };
    let condition = match kind {
        Some("session_state") => parsed.as_ref().and_then(|condition| {
            row.deadline_at
                .map(|deadline| condition_text(condition, deadline, row.only_on_timeout, row.batch))
        }),
        Some("limit_reset") => limit.as_ref().and_then(|condition| {
            row.deadline_at
                .map(|deadline| limit_condition_text(condition, deadline, row.only_on_timeout))
        }),
        Some("machine") => machine.as_ref().and_then(|condition| {
            row.deadline_at
                .map(|deadline| machine_condition_text(condition, deadline, row.only_on_timeout))
        }),
        _ => None,
    }
    .unwrap_or_else(|| match row.kind.as_str() {
        "at" => row
            .fire_at
            .map(|at| format!("at {}", clock(at)))
            .unwrap_or_default(),
        _ => String::new(),
    });
    AlarmView {
        alarm_id: row.id,
        conversation_id: row.conversation_id,
        key: row.key.clone(),
        kind: row.kind.clone(),
        condition,
        provider: limit.as_ref().map(|condition| condition.provider.clone()),
        window: limit.as_ref().map(|condition| condition.window.clone()),
        below_percent: limit.as_ref().and_then(|condition| condition.below_percent),
        free_disk_gb_above: machine
            .as_ref()
            .and_then(|condition| condition.free_disk_gb_above),
        load_per_core_below: machine
            .as_ref()
            .and_then(|condition| condition.load_per_core_below),
        mem_available_gb_above: machine
            .as_ref()
            .and_then(|condition| condition.mem_available_gb_above),
        session_ids: parsed
            .as_ref()
            .map(|condition| condition.session_ids.clone())
            .unwrap_or_default(),
        states: parsed
            .as_ref()
            .map(|condition| condition.states.clone())
            .unwrap_or_default(),
        fires_at: row.fire_at.map(time),
        deadline_at: row.deadline_at.map(time),
        only_on_timeout: row.only_on_timeout,
        batch: row.batch,
        state: row.state.clone(),
        fired_reason: row.fired_reason.clone(),
        failure_reason: row.failure_reason.clone(),
        fired_at: row.fired_at.map(time),
        note: row.note.clone(),
        note_bytes: row.note.as_deref().map_or(0, |note| note.len() as i64),
        created_at: time(row.created_at),
    }
}

async fn changed_in_tx(
    conn: &mut PgConnection,
    row: &AlarmRow,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: row.conversation_id,
            },
            kind: "alarm_changed",
            conversation_id: Some(row.conversation_id),
            execution_id: None,
            payload: serde_json::to_value(view(row)).unwrap_or(Value::Null),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

async fn row_in_tx(conn: &mut PgConnection, id: Uuid) -> Result<Option<AlarmRow>, sqlx::Error> {
    sqlx::query_as("select * from alarms where id=$1")
        .bind(id)
        .fetch_optional(conn)
        .await
}

async fn insert_job_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    run_at: DateTime<Utc>,
    alarm_id: Uuid,
    timeout: bool,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'session_task',$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(owner_id)
    .bind(conversation_id)
    .bind(run_at)
    .bind(json!({"action":"alarm_deliver","alarm_id":alarm_id,"timeout":timeout}))
    .execute(conn)
    .await?;
    Ok(id)
}

async fn cancel_jobs_in_tx(
    conn: &mut PgConnection,
    alarm_id: Uuid,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update scheduled_jobs set state='cancelled',result=$2,updated_at=clock_timestamp()
         where kind='session_task' and state='pending'
           and payload->>'action' in ('alarm_deliver','alarm_probe')
           and payload->>'alarm_id'=$1",
    )
    .bind(alarm_id.to_string())
    .bind(json!({"reason": reason}))
    .execute(conn)
    .await?;
    Ok(())
}

/// Задание опроса лимитов: просит провайдера снять свежее показание к сроку сброса.
async fn insert_probe_job_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    run_at: DateTime<Utc>,
    alarm_id: Uuid,
    provider: &str,
    probe_at: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'session_task',$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(owner_id)
    .bind(conversation_id)
    .bind(run_at)
    .bind(json!({
        "action": "alarm_probe",
        "alarm_id": alarm_id,
        "provider": provider,
        "probe_at": time(probe_at),
    }))
    .execute(conn)
    .await?;
    Ok(id)
}

/// Условие сработало раньше срока: задание срока больше не нужно, а задание доставки
/// этим не трогается. Текущее задание доставки не отменяется никогда: именно оно
/// доводит до конца части, которые уже сработали в этой транзакции.
async fn cancel_timeout_job_in_tx(
    conn: &mut PgConnection,
    alarm_id: Uuid,
    reason: &str,
    current_job: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update scheduled_jobs set state='cancelled',result=$2,updated_at=clock_timestamp()
         where kind='session_task' and state='pending'
           and payload->>'action'='alarm_deliver' and payload->>'alarm_id'=$1
           and coalesce((payload->>'timeout')::boolean,false)
           and ($3::uuid is null or id <> $3)",
    )
    .bind(alarm_id.to_string())
    .bind(json!({"reason": reason}))
    .bind(current_job)
    .execute(conn)
    .await?;
    Ok(())
}

/// Сработавший будильник больше не опрашивает лимиты. Текущее задание исключается:
/// если сработал сам опрос, он ещё должен завершиться как `Done`.
async fn cancel_probe_job_in_tx(
    conn: &mut PgConnection,
    alarm_id: Uuid,
    current_job: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update scheduled_jobs set state='cancelled',result=$2,updated_at=clock_timestamp()
         where kind='session_task' and state='pending'
           and payload->>'action'='alarm_probe' and payload->>'alarm_id'=$1
           and ($3::uuid is null or id <> $3)",
    )
    .bind(alarm_id.to_string())
    .bind(json!({"reason": "alarm_settled"}))
    .bind(current_job)
    .execute(conn)
    .await?;
    Ok(())
}

/// Срабатывание условия: `armed -> fired` один раз, задание доставки и событие в той же
/// транзакции. Вызывается из проектора цели под блокировкой строки будильника.
#[allow(clippy::too_many_arguments)]
pub async fn evaluate_session_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    activity: &crate::activity::Activity,
    queue: &crate::activity::QueueFacts,
    facts: &settle::SettleFacts,
    config: &settle::SettleConfig,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let Some(state) = settle::settled(activity, queue, facts, config) else {
        return Ok(false);
    };
    if !completion::is_completion(state) {
        return Ok(false);
    }
    let fingerprint =
        settle::fingerprint(state, facts.since, facts.last_turn_at, facts.phase_since);
    // Цель взведённых будильников входит в обход проектора (`activity::sweep`), поэтому
    // пропущенная под чужой блокировкой строка будет оценена на следующем тике. Терять
    // здесь нечего: в отличие от удаления цели, событие не одноразовое.
    let rows: Vec<AlarmRow> = sqlx::query_as(
        "select * from alarms
         where state='armed' and condition->>'kind'='session_state'
           and condition->'session_ids' ? $1
         order by id
         for update skip locked",
    )
    .bind(target_id.to_string())
    .fetch_all(&mut *conn)
    .await?;
    let mut changed = false;
    for mut row in rows {
        match apply_settled_in_tx(
            conn,
            &mut row,
            target_id,
            state.as_str(),
            &fingerprint,
            now,
            correlation_id,
        )
        .await?
        {
            Applied::Fired | Applied::Cancelled => changed = true,
            Applied::Held | Applied::Skipped => {}
        }
    }
    Ok(changed)
}

enum Applied {
    Skipped,
    Held,
    Fired,
    Cancelled,
}

async fn persist_collected(
    conn: &mut PgConnection,
    row: &mut AlarmRow,
    collected: &Collected,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    row.collected = collected.as_value();
    row.updated_at = now;
    sqlx::query("update alarms set collected=$2,updated_at=$3 where id=$1")
        .bind(row.id)
        .bind(&row.collected)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn apply_settled_in_tx(
    conn: &mut PgConnection,
    row: &mut AlarmRow,
    target_id: Uuid,
    state: &str,
    fingerprint: &str,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<Applied, sqlx::Error> {
    let Some(condition) =
        serde_json::from_value::<SessionCondition>(row.condition.clone().unwrap_or(Value::Null))
            .ok()
    else {
        return Ok(Applied::Skipped);
    };
    // Срок уже наступил: задание срока — единственный хозяин исхода. Устоявшаяся цель
    // после отсечки не отменяет сторож и не заменяет `timed_out` обычным срабатыванием.
    if row.deadline_at.is_some_and(|deadline| now >= deadline) {
        return Ok(Applied::Skipped);
    }
    let matching = condition.states.iter().any(|wanted| wanted == state);
    // Пачка засчитывает и удаление цели, даже если `deleted` не перечислен в `states`:
    // отдельное сообщение об удалении уже ушло обычной подпиской.
    if !matching && !(row.batch && state == "deleted") {
        return Ok(Applied::Skipped);
    }
    if condition
        .armed
        .get(&target_id.to_string())
        .is_some_and(|seen| seen == fingerprint)
    {
        return Ok(Applied::Skipped);
    }
    let mut collected = Collected::parse(&row.collected);
    let existing = collected.get(target_id).cloned();
    let owned = existing.as_ref().map(CollectedSession::owned);
    let resolved = existing
        .as_ref()
        .is_some_and(|entry| entry.satisfied && entry.owned() != Ownership::Pending);
    if !resolved {
        // `idle` и `stopped` пачка удерживает: пока владение итогом не разрешено
        // (подписка удержала его или уже отдала обычным событием), условие не засчитывается.
        // Иначе пачка сработала бы с пометкой «reported separately» о недоставленном итоге.
        let held_or_reported = if row.batch && matches!(state, "idle" | "stopped") {
            owned == Some(Ownership::Held)
                || subscription_reported_in_tx(conn, row.conversation_id, target_id, fingerprint)
                    .await?
        } else {
            true
        };
        if !held_or_reported {
            if existing.is_none() {
                collected.insert(
                    target_id,
                    CollectedSession {
                        state: state.into(),
                        fingerprint: Some(fingerprint.into()),
                        satisfied: false,
                        ownership: Some(Ownership::Pending),
                        settled_before_arming: false,
                        subscription_id: None,
                        armed_at: None,
                        chain_depth: 0,
                        boundary: None,
                    },
                );
                persist_collected(conn, row, &collected, now).await?;
            }
            return Ok(Applied::Held);
        }
        let mut entry = existing.unwrap_or(CollectedSession {
            state: state.into(),
            fingerprint: None,
            satisfied: false,
            ownership: None,
            settled_before_arming: false,
            subscription_id: None,
            armed_at: None,
            chain_depth: 0,
            boundary: None,
        });
        // Владение разрешено: удержание пачки (`held`) остаётся за ней, иначе итог
        // уходит или уже ушёл обычной подпиской (`separate`).
        entry.ownership = Some(if entry.owned() == Ownership::Held {
            Ownership::Held
        } else {
            Ownership::Separate
        });
        entry.state = state.into();
        entry.fingerprint = Some(fingerprint.into());
        entry.satisfied = true;
        collected.insert(target_id, entry);
        persist_collected(conn, row, &collected, now).await?;
    }
    let met = if condition.all {
        collected.complete(&condition)
    } else {
        collected.any_matching(&condition)
    };
    if !met {
        return Ok(Applied::Held);
    }
    if row.only_on_timeout {
        // Сторож: условие, наступившее раньше срока, отменяет будильник молча.
        cancel_in_tx(conn, row.id, "condition_met", now, correlation_id).await?;
        return Ok(Applied::Cancelled);
    }
    row.collected = collected.as_value();
    fire_in_tx(conn, row, "condition", now, now, correlation_id, true, None).await?;
    Ok(Applied::Fired)
}

enum LimitStatus {
    Met {
        trigger: &'static str,
        used_permille: Option<u16>,
    },
    NotReset,
    NoReading,
}

/// Решение по окну лимита: свежее точное показание, подтверждённый сброс или расход ниже
/// порога. Оценочное показание само по себе сброс не доказывает: нужен второй такой же
/// отсчёт после срока сброса или точное показание.
async fn limit_status_in_tx(
    conn: &mut PgConnection,
    condition: &LimitCondition,
) -> Result<LimitStatus, sqlx::Error> {
    let window = window_from_str(&condition.window)
        .ok_or_else(|| sqlx::Error::InvalidArgument("invalid window".into()))?;
    let row: Option<LimitReading> = sqlx::query_as(match window {
            Window::Weekly => "select weekly_used_permille,weekly_resets_at,weekly_observed_at,data_confidence from provider_limits where provider=$1",
            Window::FiveHour => "select five_hour_used_permille,five_hour_resets_at,five_hour_observed_at,data_confidence from provider_limits where provider=$1",
            Window::Monthly => "select monthly_used_permille,monthly_resets_at,monthly_observed_at,data_confidence from provider_limits where provider=$1",
        })
        .bind(&condition.provider)
        .fetch_optional(&mut *conn)
        .await?;
    let (used, _resets, observed, confidence) =
        row.unwrap_or((None, None, None, DataConfidence::Exact.as_str().into()));
    if let Some(below) = condition.below_percent
        && let Some(used) = used
        && observed.is_some_and(|observed| observed >= condition.armed_at)
        && f64::from(used) <= below * 10.0
    {
        return Ok(LimitStatus::Met {
            trigger: "below_percent",
            used_permille: u16::try_from(used).ok(),
        });
    }
    // Без известного срока сброса остаётся только порог расхода: угадывать сброс по
    // первому же свежему показанию нельзя.
    let Some(wake_at) = condition.resets_at else {
        return Ok(LimitStatus::NoReading);
    };
    if window == Window::FiveHour {
        let absent: Option<DateTime<Utc>> =
            sqlx::query_scalar("select five_hour_absent_at from provider_limits where provider=$1")
                .bind(&condition.provider)
                .fetch_optional(&mut *conn)
                .await?
                .flatten();
        if absent.is_some_and(|absent| absent >= wake_at) {
            return Ok(LimitStatus::Met {
                trigger: "reset",
                used_permille: None,
            });
        }
    }
    match crate::crunch::check_reset(conn, &condition.provider, window, wake_at).await? {
        ResetCheck::Verified => {
            let confidence = DataConfidence::from_stored(&confidence);
            if confidence == DataConfidence::Estimated
                && observed.is_some_and(|observed| observed >= wake_at)
            {
                let readings: i64 = sqlx::query_scalar(
                    "select count(*) from limit_snapshots
                     where provider=$1 and window_kind=$2 and observed_at >= $3",
                )
                .bind(&condition.provider)
                .bind(window.as_str())
                .bind(wake_at)
                .fetch_one(&mut *conn)
                .await?;
                if readings < 2 {
                    // Один оценочный отсчёт не доказывает сброс; его подтвердит второй
                    // или точное показание, иначе сработаем по часам.
                    return Ok(LimitStatus::NoReading);
                }
            }
            Ok(LimitStatus::Met {
                trigger: "reset",
                used_permille: used.and_then(|used| u16::try_from(used).ok()),
            })
        }
        ResetCheck::NotReset => Ok(LimitStatus::NotReset),
        ResetCheck::NoReading => Ok(LimitStatus::NoReading),
    }
}

/// Свежее показание провайдера: взведённые будильники этого окна.
pub(crate) async fn evaluate_limit_in_tx(
    conn: &mut PgConnection,
    provider: &str,
    window: Window,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let rows: Vec<AlarmRow> = sqlx::query_as(
        "select * from alarms
         where state='armed' and condition->>'kind'='limit_reset'
           and condition->>'provider'=$1 and condition->>'window'=$2
         order by id
         for update skip locked",
    )
    .bind(provider)
    .bind(window.as_str())
    .fetch_all(&mut *conn)
    .await?;
    let mut changed = false;
    for mut row in rows {
        let Some(mut condition) =
            serde_json::from_value::<LimitCondition>(row.condition.clone().unwrap_or(Value::Null))
                .ok()
        else {
            continue;
        };
        // Срок уже наступил: исход остаётся за заданием срока.
        if row.deadline_at.is_some_and(|deadline| now >= deadline) {
            continue;
        }
        let LimitStatus::Met {
            trigger,
            used_permille,
        } = limit_status_in_tx(conn, &condition).await?
        else {
            continue;
        };
        condition.trigger = Some(trigger.into());
        condition.verified = Some(trigger != "by_clock");
        condition.observed_used_permille = used_permille;
        row.condition = Some(serde_json::to_value(&condition).unwrap_or(Value::Null));
        if row.only_on_timeout {
            cancel_in_tx(conn, row.id, "condition_met", now, correlation_id).await?;
        } else {
            fire_in_tx(
                conn,
                &mut row,
                "condition",
                now,
                now,
                correlation_id,
                true,
                None,
            )
            .await?;
        }
        changed = true;
    }
    Ok(changed)
}

fn machine_reading(metrics: &MachineMetrics) -> (Option<f64>, Option<f64>, Option<f64>) {
    let free_disk_gb = metrics
        .disks
        .iter()
        .map(|disk| disk.free_bytes as f64 / BYTES_PER_GB)
        .reduce(f64::min);
    let load_per_core = (metrics.cpu_cores > 0)
        .then(|| f64::from(metrics.load1_centi) / 100.0 / f64::from(metrics.cpu_cores));
    let mem_available_gb = Some(metrics.mem_available_bytes as f64 / BYTES_PER_GB);
    (free_disk_gb, load_per_core, mem_available_gb)
}

fn machine_holds(
    condition: &MachineCondition,
    free_disk_gb: Option<f64>,
    load_per_core: Option<f64>,
    mem_available_gb: Option<f64>,
) -> bool {
    condition
        .free_disk_gb_above
        .is_none_or(|threshold| free_disk_gb.is_some_and(|value| value >= threshold))
        && condition
            .load_per_core_below
            .is_none_or(|threshold| load_per_core.is_some_and(|value| value < threshold))
        && condition
            .mem_available_gb_above
            .is_none_or(|threshold| mem_available_gb.is_some_and(|value| value >= threshold))
}

/// Свежие метрики машины: взведённые будильники этой машины. Показания старше
/// `MACHINE_METRICS_MAX_AGE` и снятые до взвода условие не выполняют.
pub(crate) async fn evaluate_machine_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    metrics: &MachineMetrics,
    observed_at: DateTime<Utc>,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let rows: Vec<AlarmRow> = sqlx::query_as(
        "select * from alarms
         where state='armed' and condition->>'kind'='machine'
           and condition->>'machine_id'=$1
         order by id
         for update skip locked",
    )
    .bind(machine_id.to_string())
    .fetch_all(&mut *conn)
    .await?;
    let (free_disk_gb, load_per_core, mem_available_gb) = machine_reading(metrics);
    let mut changed = false;
    for mut row in rows {
        let Some(mut condition) = serde_json::from_value::<MachineCondition>(
            row.condition.clone().unwrap_or(Value::Null),
        )
        .ok() else {
            continue;
        };
        if row.deadline_at.is_some_and(|deadline| now >= deadline)
            || observed_at < condition.armed_at
            || observed_at < now - MACHINE_METRICS_MAX_AGE
        {
            continue;
        }
        if !machine_holds(&condition, free_disk_gb, load_per_core, mem_available_gb) {
            continue;
        }
        condition.observed_free_disk_gb = free_disk_gb;
        condition.observed_load_per_core = load_per_core;
        condition.observed_mem_available_gb = mem_available_gb;
        row.condition = Some(serde_json::to_value(&condition).unwrap_or(Value::Null));
        if row.only_on_timeout {
            cancel_in_tx(conn, row.id, "condition_met", now, correlation_id).await?;
        } else {
            fire_in_tx(
                conn,
                &mut row,
                "condition",
                now,
                now,
                correlation_id,
                true,
                None,
            )
            .await?;
        }
        changed = true;
    }
    Ok(changed)
}

/// Задание `alarm_probe`: к `resets_at + reset_grace` просит опрос провайдера, повторяет
/// его до окна проверки и без свежего показания срабатывает по часам (`by_clock`).
pub(crate) async fn probe_job_in_tx(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<crate::crunch::JobOutcome, sqlx::Error> {
    let Some(alarm_id) = job
        .payload
        .get("alarm_id")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<Uuid>().ok())
    else {
        return Ok(crate::crunch::JobOutcome::Cancelled("invalid_payload"));
    };
    let Some(conversation_id) = job.conversation_id else {
        return Ok(crate::crunch::JobOutcome::Cancelled("invalid_payload"));
    };
    let probe_at = job
        .payload
        .get("probe_at")
        .and_then(Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|at| at.with_timezone(&Utc))
        .unwrap_or(now);
    let row: Option<AlarmRow> =
        sqlx::query_as("select * from alarms where id=$1 and conversation_id=$2 for update")
            .bind(alarm_id)
            .bind(conversation_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(mut row) = row else {
        return Ok(crate::crunch::JobOutcome::Cancelled("alarm_settled"));
    };
    if row.state != "armed" {
        return Ok(crate::crunch::JobOutcome::Cancelled("alarm_settled"));
    }
    if row.deadline_at.is_some_and(|deadline| now >= deadline) {
        return Ok(crate::crunch::JobOutcome::Cancelled("alarm_settled"));
    }
    let Some(mut condition) =
        serde_json::from_value::<LimitCondition>(row.condition.clone().unwrap_or(Value::Null)).ok()
    else {
        return Ok(crate::crunch::JobOutcome::Cancelled("invalid_payload"));
    };
    let status = limit_status_in_tx(conn, &condition).await?;
    let trigger = match status {
        LimitStatus::Met { trigger, .. } => Some(trigger),
        LimitStatus::NoReading if now >= probe_at + PROBE_VERIFY_WINDOW => Some("by_clock"),
        LimitStatus::NoReading | LimitStatus::NotReset => None,
    };
    if let Some(trigger) = trigger {
        condition.trigger = Some(trigger.into());
        condition.verified = Some(trigger != "by_clock");
        row.condition = Some(serde_json::to_value(&condition).unwrap_or(Value::Null));
        if row.only_on_timeout {
            cancel_in_tx(conn, row.id, "condition_met", now, correlation_id).await?;
            return Ok(crate::crunch::JobOutcome::Done(
                json!({"cancelled": "condition_met"}),
            ));
        }
        let reason = if trigger == "by_clock" {
            "by_clock"
        } else {
            "condition"
        };
        fire_in_tx(
            conn,
            &mut row,
            reason,
            now,
            now,
            correlation_id,
            true,
            Some(job.id),
        )
        .await?;
        return Ok(crate::crunch::JobOutcome::Done(
            json!({"alarm_id": row.id, "fired": reason}),
        ));
    }
    Ok(crate::crunch::JobOutcome::Retry {
        at: now + PROBE_RETRY,
        poll: true,
    })
}

/// Подписка уже отдала это устоявшееся состояние обычным событием: отпечаток совпал.
/// Обычное терминальное закрытие (`idle`/`stopped`/`failed`) не мешает признать это:
/// будильник мог быть заперт в момент оценки, а подписка успела закрыться сама.
/// Отписка и приём закрывают подписку своими причинами и итог не воскрешают.
async fn subscription_reported_in_tx(
    conn: &mut PgConnection,
    subscriber_id: Uuid,
    target_id: Uuid,
    fingerprint: &str,
) -> Result<bool, sqlx::Error> {
    let reported: Option<bool> = sqlx::query_scalar(
        "select last_fingerprint is not distinct from $3 from session_subscriptions
         where subscriber_id=$1 and target_id=$2
           and (closed_at is null or closed_reason in ('idle','stopped','failed'))
         order by armed_at desc limit 1",
    )
    .bind(subscriber_id)
    .bind(target_id)
    .bind(fingerprint)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(reported.unwrap_or(false))
}

/// Удержание итога пачкой: первая взведённая пачка, у которой этой цели ещё нет,
/// забирает отпечаток и запоминает подписку с поколением взвода. Ожидающая цель
/// (`pending`) получает владение здесь, когда строка подписки освободилась.
#[allow(clippy::too_many_arguments)]
pub async fn hold_for_batch_in_tx(
    conn: &mut PgConnection,
    subscriber_id: Uuid,
    target_id: Uuid,
    state: &str,
    fingerprint: &str,
    subscription_id: Uuid,
    armed_at: DateTime<Utc>,
    boundary: Option<&summary::Boundary>,
    chain_depth: i32,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    if !matches!(state, "idle" | "stopped") {
        return Ok(false);
    }
    let rows: Vec<AlarmRow> = sqlx::query_as(
        "select * from alarms
         where state='armed' and batch and condition->>'kind'='session_state'
           and conversation_id=$1 and condition->'session_ids' ? $2
         order by created_at,id
         for update skip locked",
    )
    .bind(subscriber_id)
    .bind(target_id.to_string())
    .fetch_all(&mut *conn)
    .await?;
    for mut row in rows {
        let Some(condition) = serde_json::from_value::<SessionCondition>(
            row.condition.clone().unwrap_or(Value::Null),
        )
        .ok() else {
            continue;
        };
        if !condition.all || !condition.states.iter().any(|wanted| wanted == state) {
            continue;
        }
        let mut collected = Collected::parse(&row.collected);
        if collected
            .get(target_id)
            .is_some_and(|entry| entry.owned() != Ownership::Pending)
        {
            continue;
        }
        collected.insert(
            target_id,
            CollectedSession {
                state: state.into(),
                fingerprint: Some(fingerprint.into()),
                satisfied: true,
                ownership: Some(Ownership::Held),
                settled_before_arming: false,
                subscription_id: Some(subscription_id),
                armed_at: Some(armed_at),
                chain_depth,
                boundary: boundary.cloned(),
            },
        );
        persist_collected(conn, &mut row, &collected, now).await?;
        return Ok(true);
    }
    Ok(false)
}

/// Цель удалена: условие с `deleted` срабатывает, пачка считает её собранной,
/// сторож молчит, если условие выполнено. Удаление одноразовое: строки будильников
/// берутся блокирующе (в порядке id), иначе запертый будильник пропустил бы событие.
pub async fn on_target_deleted_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let rows: Vec<AlarmRow> = sqlx::query_as(
        "select * from alarms
         where state='armed' and condition->>'kind'='session_state'
           and condition->'session_ids' ? $1
         order by id
         for update",
    )
    .bind(target_id.to_string())
    .fetch_all(&mut *conn)
    .await?;
    for mut row in rows {
        let _ = apply_settled_in_tx(
            conn,
            &mut row,
            target_id,
            "deleted",
            "deleted",
            now,
            Uuid::now_v7(),
        )
        .await?;
    }
    Ok(())
}

/// Переход в `fired`: состояние, причина, задание доставки и событие для PWA.
#[allow(clippy::too_many_arguments)]
async fn fire_in_tx(
    conn: &mut PgConnection,
    row: &mut AlarmRow,
    reason: &str,
    fired_at: DateTime<Utc>,
    now: DateTime<Utc>,
    correlation_id: Uuid,
    schedule: bool,
    current_job: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let condition = row.condition.clone();
    sqlx::query(
        "update alarms set state='fired',fired_reason=$2,fired_at=$3,collected=$4,
           condition=$5,updated_at=$6 where id=$1",
    )
    .bind(row.id)
    .bind(reason)
    .bind(fired_at)
    .bind(&row.collected)
    .bind(&row.condition)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    row.state = "fired".into();
    row.fired_reason = Some(reason.into());
    row.fired_at = Some(fired_at);
    row.updated_at = now;
    if condition.is_some() {
        // Условие уже сработало: задание срока больше не нужно. Текущее задание
        // доставки исключено: оно ещё должно доставить остаток частей.
        cancel_timeout_job_in_tx(conn, row.id, "alarm_settled", current_job).await?;
        cancel_probe_job_in_tx(conn, row.id, current_job).await?;
    }
    if schedule {
        insert_job_in_tx(
            conn,
            row.owner_id,
            row.conversation_id,
            now + COALESCE,
            row.id,
            false,
        )
        .await?;
    }
    changed_in_tx(conn, row, correlation_id).await?;
    Ok(())
}

/// `alarm_set`: одна транзакция с очередью вызывающего, пределом и заменой по `key`.
pub async fn set(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: SetArgs = parse_arguments(arguments)?;
    if !context.alarms_enabled {
        return Err(ToolError::with_message(
            ToolErrorCode::InvalidArguments,
            "Alarms are disabled on this hub",
        ));
    }
    validate_key(args.key.as_deref())?;
    if args
        .note
        .as_deref()
        .is_some_and(|note| note.len() > MAX_NOTE_BYTES)
    {
        return Err(ToolError::new(ToolErrorCode::TooLarge));
    }
    let only_on_timeout = args.only_on_timeout.unwrap_or(false);
    let batch = args.batch.unwrap_or(false);
    let time_sources = usize::from(args.at.is_some())
        + usize::from(args.in_seconds.is_some())
        + usize::from(args.when.is_some());
    if time_sources != 1
        || (args.when.is_none() && (args.deadline_seconds.is_some() || only_on_timeout || batch))
    {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }

    let now = Utc::now();
    let mut tx = context.pool.begin().await?;
    send::lock_sender(&mut tx, caller.conversation_id).await?;
    if let Some(stored) = sqlx::query_scalar::<_, Value>(
        "select response from alarms where owner_id=$1 and call_id=$2",
    )
    .bind(caller.owner_id)
    .bind(caller.call_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        tx.commit().await?;
        return Ok(stored);
    }
    // Порядок блокировок (docs/design/delivery.md): строки `conversations` по возрастанию ID
    // берутся до правки старого будильника. Иначе замена держит будильник и ждёт разговор,
    // пока удаление цели держит разговор и ждёт будильник.
    let when = args.when.as_ref().map(parse_when).transpose()?;
    let mut conversation_ids = vec![caller.conversation_id];
    if let Some(ParsedWhen::Session(condition, _)) = &when {
        conversation_ids.extend(condition.session_ids.iter().copied());
    }
    conversation_ids.sort_unstable();
    conversation_ids.dedup();
    for id in conversation_ids {
        access::session_in_tx(&mut tx, caller.owner_id, id).await?;
    }
    let waiter = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;

    let replaced = if let Some(key) = args.key.as_deref() {
        let old: Option<Uuid> = sqlx::query_scalar(
            "select id from alarms where conversation_id=$1 and key=$2 and state='armed'
             for update",
        )
        .bind(caller.conversation_id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(old) = old {
            cancel_in_tx(&mut tx, old, "replaced", now, caller.call_id).await?;
        }
        old
    } else {
        None
    };
    let armed: i64 = sqlx::query_scalar(
        "select count(*) from alarms where conversation_id=$1 and state='armed'",
    )
    .bind(caller.conversation_id)
    .fetch_one(&mut *tx)
    .await?;
    if armed >= MAX_ARMED {
        return Err(ToolError::new(ToolErrorCode::TooManyAlarms));
    }
    let chain_depth = access::current_depth(&mut tx, caller.conversation_id).await?;

    let grace = TimeDelta::from_std(context.limits.reset_grace)
        .map_err(|_| ToolError::new(ToolErrorCode::InvalidArguments))?;
    let (kind, fire_at, deadline_at, condition, collected, response, run_at, timeout, probe) =
        if let Some(at) = args.at.as_deref() {
            let at = DateTime::parse_from_rfc3339(at)
                .map_err(|_| ToolError::new(ToolErrorCode::InvalidArguments))?
                .with_timezone(&Utc);
            let seconds = (at - now).num_seconds();
            if !(MIN_AT_SECONDS..=MAX_AT_SECONDS).contains(&seconds) {
                return Err(ToolError::new(ToolErrorCode::InvalidArguments));
            }
            let response = json!({
                "state": "armed",
                "alarm_id": Uuid::now_v7(),
                "fires_at": time(at),
                "condition": format!("at {}", clock(at)),
                "replaced": replaced,
                "hint": HINT_ARMED,
            });
            (
                "at",
                Some(at),
                None,
                None,
                Collected::default(),
                response,
                at + COALESCE,
                false,
                None,
            )
        } else if let Some(seconds) = args.in_seconds {
            if !(MIN_AT_SECONDS..=MAX_AT_SECONDS).contains(&seconds) {
                return Err(ToolError::new(ToolErrorCode::InvalidArguments));
            }
            let at = now + TimeDelta::seconds(seconds);
            let response = json!({
                "state": "armed",
                "alarm_id": Uuid::now_v7(),
                "fires_at": time(at),
                "condition": format!("at {}", clock(at)),
                "replaced": replaced,
                "hint": HINT_ARMED,
            });
            (
                "at",
                Some(at),
                None,
                None,
                Collected::default(),
                response,
                at + COALESCE,
                false,
                None,
            )
        } else {
            let parsed = when.ok_or_else(|| ToolError::new(ToolErrorCode::InvalidArguments))?;
            if batch && !matches!(parsed, ParsedWhen::Session(ref condition, _) if condition.all) {
                return Err(ToolError::with_message(
                    ToolErrorCode::InvalidArguments,
                    "batch needs session_state with all=true",
                ));
            }
            let deadline_seconds = args.deadline_seconds.unwrap_or(DEFAULT_DEADLINE_SECONDS);
            if !(MIN_DEADLINE_SECONDS..=MAX_DEADLINE_SECONDS).contains(&deadline_seconds) {
                return Err(ToolError::new(ToolErrorCode::InvalidArguments));
            }
            let deadline = now + TimeDelta::seconds(deadline_seconds);
            match parsed {
                ParsedWhen::Session(mut condition, states) => {
                    let mut current_states: Vec<(Uuid, String)> = Vec::new();
                    let mut all_met = true;
                    let mut any_met = false;
                    for target in condition.session_ids.clone() {
                        if target == caller.conversation_id {
                            return Err(ToolError::new(ToolErrorCode::SessionNotFound));
                        }
                        access::target_in_tx(
                            &mut tx,
                            &context.config,
                            &waiter,
                            caller.owner_id,
                            target,
                        )
                        .await?;
                        if let Some(current) =
                            subs::current_in_tx(&mut tx, target, now, &context.settle).await?
                        {
                            let met = states.iter().any(|state| state == current.state.as_str());
                            any_met |= met;
                            all_met &= met;
                            condition
                                .armed
                                .insert(target.to_string(), current.fingerprint.clone());
                            current_states.push((target, current.state.as_str().to_string()));
                        } else {
                            all_met = false;
                        }
                    }
                    let satisfied = if condition.all { all_met } else { any_met };
                    if satisfied {
                        // Условие уже истинно: будильник не взводится, но вызов запоминается.
                        let current = if condition.session_ids.len() == 1 {
                            json!(current_states[0].1)
                        } else {
                            json!(
                                current_states
                                    .iter()
                                    .map(|(id, state)| json!({"session_id": id, "state": state}))
                                    .collect::<Vec<_>>()
                            )
                        };
                        let response = json!({
                            "state": "satisfied",
                            "current": current,
                            "condition": condition_text(&condition, deadline, only_on_timeout, batch),
                            "hint": HINT_SATISFIED,
                        });
                        let id = Uuid::now_v7();
                        insert_alarm_in_tx(
                            &mut tx,
                            id,
                            caller.owner_id,
                            caller.conversation_id,
                            "agent",
                            args.key.as_deref(),
                            "condition",
                            None,
                            Some(deadline),
                            only_on_timeout,
                            batch,
                            Some(condition.as_value()),
                            Collected::default(),
                            args.note.as_deref(),
                            chain_depth,
                            "cancelled",
                            Some("satisfied"),
                            None,
                            caller.call_id,
                            &response,
                            now,
                        )
                        .await?;
                        tx.commit().await?;
                        return Ok(response);
                    }
                    let mut collected = Collected::default();
                    if condition.all {
                        // Цель, уже осевшая до взвода, входит в итог `all`, и для пачки тоже:
                        // её отпечаток больше не изменится, поэтому без этого пачка ждала бы срока.
                        // Итог `idle`/`stopped` подписка уже отдала до взвода — об этом говорит строка.
                        for (target, state) in &current_states {
                            if !states.iter().any(|wanted| wanted == state) {
                                continue;
                            }
                            collected.insert(
                                *target,
                                CollectedSession {
                                    state: state.clone(),
                                    fingerprint: condition.armed.get(&target.to_string()).cloned(),
                                    satisfied: true,
                                    ownership: Some(Ownership::Separate),
                                    settled_before_arming: matches!(
                                        state.as_str(),
                                        "idle" | "stopped"
                                    ),
                                    subscription_id: None,
                                    armed_at: None,
                                    chain_depth,
                                    boundary: None,
                                },
                            );
                        }
                    }
                    if batch {
                        for target in condition.session_ids.clone() {
                            ensure_subscription_in_tx(
                                &mut tx,
                                caller.owner_id,
                                caller.conversation_id,
                                target,
                                chain_depth,
                                now,
                                &context.settle,
                            )
                            .await?;
                        }
                    }
                    let response = json!({
                        "state": "armed",
                        "alarm_id": Uuid::now_v7(),
                        "deadline_at": time(deadline),
                        "condition": condition_text(&condition, deadline, only_on_timeout, batch),
                        "replaced": replaced,
                        "hint": HINT_ARMED,
                    });
                    (
                        "condition",
                        None,
                        Some(deadline),
                        Some(condition.as_value()),
                        collected,
                        response,
                        deadline + COALESCE,
                        true,
                        None,
                    )
                }
                ParsedWhen::Limit {
                    provider,
                    window,
                    below_percent,
                } => {
                    let (resets_at, used): (Option<DateTime<Utc>>, Option<i32>) = sqlx::query_as(
                        match window {
                            Window::Weekly => "select weekly_resets_at,weekly_used_permille from provider_limits where provider=$1",
                            Window::FiveHour => "select five_hour_resets_at,five_hour_used_permille from provider_limits where provider=$1",
                            Window::Monthly => "select monthly_resets_at,monthly_used_permille from provider_limits where provider=$1",
                        },
                    )
                    .bind(&provider)
                    .fetch_optional(&mut *tx)
                    .await?
                    .unwrap_or((None, None));
                    if resets_at.is_none() && below_percent.is_none() {
                        return Err(ToolError::with_message(
                            ToolErrorCode::InvalidArguments,
                            "limit_reset needs a known resets_at or below_percent: the provider has not reported the next reset",
                        ));
                    }
                    let condition = LimitCondition {
                        kind: "limit_reset".into(),
                        provider: provider.clone(),
                        window: window.as_str().into(),
                        below_percent,
                        armed_at: now,
                        resets_at,
                        used_permille: used.and_then(|used| u16::try_from(used).ok()),
                        trigger: None,
                        verified: None,
                        observed_used_permille: None,
                    };
                    let response = json!({
                        "state": "armed",
                        "alarm_id": Uuid::now_v7(),
                        "deadline_at": time(deadline),
                        "condition": limit_condition_text(&condition, deadline, only_on_timeout),
                        "replaced": replaced,
                        "hint": HINT_ARMED,
                    });
                    let probe_at = resets_at.map(|reset| reset + grace);
                    (
                        "condition",
                        None,
                        Some(deadline),
                        Some(serde_json::to_value(&condition).unwrap_or(Value::Null)),
                        Collected::default(),
                        response,
                        deadline + COALESCE,
                        true,
                        probe_at.map(|at| (at, provider)),
                    )
                }
                ParsedWhen::Machine {
                    free_disk_gb_above,
                    load_per_core_below,
                    mem_available_gb_above,
                } => {
                    let machine_id: Option<Uuid> =
                        sqlx::query_scalar("select machine_id from conversations where id=$1")
                            .bind(caller.conversation_id)
                            .fetch_one(&mut *tx)
                            .await?;
                    let Some(machine_id) = machine_id else {
                        return Err(ToolError::new(ToolErrorCode::MachineUnavailable));
                    };
                    let condition = MachineCondition {
                        kind: "machine".into(),
                        machine_id,
                        free_disk_gb_above,
                        load_per_core_below,
                        mem_available_gb_above,
                        armed_at: now,
                        observed_free_disk_gb: None,
                        observed_load_per_core: None,
                        observed_mem_available_gb: None,
                    };
                    let response = json!({
                        "state": "armed",
                        "alarm_id": Uuid::now_v7(),
                        "deadline_at": time(deadline),
                        "condition": machine_condition_text(&condition, deadline, only_on_timeout),
                        "replaced": replaced,
                        "hint": HINT_ARMED,
                    });
                    (
                        "condition",
                        None,
                        Some(deadline),
                        Some(serde_json::to_value(&condition).unwrap_or(Value::Null)),
                        Collected::default(),
                        response,
                        deadline + COALESCE,
                        true,
                        None,
                    )
                }
            }
        };

    let id = response["alarm_id"]
        .as_str()
        .and_then(|value| value.parse::<Uuid>().ok())
        .unwrap_or_else(Uuid::now_v7);
    insert_alarm_in_tx(
        &mut tx,
        id,
        caller.owner_id,
        caller.conversation_id,
        "agent",
        args.key.as_deref(),
        kind,
        fire_at,
        deadline_at,
        only_on_timeout,
        batch,
        condition,
        collected,
        args.note.as_deref(),
        chain_depth,
        "armed",
        None,
        None,
        caller.call_id,
        &response,
        now,
    )
    .await?;
    insert_job_in_tx(
        &mut tx,
        caller.owner_id,
        caller.conversation_id,
        run_at,
        id,
        timeout,
    )
    .await?;
    if let Some((probe_at, provider)) = probe {
        insert_probe_job_in_tx(
            &mut tx,
            caller.owner_id,
            caller.conversation_id,
            probe_at.max(now),
            id,
            &provider,
            probe_at,
        )
        .await?;
    }
    if let Some(row) = row_in_tx(&mut tx, id).await? {
        changed_in_tx(&mut tx, &row, caller.call_id).await?;
    }
    tx.commit().await?;
    crate::scheduler::notify_pool(&context.pool);
    Ok(response)
}

impl SessionCondition {
    fn as_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[allow(clippy::too_many_arguments)]
async fn ensure_subscription_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    subscriber_id: Uuid,
    target_id: Uuid,
    chain_depth: i32,
    now: DateTime<Utc>,
    config: &settle::SettleConfig,
) -> Result<(), sqlx::Error> {
    let open: Option<Uuid> = sqlx::query_scalar(
        "select id from session_subscriptions
         where subscriber_id=$1 and target_id=$2 and closed_at is null",
    )
    .bind(subscriber_id)
    .bind(target_id)
    .fetch_optional(&mut *conn)
    .await?;
    if open.is_none() {
        subs::subscribe_explicit_in_tx(
            conn,
            owner_id,
            subscriber_id,
            target_id,
            chain_depth,
            now,
            config,
        )
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_alarm_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
    owner_id: Uuid,
    conversation_id: Uuid,
    created_by: &str,
    key: Option<&str>,
    kind: &str,
    fire_at: Option<DateTime<Utc>>,
    deadline_at: Option<DateTime<Utc>>,
    only_on_timeout: bool,
    batch: bool,
    condition: Option<Value>,
    collected: Collected,
    note: Option<&str>,
    chain_depth: i32,
    state: &str,
    fired_reason: Option<&str>,
    failure_reason: Option<&str>,
    call_id: Uuid,
    response: &Value,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into alarms(id,owner_id,conversation_id,created_by,key,kind,condition,fire_at,
           deadline_at,only_on_timeout,batch,collected,note,chain_depth,state,fired_reason,
           failure_reason,call_id,response,created_at,updated_at)
         values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$20)",
    )
    .bind(id)
    .bind(owner_id)
    .bind(conversation_id)
    .bind(created_by)
    .bind(key)
    .bind(kind)
    .bind(condition)
    .bind(fire_at)
    .bind(deadline_at)
    .bind(only_on_timeout)
    .bind(batch)
    .bind(collected.as_value())
    .bind(note)
    .bind(chain_depth)
    .bind(state)
    .bind(fired_reason)
    .bind(failure_reason)
    .bind(call_id)
    .bind(response)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(())
}

/// Отпускание удержанного пачкой: итог возвращается подписке, только если она жива
/// и того же поколения взвода. Иначе он устарел и отбрасывается.
async fn release_collected_in_tx(
    conn: &mut PgConnection,
    row: &AlarmRow,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    if !row.batch {
        return Ok(());
    }
    let collected = Collected::parse(&row.collected);
    for (target, entry) in &collected.sessions {
        let Ok(target_id) = target.parse::<Uuid>() else {
            continue;
        };
        let (Some(subscription_id), Some(armed_at), Some(fingerprint)) = (
            entry.subscription_id,
            entry.armed_at,
            entry.fingerprint.as_deref(),
        ) else {
            continue;
        };
        subs::release_held_in_tx(
            conn,
            row.owner_id,
            row.conversation_id,
            target_id,
            subscription_id,
            armed_at,
            &entry.state,
            fingerprint,
            entry.chain_depth,
            entry.boundary.as_ref(),
            now,
        )
        .await?;
    }
    Ok(())
}

async fn cancel_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
    reason: &str,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let row: Option<AlarmRow> = sqlx::query_as(
        "update alarms set state='cancelled',fired_reason=$2,updated_at=$3
         where id=$1 and state='armed' returning *",
    )
    .bind(id)
    .bind(reason)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?;
    cancel_jobs_in_tx(conn, id, "alarm_cancelled").await?;
    if let Some(row) = row {
        release_collected_in_tx(conn, &row, now).await?;
        changed_in_tx(conn, &row, correlation_id).await?;
    }
    Ok(())
}

/// `alarm_cancel`: идемпотентно по id или имени.
pub async fn cancel(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: CancelArgs = parse_arguments(arguments)?;
    if args.alarm_id.is_some() == args.key.is_some() {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }
    let now = Utc::now();
    let mut tx = context.pool.begin().await?;
    send::lock_sender(&mut tx, caller.conversation_id).await?;
    let row: Option<AlarmRow> = if let Some(id) = args.alarm_id {
        sqlx::query_as("select * from alarms where id=$1 and conversation_id=$2 for update")
            .bind(id)
            .bind(caller.conversation_id)
            .fetch_optional(&mut *tx)
            .await?
    } else {
        let key = args.key.as_deref();
        let armed: Option<AlarmRow> = sqlx::query_as(
            "select * from alarms where conversation_id=$1 and key=$2 and state='armed'
             for update",
        )
        .bind(caller.conversation_id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        match armed {
            Some(row) => Some(row),
            None => {
                sqlx::query_as(
                    "select * from alarms where conversation_id=$1 and key=$2
                     order by created_at desc limit 1 for update",
                )
                .bind(caller.conversation_id)
                .bind(key)
                .fetch_optional(&mut *tx)
                .await?
            }
        }
    };
    let Some(row) = row else {
        return Err(ToolError::new(ToolErrorCode::AlarmNotFound));
    };
    let result = match row.state.as_str() {
        "armed" => {
            cancel_in_tx(&mut tx, row.id, "cancelled", now, caller.call_id).await?;
            "cancelled"
        }
        "cancelled" => "already_cancelled",
        _ => "already_fired",
    };
    let updated = row_in_tx(&mut tx, row.id).await?;
    tx.commit().await?;
    Ok(json!({
        "result": result,
        "alarm": updated.as_ref().map(view),
    }))
}

/// Отмена владельцем из PWA: чужой будильник неотличим от несуществующего.
pub async fn cancel_owner_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    alarm_id: Uuid,
    correlation_id: Uuid,
) -> Result<AlarmView, ToolError> {
    let now = Utc::now();
    let row: Option<AlarmRow> = sqlx::query_as(
        "select * from alarms where id=$1 and owner_id=$2 and conversation_id=$3 for update",
    )
    .bind(alarm_id)
    .bind(owner_id)
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Err(ToolError::new(ToolErrorCode::AlarmNotFound));
    };
    if row.state == "armed" {
        cancel_in_tx(conn, row.id, "cancelled", now, correlation_id).await?;
    }
    let updated = row_in_tx(conn, row.id).await?.unwrap_or(row);
    Ok(view(&updated))
}

/// `alarm_list`: только свои будильники; заметка не отдаётся, её уже знает модель.
pub async fn list(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: ListArgs = parse_arguments(arguments)?;
    let include_finished = args.include_finished.unwrap_or(false);
    let mut conn = context.pool.acquire().await?;
    access::caller_in_tx(&mut conn, caller.owner_id, caller.conversation_id).await?;
    let rows = list_rows(
        &mut conn,
        caller.owner_id,
        caller.conversation_id,
        include_finished,
        20,
    )
    .await?;
    Ok(json!({
        "alarms": rows.iter().map(view).collect::<Vec<_>>(),
    }))
}

pub async fn list_rows(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    include_finished: bool,
    limit: i64,
) -> Result<Vec<AlarmRow>, sqlx::Error> {
    if include_finished {
        sqlx::query_as(
            "select * from alarms where owner_id=$1 and conversation_id=$2
             order by created_at desc limit $3",
        )
        .bind(owner_id)
        .bind(conversation_id)
        .bind(limit.min(100))
        .fetch_all(conn)
        .await
    } else {
        sqlx::query_as(
            "select * from alarms where owner_id=$1 and conversation_id=$2 and state='armed'
             order by coalesce(fire_at,deadline_at) limit $3",
        )
        .bind(owner_id)
        .bind(conversation_id)
        .bind(limit.min(100))
        .fetch_all(conn)
        .await
    }
}

/// Снимок разговора: взведённые и недавние завершившиеся будильники.
pub async fn snapshot_rows(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Vec<AlarmRow>, sqlx::Error> {
    sqlx::query_as(
        "select * from alarms
         where conversation_id=$1
           and (state='armed' or (state in ('fired','failed','expired')
                and updated_at > clock_timestamp() - interval '30 days'))
         order by state='armed' desc, coalesce(fire_at,deadline_at) nulls last, created_at
         limit 50",
    )
    .bind(conversation_id)
    .fetch_all(conn)
    .await
}

pub fn snapshot_views(rows: &[AlarmRow]) -> Vec<AlarmView> {
    rows.iter().map(view).collect()
}

/// Задание доставки: сливает все сработавшие и не доставленные будильники получателя
/// в один ввод. Дедупликация идёт по будильнику: у него один `delivered_input_id`.
pub async fn deliver_job_in_tx(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<crate::crunch::JobOutcome, SubmitError> {
    let Some(conversation) = job.conversation_id else {
        return Ok(crate::crunch::JobOutcome::Cancelled("invalid_payload"));
    };
    // Порядок блокировок: очередь получателя раньше подписок и будильников. Приём
    // результата держит очередь и закрывает подписку, поэтому обратный порядок даёт цикл.
    send::lock_queue(conn, conversation).await?;
    // Отложенная финализация: строка будильника была заперта в прошлый раз. Повторяем
    // именно отказ, а не обычную доставку, иначе терпение теряет смысл.
    let deferred: Option<String> =
        sqlx::query_scalar("select result->>'deferred_failure' from scheduled_jobs where id=$1")
            .bind(job.id)
            .fetch_one(&mut *conn)
            .await?;
    if let Some(reason) = deferred {
        return match fail_delivery_in_tx(conn, conversation, &job.payload, &reason, now).await? {
            Finalization::Finalized => {
                Ok(crate::crunch::JobOutcome::Cancelled("delivery_contended"))
            }
            Finalization::Deferred => Ok(crate::crunch::JobOutcome::Retry {
                at: now + TimeDelta::seconds(2),
                poll: false,
            }),
        };
    }
    let mut rows: Vec<AlarmRow> = sqlx::query_as(
        "select * from alarms where conversation_id=$1 and (
             (state='fired' and delivered_input_id is null)
             or (state='armed' and kind='at' and fire_at <= $2)
             or (state='armed' and kind='condition' and deadline_at <= $2)
         )
         order by coalesce(fire_at,deadline_at), id
         for update skip locked",
    )
    .bind(conversation)
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    // Пропущенная под чужой блокировкой строка не должна сгореть: доставка одноразовая.
    // Считаем всё, что ещё ждёт доставки, и просим обычный повтор планировщика.
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from alarms where conversation_id=$1 and (
             (state='fired' and delivered_input_id is null)
             or (state='armed' and kind='at' and fire_at <= $2)
             or (state='armed' and kind='condition' and deadline_at <= $2)
         )",
    )
    .bind(conversation)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    if pending as usize > rows.len() {
        // Строка под чужой блокировкой: терпение считается как у переполненной очереди,
        // с первой такой попытки. Иначе запертая строка откладывала бы доставку вечно.
        let blocked_since: Option<Option<DateTime<Utc>>> =
            sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
                "select blocked_since from scheduled_jobs where id=$1",
            )
            .bind(job.id)
            .fetch_optional(&mut *conn)
            .await?;
        let blocked_since = blocked_since.flatten();
        let since = blocked_since.unwrap_or(now);
        if now - since >= crate::scheduler::SESSION_TASK_QUEUE_PATIENCE {
            match fail_delivery_in_tx(conn, conversation, &job.payload, "delivery_contended", now)
                .await?
            {
                Finalization::Finalized => {
                    return Ok(crate::crunch::JobOutcome::Cancelled("delivery_contended"));
                }
                Finalization::Deferred => {
                    // Строка всё ещё заперта: не держим ни задание, ни очередь,
                    // а запоминаем, что следующий запуск должен именно финализировать отказ.
                    sqlx::query("update scheduled_jobs set result=$2 where id=$1")
                        .bind(job.id)
                        .bind(json!({"deferred_failure": "delivery_contended"}))
                        .execute(&mut *conn)
                        .await?;
                    return Ok(crate::crunch::JobOutcome::Retry {
                        at: now + TimeDelta::seconds(2),
                        poll: false,
                    });
                }
            }
        }
        if blocked_since.is_none() {
            sqlx::query(
                "update scheduled_jobs set blocked_since=$2,updated_at=clock_timestamp() where id=$1",
            )
            .bind(job.id)
            .bind(now)
            .execute(&mut *conn)
            .await?;
        }
        return Ok(crate::crunch::JobOutcome::Retry {
            at: now + TimeDelta::seconds(2),
            poll: false,
        });
    }
    if rows.is_empty() {
        return Ok(crate::crunch::JobOutcome::Cancelled("alarm_settled"));
    }
    for row in &mut rows {
        if row.state != "armed" {
            continue;
        }
        if row.kind == "at" {
            fire_in_tx(
                conn,
                row,
                "time",
                row.fire_at.unwrap_or(now),
                now,
                correlation_id,
                false,
                Some(job.id),
            )
            .await?;
        } else {
            fill_timeout_collected(conn, row).await?;
            fire_in_tx(
                conn,
                row,
                "timed_out",
                row.deadline_at.unwrap_or(now),
                now,
                correlation_id,
                false,
                Some(job.id),
            )
            .await?;
        }
    }
    // Одна часть на один запуск задания. Иначе при одном свободном месте очередь
    // отвергла бы вторую часть и откатила бы первую: доставка застряла бы навсегда.
    let mut parts = render_parts_in_tx(conn, job.owner_id, &rows, now).await?;
    let more = parts.len() > 1;
    let part = parts.remove(0);
    let first = &rows[part.start];
    // Ключ устойчив к повтору: доставленные будильники выпадают из выборки,
    // поэтому первая часть следующего запуска начинается с другого будильника.
    let key = format!("alarm:{}:{}", job.id, first.id);
    let existing: Option<Uuid> =
        sqlx::query_scalar("select id from inputs where owner_id=$1 and idempotency_key=$2")
            .bind(job.owner_id)
            .bind(&key)
            .fetch_optional(&mut *conn)
            .await?;
    let input_id = if let Some(input_id) = existing {
        input_id
    } else {
        let submitted = submit_input_in_tx(
            conn,
            &SubmitInput {
                owner_id: job.owner_id,
                conversation_id: conversation,
                idempotency_key: key,
                content: vec![ContentBlock::Text {
                    text: part.text.clone(),
                }],
                received_at: now,
                correlation_id,
                origin: Some(json!({
                    "kind": "alarm",
                    "alarm_id": first.id,
                    "chain_depth": first.chain_depth,
                    "event": origin::HubEvent {
                        kind: "alarm".into(),
                        alarm_id: Some(first.id),
                        alarm_key: first.key.clone(),
                        fired_reason: first.fired_reason.clone(),
                        ..Default::default()
                    },
                })),
            },
        )
        .await?;
        submitted.input_id
    };
    let delivered: Vec<Uuid> = rows[part.start..part.end]
        .iter()
        .map(|row| row.id)
        .collect();
    sqlx::query(
        "update alarms set delivered_input_id=$2,delivered_at=$3,updated_at=$3 where id=any($1)",
    )
    .bind(&delivered)
    .bind(input_id)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    if more {
        // Остаток получает собственное задание в этой же транзакции: состояние
        // текущего задания больше не единственная опора доставки. Оно завершится
        // как fired, а продолжение придёт следующим запуском.
        insert_job_in_tx(
            conn,
            job.owner_id,
            conversation,
            now + TimeDelta::seconds(1),
            rows[part.end].id,
            false,
        )
        .await?;
    }
    Ok(crate::crunch::JobOutcome::Done(json!({
        "input_id": input_id,
        "alarms": delivered,
    })))
}

/// Итог попытки финализировать отказ доставки. `Deferred` значит, что строка будильника
/// всё ещё заперта и повторять попытку нужно позже, не удерживая задание и очередь.
pub enum Finalization {
    Finalized,
    Deferred,
}

/// Задание доставки исчерпало попытки: будильник становится `failed` с причиной в PWA.
pub async fn abandon_deliver_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    payload: &Value,
    now: DateTime<Utc>,
) -> Result<Finalization, sqlx::Error> {
    fail_delivery_in_tx(conn, conversation_id, payload, "delivery_failed", now).await
}

/// Один обработчик отказа доставки: причина видна в PWA, удержанное возвращается подписке.
/// Строка будильника берётся `for update nowait`: планировщик не ждёт чужую блокировку.
async fn fail_delivery_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    payload: &Value,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<Finalization, sqlx::Error> {
    // Отпускание удержанного берёт строки подписок: очередь получателя идёт первой.
    send::lock_queue(conn, conversation_id).await?;
    let Some(id) = payload
        .get("alarm_id")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<Uuid>().ok())
    else {
        return Ok(Finalization::Finalized);
    };
    let some: bool = sqlx::query_scalar(
        "select exists(select 1 from alarms
         where id=$1 and conversation_id=$2 and delivered_input_id is null
           and state in ('armed','fired'))",
    )
    .bind(id)
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    if !some {
        return Ok(Finalization::Finalized);
    }
    let row: Option<AlarmRow> = sqlx::query_as(
        "select * from alarms
         where id=$1 and conversation_id=$2 and delivered_input_id is null
           and state in ('armed','fired')
         for update skip locked",
    )
    .bind(id)
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        // Строку держит чужая транзакция: не ждём её и не держим очередь с заданием.
        return Ok(Finalization::Deferred);
    };
    let row: AlarmRow = sqlx::query_as(
        "update alarms set state='failed',failure_reason=$2,updated_at=$3
         where id=$1 returning *",
    )
    .bind(row.id)
    .bind(reason)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    // Удержанные пачкой итоги не должны пропасть вместе с недоставленным будильником:
    // возвращаем их обычной подписке в этой же транзакции.
    release_collected_in_tx(conn, &row, now).await?;
    changed_in_tx(conn, &row, Uuid::now_v7()).await?;
    Ok(Finalization::Finalized)
}

async fn fill_timeout_collected(
    conn: &mut PgConnection,
    row: &mut AlarmRow,
) -> Result<(), sqlx::Error> {
    let Some(condition) =
        serde_json::from_value::<SessionCondition>(row.condition.clone().unwrap_or(Value::Null))
            .ok()
    else {
        return Ok(());
    };
    let mut collected = Collected::parse(&row.collected);
    for target in &condition.session_ids {
        if collected.has(*target) {
            continue;
        }
        let state: Option<String> = sqlx::query_scalar(
            "select activity_state from conversation_derived where conversation_id=$1",
        )
        .bind(target)
        .fetch_optional(&mut *conn)
        .await?;
        collected.insert(
            *target,
            CollectedSession {
                state: state.unwrap_or_else(|| "unknown".into()),
                fingerprint: None,
                satisfied: false,
                ownership: None,
                settled_before_arming: false,
                subscription_id: None,
                armed_at: None,
                chain_depth: 0,
                boundary: None,
            },
        );
    }
    row.collected = collected.as_value();
    Ok(())
}

/// Одна часть слитной доставки: текст и диапазон строк, которые он покрывает.
struct DeliveryPart {
    text: String,
    start: usize,
    end: usize,
}

/// Слитная доставка не должна превышать предел одного ввода: несколько крупных пачек
/// делятся на части, каждая со своим ключом идемпотентности.
async fn render_parts_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    rows: &[AlarmRow],
    now: DateTime<Utc>,
) -> Result<Vec<DeliveryPart>, sqlx::Error> {
    if rows.len() == 1 {
        let mut text = render_one_in_tx(conn, owner_id, &rows[0], now).await?;
        text.push_str("\n\n");
        text.push_str(AUTOMATION);
        return Ok(vec![DeliveryPart {
            text,
            start: 0,
            end: 1,
        }]);
    }
    let header = format!("[canterlot] {} alarms fired.\n", rows.len());
    let continued = format!("[canterlot] {} alarms fired (continued).\n", rows.len());
    let budget = canterlot_proto::limits::MAX_INPUT_TEXT_BYTES.saturating_sub(4096);
    let mut parts = Vec::new();
    let mut text = header;
    let mut start = 0;
    for (index, row) in rows.iter().enumerate() {
        let chunk = render_one_in_tx(conn, owner_id, row, now)
            .await?
            .trim_start_matches("[canterlot] ")
            .to_string();
        let bullet = format!("\n- {chunk}");
        if index > start && text.len() + bullet.len() + AUTOMATION.len() + 4 > budget {
            text.push_str("\n\n");
            text.push_str(AUTOMATION);
            parts.push(DeliveryPart {
                text,
                start,
                end: index,
            });
            text = continued.clone();
            start = index;
        }
        text.push_str(&bullet);
    }
    text.push_str("\n\n");
    text.push_str(AUTOMATION);
    parts.push(DeliveryPart {
        text,
        start,
        end: rows.len(),
    });
    Ok(parts)
}

/// Жива ли подписка, удержавшая итог, и то же ли это поколение взвода. Строка берётся
/// блокирующе, чтобы отписка и отпускание сериализовались.
async fn subscription_alive_in_tx(
    conn: &mut PgConnection,
    subscriber_id: Uuid,
    target_id: Uuid,
    entry: &CollectedSession,
) -> Result<bool, sqlx::Error> {
    let (Some(subscription_id), Some(armed_at)) = (entry.subscription_id, entry.armed_at) else {
        return Ok(false);
    };
    let alive: Option<Uuid> = sqlx::query_scalar(
        "select id from session_subscriptions
         where id=$1 and subscriber_id=$2 and target_id=$3 and armed_at=$4 and closed_at is null
         for update",
    )
    .bind(subscription_id)
    .bind(subscriber_id)
    .bind(target_id)
    .bind(armed_at)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(alive.is_some())
}

async fn render_one_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    row: &AlarmRow,
    now: DateTime<Utc>,
) -> Result<String, sqlx::Error> {
    if !(row.batch && row.kind == "condition") {
        return Ok(render_one(row, now));
    }
    let Some(condition) =
        serde_json::from_value::<SessionCondition>(row.condition.clone().unwrap_or(Value::Null))
            .ok()
    else {
        return Ok(render_one(row, now));
    };
    let collected = Collected::parse(&row.collected);
    let mut text = render_condition_text(row, now);
    // Итог держат только цели, которые пачка собрала; остальные состояния уже пришли
    // обычной подпиской и упоминаются одной строкой, без повторного уведомления.
    // Владение проверяется на каждой отдаче: отписка, приём или переподписка
    // с новым поколением выбрасывают удержанный итог.
    let mut held: Vec<(Uuid, CollectedSession)> = Vec::new();
    let mut reported: Vec<(Uuid, String, bool)> = Vec::new();
    for target in &condition.session_ids {
        let Some(entry) = collected.get(*target).cloned() else {
            continue;
        };
        if entry.subscription_id.is_some() {
            if subscription_alive_in_tx(conn, row.conversation_id, *target, &entry).await? {
                held.push((*target, entry));
            }
        } else if entry.satisfied && entry.owned() == Ownership::Separate {
            reported.push((*target, entry.state, entry.settled_before_arming));
        }
    }
    // Общий предел сообщения делится между итогами поровну.
    let budget = summary::TEXT_LIMIT.saturating_sub(text.len() + 512) / held.len().max(1);
    for (target, entry) in &held {
        text.push_str("\n\n");
        if let Some(built) = summary::build_bounded(
            conn,
            owner_id,
            *target,
            &entry.state,
            entry.armed_at,
            entry.boundary.as_ref(),
        )
        .await?
        {
            text.push_str(&summary::render_bounded(&built, budget));
        } else {
            text.push_str(&format!(
                "[canterlot] Session {target} is now {}; its summary is gone.",
                entry.state
            ));
        }
    }
    for (target, state, before_arming) in &reported {
        text.push_str("\n\n");
        if *before_arming {
            text.push_str(&format!(
                "[canterlot] Session {target} was already {state} before the alarm was armed."
            ));
        } else {
            text.push_str(&format!(
                "[canterlot] Session {target} is {state}; reported separately."
            ));
        }
    }
    if let Some(note) = row.note.as_deref().filter(|note| !note.is_empty()) {
        text.push_str("\nNote: ");
        text.push_str(note);
    }
    Ok(text)
}

#[cfg(test)]
fn render(rows: &[AlarmRow], now: DateTime<Utc>) -> String {
    let mut text = String::new();
    if rows.len() == 1 {
        text.push_str(&render_one(&rows[0], now));
    } else {
        text.push_str(&format!("[canterlot] {} alarms fired.\n", rows.len()));
        for row in rows {
            text.push_str(&format!(
                "\n- {}",
                render_one(row, now).trim_start_matches("[canterlot] ")
            ));
        }
    }
    text.push_str("\n\n");
    text.push_str(AUTOMATION);
    text
}

fn late_text(row: &AlarmRow, now: DateTime<Utc>) -> String {
    let late = now - row.fired_at.unwrap_or(now);
    if late >= TimeDelta::minutes(1) {
        format!(", late by {}", coarse(late))
    } else {
        String::new()
    }
}

fn state_list(condition: &SessionCondition, collected: &Collected) -> String {
    condition
        .session_ids
        .iter()
        .map(|id| {
            let state = collected
                .get(*id)
                .map(|entry| entry.state.as_str())
                .unwrap_or("unknown");
            format!("{id} {state}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_condition_text(row: &AlarmRow, now: DateTime<Utc>) -> String {
    let name = name_of(row.key.as_deref());
    let condition: Option<SessionCondition> =
        serde_json::from_value(row.condition.clone().unwrap_or(Value::Null)).ok();
    let collected = Collected::parse(&row.collected);
    let Some(condition) = condition else {
        return format!("[canterlot] Alarm {name}fired{}", late_text(row, now));
    };
    let timed_out = row.fired_reason.as_deref() == Some("timed_out");
    let late = late_text(row, now);
    let age = coarse(row.fired_at.unwrap_or(now) - row.created_at);
    let states = state_list(&condition, &collected);
    if row.batch {
        if timed_out {
            let missed = condition
                .session_ids
                .iter()
                .filter(|id| collected.get(**id).is_none_or(|entry| !entry.satisfied))
                .map(Uuid::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            let settled = condition
                .session_ids
                .iter()
                .filter(|id| collected.get(**id).is_some_and(|entry| entry.satisfied))
                .count();
            return format!(
                "[canterlot] Alarm {name}timed out after {age}: {settled}/{} sessions settled (batched){late}. Not settled: {missed}.",
                condition.session_ids.len()
            );
        }
        return format!(
            "[canterlot] Alarm {name}fired: sessions {} settled (all, batched){late}.",
            condition
                .session_ids
                .iter()
                .map(Uuid::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if condition.session_ids.len() == 1 {
        let target = condition.session_ids[0];
        let state = collected
            .get(target)
            .map(|entry| entry.state.as_str())
            .unwrap_or("unknown");
        if timed_out {
            format!(
                "[canterlot] Alarm {name}timed out after {age}: session {target} is still {state}{late}."
            )
        } else {
            format!("[canterlot] Alarm {name}fired: session {target} is {state}{late}.")
        }
    } else if timed_out {
        format!("[canterlot] Alarm {name}timed out after {age}: sessions {states}{late}.")
    } else {
        format!("[canterlot] Alarm {name}fired: sessions {states}{late}.")
    }
}

fn number(value: f64) -> String {
    let text = format!("{value:.1}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn percent(used_permille: u16) -> String {
    let percent = f64::from(used_permille) / 10.0;
    if (percent.fract()).abs() < f64::EPSILON {
        format!("{}", percent as u16)
    } else {
        format!("{percent:.1}")
    }
}

fn render_limit_text(row: &AlarmRow, condition: &LimitCondition, now: DateTime<Utc>) -> String {
    let name = name_of(row.key.as_deref());
    let late = late_text(row, now);
    if row.fired_reason.as_deref() == Some("timed_out") {
        let age = coarse(row.fired_at.unwrap_or(now) - row.created_at);
        return format!(
            "[canterlot] Alarm {name}timed out after {age}: {} {} window did not reset{late}.",
            condition.provider, condition.window
        );
    }
    if row.fired_reason.as_deref() == Some("by_clock") {
        return format!(
            "[canterlot] Alarm {name}fired by clock: {} {} window reset has no fresh reading{late}.",
            condition.provider, condition.window
        );
    }
    let used = condition.observed_used_permille.or(condition.used_permille);
    if condition.trigger.as_deref() == Some("below_percent") {
        let used = used.map(percent).unwrap_or_else(|| "?".into());
        let below = condition
            .below_percent
            .map(number)
            .unwrap_or_else(|| "?".into());
        return format!(
            "[canterlot] Alarm {name}fired: {} {} window usage is {used}% (below {below}%){late}.",
            condition.provider, condition.window
        );
    }
    match used {
        Some(used) => format!(
            "[canterlot] Alarm {name}fired: {} {} window reset (now {}% used){late}.",
            condition.provider,
            condition.window,
            percent(used)
        ),
        None => format!(
            "[canterlot] Alarm {name}fired: {} {} window reset{late}.",
            condition.provider, condition.window
        ),
    }
}

fn render_machine_text(row: &AlarmRow, condition: &MachineCondition, now: DateTime<Utc>) -> String {
    let name = name_of(row.key.as_deref());
    let late = late_text(row, now);
    if row.fired_reason.as_deref() == Some("timed_out") {
        let age = coarse(row.fired_at.unwrap_or(now) - row.created_at);
        return format!(
            "[canterlot] Alarm {name}timed out after {age}: the machine never freed up{late}."
        );
    }
    let mut parts = Vec::new();
    if let Some(value) = condition.observed_free_disk_gb {
        parts.push(format!("{} GB free", number(value)));
    }
    if let Some(value) = condition.observed_load_per_core {
        parts.push(format!("load {} per core", number(value)));
    }
    if let Some(value) = condition.observed_mem_available_gb {
        parts.push(format!("{} GB memory available", number(value)));
    }
    format!(
        "[canterlot] Alarm {name}fired: machine has {}{late}.",
        parts.join(", ")
    )
}

fn render_one(row: &AlarmRow, now: DateTime<Utc>) -> String {
    let name = name_of(row.key.as_deref());
    let fired_at = row.fired_at.unwrap_or(now);
    let mut text = match row.kind.as_str() {
        "at" => format!(
            "[canterlot] Alarm {name}fired at {}{} (set {} ago).",
            clock(fired_at),
            late_text(row, now),
            coarse(fired_at - row.created_at)
        ),
        _ => {
            let condition_kind = row
                .condition
                .as_ref()
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str);
            match condition_kind {
                Some("limit_reset") => row
                    .condition
                    .as_ref()
                    .and_then(|value| serde_json::from_value::<LimitCondition>(value.clone()).ok())
                    .map(|condition| render_limit_text(row, &condition, now))
                    .unwrap_or_else(|| render_condition_text(row, now)),
                Some("machine") => row
                    .condition
                    .as_ref()
                    .and_then(|value| {
                        serde_json::from_value::<MachineCondition>(value.clone()).ok()
                    })
                    .map(|condition| render_machine_text(row, &condition, now))
                    .unwrap_or_else(|| render_condition_text(row, now)),
                _ => render_condition_text(row, now),
            }
        }
    };
    if let Some(note) = row.note.as_deref().filter(|note| !note.is_empty()) {
        text.push_str("\nNote: ");
        text.push_str(note);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_time_args_from_json() {
        let at = (Utc::now() + TimeDelta::seconds(60)).to_rfc3339();
        let value = json!({"at": at, "key": "gate", "note": "wave C"});
        let parsed: SetArgs = serde_json::from_value(value).unwrap();
        assert!(parsed.at.is_some());
    }

    #[test]
    fn condition_text_names_target_states_and_deadline() {
        let id = Uuid::now_v7();
        let condition = SessionCondition {
            kind: "session_state".into(),
            session_ids: vec![id],
            states: vec!["idle".into(), "failed".into()],
            all: false,
            armed: BTreeMap::new(),
        };
        let text = condition_text(&condition, Utc::now(), false, false);
        assert!(text.contains(&id.to_string()));
        assert!(text.contains("idle, failed"));
        assert!(text.contains("deadline"));
    }

    #[test]
    fn condition_text_marks_all_batch_and_watchdog() {
        let id = Uuid::now_v7();
        let condition = SessionCondition {
            kind: "session_state".into(),
            session_ids: vec![id],
            states: vec!["idle".into()],
            all: true,
            armed: BTreeMap::new(),
        };
        let text = condition_text(&condition, Utc::now(), true, true);
        assert!(text.contains("all, batched"), "{text}");
        assert!(text.contains("watchdog"), "{text}");
    }

    #[test]
    fn parse_condition_takes_several_targets_and_all() {
        let one = Uuid::now_v7();
        let two = Uuid::now_v7();
        let when = WhenArgs {
            kind: "session_state".into(),
            session_ids: Some(vec![one, two, one]),
            states: None,
            all: Some(true),
            provider: None,
            window: None,
            below_percent: None,
            free_disk_gb_above: None,
            load_per_core_below: None,
            mem_available_gb_above: None,
        };
        let ParsedWhen::Session(condition, states) = parse_when(&when).unwrap() else {
            panic!("expected a session condition");
        };
        assert_eq!(condition.session_ids, vec![one, two], "duplicates collapse");
        assert!(condition.all);
        assert_eq!(states.len(), DEFAULT_STATES.len());
    }

    #[test]
    fn collected_complete_requires_every_target() {
        let one = Uuid::now_v7();
        let two = Uuid::now_v7();
        let condition = SessionCondition {
            kind: "session_state".into(),
            session_ids: vec![one, two],
            states: vec!["idle".into()],
            all: true,
            armed: BTreeMap::new(),
        };
        let mut collected = Collected::default();
        collected.insert(
            one,
            CollectedSession {
                state: "idle".into(),
                fingerprint: Some("a".into()),
                satisfied: true,
                ownership: None,
                settled_before_arming: false,
                subscription_id: None,
                armed_at: None,
                chain_depth: 0,
                boundary: None,
            },
        );
        assert!(!collected.complete(&condition));
        collected.insert(
            two,
            CollectedSession {
                state: "idle".into(),
                fingerprint: Some("b".into()),
                satisfied: true,
                ownership: None,
                settled_before_arming: false,
                subscription_id: None,
                armed_at: None,
                chain_depth: 0,
                boundary: None,
            },
        );
        assert!(collected.complete(&condition));
    }

    #[test]
    fn render_single_alarm_keeps_note_and_automation_line() {
        let now = Utc::now();
        let id = Uuid::now_v7();
        let row = AlarmRow {
            id,
            owner_id: Uuid::now_v7(),
            conversation_id: Uuid::now_v7(),
            key: Some("gate".into()),
            kind: "at".into(),
            condition: None,
            fire_at: Some(now - TimeDelta::minutes(2)),
            deadline_at: None,
            only_on_timeout: false,
            batch: false,
            collected: Value::Null,
            note: Some("wave C".into()),
            chain_depth: 0,
            state: "fired".into(),
            fired_reason: Some("time".into()),
            failure_reason: None,
            call_id: Uuid::now_v7(),
            response: Value::Null,
            delivered_input_id: None,
            delivered_at: None,
            created_at: now - TimeDelta::hours(2),
            updated_at: now,
            fired_at: Some(now - TimeDelta::minutes(2)),
        };
        let text = render(&[row], now);
        assert!(text.starts_with("[canterlot] Alarm \"gate\" fired at"));
        assert!(text.contains("Note: wave C"));
        assert!(text.ends_with(AUTOMATION));
    }
}
