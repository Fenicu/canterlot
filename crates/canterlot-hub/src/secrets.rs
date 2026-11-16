//! Секретные запросы: владелец один раз вводит значение, и оно доходит до файла
//! в tmpfs на машине сессии.
//!
//! Гарантия касается пути «PWA → хаб → воркер → файл»: значение не попадает ни в
//! таблицы, ни в события, ни в outbox, ни в журналы. Здесь оно живёт только в
//! аргументе одного вызова и в одном управляющем сообщении. Поэтому в этом модуле
//! нет места, куда значение можно записать: у строки есть имя, назначение и путь,
//! но не значение.

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    delivery::{ContentBlock, SubmitError, SubmitInput, submit_input_in_tx},
    events::{NewEvent, Scope},
    session::Registry,
};

/// Сколько запросов одного разговора одновременно ждут владельца.
pub const MAX_OPEN_PER_CONVERSATION: i64 = 3;
/// Сколько запросов владельца ждут владельца во всём хабе.
pub const MAX_OPEN_PER_OWNER: i64 = 20;
/// Предел значения в UTF-8 байтах; больше — 413 и значение отбрасывается.
pub const MAX_VALUE_BYTES: usize = 16 * 1024;
pub const MAX_NAME_CHARS: usize = 80;
pub const MAX_PURPOSE_CHARS: usize = 300;
/// Подпись потерянного запроса в карточке PWA и в списке.
pub const NOTE_LOST: &str = "Значение не дошло до машины, введите снова";
/// Сколько воркер держит файл после доставки.
pub const FILE_TTL_HOURS: i64 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SecretState {
    Open,
    Sending,
    Delivered,
    Declined,
    Revoking,
    Revoked,
    Expired,
    Lost,
    Cancelled,
}

impl SecretState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Sending => "sending",
            Self::Delivered => "delivered",
            Self::Declined => "declined",
            Self::Revoking => "revoking",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::Lost => "lost",
            Self::Cancelled => "cancelled",
        }
    }

    /// Запрос ещё ждёт владельца: карточка у поля ввода, сессия в «Ждут вас».
    pub fn awaits_owner(self) -> bool {
        matches!(self, Self::Open | Self::Lost)
    }
}

impl std::str::FromStr for SecretState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "open" => Ok(Self::Open),
            "sending" => Ok(Self::Sending),
            "delivered" => Ok(Self::Delivered),
            "declined" => Ok(Self::Declined),
            "revoking" => Ok(Self::Revoking),
            "revoked" => Ok(Self::Revoked),
            "expired" => Ok(Self::Expired),
            "lost" => Ok(Self::Lost),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(format!("unknown secret request state: {other}")),
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SecretRequestRow {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub owner_id: Uuid,
    pub machine_id: Uuid,
    pub name: String,
    pub purpose: String,
    pub state: SecretState,
    pub attempt_id: Option<Uuid>,
    pub path: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Монотонная версия строки: PWA применяет только свежие события.
    pub revision: i64,
}

/// Ответ списка, ответа и отзыва. Поля значения здесь нет и быть не может.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SecretRequestView {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub name: String,
    pub purpose: String,
    pub state: SecretState,
    #[schema(nullable = false)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[schema(nullable = false)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[schema(nullable = false)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revision: i64,
}

impl From<&SecretRequestRow> for SecretRequestView {
    fn from(row: &SecretRequestRow) -> Self {
        Self {
            id: row.id,
            conversation_id: row.conversation_id,
            name: row.name.clone(),
            purpose: row.purpose.clone(),
            state: row.state,
            path: row.path.clone(),
            expires_at: row.expires_at,
            note: row.note.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
            revision: row.revision,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secret request not found")]
    NotFound,
    #[error("secret request is closed")]
    NotOpen,
    #[error("machine is offline")]
    MachineOffline,
    #[error("too many secret requests")]
    Limit,
    #[error("value is too large")]
    TooLarge,
    #[error("value was not delivered")]
    NotDelivered,
    #[error("invalid name")]
    InvalidName,
    #[error("invalid purpose")]
    InvalidPurpose,
    #[error("conversation not found")]
    ConversationNotFound,
    #[error("conversation is stopped by the owner")]
    OwnerStopped,
    #[error("database error")]
    Database(#[from] sqlx::Error),
}

/// Тело ответа никогда не печатается: `Debug` нужен только слоям логирования.
impl fmt::Debug for AnswerBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnswerBody")
            .field("decline", &self.decline)
            .field("value", &self.value.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// `{"value": "..."}` или `{"decline": true}`. Значение не сериализуется и не
/// хешируется нигде, кроме одного управляющего сообщения.
#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerBody {
    /// Значение секрета; ровно одно из двух полей.
    #[serde(default)]
    #[schema(nullable = false, write_only)]
    pub value: Option<String>,
    /// `true` — владелец отказал.
    #[serde(default)]
    pub decline: bool,
}

pub enum Answer {
    Value(String),
    Decline,
}

impl AnswerBody {
    /// Ответ должен быть ровно одним из двух: значение или отказ.
    pub fn parse(self) -> Option<Answer> {
        match (self.value, self.decline) {
            (Some(value), false) if !value.is_empty() => Some(Answer::Value(value)),
            (None, true) => Some(Answer::Decline),
            _ => None,
        }
    }
}

/// Попытка доставки в памяти процесса: без значения, только адреса и срок.
#[derive(Clone, Copy)]
pub struct Attempt {
    pub request_id: Uuid,
    pub machine_id: Uuid,
    /// Соединение, на котором ушло значение: сверка после переподключения не
    /// трогает попытки, отправленные уже на текущем соединении.
    pub connection_id: Uuid,
}

/// Только попытки, ждущие квитанции. Число ограничено пределом открытых
/// запросов, а запись снимается любой квитанцией, поэтому теснить нечего.
#[derive(Clone, Default)]
pub struct Attempts(Arc<Mutex<HashMap<Uuid, Attempt>>>);

impl fmt::Debug for Attempts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Attempts")
            .field("len", &self.0.lock().map(|map| map.len()).unwrap_or(0))
            .finish()
    }
}

impl Attempts {
    pub fn track(&self, attempt_id: Uuid, attempt: Attempt) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(attempt_id, attempt);
    }

