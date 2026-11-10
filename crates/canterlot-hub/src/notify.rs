//! Пуши владельцу через Gotify.
//!
//! Событие ставит строку в `owner_pushes` в своей же транзакции: вставка дешёвая и ничего
//! не ждёт по сети. Отправляет фоновая служба после commit, одна на базу (`singleton`),
//! с коротким сроком запроса, ограниченным числом повторов и общим пределом в минуту.
//! Ключ `dedupe_key` уникален: повторная оценка того же эпизода пуш не повторяет.
//! Строки настроек нет, пока в `hub.toml` не задан `[notify.gotify]`: тогда вставки нет.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use tokio::{sync::watch, time::Instant};
use uuid::Uuid;

use crate::{
    activity::{Evidence, OWNER_LOST},
    config::{GotifyConfig, GotifyToken},
    session_tools::settle::{SettleFacts, Settled},
};

pub const KIND_WAITING: &str = "waiting";
pub const KIND_TURN_FINISHED: &str = "turn_finished";
pub const KIND_FAILURE: &str = "failure";

pub const PRIORITY_WAITING: u8 = 8;
pub const PRIORITY_FAILURE: u8 = 7;
pub const PRIORITY_TURN_FINISHED: u8 = 5;

/// Итог старше этого не шлётся: перезапуск хаба переоценивает давно осевшие сессии.
const FRESH: TimeDelta = TimeDelta::minutes(10);
/// Пуш, не ушедший за это время (лимит, долгий сбой сервера), теряет смысл.
const STALE: TimeDelta = TimeDelta::minutes(30);
/// Сколько строк забирает один проход.
const BATCH: i64 = 20;
/// Готовые строки живут неделю: ключи итогов с отметкой времени не повторяются и так.
const RETENTION: TimeDelta = TimeDelta::days(7);
const POLL: Duration = Duration::from_secs(1);
const TITLE_CHARS: usize = 60;
const ANSWER_CHARS: usize = 300;
const DETAIL_CHARS: usize = 200;

/// Какие события ставятся в очередь. Хранится в `push_settings`, чтобы постановка
/// в транзакции проектора знала о выключателях без доступа к конфигу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSettings {
    pub waiting: bool,
    pub turn_finished: bool,
    pub failures: bool,
    pub timezone: String,
}

impl From<&GotifyConfig> for PushSettings {
    fn from(config: &GotifyConfig) -> Self {
        Self {
            waiting: config.waiting,
            turn_finished: config.turn_finished,
            failures: config.failures,
            timezone: config.timezone.clone(),
        }
    }
}

/// Старт хаба: включает пуши по конфигу или выключает их и отменяет ожидающие.
pub async fn configure(pool: &PgPool, settings: Option<&PushSettings>) -> Result<(), &'static str> {
    let Some(settings) = settings else {
        let mut tx = crate::db::begin(pool).await.map_err(|_| "db_error")?;
        sqlx::query("delete from push_settings")
            .execute(&mut *tx)
            .await
            .map_err(|_| "db_error")?;
        sqlx::query(
            "update owner_pushes set state='cancelled',updated_at=clock_timestamp()
             where state='pending'",
        )
        .execute(&mut *tx)
        .await
        .map_err(|_| "db_error")?;
        return tx.commit().await.map_err(|_| "db_error");
    };
    let known: bool =
        sqlx::query_scalar("select exists(select 1 from pg_timezone_names where name=$1)")
            .bind(&settings.timezone)
            .fetch_one(pool)
            .await
            .map_err(|_| "db_error")?;
    if !known {
        return Err("notify_timezone_invalid");
    }
    sqlx::query(
        "insert into push_settings(singleton,waiting,turn_finished,failures,timezone)
         values(true,$1,$2,$3,$4)
         on conflict(singleton) do update set waiting=excluded.waiting,
           turn_finished=excluded.turn_finished,failures=excluded.failures,
           timezone=excluded.timezone,updated_at=clock_timestamp()",
    )
    .bind(settings.waiting)
    .bind(settings.turn_finished)
    .bind(settings.failures)
    .bind(&settings.timezone)
    .execute(pool)
    .await
    .map_err(|_| "db_error")?;
    Ok(())
}

