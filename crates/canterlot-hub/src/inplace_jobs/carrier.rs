//! Команды несущей машине для задач с исходниками и факты, которыми она отвечает:
//! `StageSource` → `SourceStaged`/`SourceStageFailed`, `CollectOutputs` →
//! `OutputsCollected`/`OutputsCollectFailed`, `CleanupSource` → `SourceCleaned`. Команды долговечны
//! (строки `commands` уровня машины), идемпотентны по `job_id` и уходят только на соединении
//! с `runners_v1`. Факты принимаются только от `carrier_id` задачи и только через допуск
//! несущей (роль `worker`, флаг `is_carrier`); чужой факт ничего не меняет.
//!
//! Порядок после конца на раннере: без путей результатов задача кончается сразу; с путями —
//! `collecting` и сбор, и конец по факту сбора. Очистка выдаётся после конца
//! (`finish_in_tx`), её повтор ведёт `cleanup_retry_sweep`, а итог, сообщение и ждущий вызов её
//! не ждут. Сбой или срок сбора заканчивает задачу итогом раннера с пометкой
//! `outputs not collected`.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use canterlot_proto::worker::v1::{
    OutputsCollectFailed, OutputsCollected, SourceCleaned, SourceStageFailed, SourceStaged,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{
    Job, Outcome, State, dispatch_machine_in_tx, emit_changed, finish_in_tx, load, lock_machine,
    lock_row, notify_outbox, place::repo_key,
};
use crate::session::{Availability, Registry};

/// Предел суммарного размера собранных результатов (`[jobs].outputs_cap`); задаётся при
/// старте хаба.
pub static OUTPUTS_CAP: AtomicU64 = AtomicU64::new(DEFAULT_OUTPUTS_CAP);
pub const DEFAULT_OUTPUTS_CAP: u64 = 512 * 1024 * 1024;

/// Сколько раз выдаётся очистка, пока несущая не подтвердила.
pub const MAX_CLEANUP_ATTEMPTS: i32 = 5;
/// Сколько ждать подтверждения очистки до повторной выдачи.
pub const CLEANUP_RETRY_AFTER: Duration = Duration::from_secs(10 * 60);

pub fn set_outputs_cap(bytes: u64) {
    OUTPUTS_CAP.store(bytes, Ordering::Relaxed);
}

/// Причина сбоя в короткую безопасную строку для записи и сообщения.
fn clean_reason(reason: &str) -> String {
    let cleaned: String = reason
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "unknown".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Корень цели для команд: пусто, когда цель — сама несущая.
async fn target_root(conn: &mut PgConnection, job: &Job) -> Result<(String, String), sqlx::Error> {
    let Some(machine) = job.machine_id else {
        return Ok((String::new(), String::new()));
    };
    if Some(machine) == job.carrier_id {
        return Ok((machine.to_string(), String::new()));
    }
    let root: Option<String> = sqlx::query_scalar("select root_dir from machines where id=$1")
        .bind(machine)
        .fetch_optional(conn)
        .await?
        .flatten();
    Ok((machine.to_string(), root.unwrap_or_default()))
}

async fn insert_carrier_command(
    conn: &mut PgConnection,
    carrier: Uuid,
    kind: &str,
    payload: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, execution_id, execution_generation,
                               kind, payload, delivery_state)
         values ($1, $2, null, null, null, $3, $4, 'pending')",
    )
    .bind(Uuid::now_v7())
    .bind(carrier)
    .bind(kind)
    .bind(payload)
    .execute(&mut *conn)
    .await?;
    notify_outbox(conn, carrier).await
}

/// `StageSource` после привязки. `target_root` — корень раннера или `None` для самой несущей.
pub async fn send_stage_in_tx(
    conn: &mut PgConnection,
    job: &Job,
    target_root: Option<&str>,
) -> Result<(), sqlx::Error> {
    let (Some(carrier), Some(repo), Some(rev), Some(domain), Some(machine), Some(workdir)) = (
        job.carrier_id,
        job.source_repo.as_deref(),
        job.source_rev.as_deref(),
        job.source_domain.as_deref(),
        job.machine_id,
        job.workdir.as_deref(),
    ) else {
        return Ok(());
    };
    insert_carrier_command(
        conn,
        carrier,
        "stage_source",
        json!({
            "job_id": job.id,
            "repo": repo,
            "rev": rev,
            "domain": domain,
            "target_machine_id": machine,
            "target_root_dir": target_root.unwrap_or_default(),
            "workdir": workdir,
            "repo_key": repo_key(repo),
        }),
    )
    .await
}

