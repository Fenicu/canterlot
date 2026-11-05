//! Простой машины: подписчики её незавершённых сессий один раз слышат, что состояние
//! неизвестно, и один раз — что машина вернулась. Обход идёт службой-одиночкой, а не
//! проектором цели: у простоя нет цели, есть машина и её подписки.
//!
//! Отметки живут на самой подписке (`session_subscriptions`), поэтому второй обход и
//! перезапуск хаба не дают второго события. Открытый простой один на машину.

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{config::SessionToolsConfig, scheduler::notify_pool};

/// Как часто обход смотрит на машины: реже проекции доступности, но заметно чаще грации.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub opened: usize,
    pub announced: usize,
    pub recovered: usize,
}

struct MachineRow {
    id: Uuid,
    owner_id: Uuid,
    availability: String,
    availability_at: Option<DateTime<Utc>>,
}

struct Notice {
    owner_id: Uuid,
    subscriber_id: Uuid,
    target_id: Uuid,
    chain_depth: i32,
}

async fn insert_job(
    conn: &mut PgConnection,
    notice: &Notice,
    outage_id: Uuid,
    kind: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let payload = json!({
        "action": "machine_event",
        "kind": kind,
        "outage_id": outage_id,
        "target_id": notice.target_id,
        "chain_depth": notice.chain_depth,
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

/// Сессии на машине, о простое которых подписка ещё не слышала. Терминальные
/// (`failed`, `stopped`) и архивные не в счёт: они уже не возобновятся. `owner_lost`
/// не терминален: воркер может вернуть исполнение.
async fn announce(
    conn: &mut PgConnection,
    outage_id: Uuid,
    machine_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Vec<Notice>, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, Uuid, i32)> = sqlx::query_as(
        "update session_subscriptions s
         set machine_unavailable_outage=$1,machine_unavailable_at=$2
         where s.machine_unavailable_outage is distinct from $1
           and s.closed_at is null
           and exists (
             select 1 from conversations c
             left join conversation_derived d on d.conversation_id=c.id
             where c.id=s.target_id and c.machine_id=$3 and c.archived_at is null
               and (coalesce(d.activity_state,'idle') not in ('failed','stopped')
                    or (d.activity_state='failed'
                        and d.activity_evidence->>'reason'='owner_lost')))
         returning s.owner_id,s.subscriber_id,s.target_id,s.chain_depth",
    )
    .bind(outage_id)
    .bind(now)
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

/// Подписки, которые услышали `unavailable` этого простоя и ещё не слышали возврата.
async fn recover(conn: &mut PgConnection, outage_id: Uuid) -> Result<Vec<Notice>, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, Uuid, i32)> = sqlx::query_as(
        "update session_subscriptions s
         set machine_available_outage=$1
         where s.machine_unavailable_outage=$1
           and s.machine_available_outage is distinct from $1
           and s.closed_at is null
         returning s.owner_id,s.subscriber_id,s.target_id,s.chain_depth",
    )
    .bind(outage_id)
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
    config: &SessionToolsConfig,
    now: DateTime<Utc>,
) -> Result<SweepReport, sqlx::Error> {
    let grace = TimeDelta::from_std(config.machine_unavailable_after).unwrap_or(TimeDelta::MAX);
    let cutoff = now - grace;
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            // Порядок блокировок как у проекции доступности: машины по id, затем подписки.
            let machines: Vec<(Uuid, Uuid, String, Option<DateTime<Utc>>)> = sqlx::query_as(
                "select m.id,m.owner_id,m.availability,m.availability_at from machines m
                 where m.availability='unavailable'
                    or exists (select 1 from machine_outages o
                               where o.machine_id=m.id and o.ended_at is null)
                 order by m.id for no key update of m",
            )
            .fetch_all(&mut *conn)
            .await?;
            let mut report = SweepReport::default();
            for (machine_id, owner_id, availability, availability_at) in machines {
                let machine = MachineRow {
                    id: machine_id,
                    owner_id,
                    availability,
                    availability_at,
                };
                let open: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(
                    "select id,started_at from machine_outages
                     where machine_id=$1 and ended_at is null for update",
                )
                .bind(machine.id)
                .fetch_optional(&mut *conn)
                .await?;
                if machine.availability != "unavailable" {
                    let Some((outage_id, _)) = open else {
                        continue;
                    };
                    sqlx::query("update machine_outages set ended_at=$2 where id=$1")
                        .bind(outage_id)
                        .bind(now)
                        .execute(&mut *conn)
                        .await?;
                    for notice in recover(&mut *conn, outage_id).await? {
                        insert_job(&mut *conn, &notice, outage_id, "available", now).await?;
                        report.recovered += 1;
                    }
                    continue;
                }
                let outage_id = match open {
                    Some((outage_id, _)) => outage_id,
                    None => {
                        let Some(at) = machine.availability_at else {
                            continue;
                        };
                        // Короткий простой не создаёт записи вовсе: возврат до грации молчит.
                        if at > cutoff {
                            continue;
                        }
                        let id = Uuid::now_v7();
                        sqlx::query(
                            "insert into machine_outages(id,machine_id,owner_id,started_at)
                             values($1,$2,$3,$4)",
                        )
                        .bind(id)
                        .bind(machine.id)
                        .bind(machine.owner_id)
                        .bind(at)
                        .execute(&mut *conn)
                        .await?;
                        report.opened += 1;
                        id
                    }
                };
                // Простой уже старше грации; отметка на подписке делает выдачу однократной.
                for notice in announce(&mut *conn, outage_id, machine.id, now).await? {
                    insert_job(&mut *conn, &notice, outage_id, "unavailable", now).await?;
                    report.announced += 1;
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
    config: &SessionToolsConfig,
    now: DateTime<Utc>,
) -> Result<SweepReport, sqlx::Error> {
    const ATTEMPTS: u32 = 3;
    let mut attempt = 0;
    loop {
        attempt += 1;
        match sweep_once(pool, config, now).await {
            Ok(report) => {
                if report.announced + report.recovered > 0 {
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
pub async fn run(pool: PgPool, config: SessionToolsConfig, shutdown: watch::Receiver<bool>) {
    let supervisor = pool.clone();
    crate::singleton::supervise(&supervisor, "machine_outage", shutdown, move || {
        let pool = pool.clone();
        let config = config.clone();
        async move {
            let mut tick = tokio::time::interval(SWEEP_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                if let Err(error) = sweep(&pool, &config, Utc::now()).await {
                    tracing::warn!(
                        kind = "machine_outage_database",
                        error = %crate::dberr::describe(&error),
                        "outage sweep failed"
                    );
                }
            }
        }
    })
    .await;
}
