//! Отчёт сторожа машины (`watchdog_v1`): крупные каталоги сборки, процессы,
//! пережившие свои исполнения, и число сборок вне очереди. Хаб держит только
//! последний отчёт на машину; эпизоды диска, памяти и утечек берут из него
//! подробности, PWA показывает его на экране машин.

use crate::events::{self, NewEvent, Scope};
use canterlot_proto::{
    limits::{MAX_PROBE_PATH_BYTES, MAX_WATCH_HOTSPOTS, MAX_WATCH_LEAKED},
    worker::v1::MachineWatch,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

pub const HOTSPOT_KINDS: &[&str] = &["target", "node_modules", "svelte_kit", "web_build"];
/// Отчёт сторожа старше этого не говорит о текущих утечках: воркер шлёт его раз в 5 минут.
pub const WATCH_FRESH_SECS: i64 = 11 * 60;
const MAX_COMM_BYTES: usize = 64;

#[derive(Clone, Debug, Serialize, ToSchema, PartialEq)]
pub struct HotspotView {
    pub path: String,
    /// `target`, `node_modules`, `svelte_kit` или `web_build`.
    pub kind: String,
    pub bytes: u64,
    pub incremental_bytes: u64,
    /// Последняя запись сборки.
    pub newest_at: Option<String>,
    pub measured_at: Option<String>,
    /// Рабочая копия беседы; пусто — каталог владельца.
    pub conversation_id: Option<Uuid>,
    pub title: Option<String>,
}

#[derive(Clone, Debug, Serialize, ToSchema, PartialEq)]
pub struct LeakedView {
    pub execution_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub title: Option<String>,
    pub pid: u32,
    pub comm: String,
    pub rss_bytes: u64,
    pub age_s: u64,
}

#[derive(Clone, Debug, Serialize, ToSchema, PartialEq)]
pub struct WatchView {
    pub observed_at: String,
    pub hotspots: Vec<HotspotView>,
    pub leaked: Vec<LeakedView>,
    pub unleased_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchOutcome {
    Stored,
    Ignored(&'static str),
}

fn ignored(reason: &'static str) -> WatchOutcome {
    tracing::warn!(kind = "machine_watch_ignored", reason, "machine watch ignored");
    WatchOutcome::Ignored(reason)
}

fn validate(watch: &MachineWatch) -> Option<&'static str> {
    if watch.hotspots.len() > MAX_WATCH_HOTSPOTS {
        return Some("hotspots");
    }
    if watch.leaked.len() > MAX_WATCH_LEAKED {
        return Some("leaked");
    }
    for hotspot in &watch.hotspots {
        if hotspot.path.is_empty()
            || hotspot.path.len() > MAX_PROBE_PATH_BYTES
            || !hotspot.path.starts_with('/')
            || hotspot.path.chars().any(char::is_control)
        {
            return Some("hotspot_path");
        }
        if !HOTSPOT_KINDS.contains(&hotspot.kind.as_str()) {
            return Some("hotspot_kind");
        }
        if !hotspot.conversation_id.is_empty() && Uuid::parse_str(&hotspot.conversation_id).is_err()
        {
            return Some("hotspot_conversation");
        }
    }
    for leaked in &watch.leaked {
        if Uuid::parse_str(&leaked.execution_id).is_err() {
            return Some("leaked_execution");
        }
        if leaked.comm.len() > MAX_COMM_BYTES || leaked.comm.chars().any(char::is_control) {
            return Some("leaked_comm");
        }
    }
    None
}

fn time_ms(ms: u64) -> Option<String> {
    if ms == 0 {
        return None;
    }
    i64::try_from(ms)
        .ok()
        .and_then(DateTime::from_timestamp_millis)
        .map(crate::conversations::time)
}

/// Сохраняет отчёт и сообщает клиентам. Отчёт заменяет прежний целиком.
pub async fn apply(
    pool: &PgPool,
    owner_id: Uuid,
    machine_id: Uuid,
    watch: &MachineWatch,
    registry: Option<&crate::session::Registry>,
    correlation_id: Uuid,
) -> Result<WatchOutcome, sqlx::Error> {
    if let Some(reason) = validate(watch) {
        return Ok(ignored(reason));
    }
    let now = Utc::now();
    let observed_at =
        crate::machine_info::sample_observed_at(registry, machine_id, watch.observed_at_ms, now);
    let hotspots: Vec<Value> = watch
        .hotspots
        .iter()
        .map(|h| {
            json!({
                "path": h.path,
                "kind": h.kind,
                "bytes": h.bytes,
                "incremental_bytes": h.incremental_bytes,
                "newest_ms": h.newest_ms,
                "measured_at_ms": h.measured_at_ms,
                "conversation_id": (!h.conversation_id.is_empty()).then_some(&h.conversation_id),
            })
        })
        .collect();
    let leaked: Vec<Value> = watch
        .leaked
        .iter()
        .map(|l| {
            json!({
                "execution_id": l.execution_id,
                "pid": l.pid,
                "comm": l.comm,
                "rss_bytes": l.rss_bytes,
                "age_s": l.age_s,
            })
        })
        .collect();
    let unleased = i32::try_from(watch.unleased_count).unwrap_or(i32::MAX);
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            sqlx::query("select id from machines where id=$1 and owner_id=$2 for no key update")
                .bind(machine_id)
                .bind(owner_id)
                .fetch_one(&mut *conn)
                .await?;
            let stored = sqlx::query(
                "insert into machine_watch(machine_id,owner_id,hotspots,leaked,unleased_count,observed_at)
                 values($1,$2,$3,$4,$5,$6)
                 on conflict(machine_id) do update set hotspots=excluded.hotspots,
                   leaked=excluded.leaked,unleased_count=excluded.unleased_count,
                   observed_at=excluded.observed_at
                 where machine_watch.observed_at is null
                    or machine_watch.observed_at<=excluded.observed_at",
            )
            .bind(machine_id)
            .bind(owner_id)
            .bind(json!(hotspots))
            .bind(json!(leaked))
            .bind(unleased)
            .bind(observed_at)
            .execute(&mut *conn)
            .await?
            .rows_affected();
            if stored == 0 {
                return Ok(ignored("stale_watch"));
            }
            emit_changed(&mut *conn, owner_id, machine_id, correlation_id).await?;
            Ok(WatchOutcome::Stored)
        })
    })
    .await
}

