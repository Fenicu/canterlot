//! Итог делегированной сессии для подписчика: текст события о том, что цель осела.
//!
//! Окно итога начинается с момента взвода подписки. Ответы на секретные вопросы в текст
//! не попадают: о вопросе говорит только `extract_request_summary`.

use std::fmt::{self, Write as _};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, types::Json};
use uuid::Uuid;

use crate::{
    activity::{Evidence, OWNER_LOST, extract_request_summary},
    requests::RequestKind,
    session_tools::origin::{HubCommit, HubEvent},
};

/// Предел итогового ответа в тексте события.
pub const ANSWER_LIMIT: usize = 8192;
/// Предел всего текста события.
pub const TEXT_LIMIT: usize = 16 * 1024;
const COMMITS_BUDGET: usize = 3 * 1024;
const FILES_BUDGET: usize = 2 * 1024;
const SHORT_FIELD: usize = 200;
const PATH_FIELD: usize = 1024;

/// Ход для итога: команда, длительность, git-факты и рабочее время.
type RunRow = (Uuid, Option<i64>, Option<Value>, Option<i64>, bool);

#[derive(Deserialize, Default)]
#[serde(default)]
struct RunGit {
    available: bool,
    head_before: String,
    head_after: String,
    dirty: bool,
    truncated: bool,
    commits: Vec<Commit>,
    files: Vec<File>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Commit {
    sha: String,
    subject: String,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct File {
    path: String,
    status: String,
}

struct Git {
    head_before: String,
    head_after: String,
    dirty: bool,
    truncated: bool,
    /// Свежие первыми, как в `git log`.
    commits: Vec<Commit>,
    files: Vec<File>,
}

struct Answer {
    block_id: Uuid,
    head: String,
    total_bytes: usize,
}

enum Detail {
    None,
    Waiting {
        question: Option<String>,
    },
    Stalled {
        phase: String,
        last_event_at: DateTime<Utc>,
        process_alive: Option<bool>,
        cpu_busy: Option<bool>,
    },
    Failed {
        reason: String,
    },
    QuotaPaused {
        provider: Option<String>,
        window: Option<String>,
        until: Option<DateTime<Utc>>,
    },
    Stopped {
        outcome: String,
    },
}

pub struct SessionSummary {
    pub id: Uuid,
    pub title: String,
    pub harness: String,
    pub model: String,
    state: String,
    turns: Option<usize>,
    duration_ms: Option<i64>,
    working_ms: Option<i64>,
    /// `None`, пока ход идёт и расход ещё не записан.
    tokens: Option<(i64, i64)>,
    git: Option<Git>,
    worktree: Option<(String, String)>,
    detail: Detail,
    answer: Option<Answer>,
}

impl fmt::Debug for SessionSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionSummary")
            .field("id", &self.id)
            .field("state", &self.state)
            .field("turns", &self.turns)
            .finish_non_exhaustive()
    }
}

