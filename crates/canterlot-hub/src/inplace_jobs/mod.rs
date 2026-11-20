//! Задачи на месте (`job_run`, возможность `jobs_inplace_v1`): команда оболочки в каталоге
//! машины с воркером. Строка `jobs` — единственный источник состояния; факты воркера
//! применяются только вперёд, итог доставляется ровно один раз (`delivery`).
//!
//! Порядок блокировок: замок машины (`lock_machine`), затем строки задач по возрастанию,
//! затем счётчик событий. Любой путь, который может освободить место или занять его
//! (заявка, итог, отмена, подключение машины), берёт замок машины первым.

pub mod facts;
pub mod logs;
pub mod sweep;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    events::{self, NewEvent, Scope},
    session_tools::{ToolErrorCode, spawn, subs},
};

/// Живой хвост на хабе и хвост итога от воркера.
pub const LIVE_TAIL_BYTES: usize = 64 * 1024;
pub const RESULT_TAIL_BYTES: usize = 32 * 1024;
/// Хвост в сообщении об итоге.
pub const MESSAGE_TAIL_BYTES: usize = 8 * 1024;
/// Сколько задач машина принимает, если отчёт не назвал число.
pub const DEFAULT_MAX_RUNNING: i64 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Queued,
    Dispatched,
    WaitingLease,
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Canceled,
    Lost,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dispatched => "dispatched",
            Self::WaitingLease => "waiting_lease",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Canceled => "canceled",
            Self::Lost => "lost",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "dispatched" => Self::Dispatched,
            "waiting_lease" => Self::WaitingLease,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "timed_out" => Self::TimedOut,
            "canceled" => Self::Canceled,
            "lost" => Self::Lost,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::TimedOut | Self::Canceled | Self::Lost
        )
    }

    /// Порядок «вперёд»: итог выше любого хода.
    fn rank(self) -> u8 {
        match self {
            Self::Queued => 0,
            Self::Dispatched => 1,
            Self::WaitingLease => 2,
            Self::Running => 3,
            _ => 4,
        }
    }
}

/// Строка задачи: всё, что нужно путям хаба.
#[derive(sqlx::FromRow, Clone, Debug)]
pub struct Job {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub domain: String,
    pub machine_id: Uuid,
    pub remote: bool,
    pub workdir: String,
    pub shell: String,
    pub env: Value,
    pub title: Option<String>,
    pub timeout_seconds: i32,
    pub lease: String,
    pub state: String,
    pub attempt: i32,
    pub delivery: Option<String>,
    pub call_id: Option<Uuid>,
    pub idempotency_key: Option<String>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub end_reason: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub output_bytes: i64,
    pub truncated: bool,
    pub tail: Option<Vec<u8>>,
    pub live_tail: Vec<u8>,
    pub live_offset: i64,
    pub pid: Option<i64>,
    pub proc_start_time: Option<i64>,
    pub log_media_id: Option<Uuid>,
    pub log_size: Option<i64>,
    pub late_report: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub last_fact_at: Option<DateTime<Utc>>,
}

impl Job {
    pub fn state(&self) -> State {
        State::parse(&self.state).unwrap_or(State::Failed)
    }

    pub fn is_terminal(&self) -> bool {
        self.state().is_terminal()
    }
}

/// Задача для клиентов: без вывода (его отдают отдельные чтения).
#[derive(Serialize, serde::Deserialize, ToSchema, Clone, Debug)]
pub struct JobView {
    pub id: Uuid,
    pub state: String,
    pub machine_id: Uuid,
    pub machine_name: String,
    pub conversation_id: Option<Uuid>,
    pub conversation_title: Option<String>,
    pub title: Option<String>,
    pub workdir: String,
    pub shell: String,
    pub lease: String,
    pub timeout_minutes: i32,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub output_bytes: i64,
    pub truncated: bool,
    pub end_reason: Option<String>,
    pub cancel_requested: bool,
    pub has_log: bool,
    /// Машина не на связи, а задача не закончена; в событии всегда `false`: связь машины
    /// приходит событиями машин.
    pub machine_offline: bool,
    #[schema(value_type = Option<Object>)]
    pub late_report: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Причина, по которой задача не допущена: код инструмента и слово для причины конца.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    ModeTooNarrow,
    DomainMismatch,
    RemoteWorkDisabled,
}

