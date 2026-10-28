use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use chrono::{DateTime, Utc};
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use uuid::Uuid;

use crate::{
    bus::EventBus,
    events::{self, ReadAfter, Scope, WireEvent},
    http::{AppState, auth::Owner},
};

const MAX_MERGED_BYTES: usize = 64 * 1024;
const CONTROL_BYTES: usize = 1024;
const CLOSE_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq)]
pub struct ScopeError;

pub fn resolve_scope(owner_id: Uuid, wire: &str) -> Result<Scope, ScopeError> {
    match wire {
        "conversations" => Ok(Scope::Conversations { owner_id }),
        "machines" => Ok(Scope::Machines { owner_id }),
        _ => wire
            .strip_prefix("conversation:")
            .filter(|id| id.len() == 36)
            .and_then(|id| Uuid::parse_str(id).ok())
            .map(|id| Scope::Conversation { id })
            .ok_or(ScopeError),
    }
}

pub async fn authorize(
    conn: &mut PgConnection,
    owner_id: Uuid,
    scope: &Scope,
) -> Result<bool, sqlx::Error> {
    match scope {
        Scope::Conversations { owner_id: id } | Scope::Machines { owner_id: id } => {
            Ok(*id == owner_id)
        }
        Scope::Conversation { id } => {
            sqlx::query_scalar(
                "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
            )
            .bind(id)
            .bind(owner_id)
            .fetch_one(conn)
            .await
        }
    }
}

#[derive(Debug)]
pub struct Outgoing {
    pub scope_seq_from: i64,
    pub committed_at_from: DateTime<Utc>,
    pub event: WireEvent,
}
impl Outgoing {
    fn single(event: WireEvent) -> Self {
        Self {
            scope_seq_from: event.scope_seq_from.parse().expect("projected sequence"),
            committed_at_from: DateTime::parse_from_rfc3339(&event.committed_at_from)
                .expect("projected timestamp")
                .with_timezone(&Utc),
            event,
        }
    }
}

pub struct Merger {
    window: Duration,
    id: String,
    frame_budget: usize,
    pending: Option<(Outgoing, Instant)>,
}
impl Merger {
    pub fn new(window: Duration, id: &str, data_budget: usize) -> Self {
        Self {
            window: window.min(Duration::from_millis(25)),
            id: id.to_owned(),
            frame_budget: data_budget.min(MAX_MERGED_BYTES),
            pending: None,
        }
    }
    pub fn push(&mut self, event: WireEvent, now: Instant) -> Vec<Outgoing> {
        let mut out = self.flush_due(now);
        if let Some((pending, _)) = &mut self.pending {
            let last = &mut pending.event;
            let consecutive = last.scope == event.scope
                && last
                    .scope_seq
                    .parse::<i64>()
                    .ok()
                    .and_then(|n| n.checked_add(1))
                    == event.scope_seq_from.parse::<i64>().ok();
            let hidden = last.kind == "hidden" && event.kind == "hidden";
            let delta = last.kind == "block_delta"
                && event.kind == "block_delta"
                && last.conversation_id == event.conversation_id
                && last.execution_id == event.execution_id
                && last.payload["block_id"].is_string()
                && last.payload["block_id"] == event.payload["block_id"]
                && last.payload["rev"].as_u64().is_some()
                && last.payload["rev"] == event.payload["base_rev"]
                && event.payload["rev"].as_u64() > event.payload["base_rev"].as_u64()
                && last.payload["append"]
                    .as_str()
                    .zip(event.payload["append"].as_str())
                    .is_some_and(|(a, b)| a.len() + b.len() <= MAX_MERGED_BYTES);
            if consecutive && (hidden || delta) {
                let mut merged = last.clone();
                if delta {
                    let mut text = last.payload["append"]
                        .as_str()
                        .expect("delta text")
                        .to_owned();
                    text.push_str(event.payload["append"].as_str().expect("delta text"));
                    merged.payload["append"] = json!(text);
                    merged.payload["rev"] = event.payload["rev"].clone();
                }
                merged.scope_seq = event.scope_seq.clone();
                merged.committed_at = event.committed_at.clone();
                if event_frame(&self.id, &merged).len() <= self.frame_budget {
                    *last = merged;
                    return out;
                }
            }
        }
        out.extend(self.flush());
        let mergeable = event.kind == "hidden"
            || event.kind == "block_delta"
                && event.payload["append"]
                    .as_str()
                    .is_some_and(|s| s.len() <= MAX_MERGED_BYTES)
                && event_frame(&self.id, &event).len() <= self.frame_budget;
        if mergeable {
            self.pending = Some((Outgoing::single(event), now + self.window));
        } else {
            out.push(Outgoing::single(event));
        }
        out
    }
    pub fn flush_due(&mut self, now: Instant) -> Vec<Outgoing> {
        if self.next_deadline().is_some_and(|at| now >= at) {
            self.flush()
        } else {
            Vec::new()
        }
    }
    pub fn flush(&mut self) -> Vec<Outgoing> {
        self.pending
            .take()
            .map(|(event, _)| event)
            .into_iter()
            .collect()
    }
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|(_, at)| *at)
    }
}

