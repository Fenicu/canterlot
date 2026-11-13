//! Клиент службы `canterlot-speech` по unix-сокету: `health` и начало диктовки.
//! Формат кадров — §1.1 `docs/design/dictation.md`.

use std::{path::Path, time::Duration};

use serde::Deserialize;
use tokio::net::UnixStream;

use super::frame::{self, Frame, ReadLimits};

/// Ответ на `{"type":"health"}`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Health {
    /// Последняя загрузка движка удалась (до следующей удачной).
    pub ok: bool,
    #[serde(default)]
    pub engine: Option<String>,
    /// Известен только у тёплого движка.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Идёт диктовка.
    #[serde(default)]
    pub busy: bool,
}

/// Служба недоступна: нет сокета, отказ соединения, срок, чужой протокол.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoService;

pub async fn connect(socket: &Path, timeout: Duration) -> Result<UnixStream, NoService> {
    tokio::time::timeout(timeout, UnixStream::connect(socket))
        .await
        .map_err(|_| NoService)?
        .map_err(|_| NoService)
}

pub async fn health(socket: &Path, timeout: Duration) -> Result<Health, NoService> {
    let mut stream = connect(socket, timeout).await?;
    let request = serde_json::json!({"type": "health"});
    tokio::time::timeout(timeout, frame::write_json(&mut stream, &request))
        .await
        .map_err(|_| NoService)?
        .map_err(|_| NoService)?;
    let frame = tokio::time::timeout(
        timeout,
        frame::read_frame(&mut stream, ReadLimits::PROTOCOL),
    )
    .await
    .map_err(|_| NoService)?
    .map_err(|_| NoService)?;
    let Frame::Json(bytes) = frame else {
        return Err(NoService);
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| NoService)?;
    if value["type"] != "health" {
        return Err(NoService);
    }
    serde_json::from_value(value).map_err(|_| NoService)
}
