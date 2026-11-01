//! Откуда пришёл ввод другой сессии: объект `inputs.origin` и видимая модели первая строка.

use std::fmt;

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::attachments::blobstore::hex;

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
