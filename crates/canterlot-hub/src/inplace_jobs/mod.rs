//! Задачи на месте (`job_run`, возможность `jobs_inplace_v1`): команда оболочки в каталоге
//! машины с воркером. Строка `jobs` — единственный источник состояния; факты воркера
//! применяются только вперёд, итог доставляется ровно один раз (`delivery`).
//!
//! Порядок блокировок: замок машины (`lock_machine`), затем строки задач по возрастанию,
//! затем счётчик событий. Любой путь, который может освободить место или занять его
//! (заявка, итог, отмена, подключение машины), берёт замок машины первым.

pub mod carrier;
pub mod facts;
pub mod logs;
pub mod owner;
pub mod place;
pub mod spec;
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

/// Запас сверх `wait_seconds` на сеть и разбор до того, как срок ждущего вызова считается
/// вышедшим.
pub const WAITER_SLACK_SECONDS: i64 = 30;

/// Идентичность этого процесса хаба: ждущий вызов записывает её в строку задачи, а старт
/// хаба переводит на сообщение только задачи чужих экземпляров.
pub fn hub_instance() -> Uuid {
    static INSTANCE: std::sync::OnceLock<Uuid> = std::sync::OnceLock::new();
    *INSTANCE.get_or_init(Uuid::now_v7)
}

/// Живой хвост на хабе и хвост итога от воркера.
pub const LIVE_TAIL_BYTES: usize = 64 * 1024;
pub const RESULT_TAIL_BYTES: usize = 32 * 1024;
/// Хвост в сообщении об итоге.
pub const MESSAGE_TAIL_BYTES: usize = 8 * 1024;
/// Сколько задач машина принимает, если отчёт не назвал число.
pub const DEFAULT_MAX_RUNNING: i64 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Нет места на раннере: задача ждёт в хабе без машины.
    WaitingRunner,
    /// Несущая готовит исходники на выбранной машине.
    Staging,
    Queued,
    Dispatched,
    WaitingLease,
    Running,
    /// Раннер закончил, итог сохранён; несущая собирает результаты и убирает исходники.
    Collecting,
    Succeeded,
    Failed,
    TimedOut,
    Canceled,
    Lost,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WaitingRunner => "waiting_runner",
            Self::Staging => "staging",
            Self::Queued => "queued",
            Self::Dispatched => "dispatched",
            Self::WaitingLease => "waiting_lease",
            Self::Running => "running",
            Self::Collecting => "collecting",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Canceled => "canceled",
            Self::Lost => "lost",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "waiting_runner" => Self::WaitingRunner,
            "staging" => Self::Staging,
            "queued" => Self::Queued,
            "dispatched" => Self::Dispatched,
            "waiting_lease" => Self::WaitingLease,
            "running" => Self::Running,
            "collecting" => Self::Collecting,
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
            Self::WaitingRunner => 0,
            Self::Staging => 1,
            Self::Queued => 2,
            Self::Dispatched => 3,
            Self::WaitingLease => 4,
            Self::Running => 5,
            Self::Collecting => 6,
            _ => 7,
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
    /// Пусто, пока задача в `waiting_runner` (и у отменённой оттуда).
    pub machine_id: Option<Uuid>,
    pub remote: bool,
    pub workdir: Option<String>,
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
    pub log_expired_at: Option<DateTime<Utc>>,
    pub late_report: Option<Value>,
    pub waiter_instance: Option<Uuid>,
    pub waiter_deadline: Option<DateTime<Utc>>,
    pub admission_generation: Option<i64>,
    pub finish_report: Option<Value>,
    pub finish_reported_at: Option<DateTime<Utc>>,
    pub labels: Vec<String>,
    pub expected_minutes: i32,
    pub placement: Vec<String>,
    pub source_repo: Option<String>,
    pub source_rev: Option<String>,
    pub source_domain: Option<String>,
    pub outputs: Vec<String>,
    pub carrier_id: Option<Uuid>,
    pub outputs_dir: Option<String>,
    pub outputs_note: Option<String>,
    pub outputs_files: Option<Value>,
    pub owner_job: bool,
    pub outcome_state: Option<String>,
    pub outcome_reason: Option<String>,
    pub collecting_at: Option<DateTime<Utc>>,
    pub cleanup_attempts: i32,
    pub cleanup_sent_at: Option<DateTime<Utc>>,
    pub cleaned_at: Option<DateTime<Utc>>,
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

    /// Исходники подготавливает и убирает несущая: задача проходит `staging` и `collecting`.
    pub fn has_source(&self) -> bool {
        self.source_repo.is_some() && self.carrier_id.is_some()
    }

    /// Раннер или несущая как цель задачи: машина, на которой лежит рабочий каталог.
    pub fn target(&self) -> Option<Uuid> {
        self.machine_id
    }
}

