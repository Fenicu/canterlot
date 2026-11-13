//! Пересылка живой диктовки: PWA ⇄ хаб ⇄ `canterlot-speech`.
//!
//! Один цикл `select!`: сообщения клиента, кадры службы, сроки, замена диктовки,
//! отзыв сеанса и остановка хаба. Любой конец идёт через `cleanup`: слот
//! освобождается поколением, соединение со службой закрывается (это `cancel`),
//! клиент получает итог или ошибку и код закрытия, пишется одна строка журнала.

use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc;

use crate::{
    dictation::{
        AUDIO_BYTES_PER_SECOND, MAX_AUDIO_BYTES, MAX_TEXT_BYTES, Slot, Timers, client,
        dictionary::Dictionary,
        frame::{self, Frame, ReadLimits},
        terms,
    },
    http::{AppState, auth::Owner},
};

/// Предел входящего сообщения WebSocket: текст проверяется вручную (4096), звук —
/// 8192; запас нужен, чтобы превышение давало `bad_audio`, а не обрыв 1009.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Предел исходящего сообщения: служба отдаёт JSON до 262 144 байт, словарь замен
/// его расширяет; 1 МиБ хватает здоровому клиенту, не превращая буфер в память.
pub const MAX_OUTBOUND_BYTES: usize = 1024 * 1024;

/// Конец диктовки: единственный путь уборки.
enum End {
    Final { text: String, reason: String },
    Error(&'static str),
    Cancel,
    ClientGone,
    ClientStalled,
    Replaced,
    Revoked,
    Shutdown,
}

impl End {
    fn outcome(&self) -> &str {
        match self {
            End::Final { reason, .. } => reason,
            End::Error(code) => code,
            End::Cancel => "cancel",
            End::ClientGone => "client_gone",
            End::ClientStalled => "client_stalled",
            End::Replaced => "replaced",
            End::Revoked => "revoked",
            End::Shutdown => "shutdown",
        }
    }
}

struct Relay {
    state: AppState,
    session_id: uuid::Uuid,
    slot: Slot,
    ws: futures_util::stream::SplitSink<WebSocket, Message>,
    input: futures_util::stream::SplitStream<WebSocket>,
    speech: OwnedWriteHalf,
    frames: mpsc::Receiver<Result<Frame, frame::FrameError>>,
    reader: tokio::task::JoinHandle<()>,
    dictionary: Dictionary,
    timers: Timers,
    started: Instant,
    audio_bytes: u64,
    frames_sent: u64,
    seq: u64,
    mode: Option<String>,
    listening: bool,
    last_audio: Instant,
    finish_at: Option<Instant>,
    speech_write_dead: bool,
    /// Последнее обновление текста, ещё не отправленное клиенту.
    pending_text: Option<(String, String)>,
}

/// Апгрейд WebSocket `/api/v1/dictation`.
pub fn upgrade(ws: WebSocketUpgrade, state: AppState, owner: Owner) -> axum::response::Response {
    let token = state.ws_tasks.token();
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_OUTBOUND_BYTES)
        .on_upgrade(move |socket| async move {
            let _token = token;
            handle_socket(state, owner, socket).await;
        })
}

