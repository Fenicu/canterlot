//! Секретные запросы: владелец один раз вводит значение, и оно доходит до файла
//! в tmpfs на машине сессии.
//!
//! Гарантия касается пути «PWA → хаб → воркер → файл»: значение не попадает ни в
//! таблицы, ни в события, ни в outbox, ни в журналы. Здесь оно живёт только в
//! аргументе одного вызова и в одном управляющем сообщении. Поэтому в этом модуле
//! нет места, куда значение можно записать: у строки есть имя, назначение и путь,
//! но не значение.
//!
//! Недействительность поколения (`lost`, `revoking`, `expired`, конец сессии,
//! замена) фиксируется обязательством `secret_revocations` в той же транзакции.
//! Обязательство не ссылается на строку запроса или разговора, поэтому переживает
//! окончательную уборку, а карточка остаётся `revoking`, пока воркер не подтвердит
//! отсутствие файла проверкой.

use std::{fmt, time::Duration};

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
/// База и предел роста паузы между повторами отзыва.
pub const BACKOFF_BASE_SECS: u64 = 10;
pub const BACKOFF_CAP_SECS: u64 = 10 * 60;

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
    pub path: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Монотонная версия строки: PWA применяет только свежие события.
    pub revision: i64,
    /// Наибольшее выданное поколение; 0 — значение ещё не выдавалось.
    pub generation: i64,
    /// Эпоха хранилища воркера на момент выдачи: смена означает очищенный tmpfs.
    pub secrets_epoch: Option<String>,
    /// Срок квитанции о доставке.
    pub send_deadline_at: Option<DateTime<Utc>>,
    /// Срок, отправленный с последним поколением: отзыв несёт его воркеру.
    pub generation_expires_at: Option<DateTime<Utc>>,
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

/// Живое управляющее соединение вместе с отправителем и эпохой хранилища: все
/// три берутся одним взглядом, поэтому значение не уйдёт на соединение с меткой
/// другого, а эпоха в строке совпадёт с той, что очистит tmpfs.
#[derive(Clone)]
struct Connection {
    sender: tokio::sync::mpsc::Sender<canterlot_proto::worker::v1::ControlDown>,
    connection_id: Uuid,
    epoch: String,
}

fn connection(registry: &Registry, machine_id: Uuid) -> Option<Connection> {
    let (sender, connection_id, _) = registry.command_session(machine_id)?;
    let capable = registry
        .capabilities_for_connection(machine_id, connection_id)
        .is_some_and(|capabilities| {
            capabilities
                .iter()
                .any(|capability| capability == crate::session::CAPABILITY_SECRET_REQUESTS_V2)
        });
    if !capable {
        return None;
    }
    let epoch = registry.secrets_epoch_for_connection(machine_id, connection_id)?;
    Some(Connection {
        sender,
        connection_id,
        epoch,
    })
}

fn expires_ms(at: Option<DateTime<Utc>>) -> u64 {
    at.and_then(|at| u64::try_from(at.timestamp_millis()).ok())
        .unwrap_or(0)
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

/// Очередь разговора — шаг 6 общего порядка блокировок, разговор — шаг 7:
/// квитанция ставит агенту сообщение, и очередь берётся до строки разговора.
async fn lock_queue(conn: &mut PgConnection, conversation_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("select conversation_id from queue_state where conversation_id = $1 for update")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
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
    // начинается с владельца, иначе два разговора одновременно увидят 19 и вставят двадцатый.
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
         set state = 'declined', path = null, expires_at = null,
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
        row.generation,
        correlation_id,
    )
    .await;
    Ok(view(&row))
}

/// Выдача нового поколения и отправка значения одним управляющим сообщением.
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
    if row.machine_id != machine_id {
        return Err(SecretError::MachineOffline);
    }
    // Отправитель, соединение, возможность и эпоха берутся одним взглядом: иначе
    // переподключение между проверками увело бы значение на другое соединение.
    let Some(connection) = connection(registry, machine_id) else {
        return Err(SecretError::MachineOffline);
    };
    let _ = connection.connection_id;
    let expires_at = Utc::now() + chrono::TimeDelta::hours(FILE_TTL_HOURS);
    let deadline = Utc::now() + registry.secret_attempt_timeout();
    // Прежнее поколение (потерянный запрос заполняют снова) становится
    // недействительным в этой же транзакции: его файл не должен пережить замену.
    if row.generation > 0 {
        raise_revocation_in_tx(
            &mut tx,
            machine_id,
            conversation_id,
            id,
            row.generation,
            expires_ms(row.generation_expires_at.or(row.expires_at)),
            "superseded",
        )
        .await?;
    }
    let generation = row.generation + 1;
    let row: SecretRequestRow = sqlx::query_as(
        "update secret_requests
         set state = 'sending', generation = $2, secrets_epoch = $3,
             path = null, expires_at = null, send_deadline_at = $4,
             generation_expires_at = $5,
             note = null, revision = revision + 1, updated_at = now()
         where id = $1 returning *",
    )
    .bind(id)
    .bind(generation)
    .bind(&connection.epoch)
    .bind(deadline)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await?;
    emit_in_tx(&mut tx, &row, correlation_id).await?;
    tx.commit().await?;

    let deliver = canterlot_proto::worker::v1::SecretDeliver {
        request_id: row.id.to_string(),
        conversation_id: row.conversation_id.to_string(),
        value: value.into_bytes(),
        generation: u64::try_from(generation).unwrap_or(0),
        expires_at_ms: u64::try_from(expires_at.timestamp_millis()).unwrap_or(0),
    };
    let message = canterlot_proto::worker::v1::ControlDown {
        msg: Some(canterlot_proto::worker::v1::control_down::Msg::SecretDeliver(deliver)),
    };
    if connection.sender.try_send(message).is_err() {
        // Значение уже потеряно: карточка должна предложить ввести снова, а
        // поколение получает обязательство отзыва в той же транзакции.
        lose_generation(pool, id, generation, correlation_id).await?;
        let current: SecretRequestRow =
            sqlx::query_as("select * from secret_requests where id = $1")
                .bind(id)
                .fetch_one(pool)
                .await?;
        return Ok(view(&current));
    }
    spawn_timeout(
        pool.clone(),
        id,
        generation,
        registry.secret_attempt_timeout(),
    );
    Ok(view(&row))
}

