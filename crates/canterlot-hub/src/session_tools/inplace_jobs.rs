//! Инструменты `job_run`, `job_status`, `job_log`, `job_cancel` и `jobs_list`: команды оболочки
//! на машине с воркером. Логика задач живёт в `inplace_jobs`; здесь допуск, аргументы,
//! ожидание ответа и доступ только к задачам своего разговора.

use std::{collections::BTreeMap, time::Duration};

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use tokio::time::Instant;
use uuid::Uuid;

use super::{Caller, ToolContext, ToolError, ToolErrorCode, parse_arguments, send, spawn};
use crate::{
    attachments::BlobStore,
    inplace_jobs::{
        self as jobs, Job, State,
        logs::{self, DEFAULT_READ_BYTES},
    },
    session::{Availability, Registry},
};

const DEFAULT_TIMEOUT_MINUTES: i64 = 30;
const DEFAULT_WAIT_SECONDS: i64 = 20;
const MAX_WAIT_SECONDS: i64 = 60;
const MAX_ENV: usize = 32;
const MAX_ENV_VALUE: usize = 4096;
const MAX_SHELL_BYTES: usize = 64 * 1024;
const MAX_TITLE_CHARS: usize = 200;
const MAX_KEY_CHARS: usize = 128;
const POLL: Duration = Duration::from_millis(200);
pub const MAX_LIST: i64 = 50;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    machine: Option<String>,
    workdir: String,
    shell: String,
    title: Option<String>,
    timeout_minutes: Option<i64>,
    wait_seconds: Option<i64>,
    lease: Option<String>,
    env: Option<BTreeMap<String, String>>,
    idempotency_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdArgs {
    job_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LogArgs {
    job_id: Uuid,
    offset: Option<i64>,
    limit: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    machine: Option<String>,
    state: Option<String>,
    limit: Option<i64>,
}

fn invalid(message: &str) -> ToolError {
    ToolError::with_message(ToolErrorCode::InvalidArguments, message.to_owned())
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 64
}

fn check_run(args: &RunArgs) -> Result<(), ToolError> {
    if !args.workdir.starts_with('/') || args.workdir.contains('\0') {
        return Err(invalid("job_run: workdir must be an absolute path"));
    }
    if args.shell.trim().is_empty() || args.shell.len() > MAX_SHELL_BYTES {
        return Err(invalid(
            "job_run: shell must be a non-empty command up to 64 KiB",
        ));
    }
    if args.shell.contains('\0') {
        return Err(invalid("job_run: shell must not contain NUL"));
    }
    if args
        .title
        .as_deref()
        .is_some_and(|title| title.chars().count() > MAX_TITLE_CHARS)
    {
        return Err(invalid("job_run: title is longer than 200 characters"));
    }
    if args
        .idempotency_key
        .as_deref()
        .is_some_and(|key| key.is_empty() || key.chars().count() > MAX_KEY_CHARS)
    {
        return Err(invalid(
            "job_run: idempotency_key must be 1..128 characters",
        ));
    }
    if args
        .timeout_minutes
        .is_some_and(|minutes| !(1..=1440).contains(&minutes))
    {
        return Err(invalid("job_run: timeout_minutes must be 1..1440"));
    }
    if args
        .wait_seconds
        .is_some_and(|seconds| !(0..=MAX_WAIT_SECONDS).contains(&seconds))
    {
        return Err(invalid("job_run: wait_seconds must be 0..60"));
    }
    if args
        .lease
        .as_deref()
        .is_some_and(|lease| !matches!(lease, "none" | "build" | "gate"))
    {
        return Err(invalid("job_run: lease is none, build or gate"));
    }
    if let Some(env) = &args.env {
        if env.len() > MAX_ENV {
            return Err(invalid("job_run: at most 32 env pairs"));
        }
        for (name, value) in env {
            if !valid_env_name(name) || value.len() > MAX_ENV_VALUE || value.contains('\0') {
                return Err(invalid("job_run: invalid env pair"));
            }
        }
    }
    Ok(())
}

/// Оборванный вызов: итог должен уйти сообщением, а не пропасть. Страж снимается, когда вызов
/// сам решил, как доставить итог.
struct DeliveryGuard {
    pool: PgPool,
    job_id: Uuid,
    armed: bool,
}

impl Drop for DeliveryGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (pool, id) = (self.pool.clone(), self.job_id);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let result: Result<(), sqlx::Error> = crate::db::in_tx(&pool, move |conn| {
                    Box::pin(async move { jobs::fallback_to_message_in_tx(conn, id).await })
                })
                .await;
                if result.is_err() {
                    tracing::warn!(kind = "job_delivery_fallback", job_id = %id, "dropped job_run call could not switch to a message");
                }
            });
        }
    }
}