async fn handle_socket(state: AppState, owner: Owner, socket: WebSocket) {
    let (mut ws, mut input) = socket.split();
    let timers = (*state.dictation_timers).clone();

    let first = tokio::time::timeout(timers.first_message, input.next()).await;
    let Some(Ok(Message::Text(text))) = first.ok().flatten() else {
        fail(&mut ws, &timers, "bad_request").await;
        return;
    };
    if text.len() > MAX_TEXT_BYTES {
        fail(&mut ws, &timers, "bad_request").await;
        return;
    }
    let Ok(start) = serde_json::from_str::<Value>(&text) else {
        fail(&mut ws, &timers, "bad_request").await;
        return;
    };
    if start["type"] != "start" {
        fail(&mut ws, &timers, "bad_request").await;
        return;
    }
    let client = start["client"].as_str().unwrap_or_default().to_owned();
    if client.is_empty() || client.chars().count() > 64 {
        fail(&mut ws, &timers, "bad_request").await;
        return;
    }
    if start["sample_rate"].as_u64() != Some(16_000) || start["format"] != "s16le" {
        fail(&mut ws, &timers, "bad_audio").await;
        return;
    }
    // Все сроки диктовки считаются от `start` клиента, а не от соединения со службой.
    let started = Instant::now();
    if !state.dictation.enabled {
        tracing::info!(
            kind = "speech_unavailable",
            reason = "disabled",
            "dictation is disabled in the hub configuration"
        );
        fail(&mut ws, &timers, "speech_unavailable").await;
        return;
    }
    let slot = match state.dictation_state.acquire(owner.session_id) {
        Ok(slot) => slot,
        Err(_) => {
            fail(&mut ws, &timers, "busy").await;
            return;
        }
    };
    let dictionary = match snapshot_terms(&state, owner.owner_id).await {
        Ok(terms) => Dictionary::new(&terms),
        Err(()) => {
            state.dictation_state.release(slot.generation);
            fail(&mut ws, &timers, "engine_failed").await;
            return;
        }
    };
    let mut speech = match client::connect(&state.dictation.socket, timers.speech_connect).await {
        Ok(stream) => stream,
        Err(client::NoService) => {
            tracing::info!(
                kind = "speech_unavailable",
                socket = %state.dictation.socket.display(),
                "speech service is not reachable"
            );
            state.dictation_state.release(slot.generation);
            fail(&mut ws, &timers, "speech_unavailable").await;
            return;
        }
    };
    let start_message = json!({
        "type": "start",
        "protocol": 1,
        "sample_rate": 16_000,
        "format": "s16le",
        "client": format!("hub/{}", env!("CARGO_PKG_VERSION")),
    });
    let started_write = tokio::time::timeout(
        timers.write_stall,
        frame::write_json(&mut speech, &start_message),
    )
    .await;
    if !matches!(started_write, Ok(Ok(()))) {
        tracing::warn!(
            kind = "speech_protocol_error",
            error = "start",
            "speech start failed"
        );
        state.dictation_state.release(slot.generation);
        fail(&mut ws, &timers, "engine_failed").await;
        return;
    }

    let (speech_read, speech_write) = speech.into_split();
    let (frame_tx, frame_rx) = mpsc::channel(8);
    let reader = tokio::spawn(async move {
        let mut speech_read = speech_read;
        loop {
            let frame = frame::read_frame(&mut speech_read, ReadLimits::PROTOCOL).await;
            let failed = frame.is_err();
            if frame_tx.send(frame).await.is_err() || failed {
                return;
            }
        }
    });

    tracing::info!(
        kind = "dictation_started",
        client = %client,
        "dictation started"
    );
    let mut relay = Relay {
        state,
        session_id: owner.session_id,
        slot,
        ws,
        input,
        speech: speech_write,
        frames: frame_rx,
        reader,
        dictionary,
        timers,
        started,
        audio_bytes: 0,
        frames_sent: 0,
        seq: 0,
        mode: None,
        listening: false,
        last_audio: started,
        finish_at: None,
        speech_write_dead: false,
        pending_text: None,
    };
    let end = relay.run().await;
    relay.cleanup(end).await;
}

async fn snapshot_terms(
    state: &AppState,
    owner_id: uuid::Uuid,
) -> Result<Vec<crate::dictation::dictionary::Term>, ()> {
    let mut conn = state.pool.acquire().await.map_err(|_| ())?;
    terms::load(&mut conn, owner_id).await.map_err(|_| ())
}

/// Отказ до начала диктовки: ошибка и закрытие 1000.
async fn fail(
    ws: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    timers: &Timers,
    code: &str,
) {
    let frame = json!({"type": "error", "code": code});
    let _ = tokio::time::timeout(
        timers.write_stall,
        ws.send(Message::Text(frame.to_string().into())),
    )
    .await;
    let _ = tokio::time::timeout(
        Duration::from_secs(1),
        ws.send(Message::Close(Some(CloseFrame {
            code: 1000,
            reason: "".into(),
        }))),
    )
    .await;
}