async fn send_collect_in_tx(conn: &mut PgConnection, job: &Job) -> Result<(), sqlx::Error> {
    let (Some(carrier), Some(workdir)) = (job.carrier_id, job.workdir.as_deref()) else {
        return Ok(());
    };
    let (machine, root) = target_root(conn, job).await?;
    insert_carrier_command(
        conn,
        carrier,
        "collect_outputs",
        json!({
            "job_id": job.id,
            "target_machine_id": machine,
            "target_root_dir": root,
            "workdir": workdir,
            "paths": job.outputs,
            "cap_bytes": OUTPUTS_CAP.load(Ordering::Relaxed),
        }),
    )
    .await
}

async fn send_cleanup_in_tx(conn: &mut PgConnection, job: &Job) -> Result<(), sqlx::Error> {
    let (Some(carrier), Some(workdir), Some(repo), Some(domain)) = (
        job.carrier_id,
        job.workdir.as_deref(),
        job.source_repo.as_deref(),
        job.source_domain.as_deref(),
    ) else {
        return Ok(());
    };
    let (machine, root) = target_root(conn, job).await?;
    insert_carrier_command(
        conn,
        carrier,
        "cleanup_source",
        json!({
            "job_id": job.id,
            "target_machine_id": machine,
            "target_root_dir": root,
            "workdir": workdir,
            "repo_key": repo_key(repo),
            "domain": domain,
        }),
    )
    .await
}

/// Очистка исходников выдаётся один раз на задачу (повтор — отдельная служба по сроку).
pub async fn ensure_cleanup_in_tx(conn: &mut PgConnection, job: &Job) -> Result<(), sqlx::Error> {
    if job.cleaned_at.is_some() || job.cleanup_sent_at.is_some() || !job.has_source() {
        return Ok(());
    }
    send_cleanup_in_tx(conn, job).await?;
    sqlx::query(
        "update jobs set cleanup_sent_at=clock_timestamp(), cleanup_attempts=cleanup_attempts+1
         where id=$1",
    )
    .bind(job.id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Итог задачи владельца уходит терминалу, который её подал: долговечная команда несущей.
pub async fn enqueue_owner_finished_in_tx(
    conn: &mut PgConnection,
    job: &Job,
) -> Result<(), sqlx::Error> {
    let Some(carrier) = job.carrier_id else {
        return Ok(());
    };
    let exists: bool = sqlx::query_scalar(
        "select exists(select 1 from commands where machine_id=$1 and kind='owner_job_finished'
                         and payload->>'job_id'=$2)",
    )
    .bind(carrier)
    .bind(job.id.to_string())
    .fetch_one(&mut *conn)
    .await?;
    if exists {
        return Ok(());
    }
    insert_carrier_command(
        conn,
        carrier,
        "owner_job_finished",
        json!({
            "job_id": job.id,
            "state": job.state,
            "exit_code": job.exit_code,
            "tail_hex": job.tail.as_deref().map(hex).unwrap_or_default(),
            "outputs_dir": job.outputs_dir,
            "outputs_note": job.outputs_note,
        }),
    )
    .await
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Результаты стоит собирать, когда команда реально шла: отмена и отказ запуска собирать нечего.
pub fn wants_collect(outputs: &[String], state: State, reason: &Option<String>) -> bool {
    !outputs.is_empty()
        && matches!(state, State::Succeeded | State::Failed | State::TimedOut)
        && !matches!(
            reason.as_deref(),
            Some("launch_interrupted" | "workdir_refused")
        )
}

/// Раннер закончил, а результаты просили: итог сохраняется, задача переходит в `collecting` и
/// несущей уходит команда сбора. Конец наступит по факту сбора или по сроку. Под замками
/// машины и строки.
pub async fn begin_collecting_in_tx(
    conn: &mut PgConnection,
    job: &Job,
    outcome: Outcome,
    correlation_id: Uuid,
) -> Result<Option<Job>, sqlx::Error> {
    sqlx::query(
        "update jobs set state='collecting', outcome_state=$2, outcome_reason=$3, exit_code=$4,
                duration_ms=$5, output_bytes=coalesce($6, output_bytes), tail=coalesce($7, tail),
                truncated=$8, collecting_at=clock_timestamp(), last_fact_at=clock_timestamp(),
                updated_at=clock_timestamp()
         where id=$1",
    )
    .bind(job.id)
    .bind(outcome.state.as_str())
    .bind(&outcome.reason)
    .bind(outcome.exit_code)
    .bind(outcome.duration_ms)
    .bind(outcome.output_bytes)
    .bind(&outcome.tail)
    .bind(outcome.truncated)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "update commands set delivery_state='completed', completed_at=coalesce(completed_at, now())
         where machine_id=$1 and kind in ('start_inplace_job','cancel_inplace_job')
           and payload->>'job_id'=$2 and delivery_state<>'completed'",
    )
    .bind(job.machine_id)
    .bind(job.id.to_string())
    .execute(&mut *conn)
    .await?;
    let moved = load(conn, job.id).await?.expect("job row just updated");
    send_collect_in_tx(conn, &moved).await?;
    notify_place_for_machine(conn, moved.machine_id).await?;
    let moved = load(conn, job.id).await?.expect("job row exists");
    emit_changed(conn, &moved, correlation_id).await?;
    Ok(Some(moved))
}

/// Освободилось место на машине: расстановке её несущей пора идти.
pub async fn notify_place_for_machine(
    conn: &mut PgConnection,
    machine: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let Some(machine) = machine else {
        return Ok(());
    };
    sqlx::query(
        "select pg_notify('canterlot_place', coalesce(
           (select case when role='runner' then parent_machine_id else id end
              from machines where id=$1), $1)::text)",
    )
    .bind(machine)
    .execute(conn)
    .await?;
    Ok(())
}