fn event_frame(id: &str, event: &WireEvent) -> String {
    json!({"op":"event","id":id,"event":event}).to_string()
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum ClientOp {
    Subscribe {
        id: String,
        scope: String,
        #[serde(default, deserialize_with = "after_field")]
        after: Option<String>,
    },
    Unsubscribe {
        id: String,
    },
    Ping {
        t: serde_json::Number,
    },
}

fn after_field<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}

struct Delivery {
    active: AtomicBool,
    sent: AtomicI64,
}
struct Queued {
    frame: Message,
    delivery: Option<(Arc<Delivery>, i64)>,
    _messages: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}
struct WriterQueue {
    tx: mpsc::Sender<Queued>,
    messages: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
    progress: Notify,
    last_progress: Mutex<Instant>,
    stall_timeout: Duration,
    byte_limit: usize,
    #[cfg(feature = "fault-injection")]
    write_pause: Mutex<Option<oneshot::Sender<oneshot::Sender<()>>>>,
}
#[derive(Debug)]
enum PumpError {
    Disconnected,
    Stalled,
    Database,
    Oversized,
}
impl From<sqlx::Error> for PumpError {
    fn from(error: sqlx::Error) -> Self {
        tracing::warn!(kind="ws_database_error", error=%crate::dberr::describe(&error), "websocket read failed");
        Self::Database
    }
}
impl WriterQueue {
    fn data_budget(&self) -> usize {
        self.byte_limit.saturating_sub(CONTROL_BYTES)
    }
    fn batch_capacity(&self) -> usize {
        self.messages
            .available_permits()
            .saturating_sub(1)
            .min(
                self.bytes
                    .available_permits()
                    .saturating_sub(CONTROL_BYTES)
                    .div_ceil(MAX_MERGED_BYTES),
            )
            .min(256)
    }
    async fn capacity(&self, len: usize, data: bool) -> Result<(), PumpError> {
        let since = Instant::now();
        loop {
            let changed = self.progress.notified();
            if self.messages.available_permits() > usize::from(data)
                && self.bytes.available_permits() >= len + if data { CONTROL_BYTES } else { 0 }
            {
                return Ok(());
            }
            let last = *self.last_progress.lock().unwrap_or_else(|e| e.into_inner());
            let deadline = last.max(since) + self.stall_timeout;
            if Instant::now() >= deadline {
                return Err(PumpError::Stalled);
            }
            tokio::select! {
                () = changed => {},
                () = tokio::time::sleep_until(deadline.into()) => {},
            }
        }
    }
    async fn enqueue(
        &self,
        frame: Message,
        delivery: Option<(Arc<Delivery>, i64)>,
    ) -> Result<(), PumpError> {
        let data = delivery.is_some();
        let len = match &frame {
            Message::Text(s) => s.len(),
            Message::Binary(b) | Message::Ping(b) | Message::Pong(b) => b.len(),
            Message::Close(_) => 0,
        };
        if len
            > self
                .byte_limit
                .saturating_sub(if data { CONTROL_BYTES } else { 0 })
        {
            return Err(PumpError::Oversized);
        }
        self.capacity(len, data).await?;
        let messages = self
            .messages
            .clone()
            .try_acquire_owned()
            .map_err(|_| PumpError::Disconnected)?;
        let bytes = self
            .bytes
            .clone()
            .try_acquire_many_owned(len as u32)
            .map_err(|_| PumpError::Disconnected)?;
        self.tx
            .send(Queued {
                frame,
                delivery,
                _messages: messages,
                _bytes: bytes,
            })
            .await
            .map_err(|_| PumpError::Disconnected)
    }
    async fn control(&self, value: Value) -> Result<(), PumpError> {
        self.enqueue(Message::Text(value.to_string().into()), None)
            .await
    }
    fn progressed(&self) {
        *self.last_progress.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.progress.notify_one();
    }
}

