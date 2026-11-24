//! Один повтор задачи, потерянной вместе с прерванной машиной (вытесняемый облачный сервер).
//!
//! Ход: отзыв машины с `RevokeJobs::RetryOnce` переводит задачу, которой положен повтор, в
//! `retry_pending` вместо `lost` (`hold_or_lose_in_tx`). В этом состоянии задача не размещается,
//! не теряется по сроку и не принимает фактов: прежний процесс ещё может идти. Когда облачный
//! код подтвердил, что прежняя машина остановлена или её нет, он выпускает задачи
//! (`release_retry_pending_in_tx`): попытка растёт на единицу, задача возвращается в
//! `waiting_runner`. Если повтора не будет, он же их закрывает (`lose_retry_pending_in_tx`).
//!
//! Попытка — идентичность запуска: её несут команды раннеру и несущей, их факты и имя рабочего
//! каталога. Факт не текущей попытки ничего не меняет (`accepts_fact`).
//!
//! Журнал и вывод прежней попытки не хранятся: у задачи одно вложение журнала и один хвост, и
//! при выпуске они сбрасываются вместе с остальным состоянием попытки.

use sqlx::PgConnection;
use uuid::Uuid;

use super::{Job, State, carrier, emit_changed, load, lock_machine, lock_row};

/// Факт относится к текущей попытке задачи, и задача не ждёт повтора. Иначе он ничего не меняет.
pub fn accepts_fact(job: &Job, attempt: u32, fact: &'static str) -> bool {
    if job.state() == State::RetryPending {
        tracing::debug!(
            kind = "job_fact_ignored",
            fact,
            job_id = %job.id,
            attempt,
            "job fact ignored: the job waits for its retry"
        );
        return false;
    }
    if u32::try_from(job.attempt).ok() != Some(attempt) {
        tracing::debug!(
            kind = "job_fact_ignored",
            fact,
            job_id = %job.id,
            attempt,
            current = job.attempt,
            "job fact ignored: not the current attempt"
        );
        return false;
    }
    true
}

/// Задаче положен повтор после прерывания машины `machine`: повтора ещё не было, у неё есть
/// исходники (чистый checkout коммита, который несущая подготовит заново), отмены не просили и
/// нет свидетельства, что команда закончилась. Несущая сама должна остаться.
fn retryable(job: &Job, machine: Uuid) -> bool {
    job.preemption_retries == 0
        && job.has_source()
        && job.carrier_id != Some(machine)
        && job.cancel_requested_at.is_none()
        && job.finish_report.is_none()
        && job.outcome_state.is_none()
        && job.exit_code.is_none()
        && !matches!(job.state(), State::Collecting | State::RetryPending)
}

/// Команды попытки гасятся: запуск и отмена у раннера, подготовка, сбор и очистка у несущей.
async fn retire_commands(conn: &mut PgConnection, job: &Job) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update commands set delivery_state='completed', completed_at=coalesce(completed_at, now())
         where ((machine_id=$1 and kind in ('start_inplace_job','cancel_inplace_job'))
             or (machine_id=$3 and kind in ('stage_source','collect_outputs','cleanup_source')))
           and payload->>'job_id'=$2 and delivery_state<>'completed'",
    )
    .bind(job.machine_id)
    .bind(job.id.to_string())
    .bind(job.carrier_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Отзыв прерванной машины: каждая задача из `ids` (те, что обычный отзыв закончил бы `lost`)
/// под замком строки уходит в `retry_pending`, если ей положен повтор, иначе в `lost`. Замок
/// машины держит вызывающий.
pub(crate) async fn hold_or_lose_in_tx(
    conn: &mut PgConnection,
    machine: Uuid,
    ids: Vec<Uuid>,
) -> Result<(), sqlx::Error> {
    let mut lose = Vec::new();
    for id in ids {
        let Some(job) = lock_row(&mut *conn, id).await? else {
            continue;
        };
        if job.is_terminal() {
            continue;
        }
        if !retryable(&job, machine) {
            lose.push(id);
            continue;
        }
        // Команды прежней попытки не должны дойти до машины с тем же идентификатором.
        retire_commands(conn, &job).await?;
        sqlx::query(
            "update jobs set state='retry_pending', last_fact_at=clock_timestamp(),
                    updated_at=clock_timestamp()
             where id=$1",
        )
        .bind(id)
        .execute(&mut *conn)
        .await?;
        let held = load(conn, id).await?.expect("job row just updated");
        emit_changed(conn, &held, Uuid::now_v7()).await?;
    }
    crate::machines::finish_lost_in_tx(conn, lose).await
}

/// Ждущие повтора задачи машины: шедшие на ней и названные на неё.
async fn pending_of(conn: &mut PgConnection, machine: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select id from jobs
         where state='retry_pending' and (machine_id=$1 or requested_machine_id=$1)
         order by created_at, id",
    )
    .bind(machine)
    .fetch_all(conn)
    .await
}

/// Прежняя машина подтверждена остановленной или её нет: её ждущие повтора задачи возвращаются
/// в `waiting_runner` новой попыткой, и их видят расстановка и расчёт спроса на облако.
/// Вызывающий отвечает за то, что прежний процесс идти уже не может. Берёт замок машины, затем
/// строки задач по порядку подачи. Возвращает выпущенные задачи.
pub async fn release_retry_pending_in_tx(
    conn: &mut PgConnection,
    machine: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    lock_machine(conn, machine).await?;
    let mut released = Vec::new();
    for id in pending_of(conn, machine).await? {
        let Some(job) = lock_row(&mut *conn, id).await? else {
            continue;
        };
        if job.state() != State::RetryPending {
            continue;
        }
        retire_commands(conn, &job).await?;
        // Всё, что принадлежало прежней попытке: место, факты, вывод, журнал, итог сбора и
        // очистка. Названная машина остаётся: задача ждёт именно её.
        sqlx::query(
            "update jobs set state='waiting_runner', attempt=attempt+1,
                    preemption_retries=preemption_retries+1,
                    placement_waiting_since=clock_timestamp(),
                    machine_id=null, workdir=null, remote=false, admission_generation=null,
                    pid=null, proc_start_time=null, dispatched_at=null, started_at=null,
                    last_fact_at=null, late_report=null, finish_report=null,
                    finish_reported_at=null, end_reason=null, exit_code=null, duration_ms=null,
                    output_bytes=0, truncated=false, tail=null, live_tail=''::bytea,
                    live_offset=0, log_media_id=null, log_size=null, log_sha256=null,
                    log_expired_at=null, outcome_state=null, outcome_reason=null,
                    collecting_at=null, outputs_dir=null, outputs_note=null, outputs_files=null,
                    cleanup_attempts=0, cleanup_sent_at=null, cleaned_at=null,
                    updated_at=clock_timestamp()
             where id=$1",
        )
        .bind(id)
        .execute(&mut *conn)
        .await?;
        let waiting = load(conn, id).await?.expect("job row just updated");
        emit_changed(conn, &waiting, Uuid::now_v7()).await?;
        carrier::notify_place_for_machine(conn, waiting.carrier_id).await?;
        released.push(id);
    }
    Ok(released)
}

/// Повтора не будет: ждущие повтора задачи машины заканчиваются `lost`, каждая с одним
/// сообщением об итоге. Возвращает законченные задачи.
pub async fn lose_retry_pending_in_tx(
    conn: &mut PgConnection,
    machine: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    lock_machine(conn, machine).await?;
    let ids = pending_of(conn, machine).await?;
    crate::machines::finish_lost_in_tx(conn, ids.clone()).await?;
    Ok(ids)
}
