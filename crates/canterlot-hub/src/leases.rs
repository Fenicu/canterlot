//! Зеркало очереди тяжёлых задач машины (`resource_lease_v1`). Очередь ведёт
//! воркер; хаб хранит последнее полное состояние на строке машины, отдаёт его в
//! снимке машин и событием `machine_lease_changed`, очищает при пропаже машины и
//! один раз на аренду предупреждает о долгом удержании (`lease_long_hold`);
//! получателей выбирает общее правило событий машины (`session_tools::disk`).

use std::{collections::HashMap, time::Duration};

use canterlot_proto::worker::v1::{LeaseEntry, LeaseState};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use tokio::sync::watch;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    events::{self, NewEvent, Scope},
    session::Registry,
};

/// Держателей и ожидающих вместе: очередь одной машины, а не журнал.
pub const MAX_LEASE_ENTRIES: usize = 64;
const LABEL_CHARS: usize = 120;
const LEASE_ID_CHARS: usize = 128;
const LONG_HOLD_SWEEP: Duration = Duration::from_secs(30);
/// Отметки предупреждений живут дольше любой аренды.
const MARK_RETENTION: TimeDelta = TimeDelta::days(7);
/// Окно правдоподобия часов машины без сверенного смещения.
const MACHINE_TIME_MAX_AGE: TimeDelta = TimeDelta::days(7);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LeaseKind {
    /// Исключительная: занимает все места (`check.sh --all`, `check-web.sh --stack`).
    Gate,
    /// Одно место.
    Build,
}

impl LeaseKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gate => "gate",
            Self::Build => "build",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LeaseHolder {
    /// Обёртка несёт метку исполнения.
    Execution,
    /// Процесс без метки: терминал владельца, systemd.
    Owner,
}

/// Запись очереди. Времена — в часах хаба.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LeaseEntryView {
    pub lease_id: String,
    pub kind: LeaseKind,
    pub weight: u32,
    pub label: String,
    pub holder: LeaseHolder,
    pub execution_id: Option<Uuid>,
    /// Сессия исполнения, если хаб знает его на этой машине.
    pub conversation_id: Option<Uuid>,
    /// Время выдачи; `null` у ожидающих.
    pub since: Option<String>,
    pub requested_at: Option<String>,
    /// Место в очереди с единицы; `null` у держателей.
    pub position: Option<u32>,
    /// Имя рабочего каталога клиента.
    pub dir: String,
}

/// Последнее полное состояние очереди машины.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LeaseStateView {
    pub slots: u32,
    /// Время снимка на машине, в часах хаба.
    pub observed_at: String,
    pub holders: Vec<LeaseEntryView>,
    pub waiters: Vec<LeaseEntryView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseOutcome {
    Stored,
    Ignored(&'static str),
}

fn ignored(reason: &'static str) -> LeaseOutcome {
    tracing::warn!(
        kind = "machine_lease_ignored",
        reason,
        "lease state ignored"
    );
    LeaseOutcome::Ignored(reason)
}

fn clean(text: &str, limit: usize) -> String {
    let mut text: String = text.chars().take(limit).collect();
    crate::blocks::replace_nul(&mut text);
    text
}

/// Время машины в часах хаба: смещение сеанса, если известно, иначе собственные
/// часы машины в пределах правдоподобия. Будущее прижимается к `now`.
fn machine_time(
    registry: Option<&Registry>,
    machine_id: Uuid,
    machine_ms: u64,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if machine_ms == 0 {
        return None;
    }
    let at = registry
        .and_then(|registry| registry.to_hub_time(machine_id, machine_ms))
        .or_else(|| {
            i64::try_from(machine_ms)
                .ok()
                .and_then(DateTime::from_timestamp_millis)
        })?;
    if at < now - MACHINE_TIME_MAX_AGE {
        return None;
    }
    Some(at.min(now))
}

/// Выдача и запрос записи в часах хаба.
type EntryTimes = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);
type ParsedEntry = (LeaseEntry, LeaseKind, LeaseHolder, Option<Uuid>);

struct Parsed {
    slots: u32,
    holders: Vec<ParsedEntry>,
    waiters: Vec<ParsedEntry>,
}