struct Subscription {
    scope: Scope,
    cursor: i64,
    merger: Merger,
    delivery: Arc<Delivery>,
    next_read: Instant,
    noticed_seq: i64,
    visibility_until: Instant,
    backoff: Duration,
}
struct Pump {
    state: AppState,
    owner: Owner,
    queue: Arc<WriterQueue>,
    subscriptions: BTreeMap<String, Subscription>,
    turn: usize,
}
impl Pump {
    async fn output(&self, id: &str, outgoing: Vec<Outgoing>) -> Result<(), PumpError> {
        let delivery = &self.subscriptions[id].delivery;
        for mut event in outgoing {
            let cursor = event.event.scope_seq.parse().expect("projected sequence");
            let mut frame = event_frame(id, &event.event);
            if frame.len() > self.queue.data_budget() {
                event.event.payload = json!({
                    "kind": event.event.kind,
                    "href": format!("/api/v1/events?scope={}&after={}&limit=1", event.event.scope, cursor - 1),
                });
                event.event.kind = "oversized".into();
                frame = event_frame(id, &event.event);
            }
            self.queue
                .enqueue(
                    Message::Text(frame.into()),
                    Some((delivery.clone(), cursor)),
                )
                .await?;
        }
        Ok(())
    }
    async fn error(&self, id: &str, code: &str) -> Result<(), PumpError> {
        self.queue
            .control(json!({"op":"error","id":id,"code":code}))
            .await
    }
    async fn command(&mut self, text: &str) -> Result<(), PumpError> {
        let op = match serde_json::from_str::<ClientOp>(text) {
            Ok(op) => op,
            Err(_) => {
                let id = serde_json::from_str::<Value>(text)
                    .ok()
                    .and_then(|v| {
                        v["id"]
                            .as_str()
                            .filter(|s| s.chars().count() <= 128)
                            .map(str::to_owned)
                    })
                    .unwrap_or_default();
                return self.error(&id, "bad_message").await;
            }
        };
        match op {
            ClientOp::Ping { t } => self.queue.control(json!({"op":"pong","t":t})).await,
            ClientOp::Unsubscribe { id } => {
                if id.is_empty() || id.chars().count() > 128 {
                    return self.error("", "bad_message").await;
                }
                if let Some(sub) = self.subscriptions.remove(&id) {
                    sub.delivery.active.store(false, Ordering::SeqCst);
                }
                self.queue
                    .control(json!({"op":"unsubscribed","id":id}))
                    .await
            }
            ClientOp::Subscribe { id, scope, after } => {
                if id.is_empty() || id.chars().count() > 128 || self.subscriptions.contains_key(&id)
                {
                    return self
                        .error(
                            if id.chars().count() <= 128 { &id } else { "" },
                            "bad_message",
                        )
                        .await;
                }
                let scope = match resolve_scope(self.owner.owner_id, &scope) {
                    Ok(scope) => scope,
                    Err(_) => return self.error(&id, "bad_scope").await,
                };
                let mut conn = self.state.pool.acquire().await?;
                if !authorize(&mut conn, self.owner.owner_id, &scope).await? {
                    drop(conn);
                    return self.error(&id, "forbidden").await;
                }
                let head = events::boundary(&mut conn, &scope).await?;
                drop(conn);
                let from_head = after.is_none();
                let after = match after {
                    None => head,
                    Some(s) => match super::http::history::decimal(&s) {
                        Ok(n) => n,
                        Err(_) => return self.error(&id, "bad_message").await,
                    },
                };
                if self.subscriptions.len() >= self.state.http.ws.max_subscriptions.min(16) {
                    return self.error(&id, "too_many_subscriptions").await;
                }
                let head = if from_head {
                    head
                } else {
                    let range = events::range(&self.state.pool, &scope, after, 1).await?;
                    if let ReadAfter::ResyncRequired {
                        oldest_available_seq,
                    } = range.result
                    {
                        return self.queue.control(json!({"op":"resync_required","id":id,"reason":if range.cursor_ahead {"cursor_ahead"} else {"cursor_too_old"},"oldest_available_seq":oldest_available_seq.to_string()})).await;
                    }
                    range.head
                };
                self.queue.control(json!({"op":"subscribed","id":id,"scope":events::wire_scope(&scope),"head":head.to_string()})).await?;
                let now = Instant::now();
                let merger = Merger::new(
                    self.state.http.ws.delta_window,
                    &id,
                    self.queue.data_budget(),
                );
                self.subscriptions.insert(
                    id,
                    Subscription {
                        scope,
                        cursor: after,
                        merger,
                        delivery: Arc::new(Delivery {
                            active: AtomicBool::new(true),
                            sent: AtomicI64::new(after),
                        }),
                        next_read: now,
                        noticed_seq: 0,
                        visibility_until: now,
                        backoff: Duration::from_millis(5),
                    },
                );
                Ok(())
            }
        }
    }
    async fn tick(&mut self) -> Result<(), PumpError> {
        let ids: Vec<_> = self.subscriptions.keys().cloned().collect();
        if ids.is_empty() {
            return Ok(());
        }
        let id = &ids[self.turn % ids.len()];
        self.turn = self.turn.wrapping_add(1);
        let now = Instant::now();
        let outgoing = self
            .subscriptions
            .get_mut(id)
            .expect("subscription")
            .merger
            .flush_due(now);
        self.output(id, outgoing).await?;
        if now < self.subscriptions[id].next_read {
            return Ok(());
        }
        self.queue.capacity(1, true).await?;
        let n = self.queue.batch_capacity().max(1) as i64;
        let sub = &self.subscriptions[id];
        let result = events::read_after(&self.state.pool, &sub.scope, sub.cursor, n).await?;
        match result {
            ReadAfter::ResyncRequired {
                oldest_available_seq,
            } => {
                let sub = self.subscriptions.get_mut(id).expect("subscription");
                let pending = sub.merger.flush();
                self.output(id, pending).await?;
                self.subscriptions.remove(id);
                self.queue.control(json!({"op":"resync_required","id":id,"reason":"cursor_too_old","oldest_available_seq":oldest_available_seq.to_string()})).await?;
            }
            ReadAfter::Events(batch) => {
                let sub = self.subscriptions.get_mut(id).expect("subscription");
                let now = Instant::now();
                if let Some(last) = batch.last() {
                    sub.cursor = last.scope_seq;
                    sub.next_read = now;
                    sub.backoff = Duration::from_millis(5);
                } else if sub.cursor < sub.noticed_seq && now < sub.visibility_until {
                    sub.next_read = now + sub.backoff;
                    sub.backoff = (sub.backoff * 2).min(Duration::from_millis(100));
                } else {
                    sub.next_read = now + self.state.http.ws.poll_interval;
                }
                for event in events::project_for_client(batch) {
                    let out = self
                        .subscriptions
                        .get_mut(id)
                        .expect("subscription")
                        .merger
                        .push(event, Instant::now());
                    self.output(id, out).await?;
                }
            }
        }
        Ok(())
    }
    async fn run(&mut self, mut input: SplitStream<WebSocket>) -> Result<(), PumpError> {
        let mut notices = EventBus::local().subscribe();
        loop {
            let deadline = self
                .subscriptions
                .values()
                .flat_map(|s| [Some(s.next_read), s.merger.next_deadline()])
                .flatten()
                .min();
            tokio::select! {
                biased;
                message=input.next() => match message {
                    Some(Ok(Message::Text(text))) => self.command(&text).await?,
                    Some(Ok(Message::Ping(bytes))) => self.queue.enqueue(Message::Pong(bytes),None).await?,
                    Some(Ok(Message::Pong(_))) => {},
                    Some(Ok(Message::Binary(_))) => self.error("","bad_message").await?,
                    _ => return Err(PumpError::Disconnected),
                },
                notice=notices.recv() => {
                    let now=Instant::now();
                    match notice {
                        Ok(n) => for sub in self.subscriptions.values_mut().filter(|s| n.scope=="*" || s.scope.key()==n.scope) {
                            sub.next_read=now; sub.noticed_seq=sub.noticed_seq.max(n.scope_seq);
                            sub.visibility_until=now+self.state.http.ws.poll_interval;
                            sub.backoff=Duration::from_millis(5);
                        },
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => for sub in self.subscriptions.values_mut() {sub.next_read=now;},
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return Err(PumpError::Disconnected),
                    }
                },
                ()=async {match deadline {Some(at)=>tokio::time::sleep_until(at.into()).await,None=>std::future::pending().await}} => self.tick().await?,
            }
        }
    }
}