/// Настоящий конец задачи из `collecting`: итог раннера и, если есть, каталог результатов
/// и пометка. Задача уже не в `collecting`, когда вызов повторён.
pub async fn finish_collecting_in_tx(
    conn: &mut PgConnection,
    job: &Job,
    note: Option<&str>,
    correlation_id: Uuid,
) -> Result<Option<Job>, sqlx::Error> {
    if job.state() != State::Collecting {
        return Ok(None);
    }
    if let Some(note) = note {
        sqlx::query(
            "update jobs set outputs_note=coalesce(outputs_note, $2), updated_at=clock_timestamp()
             where id=$1",
        )
        .bind(job.id)
        .bind(note)
        .execute(&mut *conn)
        .await?;
    }
    let current = load(conn, job.id).await?.expect("job row exists");
    let state = current
        .outcome_state
        .as_deref()
        .and_then(State::parse)
        .filter(|state| state.is_terminal())
        .unwrap_or(State::Failed);
    finish_in_tx(
        conn,
        &current,
        Outcome {
            state,
            reason: current.outcome_reason.clone(),
            exit_code: current.exit_code,
            duration_ms: current.duration_ms,
            output_bytes: Some(current.output_bytes),
            tail: current.tail.clone(),
            truncated: current.truncated,
        },
        correlation_id,
    )
    .await
}

// ───────── Факты несущей ─────────

