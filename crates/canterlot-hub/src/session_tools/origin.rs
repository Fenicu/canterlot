//! Откуда пришёл ввод другой сессии: объект `inputs.origin` и видимая модели первая строка.
//! Строка с названием отправителя и его идентификатор живут только в этих двух местах и в
//! копиях ввода, поэтому удаление отправителя вычищает их отсюда же.

use std::fmt;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{attachments::blobstore::hex, delivery::ContentBlock, events::Scope};

pub const KIND_EVENT: &str = "event";
const KIND_MESSAGE: &str = "message";
const TITLE_CHARS: usize = 200;

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

/// Что остаётся от ввода, когда его отправитель удалён. Текст сообщения и бриф остаются:
/// это ввод другой сессии, на него она и отвечала. Событие состоит из данных удалённой сессии
/// целиком, поэтому от него остаётся только факт.
fn scrubbed_text(kind: &str, text: &str) -> String {
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
        _ => "[canterlot] A session this conversation heard from was deleted.".to_owned(),
    }
}

fn scrubbed_content(kind: &str, content: &[ContentBlock]) -> Vec<ContentBlock> {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => ContentBlock::Text {
                text: scrubbed_text(kind, text),
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
        let content = json!(scrubbed_content(kind, &row.content));
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