impl Refusal {
    pub fn code(self) -> ToolErrorCode {
        match self {
            Self::ModeTooNarrow => ToolErrorCode::ModeTooNarrow,
            Self::DomainMismatch => ToolErrorCode::DomainMismatch,
            Self::RemoteWorkDisabled => ToolErrorCode::RemoteWorkDisabled,
        }
    }

    pub fn reason(self) -> &'static str {
        self.code().as_str()
    }
}

/// Что хаб знает о заказчике при допуске.
#[derive(sqlx::FromRow, Debug)]
pub struct CallerFacts {
    pub harness: String,
    pub permission_mode: String,
    pub pending_mode: Option<String>,
    pub side: bool,
    pub archived: bool,
    pub memory_domain: Option<String>,
    pub machine_id: Uuid,
}

pub async fn caller_facts(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Option<CallerFacts>, sqlx::Error> {
    sqlx::query_as(
        "select harness, permission_mode, pending_settings->>'permission_mode' as pending_mode,
                side_of is not null as side, archived_at is not null as archived,
                memory_domain, machine_id
         from conversations where id=$1 and trashed_at is null",
    )
    .bind(conversation_id)
    .fetch_optional(conn)
    .await
}

/// Режим, домен и выключатель: одна проверка для заявки и для диспетчеризации.
/// Действующий режим — более узкий из применённого и ожидающего, и это обязан быть `bypass`;
/// Codex `ask`, `owner`, неизвестный режим и боковые сессии отказываются.
pub async fn admit(
    conn: &mut PgConnection,
    facts: &CallerFacts,
    machine_id: Uuid,
    workdir: &str,
) -> Result<(), Refusal> {
    let mode = spawn::parent_mode(&facts.permission_mode, facts.pending_mode.as_deref());
    if mode != "bypass" || facts.side {
        return Err(Refusal::ModeTooNarrow);
    }
    let Some(domain) = facts.memory_domain.as_deref() else {
        return Err(Refusal::DomainMismatch);
    };
    let path = crate::memory::path_domain(conn, machine_id, workdir)
        .await
        .map_err(|_| Refusal::DomainMismatch)?;
    if path.map(crate::memory::MemoryDomain::as_str) != Some(domain) {
        return Err(Refusal::DomainMismatch);
    }
    if machine_id != facts.machine_id
        && !crate::machines::accepts_remote_work_for_share(conn, machine_id)
            .await
            .map_err(|_| Refusal::RemoteWorkDisabled)?
    {
        return Err(Refusal::RemoteWorkDisabled);
    }
    Ok(())
}

/// Замок машины: внутри него считается место и ставятся команды. Раньше строк задач.
pub async fn lock_machine(conn: &mut PgConnection, machine_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('canterlot:jobs:'||$1::text,0))")
        .bind(machine_id)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn load(conn: &mut PgConnection, id: Uuid) -> Result<Option<Job>, sqlx::Error> {
    sqlx::query_as("select * from jobs where id=$1")
        .bind(id)
        .fetch_optional(conn)
        .await
}

/// Строка под замком; замок машины заказчик берёт сам, если ему нужна диспетчеризация.
pub async fn lock_row(conn: &mut PgConnection, id: Uuid) -> Result<Option<Job>, sqlx::Error> {
    sqlx::query_as("select * from jobs where id=$1 for update")
        .bind(id)
        .fetch_optional(conn)
        .await
}

pub async fn machine_name(conn: &mut PgConnection, machine: Uuid) -> Result<String, sqlx::Error> {
    Ok(sqlx::query_scalar("select name from machines where id=$1")
        .bind(machine)
        .fetch_optional(conn)
        .await?
        .unwrap_or_default())
}

pub async fn view(conn: &mut PgConnection, job: &Job) -> Result<JobView, sqlx::Error> {
    let machine_name = machine_name(conn, job.machine_id).await?;
    let conversation_title = match job.conversation_id {
        Some(id) => {
            sqlx::query_scalar("select title from conversations where id=$1")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?
        }
        None => None,
    };
    Ok(view_with(job, machine_name, conversation_title))
}

pub fn view_with(job: &Job, machine_name: String, conversation_title: Option<String>) -> JobView {
    JobView {
        id: job.id,
        state: job.state.clone(),
        machine_id: job.machine_id,
        machine_name,
        conversation_id: job.conversation_id,
        conversation_title,
        title: job.title.clone(),
        workdir: job.workdir.clone(),
        shell: job.shell.clone(),
        lease: job.lease.clone(),
        timeout_minutes: job.timeout_seconds / 60,
        exit_code: job.exit_code,
        duration_ms: job.duration_ms,
        output_bytes: job.output_bytes,
        truncated: job.truncated,
        end_reason: job.end_reason.clone(),
        cancel_requested: job.cancel_requested_at.is_some(),
        has_log: job.log_media_id.is_some(),
        machine_offline: false,
        late_report: job.late_report.clone(),
        created_at: job.created_at,
        started_at: job.started_at,
        finished_at: job.finished_at,
    }
}

/// Событие клиентам: вся задача целиком (без вывода), не разность.
pub async fn emit_changed(
    conn: &mut PgConnection,
    job: &Job,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let view = view(conn, job).await?;
    events::append(
        conn,
        NewEvent {
            scope: Scope::Machines {
                owner_id: job.owner_id,
            },
            kind: "job_changed",
            conversation_id: None,
            execution_id: None,
            payload: json!({"job": view}),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

fn tail_text(bytes: &[u8], limit: usize) -> String {
    let start = bytes.len().saturating_sub(limit);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// Ответ инструмента о задаче в терминальном состоянии: тот же состав, что у `job_status`.
pub fn result_json(job: &Job, machine: &str) -> Value {
    let tail = job.tail.as_deref().unwrap_or(&[]);
    let mut value = json!({
        "job_id": job.id,
        "state": job.state,
        "machine": machine,
        "exit_code": job.exit_code,
        "duration_ms": job.duration_ms,
        "output_tail": tail_text(tail, RESULT_TAIL_BYTES),
        "output_bytes": job.output_bytes,
        "truncated": job.truncated,
    });
    if let Some(reason) = &job.end_reason {
        value["reason"] = json!(reason);
    }
    value
}

/// Ответ о задаче, у которой ещё нет итога или итог уйдёт сообщением.
pub fn state_json(job: &Job, machine: &str) -> Value {
    let mut value = json!({"job_id": job.id, "state": job.state, "machine": machine});
    if job.cancel_requested_at.is_some() && !job.is_terminal() {
        value["cancel_requested"] = json!(true);
    }
    value
}

/// Сообщение заказчику об итоге: заголовок и хвост до 8 КиБ.
pub fn message_text(job: &Job, machine: &str) -> String {
    let title = job
        .title
        .as_deref()
        .map(crate::session_tools::origin::clean_title)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| {
            let shell: String = job.shell.chars().take(60).collect();
            crate::session_tools::origin::clean_title(&shell)
        });
    let machine = crate::session_tools::origin::clean_title(machine);
    let mut text = format!(
        "[canterlot] Job {} \"{title}\" on machine \"{machine}\" ended: {}",
        job.id,
        job.state.replace('_', " ")
    );
    if let Some(code) = job.exit_code {
        text.push_str(&format!(", exit code {code}"));
    }
    if let Some(ms) = job.duration_ms {
        text.push_str(&format!(", {:.1} s", ms as f64 / 1000.0));
    }
    if let Some(reason) = &job.end_reason {
        text.push_str(&format!(" ({reason})"));
    }
    text.push('.');
    let tail = job.tail.as_deref().unwrap_or(&[]);
    if tail.is_empty() {
        text.push_str(" No output.");
    } else {
        text.push_str(&format!(
            " Output ({} bytes total{}), last part:\n{}\nUse job_log {{job_id}} for the full log.",
            job.output_bytes,
            if job.truncated { ", log truncated" } else { "" },
            tail_text(tail, MESSAGE_TAIL_BYTES)
        ));
    }
    text
}

/// Сообщение об итоге как задание заказчика. Вызывается только под замком строки и только
/// когда `delivery = 'message'` и задача терминальна; ровно один раз по построению.
pub async fn enqueue_message_in_tx(conn: &mut PgConnection, job: &Job) -> Result<(), sqlx::Error> {
    let Some(conversation) = job.conversation_id else {
        return Ok(());
    };
    subs::insert_job(
        conn,
        job.owner_id,
        conversation,
        Utc::now(),
        &json!({"action": "job_result", "job_id": job.id}),
    )
    .await?;
    Ok(())
}

/// Ждущий вызов берёт доставку на себя. `true` — теперь отвечает он. Под замком строки.
pub async fn claim_inline_in_tx(conn: &mut PgConnection, id: Uuid) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "update jobs set delivery='inline', updated_at=clock_timestamp()
         where id=$1 and delivery is null",
    )
    .bind(id)
    .execute(conn)
    .await?
    .rows_affected()
        > 0)
}

/// Итог уйдёт сообщением: срок ожидания, оборванный вызов, старт хаба. Если задача уже
/// терминальна, сообщение ставится здесь же. Под замком строки; повтор ничего не делает.
pub async fn fallback_to_message_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(job) = lock_row(conn, id).await? else {
        return Ok(());
    };
    if job.delivery.is_some() {
        return Ok(());
    }
    sqlx::query("update jobs set delivery='message', updated_at=clock_timestamp() where id=$1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    if job.is_terminal() {
        enqueue_message_in_tx(conn, &job).await?;
    }
    Ok(())
}