/// Проверка формы. Нарушение отбрасывает снимок целиком: следующий заменит его.
fn validate(state: &LeaseState) -> Result<Parsed, &'static str> {
    if state.holders.len() + state.waiters.len() > MAX_LEASE_ENTRIES {
        return Err("too_many_entries");
    }
    let mut seen = std::collections::HashSet::new();
    let mut parse = |entry: &LeaseEntry| {
        if entry.lease_id.is_empty()
            || entry.lease_id.chars().count() > LEASE_ID_CHARS
            || !seen.insert(entry.lease_id.clone())
        {
            return Err("lease_id");
        }
        let kind = match entry.kind.as_str() {
            "gate" => LeaseKind::Gate,
            "build" => LeaseKind::Build,
            _ => return Err("kind"),
        };
        let (holder, execution) = match entry.holder.as_str() {
            "execution" => (
                LeaseHolder::Execution,
                Some(Uuid::parse_str(&entry.execution_id).map_err(|_| "execution_id")?),
            ),
            "owner" => (LeaseHolder::Owner, None),
            _ => return Err("holder"),
        };
        Ok((entry.clone(), kind, holder, execution))
    };
    let holders = state
        .holders
        .iter()
        .map(&mut parse)
        .collect::<Result<Vec<_>, _>>()?;
    let waiters = state
        .waiters
        .iter()
        .map(&mut parse)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Parsed {
        slots: state.slots,
        holders,
        waiters,
    })
}

/// Применяет полное состояние очереди машины и сообщает клиентам.
pub async fn apply(
    pool: &PgPool,
    owner_id: Uuid,
    machine_id: Uuid,
    state: &LeaseState,
    registry: Option<&Registry>,
    correlation_id: Uuid,
) -> Result<LeaseOutcome, sqlx::Error> {
    let parsed = match validate(state) {
        Ok(parsed) => parsed,
        Err(reason) => return Ok(ignored(reason)),
    };
    let now = Utc::now();
    let observed_at = machine_time(registry, machine_id, state.observed_at_ms, now).unwrap_or(now);
    let machine_ms = i64::try_from(state.observed_at_ms).unwrap_or(i64::MAX);
    let times: HashMap<String, EntryTimes> = parsed
        .holders
        .iter()
        .chain(&parsed.waiters)
        .map(|(entry, ..)| {
            (
                entry.lease_id.clone(),
                (
                    machine_time(registry, machine_id, entry.since_ms, now),
                    machine_time(registry, machine_id, entry.requested_at_ms, now),
                ),
            )
        })
        .collect();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let found: Option<Uuid> = sqlx::query_scalar(
                "select id from machines where id=$1 and owner_id=$2 for no key update",
            )
            .bind(machine_id)
            .bind(owner_id)
            .fetch_optional(&mut *conn)
            .await?;
            if found.is_none() {
                return Ok(ignored("machine"));
            }
            let executions: Vec<Uuid> = parsed
                .holders
                .iter()
                .chain(&parsed.waiters)
                .filter_map(|(.., execution)| *execution)
                .collect();
            let conversations: HashMap<Uuid, Uuid> = sqlx::query_as::<_, (Uuid, Uuid)>(
                "select e.id,e.conversation_id from executions e
                 join conversations c on c.id=e.conversation_id
                 where e.id=any($1) and c.machine_id=$2 and c.owner_id=$3",
            )
            .bind(&executions)
            .bind(machine_id)
            .bind(owner_id)
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .collect();
            let view = |list: &[ParsedEntry], waiting: bool| {
                list.iter()
                    .enumerate()
                    .map(|(index, (entry, kind, holder, execution))| {
                        let (since, requested) =
                            times.get(&entry.lease_id).copied().unwrap_or_default();
                        LeaseEntryView {
                            lease_id: entry.lease_id.clone(),
                            kind: *kind,
                            weight: entry.weight,
                            label: clean(&entry.label, LABEL_CHARS),
                            holder: *holder,
                            execution_id: *execution,
                            conversation_id: execution
                                .and_then(|id| conversations.get(&id).copied()),
                            since: if waiting {
                                None
                            } else {
                                Some(crate::conversations::time(since.unwrap_or(observed_at)))
                            },
                            requested_at: requested.map(crate::conversations::time),
                            position: waiting.then(|| {
                                if entry.position > 0 {
                                    entry.position
                                } else {
                                    index as u32 + 1
                                }
                            }),
                            dir: clean(&entry.dir, LABEL_CHARS),
                        }
                    })
                    .collect::<Vec<_>>()
            };
            let lease = LeaseStateView {
                slots: parsed.slots,
                observed_at: crate::conversations::time(observed_at),
                holders: view(&parsed.holders, false),
                waiters: view(&parsed.waiters, true),
            };
            let value =
                serde_json::to_value(&lease).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
            let stored = sqlx::query(
                "update machines set lease_state=$2,lease_observed_at=$3,
                     lease_received_at=clock_timestamp(),lease_machine_ms=$4
                 where id=$1 and (lease_machine_ms is null or lease_machine_ms<=$4)",
            )
            .bind(machine_id)
            .bind(&value)
            .bind(observed_at)
            .bind(machine_ms)
            .execute(&mut *conn)
            .await?
            .rows_affected();
            if stored == 0 {
                return Ok(ignored("stale_state"));
            }
            emit(
                &mut *conn,
                owner_id,
                machine_id,
                Some(value),
                correlation_id,
            )
            .await?;
            Ok(LeaseOutcome::Stored)
        })
    })
    .await
}

