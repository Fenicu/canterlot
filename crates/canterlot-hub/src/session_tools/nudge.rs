//! Автопинок: хаб сам толкает исполнителя, бросившего ход без маркера готовности.
//!
//! Правило живёт на исполнителя (`nudge_rules`), решение «устоявшийся `idle` — это
//! завершение?» принимает общий модуль `completion` (см. `completion.rs`). Удержанный
//! итог отпускается подписчикам, когда правило исчерпано, выключено или толчок не удался.
//!
//! Порядок блокировок (docs/design/delivery.md): строки `conversations` по возрастанию ID →
//! очередь цели (`queue_state`) → правило (`nudge_rules`) → подписки → задания. Проектор
//! цели вызывает `gate_in_tx` уже под очередью цели; он не берёт строки разговоров.

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{
    Caller, ToolContext, ToolError, ToolErrorCode, access, origin, parse_arguments, send,
    settle::{SettleConfig, Settled},
    subs, summary,
};
use crate::{
    crunch::JobOutcome,
    delivery::{ContentBlock, SubmitError, SubmitInput, submit_input_in_tx},
    events::{self, NewEvent, Scope},
    scheduler::ClaimedJob,
};

/// Хвост ответа, по которому ищутся маркер и вопрос: последние 1 КиБ.
pub const ANSWER_TAIL_BYTES: usize = 1024;
const MAX_MARKERS: usize = 5;
const MAX_MARKER_CHARS: usize = 64;
/// Фраза «я продолжу» в хвосте ответа: доказательство и текст толчка, не запрет.
const CONTINUE_PHRASES: &[&str] = &[
    "i'll continue",
    "i will continue",
    "let me continue",
    "next, i will",
    "i will now",
    "now i'll",
    "waiting for",
    "once it finishes",
    "will report back",
    "i'll proceed",
    "продолжу",
    "дождусь",
    "сейчас запущу",
];
/// Признаки фоновой команды в вызове инструмента последнего хода.
const BACKGROUND_PHRASES: &[&str] = &[
    "nohup",
    "setsid",
    "disown",
    "run_in_background",
    "tmux new -d",
    "screen -dm",
];
const QUESTION_PREFIXES: &[&str] = &["should i", "do you want", "shall i", "хотите", "нужно ли"];

/// Действующие значения правила: ответ `nudge_set` и снимок разговора.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct NudgeRuleView {
    pub session_id: Uuid,
    pub enabled: bool,
    pub state: String,
    pub idle_minutes: i32,
    pub background_minutes: i32,
    pub max_consecutive: i32,
    pub max_total: i32,
    pub done_markers: Vec<String>,
    pub context_warn_percent: Option<i32>,
    pub set_by: String,
    pub set_by_session: Option<Uuid>,
    pub notify_session_id: Option<Uuid>,
    pub consecutive: i32,
    pub total: i32,
    pub held: bool,
}

/// Событие автопинка для `session_inspect` и PWA.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct NudgeEventView {
    pub id: Uuid,
    pub at: String,
    pub attempt: i32,
    pub outcome: String,
    pub signals: Value,
}

#[derive(Clone, sqlx::FromRow)]
struct RuleRow {
    conversation_id: Uuid,
    owner_id: Uuid,
    set_by: String,
    set_by_conversation: Option<Uuid>,
    notify_conversation_id: Option<Uuid>,
    enabled: bool,
    idle_minutes: i32,
    background_minutes: i32,
    max_consecutive: i32,
    max_total: i32,
    done_markers: Vec<String>,
    #[allow(dead_code)]
    text_preset_id: Option<String>,
    context_warn_percent: Option<i32>,
    #[allow(dead_code)]
    report_file: Option<String>,
    #[allow(dead_code)]
    classifier: Option<Value>,
    state: String,
    held_fingerprint: Option<String>,
    consecutive: i32,
    total: i32,
    progress_baseline: Option<Value>,
    context_warned_at: Option<DateTime<Utc>>,
    #[allow(dead_code)]
    created_at: DateTime<Utc>,
    #[allow(dead_code)]
    updated_at: DateTime<Utc>,
}

impl RuleRow {
    fn view(&self) -> NudgeRuleView {
        NudgeRuleView {
            session_id: self.conversation_id,
            enabled: self.enabled,
            state: self.state.clone(),
            idle_minutes: self.idle_minutes,
            background_minutes: self.background_minutes,
            max_consecutive: self.max_consecutive,
            max_total: self.max_total,
            done_markers: self.done_markers.clone(),
            context_warn_percent: self.context_warn_percent,
            set_by: self.set_by.clone(),
            set_by_session: self.set_by_conversation,
            notify_session_id: self.notify_conversation_id,
            consecutive: self.consecutive,
            total: self.total,
            held: self.held_fingerprint.is_some(),
        }
    }
}

/// Сигналы §4.1. В журнал и метрики не попадают: это данные уровня сообщения.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Signals {
    pub no_marker: bool,
    pub question_in_text: bool,
    pub continue_phrase: bool,
    pub background_suspected: bool,
    pub uncommitted: bool,
    pub short_turn: bool,
    pub context_high: bool,
    pub idle_minutes: i64,
    pub last_turn_seconds: i64,
    pub tools: i64,
}

impl Signals {
    /// Короткие английские фразы для текста толчка и события: `{signals}`.
    pub fn phrases(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.idle_minutes > 0 {
            out.push(format!("idle {} min", self.idle_minutes));
        }
        if self.no_marker {
            out.push("no done marker".to_owned());
        }
        if self.uncommitted {
            out.push("uncommitted changes".to_owned());
        }
        if self.continue_phrase {
            out.push("you said you would continue".to_owned());
        }
        if self.background_suspected {
            out.push("background command may still be running".to_owned());
        }
        if self.short_turn {
            out.push(format!(
                "last turn {} s without tools",
                self.last_turn_seconds.max(0)
            ));
        }
        if self.context_high {
            out.push("context almost full".to_owned());
        }
        out
    }

    pub fn text(&self) -> String {
        self.phrases().join("; ")
    }
}

// ---------------------------------------------------------------------------
// Чистые правила сигналов
// ---------------------------------------------------------------------------