    /// Снимает попытку с учёта: `true`, если она ещё ждала квитанции.
    pub fn take(&self, attempt_id: Uuid) -> Option<Attempt> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&attempt_id)
    }

    /// Соединение, на котором ушла попытка.
    pub fn connection(&self, attempt_id: Uuid) -> Option<Uuid> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&attempt_id)
            .map(|attempt| attempt.connection_id)
    }

    pub fn len(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn view(row: &SecretRequestRow) -> SecretRequestView {
    SecretRequestView::from(row)
}

/// Порядок блокировок хаба: строка разговора раньше строки запроса.
async fn lock_conversation(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
) -> Result<Uuid, SecretError> {
    let machine: Option<Uuid> = sqlx::query_scalar(
        "select machine_id from conversations
         where id = $1 and owner_id = $2 and trashed_at is null
         for no key update",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    machine.ok_or(SecretError::ConversationNotFound)
}

async fn lock_request(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<SecretRequestRow>, SecretError> {
    Ok(
        sqlx::query_as("select * from secret_requests where id = $1 for update")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

pub(crate) async fn emit_in_tx(
    conn: &mut PgConnection,
    row: &SecretRequestRow,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    crate::events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: row.conversation_id,
            },
            kind: "secret_request_changed",
            conversation_id: Some(row.conversation_id),
            execution_id: None,
            payload: serde_json::to_value(view(row)).unwrap_or_else(|_| json!({})),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

fn validate_name(name: &str) -> Result<(), SecretError> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > MAX_NAME_CHARS
        || trimmed.chars().any(char::is_control)
    {
        return Err(SecretError::InvalidName);
    }
    Ok(())
}

fn validate_purpose(purpose: &str) -> Result<(), SecretError> {
    if purpose.chars().count() > MAX_PURPOSE_CHARS || purpose.chars().any(char::is_control) {
        return Err(SecretError::InvalidPurpose);
    }
    Ok(())
}

/// Новый запрос от агента: карточка у владельца и `waiting_for_user` у сессии.
/// Значения здесь нет — только имя и назначение из аргументов инструмента.
pub async fn open(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    name: &str,
    purpose: &str,
    correlation_id: Uuid,
) -> Result<SecretRequestView, SecretError> {
    validate_name(name)?;
    validate_purpose(purpose)?;
    let mut tx = crate::db::begin(pool).await?;
    // Предел на владельца общий для всех его разговоров: порядок блокировок хаба
    // начинается с владельца, иначе два разговора одновременно увидят 19 и вставят двадцатый и двадцать первый.
    crate::conversations::lock_owner(&mut tx, owner_id).await?;
    let machine_id = lock_conversation(&mut tx, owner_id, conversation_id).await?;
    // Остановка владельцем ещё не снята: новая карточка ждала бы ответа,
    // которого после остановки не будет.
    let stopped: bool = sqlx::query_scalar(
        "select exists(
            select 1 from queue_state
            where conversation_id = $1
              and pause_reasons @> '[{\"reason\":\"user_stop\"}]'::jsonb)",
    )
    .bind(conversation_id)
    .fetch_one(&mut *tx)
    .await?;
    if stopped {
        return Err(SecretError::OwnerStopped);
    }
    let open_here: i64 = sqlx::query_scalar(
        "select count(*) from secret_requests
         where conversation_id = $1 and state in ('open', 'lost')",
    )
    .bind(conversation_id)
    .fetch_one(&mut *tx)
    .await?;
    if open_here >= MAX_OPEN_PER_CONVERSATION {
        return Err(SecretError::Limit);
    }
    let open_owner: i64 = sqlx::query_scalar(
        "select count(*) from secret_requests
         where owner_id = $1 and state in ('open', 'lost')",
    )
    .bind(owner_id)
    .fetch_one(&mut *tx)
    .await?;
    if open_owner >= MAX_OPEN_PER_OWNER {
        return Err(SecretError::Limit);
    }
    let row: SecretRequestRow = sqlx::query_as(
        "insert into secret_requests
            (id, conversation_id, owner_id, machine_id, name, purpose, state)
         values ($1, $2, $3, $4, $5, $6, 'open')
         returning *",
    )
    .bind(Uuid::now_v7())
    .bind(conversation_id)
    .bind(owner_id)
    .bind(machine_id)
    .bind(name.trim())
    .bind(purpose)
    .fetch_one(&mut *tx)
    .await?;
    emit_in_tx(&mut tx, &row, correlation_id).await?;
    crate::notify::on_secret_request_opened_in_tx(&mut tx, conversation_id, row.id).await?;
    tx.commit().await?;
    Ok(view(&row))
}

/// Все запросы разговора, новые первыми, в любом состоянии.
pub async fn list(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
) -> Result<Vec<SecretRequestView>, SecretError> {
    let known: bool = sqlx::query_scalar(
        "select exists(select 1 from conversations where id = $1 and owner_id = $2)",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_one(pool)
    .await?;
    if !known {
        return Err(SecretError::NotFound);
    }
    let rows: Vec<SecretRequestRow> = sqlx::query_as(
        "select * from secret_requests where conversation_id = $1
         order by created_at desc, id desc",
    )
    .bind(conversation_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(view).collect())
}

/// Ответ владельца: отказ или значение.
pub async fn answer(
    pool: &PgPool,
    registry: &Registry,
    owner_id: Uuid,
    id: Uuid,
    answer: Answer,
    correlation_id: Uuid,
) -> Result<SecretRequestView, SecretError> {
    match answer {
        Answer::Decline => decline(pool, owner_id, id, correlation_id).await,
        Answer::Value(value) => {
            if value.len() > MAX_VALUE_BYTES {
                return Err(SecretError::TooLarge);
            }
            deliver(pool, registry, owner_id, id, value, correlation_id).await
        }
    }
}

async fn decline(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    correlation_id: Uuid,
) -> Result<SecretRequestView, SecretError> {
    let mut tx = crate::db::begin(pool).await?;
    let conversation_id: Uuid = sqlx::query_scalar(
        "select conversation_id from secret_requests where id = $1 and owner_id = $2",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(SecretError::NotFound)?;
    lock_conversation(&mut tx, owner_id, conversation_id).await?;
    let row = lock_request(&mut tx, id)
        .await?
        .ok_or(SecretError::NotFound)?;
    if !row.state.awaits_owner() {
        return Err(SecretError::NotOpen);
    }
    let row: SecretRequestRow = sqlx::query_as(
        "update secret_requests
         set state = 'declined', attempt_id = null, path = null, expires_at = null,
             note = null, revision = revision + 1, updated_at = now()
         where id = $1 returning *",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    emit_in_tx(&mut tx, &row, correlation_id).await?;
    tx.commit().await?;
    queue_agent_message(
        pool,
        owner_id,
        &row,
        &format!("Владелец отказал в секрете «{}».", row.name),
        "declined",
        correlation_id,
    )
    .await;
    Ok(view(&row))
}

/// Запись попытки и отправка значения одним управляющим сообщением.
async fn deliver(
    pool: &PgPool,
    registry: &Registry,
    owner_id: Uuid,
    id: Uuid,
    value: String,
    correlation_id: Uuid,
) -> Result<SecretRequestView, SecretError> {
    let mut tx = crate::db::begin(pool).await?;
    let conversation_id: Uuid = sqlx::query_scalar(
        "select conversation_id from secret_requests where id = $1 and owner_id = $2",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(SecretError::NotFound)?;
    let machine_id = lock_conversation(&mut tx, owner_id, conversation_id).await?;
    let row = lock_request(&mut tx, id)
        .await?
        .ok_or(SecretError::NotFound)?;
    if !row.state.awaits_owner() {
        return Err(SecretError::NotOpen);
    }
    // Машина должна быть на связи прямо сейчас и понимать секреты: иначе значение
    // некуда девать, и оно отбрасывается вместе с ответом.
    if row.machine_id != machine_id {
        return Err(SecretError::MachineOffline);
    }
    // Отправитель и соединение берутся одним взглядом на реестр: иначе reconnect
    // между двумя вызовами увёл бы значение на новое соединение, а попытку
    // пометил бы старым, и сверка нового соединения объявила бы её потерянной.
    let Some((sender, connection_id, _)) = registry.command_session(machine_id) else {
        return Err(SecretError::MachineOffline);
    };
    let capable = registry
        .capabilities_for_connection(machine_id, connection_id)
        .is_some_and(|capabilities| {
            capabilities
                .iter()
                .any(|capability| capability == crate::session::CAPABILITY_SECRET_REQUESTS_V1)
        });
    if !capable {
        return Err(SecretError::MachineOffline);
    }
    let attempt_id = Uuid::now_v7();
    let row: SecretRequestRow = sqlx::query_as(
        "update secret_requests
         set state = 'sending', attempt_id = $2, path = null, expires_at = null,
             note = null, revision = revision + 1, updated_at = now()
         where id = $1 returning *",
    )
    .bind(id)
    .bind(attempt_id)
    .fetch_one(&mut *tx)
    .await?;
    emit_in_tx(&mut tx, &row, correlation_id).await?;
    // Учёт попытки встаёт до коммита: как только `sending` станет видимым сверке,
    // она уже знает, на каком соединении искать значение.
    registry.track_secret_attempt(attempt_id, row.id, machine_id, connection_id);
    if let Err(error) = tx.commit().await {
        registry.take_secret_attempt(attempt_id);
        return Err(error.into());
    }

    let deliver = canterlot_proto::worker::v1::SecretDeliver {
        attempt_id: attempt_id.to_string(),
        request_id: row.id.to_string(),
        conversation_id: row.conversation_id.to_string(),
        value: value.into_bytes(),
    };
    let message = canterlot_proto::worker::v1::ControlDown {
        msg: Some(canterlot_proto::worker::v1::control_down::Msg::SecretDeliver(deliver)),
    };
    // Точка для проверки гонки переподключения: значение ещё не ушло, а новая
    // сессия машины уже может быть на связи.
    test_hooks::before_secret_send().await;
    let sent = sender.try_send(message).is_ok();
    if !sent {
        // Значение уже потеряно: карточка должна предложить ввести снова. Ответ
        // берётся из базы: параллельный отзыв мог сменить состояние и ревизию.
        registry.take_secret_attempt(attempt_id);
        lose_attempt(pool, attempt_id, correlation_id).await?;
        let current: SecretRequestRow =
            sqlx::query_as("select * from secret_requests where id = $1")
                .bind(id)
                .fetch_one(pool)
                .await?;
        return Ok(view(&current));
    }
    spawn_timeout(pool.clone(), registry.clone(), attempt_id);
    Ok(view(&row))
}

/// Тестовый шов на время между коммитом `sending` и отправкой значения.
#[cfg(feature = "testkit")]
#[allow(dead_code)]
pub mod test_hooks {
    use std::sync::Mutex;
    use tokio::sync::oneshot;

    type Pause = oneshot::Sender<oneshot::Sender<()>>;

    static BEFORE_SEND: Mutex<Option<Pause>> = Mutex::new(None);

    /// Ближайшая доставка остановится перед отправкой и отдаст тесту канал
    /// «продолжить»: так гонка переподключения воспроизводится без сна.
    pub fn arm_before_secret_send() -> oneshot::Receiver<oneshot::Sender<()>> {
        let (reached, wait) = oneshot::channel();
        *BEFORE_SEND
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(reached);
        wait
    }

    pub async fn before_secret_send() {
        let pause = BEFORE_SEND
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(pause) = pause {
            let (resume, wait) = oneshot::channel();
            if pause.send(resume).is_ok() {
                let _ = wait.await;
            }
        }
    }
}

#[cfg(not(feature = "testkit"))]
pub mod test_hooks {
    pub async fn before_secret_send() {}
}

fn spawn_timeout(pool: PgPool, registry: Registry, attempt_id: Uuid) {
    let timeout = registry.secret_attempt_timeout();
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        // Квитанция снимает попытку с учёта раньше срока.
        if registry.take_secret_attempt(attempt_id).is_some()
            && let Err(error) = lose_attempt(&pool, attempt_id, Uuid::now_v7()).await
        {
            tracing::error!(
                kind = "secret_attempt_timeout_error",
                error = %crate::dberr::describe(&error),
                "secret attempt timeout failed"
            );
        }
    });
}

/// Попытка не дошла: `sending` становится `lost` с подписью для карточки.
/// Возвращает изменённую строку; `None` — состояние уже сменил другой путь.
pub async fn lose_attempt(
    pool: &PgPool,
    attempt_id: Uuid,
    correlation_id: Uuid,
) -> Result<Option<SecretRequestRow>, sqlx::Error> {
    let conversation: Option<Uuid> =
        sqlx::query_scalar("select conversation_id from secret_requests where attempt_id = $1")
            .bind(attempt_id)
            .fetch_optional(pool)
            .await?;
    let Some(conversation_id) = conversation else {
        return Ok(None);
    };
    let mut tx = crate::db::begin(pool).await?;
    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?;
    let row: Option<SecretRequestRow> = sqlx::query_as(
        "update secret_requests
         set state = 'lost', note = $2, revision = revision + 1, updated_at = now()
         where attempt_id = $1 and state = 'sending' returning *",
    )
    .bind(attempt_id)
    .bind(NOTE_LOST)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(row) = &row {
        // Значение могло дойти до файла: отзыв уберёт его, даже если квитанция
        // потерялась вместе со сроком.
        enqueue_revoke_in_tx(&mut tx, row).await?;
        emit_in_tx(&mut tx, row, correlation_id).await?;
    }
    tx.commit().await?;
    Ok(row)
}

/// Квитанция воркера: файл записан. Повтор старой попытки игнорируется.
pub async fn apply_delivered_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    fact: &canterlot_proto::worker::v1::SecretDelivered,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(attempt_id) = parse_uuid(&fact.attempt_id) else {
        return Ok(());
    };
    let conversation: Option<Uuid> = sqlx::query_scalar(
        "select conversation_id from secret_requests where attempt_id = $1 and machine_id = $2",
    )
    .bind(attempt_id)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(conversation_id) = conversation else {
        // Строка уже заменена новой попыткой или удалена: файл этой попытки мог
        // появиться, поэтому его надо убрать отдельной командой без разговора.
        if let Some(request_id) = parse_uuid(&fact.request_id) {
            revoke_late_delivery_in_tx(conn, machine_id, request_id, attempt_id).await?;
        }
        return Ok(());
    };
    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?;
    let expires_at = i64::try_from(fact.expires_at_ms)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_millis);
    let row: Option<SecretRequestRow> = sqlx::query_as(
        "update secret_requests
         set state = 'delivered', path = $2, expires_at = $3, note = null,
             revision = revision + 1, updated_at = now()
         where attempt_id = $1 and machine_id = $4 and state = 'sending' returning *",
    )
    .bind(attempt_id)
    .bind(&fact.path)
    .bind(expires_at)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        // Квитанция опоздала: запрос уже потерян, отменён, истёк или отозван.
        // Файл мог материализоваться, поэтому его надо убрать, а состояние не
        // менять; терминальное состояние не даёт отзыву потеряться.
        let stale: Option<SecretRequestRow> = sqlx::query_as(
            "select * from secret_requests
             where attempt_id = $1 and machine_id = $2
               and state in ('lost', 'cancelled', 'revoking', 'revoked', 'expired', 'declined')",
        )
        .bind(attempt_id)
        .bind(machine_id)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(stale) = stale {
            enqueue_revoke_if_missing_in_tx(conn, &stale).await?;
        } else if let Some(request_id) = parse_uuid(&fact.request_id) {
            revoke_late_delivery_in_tx(conn, machine_id, request_id, attempt_id).await?;
        }
        return Ok(());
    };
    emit_in_tx(conn, &row, correlation_id).await?;
    let text = match row.expires_at {
        Some(expires_at) => format!(
            "Секрет «{}» передан: файл {}, действует до {} UTC.",
            row.name,
            row.path.as_deref().unwrap_or_default(),
            expires_at.format("%H:%M")
        ),
        None => format!(
            "Секрет «{}» передан: файл {}.",
            row.name,
            row.path.as_deref().unwrap_or_default()
        ),
    };
    queue_message_in_tx(conn, &row, &text, "delivered", correlation_id).await;
    Ok(())
}

/// Воркер не смог записать файл: карточка снова ждёт владельца.
pub async fn apply_failed_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    fact: &canterlot_proto::worker::v1::SecretFailed,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(attempt_id) = parse_uuid(&fact.attempt_id) else {
        return Ok(());
    };
    let conversation: Option<Uuid> = sqlx::query_scalar(
        "select conversation_id from secret_requests where attempt_id = $1 and machine_id = $2",
    )
    .bind(attempt_id)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(conversation_id) = conversation else {
        return Ok(());
    };
    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?;
    let row: Option<SecretRequestRow> = sqlx::query_as(
        "update secret_requests
         set state = 'lost', note = $2, revision = revision + 1, updated_at = now()
         where attempt_id = $1 and machine_id = $3 and state = 'sending' returning *",
    )
    .bind(attempt_id)
    .bind(NOTE_LOST)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(row) = &row {
        emit_in_tx(conn, row, correlation_id).await?;
        tracing::warn!(
            kind = "secret_delivery_failed",
            reason = %fact.reason,
            "worker refused the secret file"
        );
    }
    Ok(())
}

/// Файл удалён воркером: отзыв, истечение или конец сессии.
pub async fn apply_removed_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    fact: &canterlot_proto::worker::v1::SecretRemoved,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(attempt_id) = parse_uuid(&fact.attempt_id) else {
        return Ok(());
    };
    // Команда отзыва закрывается по машине и попытке ещё до поиска строки:
    // detached-команда переживает удаление разговора, а заменённая попытка уже
    // не имеет строки, но её файл всё равно убран.
    complete_revoke_commands(
        conn,
        machine_id,
        attempt_id,
        json!({"removed": true, "reason": fact.reason}),
    )
    .await?;
    let expired = fact.reason == "expired";
    let conversation: Option<Uuid> = sqlx::query_scalar(
        "select conversation_id from secret_requests where attempt_id = $1 and machine_id = $2",
    )
    .bind(attempt_id)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(conversation_id) = conversation else {
        return Ok(());
    };
    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?;
    let next = if expired { "expired" } else { "revoked" };
    let row: Option<SecretRequestRow> = sqlx::query_as(
        "update secret_requests
         set state = $2, path = null, expires_at = null, note = null,
             revision = revision + 1, updated_at = now()
         where attempt_id = $1 and machine_id = $3 and state in ('revoking', 'delivered', 'expired', 'revoked')
         returning *",
    )
    .bind(attempt_id)
    .bind(next)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(());
    };
    emit_in_tx(conn, &row, correlation_id).await?;
    let text = if expired {
        format!("Секрет «{}» истёк, файл удалён.", row.name)
    } else {
        format!("Секрет «{}» отозван, файл удалён.", row.name)
    };
    queue_message_in_tx(conn, &row, &text, next, correlation_id).await;
    Ok(())
}