/// Источник задачи для клиентов: имя каталога и короткая ревизия, не полный локальный путь.
#[derive(Serialize, serde::Deserialize, ToSchema, Clone, Debug, PartialEq, Eq)]
pub struct SourceView {
    pub repo: String,
    pub rev: String,
}

/// Последний компонент пути: имя каталога репозитория.
pub fn repo_dir_name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("repo")
}

/// Задача для клиентов: без вывода (его отдают отдельные чтения).
#[derive(Serialize, serde::Deserialize, ToSchema, Clone, Debug)]
pub struct JobView {
    pub id: Uuid,
    pub state: String,
    /// Пусто, пока задача ждёт раннер (`waiting_runner`).
    pub machine_id: Option<Uuid>,
    pub machine_name: Option<String>,
    pub conversation_id: Option<Uuid>,
    pub conversation_title: Option<String>,
    pub title: Option<String>,
    pub workdir: Option<String>,
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
    pub labels: Vec<String>,
    pub expected_minutes: i32,
    pub source: Option<SourceView>,
    /// Каталог на несущей, куда собраны результаты.
    pub outputs_dir: Option<String>,
    pub outputs_note: Option<String>,
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

/// Состояние допуска заказчиков (режим, ожидающий режим, домен) держится до фиксации: очередь
/// разговора, затем его строка `for no key update`, как у настроек и у смены домена, по
/// возрастанию идентификатора. Берётся после замка машины и строк задач, раньше счётчиков
/// событий; параметр — все заказчики, чьи задачи диспетчер проверит в этой транзакции.
pub async fn lock_admission(
    conn: &mut PgConnection,
    conversations: &[Uuid],
) -> Result<(), sqlx::Error> {
    if conversations.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "select conversation_id from queue_state where conversation_id = any($1)
         order by conversation_id for update",
    )
    .bind(conversations)
    .execute(&mut *conn)
    .await?;
    sqlx::query("select id from conversations where id = any($1) order by id for no key update")
        .bind(conversations)
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

/// Имя машины задачи; пусто, пока машины нет (`waiting_runner`).
pub async fn job_machine_name(conn: &mut PgConnection, job: &Job) -> Result<String, sqlx::Error> {
    match job.machine_id {
        Some(machine) => machine_name(conn, machine).await,
        None => Ok(String::new()),
    }
}

pub async fn view(conn: &mut PgConnection, job: &Job) -> Result<JobView, sqlx::Error> {
    let machine_name = job_machine_name(conn, job).await?;
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
        machine_name: job.machine_id.map(|_| machine_name),
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
        labels: job.labels.clone(),
        expected_minutes: job.expected_minutes,
        source: match (&job.source_repo, &job.source_rev) {
            (Some(repo), Some(rev)) => Some(SourceView {
                repo: repo_dir_name(repo).to_owned(),
                rev: rev.chars().take(12).collect(),
            }),
            _ => None,
        },
        outputs_dir: job.outputs_dir.clone(),
        outputs_note: job.outputs_note.clone(),
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
        "machine": job.machine_id.map(|_| machine),
        "exit_code": job.exit_code,
        "duration_ms": job.duration_ms,
        "output_tail": tail_text(tail, RESULT_TAIL_BYTES),
        "output_bytes": job.output_bytes,
        "truncated": job.truncated,
    });
    if let Some(reason) = &job.end_reason {
        value["reason"] = json!(reason);
    }
    if let Some(dir) = &job.outputs_dir {
        value["outputs_dir"] = json!(dir);
    }
    if let Some(note) = &job.outputs_note {
        value["outputs_note"] = json!(note);
    }
    value
}