/// Хвост ответа по границе символа: правило смотрит только на конец.
fn tail(text: &str) -> &str {
    if text.len() <= ANSWER_TAIL_BYTES {
        return text;
    }
    let mut start = text.len() - ANSWER_TAIL_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Содержимое блоков кода не считается: там маркер может быть примером, а не итогом.
fn without_code_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Маркер готовности: строка целиком, без учёта регистра и пробелов по краям.
pub fn marker_seen(text: &str, markers: &[String]) -> bool {
    // Состояние ограды считается по полному ответу: хвост может начинаться внутри
    // длинного блока кода, и тогда закрывающая ограда перевернула бы признак.
    let full = without_code_blocks(text);
    let text = tail(&full);
    let markers: Vec<String> = markers
        .iter()
        .map(|marker| marker.trim().to_lowercase())
        .filter(|marker| !marker.is_empty())
        .collect();
    text.lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .any(|line| {
            let line = line.trim().to_lowercase();
            markers.contains(&line)
        })
}

/// Последний абзац — вопрос. Это не толчок: итог уходит оркестратору сразу.
pub fn asks_in_text(text: &str) -> bool {
    let full = without_code_blocks(text);
    let text = tail(&full);
    let last = text
        .trim_end()
        .rsplit("\n\n")
        .next()
        .unwrap_or_default()
        .trim();
    if last.is_empty() {
        return false;
    }
    if last.ends_with('?') || last.ends_with('？') {
        return true;
    }
    let lower = last.to_lowercase();
    QUESTION_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

pub fn continue_phrase(text: &str) -> bool {
    let lower = tail(text).to_lowercase();
    CONTINUE_PHRASES.iter().any(|phrase| lower.contains(phrase))
}

/// `&` в конце команды, `nohup`, `setsid`, `disown`, `run_in_background`, `tmux new -d`,
/// `screen -dm` в вызове инструмента последнего хода.
pub fn background_in_commands(commands: &[String]) -> bool {
    commands.iter().any(|command| {
        let lower = command.to_lowercase();
        if BACKGROUND_PHRASES
            .iter()
            .any(|phrase| lower.contains(phrase))
        {
            return true;
        }
        let trimmed = lower.trim_end().trim_end_matches(['\'', '"']).trim_end();
        trimmed.ends_with('&') && !trimmed.ends_with("&&")
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    crate::attachments::blobstore::hex(&Sha256::digest(bytes))
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct GitFacts {
    available: bool,
    head_after: String,
    dirty: bool,
    files: Vec<GitFile>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct GitFile {
    path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Baseline {
    head: String,
    dirty: bool,
    files: String,
    report_size: i64,
    run_id: Option<Uuid>,
    tools: i64,
    working_ms: i64,
}

/// Последний ход: идентификатор, длительность, git, рабочее время, расход контекста.
type AnswerRunFacts = (
    Option<Uuid>,
    Option<i64>,
    Option<Value>,
    Option<i64>,
    Option<i64>,
);

#[derive(Clone, Debug)]
struct Facts {
    signals: Signals,
    baseline: Baseline,
    context_tokens: Option<i64>,
    context_window: Option<i64>,
    context_percent: Option<i64>,
}

/// Собирает факты последнего законченного хода: ответ, git, инструменты, метрики.
async fn facts_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    rule: &RuleRow,
    now: DateTime<Utc>,
) -> Result<Facts, sqlx::Error> {
    let answer: Option<String> = sqlx::query_scalar(
        "select content from blocks
         where conversation_id=$1 and kind='text' and parent_block_id is null
           and agent='main' and btrim(content) <> ''
         order by created_seq desc nulls last,created_at desc limit 1",
    )
    .bind(target_id)
    .fetch_optional(&mut *conn)
    .await?;
    let answer = answer.unwrap_or_default();
    let row: Option<AnswerRunFacts> = sqlx::query_as(
            "select r.input_command_id,coalesce(r.duration_ms,
                       (extract(epoch from (coalesce(r.finished_at,now()) - r.started_at)) * 1000)::bigint),
                    r.git,coalesce(w.working_ms,r.duration_ms),r.context_tokens
             from runs r left join run_working w on w.input_command_id=r.input_command_id
             where r.conversation_id=$1
             order by coalesce(r.finished_at,r.started_at) desc,r.started_at desc limit 1",
        )
        .bind(target_id)
        .fetch_optional(&mut *conn)
        .await?;
    let (run_id, duration_ms, git, working_ms, context_tokens) = row.unwrap_or_default();
    let window: Option<i64> = sqlx::query_scalar(
        "select context_window_tokens from runs where conversation_id=$1
         order by coalesce(finished_at,started_at) desc,started_at desc limit 1",
    )
    .bind(target_id)
    .fetch_optional(&mut *conn)
    .await?
    .flatten();
    let git: GitFacts = git
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let tools: i64 = sqlx::query_scalar(
        "select count(*)::bigint from blocks
         where conversation_id=$1 and kind='tool_call'
           and ($2::uuid is null or input_command_id=$2)",
    )
    .bind(target_id)
    .bind(run_id)
    .fetch_one(&mut *conn)
    .await?;
    let commands: Vec<String> = sqlx::query_scalar(
        "select content from blocks
         where conversation_id=$1 and kind='tool_call'
           and ($2::uuid is null or input_command_id=$2)",
    )
    .bind(target_id)
    .bind(run_id)
    .fetch_all(&mut *conn)
    .await?;
    let duration_ms = duration_ms.unwrap_or(0);
    let working_ms = working_ms.unwrap_or(0);
    let context_percent = match (context_tokens, window) {
        (Some(used), Some(window)) if window > 0 => Some(used.saturating_mul(100) / window),
        _ => None,
    };
    let idle_minutes: i64 = sqlx::query_scalar(
        "select greatest(0,floor(extract(epoch from ($2::timestamptz - activity_since))/60))::bigint
         from conversation_derived where conversation_id=$1",
    )
    .bind(target_id)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or(0);
    let mut files: Vec<String> = git.files.iter().map(|file| file.path.clone()).collect();
    files.sort();
    let no_marker = !marker_seen(&answer, &rule.done_markers);
    let question_in_text = asks_in_text(&answer);
    let mut background_suspected = background_in_commands(&commands);
    if !background_suspected
        && let Some(finished) = query_last_turn_at(conn, target_id).await?
        && machine_busy_after(conn, run_id, finished).await?
    {
        background_suspected = true;
    }
    let short_turn = tools == 0 && duration_ms < 60_000 && duration_ms > 0;
    let context_high = match (context_percent, rule.context_warn_percent) {
        (Some(percent), Some(threshold)) => percent >= i64::from(threshold),
        _ => false,
    };
    let signals = Signals {
        no_marker,
        question_in_text,
        continue_phrase: continue_phrase(&answer),
        background_suspected,
        uncommitted: git.available && git.dirty,
        short_turn,
        context_high,
        idle_minutes,
        last_turn_seconds: duration_ms / 1000,
        tools,
    };
    let baseline = Baseline {
        head: git.head_after.clone(),
        dirty: git.dirty,
        files: hex_digest(files.join("\n").as_bytes()),
        report_size: 0,
        run_id,
        tools,
        working_ms,
    };
    Ok(Facts {
        signals,
        baseline,
        context_tokens,
        context_window: window,
        context_percent,
    })
}

async fn query_last_turn_at(
    conn: &mut PgConnection,
    target_id: Uuid,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar("select max(finished_at) from runs where conversation_id=$1")
        .bind(target_id)
        .fetch_one(&mut *conn)
        .await
}

/// Две свежие метрики подряд после конца хода: процесс исполнения всё ещё занят.
async fn machine_busy_after(
    conn: &mut PgConnection,
    run_id: Option<Uuid>,
    after: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let Some(run_id) = run_id else {
        return Ok(false);
    };
    let busy: Option<bool> = sqlx::query_scalar(
        "select coalesce(e.cpu_busy_streak>=2 and e.cpu_busy_first_at>$2,false)
         from runs r join executions e on e.id=r.execution_id
         where r.input_command_id=$1",
    )
    .bind(run_id)
    .bind(after)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(busy.unwrap_or(false))
}

fn still_progress(baseline: Option<&Value>, current: &Baseline) -> bool {
    let Some(baseline) =
        baseline.and_then(|value| serde_json::from_value::<Baseline>(value.clone()).ok())
    else {
        return true;
    };
    baseline.head != current.head
        || baseline.dirty != current.dirty
        || baseline.files != current.files
        || current.report_size > baseline.report_size
        || (baseline.run_id != current.run_id && current.tools > 0 && current.working_ms >= 60_000)
}

// ---------------------------------------------------------------------------
// Чтение и запись правила
// ---------------------------------------------------------------------------

async fn rule_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
) -> Result<Option<RuleRow>, sqlx::Error> {
    sqlx::query_as("select * from nudge_rules where conversation_id=$1")
        .bind(target_id)
        .fetch_optional(&mut *conn)
        .await
}

async fn rule_locked_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
) -> Result<Option<RuleRow>, sqlx::Error> {
    sqlx::query_as("select * from nudge_rules where conversation_id=$1 for update")
        .bind(target_id)
        .fetch_optional(&mut *conn)
        .await
}

async fn record_event_in_tx(
    conn: &mut PgConnection,
    rule: &RuleRow,
    attempt: i32,
    signals: &Signals,
    outcome: &str,
    input_id: Option<Uuid>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into nudge_events(id,conversation_id,owner_id,at,attempt,signals,outcome,input_id)
         values($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(Uuid::now_v7())
    .bind(rule.conversation_id)
    .bind(rule.owner_id)
    .bind(now)
    .bind(attempt)
    .bind(serde_json::to_value(signals).unwrap_or(Value::Null))
    .bind(outcome)
    .bind(input_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Событие для PWA; текст остаётся в правиле и истории толчков.
async fn changed_in_tx(
    conn: &mut PgConnection,
    rule: &RuleRow,
    correlation_id: Uuid,
    detail: Option<&str>,
) -> Result<(), sqlx::Error> {
    let mut payload = serde_json::to_value(rule.view()).unwrap_or(Value::Null);
    if let Some(detail) = detail
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("detail".to_owned(), json!(detail));
    }
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: rule.conversation_id,
            },
            kind: "nudge_changed",
            conversation_id: Some(rule.conversation_id),
            execution_id: None,
            payload,
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

/// Сброс серии и выход из `exhausted` на любом вводе не от автопинка.
pub async fn on_input_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let changed: Option<RuleRow> = sqlx::query_as(
        "update nudge_rules set consecutive=0,held_fingerprint=null,state='watching',
             updated_at=$2
         where conversation_id=$1 and state in ('watching','pending','exhausted','paused')
         returning *",
    )
    .bind(target_id)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(rule) = changed {
        // Отпечаток изменился: ожидавший толчок уже не про этот простой.
        cancel_pending_jobs_in_tx(conn, target_id, None, "input").await?;
        changed_in_tx(conn, &rule, Uuid::now_v7(), Some("input")).await?;
    }
    Ok(())
}

/// Ответ кончается вопросом в тексте: итог уходит сразу с пометкой «asks in text».
pub async fn asks_in_text_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let answer: Option<String> = sqlx::query_scalar(
        "select content from blocks
         where conversation_id=$1 and kind='text' and parent_block_id is null
           and agent='main' and btrim(content) <> ''
         order by created_seq desc nulls last,created_at desc limit 1",
    )
    .bind(target_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(answer.is_some_and(|text| asks_in_text(&text)))
}

// ---------------------------------------------------------------------------
// Шлюз завершения
// ---------------------------------------------------------------------------

/// Итог шлюза: `true` — итог удержан, подписчикам и будильникам его пока не отдают.
pub async fn gate_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    state: Settled,
    fingerprint: &str,
    now: DateTime<Utc>,
    enabled: bool,
) -> Result<bool, sqlx::Error> {
    let Some(mut rule) = rule_in_tx(conn, target_id).await? else {
        return Ok(false);
    };
    if !enabled || !rule.enabled {
        // Выключатель: удержание снимается здесь же, до обычной доставки. Иначе
        // уборка на старте увидела бы уже закрытую подписку и добавила заметку-дубль.
        if rule.held_fingerprint.is_some() || rule.state == "pending" {
            let row: RuleRow = sqlx::query_as(
                "update nudge_rules set held_fingerprint=null,
                     state=case when state='pending' then 'watching' else state end,
                     updated_at=$2
                 where conversation_id=$1 returning *",
            )
            .bind(target_id)
            .bind(now)
            .fetch_one(&mut *conn)
            .await?;
            changed_in_tx(conn, &row, Uuid::now_v7(), Some("disabled")).await?;
        }
        return Ok(false);
    }
    if state == Settled::Stopped {
        // Остановка — решение владельца: правило ждёт возобновления, а удержанный
        // `idle` заменяется одним событием `stopped`, которое уходит обычной подпиской.
        if rule.state != "paused" || rule.held_fingerprint.is_some() {
            let row: RuleRow = sqlx::query_as(
                "update nudge_rules set state='paused',held_fingerprint=null,updated_at=$2
                 where conversation_id=$1 returning *",
            )
            .bind(target_id)
            .bind(now)
            .fetch_one(&mut *conn)
            .await?;
            cancel_pending_jobs_in_tx(conn, target_id, None, "stopped").await?;
            changed_in_tx(conn, &row, Uuid::now_v7(), Some("paused")).await?;
            rule = row;
        }
        let _ = rule;
        return Ok(false);
    }
    if state != Settled::Idle || !matches!(rule.state.as_str(), "watching" | "pending") {
        return Ok(false);
    }
    let facts = facts_in_tx(conn, target_id, &rule, now).await?;
    if facts.signals.context_high {
        warn_context_in_tx(conn, &mut rule, &facts, now).await?;
    }
    if !facts.signals.no_marker || facts.signals.question_in_text {
        // Маркер или вопрос в тексте — нормальное завершение. Счётчик серии сбрасывается
        // только если он был: проектор повторяет оценку каждые 15 секунд.
        if rule.state == "watching" && rule.consecutive == 0 && rule.held_fingerprint.is_none() {
            return Ok(false);
        }
        if rule.consecutive > 0 || rule.total > 0 || rule.held_fingerprint.is_some() {
            let outcome = if facts.signals.question_in_text {
                "asked_in_text"
            } else {
                "marker_seen"
            };
            record_event_in_tx(conn, &rule, 0, &facts.signals, outcome, None, now).await?;
        }
        let row: RuleRow = sqlx::query_as(
            "update nudge_rules set state='watching',held_fingerprint=null,consecutive=0,
                 updated_at=$2
             where conversation_id=$1 returning *",
        )
        .bind(target_id)
        .bind(now)
        .fetch_one(&mut *conn)
        .await?;
        changed_in_tx(
            conn,
            &row,
            Uuid::now_v7(),
            Some(outcome_name(&facts.signals)),
        )
        .await?;
        return Ok(false);
    }
    if rule.state == "pending" && rule.held_fingerprint.as_deref() == Some(fingerprint) {
        return Ok(true);
    }
    let delay = if facts.signals.background_suspected {
        rule.idle_minutes.max(rule.background_minutes)
    } else {
        rule.idle_minutes
    };
    let run_at = now + TimeDelta::minutes(i64::from(delay));
    let row: RuleRow = sqlx::query_as(
        "update nudge_rules set state='pending',held_fingerprint=$2,updated_at=$3
         where conversation_id=$1 returning *",
    )
    .bind(target_id)
    .bind(fingerprint)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    subs::insert_job(
        conn,
        row.owner_id,
        target_id,
        run_at,
        &json!({
            "action": "nudge",
            "rule_id": target_id,
            "fingerprint": fingerprint,
            "signals": facts.signals,
        }),
    )
    .await?;
    changed_in_tx(conn, &row, Uuid::now_v7(), Some("held")).await?;
    Ok(true)
}

fn outcome_name(signals: &Signals) -> &'static str {
    if signals.question_in_text {
        "asked_in_text"
    } else {
        "marker_seen"
    }
}

/// Одно предупреждение о заполнении контекста оркестратору, не исполнителю.
async fn warn_context_in_tx(
    conn: &mut PgConnection,
    rule: &mut RuleRow,
    facts: &Facts,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    if rule.context_warned_at.is_some() {
        return Ok(());
    }
    let Some(percent) = facts.context_percent else {
        return Ok(());
    };
    let Some(notify) = rule.notify_conversation_id.or(rule.set_by_conversation) else {
        return Ok(());
    };
    let (used, window) = (
        facts.context_tokens.unwrap_or(0),
        facts.context_window.unwrap_or(0),
    );
    let text = format!(
        "[canterlot] Session {} context is {percent}% full ({used} of {window} tokens); \
         ask it to commit and summarize, or plan a fresh session.",
        rule.conversation_id
    );
    let origin = json!({"kind":"event","chain_depth":0,"event":{
        "kind":"context_high","session_id":rule.conversation_id,
        "detail":format!("{percent}%")
    }});
    subs::insert_job(
        conn,
        rule.owner_id,
        notify,
        now,
        &json!({"action":"note","text":text,"origin":origin}),
    )
    .await?;
    let row: RuleRow = sqlx::query_as(
        "update nudge_rules set context_warned_at=$2,updated_at=$2
         where conversation_id=$1 returning *",
    )
    .bind(rule.conversation_id)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    *rule = row;
    Ok(())
}

/// Только чтение для `sessions.wait` и явной подписки: правило не создаёт заданий.
pub async fn would_hold_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    now: DateTime<Utc>,
    enabled: bool,
) -> Result<bool, sqlx::Error> {
    if !enabled {
        return Ok(false);
    }
    let Some(rule) = rule_in_tx(conn, target_id).await? else {
        return Ok(false);
    };
    if !rule.enabled || !matches!(rule.state.as_str(), "watching" | "pending") {
        return Ok(false);
    }
    let facts = facts_in_tx(conn, target_id, &rule, now).await?;
    Ok(facts.signals.no_marker && !facts.signals.question_in_text)
}

// ---------------------------------------------------------------------------
// Задание `nudge`
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct NudgeJob {
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    signals: Signals,
}

/// Задание толчка: перепроверка под очередью исполнителя, прогресс, исчерпание,
/// доставка обычного ввода. Все записи — одна транзакция планировщика.
pub async fn run_job_in_tx(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    now: DateTime<Utc>,
    correlation_id: Uuid,
    settle_config: &SettleConfig,
    thresholds: &crate::activity::Thresholds,
) -> Result<JobOutcome, SubmitError> {
    let Some(target) = job.conversation_id else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    let Ok(payload) = serde_json::from_value::<NudgeJob>(job.payload.clone()) else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    let Some(head) = rule_in_tx(conn, target).await? else {
        return Ok(JobOutcome::Cancelled("rule_gone"));
    };
    // Строки разговоров — до очереди и правила: docs/design/delivery.md.
    let mut conversation_ids = vec![target];
    if let Some(author) = head.set_by_conversation {
        conversation_ids.push(author);
    }
    conversation_ids.sort_unstable();
    conversation_ids.dedup();
    let mut sessions = Vec::new();
    for id in &conversation_ids {
        if let Some(session) = access::session_in_tx(conn, head.owner_id, *id).await? {
            sessions.push(session);
        }
    }
    // Очереди автора и цели — по возрастанию ID: смену настроек хаб сериализует
    // очередью самой сессии, поэтому режимы перечитываются только после обеих.
    for id in &conversation_ids {
        send::lock_queue(conn, *id).await?;
    }
    // Режимы перечитываются после очередей: кэш, снятый до них, мог остаться прежним.
    sessions.clear();
    for id in &conversation_ids {
        if let Some(session) = access::session_in_tx(conn, head.owner_id, *id).await? {
            sessions.push(session);
        }
    }
    let Some(mut rule) = rule_locked_in_tx(conn, target).await? else {
        return Ok(JobOutcome::Cancelled("rule_gone"));
    };
    if !settle_config.nudge_enabled {
        disable_locked_in_tx(conn, &mut rule, Some(job.id), now, correlation_id).await?;
        return Ok(JobOutcome::Done(json!({"state":"disabled"})));
    }
    if !rule.enabled
        || rule.state != "pending"
        || rule.held_fingerprint.as_deref() != Some(&payload.fingerprint)
    {
        record_event_in_tx(
            conn,
            &rule,
            0,
            &payload.signals,
            "cancelled_state_changed",
            None,
            now,
        )
        .await?;
        cancel_hold_in_tx(conn, target, &payload.fingerprint, now).await?;
        return Ok(JobOutcome::Cancelled("state_changed"));
    }
    let current = subs::settled_live_in_tx(conn, target, now, settle_config, thresholds).await?;
    let Some(current) = current else {
        record_event_in_tx(
            conn,
            &rule,
            0,
            &payload.signals,
            "cancelled_state_changed",
            None,
            now,
        )
        .await?;
        cancel_hold_in_tx(conn, target, &payload.fingerprint, now).await?;
        return Ok(JobOutcome::Cancelled("state_changed"));
    };
    if current.state != Settled::Idle || current.fingerprint != payload.fingerprint {
        record_event_in_tx(
            conn,
            &rule,
            0,
            &payload.signals,
            "cancelled_state_changed",
            None,
            now,
        )
        .await?;
        cancel_hold_in_tx(conn, target, &payload.fingerprint, now).await?;
        return Ok(JobOutcome::Cancelled("state_changed"));
    }
    if let Some(reason) = permission_problem(conn, &rule, &sessions).await? {
        // Толчок больше не имеет права писать: правило сдаётся, удержанный итог уходит
        // подписчикам в этой же транзакции, чтобы оркестратор не ждал его вечно.
        exhaust_in_tx(
            conn,
            &mut rule,
            &payload.signals,
            Some(reason),
            now,
            correlation_id,
        )
        .await?;
        return Ok(JobOutcome::Done(
            json!({"state":"exhausted","reason":reason}),
        ));
    }
    let facts = facts_in_tx(conn, target, &rule, now).await?;
    let progress = still_progress(rule.progress_baseline.as_ref(), &facts.baseline);
    let consecutive = if progress { 1 } else { rule.consecutive + 1 };
    if consecutive > rule.max_consecutive || rule.total >= rule.max_total {
        let mut failed = facts.signals.clone();
        failed.no_marker = true;
        sqlx::query("update nudge_rules set consecutive=$2,updated_at=$3 where conversation_id=$1")
            .bind(target)
            .bind(consecutive)
            .bind(now)
            .execute(&mut *conn)
            .await?;
        rule.consecutive = consecutive;
        exhaust_in_tx(
            conn,
            &mut rule,
            &failed,
            Some("gave_up"),
            now,
            correlation_id,
        )
        .await?;
        return Ok(JobOutcome::Done(
            json!({"state":"exhausted","attempts":rule.total}),
        ));
    }
    let attempt = rule.total + 1;
    let template =
        crate::presets::nudge_text(&mut *conn, rule.owner_id, rule.text_preset_id.as_deref())
            .await?;
    let text = nudge_text(
        &rule,
        &template,
        &facts.signals,
        facts.signals.background_suspected,
    );
    let depth = access::current_depth(conn, target).await?;
    let event = origin::HubEvent {
        kind: "nudge".to_owned(),
        session_id: Some(target),
        detail: Some(facts.signals.text()),
        ..origin::HubEvent::default()
    };
    let origin = json!({
        "kind": "nudge",
        "rule_id": target,
        "attempt": attempt,
        "max": rule.max_consecutive,
        "chain_depth": depth,
        "event": event,
    });
    let submitted = submit_input_in_tx(
        conn,
        &SubmitInput {
            owner_id: rule.owner_id,
            conversation_id: target,
            idempotency_key: format!("nudge:{}:{}:{attempt}", target, payload.fingerprint),
            content: vec![ContentBlock::Text { text }],
            received_at: now,
            correlation_id,
            origin: Some(origin),
        },
    )
    .await?;
    let row: RuleRow = sqlx::query_as(
        "update nudge_rules set state='watching',held_fingerprint=null,consecutive=$2,
             total=$3,progress_baseline=$4,updated_at=$5
         where conversation_id=$1 returning *",
    )
    .bind(target)
    .bind(consecutive)
    .bind(attempt)
    .bind(serde_json::to_value(&facts.baseline).unwrap_or(Value::Null))
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    record_event_in_tx(
        conn,
        &row,
        attempt,
        &facts.signals,
        "nudged",
        Some(submitted.input_id),
        now,
    )
    .await?;
    changed_in_tx(conn, &row, correlation_id, Some("nudged")).await?;
    Ok(JobOutcome::Done(json!({
        "state":"nudged",
        "attempt":attempt,
        "input_id":submitted.input_id
    })))
}

/// Снять ожидающие задания толчка. `skip locked` не ждёт строку, которую уже
/// выбрал планировщик: отпечаток всё равно сделает такое задание устаревшим.
async fn cancel_pending_jobs_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    current: Option<Uuid>,
    reason: &str,
) -> Result<(), sqlx::Error> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select id from scheduled_jobs
         where kind='session_task' and state='pending' and conversation_id=$1
           and payload->>'action'='nudge'
         for update skip locked",
    )
    .bind(target_id)
    .fetch_all(&mut *conn)
    .await?;
    for id in ids {
        if Some(id) == current {
            continue;
        }
        sqlx::query(
            "update scheduled_jobs set state='cancelled',result=$2,updated_at=clock_timestamp()
             where id=$1",
        )
        .bind(id)
        .bind(json!({"reason":reason}))
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Выключатель `[orchestrator.nudge]`: правило перестаёт удерживать и толкать,
/// удержанный итог уходит подписчикам. Очередь цели уже взята вызывающим.
async fn disable_locked_in_tx(
    conn: &mut PgConnection,
    rule: &mut RuleRow,
    current: Option<Uuid>,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    if let Some(fingerprint) = rule.held_fingerprint.clone() {
        let row: RuleRow = sqlx::query_as(
            "update nudge_rules set state='watching',held_fingerprint=null,updated_at=$2
             where conversation_id=$1 returning *",
        )
        .bind(rule.conversation_id)
        .bind(now)
        .fetch_one(&mut *conn)
        .await?;
        *rule = row;
        let signals = Signals::default();
        release_held_in_tx(
            conn,
            rule,
            Some(&fingerprint),
            &signals,
            Some("disabled"),
            now,
        )
        .await?;
    } else {
        let row: Option<RuleRow> = sqlx::query_as(
            "update nudge_rules set state='watching',updated_at=$2
             where conversation_id=$1 and state='pending' returning *",
        )
        .bind(rule.conversation_id)
        .bind(now)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(row) = row {
            *rule = row;
        }
    }
    cancel_pending_jobs_in_tx(conn, rule.conversation_id, current, "disabled").await?;
    changed_in_tx(conn, rule, correlation_id, Some("disabled")).await?;
    Ok(())
}

/// Ошибка снятия удержаний на старте: нужен тип помощника повторов,
/// который умеет отличать временный сбой базы.
#[derive(Debug)]
enum DisableError {
    Db(sqlx::Error),
    Conflict(crate::idempotency::IdempotencyConflict),
}

impl From<sqlx::Error> for DisableError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl From<crate::idempotency::IdempotencyConflict> for DisableError {
    fn from(error: crate::idempotency::IdempotencyConflict) -> Self {
        Self::Conflict(error)
    }
}

impl crate::db::TransientError for DisableError {
    fn is_transient(&self) -> bool {
        match self {
            Self::Db(error) => crate::db::is_transient(error),
            Self::Conflict(_) => false,
        }
    }
}

/// Запуск хаба с выключенным автопинком: снимаем удержания и ожидающие задания
/// до первого тика планировщика. Каждое правило — своя транзакция с очередью цели.
pub async fn disable_all_on_startup(
    pool: &sqlx::PgPool,
    now: DateTime<Utc>,
) -> Result<usize, sqlx::Error> {
    let targets: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select conversation_id,owner_id from nudge_rules
         where state='pending' or held_fingerprint is not null order by conversation_id",
    )
    .fetch_all(pool)
    .await?;
    let mut done = 0;
    for (target, owner) in targets {
        // Тот же помощник повторов, что у инструментов: снятие удержания может
        // столкнуться с удалением подписчика или очередью и не должно теряться.
        let key = format!("nudge-disable:{target}:{}", now.timestamp_millis());
        let outcome = crate::idempotency::run_in_tx::<usize, DisableError, _>(
            pool,
            owner,
            &key,
            "nudge_disable",
            &json!({"target":target}),
            Some(target),
            move |conn| {
                Box::pin(async move {
                    send::lock_queue(conn, target).await?;
                    if let Some(mut rule) = rule_locked_in_tx(conn, target).await? {
                        disable_locked_in_tx(conn, &mut rule, None, now, Uuid::now_v7()).await?;
                        return Ok(1);
                    }
                    Ok(0)
                })
            },
        )
        .await
        .map_err(|error| match error {
            DisableError::Db(error) => error,
            DisableError::Conflict(_) => {
                sqlx::Error::Protocol("nudge_disable_idempotency_conflict".into())
            }
        })?;
        done += outcome.value;
    }
    Ok(done)
}