fn parse_uuid(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value).ok()
}

/// Удалить файл не удалось. «Отозван» и «истёк» без подтверждения не пишем:
/// карточка остаётся в прежнем состоянии с подписью, уборка повторит попытку.
pub async fn apply_remove_failed_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    fact: &canterlot_proto::worker::v1::SecretRemoveFailed,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(attempt_id) = parse_uuid(&fact.attempt_id) else {
        return Ok(());
    };
    // Итог команды пишется по машине и попытке даже без строки запроса:
    // detached-отзыв иначе остался бы незавершённым навсегда.
    complete_revoke_commands(
        conn,
        machine_id,
        attempt_id,
        json!({"removed": false, "reason": fact.reason}),
    )
    .await?;
    let conversation: Option<Uuid> = sqlx::query_scalar(
        "select conversation_id from secret_requests where attempt_id = $1 and machine_id = $2",
    )
    .bind(attempt_id)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(conversation_id) = conversation else {
        return Ok(());
    };
    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?;
    let note = format!(
        "Файл не удалён: воркер вернул ошибку ({})",
        crate::delivery::receipts::sanitize_reason(&fact.reason)
    );
    let row: Option<SecretRequestRow> = sqlx::query_as(
        "update secret_requests
         set note = $2, revision = revision + 1, updated_at = now()
         where attempt_id = $1 and machine_id = $3
           and state in ('revoking', 'delivered', 'expired')
         returning *",
    )
    .bind(attempt_id)
    .bind(&note)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(row) = &row {
        emit_in_tx(conn, row, correlation_id).await?;
    }
    Ok(())
}

