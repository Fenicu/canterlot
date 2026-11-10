//! Откуда пришёл ввод другой сессии: объект `inputs.origin` и видимая модели первая строка.
//! Строка с названием отправителя и его идентификатор живут только в этих двух местах и в
//! копиях ввода, поэтому удаление отправителя вычищает их отсюда же.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{attachments::blobstore::hex, delivery::ContentBlock, events::Scope};

pub const KIND_EVENT: &str = "event";
const KIND_MESSAGE: &str = "message";
const KIND_SPAWN: &str = "spawn";
const TITLE_CHARS: usize = 200;
const HEARD_FROM_DELETED: &str = "[canterlot] A session this conversation heard from was deleted.";

/// Форма `inputs.origin` для клиентов. После удаления отправителя остаются только
/// `kind`, `chain_depth` и `sender_deleted`.
#[derive(Serialize, utoipa::ToSchema)]
pub struct InputOrigin {
    pub kind: String,
    pub from: Option<Uuid>,
    pub title: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub chain_depth: i32,
    pub sha256: Option<String>,
    pub sender_deleted: Option<bool>,
    /// Разобранные факты служебного события: клиент не читает английский текст.
    pub event: Option<HubEvent>,
}

/// Коммит из итога сессии: короткий хеш и первая строка сообщения.
#[derive(Serialize, utoipa::ToSchema, Clone, Debug)]
pub struct HubCommit {
    pub sha: String,
    pub title: String,
}

/// Факты события хаба. Текст для модели остаётся прежним, а здесь лежит то, что
/// владелец видит в ленте.
#[derive(Serialize, utoipa::ToSchema, Clone, Debug, Default)]
pub struct HubEvent {
    /// `session_result`, `asked`, `stalled`, `failed`, `quota_paused`, `stopped`,
    /// `accepted`, `approval` или `deleted`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// Сумма ходов без ожидания владельца: разрешения и вопросы внутри хода.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_in: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_out: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commits: Option<Vec<HubCommit>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_files: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_block_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_alive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_busy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// `accepted`: `removed`, `none`, `dirty` или `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_kept: Option<bool>,
    /// `approval`: `allowed`, `denied`, `failed` или `lapsed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_id: Option<Uuid>,
    /// `accepted`: событие ушло другому подписчику, а не принявшему родителю.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// `alarm`: сработавший будильник.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alarm_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alarm_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fired_reason: Option<String>,
    /// `alarm` с условием `file`: путь и наблюдённый размер файла.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alarm_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alarm_size: Option<i64>,
    /// `alarm`: когда будильник взвели.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub armed_at: Option<DateTime<Utc>>,
    /// `alarm`: разобранное условие в именах полей `alarm_set`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<Value>,
    /// `alarm`: заметка себе, доставленная вместе с событием.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// `alarm` с условием `session_state`: цели и их состояние на момент срабатывания.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<AlarmSession>>,
    /// Пачка сработавших будильников: по объекту на каждый.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alarms: Option<Vec<AlarmEvent>>,
    /// `machine_*` и `lease_long_hold`: числа и имена, из которых собран английский текст.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// `machine_*`: машина, о которой говорит предупреждение.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<Uuid>,
    /// `machine_*`: имя машины для заголовка в ленте.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_name: Option<String>,
    /// `limit_resumed` и `quota_paused`: когда лимит сбросится или сбросился.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<DateTime<Utc>>,
    /// Сколько автоматических толчков получила цель до этого итога.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nudges: Option<i64>,
    /// `true`: толчки кончились без маркера, итог отпущен правилом.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nudge_exhausted: Option<bool>,
    /// Ответ кончается вопросом в тексте, а не карточкой.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asks_in_text: Option<bool>,
}

impl HubEvent {
    pub fn accepted(
        session_id: Uuid,
        worktree: &str,
        branch: Option<&str>,
        branch_kept: Option<bool>,
    ) -> Self {
        Self {
            kind: "accepted".to_owned(),
            session_id: Some(session_id),
            worktree: Some(worktree.to_owned()),
            branch: branch.map(str::to_owned),
            branch_kept,
            ..Self::default()
        }
    }

    pub fn accepted_with_detail(session_id: Uuid, worktree: &str, detail: Option<&str>) -> Self {
        Self {
            kind: "accepted".to_owned(),
            session_id: Some(session_id),
            worktree: Some(worktree.to_owned()),
            detail: detail.map(str::to_owned),
            ..Self::default()
        }
    }

    pub fn accepted_closed(session_id: Uuid) -> Self {
        Self {
            kind: "accepted".to_owned(),
            session_id: Some(session_id),
            closed: Some(true),
            ..Self::default()
        }
    }