/// Вставка строки, если пуши включены, событие выбрано и сессия подходит.
/// `root_only`: итоги и сбои делегатов уже доходят до их оркестратора.
/// `revive_cancelled`: отменённый или брошенный пуш возрождается — так повторно
/// открытый запрос получает пуш, которого у него ещё не было.
async fn enqueue_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    kind: &str,
    dedupe_key: &str,
    payload: &Value,
    root_only: bool,
    revive_cancelled: bool,
) -> Result<(), sqlx::Error> {
    let conflict = if revive_cancelled {
        "on conflict(dedupe_key) do update
           set state='pending', run_at=clock_timestamp(), attempts=0,
               updated_at=clock_timestamp()
           where owner_pushes.state in ('cancelled','dropped')"
    } else {
        "on conflict(dedupe_key) do nothing"
    };
    let sql = format!(
        "insert into owner_pushes(id,conversation_id,kind,dedupe_key,payload,run_at)
         select $1,c.id,$3,$4,$5,clock_timestamp()
         from push_settings s join conversations c on c.id=$2
         where c.trashed_at is null
           and (not $6 or c.parent_conversation_id is null)
           and case $3 when 'waiting' then s.waiting
                       when 'turn_finished' then s.turn_finished
                       else s.failures end
         {conflict}"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(Uuid::now_v7())
        .bind(conversation_id)
        .bind(kind)
        .bind(dedupe_key)
        .bind(payload)
        .bind(root_only)
        .execute(conn)
        .await?;
    Ok(())
}

/// Открыт запрос к владельцу: вопрос, разрешение, план или одобрение запуска.
/// Любая сессия, корневая или делегат. Один пуш на запрос: повторное открытие
/// того же запроса после возврата исполнения не шлёт снова.
pub async fn on_request_opened_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    request_id: Uuid,
) -> Result<(), sqlx::Error> {
    enqueue_in_tx(
        conn,
        conversation_id,
        KIND_WAITING,
        &format!("{KIND_WAITING}:{request_id}"),
        &json!({"request_id": request_id}),
        false,
        true,
    )
    .await
}

/// Осевшее состояние корневой сессии после шлюза автопинка — то же решение, что у
/// подписок. Ключ итога хода — конец хода: дребезг состояния меняет отпечаток
/// оседания, но не сам ход, и второго пуша о нём быть не должно.
pub async fn on_settled_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    state: Settled,
    fingerprint: &str,
    facts: &SettleFacts,
    evidence: &Evidence,
) -> Result<(), sqlx::Error> {
    let (kind, turn) = match state {
        // Ход должен быть и быть недавним: новая пустая сессия и давно осевшая
        // после перезапуска хаба пуша не дают. Свежесть — от последнего события:
        // сессию, которую держала фоновая задача, пуш обязан догнать после её конца.
        Settled::Idle => match facts.last_turn_at {
            Some(turn) if facts.now - turn.max(facts.last_event_at.unwrap_or(turn)) <= FRESH => {
                (KIND_TURN_FINISHED, Some(turn))
            }
            _ => return Ok(()),
        },
        Settled::Failed | Settled::OwnerLost | Settled::Stalled | Settled::QuotaPaused => {
            if facts.now - facts.since > FRESH {
                return Ok(());
            }
            (KIND_FAILURE, None)
        }
        Settled::WaitingForUser | Settled::Stopped => return Ok(()),
    };
    let key = match turn {
        Some(turn) => format!(
            "{kind}:{conversation_id}:{}",
            turn.to_rfc3339_opts(SecondsFormat::Micros, true)
        ),
        None => format!("{kind}:{conversation_id}:{fingerprint}"),
    };
    let evidence = serde_json::to_value(evidence).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    enqueue_in_tx(
        conn,
        conversation_id,
        kind,
        &key,
        &json!({"state": state_name(state), "evidence": evidence}),
        true,
        false,
    )
    .await
}

fn state_name(state: Settled) -> &'static str {
    match state {
        Settled::OwnerLost => OWNER_LOST,
        other => other.as_str(),
    }
}

/// Корзина: ожидающие пуши сессии больше не нужны. Окончательная уборка удаляет строки.
pub async fn cancel_for_conversation_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update owner_pushes set state='cancelled',updated_at=clock_timestamp()
         where conversation_id=$1 and state='pending'",
    )
    .bind(conversation_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Готовое сообщение Gotify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub title: String,
    pub body: String,
    pub priority: u8,
    pub click_url: String,
}

impl Message {
    pub fn to_json(&self) -> Value {
        json!({
            "title": self.title,
            "message": self.body,
            "priority": self.priority,
            "extras": {
                "client::display": {"contentType": "text/plain"},
                "client::notification": {"click": {"url": self.click_url}},
            },
        })
    }
}