/// Состояние ведёт только вперёд; возвращает строку после изменения или `None`, если
/// переход назад или в то же состояние. Терминальную строку не трогает.
fn moves_forward(job: &Job, to: State) -> bool {
    !job.state().is_terminal() && to.rank() > job.state().rank()
}

/// Задача стала терминальной: поля итога, команды гасятся, событие, сообщение при
/// `delivery = 'message'`, следующая из очереди. Под замком машины и строки.
pub struct Outcome {
    pub state: State,
    pub reason: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub output_bytes: Option<i64>,
    pub tail: Option<Vec<u8>>,
    pub truncated: bool,
}

impl Outcome {
    pub fn bare(state: State, reason: Option<&str>) -> Self {
        Self {
            state,
            reason: reason.map(str::to_owned),
            exit_code: None,
            duration_ms: None,
            output_bytes: None,
            tail: None,
            truncated: false,
        }
    }
}

pub async fn finish_in_tx(
    conn: &mut PgConnection,
    job: &Job,
    outcome: Outcome,
    correlation_id: Uuid,
) -> Result<Option<Job>, sqlx::Error> {
    debug_assert!(outcome.state.is_terminal());
    if job.is_terminal() {
        return Ok(None);
    }
    sqlx::query(
        "update jobs set state=$2, end_reason=$3, exit_code=$4, duration_ms=$5,
                output_bytes=coalesce($6, output_bytes), tail=coalesce($7, tail),
                truncated=$8, finished_at=clock_timestamp(), last_fact_at=clock_timestamp(),
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
    let done = load(conn, job.id).await?.expect("job row just updated");
    if done.delivery.as_deref() == Some("message") {
        enqueue_message_in_tx(conn, &done).await?;
    }
    emit_changed(conn, &done, correlation_id).await?;
    Ok(Some(done))
}

/// Машина освободила место или появилась: очередные задачи уходят на неё командами.
/// FIFO по `created_at`. Допуск перепроверяется: отказ закрывает задачу `canceled` с причиной.
pub async fn dispatch_machine_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    correlation_id: Uuid,
) -> Result<usize, sqlx::Error> {
    lock_machine(conn, machine_id).await?;
    let max: Option<i32> = sqlx::query_scalar("select jobs_max_running from machines where id=$1")
        .bind(machine_id)
        .fetch_optional(&mut *conn)
        .await?
        .flatten();
    let max = match max {
        Some(max) if max > 0 => i64::from(max),
        _ => DEFAULT_MAX_RUNNING,
    };
    let queued: Vec<Uuid> = sqlx::query_scalar(
        "select id from jobs where machine_id=$1 and state='queued' order by created_at, id",
    )
    .bind(machine_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut sent = 0;
    let mut full = false;
    for id in queued {
        let Some(job) = lock_row(conn, id).await? else {
            continue;
        };
        if job.state() != State::Queued {
            continue;
        }
        // Заказчик, чьё разрешение сменилось, больше не может ставить новую работу;
        // удалённый заказчик не мешает: заявка была допущена, задача принадлежит владельцу.
        if let Some(conversation) = job.conversation_id
            && let Some(facts) = caller_facts(conn, conversation).await?
        {
            let refusal = if facts.memory_domain.as_deref() != Some(job.domain.as_str()) {
                Some(Refusal::DomainMismatch)
            } else {
                admit(conn, &facts, job.machine_id, &job.workdir)
                    .await
                    .err()
            };
            if let Some(refusal) = refusal {
                finish_in_tx(
                    conn,
                    &job,
                    Outcome::bare(State::Canceled, Some(refusal.reason())),
                    correlation_id,
                )
                .await?;
                continue;
            }
        } else if job.remote
            && !crate::machines::accepts_remote_work_for_share(conn, job.machine_id).await?
        {
            finish_in_tx(
                conn,
                &job,
                Outcome::bare(State::Canceled, Some(Refusal::RemoteWorkDisabled.reason())),
                correlation_id,
            )
            .await?;
            continue;
        }
        // Очередь идёт по порядку: когда места нет, остальные не обгоняют, но допуск
        // перепроверяется у каждой, чтобы выключатель закрывал и стоящие дальше.
        if full || occupied_in_tx(conn, machine_id).await? >= max {
            full = true;
            continue;
        }
        send_start_in_tx(conn, &job).await?;
        sqlx::query(
            "update jobs set state='dispatched', dispatched_at=clock_timestamp(),
                    last_fact_at=clock_timestamp(), updated_at=clock_timestamp()
             where id=$1",
        )
        .bind(job.id)
        .execute(&mut *conn)
        .await?;
        let moved = load(conn, job.id).await?.expect("job row just updated");
        emit_changed(conn, &moved, correlation_id).await?;
        sent += 1;
    }
    Ok(sent)
}

/// Занятые места: задачи у воркера и потерянные, о которых он говорит, что они идут.
pub async fn occupied_in_tx(conn: &mut PgConnection, machine_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*) from jobs where machine_id=$1 and (
           state in ('dispatched','waiting_lease','running')
           or (state='lost' and late_report->>'phase'='running'))",
    )
    .bind(machine_id)
    .fetch_one(conn)
    .await
}