fn spawn_timeout(pool: PgPool, request_id: Uuid, generation: i64, timeout: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        if let Err(error) = lose_generation(&pool, request_id, generation, Uuid::now_v7()).await {
            tracing::error!(
                kind = "secret_attempt_timeout_error",
                error = %crate::dberr::describe(&error),
                "secret attempt timeout failed"
            );
        }
    });
}

/// Поколение не дошло: `sending` становится `lost` с подписью и обязательством
/// отзыва. Возвращает изменённую строку; `None` — состояние уже сменил другой путь.
pub async fn lose_generation(
    pool: &PgPool,
    request_id: Uuid,
    generation: i64,
    correlation_id: Uuid,
) -> Result<Option<SecretRequestRow>, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    let Some((conversation_id, machine_id)) = sqlx::query_as::<_, (Uuid, Uuid)>(
        "select conversation_id, machine_id from secret_requests where id = $1 and generation = $2",
    )
    .bind(request_id)
    .bind(generation)
    .fetch_optional(&mut *tx)
    .await?
    else {
        tx.commit().await?;
        return Ok(None);
    };
    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?;
    let row: Option<SecretRequestRow> = sqlx::query_as(
        "update secret_requests
         set state = 'lost', note = $3, revision = revision + 1, updated_at = now()
         where id = $1 and generation = $2 and state = 'sending' returning *",
    )
    .bind(request_id)
    .bind(generation)
    .bind(NOTE_LOST)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    raise_revocation_in_tx(
        &mut tx,
        machine_id,
        conversation_id,
        request_id,
        generation,
        expires_ms(row.generation_expires_at.or(row.expires_at)),
        "lost",
    )
    .await?;
    emit_in_tx(&mut tx, &row, correlation_id).await?;
    tx.commit().await?;
    Ok(Some(row))
}

fn receipt_mismatch(kind: &str, machine_id: Uuid, request_id: Uuid, conversation_id: Uuid) {
    tracing::warn!(
        kind = "secret_receipt_mismatch",
        receipt = kind,
        machine_id = %machine_id,
        request_id = %request_id,
        conversation_id = %conversation_id,
        "secret receipt does not name the request's conversation"
    );
}

async fn request_conversation(
    conn: &mut PgConnection,
    machine_id: Uuid,
    request_id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select conversation_id from secret_requests where id = $1 and machine_id = $2",
    )
    .bind(request_id)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await
}

/// Квитанция воркера: файл записан. Принимается только для текущего `sending`
/// того же поколения и того же разговора; опоздавшая квитанция ничего не
/// меняет — обязательство на мёртвое поколение уже создано правилом
/// недействительности.
pub async fn apply_delivered_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    fact: &canterlot_proto::worker::v1::SecretDelivered,
    correlation_id: Uuid,
    current_epoch: Option<&str>,
) -> Result<(), sqlx::Error> {
    let Some(request_id) = parse_uuid(&fact.request_id) else {
        return Ok(());
    };
    let generation = i64::try_from(fact.generation).unwrap_or(i64::MAX);
    let Some(conversation_id) = parse_uuid(&fact.conversation_id) else {
        return Ok(());
    };
    let Some(expected) = request_conversation(conn, machine_id, request_id).await? else {
        return Ok(());
    };
    if expected != conversation_id {
        receipt_mismatch("delivered", machine_id, request_id, conversation_id);
        return Ok(());
    }
    // Очередь — шаг 6 общего порядка блокировок, разговор — шаг 7. Квитанция
    // ставит агенту сообщение ниже, поэтому очередь берётся первой: иначе
    // проектор активности и приём журналов ловят взаимную блокировку.
    lock_queue(conn, conversation_id).await?;
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
             send_deadline_at = null, revision = revision + 1, updated_at = now()
         where id = $1 and machine_id = $4 and state = 'sending' and generation = $5
           and ($6::text is null or coalesce(secrets_epoch, '') = $6)
         returning *",
    )
    .bind(request_id)
    .bind(&fact.path)
    .bind(expires_at)
    .bind(machine_id)
    .bind(generation)
    .bind(current_epoch)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        // Квитанция из прошлой загрузки: tmpfs с тех пор очищен, файла нет.
        // Строку с другой эпохой нельзя объявлять доставленной.
        if let Some(current) = current_epoch {
            let stale: Option<SecretRequestRow> = sqlx::query_as(
                "select * from secret_requests
                 where id = $1 and machine_id = $2 and conversation_id = $3
                   and generation = $4 and state = 'sending'
                   and coalesce(secrets_epoch, '') <> $5
                 for update",
            )
            .bind(request_id)
            .bind(machine_id)
            .bind(conversation_id)
            .bind(generation)
            .bind(current)
            .fetch_optional(&mut *conn)
            .await?;
            if let Some(row) = stale {
                lose_epoch_in_tx(conn, row, correlation_id).await?;
            }
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
    queue_message_in_tx(
        conn,
        &row,
        &text,
        "delivered",
        row.generation,
        correlation_id,
    )
    .await;
    Ok(())
}

