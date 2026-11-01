//! Итог делегированной сессии для подписчика: текст события о том, что цель осела.
//!
//! Окно итога начинается с момента взвода подписки. Ответы на секретные вопросы в текст
//! не попадают: о вопросе говорит только `extract_request_summary`.

use std::fmt::{self, Write as _};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::Value;
use sqlx::{PgConnection, types::Json};
use uuid::Uuid;

use crate::{
    activity::{Evidence, extract_request_summary},
    requests::RequestKind,
};

/// Предел итогового ответа в тексте события.
pub const ANSWER_LIMIT: usize = 8192;
/// Предел всего текста события.
pub const TEXT_LIMIT: usize = 16 * 1024;
const COMMITS_BUDGET: usize = 3 * 1024;
const FILES_BUDGET: usize = 2 * 1024;
const SHORT_FIELD: usize = 200;
const PATH_FIELD: usize = 1024;

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
    turns: usize,
    duration_ms: i64,
    tokens_in: i64,
    tokens_out: i64,
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

/// Собирает итог. `None`, если цели нет у этого владельца.
pub async fn build(
    conn: &mut PgConnection,
    owner_id: Uuid,
    target_id: Uuid,
    state: &str,
    armed_at: Option<DateTime<Utc>>,
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
    let runs: Vec<(Uuid, Option<i64>, Option<Value>)> = sqlx::query_as(
        "select input_command_id,duration_ms,git from runs
         where conversation_id=$1 and finished_at is not null
           and ($2::timestamptz is null or finished_at >= $2)
         order by finished_at,started_at,input_command_id",
    )
    .bind(target_id)
    .bind(armed_at)
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
    let git: Vec<Option<Value>> = runs.iter().map(|run| run.2.clone()).collect();
    let answer: Option<(Uuid, String, i64)> = sqlx::query_as(
        "select block_id,left(content,$4::int),octet_length(content)::bigint from blocks
         where conversation_id=$1 and kind='text' and parent_block_id is null
           and btrim(content) <> ''
           and (input_command_id = any($2) or ($3::timestamptz is not null and created_at >= $3))
         order by created_seq desc,created_at desc limit 1",
    )
    .bind(target_id)
    .bind(&run_ids)
    .bind(armed_at)
    .bind(ANSWER_LIMIT as i32)
    .fetch_optional(&mut *conn)
    .await?;
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
        turns: runs.len(),
        duration_ms: runs.iter().filter_map(|run| run.1).sum(),
        tokens_in,
        tokens_out,
        git: aggregate_git(&git),
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

fn git_section(git: Option<&Git>) -> String {
    let Some(git) = git else {
        return "Git: unavailable\n".to_owned();
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

fn footer(state: &str) -> Option<&'static str> {
    match state {
        "idle" => Some(
            "Next: read the result, then call session_accept to clean up, or session_send to continue.",
        ),
        "stalled" => Some("Next: session_inspect to look closer, or session_send to nudge it."),
        "failed" => Some(
            "Next: session_inspect for details, session_send to retry, or session_accept to clean up.",
        ),
        "stopped" => Some("Next: session_send to continue, or session_accept to clean up."),
        _ => None,
    }
}

pub fn render(summary: &SessionSummary) -> String {
    let mut head = format!(
        "[canterlot] Session \"{}\" (id {}, {}, model {}) is now {}.\n",
        summary.title, summary.id, summary.harness, summary.model, summary.state
    );
    head.push_str(&detail_section(&summary.detail));
    let _ = writeln!(
        head,
        "Turns: {}, duration {}, tokens in/out {}/{}.",
        summary.turns,
        duration(summary.duration_ms),
        summary.tokens_in,
        summary.tokens_out
    );
    head.push_str(&git_section(summary.git.as_ref()));
    if let Some((path, branch)) = &summary.worktree {
        let _ = writeln!(head, "Worktree: {path}, branch {branch}.");
    }
    let tail = footer(&summary.state).map_or_else(String::new, |text| format!("\n{text}"));
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
            let budget = ANSWER_LIMIT.min(TEXT_LIMIT.saturating_sub(reserved));
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
    out.truncate(out.trim_end().len());
    out
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
