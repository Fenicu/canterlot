//! Сторож машин: свободное место, память и пережившие исполнения процессы. Обход
//! идёт службой-одиночкой, эпизоды и отметки получателей живут в базе, поэтому
//! повторный обход и перезапуск хаба не дают второго события. Возврат к норме с
//! запасом молча закрывает эпизод: сообщения «стало лучше» нет.
//!
//! Получатели события машины: подписчики незавершённых сессий на этой машине и
//! оркестраторы на ней (сессии с делегатами), которые работают, ждут владельца или
//! простаивают, но запускали делегата или заканчивали ход за последние сутки.
//! Остальные видят состояние только в PWA. Каждый получатель слышит событие одного
//! вида один раз за эпизод (`machine_event_marks`).

use std::{collections::BTreeMap, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{config::MachinesConfig, scheduler::notify_pool};

const SWEEP_INTERVAL: Duration = Duration::from_secs(5);
const BYTES_PER_GB: f64 = 1e9;
/// Столько выборок подряд с малой памятью открывают эпизод.
const MEM_LOW_SAMPLES: i32 = 3;
/// Возврат памяти с запасом закрывает эпизод.
const MEM_RECOVER_BYTES: u64 = 1_000_000_000;
/// Крупных каталогов сборки в задании события.
const PAYLOAD_HOTSPOTS: usize = 5;
/// Оркестратор в простое слышит события, если запускал делегата или заканчивал ход
/// не раньше этого.
const ORCHESTRATOR_RECENT: TimeDelta = TimeDelta::hours(24);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub opened: usize,
    pub warned: usize,
    pub critical: usize,
    pub recovered: usize,
    pub mem_opened: usize,
    pub mem_warned: usize,
    pub mem_recovered: usize,
    pub leak_opened: usize,
    pub leak_warned: usize,
    pub leak_recovered: usize,
}

struct Recipient {
    owner_id: Uuid,
    conversation_id: Uuid,
    chain_depth: i32,
}

/// Минимум свободного места по дискам отчёта и ёмкость этого же диска.
fn reading(disks: &Value) -> Option<(u64, u64)> {
    let mut best: Option<(u64, u64)> = None;
    for disk in disks.as_array()? {
        let total = disk["total_bytes"].as_u64()?;
        let free = disk["free_bytes"].as_u64()?;
        if total == 0 {
            continue;
        }
        if best.is_none_or(|(best_free, _)| free < best_free) {
            best = Some((free, total));
        }
    }
    best
}

fn usage(free: u64, total: u64) -> (u64, u64) {
    let free_gb = (free as f64 / BYTES_PER_GB).round() as u64;
    let pct = (free as f64 * 100.0 / total as f64).round() as u64;
    (free_gb, pct.min(100))
}

/// Получатели события машины по правилу модуля; глубина цепочки — наименьшая.
async fn recipients(
    conn: &mut PgConnection,
    machine_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Vec<Recipient>, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, i32)> = sqlx::query_as(
        "select s.owner_id,s.subscriber_id,s.chain_depth
         from session_subscriptions s
         where s.closed_at is null
           and exists (
             select 1 from conversations c
             left join conversation_derived d on d.conversation_id=c.id
             where c.id=s.target_id and c.machine_id=$1 and c.archived_at is null
               and (coalesce(d.activity_state,'idle') not in ('failed','stopped')
                    or (d.activity_state='failed'
                        and d.activity_evidence->>'reason'='owner_lost')))
         union all
         select c.owner_id,c.id,0
         from conversations c
         left join conversation_derived d on d.conversation_id=c.id
         where c.machine_id=$1 and c.archived_at is null and c.trashed_at is null
           and exists (select 1 from conversations k where k.parent_conversation_id=c.id)
           and (coalesce(d.activity_state,'idle') in ('working','waiting_for_user')
                or (coalesce(d.activity_state,'idle')='idle'
                    and (d.activity_since>=$2
                         or exists (select 1 from conversations k
                                    where k.parent_conversation_id=c.id and k.created_at>=$2))))",
    )
    .bind(machine_id)
    .bind(now - ORCHESTRATOR_RECENT)
    .fetch_all(conn)
    .await?;
    let mut merged: BTreeMap<Uuid, Recipient> = BTreeMap::new();
    for (owner_id, conversation_id, chain_depth) in rows {
        merged
            .entry(conversation_id)
            .and_modify(|r| r.chain_depth = r.chain_depth.min(chain_depth))
            .or_insert(Recipient {
                owner_id,
                conversation_id,
                chain_depth,
            });
    }
    Ok(merged.into_values().collect())
}

