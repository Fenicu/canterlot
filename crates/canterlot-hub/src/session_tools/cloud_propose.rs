//! Карточки облачных окон (`requests.kind = 'cloud_window'`): предложение агента, форма
//! владельца и карточка хаба проходят один путь. Агент только предлагает; окно создаёт
//! владелец своим решением, под строковым замком карточки и замком строки `cloud_budget`.
//!
//! Решение переиспользует механизм предложений настроек: `decision_saved` → задание хаба →
//! применение с заметкой в разговоре предлагающего. Просроченная карточка закрывается без окна.

use std::sync::Arc;

use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
use chrono::{DateTime, Duration as TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{Caller, ToolContext, ToolError, ToolErrorCode, origin, send, subs};
use crate::{
    cloud::{
        budget,
        price::PriceError,
        service::CloudService,
        windows::{self, CreateRefusal, CreateRequest, PricedProfile, Pricing, Trigger},
    },
    config::CloudConfig,
    crunch::JobOutcome,
    delivery::SubmitError,
    events,
    requests::{self, RequestKind, RequestState},
    scheduler::ClaimedJob,
};

/// Карточка живёт полчаса: просроченное предложение закрывается без окна.
pub const EXPIRY: TimeDelta = TimeDelta::minutes(30);
/// Харнесс служебного разговора, который только якорь для карточек владельца и хаба.
pub const SYSTEM_HARNESS: &str = "cloud";
const HINT_PENDING: &str = "The owner decides in the app; wait for the outcome message.";

/// Откуда пришло предложение. В окно попадает одно из значений `trigger`; для всех карточек
/// это `card`, а источник остаётся в payload как «кто попросил».
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Agent,
    Owner,
    Hub,
    /// Новая карточка после переоценки: те же причина и предлагающий.
    Repriced,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Owner => "owner",
            Self::Hub => "hub",
            Self::Repriced => "repriced",
        }
    }
}

/// Всё, что нужно карточке и что владелец видит на экране.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloudCard {
    pub source: Source,
    pub reason: String,
    pub hours: u32,
    pub hard_cap_minutes: u32,
    /// Профили на момент создания карточки (для displayed price).
    pub profiles: Vec<PricedProfile>,
    /// Цена часа самого дорогого профиля.
    pub hourly_kopecks: i64,
    pub reservation_kopecks: i64,
    /// Остаток бюджета на момент создания карточки.
    pub remaining_kopecks: i64,
    /// Только согласие на точный резерв позволяет выйти за бюджет.
    pub over_budget: bool,
    pub expires_at: DateTime<Utc>,
}

