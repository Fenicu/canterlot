//! Доска плана оркестратора (pilot.md §12.1 п. 4): `plan.list`, `plan.add`, `plan.update`,
//! `plan.adopt` и автоматика по жизни исполнителей.
//!
//! План принадлежит разговору, который его пишет; вызывающий выводится из живого исполнения.
//! Хранится только текст оркестратора. Модель, коммиты, отчёт и состояние исполнителя
//! выводятся при чтении, поэтому удалённый делегат не оставляет в таблице своего текста.
//!
//! Порядок блокировок: строки `plan_boards`, затем `plan_tasks` берутся последними, после
//! заданий и до `events::append`; держа строку плана, путь других блокировок не берёт
//! (docs/design/delivery.md, «Порядок блокировок инструментов сессии»).

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    Caller, ToolContext, ToolError, ToolErrorCode, access, completion, parse_arguments, send,
    settle::{self, SettleConfig, SettleFacts, Settled},
};
use crate::{
    activity::{Activity, ActivityState, QueueFacts},
    events::{self, NewEvent, Scope},
};

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_NOTE_BYTES: usize = 2048;
pub const MAX_BLOCKER_CHARS: usize = 500;
pub const MAX_FINDINGS: usize = 20;
pub const MAX_FINDING_CHARS: usize = 500;
/// Задач за один вызов `plan.add`.
pub const MAX_ADD: usize = 20;
/// Задач у одного оркестратора за всё время, включая готовые.
pub const MAX_TASKS: i64 = 200;
pub const MAX_COMMITS: usize = 50;
/// Оркестраторов с планом в снимке владельца, свежие первыми.
pub const SNAPSHOT_ORCHESTRATORS: i64 = 50;
/// Сколько дней готовые и отменённые задачи остаются в снимке владельца.
pub const SNAPSHOT_CLOSED_DAYS: i64 = 7;

/// Состояние задачи плана.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    Waiting,
    InProgress,
    InReview,
    Done,
    Cancelled,
    Blocked,
}

impl PlanState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::InProgress => "in_progress",
            Self::InReview => "in_review",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "waiting" => Self::Waiting,
            "in_progress" => Self::InProgress,
            "in_review" => Self::InReview,
            "done" => Self::Done,
            "cancelled" => Self::Cancelled,
            "blocked" => Self::Blocked,
            _ => return None,
        })
    }

    pub fn is_closed(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}

/// Строка `plan_tasks`.
#[derive(sqlx::FromRow, Clone)]
pub struct PlanRow {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub orchestrator_conversation_id: Uuid,
    pub number: i32,
    pub title: String,
    pub note: Option<String>,
    pub blocker: Option<String>,
    pub findings: Value,
    pub state: String,
    pub executor_conversation_id: Option<Uuid>,
    pub linked_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    pub review_fingerprint: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision: i64,
}

/// Тексты задачи — данные модели: в журнал идут только размеры.
impl fmt::Debug for PlanRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanRow")
            .field("id", &self.id)
            .field("number", &self.number)
            .field("state", &self.state)
            .field("title_bytes", &self.title.len())
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// Задача плана: только хранимые поля. Так она идёт в снимок владельца и в
/// `plan_task_changed`; исполнитель известен клиенту из списка разговоров.
#[derive(Serialize, Clone, PartialEq, ToSchema)]
pub struct PlanTask {
    pub id: Uuid,
    pub orchestrator_id: Uuid,
    pub number: i32,
    pub title: String,
    #[schema(required = true)]
    pub note: Option<String>,
    #[schema(required = true)]
    pub blocker: Option<String>,
    pub findings: Vec<String>,
    pub state: PlanState,
    #[schema(required = true)]
    pub executor_id: Option<Uuid>,
    pub attempts: i32,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
}

impl fmt::Debug for PlanTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanTask")
            .field("id", &self.id)
            .field("number", &self.number)
            .field("state", &self.state)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, ToSchema)]
pub struct PlanCommit {
    pub sha: String,
    pub subject: String,
}