/// Что решил ждущий вызов.
enum Settled {
    Inline(Job),
    /// Итог отправлен или будет отправлен сообщением.
    Message(Job),
    Pending(Job),
}

async fn settle(
    pool: &PgPool,
    caller: &Caller,
    id: Uuid,
    at_deadline: bool,
) -> Result<Settled, ToolError> {
    let mut tx = crate::db::begin(pool).await?;
    let Some(job) = jobs::lock_row(&mut tx, id).await? else {
        return Err(ToolError::new(ToolErrorCode::JobNotFound));
    };
    // Те же правила видимости, что у чтений: разговор, сменивший домен после заявки, не
    // получает ни итога в ответе, ни сообщения. Итог не берётся внутрь вызова.
    let visible: bool = sqlx::query_scalar(
        "select exists(select 1 from conversations
          where id=$1 and owner_id=$2 and memory_domain=$3)",
    )
    .bind(caller.conversation_id)
    .bind(caller.owner_id)
    .bind(&job.domain)
    .fetch_one(&mut *tx)
    .await?;
    if !visible || job.conversation_id != Some(caller.conversation_id) {
        jobs::fallback_to_message_in_tx(&mut tx, id).await?;
        tx.commit().await?;
        return Err(ToolError::new(ToolErrorCode::JobNotFound));
    }
    let settled = if job.is_terminal() {
        if job.delivery.is_none() && jobs::claim_inline_in_tx(&mut tx, id).await? {
            Settled::Inline(job)
        } else {
            Settled::Message(job)
        }
    } else if at_deadline {
        jobs::fallback_to_message_in_tx(&mut tx, id).await?;
        Settled::Pending(job)
    } else {
        Settled::Pending(job)
    };
    tx.commit().await?;
    Ok(settled)
}

fn refusal(error: jobs::Refusal) -> ToolError {
    let code = error.code();
    match error {
        jobs::Refusal::ModeTooNarrow => ToolError::with_message(
            code,
            "job_run needs bypass permission mode in the calling session",
        ),
        _ => ToolError::new(code),
    }
}

/// Повтор: задача этого же разговора и его нынешнего домена, найденная по идентификатору
/// вызова или по ключу. Чужая задача и задача старого домена не видны.
async fn replay_by_keys(
    conn: &mut PgConnection,
    caller: &Caller,
    domain: Option<&str>,
    key: Option<&str>,
) -> Result<Option<Job>, sqlx::Error> {
    let Some(domain) = domain else {
        return Ok(None);
    };
    sqlx::query_as(
        "select * from jobs
         where owner_id=$1 and conversation_id=$2 and domain=$3
           and (call_id=$4 or ($5::text is not null and idempotency_key=$5))
         order by created_at limit 1",
    )
    .bind(caller.owner_id)
    .bind(caller.conversation_id)
    .bind(domain)
    .bind(caller.call_id)
    .bind(key)
    .fetch_optional(conn)
    .await
}