/// Итог одного прохода отправки: только числа.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryReport {
    pub sent: usize,
    pub retried: usize,
    pub dropped: usize,
    pub cancelled: usize,
    /// Отложены общим пределом в минуту.
    pub deferred: usize,
}

impl DeliveryReport {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

enum Failure {
    /// Сеть, срок запроса, 5xx, 408, 429: стоит повторить.
    Retry(Option<u16>),
    /// Остальные ответы 4xx: повтор ничего не изменит.
    Fatal(u16),
}

#[derive(sqlx::FromRow)]
struct PushRow {
    id: Uuid,
    conversation_id: Uuid,
    kind: String,
    payload: Value,
    attempts: i32,
    created_at: DateTime<Utc>,
}

/// Отправитель пушей. Ключ живёт только здесь и уходит лишь в заголовке `X-Gotify-Key`.
#[derive(Clone)]
pub struct Sender {
    client: reqwest::Client,
    endpoint: String,
    token: GotifyToken,
    app_url: String,
    max_per_minute: usize,
    backoff: Vec<Duration>,
    window: Arc<Mutex<VecDeque<Instant>>>,
}

impl std::fmt::Debug for Sender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sender")
            .field("endpoint", &self.endpoint)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl Sender {
    pub fn new(config: &GotifyConfig) -> Result<Self, &'static str> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "notify_client_error")?;
        Ok(Self {
            client,
            endpoint: format!("{}/message", config.url),
            token: config.token.clone(),
            app_url: config.app_url().to_owned(),
            max_per_minute: usize::try_from(config.max_per_minute).unwrap_or(usize::MAX),
            // Три попытки примерно за пять минут.
            backoff: vec![Duration::from_secs(60), Duration::from_secs(180)],
            window: Arc::default(),
        })
    }

    /// Паузы между попытками; попыток на одну больше, чем пауз.
    pub fn with_backoff(mut self, backoff: Vec<Duration>) -> Self {
        self.backoff = backoff;
        self
    }

    fn max_attempts(&self) -> i32 {
        i32::try_from(self.backoff.len() + 1).unwrap_or(i32::MAX)
    }

    /// Скользящее окно минуты: место есть — оно занимается сразу.
    fn take_slot(&self) -> bool {
        let mut window = self.window.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        while window
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60))
        {
            window.pop_front();
        }
        if window.len() >= self.max_per_minute {
            return false;
        }
        window.push_back(now);
        true
    }

    async fn post(&self, message: &Message) -> Result<u16, Failure> {
        let mut key = reqwest::header::HeaderValue::from_str(self.token.expose())
            .map_err(|_| Failure::Fatal(0))?;
        key.set_sensitive(true);
        let response = self
            .client
            .post(&self.endpoint)
            .header("X-Gotify-Key", key)
            .json(&message.to_json())
            .send()
            .await
            .map_err(|_| Failure::Retry(None))?;
        let status = response.status().as_u16();
        drop(response);
        match status {
            200..=299 => Ok(status),
            408 | 429 | 500..=599 => Err(Failure::Retry(Some(status))),
            _ => Err(Failure::Fatal(status)),
        }
    }

    /// Один проход: забирает созревшие строки и отправляет их по одной.
    pub async fn deliver_due(
        &self,
        pool: &PgPool,
        now: DateTime<Utc>,
    ) -> Result<DeliveryReport, sqlx::Error> {
        let rows: Vec<PushRow> = sqlx::query_as(
            "select id,conversation_id,kind,payload,attempts,created_at from owner_pushes
             where state='pending' and run_at<=$1 order by run_at,id limit $2",
        )
        .bind(now)
        .bind(BATCH)
        .fetch_all(pool)
        .await?;
        let mut report = DeliveryReport::default();
        for row in rows {
            if now - row.created_at > STALE {
                finish(pool, row.id, "dropped", None).await?;
                warn_dropped(&row, None, "stale");
                report.dropped += 1;
                continue;
            }
            let Some(message) = compose(pool, &row, &self.app_url).await? else {
                finish(pool, row.id, "cancelled", None).await?;
                report.cancelled += 1;
                continue;
            };
            if !self.take_slot() {
                report.deferred += 1;
                break;
            }
            let attempts = row.attempts.saturating_add(1);
            match self.post(&message).await {
                Ok(status) => {
                    finish(pool, row.id, "sent", Some(status)).await?;
                    report.sent += 1;
                }
                Err(Failure::Retry(status)) if attempts < self.max_attempts() => {
                    let pause = self
                        .backoff
                        .get(usize::try_from(row.attempts).unwrap_or(usize::MAX))
                        .copied()
                        .unwrap_or_default();
                    let run_at = now + TimeDelta::from_std(pause).unwrap_or(TimeDelta::MAX);
                    sqlx::query(
                        "update owner_pushes set attempts=$2,last_status=$3,run_at=$4,
                           updated_at=clock_timestamp()
                         where id=$1 and state='pending'",
                    )
                    .bind(row.id)
                    .bind(attempts)
                    .bind(status.map(i32::from))
                    .bind(run_at)
                    .execute(pool)
                    .await?;
                    report.retried += 1;
                }
                Err(failure) => {
                    let status = match failure {
                        Failure::Retry(status) => status,
                        Failure::Fatal(0) => None,
                        Failure::Fatal(status) => Some(status),
                    };
                    sqlx::query(
                        "update owner_pushes set state='dropped',attempts=$2,last_status=$3,
                           updated_at=clock_timestamp()
                         where id=$1 and state='pending'",
                    )
                    .bind(row.id)
                    .bind(attempts)
                    .bind(status.map(i32::from))
                    .execute(pool)
                    .await?;
                    warn_dropped(&row, status, "gave_up");
                    report.dropped += 1;
                }
            }
        }
        Ok(report)
    }
}