/// Смена эпохи делает поколение мёртвым: tmpfs очищен, файла нет. Карточка
/// становится `lost` (её можно заполнить снова), а долг отзыва закрывается
/// подтверждённым отсутствием.
async fn lose_epoch_in_tx(
    conn: &mut PgConnection,
    row: SecretRequestRow,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let updated: Option<SecretRequestRow> = sqlx::query_as(
        "update secret_requests
         set state = 'lost', path = null, expires_at = null, note = $3,
             send_deadline_at = null, revision = revision + 1, updated_at = now()
         where id = $1 and generation = $2 and state in ('sending', 'delivered')
         returning *",
    )
    .bind(row.id)
    .bind(row.generation)
    .bind(NOTE_LOST)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(updated) = updated else {
        return Ok(false);
    };
    raise_revocation_in_tx(
        conn,
        row.machine_id,
        row.conversation_id,
        row.id,
        row.generation,
        expires_ms(row.generation_expires_at.or(row.expires_at)),
        "epoch",
    )
    .await?;
    emit_in_tx(conn, &updated, correlation_id).await?;
    Ok(true)
}

/// Воркер не смог записать файл: карточка снова ждёт владельца, поколение
/// получает обязательство отзыва.
pub async fn apply_failed_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    fact: &canterlot_proto::worker::v1::SecretFailed,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(request_id) = parse_uuid(&fact.request_id) else {
        return Ok(());
    };
    let generation = i64::try_from(fact.generation).unwrap_or(i64::MAX);
    let Some(conversation_id) = parse_uuid(&fact.conversation_id) else {
        return Ok(());
    };
    let Some(expected) = request_conversation(conn, machine_id, request_id).await? else {
        return Ok(());
    };
    if expected != conversation_id {
        receipt_mismatch("delivered", machine_id, request_id, conversation_id);
        return Ok(());
    }
    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?;
    let row: Option<SecretRequestRow> = sqlx::query_as(
        "update secret_requests
         set state = 'lost', note = $3, revision = revision + 1, updated_at = now()
         where id = $1 and machine_id = $2 and state = 'sending' and generation = $4
           and conversation_id = $5
         returning *",
    )
    .bind(request_id)
    .bind(machine_id)
    .bind(NOTE_LOST)
    .bind(generation)
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(());
    };
    raise_revocation_in_tx(
        conn,
        machine_id,
        conversation_id,
        request_id,
        generation,
        expires_ms(row.generation_expires_at.or(row.expires_at)),
        "failed",
    )
    .await?;
    emit_in_tx(conn, &row, correlation_id).await?;
    tracing::warn!(
        kind = "secret_delivery_failed",
        reason = %fact.reason,
        "worker refused the secret file"
    );
    Ok(())
}

/// Файла с поколением до `cap` больше нет — подтверждено проверкой воркера.
/// Обязательство закрывается только полностью покрывающим `cap`; строка
/// переходит в терминальное состояние только если её поколение не новее `cap`.
pub async fn apply_removed_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    fact: &canterlot_proto::worker::v1::SecretRemoved,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(request_id) = parse_uuid(&fact.request_id) else {
        return Ok(());
    };
    let Some(conversation_id) = parse_uuid(&fact.conversation_id) else {
        return Ok(());
    };
    if let Some(expected) = request_conversation(conn, machine_id, request_id).await?
        && expected != conversation_id
    {
        receipt_mismatch("removed", machine_id, request_id, conversation_id);
        return Ok(());
    }
    let cap = fact.cap;
    // Команда этого или более старого поколения исполнена: повтор теперь ведёт
    // только обязательство, иначе завершённая команда навсегда занимала бы место
    // незавершённой и новые попытки не ставились бы.
    retire_revoke_commands_in_tx(
        conn,
        machine_id,
        conversation_id,
        request_id,
        cap,
        json!({"removed": true, "reason": fact.reason}),
    )
    .await?;
    // Старый `cap` не закрывает более новое обязательство.
    sqlx::query(
        "delete from secret_revocations
         where machine_id = $1 and request_id = $2 and conversation_id = $3
           and up_to <= $4",
    )
    .bind(machine_id)
    .bind(request_id)
    .bind(conversation_id)
    .bind(i64::try_from(cap).unwrap_or(i64::MAX))
    .execute(&mut *conn)
    .await?;
    // Осталось частичное обязательство: повторяем после паузы.
    reschedule_partial_obligation(conn, machine_id, conversation_id, request_id).await?;

    let Some(conversation_id): Option<Uuid> = sqlx::query_scalar(
        "select conversation_id from secret_requests
         where id = $1 and machine_id = $2 and conversation_id = $3",
    )
    .bind(request_id)
    .bind(machine_id)
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(());
    };
    // См. `apply_delivered_in_tx`: очередь до строки разговора.
    lock_queue(conn, conversation_id).await?;
    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?;
    let expired = fact.reason == "expired";
    let next = if expired { "expired" } else { "revoked" };
    let row: Option<SecretRequestRow> = if expired {
        sqlx::query_as(
            "update secret_requests
             set state = 'expired', path = null, expires_at = null, note = null,
                 send_deadline_at = null, revision = revision + 1, updated_at = now()
             where id = $1 and machine_id = $2 and generation <= $3
               and conversation_id = $4 and state in ('revoking', 'delivered')
             returning *",
        )
        .bind(request_id)
        .bind(machine_id)
        .bind(i64::try_from(cap).unwrap_or(i64::MAX))
        .bind(conversation_id)
        .fetch_optional(&mut *conn)
        .await?
    } else {
        sqlx::query_as(
            "update secret_requests
             set state = 'revoked', path = null, expires_at = null, note = null,
                 send_deadline_at = null, revision = revision + 1, updated_at = now()
             where id = $1 and machine_id = $2 and generation <= $3
               and conversation_id = $4 and state = 'revoking'
             returning *",
        )
        .bind(request_id)
        .bind(machine_id)
        .bind(i64::try_from(cap).unwrap_or(i64::MAX))
        .bind(conversation_id)
        .fetch_optional(&mut *conn)
        .await?
    };
    let Some(row) = row else {
        return Ok(());
    };
    emit_in_tx(conn, &row, correlation_id).await?;
    let text = if expired {
        format!("Секрет «{}» истёк, файл удалён.", row.name)
    } else {
        format!("Секрет «{}» отозван, файл удалён.", row.name)
    };
    queue_message_in_tx(conn, &row, &text, next, row.generation, correlation_id).await;
    Ok(())
}

