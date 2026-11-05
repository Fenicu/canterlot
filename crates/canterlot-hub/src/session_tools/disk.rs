//! Свободное место на машинах: подписчики незавершённых сессий один раз слышат
//! предупреждение и один раз — критический уровень. Обход идёт службой-одиночкой,
//! отметки живут в базе на эпизоде и на самой подписке, поэтому повторный обход и
//! перезапуск хаба не дают второго события. Возврат выше порога с запасом молча
//! закрывает эпизод: сообщения «место вернулось» нет.

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{config::MachinesConfig, scheduler::notify_pool};

const SWEEP_INTERVAL: Duration = Duration::from_secs(5);
const BYTES_PER_GB: f64 = 1e9;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub opened: usize,
    pub warned: usize,
    pub critical: usize,
    pub recovered: usize,
}

struct Notice {
    owner_id: Uuid,
    subscriber_id: Uuid,
    target_id: Uuid,
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

async fn insert_job(
    conn: &mut PgConnection,
    notice: &Notice,
    episode_id: Uuid,
    kind: &str,
    free_gb: u64,
    free_pct: u64,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let payload = json!({
        "action": "machine_event",
        "kind": kind,
        "episode_id": episode_id,
        "target_id": notice.target_id,
        "chain_depth": notice.chain_depth,
        "free_gb": free_gb,
        "free_pct": free_pct,
    });
    sqlx::query(
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'session_task',$2,$3,$4,$5)",
    )
    .bind(Uuid::now_v7())
    .bind(notice.owner_id)
    .bind(notice.subscriber_id)
    .bind(now)
    .bind(payload)
    .execute(conn)
    .await?;
    Ok(())
}

/// Сессии на машине, о нехватке места которых подписка ещё не слышала. Терминальные
/// (`failed`, `stopped`) и архивные не в счёт: они уже не возобновятся. `owner_lost`
/// не терминален: воркер может вернуть исполнение.
async fn announce_warn(
    conn: &mut PgConnection,
    episode_id: Uuid,
    machine_id: Uuid,
) -> Result<Vec<Notice>, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, Uuid, i32)> = sqlx::query_as(
        "update session_subscriptions s
         set disk_warn_episode=$1
         where s.disk_warn_episode is distinct from $1
           and s.closed_at is null
           and exists (
             select 1 from conversations c
             left join conversation_derived d on d.conversation_id=c.id
             where c.id=s.target_id and c.machine_id=$2 and c.archived_at is null
               and (coalesce(d.activity_state,'idle') not in ('failed','stopped')
                    or (d.activity_state='failed'
                        and d.activity_evidence->>'reason'='owner_lost')))
         returning s.owner_id,s.subscriber_id,s.target_id,s.chain_depth",
    )
    .bind(episode_id)
    .bind(machine_id)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(owner_id, subscriber_id, target_id, chain_depth)| Notice {
            owner_id,
            subscriber_id,
            target_id,
            chain_depth,
        })
        .collect())
}

async fn announce_critical(
    conn: &mut PgConnection,
    episode_id: Uuid,
    machine_id: Uuid,
) -> Result<Vec<Notice>, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, Uuid, i32)> = sqlx::query_as(
        "update session_subscriptions s
         set disk_critical_episode=$1
         where s.disk_critical_episode is distinct from $1
           and s.closed_at is null
           and exists (
             select 1 from conversations c
             left join conversation_derived d on d.conversation_id=c.id
             where c.id=s.target_id and c.machine_id=$2 and c.archived_at is null
               and (coalesce(d.activity_state,'idle') not in ('failed','stopped')
                    or (d.activity_state='failed'
                        and d.activity_evidence->>'reason'='owner_lost')))
         returning s.owner_id,s.subscriber_id,s.target_id,s.chain_depth",
    )
    .bind(episode_id)
    .bind(machine_id)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(owner_id, subscriber_id, target_id, chain_depth)| Notice {
            owner_id,
            subscriber_id,
            target_id,
            chain_depth,
        })
        .collect())
}

async fn sweep_once(
    pool: &PgPool,
    config: &MachinesConfig,
    now: DateTime<Utc>,
) -> Result<SweepReport, sqlx::Error> {
    let warn = config.disk_warn_bytes();
    let critical = config.disk_critical_bytes();
    let recover = config.disk_recover_bytes();
    let max_age = TimeDelta::from_std(config.metrics_stale_after).unwrap_or(TimeDelta::MAX);
    let cutoff = now - max_age;
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            // Порядок блокировок как у проекции доступности: машины по id, затем подписки.
            let machines: Vec<(Uuid, Uuid, Value)> = sqlx::query_as(
                "select m.id,m.owner_id,mm.disks
                 from machines m join machine_metrics mm on mm.machine_id=m.id
                 where mm.observed_at >= $1
                 order by m.id for no key update of m",
            )
            .bind(cutoff)
            .fetch_all(&mut *conn)
            .await?;
            let mut report = SweepReport::default();
            for (machine_id, owner_id, disks) in machines {
                let Some((free, total)) = reading(&disks) else {
                    continue;
                };
                let open: Option<Uuid> = sqlx::query_scalar(
                    "select id from machine_disk_episodes
                     where machine_id=$1 and ended_at is null for update",
                )
                .bind(machine_id)
                .fetch_optional(&mut *conn)
                .await?;
                if free >= recover {
                    if let Some(episode_id) = open {
                        sqlx::query("update machine_disk_episodes set ended_at=$2 where id=$1")
                            .bind(episode_id)
                            .bind(now)
                            .execute(&mut *conn)
                            .await?;
                        report.recovered += 1;
                    }
                    continue;
                }
                // Между порогом и порогом с запасом эпизод не открывается заново:
                // это и есть гистерезис, который не даёт строке мигать.
                if free > warn {
                    continue;
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
                        .bind(owner_id)
                        .bind(now)
                        .execute(&mut *conn)
                        .await?;
                        report.opened += 1;
                        id
                    }
                };
                let (free_gb, free_pct) = usage(free, total);
                for notice in announce_warn(&mut *conn, episode_id, machine_id).await? {
                    insert_job(
                        &mut *conn, &notice, episode_id, "disk_low", free_gb, free_pct, now,
                    )
                    .await?;
                    report.warned += 1;
                }
                if free <= critical {
                    for notice in announce_critical(&mut *conn, episode_id, machine_id).await? {
                        insert_job(
                            &mut *conn,
                            &notice,
                            episode_id,
                            "disk_critical",
                            free_gb,
                            free_pct,
                            now,
                        )
                        .await?;
                        report.critical += 1;
                    }
                }
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
                if report.warned + report.critical > 0 {
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