async fn finish(
    pool: &PgPool,
    id: Uuid,
    state: &str,
    status: Option<u16>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update owner_pushes set state=$2,last_status=coalesce($3,last_status),
           attempts=attempts+case when $3 is null then 0 else 1 end,
           updated_at=clock_timestamp()
         where id=$1 and state='pending'",
    )
    .bind(id)
    .bind(state)
    .bind(status.map(i32::from))
    .execute(pool)
    .await?;
    Ok(())
}

/// Одна строка на брошенный пуш: вид, сессия и код ответа, без текста и ключа.
fn warn_dropped(row: &PushRow, status: Option<u16>, reason: &'static str) {
    tracing::warn!(
        kind = "owner_push_dropped",
        push_kind = %row.kind,
        conversation_id = %row.conversation_id,
        status = status.map(i64::from),
        reason,
        "owner push dropped"
    );
}

/// Текст пуша по текущему состоянию базы. `None` — пуш больше не нужен: сессия в корзине
/// или запрос уже решён.
async fn compose(
    pool: &PgPool,
    row: &PushRow,
    app_url: &str,
) -> Result<Option<Message>, sqlx::Error> {
    let conversation: Option<(String, bool)> =
        sqlx::query_as("select title,trashed_at is not null from conversations where id=$1")
            .bind(row.conversation_id)
            .fetch_optional(pool)
            .await?;
    let Some((title, trashed)) = conversation else {
        return Ok(None);
    };
    if trashed {
        return Ok(None);
    }
    let (body, priority) = match row.kind.as_str() {
        KIND_WAITING => {
            let Some(request_id) = row.payload["request_id"]
                .as_str()
                .and_then(|id| id.parse::<Uuid>().ok())
            else {
                return Ok(None);
            };
            let request: Option<(String, String, Value)> = sqlx::query_as(
                "select kind,state,request_payload from requests where id=$1 and conversation_id=$2",
            )
            .bind(request_id)
            .bind(row.conversation_id)
            .fetch_optional(pool)
            .await?;
            match request {
                Some((kind, state, payload)) if state == "open" => {
                    (waiting_text(&kind, &payload), PRIORITY_WAITING)
                }
                _ => return Ok(None),
            }
        }
        KIND_TURN_FINISHED => {
            // Ответ берём только из последнего хода: текст прошлого хода не должен
            // выдаваться за итог хода, кончившегося без ответа.
            let started: Option<DateTime<Utc>> = sqlx::query_scalar(
                "select max(started_at) from runs where conversation_id=$1 and finished_at <= $2",
            )
            .bind(row.conversation_id)
            .bind(row.created_at)
            .fetch_one(pool)
            .await?;
            let answer: Option<String> = match started {
                Some(started) => {
                    let query = format!(
                        "select left(content,4000) from blocks
                         where conversation_id=$1 and kind='text' and parent_block_id is null
                           and btrim(content) <> '' and created_at >= $2 and created_at <= $3
                           and {}
                         order by created_seq desc nulls last,created_at desc limit 1",
                        crate::agents::main_agent_filter("agent")
                    );
                    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
                        .bind(row.conversation_id)
                        .bind(started)
                        .bind(row.created_at)
                        .fetch_optional(pool)
                        .await?
                }
                None => None,
            };
            (turn_text(answer.as_deref()), PRIORITY_TURN_FINISHED)
        }
        KIND_FAILURE => {
            let at = failure_time(&row.payload["evidence"]);
            let local = match at {
                Some(at) => Some(local_time(pool, at).await?),
                None => None,
            };
            (
                failure_text(
                    row.payload["state"].as_str().unwrap_or_default(),
                    &row.payload["evidence"],
                    local.as_deref(),
                ),
                PRIORITY_FAILURE,
            )
        }
        _ => return Ok(None),
    };
    Ok(Some(Message {
        title: clean_title(&title),
        body,
        priority,
        click_url: format!("{app_url}/c/{}", row.conversation_id),
    }))
}