/// Удалить файл не удалось: «отозван» и «истёк» без подтверждения не пишем,
/// обязательство повторяется с растущей паузой.
pub async fn apply_remove_failed_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    fact: &canterlot_proto::worker::v1::SecretRemoveFailed,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Some(request_id) = parse_uuid(&fact.request_id) else {
        return Ok(());
    };
    let Some(conversation_id) = parse_uuid(&fact.conversation_id) else {
        return Ok(());
    };
    if let Some(expected) = request_conversation(conn, machine_id, request_id).await?
        && expected != conversation_id
    {
        receipt_mismatch("remove_failed", machine_id, request_id, conversation_id);
        return Ok(());
    }
    retire_revoke_commands_in_tx(
        conn,
        machine_id,
        conversation_id,
        request_id,
        fact.cap,
        json!({"removed": false, "reason": fact.reason}),
    )
    .await?;
    retry_obligation(
        conn,
        machine_id,
        conversation_id,
        request_id,
        i64::try_from(fact.cap).unwrap_or(i64::MAX),
    )
    .await?;
    let Some(row): Option<SecretRequestRow> = sqlx::query_as(
        "update secret_requests
         set note = $3, revision = revision + 1, updated_at = now()
         where id = $1 and machine_id = $2 and state = 'revoking'
           and generation <= $4 and conversation_id = $5
         returning *",
    )
    .bind(request_id)
    .bind(machine_id)
    .bind(format!(
        "Файл не удалён: воркер вернул ошибку ({})",
        crate::delivery::receipts::sanitize_reason(&fact.reason)
    ))
    .bind(i64::try_from(fact.cap).unwrap_or(i64::MAX))
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(());
    };
    emit_in_tx(conn, &row, correlation_id).await?;
    Ok(())
}

/// Отзыв доставленного (или потерянного) значения: карточка сразу становится
/// `revoking`, а обязательство убирает файл даже после уборки разговора.
pub async fn revoke(
    pool: &PgPool,
    registry: &Registry,
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
    if row.generation == 0
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
    raise_revocation_in_tx(
        &mut tx,
        row.machine_id,
        row.conversation_id,
        row.id,
        row.generation,
        expires_ms(row.generation_expires_at.or(row.expires_at)),
        "revoked",
    )
    .await?;
    emit_in_tx(&mut tx, &row, correlation_id).await?;
    tx.commit().await?;
    withdraw_delivered_message(pool, &row, correlation_id).await;
    // Отзыв владельца не ждёт тика: команда ставится сразу, иначе файл жил бы
    // до минуты после подтверждённого «отзываю».
    if let Err(error) = emit_due_revocations(pool, registry).await {
        tracing::error!(
            kind = "secret_revoke_emit_error",
            error = %crate::dberr::describe(&error),
            "secret revocation command was not emitted"
        );
    }
    Ok(view(&row))
}

/// Создаёт или поднимает обязательство отзыва до `up_to`. Без ссылок на строку
/// запроса и разговор: обязательство переживает их удаление.
async fn raise_revocation_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    conversation_id: Uuid,
    request_id: Uuid,
    up_to: i64,
    max_expires_at_ms: u64,
    reason: &str,
) -> Result<(), sqlx::Error> {
    if up_to <= 0 {
        return Ok(());
    }
    sqlx::query(
        "insert into secret_revocations
            (id, machine_id, conversation_id, request_id, up_to,
             max_expires_at_ms, reason)
         values ($1, $2, $3, $4, $5, $6, $7)
         on conflict (machine_id, request_id) do update set
             conversation_id = excluded.conversation_id,
             up_to = greatest(secret_revocations.up_to, excluded.up_to),
             max_expires_at_ms = greatest(
                 secret_revocations.max_expires_at_ms, excluded.max_expires_at_ms),
             reason = case
                 when excluded.up_to >= secret_revocations.up_to then excluded.reason
                 else secret_revocations.reason
             end,
             next_attempt_at = case
                 when excluded.up_to > secret_revocations.up_to then now()
                 else least(secret_revocations.next_attempt_at, excluded.next_attempt_at)
             end,
             updated_at = now()",
    )
    .bind(Uuid::now_v7())
    .bind(machine_id)
    .bind(conversation_id)
    .bind(request_id)
    .bind(up_to)
    .bind(i64::try_from(max_expires_at_ms).unwrap_or(i64::MAX))
    .bind(reason)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Закрывает команды отзыва, чьё поколение покрыто квитанцией: успех, ошибка
/// удаления и отказ воркера одинаково снимают команду, а повтор ведёт
/// обязательство. Иначе `stored_on_machine` навсегда блокировал бы новую попытку.
async fn retire_revoke_commands_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    conversation_id: Uuid,
    request_id: Uuid,
    cap: u64,
    result: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update commands
         set delivery_state = 'completed', completed_at = coalesce(completed_at, now()),
             result = $5
         where machine_id = $1 and kind = 'revoke_secret'
           and payload->>'request_id' = $2
           and payload->>'conversation_id' = $3
           and coalesce((payload->>'up_to')::bigint, 0) <= $4
           and delivery_state <> 'completed'",
    )
    .bind(machine_id)
    .bind(request_id.to_string())
    .bind(conversation_id.to_string())
    .bind(i64::try_from(cap).unwrap_or(i64::MAX))
    .bind(result)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Повтор не удавшегося удаления: пауза растёт от числа отправленных команд,