async fn notify_outbox(conn: &mut PgConnection, machine_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_notify('canterlot_outbox', $1)")
        .bind(machine_id.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

async fn send_start_in_tx(conn: &mut PgConnection, job: &Job) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, execution_id, execution_generation,
                               kind, payload, delivery_state)
         values ($1, $2, null, null, null, 'start_inplace_job', $3, 'pending')",
    )
    .bind(Uuid::now_v7())
    .bind(job.machine_id)
    .bind(json!({
        "job_id": job.id,
        "attempt": job.attempt,
        "workdir": job.workdir,
        "shell": job.shell,
        "env": job.env,
        "timeout_seconds": job.timeout_seconds,
        "lease": job.lease,
    }))
    .execute(&mut *conn)
    .await?;
    notify_outbox(conn, job.machine_id).await
}

async fn send_cancel_in_tx(conn: &mut PgConnection, job: &Job) -> Result<(), sqlx::Error> {
    // Одна отмена на попытку: повтор просьбы не плодит команд.
    let exists: bool = sqlx::query_scalar(
        "select exists(select 1 from commands where machine_id=$1 and kind='cancel_inplace_job'
                         and payload->>'job_id'=$2)",
    )
    .bind(job.machine_id)
    .bind(job.id.to_string())
    .fetch_one(&mut *conn)
    .await?;
    if exists {
        return Ok(());
    }
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, execution_id, execution_generation,
                               kind, payload, delivery_state)
         values ($1, $2, null, null, null, 'cancel_inplace_job', $3, 'pending')",
    )
    .bind(Uuid::now_v7())
    .bind(job.machine_id)
    .bind(json!({"job_id": job.id, "attempt": job.attempt}))
    .execute(&mut *conn)
    .await?;
    notify_outbox(conn, job.machine_id).await
}