/// Ставит задания тем получателям, у кого ещё нет отметки этого эпизода и вида.
/// Общая точка для всех событий машины, в том числе `lease_long_hold`.
pub(crate) async fn announce(
    conn: &mut PgConnection,
    machine_id: Uuid,
    episode_id: Uuid,
    kind: &str,
    details: &Value,
    now: DateTime<Utc>,
) -> Result<usize, sqlx::Error> {
    let mut sent = 0;
    for recipient in recipients(&mut *conn, machine_id, now).await? {
        let marked = sqlx::query(
            "insert into machine_event_marks(episode_id,kind,conversation_id,created_at)
             values($1,$2,$3,$4) on conflict do nothing",
        )
        .bind(episode_id)
        .bind(kind)
        .bind(recipient.conversation_id)
        .bind(now)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if marked == 0 {
            continue;
        }
        let payload = json!({
            "action": "machine_event",
            "kind": kind,
            "episode_id": episode_id,
            "machine_id": machine_id,
            "target_id": recipient.conversation_id,
            "chain_depth": recipient.chain_depth,
            "details": details,
        });
        sqlx::query(
            "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
             values($1,'session_task',$2,$3,$4,$5)",
        )
        .bind(Uuid::now_v7())
        .bind(recipient.owner_id)
        .bind(recipient.conversation_id)
        .bind(now)
        .bind(payload)
        .execute(&mut *conn)
        .await?;
        sent += 1;
    }
    Ok(sent)
}

/// Крупнейшие каталоги сборки из последнего отчёта сторожа, с названиями бесед.
async fn hotspots(
    conn: &mut PgConnection,
    owner_id: Uuid,
    machine_id: Uuid,
) -> Result<Vec<Value>, sqlx::Error> {
    let stored: Option<Value> =
        sqlx::query_scalar("select hotspots from machine_watch where machine_id=$1")
            .bind(machine_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(stored) = stored else {
        return Ok(Vec::new());
    };
    let mut rows = crate::machine_watch::resolve_hotspots(conn, owner_id, &stored).await?;
    rows.sort_by_key(|row| std::cmp::Reverse(row.bytes));
    rows.truncate(PAYLOAD_HOTSPOTS);
    Ok(rows
        .into_iter()
        .map(|h| {
            json!({
                "path": h.path,
                "kind": h.kind,
                "bytes": h.bytes,
                "incremental_bytes": h.incremental_bytes,
                "newest_at": h.newest_at,
                "conversation_id": h.conversation_id,
                "title": h.title,
            })
        })
        .collect())
}

async fn open_episode(
    conn: &mut PgConnection,
    machine_id: Uuid,
    kind: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select id from machine_watch_episodes
         where machine_id=$1 and kind=$2 and ended_at is null for update",
    )
    .bind(machine_id)
    .bind(kind)
    .fetch_optional(conn)
    .await
}