/// показатель степени ограничен до умножения на интервал.
async fn retry_obligation(
    conn: &mut PgConnection,
    machine_id: Uuid,
    conversation_id: Uuid,
    request_id: Uuid,
    cap: i64,
) -> Result<(), sqlx::Error> {
    let attempts: Option<i32> = sqlx::query_scalar(
        "select attempts from secret_revocations
         where machine_id = $1 and request_id = $2 and conversation_id = $3
           and up_to <= $4",
    )
    .bind(machine_id)
    .bind(request_id)
    .bind(conversation_id)
    .bind(cap)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(attempts) = attempts else {
        return Ok(());
    };
    let next = backoff_seconds(attempts.saturating_add(1));
    sqlx::query(
        "update secret_revocations
         set attempts = attempts + 1,
             next_attempt_at = now() + ($5::bigint * interval '1 second'),
             updated_at = now()
         where machine_id = $1 and request_id = $2 and conversation_id = $3
           and up_to <= $4",
    )
    .bind(machine_id)
    .bind(request_id)
    .bind(conversation_id)
    .bind(cap)
    .bind(i64::try_from(next).unwrap_or(600))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Частичное подтверждение: обязательство осталось, значит нужен ещё один заход.
async fn reschedule_partial_obligation(
    conn: &mut PgConnection,
    machine_id: Uuid,
    conversation_id: Uuid,
    request_id: Uuid,
) -> Result<(), sqlx::Error> {
    let attempts: Option<i32> = sqlx::query_scalar(
        "select attempts from secret_revocations
         where machine_id = $1 and request_id = $2 and conversation_id = $3",
    )
    .bind(machine_id)
    .bind(request_id)
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(attempts) = attempts else {
        return Ok(());
    };
    let next = backoff_seconds(attempts.saturating_add(1));
    sqlx::query(
        "update secret_revocations
         set next_attempt_at = now() + ($4::bigint * interval '1 second'),
             updated_at = now()
         where machine_id = $1 and request_id = $2 and conversation_id = $3",
    )
    .bind(machine_id)
    .bind(request_id)
    .bind(conversation_id)
    .bind(i64::try_from(next).unwrap_or(10))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Воркер отказал в команде отзыва: обязательство повторяется, карточка не
/// врёт про удаление.
pub async fn on_revoke_rejected_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
    command_id: Uuid,
    dispatch_attempt: i32,
    reason: &str,
    _correlation_id: Uuid,
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
    let up_to = payload
        .get("up_to")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| i64::try_from(value).ok())
        .unwrap_or(0);
    let Some(conversation_id) = payload
        .get("conversation_id")
        .and_then(|value| value.as_str())
        .and_then(parse_uuid)
    else {
        return Ok(());
    };
    sqlx::query(
        "update commands
         set delivery_state = 'completed', completed_at = coalesce(completed_at, now()),
             result = jsonb_build_object('rejected', true)
         where id = $1 and dispatch_attempt = $2 and delivery_state <> 'completed'",
    )
    .bind(command_id)
    .bind(dispatch_attempt)
    .execute(&mut *conn)
    .await?;
    retry_obligation(conn, machine_id, conversation_id, request_id, up_to).await?;
    tracing::warn!(
        kind = "secret_revoke_rejected",
        reason = %crate::delivery::receipts::sanitize_reason(reason),
        "worker rejected the secret revocation"
    );
    Ok(())
}

/// Конец сессии: открытые запросы отменяются, каждое выданное поколение получает
/// обязательство и карточку `revoking` до подтверждённой уборки.
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
        if row.generation == 0 {
            if row.state == SecretState::Revoking {
                continue;
            }
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
            continue;
        }
        raise_revocation_in_tx(
            conn,
            row.machine_id,
            conversation_id,
            row.id,
            row.generation,
            expires_ms(row.generation_expires_at.or(row.expires_at)),
            "session_end",
        )
        .await?;
        if row.state == SecretState::Revoking {
            continue;
        }
        let row: SecretRequestRow = sqlx::query_as(
            "update secret_requests
             set state = 'revoking', note = null, revision = revision + 1,
                 updated_at = now()
             where id = $1 returning *",
        )
        .bind(row.id)
        .fetch_one(&mut *conn)
        .await?;
        emit_in_tx(conn, &row, correlation_id).await?;
    }
    Ok(())
}