    pub fn approval(
        decision: &str,
        request_id: Uuid,
        child_id: Option<Uuid>,
        detail: Option<&str>,
    ) -> Self {
        Self {
            kind: "approval".to_owned(),
            decision: Some(decision.to_owned()),
            request_id: Some(request_id),
            child_id,
            detail: detail.map(str::to_owned),
            ..Self::default()
        }
    }

    pub fn deleted(session_id: Uuid) -> Self {
        Self {
            kind: "deleted".to_owned(),
            session_id: Some(session_id),
            ..Self::default()
        }
    }
}

/// Одна цель условия `session_state` на момент срабатывания.
#[derive(Serialize, utoipa::ToSchema, Clone, Debug)]
pub struct AlarmSession {
    pub id: Uuid,
    pub title: String,
    pub state: String,
}

/// Один сработавший будильник пачки: те же поля, что и у одиночного события.
#[derive(Serialize, utoipa::ToSchema, Clone, Debug, Default)]
pub struct AlarmEvent {
    pub alarm_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alarm_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fired_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alarm_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alarm_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub armed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<AlarmSession>>,
}

/// Кладёт факты рядом с отправителем, не трогая остальные поля origin.
pub fn with_event(mut origin: Value, event: HubEvent) -> Value {
    if let Some(object) = origin.as_object_mut()
        && let Ok(value) = serde_json::to_value(event)
    {
        object.insert("event".to_owned(), value);
    }
    origin
}

/// Отправитель сообщения.
#[derive(Clone)]
pub struct Sender {
    pub id: Uuid,
    pub title: String,
    pub harness: String,
    pub model: String,
}

impl fmt::Debug for Sender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sender")
            .field("id", &self.id)
            .field("harness", &self.harness)
            .finish_non_exhaustive()
    }
}