/// Команда отзыва считается исполненной, когда воркер ответил за этот attempt.
async fn complete_revoke_commands(
    conn: &mut PgConnection,
    machine_id: Uuid,
    attempt_id: Uuid,
    result: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update commands
         set delivery_state = 'completed', completed_at = coalesce(completed_at, now()),
             result = $3
         where machine_id = $1 and kind = 'revoke_secret'
           and payload->>'attempt_id' = $2 and delivery_state <> 'completed'",
    )
    .bind(machine_id)
    .bind(attempt_id.to_string())
    .bind(result)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Отзыв доставленного значения: файл удаляет воркер по постоянной команде.
pub async fn revoke(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    correlation_id: Uuid,
) -> Result<SecretRequestView, SecretError> {
    let mut tx = crate::db::begin(pool).await?;
    let conversation_id: Uuid = sqlx::query_scalar(
        "select conversation_id from secret_requests where id = $1 and owner_id = $2",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(SecretError::NotFound)?;
    lock_conversation(&mut tx, owner_id, conversation_id).await?;
    let row = lock_request(&mut tx, id)
        .await?
        .ok_or(SecretError::NotFound)?;
    if row.attempt_id.is_none()
        || !(row.state == SecretState::Delivered || row.state == SecretState::Lost)
    {
        return Err(SecretError::NotDelivered);
    }
    let row: SecretRequestRow = sqlx::query_as(
        "update secret_requests
         set state = 'revoking', note = null, revision = revision + 1, updated_at = now()
         where id = $1 returning *",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    enqueue_revoke_in_tx(&mut tx, &row).await?;
    emit_in_tx(&mut tx, &row, correlation_id).await?;
    tx.commit().await?;
    withdraw_delivered_message(pool, &row, correlation_id).await;
    Ok(view(&row))
}

async fn enqueue_revoke_in_tx(
    conn: &mut PgConnection,
    row: &SecretRequestRow,
) -> Result<(), sqlx::Error> {
    let Some(attempt_id) = row.attempt_id else {
        return Ok(());
    };
    enqueue_revoke_command_in_tx(
        conn,
        row.machine_id,
        row.id,
        attempt_id,
        Some(row.conversation_id),
        Some(row.conversation_id),
    )
    .await
}

/// Постоянная команда отзыва. `conversation_id = None` — команда переживает
/// удаление разговора и несёт каталог в payload, чтобы воркер открыл его после.
async fn enqueue_revoke_command_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    request_id: Uuid,
    attempt_id: Uuid,
    conversation_id: Option<Uuid>,
    payload_conversation: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    // Один незавершённый отзыв на попытку: повторный путь не плодит команды.
    let pending: bool = sqlx::query_scalar(
        "select exists(
            select 1 from commands
            where machine_id = $1 and kind = 'revoke_secret'
              and payload->>'attempt_id' = $2 and delivery_state <> 'completed')",
    )
    .bind(machine_id)
    .bind(attempt_id.to_string())
    .fetch_one(&mut *conn)
    .await?;
    if pending {
        return Ok(());
    }
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, execution_id,
                               execution_generation, kind, payload, delivery_state)
         values ($1, $2, $3, null, null, 'revoke_secret', $4, 'pending')",
    )
    .bind(Uuid::now_v7())
    .bind(machine_id)
    .bind(conversation_id)
    .bind(json!({
        "request_id": request_id,
        "attempt_id": attempt_id,
        "conversation_id": payload_conversation,
    }))
    .execute(&mut *conn)
    .await?;
    sqlx::query("select pg_notify('canterlot_outbox', $1)")
        .bind(machine_id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn enqueue_revoke_if_missing_in_tx(
    conn: &mut PgConnection,
    row: &SecretRequestRow,
) -> Result<(), sqlx::Error> {
    enqueue_revoke_in_tx(conn, row).await
}

/// Отзыв для опоздавшей квитанции: он нужен только чужой попытке. Строка,
/// которая всё ещё владеет этой попыткой (например, уже `delivered`), не
/// отзывается: иначе каждая сверка после переподключения убивала бы живой файл.
async fn revoke_late_delivery_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    request_id: Uuid,
    attempt_id: Uuid,
) -> Result<(), sqlx::Error> {
    let existing: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "select conversation_id, attempt_id from secret_requests
         where id = $1 and machine_id = $2",
    )
    .bind(request_id)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    match existing {
        Some((_, Some(current))) if current == attempt_id => Ok(()),
        Some((conversation, _)) => {
            enqueue_revoke_command_in_tx(
                conn,
                machine_id,
                request_id,
                attempt_id,
                Some(conversation),
                Some(conversation),
            )
            .await
        }
        None => {
            // Строки уже нет: каталог помнит прежняя команда по этой попытке
            // (её отвязывает окончательная уборка), иначе файл не найти.
            let conversation: Option<Uuid> = sqlx::query_scalar(
                "select nullif(payload->>'conversation_id', '')::uuid from commands
                 where machine_id = $1 and kind = 'revoke_secret'
                   and payload->>'attempt_id' = $2
                   and nullif(payload->>'conversation_id', '') is not null
                 order by created_at desc limit 1",
            )
            .bind(machine_id)
            .bind(attempt_id.to_string())
            .fetch_optional(&mut *conn)
            .await?;
            enqueue_revoke_command_in_tx(
                conn,
                machine_id,
                request_id,
                attempt_id,
                None,
                conversation,
            )
            .await
        }
    }
}

