//! Факты воркера о задачах: применяются только вперёд и только к задачам своей машины.
//! Итог терминальной строки ничего не меняет, а фиксируется в `late_report`.

use canterlot_proto::worker::v1::{
    JobDispatched, JobFinished, JobLogChunk, JobOutcome, JobPhase, JobSnapshot, JobStarted,
    JobWaitingLease, JobsReport,
};
use chrono::{TimeZone, Utc};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{
    Job, LIVE_TAIL_BYTES, Outcome, RESULT_TAIL_BYTES, State, advance_to_in_tx,
    dispatch_machine_in_tx, finish_in_tx, load, lock_machine, lock_row,
};

/// Строка задачи этой машины и этой попытки под замками; `None` — чужая или неизвестная.
async fn locked(
    conn: &mut PgConnection,
    machine_id: Uuid,
    job_id: &str,
    attempt: u32,
) -> Result<Option<Job>, sqlx::Error> {
    let Ok(id) = Uuid::parse_str(job_id) else {
        return Ok(None);
    };
    let owned: Option<Uuid> = sqlx::query_scalar("select machine_id from jobs where id=$1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    if owned != Some(machine_id) {
        return Ok(None);
    }
    lock_machine(conn, machine_id).await?;
    let Some(job) = lock_row(conn, id).await? else {
        return Ok(None);
    };
    if u32::try_from(job.attempt).ok() != Some(attempt) {
        return Ok(None);
    }
    Ok(Some(job))
}

fn finished_state(outcome: i32) -> (State, Option<&'static str>) {
    match JobOutcome::try_from(outcome).unwrap_or(JobOutcome::Unspecified) {
        JobOutcome::Succeeded => (State::Succeeded, None),
        JobOutcome::TimedOut => (State::TimedOut, None),
        JobOutcome::Canceled => (State::Canceled, None),
        JobOutcome::LaunchInterrupted => (State::Failed, Some("launch_interrupted")),
        JobOutcome::WorkdirRefused => (State::Failed, Some("workdir_refused")),
        JobOutcome::Failed | JobOutcome::Unspecified => (State::Failed, None),
    }
}

/// Воркер или хаб отказали команде задачи (`capability_missing`, `unsupported_command`).
/// Отказанный запуск закрывает задачу `failed` с причиной; отказ отмены ничего не меняет:
/// итог всё равно придёт фактом или потерей.
pub async fn apply_command_rejected(
    conn: &mut PgConnection,
    machine_id: Uuid,
    command_id: Uuid,
    kind: &str,
    reason: &str,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    if kind != "start_inplace_job" {
        return Ok(());
    }
    let payload: Option<Value> = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(id) = payload
        .as_ref()
        .and_then(|payload| payload.get("job_id"))
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
    else {
        return Ok(());
    };
    lock_machine(conn, machine_id).await?;
    let Some(job) = lock_row(conn, id).await? else {
        return Ok(());
    };
    if job.machine_id != machine_id || job.is_terminal() {
        return Ok(());
    }
    let reason: String = reason
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect();
    let reason = if reason.is_empty() {
        "rejected".to_owned()
    } else {
        reason
    };
    finish_in_tx(
        conn,
        &job,
        Outcome::bare(State::Failed, Some(&reason)),
        correlation_id,
    )
    .await?;
    dispatch_machine_in_tx(conn, machine_id, correlation_id).await?;
    Ok(())
}

/// Запись о факте после потери: строка остаётся `lost`, но итог видно.
async fn record_late(
    conn: &mut PgConnection,
    job: &Job,
    late: Value,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    // Потеря идёт вперёд только до итога: сообщённый воркером итог не откатывается назад
    // к «идёт» от опоздавшего отчёта.
    let merged = match (&job.late_report, &late) {
        (Some(old), new) if old["phase"] == "finished" && new["phase"] != "finished" => old.clone(),
        _ => late,
    };
    sqlx::query("update jobs set late_report=$2, updated_at=clock_timestamp() where id=$1")
        .bind(job.id)
        .bind(&merged)
        .execute(&mut *conn)
        .await?;
    let moved = load(conn, job.id).await?.expect("job row exists");
    super::emit_changed(conn, &moved, correlation_id).await
}