async fn start_episode(
    conn: &mut PgConnection,
    machine_id: Uuid,
    owner_id: Uuid,
    kind: &str,
    now: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into machine_watch_episodes(id,machine_id,owner_id,kind,started_at)
         values($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(machine_id)
    .bind(owner_id)
    .bind(kind)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(id)
}

async fn end_episode(
    conn: &mut PgConnection,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("update machine_watch_episodes set ended_at=$2 where id=$1")
        .bind(id)
        .bind(now)
        .execute(conn)
        .await?;
    Ok(())
}

/// Строка выборки: машина, владелец, диски, память, исполнения, время выборки.
type SampleRow = (Uuid, Uuid, Value, i64, i64, Value, DateTime<Utc>);

struct Sample {
    machine_id: Uuid,
    owner_id: Uuid,
    disks: Value,
    mem_total: i64,
    mem_available: i64,
    executions: Value,
    observed_at: DateTime<Utc>,
}

async fn sweep_disk(
    conn: &mut PgConnection,
    config: &MachinesConfig,
    sample: &Sample,
    now: DateTime<Utc>,
    report: &mut SweepReport,
) -> Result<(), sqlx::Error> {
    let Some((free, total)) = reading(&sample.disks) else {
        return Ok(());
    };
    let machine_id = sample.machine_id;
    let open: Option<Uuid> = sqlx::query_scalar(
        "select id from machine_disk_episodes
         where machine_id=$1 and ended_at is null for update",
    )
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    if free >= config.disk_recover_bytes() {
        if let Some(episode_id) = open {
            sqlx::query("update machine_disk_episodes set ended_at=$2 where id=$1")
                .bind(episode_id)
                .bind(now)
                .execute(&mut *conn)
                .await?;
            report.recovered += 1;
        }
        return Ok(());
    }
    // Между порогом и порогом с запасом эпизод не открывается заново:
    // это и есть гистерезис, который не даёт строке мигать.
    if free > config.disk_warn_bytes() {
        return Ok(());
    }
    let episode_id = match open {
        Some(episode_id) => episode_id,
        None => {
            let id = Uuid::now_v7();
            sqlx::query(
                "insert into machine_disk_episodes(id,machine_id,owner_id,started_at)
                 values($1,$2,$3,$4)",
            )
            .bind(id)
            .bind(machine_id)
            .bind(sample.owner_id)
            .bind(now)
            .execute(&mut *conn)
            .await?;
            report.opened += 1;
            id
        }
    };
    let (free_gb, free_pct) = usage(free, total);
    let details = json!({
        "free_gb": free_gb,
        "free_pct": free_pct,
        "hotspots": hotspots(&mut *conn, sample.owner_id, machine_id).await?,
    });
    report.warned += announce(
        &mut *conn, machine_id, episode_id, "disk_low", &details, now,
    )
    .await?;
    if free <= config.disk_critical_bytes() {
        report.critical += announce(
            &mut *conn,
            machine_id,
            episode_id,
            "disk_critical",
            &details,
            now,
        )
        .await?;
    }
    Ok(())
}

/// Крупнейшие по памяти исполнения выборки с названиями бесед.
async fn heaviest(
    conn: &mut PgConnection,
    owner_id: Uuid,
    executions: &Value,
) -> Result<Vec<Value>, sqlx::Error> {
    let mut rows: Vec<(Uuid, u64)> = executions
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            Some((
                Uuid::parse_str(e["execution_id"].as_str()?).ok()?,
                e["rss_bytes"].as_u64()?,
            ))
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.1));
    rows.truncate(3);
    let mut out = Vec::new();
    for (execution_id, rss) in rows {
        let title: Option<String> = sqlx::query_scalar(
            "select c.title from executions e join conversations c on c.id=e.conversation_id
             where e.id=$1 and c.owner_id=$2",
        )
        .bind(execution_id)
        .bind(owner_id)
        .fetch_optional(&mut *conn)
        .await?;
        out.push(json!({
            "execution_id": execution_id,
            "title": title.map(|t| super::origin::clean_title(&t)),
            "rss_bytes": rss,
        }));
    }
    Ok(out)
}