impl Relay {
    async fn run(&mut self) -> End {
        let mut session_check = tokio::time::interval_at(
            tokio::time::Instant::now() + self.state.http.ws.session_check_interval,
            self.state.http.ws.session_check_interval,
        );
        session_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut notices = crate::bus::EventBus::local().subscribe();

        loop {
            let deadline = self.next_deadline();
            let tick = tokio::select! {
                biased;
                message = self.input.next() => Tick::Client(message),
                frame = self.frames.recv() => Tick::Speech(frame),
                _ = tokio::time::sleep_until(deadline.into()) => Tick::Deadline,
                _ = self.slot.replaced.cancelled() => Tick::Replaced,
                _ = self.state.shutdown.cancelled() => Tick::Shutdown,
                _ = session_check.tick() => Tick::SessionCheck,
                notice = notices.recv() => Tick::Notice(notice),
            };
            let mut end = None;
            match tick {
                Tick::Client(message) => end = self.on_client(message).await,
                Tick::Speech(frame) => end = self.on_speech(frame).await,
                Tick::Deadline => end = self.on_deadline(),
                Tick::Replaced => end = Some(End::Replaced),
                Tick::Shutdown => end = Some(End::Shutdown),
                Tick::SessionCheck => {
                    if !self.session_live().await {
                        end = Some(End::Revoked);
                    }
                }
                Tick::Notice(notice) => {
                    let relevant = notice.as_ref().map_or(true, |notice| {
                        notice.scope == "*" || notice.scope == crate::owner_auth::BUS_SCOPE
                    });
                    if relevant && !self.session_live().await {
                        end = Some(End::Revoked);
                    }
                }
            }
            if !matches!(end, Some(End::ClientGone))
                && let Some(text) = self.pending_text.take()
            {
                self.seq += 1;
                let frame = json!({
                    "type": "text",
                    "committed": text.0,
                    "tentative": text.1,
                    "seq": self.seq,
                });
                if self.oversized(&frame) {
                    end = Some(End::Error("engine_failed"));
                } else if self.send(frame).await.is_err() {
                    end = Some(End::ClientStalled);
                }
            }
            if let Some(end) = end {
                return end;
            }
        }
    }

    /// Пока сеанс устройства жив; сбой базы не считается отзывом.
    async fn session_live(&self) -> bool {
        match crate::owner_auth::sessions::is_live(&self.state.pool, self.session_id).await {
            Ok(live) => live,
            Err(error) => {
                tracing::warn!(
                    kind = "ws_session_check_failed",
                    error = %crate::dberr::describe(&error),
                    "dictation session check failed"
                );
                true
            }
        }
    }

    fn next_deadline(&self) -> Instant {
        let mut deadline = self.started + self.timers.lifetime;
        if !self.listening {
            deadline = deadline.min(self.started + self.timers.start_deadline);
        }
        deadline = match self.finish_at {
            Some(finish) => deadline.min(finish + self.timers.finish_final),
            None => deadline.min(self.last_audio + self.timers.idle),
        };
        deadline
    }

    fn on_deadline(&self) -> Option<End> {
        let now = Instant::now();
        if now >= self.started + self.timers.lifetime {
            return Some(End::Error("lifetime"));
        }
        if !self.listening && now >= self.started + self.timers.start_deadline {
            return Some(End::Error("start_timeout"));
        }
        if let Some(finish) = self.finish_at {
            if now >= finish + self.timers.finish_final {
                return Some(End::Error("engine_failed"));
            }
        } else if now >= self.last_audio + self.timers.idle {
            return Some(End::Error("idle"));
        }
        None
    }

    async fn on_client(&mut self, message: Option<Result<Message, axum::Error>>) -> Option<End> {
        match message {
            Some(Ok(Message::Text(text))) => self.on_text(&text).await,
            Some(Ok(Message::Binary(bytes))) => self.on_audio(&bytes).await,
            Some(Ok(Message::Ping(bytes))) => {
                if tokio::time::timeout(self.timers.write_stall, self.ws.send(Message::Pong(bytes)))
                    .await
                    .is_err()
                {
                    return Some(End::ClientStalled);
                }
                None
            }
            Some(Ok(Message::Pong(_))) => None,
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => Some(End::ClientGone),
        }
    }