/// Кто предложил: агент — из своего исполнения, владелец и хаб — из служебного разговора.
#[derive(Debug, Clone)]
pub struct Proposer {
    pub source: Source,
    pub owner_id: Uuid,
    pub conversation_id: Uuid,
    pub execution_id: Uuid,
    pub generation: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Opened {
    pub request_id: Uuid,
    pub reservation_kopecks: i64,
    pub over_budget: bool,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum CardError {
    #[error("cloud_disabled")]
    Disabled,
    #[error("live_window_exists")]
    LiveWindow,
    #[error("open_card_exists")]
    OpenCard,
    #[error("invalid_hours")]
    InvalidHours,
    #[error("provider_unavailable")]
    Provider,
    #[error("price_error")]
    Price(#[from] PriceError),
    #[error("db_error")]
    Db(#[from] sqlx::Error),
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::with_message(ToolErrorCode::InvalidArguments, message)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeArgs {
    hours: u32,
    reason: String,
}

/// `runner_propose`: предложение агента. Возвращает id карточки и деньги; решение придёт
/// сообщением в его разговор.
pub async fn propose(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: ProposeArgs = super::parse_arguments("runner_propose", arguments)?;
    let Some(cloud) = context.cloud.as_ref() else {
        return Err(invalid("cloud_disabled"));
    };
    let mut conn = context.pool.acquire().await?;
    let generation: Option<i64> = sqlx::query_scalar(
        "select generation from executions
          where conversation_id=$1 and state not in ('stopped','failed','owner_lost')",
    )
    .bind(caller.conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    if generation.is_none() {
        return Err(ToolError::new(ToolErrorCode::ConversationClosed));
    }
    drop(conn);
    let opened = open(
        &context.pool,
        cloud,
        Proposer {
            source: Source::Agent,
            owner_id: caller.owner_id,
            conversation_id: caller.conversation_id,
            execution_id: caller.execution_id,
            generation: generation.unwrap_or_default(),
        },
        args.hours,
        args.reason,
    )
    .await
    .map_err(|error| invalid(error.to_string()))?;
    Ok(json!({
        "request_id": opened.request_id,
        "reservation_kopecks": opened.reservation_kopecks,
        "over_budget": opened.over_budget,
        "expires_at": opened.expires_at,
        "hint": HINT_PENDING,
    }))
}

/// Открыть карточку: одна транзакция, проверка живого окна и открытой карточки, резерв по
/// текущей цене провайдера. Вызывается агентом, формой владельца, хабом и при переоценке.
pub async fn open(
    pool: &sqlx::PgPool,
    cloud: &CloudService,
    proposer: Proposer,
    hours: u32,
    reason: String,
) -> Result<Opened, CardError> {
    if !(1..=4).contains(&hours) {
        return Err(CardError::InvalidHours);
    }
    let pricing = cloud.pricing().await.map_err(|_| CardError::Provider)?;
    let config = cloud.config().clone();
    let mut tx = crate::db::begin(pool).await?;
    let opened = open_in_tx(&mut tx, &config, &pricing, &proposer, hours * 60, reason).await?;
    tx.commit().await?;
    Ok(opened)
}

/// То же в чужой транзакции: вызывающая сторона решает, когда фиксировать. Предел задаётся
/// минутами: у карточки агента это `hours * 60`, у карточки хаба — та же формула, что у
/// автоматического режима, `min(240, ожидание + 30)`.
pub async fn open_in_tx(
    conn: &mut PgConnection,
    config: &CloudConfig,
    pricing: &Pricing,
    proposer: &Proposer,
    hard_cap: u32,
    reason: String,
) -> Result<Opened, CardError> {
    if hard_cap == 0 || hard_cap > windows::MAX_HARD_CAP_MINUTES {
        return Err(CardError::InvalidHours);
    }
    let hours = hard_cap.div_ceil(60);
    if budget::has_live_window(conn).await? {
        return Err(CardError::LiveWindow);
    }
    if budget::has_open_card(conn).await? {
        return Err(CardError::OpenCard);
    }
    let Some(max_hourly) = pricing.max_hourly_kopecks() else {
        return Err(CardError::Provider);
    };
    let reservation = windows::reservation_for(config, max_hourly, hard_cap)?;
    let remaining = budget::remaining_in_month(conn).await?;
    let over_budget = reservation > remaining;
    let expires_at = Utc::now() + EXPIRY;
    let card = CloudCard {
        source: proposer.source.clone(),
        reason,
        hours,
        hard_cap_minutes: hard_cap,
        profiles: pricing.profiles.clone(),
        hourly_kopecks: max_hourly,
        reservation_kopecks: reservation,
        remaining_kopecks: remaining,
        over_budget,
        expires_at,
    };
    let id = Uuid::now_v7();
    let payload = serde_json::to_value(&card).expect("cloud card serializes");
    let event = events::append(
        conn,
        events::NewEvent {
            scope: events::Scope::Conversation {
                id: proposer.conversation_id,
            },
            kind: "request_opened",
            conversation_id: Some(proposer.conversation_id),
            execution_id: Some(proposer.execution_id),
            payload: json!({
                "request_id": id,
                "kind": RequestKind::CloudWindow.as_str(),
                "payload": payload,
            }),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await?;
    requests::create_request_in_tx(
        conn,
        &requests::NewRequest {
            id,
            conversation_id: proposer.conversation_id,
            execution_id: proposer.execution_id,
            generation: proposer.generation,
            kind: RequestKind::CloudWindow,
            native_request_id: format!("cloud_window:{id}"),
            request_payload: payload,
            expires_at: Some(expires_at),
        },
    )
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .is_some_and(|database| database.is_unique_violation())
        {
            CardError::OpenCard
        } else {
            CardError::Db(error)
        }
    })?;
    sqlx::query("update requests set created_seq=$2 where id=$1")
        .bind(id)
        .bind(event.scope_seq)
        .execute(&mut *conn)
        .await?;
    crate::notify::on_request_opened_in_tx(conn, proposer.conversation_id, id).await?;
    subs::insert_job(
        conn,
        proposer.owner_id,
        proposer.conversation_id,
        expires_at,
        &json!({"action": "cloud_window_expire", "request_id": id}),
    )
    .await?;
    Ok(Opened {
        request_id: id,
        reservation_kopecks: reservation,
        over_budget,
        expires_at,
    })
}

/// Итог решения карточки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApproveOutcome {
    Created(Uuid),
    Outdated(Uuid),
    Refused(String),
    Expired,
    AlreadyDecided,
}

/// Принять карточку: всё в одной транзакции. Замок строки карточки держится от проверки
/// состояния до отметки решения, `create_in_tx` берёт замок `cloud_budget`. Повторное
/// одобрение не создаёт второе окно ни по состоянию, ни по уникальному индексу proposal_id.
pub async fn approve_in_tx(
    conn: &mut PgConnection,
    cloud: &CloudService,
    request_id: Uuid,
    parent: Uuid,
    correlation: Uuid,
) -> Result<ApproveOutcome, SubmitError> {
    send::lock_queue(conn, parent).await?;
    let row: Option<(RequestState, Value, Option<DateTime<Utc>>)> = sqlx::query_as(
        "select state, request_payload, expires_at from requests
          where id = $1 and kind = 'cloud_window' for update",
    )
    .bind(request_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((RequestState::DecisionSaved, payload, expires_at)) = row else {
        return Ok(ApproveOutcome::AlreadyDecided);
    };
    let Ok(card) = serde_json::from_value::<CloudCard>(payload) else {
        return Ok(ApproveOutcome::Refused("invalid_card".to_owned()));
    };
    // Источник срока — колонка запроса: задание истечения могло изменить именно её.
    if expires_at.is_some_and(|at| at <= Utc::now()) || card.expires_at <= Utc::now() {
        close_in_tx(
            conn,
            request_id,
            RequestState::Expired,
            "expired",
            correlation,
        )
        .await?;
        return Ok(ApproveOutcome::Expired);
    }
    // Переоценка: цена и признак перерасхода должны совпадать с карточкой. Сетевой вызов
    // делается до взятия замка `cloud_budget` внутри `create_in_tx`.
    let pricing = match cloud.pricing().await {
        Ok(pricing) => pricing,
        Err(error) => {
            return Ok(ApproveOutcome::Refused(error.to_string()));
        }
    };
    let config = cloud.config();
    let Some(max_hourly) = pricing.max_hourly_kopecks() else {
        return Ok(ApproveOutcome::Refused("provider_unavailable".to_owned()));
    };
    let reservation = match windows::reservation_for(config, max_hourly, card.hard_cap_minutes) {
        Ok(reservation) => reservation,
        Err(error) => return Ok(ApproveOutcome::Refused(error.to_string())),
    };
    let remaining = budget::remaining_in_month(conn).await?;
    let over_budget = reservation > remaining;
    if reservation != card.reservation_kopecks || over_budget != card.over_budget {
        close_in_tx(
            conn,
            request_id,
            RequestState::DecisionNotApplied,
            "card_outdated",
            correlation,
        )
        .await?;
        let proposer = proposer_of(conn, request_id).await?;
        let fresh = match open_in_tx(
            conn,
            config,
            &pricing,
            &proposer,
            card.hard_cap_minutes,
            card.reason.clone(),
        )
        .await
        {
            Ok(fresh) => fresh,
            Err(CardError::Db(error)) => return Err(error.into()),
            Err(other) => return Ok(ApproveOutcome::Refused(other.to_string())),
        };
        return Ok(ApproveOutcome::Outdated(fresh.request_id));
    }
    let consent = card.over_budget.then_some(reservation);
    let outcome = windows::create_in_tx(
        conn,
        config,
        pricing,
        CreateRequest {
            trigger: Trigger::Card,
            proposal_id: Some(request_id),
            hard_cap_minutes: card.hard_cap_minutes,
            consent_over_budget_kopecks: consent,
        },
    )
    .await;
    let window = match outcome {
        Ok(window) => window,
        Err(CreateRefusal::Database(error)) => return Err(error.into()),
        Err(refusal) => {
            let code = refusal.to_string();
            close_in_tx(
                conn,
                request_id,
                RequestState::DecisionNotApplied,
                &code,
                correlation,
            )
            .await?;
            return Ok(ApproveOutcome::Refused(code));
        }
    };
    close_in_tx(
        conn,
        request_id,
        RequestState::DecisionApplied,
        "allowed",
        correlation,
    )
    .await?;
    let _ = window;
    Ok(ApproveOutcome::Created(window))
}

/// Откуда карточка: данные proposing conversation хранятся в строке запроса.
async fn proposer_of(conn: &mut PgConnection, request_id: Uuid) -> Result<Proposer, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        owner_id: Uuid,
        conversation_id: Uuid,
        execution_id: Uuid,
        generation: i64,
    }
    let row: Row = sqlx::query_as(
        "select c.owner_id, r.conversation_id, r.execution_id, r.generation
           from requests r join conversations c on c.id = r.conversation_id
          where r.id = $1",
    )
    .bind(request_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(Proposer {
        source: Source::Repriced,
        owner_id: row.owner_id,
        conversation_id: row.conversation_id,
        execution_id: row.execution_id,
        generation: row.generation,
    })
}

/// Закрыть карточку под уже взятым замком строки.
async fn close_in_tx(
    conn: &mut PgConnection,
    request: Uuid,
    next: RequestState,
    reason: &str,
    correlation: Uuid,
) -> Result<bool, sqlx::Error> {
    let changed = sqlx::query(
        "update requests set state=$2, updated_at=clock_timestamp()
          where id=$1 and kind='cloud_window' and state in ('open','decision_saved')",
    )
    .bind(request)
    .bind(next.as_str())
    .execute(&mut *conn)
    .await?
    .rows_affected()
        > 0;
    if changed {
        let owner_id: Uuid = sqlx::query_scalar(
            "select c.owner_id from requests r join conversations c on c.id=r.conversation_id where r.id=$1",
        )
        .bind(request)
        .fetch_one(&mut *conn)
        .await?;
        events::append(
            conn,
            events::NewEvent {
                scope: events::Scope::Conversations { owner_id },
                kind: "request_closed",
                conversation_id: None,
                execution_id: None,
                payload: json!({"request_id": request, "reason": reason}),
                correlation_id: correlation,
            },
        )
        .await?;
    }
    Ok(changed)
}

pub(crate) async fn queue_decision_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    parent: Uuid,
    request: Uuid,
) -> Result<Uuid, sqlx::Error> {
    subs::insert_job(
        conn,
        owner,
        parent,
        Utc::now(),
        &json!({"action": "cloud_window", "request_id": request}),
    )
    .await
}

fn outcome_text(request: Uuid, outcome: &str, detail: &str) -> String {
    format!("[canterlot] Your cloud_window request {request} was {outcome}. {detail}")
}

fn note_job(request: Uuid, reason: &str, text: String) -> Value {
    json!({
        "action": "note",
        "text": text,
        "origin": origin::with_event(
            origin::note_origin(0),
            origin::HubEvent::approval(reason, request, None, None),
        ),
        "dedupe_key": format!("cloud_window:{request}"),
    })
}

#[derive(Deserialize)]
struct CloudJob {
    request_id: Uuid,
}

/// Решение владельца, доставленное заданием хаба.
pub async fn run_job(
    conn: &mut PgConnection,
    cloud: Option<&Arc<CloudService>>,
    job: &ClaimedJob,
    parent: Uuid,
    now: DateTime<Utc>,
    correlation: Uuid,
) -> Result<JobOutcome, SubmitError> {
    let Ok(CloudJob { request_id }) = serde_json::from_value(job.payload.clone()) else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    crate::conversations::lock_owner(conn, job.owner_id).await?;
    send::lock_queue(conn, parent).await?;
    let row: Option<(RequestState, Option<Value>)> = sqlx::query_as(
        "select state, decision_payload from requests
          where id=$1 and conversation_id=$2 and kind='cloud_window' for update",
    )
    .bind(request_id)
    .bind(parent)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((RequestState::DecisionSaved, decision)) = row else {
        return Ok(JobOutcome::Done(json!({"skipped": true})));
    };
    let trashed: bool =
        sqlx::query_scalar("select trashed_at is not null from conversations where id=$1")
            .bind(parent)
            .fetch_one(&mut *conn)
            .await?;
    if trashed {
        return Ok(JobOutcome::Done(json!({"skipped": true})));
    }
    let outcome = match decision
        .and_then(|value| serde_json::from_value::<DecisionPayload>(value).ok())
    {
        Some(DecisionPayload::Permission {
            behavior: PermissionBehavior::Deny,
            message,
        }) => {
            let remark = message
                .filter(|text| !text.trim().is_empty())
                .map_or(String::new(), |text| format!(" The owner said: {text}"));
            close_in_tx(
                conn,
                request_id,
                RequestState::Declined,
                "declined",
                correlation,
            )
            .await?;
            let text = format!(
                "[canterlot] The owner declined your cloud_window request {request_id}. No window was started.{remark}"
            );
            note_job(request_id, "declined", text)
        }
        Some(DecisionPayload::Permission {
            behavior: PermissionBehavior::Allow,
            ..
        }) => {
            let result = match cloud.as_ref() {
                Some(cloud) => {
                    approve_in_tx(conn, cloud.as_ref(), request_id, parent, correlation).await?
                }
                None => ApproveOutcome::Refused("cloud_disabled".to_owned()),
            };
            let (reason, text) = match &result {
                ApproveOutcome::Created(_) => (
                    "allowed",
                    outcome_text(
                        request_id,
                        "approved",
                        "The owner approved it; the cloud window is starting.",
                    ),
                ),
                ApproveOutcome::Outdated(fresh) => (
                    "card_outdated",
                    outcome_text(
                        request_id,
                        "not applied",
                        &format!(
                            "The price or budget changed while the card was open; a fresh card {fresh} was issued."
                        ),
                    ),
                ),
                ApproveOutcome::Refused(code) => (
                    "refused",
                    outcome_text(
                        request_id,
                        "not applied",
                        &format!("The provider refused the window: {code}. No server was created."),
                    ),
                ),
                ApproveOutcome::Expired => (
                    "expired",
                    outcome_text(request_id, "expired", "The 30-minute deadline passed."),
                ),
                ApproveOutcome::AlreadyDecided => (
                    "already_decided",
                    outcome_text(request_id, "not applied", "The card was already decided."),
                ),
            };
            note_job(request_id, reason, text)
        }
        _ => note_job(
            request_id,
            "invalid_payload",
            outcome_text(request_id, "not applied", "The decision was malformed."),
        ),
    };
    subs::insert_job(conn, job.owner_id, parent, now, &outcome).await?;
    Ok(JobOutcome::Done(json!({"request_id": request_id})))
}

/// Срок карточки истёк: окна не будет.
pub async fn expire_job(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    parent: Uuid,
    correlation: Uuid,
) -> Result<JobOutcome, SubmitError> {
    let Ok(CloudJob { request_id }) = serde_json::from_value(job.payload.clone()) else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    crate::conversations::lock_owner(conn, job.owner_id).await?;
    if !close_in_tx(
        conn,
        request_id,
        RequestState::Expired,
        "expired",
        correlation,
    )
    .await?
    {
        return Ok(JobOutcome::Done(json!({"skipped": true})));
    }
    let text = outcome_text(
        request_id,
        "expired",
        "The 30-minute deadline passed; no window was started.",
    );
    subs::insert_job(
        conn,
        job.owner_id,
        parent,
        Utc::now(),
        &note_job(request_id, "expired", text),
    )
    .await?;
    Ok(JobOutcome::Done(
        json!({"request_id": request_id, "outcome": "expired"}),
    ))
}

/// Задание исчерпало попытки: карточку нельзя оставлять в `decision_saved`.
pub(crate) async fn abandon_in_tx(
    conn: &mut PgConnection,
    conversation: Uuid,
    payload: &Value,
    correlation: Uuid,
) -> Result<(), sqlx::Error> {
    let Ok(CloudJob { request_id }) = serde_json::from_value(payload.clone()) else {
        return Ok(());
    };
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(conversation)
        .fetch_one(&mut *conn)
        .await?;
    let next = if close_in_tx(
        conn,
        request_id,
        RequestState::DecisionNotApplied,
        "job_failed",
        correlation,
    )
    .await?
        || close_in_tx(
            conn,
            request_id,
            RequestState::Expired,
            "expired",
            correlation,
        )
        .await?
    {
        RequestState::DecisionNotApplied
    } else {
        return Ok(());
    };
    let _ = next;
    let text = outcome_text(
        request_id,
        "not applied",
        "The hub could not finish the cloud window decision.",
    );
    subs::insert_job(
        conn,
        owner,
        conversation,
        Utc::now(),
        &note_job(request_id, "job_failed", text),
    )
    .await?;
    Ok(())
}

/// Служебный разговор для карточек владельца и хаба: у них нет своего исполнения, но строка
/// `requests` требует conversation_id и execution_id. Разговор переиспользуется.
pub async fn system_proposer(
    conn: &mut PgConnection,
    owner_id: Uuid,
    machine_id: Uuid,
) -> Result<Proposer, sqlx::Error> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "select id from conversations where owner_id=$1 and machine_id=$2 and harness=$3 order by created_at limit 1",
    )
    .bind(owner_id)
    .bind(machine_id)
    .bind(SYSTEM_HARNESS)
    .fetch_optional(&mut *conn)
    .await?;
    let conversation = match existing {
        Some(id) => id,
        None => create_system_conversation(conn, owner_id, machine_id).await?,
    };
    let execution: Option<(Uuid, i64)> = sqlx::query_as(
        "select id, generation from executions where conversation_id=$1 order by generation desc limit 1",
    )
    .bind(conversation)
    .fetch_optional(&mut *conn)
    .await?;
    let (execution_id, generation) = match execution {
        Some(row) => row,
        None => {
            let execution_id = Uuid::now_v7();
            sqlx::query(
                "insert into executions (id, conversation_id, generation, state)
                 values ($1, $2, 0, 'stopped')",
            )
            .bind(execution_id)
            .bind(conversation)
            .execute(&mut *conn)
            .await?;
            (execution_id, 0)
        }
    };
    Ok(Proposer {
        source: Source::Owner,
        owner_id,
        conversation_id: conversation,
        execution_id,
        generation,
    })
}

async fn create_system_conversation(
    conn: &mut PgConnection,
    owner_id: Uuid,
    machine_id: Uuid,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into conversations (id, owner_id, machine_id, harness, workdir, title, title_source)
         values ($1, $2, $3, $4, '/srv/canterlot-runner', 'Облачные окна', 'auto')",
    )
    .bind(id)
    .bind(owner_id)
    .bind(machine_id)
    .bind(SYSTEM_HARNESS)
    .execute(&mut *conn)
    .await?;
    sqlx::query("insert into queue_state (conversation_id, hold_reasons) values ($1, '[]'::jsonb)")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "insert into conversation_derived (conversation_id, hold_reasons, activity_evidence)
         values ($1, '[]'::jsonb, '{\"kind\":\"idle\",\"last_turn_at\":null,\"background_work\":null}'::jsonb)",
    )
    .bind(id)
    .execute(&mut *conn)
    .await?;
    Ok(id)
}