/// Задание не смогло толкнуть: удержание снимается, проектор оценит простой заново.
/// Иначе ожидающего задания больше нет, а правило осталось бы `pending` с тем же
/// отпечатком и удерживало итог до вмешательства.
async fn cancel_hold_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    fingerprint: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let row: Option<RuleRow> = sqlx::query_as(
        "update nudge_rules set state='watching',held_fingerprint=null,updated_at=$3
         where conversation_id=$1 and state='pending' and held_fingerprint=$2
         returning *",
    )
    .bind(target_id)
    .bind(fingerprint)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(row) = row {
        changed_in_tx(conn, &row, Uuid::now_v7(), Some("state_changed")).await?;
    }
    Ok(())
}

/// Права отложенного толчка (§10 п. 6): автор всё ещё предок, режимы не расширились,
/// у цели нет открытых запросов владельцу. Правило владельца проверяет только цель.
async fn permission_problem(
    conn: &mut PgConnection,
    rule: &RuleRow,
    sessions: &[access::Session],
) -> Result<Option<&'static str>, sqlx::Error> {
    let target = sessions.iter().find(|s| s.id == rule.conversation_id);
    let Some(target) = target else {
        return Ok(Some("target_gone"));
    };
    let open: bool = sqlx::query_scalar(
        "select exists(select 1 from requests where conversation_id=$1 and state='open')",
    )
    .bind(rule.conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    if open {
        return Ok(Some("owner_request_open"));
    }
    if rule.set_by != "agent" {
        return Ok(None);
    }
    let Some(author_id) = rule.set_by_conversation else {
        return Ok(Some("author_gone"));
    };
    let Some(author) = sessions.iter().find(|s| s.id == author_id) else {
        return Ok(Some("author_gone"));
    };
    let ancestor: bool = sqlx::query_scalar(
        "with recursive up(id,parent,step) as (
           select id,parent_conversation_id,0 from conversations where id=$1 and owner_id=$3
           union all
           select c.id,c.parent_conversation_id,u.step+1 from conversations c
             join up u on c.id=u.parent where u.step<32 and c.owner_id=$3)
         select exists(select 1 from up where id=$2 and step>0)",
    )
    .bind(rule.conversation_id)
    .bind(author_id)
    .bind(rule.owner_id)
    .fetch_one(&mut *conn)
    .await?;
    if !ancestor {
        return Ok(Some("author_not_ancestor"));
    }
    if access::check_read_only(author, target).is_err() {
        return Ok(Some("read_only"));
    }
    Ok(None)
}