/// Задача с выведенными полями исполнителя: считается по запросу
/// (`GET /api/v1/plan` и `plan_list`), в события не идёт.
#[derive(Serialize, Clone, Debug, PartialEq, ToSchema)]
pub struct PlanTaskDetail {
    #[serde(flatten)]
    pub task: PlanTask,
    /// Коммиты ходов исполнителя, свежие первыми, без повторов, не больше 50.
    pub commits: Vec<PlanCommit>,
    /// Список коммитов неполон: обрезан или git не смог прочитать диапазон.
    pub commits_truncated: bool,
    pub files_changed: i64,
    /// Последний ответ основного агента исполнителя.
    #[schema(required = true)]
    pub report_block_id: Option<Uuid>,
    /// Место этого ответа в ленте: ссылка открывает ленту сразу вокруг него.
    #[schema(required = true)]
    pub report_seq: Option<i64>,
}

fn time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn findings_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

pub fn view(row: &PlanRow) -> PlanTask {
    PlanTask {
        id: row.id,
        orchestrator_id: row.orchestrator_conversation_id,
        number: row.number,
        title: row.title.clone(),
        note: row.note.clone(),
        blocker: row.blocker.clone(),
        findings: findings_of(&row.findings),
        state: PlanState::parse(&row.state).unwrap_or(PlanState::Waiting),
        executor_id: row.executor_conversation_id,
        attempts: row.attempts,
        created_at: time(row.created_at),
        updated_at: time(row.updated_at),
        revision: row.revision,
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::with_message(ToolErrorCode::InvalidArguments, message)
}

/// Одна строка: переводы строки и прочие управляющие символы становятся пробелами.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn clean_title(text: &str) -> Result<String, ToolError> {
    let title = one_line(text);
    if title.is_empty() {
        return Err(invalid("title must not be empty"));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(invalid(format!(
            "title is longer than {MAX_TITLE_CHARS} characters"
        )));
    }
    Ok(title)
}

/// Пустая строка очищает заметку.
fn clean_note(text: &str) -> Result<Option<String>, ToolError> {
    let note: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let note = note.trim();
    if note.len() > MAX_NOTE_BYTES {
        return Err(invalid(format!(
            "note is larger than {MAX_NOTE_BYTES} bytes"
        )));
    }
    Ok((!note.is_empty()).then(|| note.to_owned()))
}

fn clean_blocker(text: &str) -> Result<Option<String>, ToolError> {
    let blocker = one_line(text);
    if blocker.chars().count() > MAX_BLOCKER_CHARS {
        return Err(invalid(format!(
            "blocker is longer than {MAX_BLOCKER_CHARS} characters"
        )));
    }
    Ok((!blocker.is_empty()).then_some(blocker))
}

fn clean_findings(items: &[String]) -> Result<Vec<String>, ToolError> {
    if items.len() > MAX_FINDINGS {
        return Err(invalid(format!("at most {MAX_FINDINGS} findings")));
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let finding = one_line(item);
        if finding.chars().count() > MAX_FINDING_CHARS {
            return Err(invalid(format!(
                "a finding is longer than {MAX_FINDING_CHARS} characters"
            )));
        }
        if !finding.is_empty() {
            out.push(finding);
        }
    }
    Ok(out)
}