/// Окончательное удаление сессии: обязательства переживают строки, поэтому
/// содержат только id. Значение на машине исчезнет и без строки запроса.
pub async fn revoke_before_deletion_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let rows: Vec<SecretRequestRow> = sqlx::query_as(
        "select * from secret_requests
         where conversation_id = $1 and generation > 0
           and state not in ('revoked', 'expired')
         order by id for update",
    )
    .bind(conversation_id)
    .fetch_all(&mut *conn)
    .await?;
    for row in rows {
        raise_revocation_in_tx(
            conn,
            row.machine_id,
            row.conversation_id,
            row.id,
            row.generation,
            expires_ms(row.generation_expires_at.or(row.expires_at)),
            "session_end",
        )
        .await?;
    }
    let _ = correlation_id;
    Ok(())
}

/// Перезапуск хаба: `sending` не дождётся квитанции и становится `lost` вместе
/// с обязательством отзыва.
pub async fn recover_after_restart(pool: &PgPool, registry: &Registry) -> Result<u64, sqlx::Error> {
    sweep_sending(pool, registry).await
}

/// Служба хаба: восстановление после старта, затем раз в минуту потерянные
/// доставки, истёкшие файлы и созревшие обязательства.
///
/// Стартовая работа выполняется ровно один раз и не повторяется первым тиком
/// интервала: он назначается на минуту вперёд. `ready` получает `true` после
/// старта — тестовый стенд ждёт его, чтобы не гоняться со службой.
pub async fn run(
    pool: PgPool,
    registry: Registry,
    mut stopped: tokio::sync::watch::Receiver<bool>,
    ready: Option<tokio::sync::watch::Sender<bool>>,
) {
    if let Err(error) = recover_after_restart(&pool, &registry).await {
        tracing::error!(
            kind = "secret_recovery_error",
            error = %crate::dberr::describe(&error),
            "secret requests recovery failed"
        );
    }
    tick_logged(&pool, &registry).await;
    if let Some(ready) = ready {
        let _ = ready.send(true);
    }
    let period = Duration::from_secs(60);
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            changed = stopped.changed() => {
                if changed.is_err() || *stopped.borrow_and_update() {
                    return;
                }
            }
            _ = ticker.tick() => tick_logged(&pool, &registry).await,
        }
    }
}

async fn tick_logged(pool: &PgPool, registry: &Registry) {
    if let Err(error) = tick(pool, registry).await {
        tracing::error!(
            kind = "secret_sweep_error",
            error = %crate::dberr::describe(&error),
            "secret requests sweep failed"
        );
    }
}

async fn tick(pool: &PgPool, registry: &Registry) -> Result<(), sqlx::Error> {
    sweep_sending(pool, registry).await?;
    sweep_expired(pool, registry).await?;
    emit_due_revocations(pool, registry).await?;
    Ok(())
}

/// `sending` без живой связи или с истёкшим сроком квитанции: поколение
/// становится `lost`, а обязательство отзыва создаётся в той же транзакции.
pub async fn sweep_sending(pool: &PgPool, registry: &Registry) -> Result<u64, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, i64)> = sqlx::query_as(
        "select id, machine_id, generation from secret_requests
         where state = 'sending' order by id",
    )
    .fetch_all(pool)
    .await?;
    let mut lost = 0u64;
    for (request_id, machine_id, generation) in rows {
        let expired = sqlx::query_scalar::<_, bool>(
            "select coalesce(send_deadline_at <= now(), false)
             from secret_requests where id = $1",
        )
        .bind(request_id)
        .fetch_one(pool)
        .await?;
        if !expired && registry.connection_id(machine_id).is_some() {
            continue;
        }
        if lose_generation(pool, request_id, generation, Uuid::now_v7())
            .await?
            .is_some()
        {
            lost += 1;
        }
    }
    Ok(lost)
}

/// Доставленный файл дожил до срока: карточка становится `revoking`, воркер
/// получит отзыв до этого поколения и подтвердит отсутствие.
pub async fn sweep_expired(pool: &PgPool, registry: &Registry) -> Result<u64, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, Uuid, Uuid, i64)> = sqlx::query_as(
        "select id, conversation_id, machine_id, owner_id, generation
         from secret_requests
         where state = 'delivered' and expires_at is not null and expires_at <= now()
         order by id",
    )
    .fetch_all(pool)
    .await?;
    let mut expired = 0u64;
    for (request_id, conversation_id, machine_id, _owner, generation) in rows {
        let mut tx = crate::db::begin(pool).await?;
        sqlx::query("select id from conversations where id = $1 for no key update")
            .bind(conversation_id)
            .execute(&mut *tx)
            .await?;
        let row: Option<SecretRequestRow> = sqlx::query_as(
            "update secret_requests
             set state = 'revoking', note = null, revision = revision + 1, updated_at = now()
             where id = $1 and state = 'delivered' and generation = $2 returning *",
        )
        .bind(request_id)
        .bind(generation)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            continue;
        };
        raise_revocation_in_tx(
            &mut tx,
            machine_id,
            conversation_id,
            request_id,
            row.generation,
            expires_ms(row.generation_expires_at.or(row.expires_at)),
            "expired",
        )
        .await?;
        emit_in_tx(&mut tx, &row, Uuid::now_v7()).await?;
        tx.commit().await?;
        expired += 1;
    }
    if expired > 0 {
        // Уборка срока не ждёт тика: машина на связи прямо сейчас.
        emit_due_revocations(pool, registry).await?;
    }
    Ok(expired)
}

/// Сколько секунд ждать перед следующей попыткой отзыва. Показатель степени
/// ограничен до умножения на интервал: `attempts = 1000` не переполняет его.
pub fn backoff_seconds(attempts: i32) -> u64 {
    let exponent = u32::try_from(attempts.max(0)).unwrap_or(u32::MAX).min(6);
    BACKOFF_BASE_SECS
        .saturating_mul(1u64 << exponent)
        .min(BACKOFF_CAP_SECS)
}