/// Одна строка без переводов строки и не длиннее `max` символов.
fn one_line(text: &str, max: usize) -> String {
    text.chars()
        .take(max)
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

fn truncate_at_boundary(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

fn aggregate_git(runs: &[Option<Value>]) -> Option<Git> {
    let facts: Vec<RunGit> = runs
        .iter()
        .filter_map(|git| git.clone())
        .filter_map(|value| serde_json::from_value::<RunGit>(value).ok())
        .filter(|git| git.available)
        .collect();
    let last = facts.last()?;
    let head_before = facts
        .iter()
        .map(|git| git.head_before.as_str())
        .find(|head| !head.is_empty())
        .unwrap_or_default()
        .to_owned();
    let mut commits: Vec<Commit> = Vec::new();
    for git in facts.iter().rev() {
        for commit in &git.commits {
            if !commits.iter().any(|seen| seen.sha == commit.sha) {
                commits.push(commit.clone());
            }
        }
    }
    let mut files: Vec<File> = Vec::new();
    for git in &facts {
        for file in &git.files {
            match files.iter_mut().find(|seen| seen.path == file.path) {
                Some(seen) => seen.status.clone_from(&file.status),
                None => files.push(file.clone()),
            }
        }
    }
    Some(Git {
        head_before,
        head_after: last.head_after.clone(),
        dirty: last.dirty,
        truncated: facts.iter().any(|git| git.truncated),
        commits,
        files,
    })
}

/// Граница завершения, удержанная пачкой: идентичность, а не время.
///
/// Часы воркера (`runs.finished_at`) и часы хаба (`blocks.created_at`) идут по-разному,
/// а после переподключения и повтора журнала ответ цели появляется в базе позже
/// завершения хода. Поэтому верхняя граница окна — идентификатор последнего хода
/// (`runs.input_command_id`) и снятая в момент удержания последовательность блоков хаба.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Boundary {
    /// Последний законченный ход, который покрывает удержанный итог.
    pub last_command_id: Uuid,
    /// Команды всех ходов, известных на момент удержания: их ответы входят в итог.
    pub command_ids: Vec<Uuid>,
    /// Наибольший `blocks.created_seq` разговора на момент удержания.
    #[serde(default)]
    pub block_seq: i64,
}

/// Граница по текущей проекции цели: последний ход и последовательность блоков хаба.
/// `None`, если ходов ещё не было: тогда итог строится по прежнему окну подписки.
pub async fn boundary_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
) -> Result<Option<Boundary>, sqlx::Error> {
    let command_ids: Vec<Uuid> = sqlx::query_scalar(
        "select input_command_id from runs where conversation_id=$1 order by input_command_id",
    )
    .bind(target_id)
    .fetch_all(&mut *conn)
    .await?;
    let Some(last_command_id) = command_ids.last().copied() else {
        return Ok(None);
    };
    let block_seq: i64 = sqlx::query_scalar(
        "select coalesce(max(created_seq),0)::bigint from blocks where conversation_id=$1",
    )
    .bind(target_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(Some(Boundary {
        last_command_id,
        command_ids,
        block_seq,
    }))
}

/// Собирает итог. `None`, если цели нет у этого владельца.
pub async fn build(
    conn: &mut PgConnection,
    owner_id: Uuid,
    target_id: Uuid,
    state: &str,
    armed_at: Option<DateTime<Utc>>,
) -> Result<Option<SessionSummary>, sqlx::Error> {
    build_bounded(conn, owner_id, target_id, state, armed_at, None).await
}

/// Тот же итог, но не позже удержанной границы: более поздний ход цели не подменяет
/// результат, который пачка уже собрала. Граница — идентичность, время не сравнивается.
pub async fn build_bounded(
    conn: &mut PgConnection,
    owner_id: Uuid,
    target_id: Uuid,
    state: &str,
    armed_at: Option<DateTime<Utc>>,
    boundary: Option<&Boundary>,
) -> Result<Option<SessionSummary>, sqlx::Error> {
    let conversation: Option<(String, String, String, Option<Value>)> = sqlx::query_as(
        "select title,harness,model,worktree from conversations where id=$1 and owner_id=$2",
    )
    .bind(target_id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((title, harness, model, worktree)) = conversation else {
        return Ok(None);
    };
    // Верхняя граница — идентификатор хода, а не время: часы воркера и хаба расходятся.
    // Нижняя граница окна остаётся моментом взвода подписки: ход, начатый до него и
    // законченный после, всё ещё принадлежит окну.
    let runs: Vec<RunRow> = sqlx::query_as(
        "select r.input_command_id,
                coalesce(r.duration_ms,
                  (extract(epoch from (coalesce(r.finished_at,now()) - r.started_at)) * 1000)::bigint),
                r.git,w.working_ms,r.finished_at is null
         from runs r
         left join run_working w on w.input_command_id=r.input_command_id
         where r.conversation_id=$1
           and ($3::uuid is null or r.input_command_id <= $3)
           and ((r.finished_at is not null
                 and ($2::timestamptz is null or r.finished_at >= $2))
                or (r.finished_at is null
                    and $3::uuid is null
                    and exists(select 1 from executions e where e.id=r.execution_id
                               and e.state in ('created','starting','running','stopping'))))
         order by coalesce(r.finished_at,r.started_at),r.started_at,r.input_command_id",
    )
    .bind(target_id)
    .bind(armed_at)
    .bind(boundary.map(|boundary| boundary.last_command_id))
    .fetch_all(&mut *conn)
    .await?;
    let run_ids: Vec<Uuid> = runs.iter().map(|run| run.0).collect();
    let (tokens_in, tokens_out): (i64, i64) = sqlx::query_as(
        "select coalesce(sum(input_tokens),0)::bigint,coalesce(sum(output_tokens),0)::bigint
         from run_model_usage where input_command_id = any($1)",
    )
    .bind(&run_ids)
    .fetch_one(&mut *conn)
    .await?;
    let any_open = runs.iter().any(|run| run.4);
    // Удержанный итог снят в момент, когда цель уже устоялась: идущего хода в нём нет.
    let active = if boundary.is_some() {
        false
    } else {
        sqlx::query_scalar(
            "select exists(select 1 from executions where conversation_id=$1
                           and state in ('created','starting','running','stopping'))",
        )
        .bind(target_id)
        .fetch_one(&mut *conn)
        .await?
    };
    // У идущего хода расход ещё не записан: показываем «unknown», а не нули.
    let (turns, tokens) = if runs.is_empty() && active {
        (None, None)
    } else {
        (
            Some(runs.len()),
            (!any_open).then_some((tokens_in, tokens_out)),
        )
    };
    let git_values: Vec<Option<Value>> = runs.iter().map(|run| run.2.clone()).collect();
    let mut git = aggregate_git(&git_values);
    if git.is_none() {
        let previous: Option<Value> = sqlx::query_scalar(
            "select git from runs where conversation_id=$1 and git is not null
             and git->>'available' = 'true'
             and ($2::uuid is null or input_command_id <= $2)
             order by started_at desc limit 1",
        )
        .bind(target_id)
        .bind(boundary.map(|boundary| boundary.last_command_id))
        .fetch_optional(&mut *conn)
        .await?;
        git = aggregate_git(&[previous]);
    }
    // Ответ удержанного итога — по идентичности хода или по последовательности блоков хаба,
    // снятой при удержании. Без границы действует прежнее окно подписки.
    let answer: Option<(Uuid, String, i64)> = if let Some(boundary) = boundary {
        sqlx::query_as(
            "select block_id,left(content,$5::int),octet_length(content)::bigint from blocks
                 where conversation_id=$1 and kind='text' and parent_block_id is null
                   and btrim(content) <> ''
                   and ($2::timestamptz is null or created_at >= $2)
                   and (input_command_id = any($3) or coalesce(created_seq,0) <= $4)
                 order by created_seq desc,created_at desc limit 1",
        )
        .bind(target_id)
        .bind(armed_at)
        .bind(&boundary.command_ids)
        .bind(boundary.block_seq)
        .bind(ANSWER_LIMIT as i32)
        .fetch_optional(&mut *conn)
        .await?
    } else {
        sqlx::query_as(
            "select block_id,left(content,$4::int),octet_length(content)::bigint from blocks
                 where conversation_id=$1 and kind='text' and parent_block_id is null
                   and btrim(content) <> ''
                   and (input_command_id = any($2)
                        or ($3::timestamptz is not null and created_at >= $3))
                 order by created_seq desc,created_at desc limit 1",
        )
        .bind(target_id)
        .bind(&run_ids)
        .bind(armed_at)
        .bind(ANSWER_LIMIT as i32)
        .fetch_optional(&mut *conn)
        .await?
    };
    let detail = detail(conn, target_id, state).await?;
    let worktree = worktree.and_then(|value| {
        if matches!(
            value.get("state").and_then(Value::as_str),
            Some("removing" | "removed")
        ) {
            return None;
        }
        let path = value.get("path")?.as_str()?;
        let branch = value.get("branch")?.as_str()?;
        Some((one_line(path, PATH_FIELD), one_line(branch, SHORT_FIELD)))
    });
    Ok(Some(SessionSummary {
        id: target_id,
        title: one_line(&title, SHORT_FIELD),
        harness: one_line(&harness, SHORT_FIELD),
        model: one_line(&model, SHORT_FIELD),
        state: state.to_owned(),
        turns,
        duration_ms: if turns.is_some() {
            Some(runs.iter().filter_map(|run| run.1).sum())
        } else {
            None
        },
        working_ms: if turns.is_some() {
            Some(runs.iter().filter_map(|run| run.3).sum())
        } else {
            None
        },
        tokens,
        git,
        worktree,
        detail,
        answer: answer.map(|(block_id, head, total)| Answer {
            block_id,
            head,
            total_bytes: total.max(0) as usize,
        }),
    }))
}

/// Подробности состояния берутся из проекции, только если она всё ещё в этом состоянии.
async fn detail(
    conn: &mut PgConnection,
    target_id: Uuid,
    state: &str,
) -> Result<Detail, sqlx::Error> {
    let derived: Option<(String, Json<Evidence>)> = sqlx::query_as(
        "select activity_state,activity_evidence from conversation_derived where conversation_id=$1",
    )
    .bind(target_id)
    .fetch_optional(&mut *conn)
    .await
    .or_else(|error| match error {
        // Доказательство из старой версии может не разбираться: итог обходится без него.
        sqlx::Error::ColumnDecode { .. } => Ok(None),
        other => Err(other),
    })?;
    let evidence = derived
        .filter(|(current, _)| current == state)
        .map(|(_, Json(evidence))| evidence);
    Ok(match (state, evidence) {
        ("waiting_for_user", _) => Detail::Waiting {
            question: open_question(conn, target_id).await?,
        },
        (
            "stalled",
            Some(Evidence::Stalled {
                last_event_at,
                phase,
                process_alive,
                cpu_busy,
                ..
            }),
        ) => Detail::Stalled {
            phase: one_line(&phase, SHORT_FIELD),
            last_event_at,
            process_alive,
            cpu_busy,
        },
        ("failed", Some(Evidence::Failed { reason })) => Detail::Failed {
            reason: one_line(&reason, SHORT_FIELD),
        },
        (
            "quota_paused",
            Some(Evidence::QuotaPaused {
                provider,
                window,
                until,
                ..
            }),
        ) => Detail::QuotaPaused {
            provider: provider.map(|value| one_line(&value, SHORT_FIELD)),
            window: window.map(|value| one_line(&value, SHORT_FIELD)),
            until,
        },
        ("stopped", Some(Evidence::Stopped { outcome })) => Detail::Stopped {
            outcome: one_line(&outcome, SHORT_FIELD),
        },
        _ => Detail::None,
    })
}

/// Первое открытое обращение к владельцу, только в виде `extract_request_summary`.
async fn open_question(
    conn: &mut PgConnection,
    target_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let rows: Vec<(String, Value)> = sqlx::query_as(
        "select r.kind,r.request_payload from requests r join executions e on e.id=r.execution_id
         where r.conversation_id=$1 and r.state='open' and r.kind <> 'spawn_approval'
           and e.state not in ('stopped','failed','owner_lost')
         order by r.created_at asc limit 1",
    )
    .bind(target_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.first().and_then(|(kind, payload)| {
        let kind = kind.parse().unwrap_or(RequestKind::Question);
        extract_request_summary(kind, payload).map(|summary| one_line(&summary, SHORT_FIELD))
    }))
}

fn duration(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    let (hours, minutes, seconds) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if hours > 0 {
        format!("{hours}h{minutes:02}m{seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

fn yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

fn instant(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Строки списка, пока они помещаются в бюджет; остаток сводится в одну строку.
fn list(lines: impl ExactSizeIterator<Item = String>, budget: usize, noun: &str) -> String {
    let total = lines.len();
    let (mut out, mut shown) = (String::new(), 0);
    for line in lines {
        if out.len() + line.len() + 1 > budget {
            break;
        }
        out.push_str(&line);
        out.push('\n');
        shown += 1;
    }
    if shown < total {
        let _ = writeln!(out, "... and {} more {noun}", total - shown);
    }
    out
}

fn git_section(git: Option<&Git>, worktree: Option<&(String, String)>) -> String {
    let Some(git) = git else {
        return if worktree.is_some() {
            "Git: no finished turn yet.\n".to_owned()
        } else {
            "Git: unavailable\n".to_owned()
        };
    };
    let mut out = String::new();
    let after = short(&git.head_after);
    let moved = !git.head_before.is_empty() && git.head_before != git.head_after;
    let head = if moved {
        format!("HEAD {} -> {after}", short(&git.head_before))
    } else {
        format!("HEAD {after}")
    };
    let count = git.commits.len();
    if count > 0 {
        let noun = if count == 1 { "commit" } else { "commits" };
        let _ = writeln!(out, "{head}, {count} {noun}:");
        out.push_str(&list(
            git.commits.iter().map(|commit| {
                format!(
                    "- {} {}",
                    short(&commit.sha),
                    one_line(&commit.subject, SHORT_FIELD + 56)
                )
            }),
            COMMITS_BUDGET,
            "commits",
        ));
    } else if moved {
        let _ = writeln!(out, "{head}.");
    } else {
        let _ = writeln!(out, "{head}, no new commits.");
    }
    if git.files.is_empty() {
        out.push_str("Changed files: none\n");
    } else {
        let _ = writeln!(out, "Changed files ({}):", git.files.len());
        out.push_str(&list(
            git.files.iter().map(|file| {
                format!(
                    "{} {}",
                    one_line(&file.status, 4),
                    one_line(&file.path, PATH_FIELD)
                )
            }),
            FILES_BUDGET,
            "files",
        ));
    }
    if git.truncated {
        out.push_str("Git facts were truncated.\n");
    }
    let _ = writeln!(
        out,
        "Uncommitted changes: {}",
        if git.dirty { "yes" } else { "no" }
    );
    out
}

fn detail_section(detail: &Detail) -> String {
    match detail {
        Detail::None => String::new(),
        Detail::Waiting { question } => {
            let mut out = String::new();
            if let Some(question) = question {
                let _ = writeln!(out, "Question: {question}");
            }
            out.push_str("The owner answers in the app; do not answer for them.\n");
            out
        }
        Detail::Stalled {
            phase,
            last_event_at,
            process_alive,
            cpu_busy,
        } => format!(
            "Phase: {phase}. Last event: {}.\nProcess alive: {}. CPU busy: {}.\n",
            instant(*last_event_at),
            yes_no(*process_alive),
            yes_no(*cpu_busy)
        ),
        Detail::Failed { reason } if reason == OWNER_LOST => format!(
            "Reason: {reason}. The worker lost this session but may get it back; you stay subscribed.\n"
        ),
        Detail::Failed { reason } => format!("Reason: {reason}.\n"),
        Detail::QuotaPaused {
            provider,
            window,
            until,
        } => format!(
            "Provider: {}. Window: {}. Until: {}.\n",
            provider.as_deref().unwrap_or("unknown"),
            window.as_deref().unwrap_or("unknown"),
            until.map_or_else(|| "unknown".to_owned(), instant)
        ),
        Detail::Stopped { outcome } => format!("Outcome: {outcome}.\n"),
    }
}

fn footer(state: &str, detail: &Detail) -> Option<&'static str> {
    if matches!(detail, Detail::Failed { reason } if reason == OWNER_LOST) {
        return Some(
            "Next: wait for the next [canterlot] message, or session_accept to give up on it.",
        );
    }
    match state {
        "idle" => Some(
            "Next: read the result, then call session_accept to clean up, or session_send to continue.",
        ),
        "stalled" => Some("Next: session_inspect to look closer, or session_send to nudge it."),
        "failed" => Some(
            "Next: session_inspect for details, or session_accept to clean up. It runs again only when the owner resumes it: session_send just queues a message until then.",
        ),
        "stopped" => Some(
            "Next: session_accept to clean up. It runs again only when the owner resumes it: session_send just queues a message until then.",
        ),
        _ => None,
    }
}

pub fn render(summary: &SessionSummary) -> String {
    render_bounded(summary, TEXT_LIMIT)
}

/// Тот же итог, но не длиннее `limit`: пачка делит общий предел поровну между целями.
pub fn render_bounded(summary: &SessionSummary, limit: usize) -> String {
    let mut head = format!(
        "[canterlot] Session \"{}\" (id {}, {}, model {}) is now {}.\n",
        summary.title, summary.id, summary.harness, summary.model, summary.state
    );
    head.push_str(&detail_section(&summary.detail));
    let turns = summary
        .turns
        .map_or_else(|| "unknown".to_owned(), |turns| turns.to_string());
    let duration = summary
        .duration_ms
        .map_or_else(|| "unknown".to_owned(), duration);
    let tokens = summary.tokens.map_or_else(
        || "unknown".to_owned(),
        |(input, output)| format!("{input}/{output}"),
    );
    let _ = writeln!(
        head,
        "Turns: {turns}, duration {duration}, tokens in/out {tokens}."
    );
    head.push_str(&git_section(
        summary.git.as_ref(),
        summary.worktree.as_ref(),
    ));
    if let Some((path, branch)) = &summary.worktree {
        let _ = writeln!(head, "Worktree: {path}, branch {branch}.");
    }
    let tail = footer(&summary.state, &summary.detail)
        .map_or_else(String::new, |text| format!("\n{text}"));
    let label = match summary.state.as_str() {
        "idle" | "failed" | "stopped" => "Final answer",
        _ => "Last message",
    };
    let mut out = head;
    match &summary.answer {
        None if label == "Final answer" => out.push_str("Final answer: none.\n"),
        None => {}
        Some(answer) => {
            // Заголовок ответа и разделитель перед подвалом входят в общий предел.
            let reserved = out.len() + tail.len() + label.len() + 160;
            let budget = ANSWER_LIMIT.min(limit.saturating_sub(reserved));
            let kept = truncate_at_boundary(&answer.head, budget);
            if answer.total_bytes > kept.len() {
                let _ = writeln!(
                    out,
                    "{label} (truncated to {kept_bytes} bytes of {}; session_inspect with block_id {} and offset {kept_bytes} reads on):",
                    answer.total_bytes,
                    answer.block_id,
                    kept_bytes = kept.len(),
                );
                out.push_str(kept);
            } else {
                let _ = writeln!(out, "{label}:");
                out.push_str(kept.trim_end());
            }
            out.push('\n');
        }
    }
    out.push_str(&tail);
    let end = truncate_at_boundary(out.trim_end(), limit).len();
    out.truncate(end);
    out
}

impl SessionSummary {
    /// Факты для ленты владельца: текст события остаётся прежним.
    pub fn hub_event(&self) -> HubEvent {
        let kind = match self.state.as_str() {
            "idle" => "session_result",
            "waiting_for_user" => "asked",
            "stalled" => "stalled",
            "failed" => "failed",
            "quota_paused" => "quota_paused",
            "stopped" => "stopped",
            _ => "session_result",
        };
        let mut event = HubEvent {
            kind: kind.to_owned(),
            state: Some(self.state.clone()),
            session_id: Some(self.id),
            turns: self.turns.map(|turns| turns as i64),
            duration_ms: self.duration_ms,
            working_ms: self.working_ms,
            tokens_in: self.tokens.map(|(input, _)| input),
            tokens_out: self.tokens.map(|(_, output)| output),
            commits: self.git.as_ref().map(|git| {
                git.commits
                    .iter()
                    .map(|commit| HubCommit {
                        sha: short(&commit.sha),
                        title: one_line(&commit.subject, SHORT_FIELD),
                    })
                    .collect()
            }),
            changed_files: self.git.as_ref().map(|git| git.files.len() as i64),
            ..HubEvent::default()
        };
        if let Some(answer) = &self.answer {
            event.answer = Some(answer.head.clone());
            event.answer_bytes = Some(answer.total_bytes as i64);
            event.answer_block_id = Some(answer.block_id);
        }
        match &self.detail {
            Detail::Waiting { question } => event.question.clone_from(question),
            Detail::Stalled {
                phase,
                last_event_at,
                process_alive,
                cpu_busy,
            } => {
                event.phase = Some(phase.clone());
                event.last_event_at = Some(*last_event_at);
                event.process_alive = *process_alive;
                event.cpu_busy = *cpu_busy;
            }
            Detail::Failed { reason } => event.reason = Some(reason.clone()),
            Detail::QuotaPaused {
                provider,
                window,
                until,
            } => {
                event.provider.clone_from(provider);
                event.window.clone_from(window);
                event.until = *until;
            }
            Detail::Stopped { outcome } => event.outcome = Some(outcome.clone()),
            Detail::None => {}
        }
        event
    }
}

/// Текст события для подписчика; `None`, если цели уже нет.
pub async fn event_text(
    conn: &mut PgConnection,
    owner_id: Uuid,
    target_id: Uuid,
    state: &str,
    armed_at: Option<DateTime<Utc>>,
) -> Result<Option<String>, sqlx::Error> {
    Ok(build(conn, owner_id, target_id, state, armed_at)
        .await?
        .map(|summary| render(&summary)))
}