/// Событие владельцу. План делегата в общую область не попадает: его не видно и в снимке.
pub async fn changed_in_tx(
    conn: &mut PgConnection,
    row: &PlanRow,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let root: Option<bool> =
        sqlx::query_scalar("select spawn_depth=0 from conversations where id=$1")
            .bind(row.orchestrator_conversation_id)
            .fetch_optional(&mut *conn)
            .await?;
    if root != Some(true) {
        return Ok(());
    }
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversations {
                owner_id: row.owner_id,
            },
            kind: "plan_task_changed",
            conversation_id: Some(row.orchestrator_conversation_id),
            execution_id: None,
            payload: json!({"task": view(row)}),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

async fn emit_all(
    conn: &mut PgConnection,
    rows: &[PlanRow],
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    for row in rows {
        changed_in_tx(conn, row, correlation_id).await?;
    }
    Ok(())
}

/// Строка счётчика номеров оркестратора, взятая `for update`.
async fn lock_board(
    conn: &mut PgConnection,
    owner_id: Uuid,
    orchestrator: Uuid,
) -> Result<i32, sqlx::Error> {
    sqlx::query(
        "insert into plan_boards(orchestrator_conversation_id,owner_id) values($1,$2)
         on conflict do nothing",
    )
    .bind(orchestrator)
    .bind(owner_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query_scalar(
        "select next_number from plan_boards where orchestrator_conversation_id=$1 for update",
    )
    .bind(orchestrator)
    .fetch_one(conn)
    .await
}

async fn set_next(
    conn: &mut PgConnection,
    orchestrator: Uuid,
    next: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update plan_boards set next_number=$2,updated_at=clock_timestamp()
         where orchestrator_conversation_id=$1",
    )
    .bind(orchestrator)
    .bind(next)
    .execute(conn)
    .await?;
    Ok(())
}

async fn task_count(conn: &mut PgConnection, orchestrator: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("select count(*) from plan_tasks where orchestrator_conversation_id=$1")
        .bind(orchestrator)
        .fetch_one(conn)
        .await
}

fn full(existing: i64, adding: usize) -> Result<(), ToolError> {
    if existing + adding as i64 > MAX_TASKS {
        return Err(ToolError::with_message(
            ToolErrorCode::PlanFull,
            format!(
                "This plan already has {existing} tasks; a plan holds at most {MAX_TASKS}. Cancel or adopt into a fresh session instead."
            ),
        ));
    }
    Ok(())
}

pub async fn rows_of(
    conn: &mut PgConnection,
    owner_id: Uuid,
    orchestrator: Uuid,
) -> Result<Vec<PlanRow>, sqlx::Error> {
    sqlx::query_as(
        "select * from plan_tasks where owner_id=$1 and orchestrator_conversation_id=$2
         order by number",
    )
    .bind(owner_id)
    .bind(orchestrator)
    .fetch_all(conn)
    .await
}

/// Выведенные поля: коммиты всех ходов исполнителя (он всегда свежий ребёнок, поэтому
/// фильтр по времени связи не нужен) и его последний ответ.
pub async fn details(
    conn: &mut PgConnection,
    rows: &[PlanRow],
) -> Result<Vec<PlanTaskDetail>, sqlx::Error> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut detail = PlanTaskDetail {
            task: view(row),
            commits: Vec::new(),
            commits_truncated: false,
            files_changed: 0,
            report_block_id: None,
            report_seq: None,
        };
        if let Some(executor) = row.executor_conversation_id {
            let runs: Vec<Option<Value>> = sqlx::query_scalar(
                "select git from runs where conversation_id=$1
                 order by coalesce(finished_at,started_at),started_at,input_command_id",
            )
            .bind(executor)
            .fetch_all(&mut *conn)
            .await?;
            let git = super::summary::plan_git(&runs, MAX_COMMITS);
            detail.commits = git
                .commits
                .into_iter()
                .map(|(sha, subject)| PlanCommit { sha, subject })
                .collect();
            detail.commits_truncated = git.truncated;
            detail.files_changed = git.files_changed as i64;
            let query = format!(
                "select block_id,created_seq from blocks
                 where conversation_id=$1 and kind='text' and parent_block_id is null
                   and {} and btrim(content) <> ''
                 order by created_seq desc nulls last,created_at desc limit 1",
                crate::agents::main_agent_filter("agent")
            );
            let report: Option<(Uuid, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(query))
                .bind(executor)
                .fetch_optional(&mut *conn)
                .await?;
            detail.report_block_id = report.map(|(id, _)| id);
            detail.report_seq = report.map(|(_, seq)| seq);
        }
        out.push(detail);
    }
    Ok(out)
}