/// Отправляет созревшие обязательства живым воркерам `secret_requests_v2`
/// обычной командой outbox: значения в ней нет, только поколение и каталог.
/// Строка обязательства, прочитанная под замком.
type DueObligation = (bool, Uuid, Uuid, Uuid, i64, String, i32, i64);

pub async fn emit_due_revocations(pool: &PgPool, registry: &Registry) -> Result<u64, sqlx::Error> {
    // Отбор по живым машинам с v2 идёт до `limit`: очередь офлайн-машин иначе
    // занимала бы все сто мест и голодом морила готовый отзыв живой машины.
    let eligible = registry.machines_with_capability(crate::session::CAPABILITY_SECRET_REQUESTS_V2);
    if eligible.is_empty() {
        return Ok(0);
    }
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select id from secret_revocations
         where next_attempt_at <= clock_timestamp() and machine_id = any($1::uuid[])
         order by next_attempt_at, id limit 100",
    )
    .bind(&eligible)
    .fetch_all(pool)
    .await?;
    // Тестовый шов: квитанция может удалить обязательство между выборкой и
    // блокировкой; партия не должна из-за этого падать.
    test_hooks::before_lock(&ids).await;
    let mut sent = 0u64;
    for obligation_id in ids {
        let mut tx = crate::db::begin(pool).await?;
        // Читаем поля под замком: между выборкой и блокировкой строку могли
        // удалить или изменить квитанцией, и исчезнувшая строка — не ошибка партии.
        // Срок сверяется по часам после ожидания замка: `now()` — время начала
        // транзакции, а строку могли поднять уже после него, и обязательство
        // ложно выглядело бы незрелым.
        let locked: Option<DueObligation> = sqlx::query_as(
            "select next_attempt_at <= clock_timestamp(), machine_id, conversation_id, request_id,
                    up_to, reason, attempts, max_expires_at_ms
             from secret_revocations where id = $1 for update",
        )
        .bind(obligation_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((
            due,
            machine_id,
            conversation_id,
            request_id,
            up_to,
            reason,
            attempts,
            max_expires_at_ms,
        )) = locked
        else {
            tx.commit().await?;
            continue;
        };
        if !due {
            // Другой отправитель уже взял это обязательство.
            tx.commit().await?;
            continue;
        }
        let Some((_, connection_id, _)) = registry.command_session(machine_id) else {
            tx.commit().await?;
            continue;
        };
        let capable = registry
            .capabilities_for_connection(machine_id, connection_id)
            .is_some_and(|capabilities| {
                capabilities
                    .iter()
                    .any(|capability| capability == crate::session::CAPABILITY_SECRET_REQUESTS_V2)
            });
        if !capable {
            tx.commit().await?;
            continue;
        }
        // Пока команда этого или большего поколения ждёт воркер, новой не нужно.
        let pending: bool = sqlx::query_scalar(
            "select exists(
                select 1 from commands
                where machine_id = $1 and kind = 'revoke_secret'
                  and payload->>'request_id' = $2
                  and coalesce((payload->>'up_to')::bigint, 0) >= $3
                  and delivery_state <> 'completed')",
        )
        .bind(machine_id)
        .bind(request_id.to_string())
        .bind(up_to)
        .fetch_one(&mut *tx)
        .await?;
        if pending {
            sqlx::query(
                "update secret_revocations set next_attempt_at = now() + interval '60 seconds',
                        updated_at = now() where id = $1",
            )
            .bind(obligation_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            continue;
        }
        sqlx::query(
            "insert into commands
                (id, machine_id, conversation_id, execution_id, execution_generation,
                 kind, payload, delivery_state)
             values ($1, $2, (select id from conversations where id = $3),
                     null, null, 'revoke_secret', $4, 'pending')",
        )
        .bind(Uuid::now_v7())
        .bind(machine_id)
        .bind(conversation_id)
        .bind(json!({
            "request_id": request_id,
            "conversation_id": conversation_id,
            "up_to": up_to,
            "reason": reason,
            "max_expires_at_ms": u64::try_from(max_expires_at_ms).unwrap_or(0),
        }))
        .execute(&mut *tx)
        .await?;
        sqlx::query("select pg_notify('canterlot_outbox', $1)")
            .bind(machine_id.to_string())
            .execute(&mut *tx)
            .await?;
        let next = backoff_seconds(attempts.saturating_add(1));
        sqlx::query(
            "update secret_revocations
             set attempts = attempts + 1,
                 next_attempt_at = clock_timestamp() + ($2::bigint * interval '1 second'),
                 updated_at = now()
             where id = $1",
        )
        .bind(obligation_id)
        .bind(i64::try_from(next).unwrap_or(600))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        sent += 1;
    }
    Ok(sent)
}

