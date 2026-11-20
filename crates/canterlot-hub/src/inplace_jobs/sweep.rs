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

/// Задачи, у которых давно нет выбранной доставки: ждущий вызов живёт не дольше минуты, так
/// что за `older_than` его уже нет, а оборванный в окне между фиксацией заявки и стражем
/// вызов не оставил сообщения. Они уходят сообщением, как при старте хаба.
pub async fn undelivered_sweep(
    pool: &PgPool,
    older_than: Duration,
    now: DateTime<Utc>,
) -> Result<usize, sqlx::Error> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select id from jobs
         where delivery is null and created_at < $1 - make_interval(secs => $2)
         order by id",
    )
    .bind(now)
    .bind(older_than.as_secs_f64())
    .fetch_all(pool)
    .await?;
    let count = ids.len();
    for id in ids {
        crate::db::in_tx(pool, move |conn| {
            Box::pin(async move { fallback_to_message_in_tx(conn, id).await })
        })
        .await?;
    }
    Ok(count)
}

/// Срок хранения полного журнала вышел: вложение отпускается (сборщик блобов уберёт файл),
/// строка хранит метаданные и хвост, а чтение отвечает хвостом с пометкой. `retention = 0`
/// не убирает ничего.
pub async fn expire_logs(
    pool: &PgPool,
    retention: Duration,
    now: DateTime<Utc>,
) -> Result<usize, sqlx::Error> {
    if retention.is_zero() {
        return Ok(0);
    }
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select id from jobs
         where log_media_id is not null
           and state in ('succeeded','failed','timed_out','canceled','lost')
           and finished_at < $1 - make_interval(secs => $2)
         order by id limit 500",
    )
    .bind(now)
    .bind(retention.as_secs_f64())
    .fetch_all(pool)
    .await?;
    let mut done = 0;
    for id in ids {
        let released = crate::db::in_tx(pool, move |conn| {
            Box::pin(async move {
                let released = sqlx::query(
                    "update jobs set log_media_id=null, log_expired_at=$2, updated_at=clock_timestamp()
                     where id=$1 and log_media_id is not null",
                )
                .bind(id)
                .bind(now)
                .execute(&mut *conn)
                .await?
                .rows_affected()
                    > 0;
                if released && let Some(job) = super::load(conn, id).await? {
                    super::emit_changed(conn, &job, Uuid::now_v7()).await?;
                }
                Ok::<_, sqlx::Error>(released)
            })
        })
        .await?;
        done += usize::from(released);
    }
    Ok(done)
}

/// Дольше самого долгого ожидания вызова (60 с) с запасом на сеть и разбор.
const UNDELIVERED_AFTER: Duration = Duration::from_secs(3 * 60);

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
    config: crate::config::JobsConfig,
    shutdown: watch::Receiver<bool>,
) {
    let (grace, retention) = (config.lost_grace, config.log_retention);
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
                    _ = tick.tick() => sweep_once(&pool, &registry, grace, retention).await,
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
    retention: Duration,
) -> Result<(), sqlx::Error> {
    lost_sweep(pool, registry, grace, Utc::now()).await?;
    undelivered_sweep(pool, UNDELIVERED_AFTER, Utc::now()).await?;
    expire_logs(pool, retention, Utc::now()).await?;
    for machine in queued_machines(pool).await? {
        if registry.availability(machine) == Availability::Available {
            dispatch_machine(pool, machine).await?;
        }
    }
    Ok(())
}