/// Время, которое назовёт текст сбоя: конец паузы по лимиту или последнее событие застрявшей.
fn failure_time(evidence: &Value) -> Option<DateTime<Utc>> {
    let field = match evidence["kind"].as_str()? {
        "quota_paused" => "until",
        "stalled" => "last_event_at",
        _ => return None,
    };
    evidence[field]
        .as_str()
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.with_timezone(&Utc))
}

async fn local_time(pool: &PgPool, at: DateTime<Utc>) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "select to_char($1 at time zone coalesce((select timezone from push_settings),'UTC'),
                        'DD.MM HH24:MI')",
    )
    .bind(at)
    .fetch_one(pool)
    .await
}

/// Строка без управляющих символов и лишних пробелов, не длиннее `limit` символов.
pub fn clip(text: &str, limit: usize) -> String {
    let plain: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let mut cut: String = flat.chars().take(limit.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

pub fn clean_title(title: &str) -> String {
    let title = clip(title, TITLE_CHARS);
    if title.is_empty() {
        "Canterlot".into()
    } else {
        title
    }
}

fn text_field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|text| !text.is_empty())
}

/// Текст пуша о запросе. Ответов в нём нет: запрос ещё открыт, а секретный ответ
/// хаб и так нигде не показывает.
pub fn waiting_text(kind: &str, payload: &Value) -> String {
    match kind {
        "question" => {
            let questions: Vec<&Value> = payload
                .get("questions")
                .or_else(|| payload.get("payload").and_then(|p| p.get("questions")))
                .and_then(Value::as_array)
                .map(|list| list.iter().collect())
                .unwrap_or_else(|| vec![payload]);
            let first = questions
                .first()
                .and_then(|q| text_field(q, &["text", "question", "message", "header"]));
            match first {
                Some(text) => {
                    let more = questions.len().saturating_sub(1);
                    let suffix = if more > 0 {
                        format!(" (+{more})")
                    } else {
                        String::new()
                    };
                    format!("Ждёт ответа: {}{suffix}", clip(text, DETAIL_CHARS))
                }
                None => "Ждёт ответа".into(),
            }
        }
        "permission" => {
            let tool = text_field(payload, &["tool_name", "tool"]);
            if tool == Some("ExitPlanMode") {
                return "Ждёт одобрения плана".into();
            }
            let input = payload.get("input").unwrap_or(&Value::Null);
            let detail = text_field(payload, &["command", "description"])
                .or_else(|| text_field(input, &["command", "description", "file_path"]));
            match (tool, detail) {
                (Some(tool), Some(detail)) => {
                    format!("Ждёт разрешения: {tool}: {}", clip(detail, DETAIL_CHARS))
                }
                (Some(tool), None) => format!("Ждёт разрешения: {}", clip(tool, DETAIL_CHARS)),
                (None, Some(detail)) => {
                    format!("Ждёт разрешения: {}", clip(detail, DETAIL_CHARS))
                }
                (None, None) => "Ждёт разрешения".into(),
            }
        }
        "spawn_approval" => match text_field(payload, &["harness"]) {
            Some(harness) => format!("Ждёт одобрения запуска: {}", clip(harness, 40)),
            None => "Ждёт одобрения запуска".into(),
        },
        _ => "Ждёт ответа".into(),
    }
}