/// Сеанс с хабом установлен: эпоха хранилища сменилась — прежние файлы исчезли
/// вместе с tmpfs, поэтому доставленные строки становятся `lost` (карточка уже
/// предлагает ввести снова), а обязательства — созревшими.
pub async fn on_hub_session_established(
    pool: &PgPool,
    registry: &Registry,
    machine_id: Uuid,
    connection_id: Uuid,
) -> Result<u64, sqlx::Error> {
    // Сверка принадлежит конкретному соединению: если за время её работы машина
    // уже подключилась заново, старая задача не имеет права трогать новые строки.
    let Some(epoch) = registry.secrets_epoch_for_connection(machine_id, connection_id) else {
        return Ok(0);
    };
    let rows: Vec<SecretRequestRow> = sqlx::query_as(
        "select * from secret_requests
         where machine_id = $1 and state in ('delivered', 'sending')
           and coalesce(secrets_epoch, '') <> $2
         order by id",
    )
    .bind(machine_id)
    .bind(&epoch)
    .fetch_all(pool)
    .await?;
    let mut found = 0u64;
    for row in rows {
        let mut tx = crate::db::begin(pool).await?;
        sqlx::query("select id from conversations where id = $1 for no key update")
            .bind(row.conversation_id)
            .execute(&mut *tx)
            .await?;
        // Замок взят: проверяем, что это всё ещё текущее соединение машины.
        if registry.connection_id(machine_id) != Some(connection_id) {
            tx.commit().await?;
            return Ok(found);
        }
        let Some(current_epoch) = registry.secrets_epoch_for_connection(machine_id, connection_id)
        else {
            tx.commit().await?;
            return Ok(found);
        };
        // Поколение, выданное уже текущим соединением, не трогаем.
        if row.secrets_epoch.as_deref() == Some(current_epoch.as_str()) {
            tx.commit().await?;
            continue;
        }
        if lose_epoch_in_tx(&mut tx, row, Uuid::now_v7()).await? {
            found += 1;
        }
        tx.commit().await?;
    }
    if registry.connection_id(machine_id) != Some(connection_id) {
        return Ok(found);
    }
    // Повторные отзывы не ждут тика: машина только что на связи.
    sqlx::query(
        "update secret_revocations set next_attempt_at = now(), updated_at = now()
         where machine_id = $1",
    )
    .bind(machine_id)
    .execute(pool)
    .await?;
    emit_due_revocations(pool, registry).await?;
    Ok(found)
}

fn parse_uuid(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value).ok()
}

fn message_key(request_id: Uuid, stage: &str, generation: i64) -> String {
    // Поколение в ключе: повторное заполнение не должно потерять своё сообщение
    // агенту из-за ключа, уже занятого прежней выдачей.
    format!("secret:{request_id}:{stage}:{generation}")
}

async fn queue_agent_message(
    pool: &PgPool,
    owner_id: Uuid,
    row: &SecretRequestRow,
    text: &str,
    stage: &str,
    generation: i64,
    correlation_id: Uuid,
) {
    let mut tx = match crate::db::begin(pool).await {
        Ok(tx) => tx,
        Err(_) => return,
    };
    queue_message_in_tx(&mut tx, row, text, stage, generation, correlation_id).await;
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
    generation: i64,
    correlation_id: Uuid,
) {
    let submitted = submit_input_in_tx(
        conn,
        &SubmitInput {
            owner_id: row.owner_id,
            conversation_id: row.conversation_id,
            idempotency_key: message_key(row.id, stage, generation),
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

/// Снимает ещё не отправленное сообщение «передан», чтобы оно не пришло после отзыва.
async fn withdraw_delivered_message(pool: &PgPool, row: &SecretRequestRow, correlation_id: Uuid) {
    let key = message_key(row.id, "delivered", row.generation);
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
        &format!("secret-withdraw:{}", row.id),
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

/// Строка состояния читается только внутри SQL; помощник нужен тестам.
pub fn state_of(row: &SecretRequestRow) -> &'static str {
    row.state.as_str()
}

#[cfg(feature = "testkit")]
pub mod test_hooks {
    use std::sync::Mutex;
    use tokio::sync::oneshot;
    use uuid::Uuid;

    type Pause = (oneshot::Sender<Vec<Uuid>>, oneshot::Receiver<()>);

    static BEFORE_LOCK: Mutex<Option<Pause>> = Mutex::new(None);

    /// Останавливает следующую отправку после выборки обязательств и отдаёт
    /// тесту их идентификаторы; `Sender<()>` продолжает партию.
    pub fn arm_before_lock() -> (oneshot::Receiver<Vec<Uuid>>, oneshot::Sender<()>) {
        let (reached, ids) = oneshot::channel();
        let (resume, wait) = oneshot::channel();
        *BEFORE_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some((reached, wait));
        (ids, resume)
    }

    pub(crate) async fn before_lock(ids: &[Uuid]) {
        let pause = BEFORE_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some((reached, wait)) = pause {
            let _ = reached.send(ids.to_vec());
            let _ = wait.await;
        }
    }
}

#[cfg(not(feature = "testkit"))]
pub mod test_hooks {
    pub(crate) async fn before_lock(_: &[uuid::Uuid]) {}
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
    fn backoff_is_capped_before_the_interval_is_multiplied() {
        assert_eq!(backoff_seconds(0), 10);
        assert_eq!(backoff_seconds(1), 20);
        assert_eq!(backoff_seconds(6), 600);
        assert_eq!(backoff_seconds(7), 600);
        assert_eq!(backoff_seconds(1000), 600, "no overflow at huge attempts");
        assert_eq!(backoff_seconds(i32::MAX), 600);
        assert_eq!(backoff_seconds(-5), 10, "a negative count is not a panic");
    }

    #[test]
    fn revocations_print_no_value() {
        let deliver = canterlot_proto::worker::v1::SecretDeliver {
            request_id: "r".into(),
            conversation_id: "c".into(),
            value: b"canterlot-secret-PROBE".to_vec(),
            generation: 1,
            expires_at_ms: 2,
        };
        let printed = format!("{deliver:?}");
        assert!(!printed.contains("PROBE"));
        assert!(printed.contains("value_bytes"));
    }
}