/// Исчерпание: правило сдаётся, удержанный итог уходит подписчикам или заметкой
/// `notify_conversation_id`. Всё в одной транзакции с заданием.
async fn exhaust_in_tx(
    conn: &mut PgConnection,
    rule: &mut RuleRow,
    signals: &Signals,
    detail: Option<&str>,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let held = rule.held_fingerprint.clone();
    let row: RuleRow = sqlx::query_as(
        "update nudge_rules set state='exhausted',held_fingerprint=null,updated_at=$2
         where conversation_id=$1 returning *",
    )
    .bind(rule.conversation_id)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    *rule = row;
    record_event_in_tx(conn, rule, rule.total, signals, "exhausted", None, now).await?;
    release_held_in_tx(conn, rule, held.as_deref(), signals, detail, now).await?;
    changed_in_tx(conn, rule, correlation_id, detail).await?;
    Ok(())
}

/// Отпускание удержанного: живые подписки получают ровно одно событие, а без подписчиков
/// заметка уходит `notify_conversation_id`. Подписки берутся после правила и по возрастанию.
async fn release_held_in_tx(
    conn: &mut PgConnection,
    rule: &RuleRow,
    held: Option<&str>,
    signals: &Signals,
    detail: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let Some(fingerprint) = held else {
        return Ok(());
    };
    // Порядок блокировок: разговоры подписчиков по возрастанию ID, затем подписки,
    // затем задания. Удаление разговора держит строку разговора и удаляет подписку;
    // обратный порядок давал цикл. Ждём, а не пропускаем: иначе отпускание решило бы,
    // что подписчиков нет, и поставило заметку, а затем пришёл бы ещё и обычный итог.
    let subscribers: Vec<Uuid> = sqlx::query_scalar(
        "select distinct subscriber_id from session_subscriptions
         where target_id=$1 and closed_at is null and last_fingerprint is distinct from $2
         order by subscriber_id",
    )
    .bind(rule.conversation_id)
    .bind(fingerprint)
    .fetch_all(&mut *conn)
    .await?;
    if !subscribers.is_empty() {
        sqlx::query("select id from conversations where id=any($1) order by id for key share")
            .bind(&subscribers)
            .fetch_all(&mut *conn)
            .await?;
    }
    let rows: Vec<(Uuid, Uuid, Uuid, i32, DateTime<Utc>)> = sqlx::query_as(
        "select id,owner_id,subscriber_id,chain_depth,armed_at
         from session_subscriptions
         where target_id=$1 and closed_at is null and last_fingerprint is distinct from $2
         order by id
         for update",
    )
    .bind(rule.conversation_id)
    .bind(fingerprint)
    .fetch_all(&mut *conn)
    .await?;
    if rows.is_empty() {
        // Отпечаток уже ушёл подписке и она закрылась: это не «нет подписчиков»,
        // а уже доставленный итог, повторная заметка была бы дублем.
        let reported: bool = sqlx::query_scalar(
            "select exists(select 1 from session_subscriptions
                           where target_id=$1 and last_fingerprint=$2 and closed_at is not null)",
        )
        .bind(rule.conversation_id)
        .bind(fingerprint)
        .fetch_one(&mut *conn)
        .await?;
        if reported {
            return Ok(());
        }
        if let Some(notify) = rule.notify_conversation_id.or(rule.set_by_conversation) {
            let mut event = origin::HubEvent {
                kind: "nudge_exhausted".to_owned(),
                session_id: Some(rule.conversation_id),
                detail: Some(detail.unwrap_or("exhausted").to_owned()),
                ..origin::HubEvent::default()
            };
            event.outcome = Some("nudge_exhausted".to_owned());
            let text = format!(
                "[canterlot] Auto-nudge gave up on session {} after {} nudges. {}",
                rule.conversation_id,
                rule.total,
                signals.text()
            );
            subs::insert_job(
                conn,
                rule.owner_id,
                notify,
                now,
                &json!({
                    "action":"note",
                    "text":text,
                    "origin":origin::with_event(
                        json!({"kind":"event","chain_depth":0}), event)
                }),
            )
            .await?;
        }
        return Ok(());
    }
    let boundary = summary::boundary_in_tx(conn, rule.conversation_id).await?;
    for (subscription_id, owner_id, subscriber_id, chain_depth, armed_at) in rows {
        // Пачка остаётся владельцем итога: отпускание только сдвигает отпечаток,
        // а сообщение соберётся, когда устоятся все её цели. Иначе оркестратор
        // получил бы и отпущенный итог, и пачку за один простой.
        if crate::session_tools::alarms::hold_for_batch_in_tx(
            conn,
            subscriber_id,
            rule.conversation_id,
            "idle",
            fingerprint,
            subscription_id,
            armed_at,
            boundary.as_ref(),
            chain_depth,
            now,
        )
        .await?
        {
            sqlx::query("update session_subscriptions set last_fingerprint=$2 where id=$1")
                .bind(subscription_id)
                .bind(fingerprint)
                .execute(&mut *conn)
                .await?;
            continue;
        }
        let payload = json!({
            "action": "event",
            "subscription_id": subscription_id,
            "target_id": rule.conversation_id,
            "state": "idle",
            "fingerprint": fingerprint,
            "armed_at": armed_at,
            "chain_depth": chain_depth,
            "boundary": boundary,
            "nudge_exhausted": true,
            "nudges": rule.total,
            "signals": signals,
            "signals_text": signals.text(),
        });
        subs::insert_job(conn, owner_id, subscriber_id, now, &payload).await?;
        sqlx::query(
            "update session_subscriptions set last_fingerprint=$2,closed_at=clock_timestamp(),
                 closed_reason='idle' where id=$1",
        )
        .bind(subscription_id)
        .bind(fingerprint)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Приём результата: правило больше не нужно, ожидающие толчки отменяются.
/// Очередь цели уже взята вызывающим (accept/delete), удержанный итог закрыт подписками.
pub async fn remove_rule_in_tx(
    conn: &mut PgConnection,
    target_id: Uuid,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    cancel_pending_jobs_in_tx(conn, target_id, None, "accepted").await?;
    let Some(mut rule) = rule_locked_in_tx(conn, target_id).await? else {
        return Ok(());
    };
    sqlx::query("delete from nudge_rules where conversation_id=$1")
        .bind(target_id)
        .execute(&mut *conn)
        .await?;
    rule.held_fingerprint = None;
    rule.state = "exhausted".to_owned();
    rule.updated_at = now;
    changed_in_tx(conn, &rule, correlation_id, Some("accepted")).await?;
    Ok(())
}

/// Задание исчерпало попытки: правило сдаётся, удержанный итог отпускается здесь же.
/// `true` значит, что строку держит чужая транзакция и задание надо повторить позже.
pub async fn abandon_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    payload: &Value,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let Ok(job) = serde_json::from_value::<NudgeJob>(payload.clone()) else {
        return Ok(false);
    };
    // Очередь получателя — до правила и подписок, как в доставке будильника.
    send::lock_queue(conn, conversation_id).await?;
    let row: Option<RuleRow> = sqlx::query_as(
        "select * from nudge_rules where conversation_id=$1 and state='pending' for update nowait",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(mut rule) = row else {
        return Ok(false);
    };
    if rule.held_fingerprint.as_deref() != Some(&job.fingerprint) {
        return Ok(false);
    }
    exhaust_in_tx(
        conn,
        &mut rule,
        &job.signals,
        Some("nudge_failed"),
        now,
        Uuid::now_v7(),
    )
    .await?;
    Ok(false)
}

// ---------------------------------------------------------------------------
// Текст толчка (встроенный пресет B1)
// ---------------------------------------------------------------------------

fn nudge_text(rule: &RuleRow, template: &str, signals: &Signals, background: bool) -> String {
    let markers = rule.done_markers.join(" or ");
    let mut text = template
        .replace("{markers}", &markers)
        .replace("{signals}", &signals.text());
    if background {
        text.push_str(
            "\nA command you started in the background does not wake you when it finishes. \
             Check its output now; if it is still running, wait for it inside your turn, then continue.",
        );
    }
    text
}

// ---------------------------------------------------------------------------
// Инструменты `nudge_set` / `nudge_clear`
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetArgs {
    session_id: Uuid,
    enabled: Option<bool>,
    idle_minutes: Option<i32>,
    max_nudges: Option<i32>,
    done_markers: Option<Vec<String>>,
    text_preset: Option<String>,
    context_warn_percent: Option<i32>,
}

fn validate_set(args: &SetArgs) -> Result<(), ToolError> {
    if args
        .idle_minutes
        .is_some_and(|value| !(1..=120).contains(&value))
        || args
            .max_nudges
            .is_some_and(|value| !(1..=10).contains(&value))
        || args
            .context_warn_percent
            .is_some_and(|value| !(50..=99).contains(&value))
    {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }
    if let Some(markers) = &args.done_markers
        && (markers.is_empty()
            || markers.len() > MAX_MARKERS
            || markers.iter().any(|marker| {
                marker.trim().is_empty() || marker.chars().count() > MAX_MARKER_CHARS
            }))
    {
        return Err(ToolError::new(ToolErrorCode::InvalidArguments));
    }
    Ok(())
}

/// Пресет текста толчка: встроенный или пользовательский с назначением `nudge`.
/// Чужая ссылка отвечает `profile_unknown`, как неизвестный профиль.
async fn check_text_preset_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    reference: Option<&str>,
) -> Result<(), ToolError> {
    let Some(reference) = reference else {
        return Ok(());
    };
    if reference == crate::presets::BUILTIN_NUDGE_ID {
        return Ok(());
    }
    let Ok(id) = Uuid::parse_str(reference) else {
        return Err(ToolError::new(ToolErrorCode::ProfileUnknown));
    };
    let alive: bool = sqlx::query_scalar(
        "select exists(select 1 from instruction_presets
         where owner_id=$1 and id=$2 and purpose='nudge' and deleted_at is null)",
    )
    .bind(owner_id)
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    if alive {
        Ok(())
    } else {
        Err(ToolError::new(ToolErrorCode::ProfileUnknown))
    }
}