fn machine_offline(registry: &Registry, job: &Job) -> bool {
    !job.is_terminal()
        && job.state() != State::Queued
        && registry.availability(job.machine_id) != Availability::Available
}

/// Видимость задачи: свой разговор и тот же домен, что при заявке. Чужая и несуществующая
/// неотличимы.
async fn visible(conn: &mut PgConnection, caller: &Caller, id: Uuid) -> Result<Job, ToolError> {
    let job: Option<Job> = sqlx::query_as(
        "select j.* from jobs j join conversations c on c.id=j.conversation_id
         where j.id=$1 and j.owner_id=$2 and j.conversation_id=$3 and c.memory_domain=j.domain",
    )
    .bind(id)
    .bind(caller.owner_id)
    .bind(caller.conversation_id)
    .fetch_optional(conn)
    .await?;
    job.ok_or_else(|| ToolError::new(ToolErrorCode::JobNotFound))
}

async fn answer(
    context: &ToolContext,
    conn: &mut PgConnection,
    job: &Job,
) -> Result<Value, ToolError> {
    let machine = jobs::machine_name(conn, job.machine_id).await?;
    let mut value = if job.is_terminal() {
        jobs::result_json(job, &machine)
    } else {
        jobs::state_json(job, &machine)
    };
    if machine_offline(&context.registry, job) {
        value["machine_offline"] = json!(true);
    }
    if let Some(late) = &job.late_report {
        value["late_report"] = late.clone();
    }
    Ok(value)
}