/// Событие клиентам: последний отчёт и признак малой памяти целиком, не разность.
pub async fn emit_changed(
    conn: &mut PgConnection,
    owner_id: Uuid,
    machine_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let watch = view(&mut *conn, owner_id, machine_id).await?;
    let memory_low = memory_low(&mut *conn, machine_id).await?;
    events::append(
        conn,
        NewEvent {
            scope: Scope::Machines { owner_id },
            kind: "machine_watch_changed",
            conversation_id: None,
            execution_id: None,
            payload: json!({"machine_id": machine_id, "watch": watch, "memory_low": memory_low}),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

pub async fn memory_low(conn: &mut PgConnection, machine_id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(select 1 from machine_watch_episodes
                       where machine_id=$1 and kind='mem_low' and ended_at is null)",
    )
    .bind(machine_id)
    .fetch_one(conn)
    .await
}

/// Последний отчёт с названиями бесед владельца; `None`, пока отчёта не было.
pub async fn view(
    conn: &mut PgConnection,
    owner_id: Uuid,
    machine_id: Uuid,
) -> Result<Option<WatchView>, sqlx::Error> {
    let row: Option<(Value, Value, i32, Option<DateTime<Utc>>)> = sqlx::query_as(
        "select hotspots,leaked,unleased_count,observed_at from machine_watch
         where machine_id=$1 and owner_id=$2",
    )
    .bind(machine_id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((hotspots, leaked, unleased, Some(observed_at))) = row else {
        return Ok(None);
    };
    let hotspots = resolve_hotspots(&mut *conn, owner_id, &hotspots).await?;
    let leaked = resolve_leaked(&mut *conn, owner_id, &leaked).await?;
    Ok(Some(WatchView {
        observed_at: crate::conversations::time(observed_at),
        hotspots,
        leaked,
        unleased_count: u32::try_from(unleased).unwrap_or(0),
    }))
}

pub async fn resolve_hotspots(
    conn: &mut PgConnection,
    owner_id: Uuid,
    hotspots: &Value,
) -> Result<Vec<HotspotView>, sqlx::Error> {
    let mut out = Vec::new();
    for h in hotspots.as_array().into_iter().flatten() {
        let conversation_id = h["conversation_id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok());
        let title = match conversation_id {
            Some(id) => title(&mut *conn, owner_id, id).await?,
            None => None,
        };
        out.push(HotspotView {
            path: h["path"].as_str().unwrap_or_default().to_owned(),
            kind: h["kind"].as_str().unwrap_or_default().to_owned(),
            bytes: h["bytes"].as_u64().unwrap_or(0),
            incremental_bytes: h["incremental_bytes"].as_u64().unwrap_or(0),
            newest_at: time_ms(h["newest_ms"].as_u64().unwrap_or(0)),
            measured_at: time_ms(h["measured_at_ms"].as_u64().unwrap_or(0)),
            conversation_id,
            title,
        });
    }
    Ok(out)
}

pub async fn resolve_leaked(
    conn: &mut PgConnection,
    owner_id: Uuid,
    leaked: &Value,
) -> Result<Vec<LeakedView>, sqlx::Error> {
    let mut out = Vec::new();
    for l in leaked.as_array().into_iter().flatten() {
        let Some(execution_id) = l["execution_id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
        else {
            continue;
        };
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "select c.id,c.title from executions e join conversations c on c.id=e.conversation_id
             where e.id=$1 and c.owner_id=$2",
        )
        .bind(execution_id)
        .bind(owner_id)
        .fetch_optional(&mut *conn)
        .await?;
        out.push(LeakedView {
            execution_id,
            conversation_id: row.as_ref().map(|(id, _)| *id),
            title: row.map(|(_, title)| crate::session_tools::origin::clean_title(&title)),
            pid: u32::try_from(l["pid"].as_u64().unwrap_or(0)).unwrap_or(0),
            comm: l["comm"].as_str().unwrap_or_default().to_owned(),
            rss_bytes: l["rss_bytes"].as_u64().unwrap_or(0),
            age_s: l["age_s"].as_u64().unwrap_or(0),
        });
    }
    Ok(out)
}

async fn title(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let title: Option<String> =
        sqlx::query_scalar("select title from conversations where id=$1 and owner_id=$2")
            .bind(id)
            .bind(owner_id)
            .fetch_optional(conn)
            .await?;
    Ok(title.map(|t| crate::session_tools::origin::clean_title(&t)))
}