    async fn on_text(&mut self, text: &str) -> Option<End> {
        if text.len() > MAX_TEXT_BYTES {
            return Some(End::Error("bad_request"));
        }
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return Some(End::Error("bad_request"));
        };
        match value["type"].as_str() {
            Some("finish") if self.finish_at.is_none() => {
                self.finish_at = Some(Instant::now());
                if self.speech_write_dead {
                    return None;
                }
                let stall = self.timers.write_stall;
                let message = json!({"type": "finish"});
                match tokio::time::timeout(stall, frame::write_json(&mut self.speech, &message))
                    .await
                {
                    Ok(Ok(())) => None,
                    Ok(Err(_)) => {
                        self.speech_write_dead = true;
                        None
                    }
                    Err(_) => {
                        tracing::warn!(
                            kind = "speech_protocol_error",
                            error = "write_stalled",
                            "speech write stalled"
                        );
                        Some(End::Error("engine_failed"))
                    }
                }
            }
            Some("cancel") => Some(End::Cancel),
            _ => Some(End::Error("bad_request")),
        }
    }

    async fn on_audio(&mut self, bytes: &[u8]) -> Option<End> {
        if self.finish_at.is_some() {
            return Some(End::Error("bad_request"));
        }
        if bytes.is_empty() || !bytes.len().is_multiple_of(2) || bytes.len() > MAX_AUDIO_BYTES {
            return Some(End::Error("bad_audio"));
        }
        let received = self.audio_bytes + bytes.len() as u64;
        let audio = Duration::from_millis(received * 1000 / AUDIO_BYTES_PER_SECOND);
        if audio > self.started.elapsed() + self.timers.max_lead {
            return Some(End::Error("bad_audio"));
        }
        self.audio_bytes = received;
        self.frames_sent += 1;
        self.last_audio = Instant::now();
        if self.speech_write_dead {
            // Служба уже закрыла соединение: звук дальше не нужен, ждём итог.
            return None;
        }
        let frame = frame::encode(frame::KIND_AUDIO, bytes);
        let stall = self.timers.write_stall;
        match tokio::time::timeout(stall, self.speech.write_all(&frame)).await {
            Ok(Ok(())) => None,
            Ok(Err(_)) => {
                self.speech_write_dead = true;
                None
            }
            Err(_) => {
                tracing::warn!(
                    kind = "speech_protocol_error",
                    error = "write_stalled",
                    "speech write stalled"
                );
                Some(End::Error("engine_failed"))
            }
        }
    }

    async fn on_speech(&mut self, frame: Option<Result<Frame, frame::FrameError>>) -> Option<End> {
        let mut frame = frame;
        loop {
            let Some(result) = frame else {
                tracing::warn!(
                    kind = "speech_protocol_error",
                    error = "closed",
                    "speech connection closed"
                );
                return Some(End::Error("engine_failed"));
            };
            let json = match result {
                Ok(Frame::Json(bytes)) => bytes,
                Ok(Frame::Audio(_)) => {
                    tracing::warn!(
                        kind = "speech_protocol_error",
                        error = "audio",
                        "speech sent audio"
                    );
                    return Some(End::Error("engine_failed"));
                }
                Err(error) => {
                    tracing::warn!(
                        kind = "speech_protocol_error",
                        error = %error,
                        "speech frame failed"
                    );
                    return Some(End::Error("engine_failed"));
                }
            };
            let Ok(value) = serde_json::from_slice::<Value>(&json) else {
                tracing::warn!(
                    kind = "speech_protocol_error",
                    error = "json",
                    "speech frame is not JSON"
                );
                return Some(End::Error("engine_failed"));
            };
            match value["type"].as_str() {
                Some("state") => {
                    let state = value["state"].as_str().unwrap_or_default();
                    let mode = value["mode"].as_str().map(str::to_owned);
                    match state {
                        "loading" => {
                            if self
                                .send_json(
                                    json!({"type": "state", "state": "loading", "mode": null}),
                                )
                                .await
                                .is_err()
                            {
                                return Some(End::ClientStalled);
                            }
                        }
                        "listening" => {
                            self.listening = true;
                            self.mode = mode.clone();
                            if self
                                .send_json(
                                    json!({"type": "state", "state": "listening", "mode": mode}),
                                )
                                .await
                                .is_err()
                            {
                                return Some(End::ClientStalled);
                            }
                        }
                        other => {
                            tracing::warn!(
                                kind = "speech_protocol_error",
                                error = "state",
                                state = other,
                                "unexpected speech state"
                            );
                            return Some(End::Error("engine_failed"));
                        }
                    }
                }
                Some("text") => {
                    let committed = value["committed"].as_str().unwrap_or_default();
                    let tentative = value["tentative"].as_str().unwrap_or_default();
                    self.pending_text = Some(self.dictionary.apply_update(committed, tentative));
                }
                Some("final") => {
                    let text = self.dictionary.apply(value["text"].as_str().unwrap_or_default());
                    let reason = match value["reason"].as_str() {
                        Some("too_long") => "too_long",
                        _ => "finish",
                    };
                    let frame = json!({"type": "final", "text": &text, "reason": reason});
                    if self.oversized(&frame) {
                        return Some(End::Error("engine_failed"));
                    }
                    return Some(End::Final {
                        text,
                        reason: reason.to_owned(),
                    });
                }
                Some("error") => {
                    let code = value["code"].as_str().unwrap_or_default();
                    return Some(map_speech_error(code));
                }
                Some(other) => {
                    tracing::warn!(
                        kind = "speech_protocol_error",
                        error = "type",
                        kind_name = other,
                        "unexpected speech message"
                    );
                    return Some(End::Error("engine_failed"));
                }
                None => {
                    tracing::warn!(
                        kind = "speech_protocol_error",
                        error = "type",
                        "speech message without type"
                    );
                    return Some(End::Error("engine_failed"));
                }
            }
            // Свежий текст в очереди заменяет предыдущий, пока клиент не получил ни одного.
            frame = self.frames.try_recv().ok();
            frame.as_ref()?;
        }
    }

    /// Расширенный словарём кадр больше предела: конец `engine_failed`. В журнал
    /// идут только размеры — текст остаётся в памяти.
    fn oversized(&self, frame: &Value) -> bool {
        let size = frame.to_string().len();
        if size <= MAX_OUTBOUND_BYTES {
            return false;
        }
        tracing::warn!(
            kind = "speech_protocol_error",
            error = "oversized",
            size,
            limit = MAX_OUTBOUND_BYTES,
            "transformed dictation message exceeds the outbound limit"
        );
        true
    }

    async fn send_json(&mut self, value: Value) -> Result<(), ()> {
        self.send(value).await
    }

    async fn send(&mut self, value: Value) -> Result<(), ()> {
        let stall = self.timers.write_stall;
        let text = value.to_string();
        match tokio::time::timeout(stall, self.ws.send(Message::Text(text.into()))).await {
            Ok(Ok(())) => Ok(()),
            _ => Err(()),
        }
    }

    /// Единственный путь конца: слот, служба, итог клиенту, код закрытия, журнал.
    async fn cleanup(mut self, end: End) {
        let outcome = end.outcome().to_owned();
        let duration_ms = self.started.elapsed().as_millis() as u64;
        let audio_ms = self.audio_bytes * 1000 / AUDIO_BYTES_PER_SECOND;
        self.reader.abort();
        self.frames.close();
        self.state.dictation_state.release(self.slot.generation);
        // Закрытие соединения со службой равносильно `cancel`.
        let _ = tokio::time::timeout(Duration::from_secs(1), self.speech.shutdown()).await;
        tracing::info!(
            kind = "dictation_finished",
            outcome = %outcome,
            audio_ms,
            frames = self.frames_sent,
            duration_ms,
            mode = self.mode.as_deref().unwrap_or(""),
            "dictation finished"
        );
        match end {
            End::Final { text, reason } => {
                let frame = json!({"type": "final", "text": text, "reason": reason});
                let _ = self.send(frame).await;
                self.close(1000).await;
            }
            End::Error(code) => {
                let frame = json!({"type": "error", "code": code});
                let _ = self.send(frame).await;
                self.close(1000).await;
            }
            End::Cancel | End::ClientStalled | End::ClientGone => self.close(1000).await,
            End::Replaced => {
                let frame = json!({"type": "error", "code": "replaced"});
                let _ = self.send(frame).await;
                self.close(1000).await;
            }
            End::Revoked => self.close(4401).await,
            End::Shutdown => self.close(1001).await,
        };
    }

    async fn close(&mut self, code: u16) {
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            self.ws.send(Message::Close(Some(CloseFrame {
                code,
                reason: "".into(),
            }))),
        )
        .await;
    }
}