pub async fn run(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: RunArgs = parse_arguments("job_run", arguments)?;
    check_run(&args)?;
    let wait = args.wait_seconds.unwrap_or(DEFAULT_WAIT_SECONDS);
    let mut tx = crate::db::begin(&context.pool).await?;
    send::lock_sender(&mut tx, caller.conversation_id).await?;
    let facts = jobs::caller_facts(&mut tx, caller.conversation_id)
        .await?
        .ok_or_else(|| ToolError::new(ToolErrorCode::SessionNotFound))?;
    if let Some(job) = replay_by_keys(
        &mut tx,
        caller,
        facts.memory_domain.as_deref(),
        args.idempotency_key.as_deref(),
    )
    .await?
    {
        // Повтор: сохранённый итог, а у идущей задачи итог пойдёт сообщением, потому что
        // прежний ждущий вызов мог умереть вместе со своим ответом.
        if !job.is_terminal() {
            jobs::fallback_to_message_in_tx(&mut tx, job.id).await?;
        }
        let job = jobs::load(&mut tx, job.id).await?.expect("job row exists");
        let value = answer(context, &mut tx, &job).await?;
        tx.commit().await?;
        return Ok(value);
    }
    let target = spawn::resolve_machine(&mut tx, caller, args.machine.as_deref()).await?;
    let machine_id = target.unwrap_or(caller.machine_id);
    let remote = machine_id != facts.machine_id;
    if remote {
        // Владелец раньше строки машины: тот же порядок, что у запуска удалённого делегата.
        crate::conversations::lock_owner(&mut tx, caller.owner_id).await?;
    }
    jobs::admit(&mut tx, &facts, machine_id, &args.workdir)
        .await
        .map_err(refusal)?;
    let capabilities: Option<Vec<String>> =
        sqlx::query_scalar("select capabilities from machines where id=$1 and owner_id=$2")
            .bind(machine_id)
            .bind(caller.owner_id)
            .fetch_optional(&mut *tx)
            .await?;
    let capable = capabilities.is_some_and(|list| {
        list.iter()
            .any(|capability| capability == crate::session::CAPABILITY_JOBS_INPLACE_V1)
    });
    if !capable {
        return Err(ToolError::new(ToolErrorCode::CapabilityMissing));
    }
    let id = Uuid::now_v7();
    // Страж встаёт до фиксации: вызов, оборванный внутри `commit`, не оставит задачу без
    // доставки. Если заявка не зафиксирована, строки нет и страж ничего не делает.
    let mut guard = DeliveryGuard {
        pool: context.pool.clone(),
        job_id: id,
        armed: wait > 0,
    };
    let env: Vec<Value> = args
        .env
        .iter()
        .flatten()
        .map(|(name, value)| json!({"name": name, "value": value}))
        .collect();
    let inserted = sqlx::query(
        "insert into jobs (id, owner_id, conversation_id, domain, machine_id, remote, workdir,
                           shell, env, title, timeout_seconds, lease, call_id, idempotency_key,
                           delivery)
         values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)
         on conflict do nothing",
    )
    .bind(id)
    .bind(caller.owner_id)
    .bind(caller.conversation_id)
    .bind(facts.memory_domain.as_deref().unwrap_or_default())
    .bind(machine_id)
    .bind(remote)
    .bind(&args.workdir)
    .bind(&args.shell)
    .bind(json!(env))
    .bind(args.title.as_deref())
    .bind(
        i32::try_from(args.timeout_minutes.unwrap_or(DEFAULT_TIMEOUT_MINUTES) * 60).unwrap_or(1800),
    )
    .bind(args.lease.as_deref().unwrap_or("none"))
    .bind(caller.call_id)
    .bind(args.idempotency_key.as_deref())
    // Без ожидания итог может прийти только сообщением.
    .bind((wait == 0).then_some("message"))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
        // Гонка двух повторов с одним ключом: выигравший уже зафиксирован.
        let job = replay_by_keys(
            &mut tx,
            caller,
            facts.memory_domain.as_deref(),
            args.idempotency_key.as_deref(),
        )
        .await?
            .ok_or_else(|| ToolError::new(ToolErrorCode::Internal))?;
        let value = answer(context, &mut tx, &job).await?;
        tx.commit().await?;
        return Ok(value);
    }
    let created = jobs::load(&mut tx, id).await?.expect("job row inserted");
    jobs::emit_changed(&mut tx, &created, caller.call_id).await?;
    jobs::dispatch_machine_in_tx(&mut tx, machine_id, caller.call_id).await?;
    tx.commit().await?;
    let mut conn = context.pool.acquire().await?;
    let machine = jobs::machine_name(&mut conn, machine_id).await?;
    drop(conn);
    if wait == 0 {
        let job = jobs::load(&mut *context.pool.acquire().await?, id)
            .await?
            .expect("job row exists");
        return Ok(jobs::state_json(&job, &machine));
    }
    let deadline = Instant::now() + Duration::from_secs(u64::try_from(wait).unwrap_or(20));
    loop {
        let at_deadline = Instant::now() >= deadline;
        match settle(&context.pool, caller, id, at_deadline).await? {
            Settled::Inline(job) => {
                guard.armed = false;
                return Ok(jobs::result_json(&job, &machine));
            }
            Settled::Message(job) => {
                guard.armed = false;
                return Ok(jobs::state_json(&job, &machine));
            }
            Settled::Pending(job) if at_deadline => {
                guard.armed = false;
                return Ok(jobs::state_json(&job, &machine));
            }
            Settled::Pending(_) => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        tokio::select! {
            _ = tokio::time::sleep(POLL.min(left)) => {}
            _ = context.shutdown.cancelled() => {
                // Хаб останавливается: страж остаётся взведённым, итог уйдёт сообщением.
                return Err(ToolError::with_message(
                    ToolErrorCode::Internal,
                    "The hub is shutting down; the result will arrive as a message",
                ));
            }
        }
    }
}

pub async fn status(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: IdArgs = parse_arguments("job_status", arguments)?;
    let mut conn = context.pool.acquire().await?;
    let job = visible(&mut conn, caller, args.job_id).await?;
    answer(context, &mut conn, &job).await
}

