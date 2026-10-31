//! Сверка по проекциям. Replay у сервера нет: `/api/event` не умеет `Last-Event-ID`, а `/log`
//! на CLI пуст, поэтому всё, что потеряно, читается из `.../message`, `.../inbox`,
//! `.../permission` и `.../form`. Один путь на разрыв потока и на запуск после падения.
//!
//! Порядок важен: подписка раньше чтения. Событие либо придёт живым, либо уже видно здесь.

use super::http::{Http, HttpError};
use serde_json::Value;
use std::{collections::HashSet, time::Duration};

const READ_WAIT: Duration = Duration::from_secs(10);
const PAGE_SIZE: usize = 100;
/// Сверх этого хвост длинной сессии не читается: потерян может быть только её конец.
const MAX_PAGES: usize = 20;

/// Что известно о сессии после подписки.
pub struct Snapshot {
    pub running: bool,
    pub inbox: Vec<String>,
    pub children: Vec<String>,
    pub permissions: Vec<Value>,
    pub forms: Vec<Value>,
    /// Хвост истории, старое раньше.
    pub messages: Vec<Value>,
}

fn items(reply: &Value) -> Vec<Value> {
    reply
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn kind(item: &Value) -> &str {
    item.get("type").and_then(Value::as_str).unwrap_or_default()
}

fn id_of(item: &Value) -> Option<&str> {
    item.get("id").and_then(Value::as_str)
}

// Последний период и тот, что перед ним. Потерять можно только конец работы, а `idle`
// отделяет периоды: второй с конца — граница, раньше него всё уже было прочитано живьём.
async fn tail(http: &Http, session: &str) -> Result<Vec<Value>, HttpError> {
    let mut messages = Vec::new();
    let mut idle = 0;
    let mut size = PAGE_SIZE;
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    while pages < MAX_PAGES {
        let query = match &cursor {
            Some(cursor) => format!("limit={size}&cursor={cursor}"),
            None => format!("limit={size}&order=desc"),
        };
        let path = format!("/api/session/{session}/message?{query}");
        let reply = match http.get(&path, READ_WAIT).await {
            // Вложения и картинки лежат в сообщениях прямо в base64.
            Err(HttpError::TooLarge) if size > 1 => {
                size = (size / 4).max(1);
                continue;
            }
            other => other?,
        };
        pages += 1;
        let page = items(&reply);
        let last_page = page.len() < size;
        for item in page {
            if kind(&item) == "idle" {
                idle += 1;
                if idle == 2 {
                    messages.reverse();
                    return Ok(messages);
                }
            }
            messages.push(item);
        }
        cursor = reply
            .pointer("/cursor/next")
            .and_then(Value::as_str)
            .filter(|_| !last_page)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    messages.reverse();
    Ok(messages)
}

fn missing_is_empty(reply: Result<Value, HttpError>) -> Result<Value, HttpError> {
    match reply {
        Err(HttpError::Status { status: 404, .. }) => Ok(Value::Null),
        other => other,
    }
}

/// Потомок мог родиться в разрыве: о нём знает только список сессий.
async fn children_of(
    http: &Http,
    session: &str,
    known: Vec<String>,
) -> Result<Vec<String>, HttpError> {
    let reply = http
        .get(
            &format!("/api/session?parentID={session}&limit={PAGE_SIZE}"),
            READ_WAIT,
        )
        .await;
    let mut children: HashSet<String> = known.into_iter().collect();
    children.extend(
        items(&missing_is_empty(reply)?)
            .iter()
            .filter_map(id_of)
            .map(str::to_owned),
    );
    Ok(children.into_iter().collect())
}

pub async fn read(http: &Http, session: &str, known: Vec<String>) -> Result<Snapshot, HttpError> {
    let active = http.get("/api/session/active", READ_WAIT).await?;
    let running = active
        .get("data")
        .is_some_and(|data| data.get(session).is_some());
    let inbox = http
        .get(&format!("/api/session/{session}/inbox"), READ_WAIT)
        .await?;
    let children = children_of(http, session, known).await?;
    let mut permissions = Vec::new();
    let mut forms = Vec::new();
    for owner in std::iter::once(session).chain(children.iter().map(String::as_str)) {
        let path = format!("/api/session/{owner}");
        permissions.extend(items(&missing_is_empty(
            http.get(&format!("{path}/permission"), READ_WAIT).await,
        )?));
        forms.extend(items(&missing_is_empty(
            http.get(&format!("{path}/form"), READ_WAIT).await,
        )?));
    }
    Ok(Snapshot {
        running,
        inbox: items(&inbox)
            .iter()
            .filter_map(id_of)
            .map(str::to_owned)
            .collect(),
        children,
        permissions,
        forms,
        messages: tail(http, session).await?,
    })
}

/// Итог последнего периода, если он закончился после ввода `after`.
pub fn last_outcome<'a>(messages: &'a [Value], after: Option<&str>) -> Option<&'a Value> {
    let from = after
        .and_then(|id| messages.iter().position(|item| id_of(item) == Some(id)))
        .unwrap_or(0);
    messages[from..]
        .iter()
        .rev()
        .find(|item| kind(item) == "idle")
}

pub fn delivered(messages: &[Value], id: &str) -> bool {
    messages.iter().any(|item| id_of(item) == Some(id))
}