/// Строка задачи под замками для факта несущей `reporter`; `None` — факт чужой или задачи нет.
/// Чужой факт пишет предупреждение и ничего не меняет.
async fn locked_for_carrier(
    conn: &mut PgConnection,
    reporter: Uuid,
    job_id: &str,
    fact: &'static str,
) -> Result<Option<Job>, sqlx::Error> {
    let Ok(id) = Uuid::parse_str(job_id) else {
        return Ok(None);
    };
    let row: Option<(Option<Uuid>, Option<Uuid>)> =
        sqlx::query_as("select carrier_id, machine_id from jobs where id=$1")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((carrier, machine)) = row else {
        return Ok(None);
    };
    let gate: bool = sqlx::query_scalar(
        "select coalesce((select role = 'worker' and is_carrier from machines where id = $1 for share), false)",
    )
    .bind(reporter)
    .fetch_one(&mut *conn)
    .await?;
    if carrier != Some(reporter) || !gate {
        tracing::warn!(
            kind = "carrier_fact_refused",
            fact,
            machine_id = %reporter,
            job_id = %id,
            "carrier fact refused: not the carrier of the job"
        );
        return Ok(None);
    }
    if let Some(machine) = machine {
        lock_machine(conn, machine).await?;
    }
    lock_row(conn, id).await
}