enum Tick {
    Client(Option<Result<Message, axum::Error>>),
    Speech(Option<Result<Frame, frame::FrameError>>),
    Deadline,
    Replaced,
    Shutdown,
    SessionCheck,
    Notice(Result<std::sync::Arc<crate::bus::Notice>, tokio::sync::broadcast::error::RecvError>),
}

/// Коды службы по контракту; её `bad_request` — ошибка хаба, о ней WARN.
fn map_speech_error(code: &str) -> End {
    match code {
        "engine_unavailable" | "shutting_down" => End::Error("speech_unavailable"),
        "busy" => End::Error("busy"),
        "engine_failed" => {
            tracing::warn!(
                kind = "speech_engine_failed",
                "speech reported engine_failed"
            );
            End::Error("engine_failed")
        }
        "bad_request" => {
            tracing::warn!(
                kind = "speech_protocol_error",
                error = "bad_request",
                "speech rejected a hub message: hub bug"
            );
            End::Error("engine_failed")
        }
        "idle" => End::Error("idle"),
        "lifetime" => End::Error("lifetime"),
        "start_timeout" => End::Error("start_timeout"),
        "bad_audio" => End::Error("bad_audio"),
        other => {
            tracing::warn!(
                kind = "speech_protocol_error",
                error = "code",
                code = other,
                "unknown speech error code"
            );
            End::Error("engine_failed")
        }
    }
}