enum End {
    Disconnected,
    Failure,
    Reauth,
    Shutdown,
    Lagged(Vec<(String, Arc<Delivery>)>),
}
async fn writer(
    mut sink: SplitSink<WebSocket, Message>,
    mut input: mpsc::Receiver<Queued>,
    mut ending: oneshot::Receiver<End>,
    queue: Arc<WriterQueue>,
) {
    let end = loop {
        let item = tokio::select! {
            biased;
            end=&mut ending => break end.unwrap_or(End::Disconnected),
            item=input.recv() => match item {Some(item)=>item,None=>break End::Disconnected},
        };
        if item
            .delivery
            .as_ref()
            .is_none_or(|(delivery, _)| delivery.active.load(Ordering::SeqCst))
        {
            #[cfg(feature = "fault-injection")]
            if item.delivery.is_some() {
                let pause = queue
                    .write_pause
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                if let Some(pause) = pause {
                    let (resume, resumed) = oneshot::channel();
                    let _ = pause.send(resume);
                    let _ = resumed.await;
                }
            }
            if sink.send(item.frame.clone()).await.is_err() {
                return;
            }
            if let Some((delivery, cursor)) = &item.delivery {
                delivery.sent.store(*cursor, Ordering::SeqCst);
            }
            queue.progressed();
        }
        drop(item);
        queue.progress.notify_one();
    };
    input.close();
    while input.try_recv().is_ok() {}
    let code = match end {
        End::Reauth => {
            if sink
                .send(Message::Text(
                    json!({"op":"close","reason":"reauth"}).to_string().into(),
                ))
                .await
                .is_err()
            {
                return;
            }
            4401
        }
        End::Lagged(subscriptions) => {
            for (id, delivery) in subscriptions {
                let frame = json!({"op":"lagged","id":id,"cursor":delivery.sent.load(Ordering::SeqCst).to_string()});
                if sink
                    .send(Message::Text(frame.to_string().into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            1008
        }
        End::Shutdown => 1001,
        End::Failure => 1011,
        End::Disconnected => 1000,
    };
    let _ = sink
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: "".into(),
        })))
        .await;
}