/// Снимок владельца: задачи корневых оркестраторов вне архива и корзины, без готовых и
/// отменённых старше 7 дней; не больше 50 оркестраторов, свежие первыми.
pub async fn snapshot_views(
    conn: &mut PgConnection,
    owner_id: Uuid,
) -> Result<Vec<PlanTask>, sqlx::Error> {
    let rows: Vec<PlanRow> = sqlx::query_as(
        "with visible as (
           select p.* from plan_tasks p
           join conversations c on c.id=p.orchestrator_conversation_id
           where p.owner_id=$1 and c.owner_id=$1 and c.spawn_depth=0
             and c.archived_at is null and c.trashed_at is null
             and (p.state not in ('done','cancelled')
                  or p.updated_at > clock_timestamp() - make_interval(days => $2::int))),
         boards as (
           select orchestrator_conversation_id id, max(updated_at) at from visible
           group by orchestrator_conversation_id
           order by max(updated_at) desc, orchestrator_conversation_id
           limit $3)
         select v.* from visible v join boards b on b.id=v.orchestrator_conversation_id
         order by b.at desc, v.orchestrator_conversation_id, v.number",
    )
    .bind(owner_id)
    .bind(SNAPSHOT_CLOSED_DAYS as i32)
    .bind(SNAPSHOT_ORCHESTRATORS)
    .fetch_all(conn)
    .await?;
    Ok(rows.iter().map(view).collect())
}