/// Снимает ещё не отправленное сообщение «передан», чтобы оно не пришло после отзыва.
async fn withdraw_delivered_message(pool: &PgPool, row: &SecretRequestRow, correlation_id: Uuid) {
    let Some(attempt_id) = row.attempt_id else {
        return;
    };
    let key = message_key(row.id, "delivered");
    let input: Option<Uuid> = sqlx::query_scalar(
        "select id from inputs where conversation_id = $1 and idempotency_key = $2 and state = 'queued'",
    )
    .bind(row.conversation_id)
    .bind(&key)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some(input_id) = input else {
        return;
    };
    if crate::delivery::control::cancel_input(
        pool,
        row.owner_id,
        input_id,
        &format!("secret-withdraw:{attempt_id}"),
        correlation_id,
    )
    .await
    .is_err()
    {
        tracing::warn!(
            kind = "secret_message_withdraw_failed",
            "could not withdraw the delivered message"
        );
    }
}

/// Воркер отказал в отзыве: файл, возможно, остался. Карточка снова говорит
/// «передан», а не врёт про удаление; повтор отзыва снова поставит команду.
pub async fn on_revoke_rejected_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    command_id: Uuid,
    dispatch_attempt: i32,
    reason: &str,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let payload: Option<serde_json::Value> = sqlx::query_scalar(
        "select payload from commands
         where id = $1 and machine_id = $2 and kind = 'revoke_secret'
           and dispatch_attempt = $3 and delivery_state <> 'completed'",
    )
    .bind(command_id)
    .bind(machine_id)
    .bind(dispatch_attempt)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(payload) = payload else {
        return Ok(());
    };
    let Some(request_id) = payload
        .get("request_id")
        .and_then(|value| value.as_str())
        .and_then(parse_uuid)
    else {
        return Ok(());
    };
    let conversation: Option<Uuid> =
        sqlx::query_scalar("select conversation_id from secret_requests where id = $1")
            .bind(request_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(conversation_id) = conversation else {
        return Ok(());
    };
    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?;
    let row: Option<SecretRequestRow> = sqlx::query_as(
        "update secret_requests
         set state = 'delivered', note = $2, revision = revision + 1, updated_at = now()
         where id = $1 and state = 'revoking' returning *",
    )
    .bind(request_id)
    .bind(format!(
        "Файл не удалён: воркер отказал ({})",
        crate::delivery::receipts::sanitize_reason(reason)
    ))
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(row) = &row {
        emit_in_tx(conn, row, correlation_id).await?;
        let _ = sqlx::query(
            "update commands set delivery_state = 'completed',
                    completed_at = coalesce(completed_at, now()),
                    result = jsonb_build_object('rejected', true)
             where id = $1 and dispatch_attempt = $2",
        )
        .bind(command_id)
        .bind(dispatch_attempt)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Конец сессии: открытые запросы отменяются, а каждый attempt, который мог
/// материализоваться, получает отзыв: `sending`, `lost` с attempt, `delivered`
/// и `revoking`.
pub async fn on_session_end_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let rows: Vec<SecretRequestRow> = sqlx::query_as(
        "select * from secret_requests
         where conversation_id = $1
           and state in ('open', 'lost', 'sending', 'delivered', 'revoking')
         order by id for update",
    )
    .bind(conversation_id)
    .fetch_all(&mut *conn)
    .await?;
    for row in rows {
        match row.state {
            SecretState::Open | SecretState::Lost if row.attempt_id.is_none() => {
                let row: SecretRequestRow = sqlx::query_as(
                    "update secret_requests
                     set state = 'cancelled', note = null, revision = revision + 1,
                         updated_at = now()
                     where id = $1 returning *",
                )
                .bind(row.id)
                .fetch_one(&mut *conn)
                .await?;
                emit_in_tx(conn, &row, correlation_id).await?;
            }
            SecretState::Lost if row.attempt_id.is_some() => {
                // Потерянный attempt мог оставить файл: отзыв, как у доставленного.
                let row: SecretRequestRow = sqlx::query_as(
                    "update secret_requests
                     set state = 'revoking', note = null, revision = revision + 1,
                         updated_at = now()
                     where id = $1 returning *",
                )
                .bind(row.id)
                .fetch_one(&mut *conn)
                .await?;
                enqueue_revoke_in_tx(conn, &row).await?;
                emit_in_tx(conn, &row, correlation_id).await?;
            }
            SecretState::Sending | SecretState::Delivered => {
                let row: SecretRequestRow = sqlx::query_as(
                    "update secret_requests
                     set state = 'revoking', note = null, revision = revision + 1,
                         updated_at = now()
                     where id = $1 returning *",
                )
                .bind(row.id)
                .fetch_one(&mut *conn)
                .await?;
                enqueue_revoke_in_tx(conn, &row).await?;
                emit_in_tx(conn, &row, correlation_id).await?;
            }
            SecretState::Revoking => {
                enqueue_revoke_in_tx(conn, &row).await?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Окончательное удаление сессии: команды отзыва переживут её строки, поэтому
/// идут без `conversation_id` — файл на машине должен исчезнуть и без сессии.
pub async fn revoke_before_deletion_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let rows: Vec<SecretRequestRow> = sqlx::query_as(
        "select * from secret_requests
         where conversation_id = $1 and attempt_id is not null
           and state in ('sending', 'lost', 'delivered', 'revoking')
         order by id for update",
    )
    .bind(conversation_id)
    .fetch_all(&mut *conn)
    .await?;
    for row in rows {
        let Some(attempt_id) = row.attempt_id else {
            continue;
        };
        // Незавершённая команда, созданная остановкой или корзиной, должна
        // пережить удаление строк разговора: отвязываем её от каталога.
        sqlx::query(
            "update commands
             set conversation_id = null,
                 payload = payload || jsonb_build_object('conversation_id', $3::text)
             where machine_id = $1 and kind = 'revoke_secret'
               and payload->>'attempt_id' = $2 and delivery_state <> 'completed'",
        )
        .bind(row.machine_id)
        .bind(attempt_id.to_string())
        .bind(row.conversation_id.to_string())
        .execute(&mut *conn)
        .await?;
        enqueue_revoke_command_in_tx(
            conn,
            row.machine_id,
            row.id,
            attempt_id,
            Some(row.conversation_id),
            Some(row.conversation_id),
        )
        .await?;
        let _ = correlation_id;
    }
    Ok(())
}

/// Перезапуск хаба: `sending` без квитанции не дождётся ответа воркера. Граница —
/// момент старта: строки, отправленные уже этим процессом, остаются в покое.
pub async fn recover_after_restart(pool: &PgPool) -> Result<u64, sqlx::Error> {
    recover_after_restart_before(pool, Utc::now()).await
}

async fn recover_after_restart_before(
    pool: &PgPool,
    cutoff: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let conversation_ids: Vec<Uuid> = sqlx::query_scalar(
        "select conversation_id from secret_requests
         where state = 'sending' and updated_at < $1 order by id",
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await?;
    let mut lost = 0u64;
    for conversation_id in conversation_ids {
        lost += lose_sending_in_conversation(pool, conversation_id, None, Some(cutoff)).await?;
    }
    Ok(lost)
}

/// Служба хаба: после старта `sending` становится `lost`, дальше раз в минуту
/// подбираются попытки, о которых не помнит ни память, ни воркер.
pub async fn run(
    pool: PgPool,
    recovered_before: DateTime<Utc>,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) {
    if let Err(error) = recover_after_restart_before(&pool, recovered_before).await {
        tracing::error!(
            kind = "secret_recovery_error",
            error = %crate::dberr::describe(&error),
            "secret requests recovery failed"
        );
    }
    retry_removals_logged(&pool).await;
    let mut ticker = tokio::time::interval(Duration::from_secs(60));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            changed = stopped.changed() => {
                if changed.is_err() || *stopped.borrow_and_update() {
                    return;
                }
            }
            _ = ticker.tick() => {
                if let Err(error) = sweep_sending(&pool, Duration::from_secs(300)).await {
                    tracing::error!(
                        kind = "secret_sweep_error",
                        error = %crate::dberr::describe(&error),
                        "secret requests sweep failed"
                    );
                }
                retry_removals_logged(&pool).await;
            }
        }
    }
}

async fn retry_removals_logged(pool: &PgPool) {
    if let Err(error) = retry_removals(pool).await {
        tracing::error!(
            kind = "secret_removal_retry_error",
            error = %crate::dberr::describe(&error),
            "secret removal retry failed"
        );
    }
}

/// Повторяет отзывы, которые не подтвердились: `revoking` без завершённой
/// команды и доставленные/истёкшие файлы с ошибкой удаления. Пока команда
/// ждёт воркер, новой нет; срок растёт вдвое с каждой завершённой попыткой,
/// но не больше десяти минут.
pub async fn retry_removals(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let rows: Vec<SecretRequestRow> = sqlx::query_as(
        "select * from secret_requests r
         where r.attempt_id is not null
           and (r.state = 'revoking'
                or (r.state in ('delivered', 'expired')
                    and coalesce(r.note, '') like 'Файл не удалён:%'))
           and not exists (
               select 1 from commands c
               where c.machine_id = r.machine_id and c.kind = 'revoke_secret'
                 and c.payload->>'attempt_id' = r.attempt_id::text
                 and c.delivery_state <> 'completed')
           and r.updated_at < now() - least(
               power(2.0, (select count(*) from commands c
                         where c.machine_id = r.machine_id and c.kind = 'revoke_secret'
                           and c.payload->>'attempt_id' = r.attempt_id::text
                           and c.delivery_state = 'completed')::double precision) * interval '10 seconds',
               interval '10 minutes')
         order by r.id",
    )
    .fetch_all(pool)
    .await?;
    let mut retried = 0u64;
    for row in rows {
        let mut tx = crate::db::begin(pool).await?;
        sqlx::query("select id from conversations where id = $1 for no key update")
            .bind(row.conversation_id)
            .execute(&mut *tx)
            .await?;
        enqueue_revoke_in_tx(&mut tx, &row).await?;
        tx.commit().await?;
        retried += 1;
    }
    Ok(retried)
}

/// Периодическая уборка: `sending`, которое не получило квитанции и не попало в
/// память процесса (например, хаб перезапустился между commit и отправкой).
pub async fn sweep_sending(pool: &PgPool, older_than: Duration) -> Result<u64, sqlx::Error> {
    let conversations: Vec<Uuid> = sqlx::query_scalar(
        "select distinct conversation_id from secret_requests
         where state = 'sending' and updated_at < now() - $1 * interval '1 second'
         order by conversation_id",
    )
    .bind(older_than.as_secs_f64())
    .fetch_all(pool)
    .await?;
    let mut lost = 0u64;
    for conversation_id in conversations {
        lost += lose_sending_in_conversation(pool, conversation_id, Some(older_than), None).await?;
    }
    Ok(lost)
}

async fn lose_sending_in_conversation(
    pool: &PgPool,
    conversation_id: Uuid,
    older_than: Option<Duration>,
    before: Option<DateTime<Utc>>,
) -> Result<u64, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?;
    let rows: Vec<SecretRequestRow> = sqlx::query_as(
        "update secret_requests set state = 'lost', note = $2,
                revision = revision + 1, updated_at = now()
         where conversation_id = $1 and state = 'sending'
           and ($3::double precision is null
                or updated_at < now() - $3 * interval '1 second')
           and ($4::timestamptz is null or updated_at < $4)
         returning *",
    )
    .bind(conversation_id)
    .bind(NOTE_LOST)
    .bind(older_than.map(|age| age.as_secs_f64()))
    .bind(before)
    .fetch_all(&mut *tx)
    .await?;
    let count = rows.len() as u64;
    for row in &rows {
        // Файл мог появиться до потери квитанции: отзыв уберёт его.
        enqueue_revoke_in_tx(&mut tx, row).await?;
        emit_in_tx(&mut tx, row, Uuid::now_v7()).await?;
    }
    tx.commit().await?;
    Ok(count)
}

/// Подключение воркера: его манифесты решают судьбу `sending` этого хаба.
/// Граница снимка — `snapshot_id` соединения: попытку, отправленную уже на нём,
/// сверка не трогает, иначе доставка после снимка стала бы «потерянной».
pub async fn reconcile_manifests(
    pool: &PgPool,
    registry: &Registry,
    machine_id: Uuid,
    snapshot_id: Uuid,
    manifests: &[canterlot_proto::worker::v1::SecretManifest],
    correlation_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let mut files = std::collections::HashSet::new();
    for manifest in manifests {
        if let Some(attempt_id) = parse_uuid(&manifest.attempt_id) {
            files.insert(attempt_id);
        }
    }
    for manifest in manifests {
        let Some(attempt_id) = parse_uuid(&manifest.attempt_id) else {
            continue;
        };
        // Снимок не может знать о доставке, ушедшей уже после него.
        if registry.secret_attempt_connection(attempt_id) == Some(snapshot_id) {
            continue;
        }
        let fact = canterlot_proto::worker::v1::SecretDelivered {
            attempt_id: attempt_id.to_string(),
            request_id: manifest.request_id.clone(),
            path: manifest.path.clone(),
            expires_at_ms: manifest.expires_at_ms,
        };
        let mut tx = crate::db::begin(pool).await?;
        apply_delivered_in_tx(&mut tx, machine_id, &fact, correlation_id).await?;
        tx.commit().await?;
    }
    // Попытка, файла которой нет, потеряна: значение уже не прочитать. Каждая
    // строка обновляется по своему attempt_id, а не целым разговором.
    let sending: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select conversation_id, attempt_id from secret_requests
         where machine_id = $1 and state = 'sending' and attempt_id is not null
         order by conversation_id, id",
    )
    .bind(machine_id)
    .fetch_all(pool)
    .await?;
    let mut lost = 0u64;
    for (conversation_id, attempt_id) in sending {
        if files.contains(&attempt_id)
            || registry.secret_attempt_connection(attempt_id) == Some(snapshot_id)
        {
            continue;
        }
        lost += lose_sending_attempt(pool, conversation_id, attempt_id, correlation_id).await?;
    }
    Ok(lost)
}

/// Потерять одну конкретную попытку: обновление условно по её `attempt_id`.
async fn lose_sending_attempt(
    pool: &PgPool,
    conversation_id: Uuid,
    attempt_id: Uuid,
    correlation_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?;
    let row: Option<SecretRequestRow> = sqlx::query_as(
        "update secret_requests set state = 'lost', note = $2,
                revision = revision + 1, updated_at = now()
         where attempt_id = $1 and conversation_id = $3 and state = 'sending'
         returning *",
    )
    .bind(attempt_id)
    .bind(NOTE_LOST)
    .bind(conversation_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(0);
    };
    enqueue_revoke_in_tx(&mut tx, &row).await?;
    emit_in_tx(&mut tx, &row, correlation_id).await?;
    tx.commit().await?;
    Ok(1)
}

fn message_key(request_id: Uuid, stage: &str) -> String {
    format!("secret:{request_id}:{stage}")
}

async fn queue_agent_message(
    pool: &PgPool,
    owner_id: Uuid,
    row: &SecretRequestRow,
    text: &str,
    stage: &str,
    correlation_id: Uuid,
) {
    let mut tx = match crate::db::begin(pool).await {
        Ok(tx) => tx,
        Err(_) => return,
    };
    queue_message_in_tx(&mut tx, row, text, stage, correlation_id).await;
    if tx.commit().await.is_err() {
        tracing::warn!(
            kind = "secret_message_failed",
            stage,
            "secret message was not queued"
        );
    }
    let _ = owner_id;
}

/// Сообщение агенту идёт тем же путём, что и сообщение будильника: обычный ввод.
/// Занятая очередь не должна отменять уже записанный факт доставки.
async fn queue_message_in_tx(
    conn: &mut PgConnection,
    row: &SecretRequestRow,
    text: &str,
    stage: &str,
    correlation_id: Uuid,
) {
    let submitted = submit_input_in_tx(
        conn,
        &SubmitInput {
            owner_id: row.owner_id,
            conversation_id: row.conversation_id,
            idempotency_key: message_key(row.id, stage),
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            received_at: Utc::now(),
            correlation_id,
            origin: None,
        },
    )
    .await;
    match submitted {
        Ok(_) => {}
        Err(SubmitError::QueueFull { .. } | SubmitError::ConversationClosed) => {
            tracing::warn!(
                kind = "secret_message_skipped",
                stage,
                "conversation could not take the secret message"
            );
        }
        Err(SubmitError::Db(error)) => {
            tracing::error!(
                kind = "secret_message_db_error",
                error = %crate::dberr::describe(&error),
                "secret message was not queued"
            );
        }
        Err(_) => {
            tracing::warn!(
                kind = "secret_message_skipped",
                stage,
                "secret message was rejected"
            );
        }
    }
}

/// Сколько запросов ждут владельца: для журнала и тестов.
pub async fn awaiting_count(pool: &PgPool, conversation_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*) from secret_requests
         where conversation_id = $1 and state in ('open', 'lost')",
    )
    .bind(conversation_id)
    .fetch_one(pool)
    .await
}