fn hint() -> &'static str {
    "canterlot now nudges this session itself. Your subscription stays: you get one message when it is done, asks, fails, or the nudges run out. End your turn."
}

/// Агент ставит правило только на своего потомка: видимость семьи здесь не расширяет запись.
async fn check_descendant_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    parent_id: Uuid,
    target_id: Uuid,
) -> Result<(), ToolError> {
    let descendant: bool = sqlx::query_scalar(
        "with recursive up(id,parent,step) as (
           select id,parent_conversation_id,0 from conversations where id=$1 and owner_id=$3
           union all
           select c.id,c.parent_conversation_id,u.step+1 from conversations c
             join up u on c.id=u.parent where u.step<32 and c.owner_id=$3)
         select exists(select 1 from up where id=$2 and step>0)",
    )
    .bind(target_id)
    .bind(parent_id)
    .bind(owner_id)
    .fetch_one(&mut *conn)
    .await?;
    if descendant {
        Ok(())
    } else {
        Err(ToolError::new(ToolErrorCode::NotParent))
    }
}

async fn upsert_rule_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    author: &access::Session,
    target_id: Uuid,
    args: &SetArgs,
    now: DateTime<Utc>,
) -> Result<RuleRow, sqlx::Error> {
    let enabled = args.enabled.unwrap_or(true);
    let markers = args
        .done_markers
        .clone()
        .unwrap_or_else(|| vec!["STATUS: DONE".to_owned(), "STATUS: BLOCKED".to_owned()]);
    sqlx::query_as(
        "insert into nudge_rules(conversation_id,owner_id,set_by,set_by_conversation,
             notify_conversation_id,enabled,idle_minutes,background_minutes,max_consecutive,
             max_total,done_markers,text_preset_id,context_warn_percent,state,created_at,updated_at)
         values($1,$2,'agent',$3,$3,$4,
             coalesce($5,3),greatest(coalesce($5,3),10),coalesce($6,3),20,$7,$10,
             coalesce($8,80),'watching',$9,$9)
         on conflict(conversation_id) do update set
             enabled=excluded.enabled,
             idle_minutes=excluded.idle_minutes,
             background_minutes=excluded.background_minutes,
             max_consecutive=excluded.max_consecutive,
             done_markers=excluded.done_markers,
             text_preset_id=excluded.text_preset_id,
             context_warn_percent=excluded.context_warn_percent,
             notify_conversation_id=excluded.notify_conversation_id,
             set_by='agent',set_by_conversation=$3,
             state=case when nudge_rules.state='exhausted' and excluded.enabled
                        then 'watching' else nudge_rules.state end,
             held_fingerprint=case when excluded.enabled then nudge_rules.held_fingerprint
                                   else null end,
             updated_at=$9
         returning *",
    )
    .bind(target_id)
    .bind(owner_id)
    .bind(author.id)
    .bind(enabled)
    .bind(args.idle_minutes)
    .bind(args.max_nudges)
    .bind(&markers)
    .bind(args.context_warn_percent)
    .bind(now)
    .bind(&args.text_preset)
    .fetch_one(&mut *conn)
    .await
}