/// Отмена: очередь закрывается сразу, идущая задача получает команду отмены и остаётся в
/// своём состоянии до подтверждения воркера. Потерянную задачу отменяет только владелец,
/// команда уйдёт, когда машина вернётся. Возвращает строку после отмены.
pub async fn cancel_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
    reason: &str,
    correlation_id: Uuid,
) -> Result<Option<Job>, sqlx::Error> {
    let machine: Option<Uuid> = sqlx::query_scalar("select machine_id from jobs where id=$1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(machine) = machine else {
        return Ok(None);
    };
    lock_machine(conn, machine).await?;
    let Some(job) = lock_row(conn, id).await? else {
        return Ok(None);
    };
    match job.state() {
        State::Queued => {
            finish_in_tx(
                conn,
                &job,
                Outcome::bare(State::Canceled, Some(reason)),
                correlation_id,
            )
            .await?;
        }
        State::Dispatched | State::WaitingLease | State::Running | State::Lost => {
            if job.cancel_requested_at.is_none() {
                sqlx::query(
                    "update jobs set cancel_requested_at=clock_timestamp(), cancel_reason=$2,
                            updated_at=clock_timestamp() where id=$1",
                )
                .bind(id)
                .bind(reason)
                .execute(&mut *conn)
                .await?;
            }
            send_cancel_in_tx(conn, &job).await?;
            let moved = load(conn, id).await?.expect("job row exists");
            emit_changed(conn, &moved, correlation_id).await?;
        }
        _ => {}
    }
    load(conn, id).await
}

/// Один шаг вперёд по факту воркера; `None`, если факт ничего не меняет.
pub async fn advance_to_in_tx(
    conn: &mut PgConnection,
    job: &Job,
    to: State,
    correlation_id: Uuid,
) -> Result<Option<Job>, sqlx::Error> {
    if !moves_forward(job, to) {
        return Ok(None);
    }
    sqlx::query(
        "update jobs set state=$2, last_fact_at=clock_timestamp(), updated_at=clock_timestamp()
         where id=$1",
    )
    .bind(job.id)
    .bind(to.as_str())
    .execute(&mut *conn)
    .await?;
    let moved = load(conn, job.id).await?.expect("job row just updated");
    emit_changed(conn, &moved, correlation_id).await?;
    Ok(Some(moved))
}