async fn touch(conn: &mut PgConnection, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("update jobs set last_fact_at=clock_timestamp() where id=$1")
        .bind(id)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn apply_dispatched(
    conn: &mut PgConnection,
    machine_id: Uuid,
    fact: &JobDispatched,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(job) = locked(conn, machine_id, &fact.job_id, fact.attempt).await? else {
        return Ok(());
    };
    if job.is_terminal() {
        return Ok(());
    }
    // Воркер принял команду: она завершена для хаба, дальше следят факты задачи.
    sqlx::query(
        "update commands set delivery_state='completed', completed_at=coalesce(completed_at, now())
         where machine_id=$1 and kind='start_inplace_job' and payload->>'job_id'=$2
           and delivery_state<>'completed'",
    )
    .bind(machine_id)
    .bind(job.id.to_string())
    .execute(&mut *conn)
    .await?;
    if job.state() == State::Queued {
        // Факт раньше хаба невозможен, но «вперёд» остаётся правилом.
        advance_to_in_tx(conn, &job, State::Dispatched, correlation_id).await?;
    } else {
        touch(conn, job.id).await?;
    }
    Ok(())
}

pub async fn apply_waiting_lease(
    conn: &mut PgConnection,
    machine_id: Uuid,
    fact: &JobWaitingLease,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(job) = locked(conn, machine_id, &fact.job_id, fact.attempt).await? else {
        return Ok(());
    };
    if job.is_terminal() {
        if job.state() == State::Lost {
            let late = json!({"phase": "waiting_lease", "at": Utc::now()});
            record_late(conn, &job, late, correlation_id).await?;
        }
        return Ok(());
    }
    if advance_to_in_tx(conn, &job, State::WaitingLease, correlation_id)
        .await?
        .is_none()
    {
        touch(conn, job.id).await?;
    }
    Ok(())
}

pub async fn apply_started(
    conn: &mut PgConnection,
    machine_id: Uuid,
    fact: &JobStarted,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(job) = locked(conn, machine_id, &fact.job_id, fact.attempt).await? else {
        return Ok(());
    };
    if job.is_terminal() {
        if job.state() == State::Lost {
            let late = json!({"phase": "running", "at": Utc::now(), "pid": fact.pid});
            record_late(conn, &job, late, correlation_id).await?;
        }
        return Ok(());
    }
    let started_at = i64::try_from(fact.started_at_ms)
        .ok()
        .and_then(|ms| Utc.timestamp_millis_opt(ms).single());
    sqlx::query(
        "update jobs set pid=$2, proc_start_time=$3, started_at=coalesce(started_at, $4)
         where id=$1",
    )
    .bind(job.id)
    .bind(i64::from(fact.pid))
    .bind(i64::try_from(fact.start_time).ok())
    .bind(started_at)
    .execute(&mut *conn)
    .await?;
    if advance_to_in_tx(conn, &job, State::Running, correlation_id)
        .await?
        .is_none()
    {
        touch(conn, job.id).await?;
    }
    Ok(())
}

pub async fn apply_finished(
    conn: &mut PgConnection,
    machine_id: Uuid,
    fact: &JobFinished,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(job) = locked(conn, machine_id, &fact.job_id, fact.attempt).await? else {
        return Ok(());
    };
    let (state, reason) = finished_state(fact.outcome);
    if job.is_terminal() {
        if job.state() == State::Lost {
            let late = json!({
                "phase": "finished",
                "at": Utc::now(),
                "state": state.as_str(),
                "reason": reason,
                "exit_code": fact.exit_code,
                "duration_ms": fact.duration_ms,
            });
            record_late(conn, &job, late, correlation_id).await?;
            // Потерянная задача освободила место воркера.
            dispatch_machine_in_tx(conn, machine_id, correlation_id).await?;
        }
        return Ok(());
    }
    let keep = fact.tail.len().saturating_sub(RESULT_TAIL_BYTES);
    let tail = fact.tail[keep..].to_vec();
    let reason = reason.map(str::to_owned);
    finish_in_tx(
        conn,
        &job,
        Outcome {
            state,
            reason,
            exit_code: fact.exit_code,
            duration_ms: i64::try_from(fact.duration_ms).ok(),
            output_bytes: i64::try_from(fact.log_bytes).ok(),
            tail: Some(tail),
            truncated: fact.truncated,
        },
        correlation_id,
    )
    .await?;
    dispatch_machine_in_tx(conn, machine_id, correlation_id).await?;
    Ok(())
}

/// Отчёт при подключении: каждая задача применяется вперёд тем же путём, что и факт.
pub async fn apply_report(
    conn: &mut PgConnection,
    machine_id: Uuid,
    report: &JobsReport,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    for snapshot in &report.jobs {
        apply_snapshot(conn, machine_id, snapshot, correlation_id).await?;
    }
    // Машина вернулась: очередь на неё может идти.
    dispatch_machine_in_tx(conn, machine_id, correlation_id).await?;
    Ok(())
}

fn finished_from_snapshot(snapshot: &JobSnapshot) -> JobFinished {
    JobFinished {
        job_id: snapshot.job_id.clone(),
        attempt: snapshot.attempt,
        outcome: snapshot.outcome,
        exit_code: snapshot.exit_code,
        duration_ms: snapshot.duration_ms,
        log_bytes: snapshot.log_bytes,
        tail: Vec::new(),
        truncated: snapshot.truncated,
    }
}

fn snapshot_json(snapshot: &JobSnapshot) -> Value {
    json!({
        "attempt": snapshot.attempt,
        "outcome": snapshot.outcome,
        "exit_code": snapshot.exit_code,
        "duration_ms": snapshot.duration_ms,
        "log_bytes": snapshot.log_bytes,
        "truncated": snapshot.truncated,
    })
}

/// Снимок «закончена» без факта за срок: журнал воркера не доставил `JobFinished`, задача
/// закрывается по снимку без хвоста (`tail_unavailable`), а не висит вечно. Под замками
/// машины и строки; `false` — задача уже закончена или снимка нет.
pub async fn finish_from_report(
    conn: &mut PgConnection,
    machine_id: Uuid,
    id: Uuid,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    lock_machine(conn, machine_id).await?;
    let Some(job) = lock_row(conn, id).await? else {
        return Ok(false);
    };
    let report: Option<Value> = sqlx::query_scalar("select finish_report from jobs where id=$1")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    let Some(report) = report.filter(|_| !job.is_terminal()) else {
        return Ok(false);
    };
    let outcome = report["outcome"]
        .as_i64()
        .and_then(|v| i32::try_from(v).ok());
    let (state, reason) = finished_state(outcome.unwrap_or_default());
    let reason = reason.unwrap_or("tail_unavailable");
    finish_in_tx(
        conn,
        &job,
        Outcome {
            state,
            reason: Some(reason.to_owned()),
            exit_code: report["exit_code"]
                .as_i64()
                .and_then(|v| i32::try_from(v).ok()),
            duration_ms: report["duration_ms"].as_i64(),
            output_bytes: report["log_bytes"].as_i64(),
            tail: None,
            truncated: report["truncated"].as_bool().unwrap_or(false),
        },
        correlation_id,
    )
    .await?;
    dispatch_machine_in_tx(conn, machine_id, correlation_id).await?;
    Ok(true)
}

async fn apply_snapshot(
    conn: &mut PgConnection,
    machine_id: Uuid,
    snapshot: &JobSnapshot,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    match JobPhase::try_from(snapshot.phase).unwrap_or(JobPhase::Unspecified) {
        JobPhase::Dispatched => {
            let fact = JobDispatched {
                job_id: snapshot.job_id.clone(),
                attempt: snapshot.attempt,
                ..Default::default()
            };
            apply_dispatched(conn, machine_id, &fact, correlation_id).await
        }
        JobPhase::WaitingLease => {
            let fact = JobWaitingLease {
                job_id: snapshot.job_id.clone(),
                attempt: snapshot.attempt,
                lease: 0,
            };
            apply_waiting_lease(conn, machine_id, &fact, correlation_id).await
        }
        JobPhase::Running => {
            let fact = JobStarted {
                job_id: snapshot.job_id.clone(),
                attempt: snapshot.attempt,
                pid: snapshot.pid,
                start_time: snapshot.start_time,
                started_at_ms: snapshot.started_at_ms,
            };
            apply_started(conn, machine_id, &fact, correlation_id).await
        }
        JobPhase::Finished => {
            let fact = finished_from_snapshot(snapshot);
            // Снимок не несёт хвоста вывода. Если задача ещё не закончена на хабе и у неё есть
            // вывод, итог не фиксируется: журнал воркера доставит `JobFinished` с хвостом,
            // а закончить задачу без него значило бы навсегда потерять хвост (поздний факт
            // уже ничего не меняет). Снимок запоминается; не пришёл факт за срок —
            // `sweep::reported_finish_sweep` закрывает задачу по снимку.
            if let Some(job) = locked(conn, machine_id, &snapshot.job_id, snapshot.attempt).await?
                && !job.is_terminal()
                && snapshot.log_bytes > 0
            {
                sqlx::query(
                    "update jobs set finish_report=$2,
                            finish_reported_at=coalesce(finish_reported_at, clock_timestamp()),
                            last_fact_at=clock_timestamp()
                     where id=$1",
                )
                .bind(job.id)
                .bind(snapshot_json(snapshot))
                .execute(&mut *conn)
                .await?;
                return Ok(());
            }
            apply_finished(conn, machine_id, &fact, correlation_id).await
        }
        JobPhase::Unspecified => Ok(()),
    }
}

/// Живой кусок: непостоянный, поэтому ничего не публикует. Хаб держит последние 64 КиБ.
pub async fn apply_chunk(
    conn: &mut PgConnection,
    machine_id: Uuid,
    chunk: &JobLogChunk,
) -> Result<(), sqlx::Error> {
    let Ok(id) = Uuid::parse_str(&chunk.job_id) else {
        return Ok(());
    };
    let Ok(offset) = i64::try_from(chunk.offset) else {
        return Ok(());
    };
    let row: Option<(Vec<u8>, i64, String, i32)> = sqlx::query_as(
        "select live_tail, live_offset, state, attempt from jobs
         where id=$1 and machine_id=$2 for update",
    )
    .bind(id)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((mut tail, tail_end, state, attempt)) = row else {
        return Ok(());
    };
    if u32::try_from(attempt).ok() != Some(chunk.attempt)
        || State::parse(&state).is_none_or(State::is_terminal)
        || chunk.data.is_empty()
    {
        return Ok(());
    }
    // `live_offset` — позиция конца хвоста в потоке вывода. Кусок раньше конца пропускается,
    // дыра сбрасывает хвост: после неё склеивать нечего.
    let end = offset.saturating_add(i64::try_from(chunk.data.len()).unwrap_or(i64::MAX));
    if end <= tail_end {
        return Ok(());
    }
    if offset > tail_end {
        tail.clear();
    }
    let skip = usize::try_from((tail_end - offset).max(0)).unwrap_or(0);
    let skip = if offset > tail_end { 0 } else { skip };
    tail.extend_from_slice(&chunk.data[skip.min(chunk.data.len())..]);
    if tail.len() > LIVE_TAIL_BYTES {
        tail.drain(..tail.len() - LIVE_TAIL_BYTES);
    }
    sqlx::query("update jobs set live_tail=$2, live_offset=$3 where id=$1")
        .bind(id)
        .bind(tail)
        .bind(end)
        .execute(conn)
        .await?;
    Ok(())
}