/// `Json<Vec<HoldReason>>`-подобный разбор не нужен: строка состояния читается
/// только внутри SQL, поэтому здесь остаётся маленький помощник для тестов.
pub fn state_of(row: &SecretRequestRow) -> &'static str {
    row.state.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_body_prints_no_value() {
        let body = AnswerBody {
            value: Some("canterlot-secret-PROBE-value".into()),
            decline: false,
        };
        let printed = format!("{body:?}");
        assert!(!printed.contains("PROBE"));
        assert!(printed.contains("[redacted]"));
    }

    #[test]
    fn answer_body_is_either_a_value_or_a_decline() {
        assert!(matches!(
            AnswerBody {
                value: Some("v".into()),
                decline: false
            }
            .parse(),
            Some(Answer::Value(_))
        ));
        assert!(matches!(
            AnswerBody {
                value: None,
                decline: true
            }
            .parse(),
            Some(Answer::Decline)
        ));
        for body in [
            AnswerBody {
                value: Some("v".into()),
                decline: true,
            },
            AnswerBody {
                value: None,
                decline: false,
            },
            AnswerBody {
                value: Some(String::new()),
                decline: false,
            },
        ] {
            assert!(body.parse().is_none());
        }
    }

    #[test]
    fn names_are_short_and_printable() {
        assert!(validate_name("Пароль от базы").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name(&"x".repeat(MAX_NAME_CHARS + 1)).is_err());
        assert!(validate_name("a\nb").is_err());
        assert!(validate_purpose(&"x".repeat(MAX_PURPOSE_CHARS)).is_ok());
        assert!(validate_purpose(&"x".repeat(MAX_PURPOSE_CHARS + 1)).is_err());
    }

    #[test]
    fn attempts_are_removed_by_a_receipt_and_never_hold_a_value() {
        let attempts = Attempts::default();
        let connection = Uuid::now_v7();
        let ids: Vec<Uuid> = (0..4).map(|_| Uuid::now_v7()).collect();
        for attempt_id in &ids {
            attempts.track(
                *attempt_id,
                Attempt {
                    request_id: Uuid::now_v7(),
                    machine_id: Uuid::now_v7(),
                    connection_id: connection,
                },
            );
        }
        assert_eq!(attempts.len(), 4);
        assert_eq!(attempts.connection(ids[1]), Some(connection));
        assert!(attempts.take(ids[1]).is_some());
        assert_eq!(attempts.len(), 3);
        assert!(attempts.connection(ids[1]).is_none());
        let printed = format!("{attempts:?}");
        assert_eq!(printed, "Attempts { len: 3 }");
    }
}