pub fn turn_text(answer: Option<&str>) -> String {
    match answer.map(|text| clip(text, ANSWER_CHARS)) {
        Some(text) if !text.is_empty() => format!("Ход завершён\n{text}"),
        _ => "Ход завершён".into(),
    }
}

pub fn failure_text(state: &str, evidence: &Value, local_time: Option<&str>) -> String {
    match state {
        "stalled" => match local_time {
            Some(at) => format!("Застряла: нет событий с {at}"),
            None => "Застряла".into(),
        },
        "quota_paused" => match local_time {
            Some(at) => format!("Пауза по лимиту до {at}"),
            None => "Пауза по лимиту".into(),
        },
        OWNER_LOST => "Сессия упала: воркер потерян".into(),
        _ => match text_field(evidence, &["reason"]) {
            Some(reason) => format!("Сессия упала: {}", clip(reason, DETAIL_CHARS)),
            None => "Сессия упала".into(),
        },
    }
}

/// Фоновая служба: одна на базу, опрос раз в секунду, уборка готовых строк раз в час.
pub async fn run(pool: PgPool, sender: Sender, shutdown: watch::Receiver<bool>) {
    crate::singleton::supervise(&pool, "owner_push", shutdown, || {
        let pool = pool.clone();
        let sender = sender.clone();
        async move {
            let mut cleaned_at: Option<Instant> = None;
            loop {
                if cleaned_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(3600)) {
                    cleaned_at = Some(Instant::now());
                    if let Err(error) = sqlx::query(
                        "delete from owner_pushes where state<>'pending' and updated_at<$1",
                    )
                    .bind(Utc::now() - RETENTION)
                    .execute(&pool)
                    .await
                    {
                        log_error(&error);
                    }
                }
                match sender.deliver_due(&pool, Utc::now()).await {
                    Ok(report) if !report.is_empty() => tracing::info!(
                        kind = "owner_push_delivery",
                        sent = report.sent,
                        retried = report.retried,
                        dropped = report.dropped,
                        cancelled = report.cancelled,
                        deferred = report.deferred,
                        "owner pushes processed"
                    ),
                    Ok(_) => {}
                    Err(error) => log_error(&error),
                }
                tokio::time::sleep(POLL).await;
            }
        }
    })
    .await;
}

fn log_error(error: &sqlx::Error) {
    tracing::warn!(
        kind = "owner_push_db_error",
        error = %crate::dberr::describe(error),
        "owner push delivery failed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_flattens_and_marks_the_cut() {
        assert_eq!(clip("  a\n\tb  ", 10), "a b");
        assert_eq!(clip("абвгдеёжз", 5), "абвг…");
    }

    #[test]
    fn title_falls_back_and_drops_control_characters() {
        assert_eq!(clean_title(""), "Canterlot");
        assert_eq!(clean_title("a\u{7}\nb"), "a b");
        assert!(clean_title(&"x".repeat(200)).chars().count() <= TITLE_CHARS);
    }

    #[test]
    fn waiting_texts_name_the_request() {
        let question = json!({"questions":[{"id":"q1","text":"Деплоить?","is_secret":true},{"id":"q2","text":"Ещё"}]});
        assert_eq!(
            waiting_text("question", &question),
            "Ждёт ответа: Деплоить? (+1)"
        );
        assert_eq!(
            waiting_text(
                "permission",
                &json!({"tool_name":"ExitPlanMode","input":{"plan":"x"}})
            ),
            "Ждёт одобрения плана"
        );
        assert_eq!(
            waiting_text(
                "permission",
                &json!({"tool_name":"Bash","input":{"command":"ls -la"}})
            ),
            "Ждёт разрешения: Bash: ls -la"
        );
        assert_eq!(
            waiting_text(
                "spawn_approval",
                &json!({"harness":"codex","brief":"секрет"})
            ),
            "Ждёт одобрения запуска: codex"
        );
    }

    #[test]
    fn failure_texts() {
        assert_eq!(
            failure_text(
                "failed",
                &json!({"kind":"failed","reason":"process_exited"}),
                None
            ),
            "Сессия упала: process_exited"
        );
        assert_eq!(
            failure_text(OWNER_LOST, &json!({}), None),
            "Сессия упала: воркер потерян"
        );
        assert_eq!(
            failure_text("quota_paused", &json!({}), Some("10.11 14:00")),
            "Пауза по лимиту до 10.11 14:00"
        );
        assert_eq!(failure_text("stalled", &json!({}), None), "Застряла");
    }
}