/// Ответ о задаче, у которой ещё нет итога или итог уйдёт сообщением.
pub fn state_json(job: &Job, machine: &str) -> Value {
    // Пока машины нет (`waiting_runner`), в ответе `machine: null` и признак ожидания.
    let mut value = json!({
        "job_id": job.id,
        "state": job.state,
        "machine": job.machine_id.map(|_| machine),
    });
    if job.state() == State::WaitingRunner {
        value["waiting_runner"] = json!(true);
    }
    if job.cancel_requested_at.is_some() && !job.is_terminal() {
        value["cancel_requested"] = json!(true);
    }
    value
}

/// Хвост для текста сообщения: управляющие байты, которые проверка ввода отвергает (NUL) или
/// которые ломают отображение (ESC и прочие), записываются как `\x00`, `\x1b`. Перевод строки
/// и табуляция остаются. В строке задачи хвост лежит как есть, экранируется только текст.
pub fn escape_control(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() && c != '\n' && c != '\t' {
            let code = c as u32;
            if code < 0x100 {
                out.push_str(&format!("\\x{code:02x}"));
            } else {
                out.push_str(&format!("\\u{{{code:x}}}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Сообщение заказчику об итоге: заголовок и хвост до 8 КиБ.
pub fn message_text(job: &Job, machine: &str) -> String {
    render_message(job, machine, true)
}

/// Сообщение без хвоста: короткий запасной вид, если с хвостом текст не прошёл проверку ввода.
pub fn message_text_without_tail(job: &Job, machine: &str) -> String {
    render_message(job, machine, false)
}

/// Текст для доставки: с хвостом, а если проверка не принимает его, то короткий без хвоста.
/// Итог не пропадает навсегда из-за содержимого вывода.
pub fn message_for_delivery(job: &Job, machine: &str, accepts: impl Fn(&str) -> bool) -> String {
    let full = message_text(job, machine);
    if accepts(&full) {
        full
    } else {
        message_text_without_tail(job, machine)
    }
}

fn render_message(job: &Job, machine: &str, with_tail: bool) -> String {
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
    if let Some(dir) = &job.outputs_dir {
        text.push_str(&format!(
            " Outputs collected on the carrier in {}.",
            crate::session_tools::origin::clean_title(dir)
        ));
    }
    if let Some(note) = &job.outputs_note {
        text.push_str(&format!(
            " {}.",
            crate::session_tools::origin::clean_title(note)
        ));
    }
    let tail = job.tail.as_deref().unwrap_or(&[]);
    if (!with_tail || tail.is_empty()) && job.output_bytes > 0 {
        text.push_str(&format!(
            " Output ({} bytes total) is not shown here. Use job_log {{job_id}} for the log.",
            job.output_bytes
        ));
    } else if tail.is_empty() {
        text.push_str(" No output.");
    } else {
        text.push_str(&format!(
            " Output ({} bytes total{}), last part:\n{}\nUse job_log {{job_id}} for the full log.",
            job.output_bytes,
            if job.truncated { ", log truncated" } else { "" },
            escape_control(&tail_text(tail, MESSAGE_TAIL_BYTES))
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
         where ((machine_id=$1 and kind in ('start_inplace_job','cancel_inplace_job'))
             or (machine_id=$3 and kind in ('stage_source','collect_outputs')))
           and payload->>'job_id'=$2 and delivery_state<>'completed'",
    )
    .bind(job.machine_id)
    .bind(job.id.to_string())
    .bind(job.carrier_id)
    .execute(&mut *conn)
    .await?;
    let done = load(conn, job.id).await?.expect("job row just updated");
    if done.delivery.as_deref() == Some("message") {
        enqueue_message_in_tx(conn, &done).await?;
    }
    // Исходники убирает несущая: команда очистки уходит при любом конце, если цель была выбрана.
    if done.has_source() && done.machine_id.is_some() {
        carrier::ensure_cleanup_in_tx(conn, &done).await?;
    }
    if done.owner_job {
        carrier::enqueue_owner_finished_in_tx(conn, &done).await?;
    }
    // Место освободилось (или ожидание закрыто): расстановка несущей пойдёт после фиксации.
    carrier::notify_place_for_machine(conn, done.machine_id.or(done.carrier_id)).await?;
    emit_changed(conn, &done, correlation_id).await?;
    Ok(Some(done))
}

/// Итог пришёл от раннера: задача с исходниками и результатами для сбора переходит в
/// `collecting`, пока несущая собирает результаты; остальные заканчиваются сразу. Очистка
/// исходников идёт после конца (`finish_in_tx`) и конец не держит. Сообщение об итоге и ждущий
/// вызов срабатывают только при настоящем конце. Под замками машины и строки.
pub async fn runner_finished_in_tx(
    conn: &mut PgConnection,
    job: &Job,
    outcome: Outcome,
    correlation_id: Uuid,
) -> Result<Option<Job>, sqlx::Error> {
    if job.is_terminal() || job.state() == State::Collecting {
        return Ok(None);
    }
    if !job.has_source() || !carrier::wants_collect(&job.outputs, outcome.state, &outcome.reason) {
        return finish_in_tx(conn, job, outcome, correlation_id).await;
    }
    carrier::begin_collecting_in_tx(conn, job, outcome, correlation_id).await
}

/// Машина освободила место или появилась: очередные задачи уходят на неё командами.
/// FIFO по `created_at`. Допуск перепроверяется: отказ закрывает задачу `canceled` с причиной.
pub async fn dispatch_machine_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    correlation_id: Uuid,
) -> Result<usize, sqlx::Error> {
    lock_machine(conn, machine_id).await?;
    let max = max_running_in_tx(conn, machine_id).await?;
    let queued_rows: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "select id, conversation_id from jobs
         where machine_id=$1 and state='queued' order by created_at, id",
    )
    .bind(machine_id)
    .fetch_all(&mut *conn)
    .await?;
    // Допуск заказчиков фиксируется до фиксации диспетчеризации: смена режима или домена
    // либо успевает раньше и видна проверке, либо ждёт и приходит после старта.
    let mut callers: Vec<Uuid> = queued_rows.iter().filter_map(|(_, c)| *c).collect();
    callers.sort_unstable();
    callers.dedup();
    lock_admission(conn, &callers).await?;
    let queued: Vec<Uuid> = queued_rows.into_iter().map(|(id, _)| id).collect();
    let mut sent = 0;
    let mut full = false;
    for id in queued {
        let Some(job) = lock_row(conn, id).await? else {
            continue;
        };
        if job.state() != State::Queued {
            continue;
        }
        // Удалённая задача, ждавшая в очереди, пока машину выключали, не оживает от повторного
        // включения: поколение допуска выросло с её заявки. Задача с исходниками размещена
        // хабом в свободное место и уже подготовлена: пауза раннера (выключатель) закрывает
        // только новые привязки, а не её.
        if job.remote && !job.has_source() {
            let (accepts, generation) =
                crate::machines::remote_admission_for_share(conn, machine_id).await?;
            if !accepts || job.admission_generation.unwrap_or(0) < generation {
                finish_in_tx(
                    conn,
                    &job,
                    Outcome::bare(State::Canceled, Some(Refusal::RemoteWorkDisabled.reason())),
                    correlation_id,
                )
                .await?;
                continue;
            }
        }
        // Заказчик, чьё разрешение сменилось, больше не может ставить новую работу;
        // удалённый заказчик не мешает: заявка была допущена, задача принадлежит владельцу.
        if let Some(conversation) = job.conversation_id
            && let Some(facts) = caller_facts(conn, conversation).await?
        {
            let refusal = if facts.memory_domain.as_deref() != Some(job.domain.as_str()) {
                Some(Refusal::DomainMismatch)
            } else if job.has_source() {
                // Домен проверен по пути исходников при заявке; у раннера путь рабочего
                // каталога не говорит о домене, остаётся проверка режима.
                admit_mode(&facts).err()
            } else {
                admit(
                    conn,
                    &facts,
                    machine_id,
                    job.workdir.as_deref().unwrap_or_default(),
                )
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
            && !job.has_source()
            && !crate::machines::accepts_remote_work_for_share(conn, machine_id).await?
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
        send_start_in_tx(conn, &job, machine_id).await?;
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

/// Режим заказчика при допуске: `bypass` и не боковая сессия. Домен и выключатель проверяет
/// вызывающий.
pub fn admit_mode(facts: &CallerFacts) -> Result<(), Refusal> {
    let mode = spawn::parent_mode(&facts.permission_mode, facts.pending_mode.as_deref());
    if mode != "bypass" || facts.side {
        return Err(Refusal::ModeTooNarrow);
    }
    Ok(())
}

/// Занятые места: задачи у воркера, потерянные, о которых он говорит, что они идут, и задачи,
/// которым несущая готовит исходники на этой машине: место закреплено за ними с привязки.
pub async fn occupied_in_tx(conn: &mut PgConnection, machine_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*) from jobs where machine_id=$1 and (
           state in ('staging','dispatched','waiting_lease','running')
           or (state='lost' and late_report->>'phase'='running'))",
    )
    .bind(machine_id)
    .fetch_one(conn)
    .await
}

/// Сколько задач машина принимает одновременно: из отчёта воркера или по умолчанию.
pub async fn max_running_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let max: Option<i32> = sqlx::query_scalar("select jobs_max_running from machines where id=$1")
        .bind(machine_id)
        .fetch_optional(conn)
        .await?
        .flatten();
    Ok(match max {
        Some(max) if max > 0 => i64::from(max),
        _ => DEFAULT_MAX_RUNNING,
    })
}

pub(super) async fn notify_outbox(
    conn: &mut PgConnection,
    machine_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_notify('canterlot_outbox', $1)")
        .bind(machine_id.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

async fn send_start_in_tx(
    conn: &mut PgConnection,
    job: &Job,
    machine_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, execution_id, execution_generation,
                               kind, payload, delivery_state)
         values ($1, $2, null, null, null, 'start_inplace_job', $3, 'pending')",
    )
    .bind(Uuid::now_v7())
    .bind(machine_id)
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
    notify_outbox(conn, machine_id).await
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
    let Some(machine_id) = job.machine_id else {
        return Ok(());
    };
    if exists {
        return Ok(());
    }
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, execution_id, execution_generation,
                               kind, payload, delivery_state)
         values ($1, $2, null, null, null, 'cancel_inplace_job', $3, 'pending')",
    )
    .bind(Uuid::now_v7())
    .bind(machine_id)
    .bind(json!({"job_id": job.id, "attempt": job.attempt}))
    .execute(&mut *conn)
    .await?;
    notify_outbox(conn, machine_id).await
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
    let machine: Option<Option<Uuid>> =
        sqlx::query_scalar("select machine_id from jobs where id=$1")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(machine) = machine else {
        return Ok(None);
    };
    // У ожидающей раннера задачи машины нет: замок машины не нужен, строку держит она сама.
    if let Some(machine) = machine {
        lock_machine(conn, machine).await?;
    }
    let Some(job) = lock_row(conn, id).await? else {
        return Ok(None);
    };
    match job.state() {
        // Ожидание, подготовка и очередь закрываются сразу; подготовленное убирает очистка,
        // которую `finish_in_tx` ставит для любой задачи с исходниками и выбранной целью.
        State::WaitingRunner | State::Staging | State::Queued => {
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