async fn emit(
    conn: &mut PgConnection,
    owner_id: Uuid,
    machine_id: Uuid,
    lease: Option<serde_json::Value>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Machines { owner_id },
            kind: "machine_lease_changed",
            conversation_id: None,
            execution_id: None,
            payload: json!({"machine_id": machine_id, "lease": lease}),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

/// Пропавшая машина больше не подтверждает очередь: состояние снимается, а не
/// висит вечным «гейт · 3 ч». Снимок, принятый после решения о пропаже (быстрое
/// переподключение), остаётся. Строка машины уже заблокирована вызывающим.
pub(crate) async fn clear_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    machine_id: Uuid,
    decided_at: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let cleared = sqlx::query(
        "update machines set lease_state=null,lease_observed_at=null,
             lease_received_at=null,lease_machine_ms=null
         where id=$1 and lease_state is not null and lease_received_at<$2",
    )
    .bind(machine_id)
    .bind(decided_at)
    .execute(&mut *conn)
    .await?
    .rows_affected()
        != 0;
    if cleared {
        emit(conn, owner_id, machine_id, None, correlation_id).await?;
    }
    Ok(cleared)
}

/// Состояние очереди для снимка машины; только свежее, то есть не очищенное.
pub(crate) async fn load(
    conn: &mut PgConnection,
    machine_id: Uuid,
) -> Result<Option<LeaseStateView>, sqlx::Error> {
    let value: Option<serde_json::Value> =
        sqlx::query_scalar("select lease_state from machines where id=$1")
            .bind(machine_id)
            .fetch_optional(&mut *conn)
            .await?
            .flatten();
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// Ожидание исполнения в очереди: свидетельство активности `lease_wait`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LeaseWait {
    pub kind: LeaseKind,
    pub position: u32,
    #[serde(with = "crate::activity::time")]
    #[schema(value_type = String)]
    pub since: DateTime<Utc>,
}