async fn complete_commands(
    conn: &mut PgConnection,
    carrier: Uuid,
    job: Uuid,
    kind: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update commands set delivery_state='completed', completed_at=coalesce(completed_at, now())
         where machine_id=$1 and kind=$2 and payload->>'job_id'=$3 and delivery_state<>'completed'",
    )
    .bind(carrier)
    .bind(kind)
    .bind(job.to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// Несущая отказала команде (`capability_missing`, `unsupported_command`): подготовка
/// заканчивает задачу `failed`, сбор — пометкой; очистка и итог владельцу ничего не меняют.
pub async fn apply_command_rejected(
    conn: &mut PgConnection,
    machine_id: Uuid,
    command_id: Uuid,
    kind: &str,
    reason: &str,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let payload: Option<Value> = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(job_id) = payload
        .as_ref()
        .and_then(|payload| payload.get("job_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(());
    };
    let reason = format!("rejected: {}", clean_reason(reason));
    match kind {
        "stage_source" => {
            let fact = SourceStageFailed { job_id, reason };
            apply_stage_failed(conn, machine_id, &fact, correlation_id).await
        }
        "collect_outputs" => {
            let fact = OutputsCollectFailed { job_id, reason };
            apply_collect_failed(conn, machine_id, &fact, correlation_id).await
        }
        _ => Ok(()),
    }
}

/// `SourceStaged`: подготовка закончена, задача встаёт в очередь машины и уходит воркеру.
pub async fn apply_source_staged(
    conn: &mut PgConnection,
    reporter: Uuid,
    fact: &SourceStaged,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(job) = locked_for_carrier(conn, reporter, &fact.job_id, "source_staged").await? else {
        return Ok(());
    };
    complete_commands(conn, reporter, job.id, "stage_source").await?;
    if job.is_terminal() {
        // Задачу закончили (отмена, срок подготовки), пока подготовка ещё шла: очистка, что
        // несущая сделала раньше (или ещё сделает), не застала ни checkout, ни ссылки. Прежнее
        // подтверждение не считается, очистка выдаётся заново.
        if job.has_source() && job.machine_id.is_some() {
            sqlx::query(
                "update jobs set cleaned_at=null, cleanup_sent_at=null, cleanup_attempts=0
                 where id=$1",
            )
            .bind(job.id)
            .execute(&mut *conn)
            .await?;
            let reset = load(conn, job.id).await?.expect("job row exists");
            ensure_cleanup_in_tx(conn, &reset).await?;
        }
        return Ok(());
    }
    if job.state() != State::Staging {
        return Ok(());
    }
    let Some(machine) = job.machine_id else {
        return Ok(());
    };
    sqlx::query(
        "update jobs set state='queued', last_fact_at=clock_timestamp(), updated_at=clock_timestamp()
         where id=$1",
    )
    .bind(job.id)
    .execute(&mut *conn)
    .await?;
    let moved = load(conn, job.id).await?.expect("job row just updated");
    emit_changed(conn, &moved, correlation_id).await?;
    dispatch_machine_in_tx(conn, machine, correlation_id).await?;
    Ok(())
}

/// `SourceStageFailed`: задача заканчивается `failed` с причиной.
pub async fn apply_stage_failed(
    conn: &mut PgConnection,
    reporter: Uuid,
    fact: &SourceStageFailed,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(job) = locked_for_carrier(conn, reporter, &fact.job_id, "source_stage_failed").await?
    else {
        return Ok(());
    };
    complete_commands(conn, reporter, job.id, "stage_source").await?;
    if job.state() != State::Staging {
        return Ok(());
    }
    let reason = format!("staging_failed: {}", clean_reason(&fact.reason));
    finish_in_tx(
        conn,
        &job,
        Outcome::bare(State::Failed, Some(&reason)),
        correlation_id,
    )
    .await?;
    if let Some(machine) = job.machine_id {
        dispatch_machine_in_tx(conn, machine, correlation_id).await?;
    }
    Ok(())
}

/// `OutputsCollected`: каталог и список файлов сохраняются, дальше очистка.
pub async fn apply_outputs_collected(
    conn: &mut PgConnection,
    reporter: Uuid,
    fact: &OutputsCollected,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(job) = locked_for_carrier(conn, reporter, &fact.job_id, "outputs_collected").await?
    else {
        return Ok(());
    };
    complete_commands(conn, reporter, job.id, "collect_outputs").await?;
    if job.state() != State::Collecting {
        return Ok(());
    }
    let files: Vec<Value> = fact
        .files
        .iter()
        .map(|file| json!({"path": file.path, "bytes": file.bytes}))
        .collect();
    let note = fact
        .truncated
        .then(|| "outputs truncated at the size cap".to_owned());
    sqlx::query(
        "update jobs set outputs_dir=$2, outputs_files=$3, outputs_note=$4,
                updated_at=clock_timestamp() where id=$1",
    )
    .bind(job.id)
    .bind(&fact.local_dir)
    .bind(Value::Array(files))
    .bind(note)
    .execute(&mut *conn)
    .await?;
    // Сбор закончен: задача кончается итогом раннера; очистку выдаёт `finish_in_tx`.
    let moved = load(conn, job.id).await?.expect("job row exists");
    finish_collecting_in_tx(conn, &moved, None, correlation_id).await?;
    Ok(())
}

/// `OutputsCollectFailed`: итог раннера остаётся, к нему пометка; задача кончается.
pub async fn apply_collect_failed(
    conn: &mut PgConnection,
    reporter: Uuid,
    fact: &OutputsCollectFailed,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(job) =
        locked_for_carrier(conn, reporter, &fact.job_id, "outputs_collect_failed").await?
    else {
        return Ok(());
    };
    complete_commands(conn, reporter, job.id, "collect_outputs").await?;
    if job.state() != State::Collecting {
        return Ok(());
    }
    let note = format!("outputs not collected: {}", clean_reason(&fact.reason));
    finish_collecting_in_tx(conn, &job, Some(&note), correlation_id).await?;
    Ok(())
}

/// `SourceCleaned`: очистка подтверждена, задача отмечается чистой. Конец задачи от очистки
/// не зависит.
pub async fn apply_source_cleaned(
    conn: &mut PgConnection,
    reporter: Uuid,
    fact: &SourceCleaned,
    _correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(job) = locked_for_carrier(conn, reporter, &fact.job_id, "source_cleaned").await?
    else {
        return Ok(());
    };
    complete_commands(conn, reporter, job.id, "cleanup_source").await?;
    sqlx::query("update jobs set cleaned_at=coalesce(cleaned_at, clock_timestamp()) where id=$1")
        .bind(job.id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

// ───────── Службы по сроку ─────────

/// Сбор не закончился за `timeout`: задача заканчивается итогом раннера; если сбор ждали и не
/// дождались, к итогу пометка. Очистка продолжается: команда остаётся в outbox.
pub async fn collect_timeout_sweep(
    pool: &PgPool,
    timeout: Duration,
    now: DateTime<Utc>,
) -> Result<usize, sqlx::Error> {
    let rows: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "select id, machine_id from jobs
         where state='collecting' and collecting_at + make_interval(secs => $1) < $2
         order by collecting_at, id",
    )
    .bind(timeout.as_secs_f64())
    .bind(now)
    .fetch_all(pool)
    .await?;
    let mut done = 0;
    for (id, machine) in rows {
        let ended = crate::db::in_tx(pool, move |conn| {
            Box::pin(async move {
                if let Some(machine) = machine {
                    lock_machine(conn, machine).await?;
                }
                let Some(job) = lock_row(conn, id).await? else {
                    return Ok::<_, sqlx::Error>(false);
                };
                if job.state() != State::Collecting {
                    return Ok(false);
                }
                // В `collecting` задача стоит только за сбором, которого так и не дождались.
                let pending = job.outputs_dir.is_none() && job.outputs_note.is_none();
                let note = pending.then_some("outputs not collected: timeout");
                if pending {
                    complete_commands(
                        conn,
                        job.carrier_id.unwrap_or_default(),
                        job.id,
                        "collect_outputs",
                    )
                    .await?;
                }
                finish_collecting_in_tx(conn, &job, note, Uuid::now_v7()).await?;
                Ok(true)
            })
        })
        .await?;
        done += usize::from(ended);
    }
    Ok(done)
}

/// Подготовка не закончилась за `timeout` (несущая молчит): `failed` с причиной.
pub async fn stage_timeout_sweep(
    pool: &PgPool,
    timeout: Duration,
    now: DateTime<Utc>,
) -> Result<usize, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select id, machine_id from jobs
         where state='staging'
           and coalesce(last_fact_at, created_at) + make_interval(secs => $1) < $2
         order by created_at, id",
    )
    .bind(timeout.as_secs_f64())
    .bind(now)
    .fetch_all(pool)
    .await?;
    let mut done = 0;
    for (id, machine) in rows {
        let ended = crate::db::in_tx(pool, move |conn| {
            Box::pin(async move {
                lock_machine(conn, machine).await?;
                let Some(job) = lock_row(conn, id).await? else {
                    return Ok::<_, sqlx::Error>(false);
                };
                if job.state() != State::Staging {
                    return Ok(false);
                }
                finish_in_tx(
                    conn,
                    &job,
                    Outcome::bare(State::Failed, Some("staging_failed: timeout")),
                    Uuid::now_v7(),
                )
                .await?;
                dispatch_machine_in_tx(conn, machine, Uuid::now_v7()).await?;
                Ok(true)
            })
        })
        .await?;
        done += usize::from(ended);
    }
    Ok(done)
}

/// Очистка не подтверждена за `after`: команда выдаётся снова, не больше
/// `MAX_CLEANUP_ATTEMPTS` раз. Несущая вне связи не получает лишних команд: прежняя ждёт в outbox.
pub async fn cleanup_retry_sweep(
    pool: &PgPool,
    registry: &Registry,
    after: Duration,
    now: DateTime<Utc>,
) -> Result<usize, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select id, carrier_id from jobs
         where source_repo is not null and cleaned_at is null and cleanup_sent_at is not null
           and cleanup_attempts < $1
           and state in ('succeeded','failed','timed_out','canceled','lost')
           and cleanup_sent_at + make_interval(secs => $2) < $3
         order by cleanup_sent_at, id limit 200",
    )
    .bind(MAX_CLEANUP_ATTEMPTS)
    .bind(after.as_secs_f64())
    .bind(now)
    .fetch_all(pool)
    .await?;
    let mut done = 0;
    for (id, carrier) in rows {
        if registry.availability(carrier) != Availability::Available {
            continue;
        }
        let resent = crate::db::in_tx(pool, move |conn| {
            Box::pin(async move {
                let Some(job) = lock_row(conn, id).await? else {
                    return Ok::<_, sqlx::Error>(false);
                };
                if job.cleaned_at.is_some() || job.cleanup_attempts >= MAX_CLEANUP_ATTEMPTS {
                    return Ok(false);
                }
                // Прежняя команда закрывается: её место занимает новая.
                complete_commands(conn, carrier, id, "cleanup_source").await?;
                send_cleanup_in_tx(conn, &job).await?;
                sqlx::query(
                    "update jobs set cleanup_sent_at=clock_timestamp(),
                            cleanup_attempts=cleanup_attempts+1 where id=$1",
                )
                .bind(id)
                .execute(conn)
                .await?;
                Ok(true)
            })
        })
        .await?;
        done += usize::from(resent);
    }
    Ok(done)
}