fn compact(tasks: &[PlanTaskDetail]) -> String {
    if tasks.is_empty() {
        return "Your plan is empty. Add tasks with plan_add.".to_owned();
    }
    let mut lines = Vec::with_capacity(tasks.len());
    for task in tasks {
        let mut line = format!(
            "#{} [{}] {}",
            task.task.number,
            task.task.state.as_str(),
            task.task.title
        );
        if let Some(executor) = task.task.executor_id {
            line.push_str(&format!(" (executor {executor}"));
            if !task.commits.is_empty() {
                line.push_str(&format!(
                    ", {}{} commits",
                    task.commits.len(),
                    if task.commits_truncated { "+" } else { "" }
                ));
            }
            line.push(')');
        }
        if let Some(blocker) = &task.task.blocker {
            line.push_str(&format!(" blocked: {blocker}"));
        }
        lines.push(line);
    }
    lines.join("\n")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {}

/// `plan.list`: план вызывающего с выведенными полями.
pub async fn list(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let _: ListArgs = parse_arguments(arguments)?;
    let mut conn = context.pool.acquire().await?;
    access::caller_in_tx(&mut conn, caller.owner_id, caller.conversation_id).await?;
    let rows = rows_of(&mut conn, caller.owner_id, caller.conversation_id).await?;
    let tasks = details(&mut conn, &rows).await?;
    let hint = compact(&tasks);
    Ok(json!({"tasks": tasks, "hint": hint}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewTask {
    title: String,
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddArgs {
    tasks: Vec<NewTask>,
}

/// `plan.add`: 1–20 задач, номера по счётчику оркестратора.
pub async fn add(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: AddArgs = parse_arguments(arguments)?;
    if args.tasks.is_empty() || args.tasks.len() > MAX_ADD {
        return Err(invalid(format!("pass 1 to {MAX_ADD} tasks")));
    }
    let mut cleaned = Vec::with_capacity(args.tasks.len());
    for task in &args.tasks {
        let note = match task.note.as_deref() {
            Some(note) => clean_note(note)?,
            None => None,
        };
        cleaned.push((clean_title(&task.title)?, note));
    }
    let mut tx = crate::db::begin(&context.pool).await?;
    send::lock_sender(&mut tx, caller.conversation_id).await?;
    access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    let mut next = lock_board(&mut tx, caller.owner_id, caller.conversation_id).await?;
    full(
        task_count(&mut tx, caller.conversation_id).await?,
        cleaned.len(),
    )?;
    let mut rows = Vec::with_capacity(cleaned.len());
    for (title, note) in cleaned {
        let row: PlanRow = sqlx::query_as(
            "insert into plan_tasks(id,owner_id,orchestrator_conversation_id,number,title,note)
             values($1,$2,$3,$4,$5,$6) returning *",
        )
        .bind(Uuid::now_v7())
        .bind(caller.owner_id)
        .bind(caller.conversation_id)
        .bind(next)
        .bind(title)
        .bind(note)
        .fetch_one(&mut *tx)
        .await?;
        next += 1;
        rows.push(row);
    }
    set_next(&mut tx, caller.conversation_id, next).await?;
    emit_all(&mut tx, &rows, caller.call_id).await?;
    tx.commit().await?;
    let numbers: Vec<i32> = rows.iter().map(|row| row.number).collect();
    Ok(json!({
        "numbers": numbers,
        "tasks": rows.iter().map(view).collect::<Vec<_>>(),
        "hint": "Pass plan_task to session_spawn to link a delegate; its finish moves the task to in_review and session_accept to done.",
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateArgs {
    number: i32,
    state: Option<PlanState>,
    title: Option<String>,
    note: Option<String>,
    blocker: Option<String>,
    findings: Option<Vec<String>>,
}

/// `plan.update`: ручная правка любой задачи своего плана.
pub async fn update(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: UpdateArgs = parse_arguments(arguments)?;
    let title = args.title.as_deref().map(clean_title).transpose()?;
    let note = args.note.as_deref().map(clean_note).transpose()?;
    let blocker = args.blocker.as_deref().map(clean_blocker).transpose()?;
    let findings = args.findings.as_deref().map(clean_findings).transpose()?;
    let mut tx = crate::db::begin(&context.pool).await?;
    send::lock_sender(&mut tx, caller.conversation_id).await?;
    access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    let row: Option<PlanRow> = sqlx::query_as(
        "select * from plan_tasks
         where owner_id=$1 and orchestrator_conversation_id=$2 and number=$3 for update",
    )
    .bind(caller.owner_id)
    .bind(caller.conversation_id)
    .bind(args.number)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        return Err(not_found(args.number));
    };
    let mut next = row.clone();
    if let Some(title) = title {
        next.title = title;
    }
    if let Some(note) = note {
        next.note = note;
    }
    if let Some(findings) = findings {
        next.findings = json!(findings);
    }
    let current = PlanState::parse(&row.state).unwrap_or(PlanState::Waiting);
    let mut state = current;
    if let Some(blocker) = blocker {
        match (&blocker, args.state) {
            // Причина без состояния — это блокировка.
            (Some(_), None) => state = PlanState::Blocked,
            // Снятая причина снимает и блокировку.
            (None, None) if current == PlanState::Blocked => {
                state = if row.executor_conversation_id.is_some() {
                    PlanState::InProgress
                } else {
                    PlanState::Waiting
                };
            }
            _ => {}
        }
        next.blocker = blocker;
    }
    if let Some(wanted) = args.state {
        state = wanted;
    }
    next.state = state.as_str().to_owned();
    let changed = next.title != row.title
        || next.note != row.note
        || next.blocker != row.blocker
        || next.findings != row.findings
        || next.state != row.state;
    if !changed {
        tx.commit().await?;
        return Ok(json!({"status": "unchanged", "task": view(&row)}));
    }
    let saved: PlanRow = sqlx::query_as(
        "update plan_tasks set title=$2,note=$3,blocker=$4,findings=$5,state=$6,
           revision=revision+1,updated_at=clock_timestamp()
         where id=$1 returning *",
    )
    .bind(row.id)
    .bind(&next.title)
    .bind(&next.note)
    .bind(&next.blocker)
    .bind(&next.findings)
    .bind(&next.state)
    .fetch_one(&mut *tx)
    .await?;
    changed_in_tx(&mut tx, &saved, caller.call_id).await?;
    tx.commit().await?;
    Ok(json!({"status": "updated", "task": view(&saved)}))
}

fn not_found(number: i32) -> ToolError {
    ToolError::with_message(
        ToolErrorCode::PlanTaskNotFound,
        format!("Your plan has no task #{number}; plan_list shows the numbers"),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AdoptArgs {
    from_session_id: Uuid,
}

/// `plan.adopt`: открытые задачи другого корневого разговора того же владельца переходят
/// в план вызывающего с новыми номерами. Источник может лежать в корзине: это главный
/// случай передачи плана свежему оркестратору.
pub async fn adopt(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: AdoptArgs = parse_arguments(arguments)?;
    if args.from_session_id == caller.conversation_id {
        return Err(invalid("from_session_id is this session"));
    }
    let mut tx = crate::db::begin(&context.pool).await?;
    send::lock_sender(&mut tx, caller.conversation_id).await?;
    // Строки обоих разговоров по возрастанию ID, как у остальных инструментов.
    let mut pair = [caller.conversation_id, args.from_session_id];
    pair.sort();
    let found: Vec<(Uuid, i32)> = sqlx::query_as(
        "select id,spawn_depth from conversations where id=any($1) and owner_id=$2
         order by id for key share",
    )
    .bind(&pair[..])
    .bind(caller.owner_id)
    .fetch_all(&mut *tx)
    .await?;
    let me = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    if me.spawn_depth != 0 {
        return Err(invalid(
            "Only a root session (one the owner started) can adopt a plan",
        ));
    }
    let source = found
        .iter()
        .find(|(id, _)| *id == args.from_session_id)
        .ok_or_else(|| ToolError::new(ToolErrorCode::SessionNotFound))?;
    if source.1 != 0 {
        return Err(invalid(
            "The source is a delegated session; only a root session's plan can be adopted",
        ));
    }
    // Счётчики обоих по возрастанию ID.
    let mut mine = 0;
    for id in pair {
        let next = lock_board(&mut tx, caller.owner_id, id).await?;
        if id == caller.conversation_id {
            mine = next;
        }
    }
    let moving: Vec<PlanRow> = sqlx::query_as(
        "select * from plan_tasks where orchestrator_conversation_id=$1 and state in ('waiting','in_progress','in_review','blocked')
         order by number for update",
    )
    .bind(args.from_session_id)
    .fetch_all(&mut *tx)
    .await?;
    full(
        task_count(&mut tx, caller.conversation_id).await?,
        moving.len(),
    )?;
    let mut mapping = Vec::with_capacity(moving.len());
    let mut moved = Vec::with_capacity(moving.len());
    for row in moving {
        let saved: PlanRow = sqlx::query_as(
            "update plan_tasks set orchestrator_conversation_id=$2,number=$3,
               revision=revision+1,updated_at=clock_timestamp()
             where id=$1 returning *",
        )
        .bind(row.id)
        .bind(caller.conversation_id)
        .bind(mine)
        .fetch_one(&mut *tx)
        .await?;
        mapping.push(json!({"from": row.number, "to": mine}));
        mine += 1;
        moved.push(saved);
    }
    set_next(&mut tx, caller.conversation_id, mine).await?;
    emit_all(&mut tx, &moved, caller.call_id).await?;
    tx.commit().await?;
    let linked = moved
        .iter()
        .filter(|row| row.executor_conversation_id.is_some())
        .count();
    let hint = if moved.is_empty() {
        "The source has no open tasks; nothing moved.".to_owned()
    } else if linked > 0 {
        format!(
            "Moved {} tasks. {linked} keep their executors; you may session_accept them from this session.",
            moved.len()
        )
    } else {
        format!("Moved {} tasks.", moved.len())
    };
    Ok(json!({"moved": mapping, "hint": hint}))
}

/// Номер задачи из `session_spawn` → её ID, один раз при проверке вызова.
pub async fn resolve_for_spawn_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    orchestrator: Uuid,
    number: i32,
) -> Result<Uuid, ToolError> {
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "select id,state from plan_tasks
         where owner_id=$1 and orchestrator_conversation_id=$2 and number=$3",
    )
    .bind(owner_id)
    .bind(orchestrator)
    .bind(number)
    .fetch_optional(conn)
    .await?;
    let Some((id, state)) = row else {
        return Err(not_found(number));
    };
    if PlanState::parse(&state).is_some_and(PlanState::is_closed) {
        return Err(ToolError::with_message(
            ToolErrorCode::PlanTaskClosed,
            format!("Task #{number} is {state}; reopen it with plan_update first"),
        ));
    }
    Ok(id)
}

/// Исход связи задачи с ребёнком в `start_child`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkOutcome {
    Linked {
        number: i32,
    },
    /// Задачу за время ожидания закрыли, перенесли или удалили: ребёнок стартует без связи.
    NotLinked {
        number: Option<i32>,
    },
}

impl LinkOutcome {
    pub fn note(&self) -> String {
        match self {
            Self::Linked { number } => format!(
                "Linked to plan task #{number}: it moves to in_review when the session finishes."
            ),
            Self::NotLinked {
                number: Some(number),
            } => format!(
                "Plan task #{number} was closed or moved meanwhile; the session is not linked to it."
            ),
            Self::NotLinked { number: None } => {
                "The plan task is gone; the session is not linked to it.".to_owned()
            }
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            Self::Linked { number } => json!({"number": number, "linked": true}),
            Self::NotLinked { number } => json!({"number": number, "linked": false}),
        }
    }
}

/// Последний оператор `start_child`: задача всё ещё в плане родителя и открыта —
/// ребёнок становится её исполнителем. Иначе запуск идёт без связи и не падает.
pub async fn link_child_in_tx(
    conn: &mut PgConnection,
    parent: Uuid,
    child: Uuid,
    task_id: Uuid,
    correlation_id: Uuid,
) -> Result<LinkOutcome, sqlx::Error> {
    let linked: Option<PlanRow> = sqlx::query_as(
        "update plan_tasks set executor_conversation_id=$3,linked_at=clock_timestamp(),
           attempts=attempts+1,state='in_progress',blocker=null,review_fingerprint=null,
           revision=revision+1,updated_at=clock_timestamp()
         where id=$1 and orchestrator_conversation_id=$2 and state in ('waiting','in_progress','in_review','blocked')
         returning *",
    )
    .bind(task_id)
    .bind(parent)
    .bind(child)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(row) = linked {
        changed_in_tx(conn, &row, correlation_id).await?;
        return Ok(LinkOutcome::Linked { number: row.number });
    }
    let number: Option<i32> = sqlx::query_scalar(
        "select number from plan_tasks where id=$1 and orchestrator_conversation_id=$2",
    )
    .bind(task_id)
    .bind(parent)
    .fetch_optional(conn)
    .await?;
    Ok(LinkOutcome::NotLinked { number })
}

/// Автоматика по проекции исполнителя (из `activity::evaluate_subscriptions`):
/// - новый ход (`working`) возвращает задачу с проверки в работу;
/// - ровно `Settled::Idle` с отпущенным шлюзом автопинка и *новым* отпечатком переводит
///   задачу из работы на проверку. Остальные устоявшиеся состояния задачу не трогают.
///
/// Повторная оценка без перемены ревизию не двигает.
#[allow(clippy::too_many_arguments)]
pub async fn evaluate_executor_in_tx(
    conn: &mut PgConnection,
    executor: Uuid,
    activity: &Activity,
    queue: &QueueFacts,
    facts: &SettleFacts,
    config: &SettleConfig,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let linked: bool = sqlx::query_scalar(
        "select exists(select 1 from plan_tasks
         where executor_conversation_id=$1 and state in ('in_progress','in_review'))",
    )
    .bind(executor)
    .fetch_one(&mut *conn)
    .await?;
    if !linked {
        return Ok(());
    }
    if activity.state == ActivityState::Working {
        let rows: Vec<PlanRow> = sqlx::query_as(
            "update plan_tasks set state='in_progress',revision=revision+1,
               updated_at=clock_timestamp()
             where executor_conversation_id=$1 and state='in_review' returning *",
        )
        .bind(executor)
        .fetch_all(&mut *conn)
        .await?;
        return emit_all(conn, &rows, correlation_id).await;
    }
    if settle::settled(activity, queue, facts, config) != Some(Settled::Idle) {
        return Ok(());
    }
    let fingerprint = settle::fingerprint(
        Settled::Idle,
        facts.since,
        facts.last_turn_at,
        facts.phase_since,
    );
    let pending: bool = sqlx::query_scalar(
        "select exists(select 1 from plan_tasks
         where executor_conversation_id=$1 and state='in_progress'
           and review_fingerprint is distinct from $2)",
    )
    .bind(executor)
    .bind(&fingerprint)
    .fetch_one(&mut *conn)
    .await?;
    if !pending {
        return Ok(());
    }
    // Тот же шлюз, что у подписки и будильников: повторный вызов с тем же отпечатком
    // второго задания толчка не ставит.
    if completion::gate_in_tx(
        conn,
        executor,
        Settled::Idle,
        &fingerprint,
        facts.now,
        config.nudge_enabled,
    )
    .await?
    {
        return Ok(());
    }
    let rows: Vec<PlanRow> = sqlx::query_as(
        "update plan_tasks set state='in_review',review_fingerprint=$2,revision=revision+1,
           updated_at=clock_timestamp()
         where executor_conversation_id=$1 and state='in_progress'
           and review_fingerprint is distinct from $2
         returning *",
    )
    .bind(executor)
    .bind(&fingerprint)
    .fetch_all(&mut *conn)
    .await?;
    emit_all(conn, &rows, correlation_id).await
}

/// Приём результата (MCP и PWA): открытая задача этого исполнителя готова.
pub async fn on_accept_in_tx(
    conn: &mut PgConnection,
    executor: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let rows: Vec<PlanRow> = sqlx::query_as(
        "update plan_tasks set state='done',blocker=null,revision=revision+1,
           updated_at=clock_timestamp()
         where executor_conversation_id=$1 and state in ('waiting','in_progress','in_review','blocked')
         returning *",
    )
    .bind(executor)
    .fetch_all(&mut *conn)
    .await?;
    emit_all(conn, &rows, correlation_id).await
}

/// Вызывающий держит задачу, связанную с этим исполнителем: исключение из правила
/// «принимает только прямой родитель» для перенятого плана.
pub async fn owns_executor_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    orchestrator: Uuid,
    executor: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(select 1 from plan_tasks
         where owner_id=$1 and orchestrator_conversation_id=$2 and executor_conversation_id=$3)",
    )
    .bind(owner_id)
    .bind(orchestrator)
    .bind(executor)
    .fetch_one(conn)
    .await
}

/// Задачи, связанные с исполнителем, до его удаления: внешний ключ обнулит связь молча.
pub async fn linked_to_in_tx(
    conn: &mut PgConnection,
    executor: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar("select id from plan_tasks where executor_conversation_id=$1 order by id")
        .bind(executor)
        .fetch_all(conn)
        .await
}

/// После `purge_events`: старые события задач с ID исполнителя скрыты, поэтому
/// переподключившийся клиент получает новую ревизию без удалённого ID.
pub async fn on_executor_deleted_in_tx(
    conn: &mut PgConnection,
    task_ids: &[Uuid],
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    if task_ids.is_empty() {
        return Ok(());
    }
    let rows: Vec<PlanRow> = sqlx::query_as(
        "update plan_tasks set revision=revision+1,updated_at=clock_timestamp()
         where id=any($1) returning *",
    )
    .bind(task_ids)
    .fetch_all(&mut *conn)
    .await?;
    emit_all(conn, &rows, correlation_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_is_one_trimmed_line() {
        assert_eq!(clean_title("  a\nb\tc ").unwrap(), "a b c");
        assert!(clean_title(" \n ").is_err());
        assert!(clean_title(&"x".repeat(201)).is_err());
        assert!(clean_title(&"я".repeat(200)).is_ok());
    }

    #[test]
    fn empty_note_and_blocker_clear() {
        assert_eq!(clean_note("").unwrap(), None);
        assert_eq!(clean_blocker("  ").unwrap(), None);
        assert_eq!(clean_note("a\nb").unwrap().as_deref(), Some("a\nb"));
        assert!(clean_note(&"x".repeat(2049)).is_err());
    }

    #[test]
    fn findings_are_capped() {
        assert!(clean_findings(&vec!["a".to_owned(); 21]).is_err());
        assert!(clean_findings(&["x".repeat(501)]).is_err());
        assert_eq!(
            clean_findings(&["a".into(), " ".into()]).unwrap(),
            vec!["a".to_owned()]
        );
    }

    #[test]
    fn states_round_trip() {
        for state in [
            PlanState::Waiting,
            PlanState::InProgress,
            PlanState::InReview,
            PlanState::Done,
            PlanState::Cancelled,
            PlanState::Blocked,
        ] {
            assert_eq!(PlanState::parse(state.as_str()), Some(state));
        }
    }
}