/// Ближайшее место исполнения среди ожидающих свежего состояния его машины.
pub(crate) async fn wait_of(
    conn: &mut PgConnection,
    execution_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Option<LeaseWait>, sqlx::Error> {
    let lease: Option<serde_json::Value> = sqlx::query_scalar(
        "select m.lease_state from executions e
         join conversations c on c.id=e.conversation_id
         join machines m on m.id=c.machine_id
         where e.id=$1 and m.lease_state is not null",
    )
    .bind(execution_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(lease) = lease.and_then(|v| serde_json::from_value::<LeaseStateView>(v).ok()) else {
        return Ok(None);
    };
    Ok(lease
        .waiters
        .iter()
        .filter(|entry| entry.execution_id == Some(execution_id))
        .min_by_key(|entry| entry.position.unwrap_or(u32::MAX))
        .map(|entry| LeaseWait {
            kind: entry.kind,
            position: entry.position.unwrap_or(1),
            since: entry
                .requested_at
                .as_deref()
                .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.with_timezone(&Utc))
                .unwrap_or(now),
        }))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LongHoldReport {
    pub leases: usize,
    pub delivered: usize,
}

/// Один обход: каждая аренда свежего состояния, удерживаемая дольше порога,
/// один раз даёт задание каждому получателю. Свежесть — состояние не очищено и
/// машина в сети: иначе предупреждение могло бы прийти об уже снятой аренде.
pub async fn long_hold_sweep(
    pool: &PgPool,
    warn_after: Duration,
    now: DateTime<Utc>,
) -> Result<LongHoldReport, sqlx::Error> {
    let threshold = TimeDelta::from_std(warn_after).unwrap_or(TimeDelta::MAX);
    let report = crate::db::in_tx::<_, sqlx::Error, _>(pool, move |conn| {
        Box::pin(async move {
            sqlx::query("delete from lease_long_hold_marks where created_at<$1")
                .bind(now - MARK_RETENTION)
                .execute(&mut *conn)
                .await?;
            let machines: Vec<(Uuid, Uuid, serde_json::Value)> = sqlx::query_as(
                "select id,owner_id,lease_state from machines
                 where lease_state is not null and availability='available'
                 order by id for no key update",
            )
            .fetch_all(&mut *conn)
            .await?;
            let mut report = LongHoldReport::default();
            for (machine_id, owner_id, value) in machines {
                let Ok(lease) = serde_json::from_value::<LeaseStateView>(value) else {
                    continue;
                };
                for holder in &lease.holders {
                    let Some(since) = holder
                        .since
                        .as_deref()
                        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                        .map(|at| at.with_timezone(&Utc))
                    else {
                        continue;
                    };
                    if now - since < threshold {
                        continue;
                    }
                    // Аренда — эпизод: одна отметка на аренду, её id — эпизод для
                    // отметок получателей.
                    let episode: Option<Uuid> = sqlx::query_scalar(
                        "insert into lease_long_hold_marks(machine_id,lease_id,episode_id,created_at)
                         values($1,$2,$3,$4) on conflict do nothing returning episode_id",
                    )
                    .bind(machine_id)
                    .bind(&holder.lease_id)
                    .bind(Uuid::now_v7())
                    .bind(now)
                    .fetch_optional(&mut *conn)
                    .await?;
                    let Some(episode_id) = episode else {
                        continue;
                    };
                    report.leases += 1;
                    let minutes = (now - since).num_minutes().max(1);
                    let title: Option<String> = match holder.conversation_id {
                        Some(id) => {
                            sqlx::query_scalar(
                                "select title from conversations where id=$1 and owner_id=$2",
                            )
                            .bind(id)
                            .bind(owner_id)
                                .fetch_optional(&mut *conn)
                                .await?
                        }
                        None => None,
                    };
                    let details = json!({
                        "lease_id": holder.lease_id,
                        "lease_kind": holder.kind.as_str(),
                        "label": holder.label,
                        "holder": if holder.holder == LeaseHolder::Owner { "owner" } else { "execution" },
                        "execution_id": holder.execution_id,
                        "conversation_id": holder.conversation_id,
                        "holder_title": title.as_deref().map(crate::session_tools::origin::clean_title),
                        "held_minutes": minutes,
                    });
                    // Получателей и их отметки выбирает общее правило событий машины.
                    report.delivered += crate::session_tools::disk::announce(
                        &mut *conn,
                        machine_id,
                        episode_id,
                        "lease_long_hold",
                        &details,
                        now,
                    )
                    .await?;
                }
            }
            Ok(report)
        })
    })
    .await?;
    if report.delivered > 0 {
        crate::scheduler::notify_pool(pool);
    }
    Ok(report)
}

/// Служба-одиночка: отметки в базе переживают перезапуск хаба.
pub async fn run_long_hold(pool: PgPool, warn_after: Duration, shutdown: watch::Receiver<bool>) {
    let supervisor = pool.clone();
    crate::singleton::supervise(&supervisor, "lease_long_hold", shutdown, move || {
        let pool = pool.clone();
        async move {
            let mut tick = tokio::time::interval(LONG_HOLD_SWEEP);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if let Err(error) = long_hold_sweep(&pool, warn_after, Utc::now()).await {
                    tracing::warn!(
                        kind = "lease_long_hold_database",
                        error = %crate::dberr::describe(&error),
                        "lease long hold sweep failed"
                    );
                }
            }
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: &str, holder: &str, execution: &str) -> LeaseEntry {
        LeaseEntry {
            lease_id: id.into(),
            kind: kind.into(),
            weight: 1,
            label: String::new(),
            execution_id: execution.into(),
            holder: holder.into(),
            since_ms: 0,
            requested_at_ms: 0,
            position: 0,
            dir: String::new(),
        }
    }

    #[test]
    fn validation_rejects_malformed_entries() {
        let id = Uuid::now_v7().to_string();
        let ok = LeaseState {
            slots: 2,
            holders: vec![entry("a", "build", "execution", &id)],
            waiters: vec![entry("b", "gate", "owner", "")],
            observed_at_ms: 1,
        };
        assert!(validate(&ok).is_ok());
        for (bad, reason) in [
            (entry("c", "huge", "owner", ""), "kind"),
            (entry("c", "build", "robot", ""), "holder"),
            (entry("c", "build", "execution", "nope"), "execution_id"),
            (entry("a", "build", "owner", ""), "lease_id"),
            (entry("", "build", "owner", ""), "lease_id"),
        ] {
            let mut state = ok.clone();
            state.waiters.push(bad);
            assert_eq!(validate(&state).err(), Some(reason));
        }
        let mut state = ok.clone();
        state.waiters = (0..MAX_LEASE_ENTRIES)
            .map(|n| entry(&format!("w{n}"), "build", "owner", ""))
            .collect();
        assert_eq!(validate(&state).err(), Some("too_many_entries"));
    }

    #[test]
    fn machine_time_clamps_future_and_drops_absurd() {
        let now = Utc::now();
        let ms = |at: DateTime<Utc>| at.timestamp_millis() as u64;
        assert_eq!(machine_time(None, Uuid::nil(), 0, now), None);
        assert_eq!(
            machine_time(None, Uuid::nil(), ms(now + TimeDelta::hours(1)), now),
            Some(now)
        );
        assert_eq!(
            machine_time(None, Uuid::nil(), ms(now - TimeDelta::days(30)), now),
            None
        );
    }
}