pub async fn log(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
    store: Option<&dyn BlobStore>,
) -> Result<Value, ToolError> {
    let args: LogArgs = parse_arguments("job_log", arguments)?;
    if args.offset.is_some_and(|offset| offset < 0)
        || args
            .limit
            .is_some_and(|limit| !(1..=logs::MAX_READ_BYTES as i64).contains(&limit))
    {
        return Err(invalid("job_log: offset >= 0 and limit 1..65536"));
    }
    let mut conn = context.pool.acquire().await?;
    let job = visible(&mut conn, caller, args.job_id).await?;
    let limit = args
        .limit
        .map_or(DEFAULT_READ_BYTES, |limit| limit as usize);
    let offset = args.offset.map(|offset| offset as u64);
    let slice = match store {
        Some(store) => logs::read(store, &job, offset, limit).await,
        None => logs::read_tail(&job, offset, limit),
    }
    .map_err(|error| {
        tracing::warn!(kind = "job_log_read", error = %error.kind(), "job log read failed");
        ToolError::new(ToolErrorCode::Internal)
    })?;
    Ok(json!({
        "job_id": job.id,
        "state": job.state,
        "source": slice.source,
        "offset": slice.offset,
        "next_offset": slice.next_offset,
        "total_bytes": slice.total_bytes,
        "eof": slice.eof,
        "text": slice.text,
        "note": slice.note,
    }))
}

pub async fn cancel(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: IdArgs = parse_arguments("job_cancel", arguments)?;
    let mut tx = crate::db::begin(&context.pool).await?;
    visible(&mut tx, caller, args.job_id).await?;
    let job = jobs::cancel_in_tx(&mut tx, args.job_id, "canceled_by_caller", caller.call_id)
        .await?
        .ok_or_else(|| ToolError::new(ToolErrorCode::JobNotFound))?;
    let mut value = answer(context, &mut tx, &job).await?;
    if !job.is_terminal() {
        value["cancel_requested"] = json!(true);
    }
    tx.commit().await?;
    Ok(value)
}

pub async fn list(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: ListArgs = parse_arguments("jobs_list", arguments)?;
    let limit = args.limit.unwrap_or(20);
    if !(1..=MAX_LIST).contains(&limit) {
        return Err(invalid("jobs_list: limit is 1..50"));
    }
    if args
        .state
        .as_deref()
        .is_some_and(|state| State::parse(state).is_none())
    {
        return Err(invalid("jobs_list: unknown state"));
    }
    let mut conn = context.pool.acquire().await?;
    let machine = spawn::resolve_machine(&mut conn, caller, args.machine.as_deref()).await?;
    // Без аргумента `machine` — все машины заказчика; своя машина названа явно тоже ограничивает.
    let machine_filter = match (&args.machine, machine) {
        (Some(_), Some(id)) => Some(id),
        (Some(_), None) => Some(caller.machine_id),
        _ => None,
    };
    let rows: Vec<Job> = sqlx::query_as(
        "select j.* from jobs j join conversations c on c.id=j.conversation_id
         where j.owner_id=$1 and j.conversation_id=$2 and c.memory_domain=j.domain
           and ($3::uuid is null or j.machine_id=$3)
           and ($4::text is null or j.state=$4)
         order by j.created_at desc, j.id desc limit $5",
    )
    .bind(caller.owner_id)
    .bind(caller.conversation_id)
    .bind(machine_filter)
    .bind(args.state.as_deref())
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for job in &rows {
        let name = jobs::machine_name(&mut conn, job.machine_id).await?;
        out.push(json!({
            "job_id": job.id,
            "state": job.state,
            "machine": name,
            "title": job.title,
            "shell": job.shell.chars().take(120).collect::<String>(),
            "exit_code": job.exit_code,
            "duration_ms": job.duration_ms,
            "created_at": job.created_at,
            "machine_offline": machine_offline(&context.registry, job),
        }));
    }
    Ok(json!({"jobs": out, "count": out.len()}))
}