pub async fn handle_socket(state: AppState, owner: Owner, socket: WebSocket) {
    let config = &state.http.ws;
    let message_limit = config.queue_messages.clamp(2, 256);
    let byte_limit = config.queue_bytes.min(1024 * 1024);
    let (tx, rx) = mpsc::channel(message_limit);
    let queue = Arc::new(WriterQueue {
        tx,
        messages: Arc::new(Semaphore::new(message_limit)),
        bytes: Arc::new(Semaphore::new(byte_limit)),
        progress: Notify::new(),
        last_progress: Mutex::new(Instant::now()),
        stall_timeout: config.stall_timeout,
        byte_limit,
        #[cfg(feature = "fault-injection")]
        write_pause: Mutex::new(
            state
                .ws_faults
                .take_pause(crate::fault_injection::Point::WsBeforeDataSend),
        ),
    });
    #[cfg(feature = "fault-injection")]
    let faults = state.ws_faults.clone();
    let age = config.max_connection_age;
    let shutdown = state.shutdown.clone();
    let mut pump = Pump {
        state,
        owner,
        queue: queue.clone(),
        subscriptions: BTreeMap::new(),
        turn: 0,
    };
    let (sink, input) = socket.split();
    let (end_tx, end_rx) = oneshot::channel();
    let mut writer = std::pin::pin!(writer(sink, rx, end_rx, queue));
    let result = tokio::select! {
        biased;
        ()=shutdown.cancelled()=>End::Shutdown,
        ()=tokio::time::sleep(age)=>End::Reauth,
        ()=&mut writer=>return,
        result=pump.run(input)=>match result {
            Err(PumpError::Stalled)=>End::Lagged(pump.subscriptions.iter().map(|(id,s)|(id.clone(),s.delivery.clone())).collect()),
            Err(PumpError::Database | PumpError::Oversized)=>End::Failure,
            _=>End::Disconnected,
        },
    };
    #[cfg(feature = "fault-injection")]
    if matches!(&result, End::Lagged(_)) {
        faults.pause(crate::fault_injection::Point::WsStalled).await;
    }
    let _ = end_tx.send(result);
    let _ = tokio::time::timeout(CLOSE_GRACE, &mut writer).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue(
        messages: usize,
        bytes: usize,
        timeout: Duration,
    ) -> (Arc<WriterQueue>, mpsc::Receiver<Queued>) {
        let (tx, rx) = mpsc::channel(messages);
        (
            Arc::new(WriterQueue {
                tx,
                messages: Arc::new(Semaphore::new(messages)),
                bytes: Arc::new(Semaphore::new(bytes)),
                progress: Notify::new(),
                last_progress: Mutex::new(Instant::now()),
                stall_timeout: timeout,
                byte_limit: bytes,
                #[cfg(feature = "fault-injection")]
                write_pause: Mutex::new(None),
            }),
            rx,
        )
    }
    fn delivery() -> Option<(Arc<Delivery>, i64)> {
        Some((
            Arc::new(Delivery {
                active: AtomicBool::new(true),
                sent: AtomicI64::new(0),
            }),
            1,
        ))
    }
    #[tokio::test]
    async fn writer_queue_counts_in_flight_and_reserves_control_capacity() {
        let (q, mut rx) = queue(3, 4096, Duration::from_secs(1));
        for _ in 0..2 {
            q.enqueue(Message::Text("x".repeat(1000).into()), delivery())
                .await
                .unwrap();
        }
        let in_flight = rx.recv().await.unwrap();
        assert_eq!(q.messages.available_permits(), 1);
        assert_eq!(q.bytes.available_permits(), 2096);
        assert_eq!(q.batch_capacity(), 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), q.capacity(1, true))
                .await
                .is_err()
        );
        q.control(json!({"op":"pong","t":1})).await.unwrap();
        assert_eq!(q.messages.available_permits(), 0);
        assert!(q.bytes.available_permits() >= CONTROL_BYTES);
        drop(in_flight);
        assert_eq!(q.messages.available_permits(), 1);
        drop(rx);
        assert_eq!(q.messages.available_permits(), 3);
        assert_eq!(q.bytes.available_permits(), 4096);
    }
    #[tokio::test]
    async fn writer_queue_byte_bound_is_independent_of_message_bound() {
        let (q, _rx) = queue(256, 4096, Duration::from_secs(1));
        q.enqueue(Message::Text("x".repeat(3000).into()), delivery())
            .await
            .unwrap();
        assert_eq!(q.messages.available_permits(), 255);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), q.capacity(100, true))
                .await
                .is_err()
        );
        q.control(json!({"op":"pong","t":1})).await.unwrap();
        assert!(q.bytes.available_permits() > 0);
        assert!(matches!(
            q.enqueue(Message::Text("x".repeat(4096).into()), delivery())
                .await,
            Err(PumpError::Oversized)
        ));
    }
    #[tokio::test]
    async fn writer_progress_restarts_the_stall_deadline() {
        let (q, mut rx) = queue(2, 4096, Duration::from_millis(200));
        q.enqueue(Message::Text("x".into()), delivery())
            .await
            .unwrap();
        let waiting = q.capacity(1, true);
        let progress = async {
            for _ in 0..2 {
                tokio::time::sleep(Duration::from_millis(80)).await;
                q.progressed();
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
            drop(rx.recv().await.unwrap());
            q.progressed();
        };
        let (result, ()) = tokio::join!(waiting, progress);
        assert!(result.is_ok());
        q.enqueue(Message::Text("x".into()), delivery())
            .await
            .unwrap();
        assert!(matches!(q.capacity(1, true).await, Err(PumpError::Stalled)));
    }
}