/// Название попадает в кавычки первой строки: перевод строки или кавычка увели бы текст
/// отправителя за рамку служебной строки.
pub fn clean_title(title: &str) -> String {
    title
        .chars()
        .take(TITLE_CHARS)
        .map(|c| match c {
            '"' => '\'',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect()
}

pub fn text_sha256(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

pub fn message_origin(sender: &Sender, chain_depth: i32, sha256: &str) -> Value {
    json!({
        "kind": KIND_MESSAGE,
        "from": sender.id,
        "title": clean_title(&sender.title),
        "harness": sender.harness,
        "model": sender.model,
        "chain_depth": chain_depth,
        "sha256": sha256,
    })
}

pub fn message_text(sender: &Sender, text: &str) -> String {
    format!(
        "[canterlot] Message from session \"{}\" (id {}, {}, model {}). \
         This is another agent, not the owner.\n\n{text}",
        clean_title(&sender.title),
        sender.id,
        sender.harness,
        sender.model,
    )
}

pub fn spawn_origin(sender: &Sender, chain_depth: i32) -> Value {
    json!({
        "kind": KIND_SPAWN,
        "from": sender.id,
        "title": clean_title(&sender.title),
        "harness": sender.harness,
        "model": sender.model,
        "chain_depth": chain_depth,
    })
}

/// Первая строка остаётся одной строкой: по ней удаление отправителя отделяет бриф.
pub fn spawn_text(sender: &Sender, branch: Option<&str>, brief: &str) -> String {
    let worktree = branch.map_or(String::new(), |branch| {
        format!(" You work in an isolated git worktree on branch {branch}; commit there. Nothing is pushed.")
    });
    format!(
        "[canterlot] You were started by session \"{}\" (id {}, {}, model {}). \
         Your final message goes back to it. It is an agent, not the owner.{worktree}\n\n{brief}",
        clean_title(&sender.title),
        sender.id,
        sender.harness,
        sender.model,
    )
}

/// Служебное сообщение хаба о судьбе запуска. Как и событие, глубину цепочки не растит.
pub fn note_origin(chain_depth: i32) -> Value {
    json!({"kind": KIND_EVENT, "chain_depth": chain_depth})
}

/// Сообщение о запущенной сессии: при её удалении оно вычищается как событие от неё.
pub fn started_note_origin(child: &Sender, chain_depth: i32) -> Value {
    json!({
        "kind": KIND_EVENT,
        "from": child.id,
        "title": clean_title(&child.title),
        "harness": child.harness,
        "model": child.model,
        "chain_depth": chain_depth,
    })
}

/// Что остаётся от ввода, когда его отправитель удалён. Текст сообщения и бриф остаются:
/// это ввод другой сессии, на него она и отвечала. Итог и другие события делегата тоже
/// остаются, без строки с его названием; событие без узнаваемого заголовка сводится к факту.
fn scrubbed_text(kind: &str, text: &str, sender: Uuid) -> String {
    let body = text.split_once("\n\n").map(|(_, body)| body);
    match (kind, body) {
        ("message", Some(body)) => format!(
            "[canterlot] Message from a session that was deleted. \
             This is another agent, not the owner.\n\n{body}"
        ),
        ("spawn", Some(body)) => format!(
            "[canterlot] You were started by a session that was deleted. \
             It was an agent, not the owner.\n\n{body}"
        ),
        (KIND_EVENT, _) => {
            scrubbed_event(text, sender).unwrap_or_else(|| HEARD_FROM_DELETED.to_owned())
        }
        _ => HEARD_FROM_DELETED.to_owned(),
    }
}

/// Итог, вопрос, простой и прочие события подписки начинаются строкой
/// `[canterlot] Session "<название>" (id <id>, <харнесс>, model <модель>)<состояние>`, а
/// дальше идут строки фактов и ответ. Заголовок узнаётся по id удалённой сессии; из него
/// остаётся только хвост после последней скобки («is now idle.»: в модели скобки бывают, в хвосте нет), остальные строки сохраняются, а id
/// в них (путь рабочей копии) заменяется. Однострочные заметки о запуске и приёме несут имя
/// посреди фразы и сводятся к факту.
fn scrubbed_event(text: &str, sender: Uuid) -> Option<String> {
    let (head, body) = text.split_once('\n')?;
    let rest = head.strip_prefix("[canterlot] Session \"")?;
    let marker = format!("\" (id {sender}, ");
    let after = &rest[rest.find(&marker)? + marker.len()..];
    let state = &after[after.rfind(')')? + 1..];
    let body = body.trim_start_matches('\n');
    if body.trim().is_empty() {
        return None;
    }
    let kept = format!(
        "[canterlot] Report from a delegate session that was later deleted. \
         It was an agent, not the owner.\nThe session{state}\n{body}"
    );
    Some(kept.replace(&sender.to_string(), "(deleted)"))
}

fn scrubbed_content(kind: &str, content: &[ContentBlock], sender: Uuid) -> Vec<ContentBlock> {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => ContentBlock::Text {
                text: scrubbed_text(kind, text, sender),
            },
            attachment => attachment.clone(),
        })
        .collect()
}

#[derive(sqlx::FromRow)]
struct SentInput {
    id: Uuid,
    conversation_id: Uuid,
    command_id: Uuid,
    content: sqlx::types::Json<Vec<ContentBlock>>,
    origin: Value,
}

/// Удаление отправителя: у чужих вводов пропадают его название, идентификатор и модель,
/// глубина цепочки остаётся. Только строки, очередь получателя не блокируется.
pub async fn scrub_sender_in_tx(conn: &mut PgConnection, sender: Uuid) -> Result<(), sqlx::Error> {
    // Заметка о запуске или приёме хранит имя и в задании: строка задания живёт дольше сессии.
    let notes: Vec<(Uuid, String, Option<Value>)> = sqlx::query_as(
        "select id,coalesce(payload->>'text',''),payload->'origin'->'chain_depth' from scheduled_jobs
         where kind='session_task' and payload->>'action'='note'
           and payload->'origin'->>'from'=$1 order by id",
    )
    .bind(sender.to_string())
    .fetch_all(&mut *conn)
    .await?;
    for (id, text, depth) in notes {
        sqlx::query(
            "update scheduled_jobs
             set payload=jsonb_build_object('action','note','text',$2::text,'origin',
                   jsonb_build_object('kind','event','chain_depth',$3::jsonb,'sender_deleted',true))
             where id=$1",
        )
        .bind(id)
        .bind(scrubbed_text(KIND_EVENT, &text, sender))
        .bind(depth)
        .execute(&mut *conn)
        .await?;
    }
    let rows: Vec<SentInput> = sqlx::query_as(
        "select id,conversation_id,command_id,content,origin from inputs
         where origin->>'from'=$1 and conversation_id<>$2 order by id",
    )
    .bind(sender.to_string())
    .bind(sender)
    .fetch_all(&mut *conn)
    .await?;
    for row in rows {
        let kind = row.origin["kind"].as_str().unwrap_or_default();
        let content = json!(scrubbed_content(kind, &row.content, sender));
        let origin = json!({
            "kind": kind,
            "chain_depth": row.origin["chain_depth"],
            "sender_deleted": true,
        });
        sqlx::query("update inputs set content=$2,origin=$3 where id=$1")
            .bind(row.id)
            .bind(&content)
            .bind(&origin)
            .execute(&mut *conn)
            .await?;
        sqlx::query("update commands set payload=jsonb_set(payload,'{content}',$2) where id=$1")
            .bind(row.command_id)
            .bind(&content)
            .execute(&mut *conn)
            .await?;
        sqlx::query(
            "update events set payload=payload||jsonb_build_object('content',$3::jsonb,'origin',$4::jsonb)
             where scope=$1 and kind='input_queued' and payload->>'input_id'=$2",
        )
        .bind(
            Scope::Conversation {
                id: row.conversation_id,
            }
            .key(),
        )
        .bind(row.id.to_string())
        .bind(&content)
        .bind(&origin)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENDER: Uuid = Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_00aa);

    fn sender() -> Sender {
        Sender {
            id: SENDER,
            title: "секретная задача".to_owned(),
            harness: "codex".to_owned(),
            model: "gpt-secret".to_owned(),
        }
    }

    #[test]
    fn a_settled_delegate_keeps_its_report_without_the_title_line() {
        let text = format!(
            "[canterlot] Session \"секретная задача\" (id {SENDER}, codex, model gpt-secret) is now idle.\n\
             Turns: 3, duration 4m 10s, tokens in/out 1200/300.\n\
             Worktree: /w/worktrees/{SENDER}, branch canterlot/000000aa.\n\
             Final answer:\nГотово, тесты зелёные.\n\n\
             Next: read the result, then call session_accept to clean up, or session_send to continue."
        );
        let kept = scrubbed_text(KIND_EVENT, &text, SENDER);
        assert_eq!(
            kept,
            "[canterlot] Report from a delegate session that was later deleted. \
             It was an agent, not the owner.\nThe session is now idle.\n\
             Turns: 3, duration 4m 10s, tokens in/out 1200/300.\n\
             Worktree: /w/worktrees/(deleted), branch canterlot/000000aa.\n\
             Final answer:\nГотово, тесты зелёные.\n\n\
             Next: read the result, then call session_accept to clean up, or session_send to continue."
        );
        for secret in [
            "секретная задача",
            "gpt-secret",
            "codex",
            &SENDER.to_string(),
        ] {
            assert!(!kept.contains(secret), "{secret} survives in {kept}");
        }
    }

    #[test]
    fn a_question_and_an_idle_warning_keep_their_facts() {
        let asked = format!(
            "[canterlot] Session \"a \"quoted\" (title)\" (id {SENDER}, claude_code, model opus (1m)) is now waiting_for_user.\n\
             Question: merge into main?\nThe owner answers in the app; do not answer for them."
        );
        assert_eq!(
            scrubbed_text(KIND_EVENT, &asked, SENDER),
            "[canterlot] Report from a delegate session that was later deleted. \
             It was an agent, not the owner.\nThe session is now waiting_for_user.\n\
             Question: merge into main?\nThe owner answers in the app; do not answer for them."
        );
        let idle = crate::session_tools::summary::idle_background_text(
            "секретная задача",
            SENDER,
            "codex",
            "gpt-secret",
            12,
            1,
        );
        let kept = scrubbed_text(KIND_EVENT, &idle, SENDER);
        assert!(kept.starts_with(
            "[canterlot] Report from a delegate session that was later deleted. \
             It was an agent, not the owner.\nThe session has been idle for 12 min with 1 background job"
        ));
        assert!(kept.contains("This is not a result"));
        assert!(!kept.contains("секретная") && !kept.contains(&SENDER.to_string()));
    }

    #[test]
    fn an_event_without_a_recognisable_header_becomes_the_fact() {
        let other = Uuid::from_u128(7);
        for text in [
            format!("[canterlot] Session {SENDER} was deleted."),
            format!(
                "[canterlot] Session \"секретная задача\" (id {SENDER}) was accepted: it is stopped and archived."
            ),
            format!(
                "[canterlot] Session \"секретная задача\" (id {SENDER}, codex, model m) is now idle."
            ),
            format!(
                "[canterlot] Session \"чужая\" (id {other}, codex, model m) is now idle.\nFinal answer:\nтекст"
            ),
            "итог без заголовка\n\nтело".to_owned(),
        ] {
            assert_eq!(
                scrubbed_text(KIND_EVENT, &text, SENDER),
                HEARD_FROM_DELETED,
                "{text}"
            );
        }
    }

    #[test]
    fn a_message_and_a_brief_keep_their_body_as_before() {
        let message = message_text(&sender(), "тело сообщения");
        assert_eq!(
            scrubbed_text("message", &message, SENDER),
            "[canterlot] Message from a session that was deleted. \
             This is another agent, not the owner.\n\nтело сообщения"
        );
        let brief = spawn_text(&sender(), Some("canterlot/1"), "тело брифа");
        assert_eq!(
            scrubbed_text("spawn", &brief, SENDER),
            "[canterlot] You were started by a session that was deleted. \
             It was an agent, not the owner.\n\nтело брифа"
        );
        assert_eq!(
            scrubbed_text("message", "без тела", SENDER),
            HEARD_FROM_DELETED
        );
    }
}