pub async fn set(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: SetArgs = parse_arguments(arguments)?;
    validate_set(&args)?;
    if !context.nudge_enabled {
        return Err(ToolError::with_message(
            ToolErrorCode::InvalidArguments,
            "Auto-nudge is disabled on this hub",
        ));
    }
    if args.enabled == Some(false) {
        return clear(context, caller, json!({"session_id": args.session_id})).await;
    }
    let now = Utc::now();
    let mut tx = crate::db::begin(&context.pool).await?;
    send::lock_sender(&mut tx, caller.conversation_id).await?;
    let author = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    let target = access::session_in_tx(&mut tx, caller.owner_id, args.session_id)
        .await?
        .ok_or_else(|| ToolError::new(ToolErrorCode::SessionNotFound))?;
    check_descendant_in_tx(&mut tx, caller.owner_id, author.id, target.id).await?;
    access::check_read_only(&author, &target)?;
    check_text_preset_in_tx(&mut tx, caller.owner_id, args.text_preset.as_deref()).await?;
    send::lock_queue(&mut tx, target.id).await?;
    let rule = upsert_rule_in_tx(&mut tx, caller.owner_id, &author, target.id, &args, now).await?;
    changed_in_tx(&mut tx, &rule, caller.call_id, Some("set")).await?;
    tx.commit().await?;
    if let Some(dispatcher) = &context.dispatcher {
        dispatcher.trigger(target.id);
    }
    Ok(json!({
        "rule": rule.view(),
        "state": rule.state,
        "hint": hint(),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClearArgs {
    session_id: Uuid,
}

pub async fn clear(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: ClearArgs = parse_arguments(arguments)?;
    let now = Utc::now();
    let mut tx = crate::db::begin(&context.pool).await?;
    send::lock_sender(&mut tx, caller.conversation_id).await?;
    let author = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    let target = access::session_in_tx(&mut tx, caller.owner_id, args.session_id)
        .await?
        .ok_or_else(|| ToolError::new(ToolErrorCode::SessionNotFound))?;
    check_descendant_in_tx(&mut tx, caller.owner_id, author.id, target.id).await?;
    access::check_read_only(&author, &target)?;
    send::lock_queue(&mut tx, target.id).await?;
    let Some(mut rule) = rule_locked_in_tx(&mut tx, target.id).await? else {
        tx.commit().await?;
        return Ok(json!({"state":"none","hint":"No auto-nudge rule in this session."}));
    };
    let held = rule.held_fingerprint.clone();
    let signals = Signals::default();
    // Выключение снимает удержание в той же транзакции: подписчики получают удержанный итог.
    release_held_in_tx(
        &mut tx,
        &rule,
        held.as_deref(),
        &signals,
        Some("rule_off"),
        now,
    )
    .await?;
    cancel_pending_jobs_in_tx(&mut tx, target.id, None, "rule_off").await?;
    sqlx::query("delete from nudge_rules where conversation_id=$1")
        .bind(target.id)
        .execute(&mut *tx)
        .await?;
    rule.held_fingerprint = None;
    changed_in_tx(&mut tx, &rule, caller.call_id, Some("cleared")).await?;
    tx.commit().await?;
    Ok(json!({"state":"cleared","released":held.is_some()}))
}

/// Правило, поставленное вместе с запуском: та же проверка, без инструментальных аргументов.
pub fn validate_spawn_arg(value: &Option<Value>) -> Result<(), ToolError> {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => Ok(()),
        Some(Value::Bool(true)) => Ok(()),
        Some(Value::Object(_)) => {
            let mut value = value.clone().unwrap_or_else(|| json!({}));
            if let Some(object) = value.as_object_mut() {
                object.insert("session_id".to_owned(), json!(Uuid::nil()));
            }
            let args: SetArgs = parse_arguments(value)?;
            validate_set(&args)?;
            Ok(())
        }
        Some(_) => Err(ToolError::new(ToolErrorCode::InvalidArguments)),
    }
}

pub async fn set_for_child_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    author: &access::Session,
    child: Uuid,
    nudge: &Value,
    now: DateTime<Utc>,
) -> Result<(), ToolError> {
    let (enabled, args_value) = match nudge {
        Value::Bool(enabled) => (*enabled, json!({})),
        Value::Object(_) => (true, nudge.clone()),
        Value::Null => (false, json!({})),
        _ => return Err(ToolError::new(ToolErrorCode::InvalidArguments)),
    };
    if !enabled {
        return Ok(());
    }
    let mut value = args_value;
    if let Some(object) = value.as_object_mut() {
        object.insert("session_id".to_owned(), json!(child));
    }
    let args: SetArgs = parse_arguments(value)?;
    validate_set(&args)?;
    check_text_preset_in_tx(conn, owner_id, args.text_preset.as_deref()).await?;
    let rule = upsert_rule_in_tx(conn, owner_id, author, child, &args, now).await?;
    changed_in_tx(conn, &rule, Uuid::now_v7(), Some("set")).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Снимок и осмотр
// ---------------------------------------------------------------------------

pub async fn snapshot_rule(
    conn: &mut PgConnection,
    target_id: Uuid,
) -> Result<Option<NudgeRuleView>, sqlx::Error> {
    Ok(rule_in_tx(conn, target_id).await?.map(|rule| rule.view()))
}

pub async fn snapshot_events(
    conn: &mut PgConnection,
    target_id: Uuid,
    limit: i64,
) -> Result<Vec<NudgeEventView>, sqlx::Error> {
    let rows: Vec<(Uuid, DateTime<Utc>, i32, String, Value)> = sqlx::query_as(
        "select id,at,attempt,outcome,signals from nudge_events
         where conversation_id=$1 order by at desc,id desc limit $2",
    )
    .bind(target_id)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, at, attempt, outcome, signals)| NudgeEventView {
            id,
            at: at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            attempt,
            outcome,
            signals,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Тесты чистых правил
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn markers() -> Vec<String> {
        vec!["STATUS: DONE".into(), "STATUS: BLOCKED".into()]
    }

    #[test]
    fn marker_matches_line_ignoring_case_and_spaces() {
        assert!(marker_seen("work\n  status: done  \n", &markers()));
        assert!(marker_seen("done\nSTATUS: BLOCKED", &markers()));
        assert!(!marker_seen("I will write STATUS: DONE later", &markers()));
    }

    #[test]
    fn marker_inside_a_code_fence_does_not_count() {
        assert!(!marker_seen(
            "example:\n```\nSTATUS: DONE\n```\n",
            &markers()
        ));
        assert!(marker_seen(
            "```\nSTATUS: DONE\n```\nSTATUS: DONE",
            &markers()
        ));
    }

    #[test]
    fn a_long_fenced_report_does_not_hide_a_trailing_marker() {
        let text = format!("```\n{}\n```\nSTATUS: DONE", "x".repeat(2000));
        assert!(marker_seen(&text, &markers()));
    }

    #[test]
    fn marker_outside_the_last_line_counts() {
        assert!(marker_seen("report\n\nSTATUS: DONE\nthanks", &markers()));
    }

    #[test]
    fn tail_is_bounded_by_char_boundary() {
        let text = format!("{}\nSTATUS: DONE", "я".repeat(2000));
        assert!(marker_seen(&text, &markers()));
        assert!(tail(&text).len() <= ANSWER_TAIL_BYTES);
    }

    #[test]
    fn question_in_the_last_paragraph_blocks_a_nudge() {
        assert!(asks_in_text("Done with A.\n\nShould I merge it?"));
        assert!(asks_in_text("Do you want me to continue"));
        assert!(asks_in_text(" Shall I push?\n"));
        assert!(asks_in_text("Хотите, чтобы я продолжил"));
        assert!(asks_in_text("Нужно ли коммитить"));
        assert!(!asks_in_text("Should I merge it?\n\nI did it already."));
        assert!(!asks_in_text("Report ready.\nSTATUS: DONE"));
    }

    #[test]
    fn every_continue_phrase_is_recognized() {
        for phrase in CONTINUE_PHRASES {
            assert!(
                continue_phrase(&format!("Some text {phrase} and more")),
                "{phrase}"
            );
        }
        assert!(continue_phrase("Продолжу после сборки"));
        assert!(continue_phrase("Дождусь тестов"));
        assert!(!continue_phrase("The build is finished."));
    }

    #[test]
    fn background_commands_are_suspected() {
        for command in [
            "cargo test &",
            "nohup ./run.sh",
            "setsid sleep 10",
            "disown %1",
            "{\"run_in_background\": true}",
            "tmux new -d -s build",
            "screen -dm bash",
        ] {
            assert!(background_in_commands(&[command.to_owned()]), "{command}");
        }
        assert!(!background_in_commands(&["cargo test && cargo fmt".into()]));
        assert!(background_in_commands(&["bash -lc 'cargo test &'".into()]));
        assert!(!background_in_commands(&["cargo test".into()]));
    }

    #[test]
    fn progress_compares_head_files_and_tool_turns() {
        let current = Baseline {
            head: "bbb".into(),
            dirty: true,
            files: "f".into(),
            report_size: 0,
            run_id: Some(Uuid::nil()),
            tools: 2,
            working_ms: 90_000,
        };
        let same = json!({
            "head":"bbb","dirty":true,"files":"f","report_size":0,
            "run_id":Uuid::nil(),"tools":2,"working_ms":90_000
        });
        assert!(!still_progress(Some(&same), &current));
        let moved = json!({
            "head":"aaa","dirty":true,"files":"e","report_size":0,
            "run_id":Uuid::nil(),"tools":2,"working_ms":90_000
        });
        assert!(still_progress(Some(&moved), &current));
        assert!(still_progress(None, &current));
    }

    #[test]
    fn signals_text_is_short_english() {
        let signals = Signals {
            no_marker: true,
            uncommitted: true,
            idle_minutes: 6,
            ..Signals::default()
        };
        assert_eq!(
            signals.text(),
            "idle 6 min; no done marker; uncommitted changes"
        );
    }

    #[test]
    fn nudge_text_names_markers_and_does_not_pretend_to_be_the_owner() {
        let rule = rule_stub();
        let text = nudge_text(
            &rule,
            crate::presets::BUILTIN_NUDGE_TEXT,
            &Signals::default(),
            false,
        );
        assert!(text.starts_with("[canterlot] Automatic reminder"));
        assert!(text.contains("not the owner"));
        assert!(text.contains("STATUS: DONE or STATUS: BLOCKED"));
        assert!(!text.contains("background command may still be running"));
        let text = nudge_text(
            &rule,
            crate::presets::BUILTIN_NUDGE_TEXT,
            &Signals::default(),
            true,
        );
        assert!(text.contains("does not wake you when it finishes"));
    }

    #[test]
    fn nudge_text_substitutes_the_rule_markers_and_signals_into_a_preset() {
        let rule = rule_stub();
        let signals = Signals {
            no_marker: true,
            continue_phrase: true,
            idle_minutes: 6,
            ..Signals::default()
        };
        let text = nudge_text(
            &rule,
            "[canterlot] Continue. Finish with {markers}. Observed: {signals}.",
            &signals,
            false,
        );
        assert_eq!(
            text,
            "[canterlot] Continue. Finish with STATUS: DONE or STATUS: BLOCKED. \
             Observed: idle 6 min; no done marker; you said you would continue."
        );
    }

    fn rule_stub() -> RuleRow {
        RuleRow {
            conversation_id: Uuid::nil(),
            owner_id: Uuid::nil(),
            set_by: "agent".into(),
            set_by_conversation: None,
            notify_conversation_id: None,
            enabled: true,
            idle_minutes: 3,
            background_minutes: 10,
            max_consecutive: 3,
            max_total: 20,
            done_markers: markers(),
            text_preset_id: None,
            context_warn_percent: Some(80),
            report_file: None,
            classifier: None,
            state: "watching".into(),
            held_fingerprint: None,
            consecutive: 0,
            total: 0,
            progress_baseline: None,
            context_warned_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}