/// Серия выборок с малой памятью: каждая новая выборка считается один раз.
async fn sweep_memory(
    conn: &mut PgConnection,
    config: &MachinesConfig,
    sample: &Sample,
    now: DateTime<Utc>,
    report: &mut SweepReport,
) -> Result<bool, sqlx::Error> {
    let machine_id = sample.machine_id;
    let warn = config.mem_warn_gb.saturating_mul(1_000_000_000);
    let available = u64::try_from(sample.mem_available).unwrap_or(0);
    let low = available < warn;
    let samples: Option<i32> = sqlx::query_scalar(
        "insert into machine_watch(machine_id,owner_id,mem_low_samples,mem_sample_at)
         values($1,$2,$3,$4)
         on conflict(machine_id) do update set
           mem_low_samples=case when $5 then machine_watch.mem_low_samples+1 else 0 end,
           mem_sample_at=excluded.mem_sample_at
         where machine_watch.mem_sample_at is null or machine_watch.mem_sample_at<excluded.mem_sample_at
         returning mem_low_samples",
    )
    .bind(machine_id)
    .bind(sample.owner_id)
    .bind(i32::from(low))
    .bind(sample.observed_at)
    .bind(low)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(samples) = samples else {
        return Ok(false);
    };
    let open = open_episode(&mut *conn, machine_id, "mem_low").await?;
    match open {
        Some(id) if available >= warn.saturating_add(MEM_RECOVER_BYTES) => {
            end_episode(&mut *conn, id, now).await?;
            report.mem_recovered += 1;
            Ok(true)
        }
        None if samples >= MEM_LOW_SAMPLES => {
            let id = start_episode(&mut *conn, machine_id, sample.owner_id, "mem_low", now).await?;
            report.mem_opened += 1;
            let details = json!({
                "available_bytes": available,
                "total_bytes": u64::try_from(sample.mem_total).unwrap_or(0),
                "heaviest": heaviest(&mut *conn, sample.owner_id, &sample.executions).await?,
            });
            report.mem_warned +=
                announce(&mut *conn, machine_id, id, "mem_low", &details, now).await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Утечки по последнему свежему отчёту сторожа.
async fn sweep_leaks(
    conn: &mut PgConnection,
    machine_id: Uuid,
    owner_id: Uuid,
    now: DateTime<Utc>,
    report: &mut SweepReport,
) -> Result<(), sqlx::Error> {
    let row: Option<(Value,)> =
        sqlx::query_as("select leaked from machine_watch where machine_id=$1 and observed_at>=$2")
            .bind(machine_id)
            .bind(now - TimeDelta::seconds(crate::machine_watch::WATCH_FRESH_SECS))
            .fetch_optional(&mut *conn)
            .await?;
    let Some((leaked,)) = row else {
        return Ok(());
    };
    let open = open_episode(&mut *conn, machine_id, "processes_leaked").await?;
    let any = leaked.as_array().is_some_and(|rows| !rows.is_empty());
    match (open, any) {
        (Some(id), false) => {
            end_episode(&mut *conn, id, now).await?;
            report.leak_recovered += 1;
        }
        (None, true) => {
            let id =
                start_episode(&mut *conn, machine_id, owner_id, "processes_leaked", now).await?;
            report.leak_opened += 1;
            let rows = crate::machine_watch::resolve_leaked(&mut *conn, owner_id, &leaked).await?;
            let details = json!({"leaked": rows.iter().map(|r| json!({
                "execution_id": r.execution_id,
                "title": r.title,
                "comm": r.comm,
                "rss_bytes": r.rss_bytes,
            })).collect::<Vec<_>>()});
            report.leak_warned += announce(
                &mut *conn,
                machine_id,
                id,
                "processes_leaked",
                &details,
                now,
            )
            .await?;
        }
        _ => {}
    }
    Ok(())
}

async fn sweep_once(
    pool: &PgPool,
    config: &MachinesConfig,
    now: DateTime<Utc>,
) -> Result<SweepReport, sqlx::Error> {
    let max_age = TimeDelta::from_std(config.metrics_stale_after).unwrap_or(TimeDelta::MAX);
    let cutoff = now - max_age;
    let config = config.clone();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            // Порядок блокировок как у проекции доступности: машины по id, затем остальное.
            let machines: Vec<SampleRow> = sqlx::query_as(
                "select m.id,m.owner_id,mm.disks,mm.mem_total_bytes,mm.mem_available_bytes,
                        mm.executions,mm.observed_at
                 from machines m join machine_metrics mm on mm.machine_id=m.id
                 where mm.observed_at >= $1
                 order by m.id for no key update of m",
            )
            .bind(cutoff)
            .fetch_all(&mut *conn)
            .await?;
            let mut report = SweepReport::default();
            for (machine_id, owner_id, disks, mem_total, mem_available, executions, observed_at) in
                machines
            {
                let sample = Sample {
                    machine_id,
                    owner_id,
                    disks,
                    mem_total,
                    mem_available,
                    executions,
                    observed_at,
                };
                sweep_disk(&mut *conn, &config, &sample, now, &mut report).await?;
                if sweep_memory(&mut *conn, &config, &sample, now, &mut report).await? {
                    crate::machine_watch::emit_changed(
                        &mut *conn,
                        owner_id,
                        machine_id,
                        Uuid::now_v7(),
                    )
                    .await?;
                }
                sweep_leaks(&mut *conn, machine_id, owner_id, now, &mut report).await?;
            }
            Ok(report)
        })
    })
    .await
}

/// Один обход. Взаимоблокировка и сбой сериализации повторяют транзакцию целиком:
/// до трёх попыток с паузой, как у остальных изменений хаба.
pub async fn sweep(
    pool: &PgPool,
    config: &MachinesConfig,
    now: DateTime<Utc>,
) -> Result<SweepReport, sqlx::Error> {
    const ATTEMPTS: u32 = 3;
    let mut attempt = 0;
    loop {
        attempt += 1;
        match sweep_once(pool, config, now).await {
            Ok(report) => {
                if report.warned + report.critical + report.mem_warned + report.leak_warned > 0 {
                    notify_pool(pool);
                }
                return Ok(report);
            }
            Err(error) if attempt < ATTEMPTS && crate::db::is_transient(&error) => {
                tokio::time::sleep(crate::db::retry_delay(Uuid::now_v7(), attempt)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Служба-одиночка: обход переживает перезапуск хаба вместе с отметками в базе.
pub async fn run(pool: PgPool, config: MachinesConfig, shutdown: watch::Receiver<bool>) {
    let supervisor = pool.clone();
    crate::singleton::supervise(&supervisor, "machine_disk", shutdown, move || {
        let pool = pool.clone();
        let config = config.clone();
        async move {
            let mut tick = tokio::time::interval(SWEEP_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if let Err(error) = sweep(&pool, &config, Utc::now()).await {
                    tracing::warn!(
                        kind = "machine_disk_database",
                        error = %crate::dberr::describe(&error),
                        "disk sweep failed"
                    );
                }
            }
        }
    })
    .await;
}
