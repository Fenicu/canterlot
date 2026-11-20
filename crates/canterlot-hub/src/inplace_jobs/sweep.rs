//! Служба задач на месте: старт хаба, подключение машины, потерянные задачи.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::sync::watch;
use uuid::Uuid;

use super::{
    Outcome, State, dispatch_machine_in_tx, fallback_to_message_in_tx, finish_in_tx, lock_machine,
    lock_row,
};
use crate::session::{Availability, Registry};

const TICK: Duration = Duration::from_secs(30);

/// Старт хаба: ждавший вызов, если и был, умер вместе с процессом. Каждая задача без выбранной
/// доставки уходит на сообщение; уже терминальная ставит его в той же транзакции.
pub async fn on_start(pool: &PgPool) -> Result<usize, sqlx::Error> {
    let ids: Vec<Uuid> =
        sqlx::query_scalar("select id from jobs where delivery is null order by id")
            .fetch_all(pool)
            .await?;
    let mut done = 0;
    for id in ids {
        crate::db::in_tx(pool, move |conn| {
            Box::pin(async move { fallback_to_message_in_tx(conn, id).await })
        })
        .await?;
        done += 1;
    }
    Ok(done)
}

/// Задачи, о которых машина молчит дольше срока команды и запаса, становятся `lost`.
/// Молчание считается от последнего факта; машина на связи не теряет задач: её факты придут.
pub async fn lost_sweep(
    pool: &PgPool,
    registry: &Registry,
    grace: Duration,
    now: DateTime<Utc>,
) -> Result<usize, sqlx::Error> {
    let candidates: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select id, machine_id from jobs
         where state in ('dispatched','waiting_lease','running')
           and coalesce(last_fact_at, dispatched_at, created_at)
               + make_interval(secs => timeout_seconds + $1) < $2
         order by created_at, id",
    )
    .bind(grace.as_secs_f64())
    .bind(now)
    .fetch_all(pool)
    .await?;
    let mut lost = 0;
    for (id, machine_id) in candidates {
        if registry.availability(machine_id) == Availability::Available {
            continue;
        }
        let ended = crate::db::in_tx(pool, move |conn| {
            Box::pin(async move {
                lock_machine(conn, machine_id).await?;
                let Some(job) = lock_row(conn, id).await? else {
                    return Ok::<_, sqlx::Error>(false);
                };
                if job.is_terminal() || job.state() == State::Queued {
                    return Ok(false);
                }
                finish_in_tx(
                    conn,
                    &job,
                    Outcome::bare(State::Lost, Some("lost")),
                    Uuid::now_v7(),
                )
                .await?;
                // Потеря снимает место, пока воркер не скажет, что задача идёт.
                dispatch_machine_in_tx(conn, machine_id, Uuid::now_v7()).await?;
                Ok(true)
            })
        })
        .await?;
        lost += usize::from(ended);
    }
    Ok(lost)
}

/// Машины с задачами в очереди.
async fn queued_machines(pool: &PgPool) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar("select distinct machine_id from jobs where state='queued'")
        .fetch_all(pool)
        .await
}

pub async fn dispatch_machine(pool: &PgPool, machine_id: Uuid) -> Result<(), sqlx::Error> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            dispatch_machine_in_tx(conn, machine_id, Uuid::now_v7())
                .await
                .map(|_| ())
        })
    })
    .await
}

/// Служба-одиночка: старт, подключение машины и таймер.
pub async fn run(
    pool: PgPool,
    registry: Registry,
    grace: Duration,
    shutdown: watch::Receiver<bool>,
) {
    let supervisor = pool.clone();
    crate::singleton::supervise(&supervisor, "inplace_jobs", shutdown, move || {
        let pool = pool.clone();
        let registry = registry.clone();
        async move {
            if let Err(error) = on_start(&pool).await {
                tracing::warn!(
                    kind = "inplace_jobs_start",
                    error = %crate::dberr::describe(&error),
                    "in-place jobs start sweep failed"
                );
            }
            let mut changes = registry.subscribe_changes();
            let mut tick = tokio::time::interval(TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let result = tokio::select! {
                    change = changes.recv() => match change {
                        Ok((machine, Availability::Available)) => dispatch_machine(&pool, machine).await,
                        _ => Ok(()),
                    },
                    _ = tick.tick() => sweep_once(&pool, &registry, grace).await,
                };
                if let Err(error) = result {
                    tracing::warn!(
                        kind = "inplace_jobs_database",
                        error = %crate::dberr::describe(&error),
                        "in-place jobs sweep failed"
                    );
                }
            }
        }
    })
    .await;
}

async fn sweep_once(
    pool: &PgPool,
    registry: &Registry,
    grace: Duration,
) -> Result<(), sqlx::Error> {
    lost_sweep(pool, registry, grace, Utc::now()).await?;
    for machine in queued_machines(pool).await? {
        if registry.availability(machine) == Availability::Available {
            dispatch_machine(pool, machine).await?;
        }
    }
    Ok(())
}
