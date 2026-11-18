//! Предложения агента изменить настройки владельца. Агент читает текущее значение и ревизию,
//! предлагает новое значение, а применяет только владелец через HTTP-решение. До применения
//! значение проверяется теми же функциями, что и экран настроек; ревизия защищает от потери
//! правки, сделанной владельцем, пока карточка была открыта.

use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
use chrono::{DateTime, Duration as TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    crunch::JobOutcome,
    delivery::SubmitError,
    events::{self, NewEvent, Scope},
    presets::{self, PresetPatch, PresetPurpose, PresetRef},
    requests::{self, RequestKind, RequestState},
    scheduler::ClaimedJob,
};

use super::{
    Caller, ToolContext, ToolError, ToolErrorCode, origin, routing, send, subs, whitelist,
};

/// Карточка живёт сутки; просроченное предложение закрывается без применения.
pub const EXPIRY: TimeDelta = TimeDelta::hours(24);
const HINT_PENDING: &str = "The owner decides in the app; wait for the outcome message.";

/// Всё, что нужно карточке «до → после»: вид настройки, причина и base revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "setting", rename_all = "snake_case")]
pub enum SettingsProposal {
    Routing {
        before: Option<routing::Policy>,
        after: Option<routing::Policy>,
        reason: String,
        base_revision: i64,
    },
    Whitelist {
        before: Option<Vec<whitelist::WhitelistEntry>>,
        after: Option<Vec<whitelist::WhitelistEntry>>,
        reason: String,
        base_revision: i64,
    },
    Preset {
        preset_id: Uuid,
        title: String,
        before: String,
        after: String,
        reason: String,
        base_revision: i64,
    },
}

/// Описание payload для OpenAPI: точный JSON-контракт карточке #200 задаёт `SettingsProposal`.
#[derive(Debug, ToSchema)]
pub struct SettingsChangePayload {
    /// `routing`, `whitelist` или `preset` — дискриминант `SettingsProposal`.
    pub setting: String,
    pub reason: String,
    pub base_revision: i64,
    /// Текущее значение; у пресета — старый текст.
    pub before: Value,
    /// Предлагаемое значение; у пресета — новый текст.
    pub after: Value,
    /// Только для `preset`.
    pub preset_id: Option<Uuid>,
    /// Только для `preset`: название, под которым владелец узнает пресет.
    pub title: Option<String>,
}

impl SettingsProposal {
    fn base_revision(&self) -> i64 {
        match self {
            Self::Routing { base_revision, .. }
            | Self::Whitelist { base_revision, .. }
            | Self::Preset { base_revision, .. } => *base_revision,
        }
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::with_message(ToolErrorCode::InvalidArguments, message)
}

fn identical() -> ToolError {
    invalid("the proposed value is identical to the current value; nothing to propose")
}

fn pending(id: Uuid) -> Value {
    json!({"request_id": id, "status": "pending_approval", "hint": HINT_PENDING})
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutingArgs {
    policy: Option<routing::Policy>,
    reason: String,
    base_revision: i64,
}

/// `null` — сброс ко встроенному списку; пропущенное поле не сбрасывает молча.
fn required_nullable<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<whitelist::WhitelistEntry>>, D::Error> {
    Option::deserialize(deserializer)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WhitelistArgs {
    #[serde(deserialize_with = "required_nullable")]
    entries: Option<Vec<whitelist::WhitelistEntry>>,
    reason: String,
    base_revision: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetArgs {
    id: String,
    text: String,
    reason: String,
    base_revision: i64,
}

/// Создать карточку, событие в ленте, push владельцу и задание на суточный срок.
async fn open_proposal(
    context: &ToolContext,
    caller: &Caller,
    proposal: SettingsProposal,
) -> Result<Value, ToolError> {
    let mut tx = crate::db::begin(&context.pool).await?;
    send::lock_queue(&mut tx, caller.conversation_id).await?;
    let generation: Option<i64> = sqlx::query_scalar(
        "select generation from executions
          where id=$1 and conversation_id=$2 and state not in ('stopped','failed','owner_lost')",
    )
    .bind(caller.execution_id)
    .bind(caller.conversation_id)
    .fetch_optional(&mut *tx)
    .await?;
    if generation.is_none() {
        return Err(ToolError::new(ToolErrorCode::ConversationClosed));
    }
    let id = Uuid::now_v7();
    let deadline = Utc::now() + EXPIRY;
    let payload =
        serde_json::to_value(&proposal).map_err(|_| ToolError::new(ToolErrorCode::Internal))?;
    requests::create_request_in_tx(
        &mut tx,
        &requests::NewRequest {
            id,
            conversation_id: caller.conversation_id,
            execution_id: caller.execution_id,
            generation: generation.unwrap_or_default(),
            kind: RequestKind::SettingsChange,
            native_request_id: format!("settings:{}", id),
            request_payload: payload.clone(),
            expires_at: Some(deadline),
        },
    )
    .await?;
    let event = events::append(
        &mut tx,
        NewEvent {
            scope: Scope::Conversation {
                id: caller.conversation_id,
            },
            kind: "request_opened",
            conversation_id: Some(caller.conversation_id),
            execution_id: Some(caller.execution_id),
            payload: json!({
                "request_id": id,
                "kind": RequestKind::SettingsChange.as_str(),
                "payload": payload,
            }),
            correlation_id: caller.call_id,
        },
    )
    .await?;
    sqlx::query("update requests set created_seq=$2 where id=$1")
        .bind(id)
        .bind(event.scope_seq)
        .execute(&mut *tx)
        .await?;
    crate::notify::on_request_opened_in_tx(&mut tx, caller.conversation_id, id).await?;
    subs::insert_job(
        &mut tx,
        caller.owner_id,
        caller.conversation_id,
        deadline,
        &json!({"action": "settings_expire", "request_id": id}),
    )
    .await?;
    tx.commit().await?;
    Ok(pending(id))
}

/// Текущая политика владельца и её ревизия — тот же чтение, что у экрана настроек.
pub async fn routing_policy(
    context: &ToolContext,
    _caller: &Caller,
    _arguments: Value,
) -> Result<Value, ToolError> {
    let mut conn = context.pool.acquire().await?;
    let (policy, revision) = routing::policy_view(&mut conn, _caller.owner_id).await?;
    Ok(json!({"policy": policy, "revision": revision, "builtin": routing::builtin()}))
}

/// Текущий список делегирования и его ревизия — тот же чтение, что у экрана настроек.
pub async fn whitelist(
    context: &ToolContext,
    caller: &Caller,
    _arguments: Value,
) -> Result<Value, ToolError> {
    let mut conn = context.pool.acquire().await?;
    let (view, revision) = whitelist::view_with_revision(&mut conn, caller.owner_id).await?;
    Ok(json!({
        "entries": view.entries,
        "is_default": view.is_default,
        "default_entries": view.default_entries,
        "revision": revision,
    }))
}

pub async fn propose_routing(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: RoutingArgs = super::parse_arguments("settings.propose_routing", arguments)?;
    if let Some(policy) = args.policy.as_ref()
        && let Err(message) = routing::validate(policy)
    {
        return Err(invalid(format!("routing_policy_invalid: {message}")));
    }
    let mut conn = context.pool.acquire().await?;
    let (before, revision) = routing::policy_view(&mut conn, caller.owner_id).await?;
    drop(conn);
    if args.base_revision != revision {
        return Err(invalid(
            "the routing policy changed; read settings.routing again",
        ));
    }
    if args.policy == before {
        return Err(identical());
    }
    open_proposal(
        context,
        caller,
        SettingsProposal::Routing {
            before,
            after: args.policy,
            reason: args.reason,
            base_revision: args.base_revision,
        },
    )
    .await
}

pub async fn propose_whitelist(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: WhitelistArgs = super::parse_arguments("settings.propose_whitelist", arguments)?;
    if args
        .entries
        .as_deref()
        .is_some_and(|entries| !whitelist::valid(entries))
    {
        return Err(invalid("spawn_whitelist_invalid: invalid entries"));
    }
    let mut conn = context.pool.acquire().await?;
    let (view, revision) = whitelist::view_with_revision(&mut conn, caller.owner_id).await?;
    drop(conn);
    if args.base_revision != revision {
        return Err(invalid(
            "the spawn whitelist changed; read settings.whitelist again",
        ));
    }
    let before = (!view.is_default).then_some(view.entries);
    if args.entries == before {
        return Err(identical());
    }
    open_proposal(
        context,
        caller,
        SettingsProposal::Whitelist {
            before,
            after: args.entries,
            reason: args.reason,
            base_revision: args.base_revision,
        },
    )
    .await
}

pub async fn propose_preset(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: PresetArgs = super::parse_arguments("settings.propose_preset", arguments)?;
    let reference = PresetRef::parse(&args.id).map_err(|_| invalid("preset_not_found"))?;
    let PresetRef::Custom(id) = reference else {
        return Err(invalid("a builtin preset cannot be changed"));
    };
    presets::validate_text(&args.text, PresetPurpose::Instructions)
        .map_err(|error| invalid(format!("preset_invalid: {error}")))?;
    let mut conn = context.pool.acquire().await?;
    let preset = presets::get_for(
        &mut conn,
        caller.owner_id,
        &reference,
        PresetPurpose::Instructions,
    )
    .await
    .map_err(|_| invalid("preset_not_found"))?;
    let revision = preset.summary.revision.unwrap_or(1);
    drop(conn);
    if args.base_revision != revision {
        return Err(invalid(
            "the preset changed; read canterlot_presets_list again",
        ));
    }
    let before = preset.text.unwrap_or_default();
    if args.text == before {
        return Err(identical());
    }
    open_proposal(
        context,
        caller,
        SettingsProposal::Preset {
            preset_id: id,
            title: preset.summary.title,
            before,
            after: args.text,
            reason: args.reason,
            base_revision: args.base_revision,
        },
    )
    .await
}

/// Вместо команды машине: решение выполнит задание хаба.
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
        &json!({"action": "settings_change", "request_id": request}),
    )
    .await
}

#[derive(Deserialize)]
struct ChangeJob {
    request_id: Uuid,
}

struct Resolution {
    state: RequestState,
    reason: &'static str,
    text: String,
}

fn outcome_text(request: Uuid, outcome: &str, detail: &str) -> String {
    format!(
        "[canterlot] Your settings_change request {request} was {outcome}. {detail} Wait for the owner or start a new proposal."
    )
}

fn note_job(request: Uuid, resolution: &Resolution) -> Value {
    json!({
        "action": "note",
        "text": resolution.text,
        "origin": origin::with_event(
            origin::note_origin(0),
            origin::HubEvent::approval(resolution.reason, request, None, None),
        ),
        "dedupe_key": format!("settings_change:{request}"),
    })
}

async fn current_revision(
    conn: &mut PgConnection,
    owner: Uuid,
    proposal: &SettingsProposal,
) -> Result<i64, sqlx::Error> {
    match proposal {
        SettingsProposal::Routing { .. } => {
            let (_, revision) = routing::policy_view(conn, owner).await?;
            Ok(revision)
        }
        SettingsProposal::Whitelist { .. } => {
            let (_, revision) = whitelist::view_with_revision(conn, owner).await?;
            Ok(revision)
        }
        SettingsProposal::Preset { preset_id, .. } => {
            sqlx::query_scalar("select revision from instruction_presets where owner_id=$1 and id=$2 and deleted_at is null")
                .bind(owner)
                .bind(preset_id)
                .fetch_optional(&mut *conn)
                .await
                .map(|revision| revision.unwrap_or(-1))
        }
    }
}

async fn apply(
    conn: &mut PgConnection,
    owner: Uuid,
    parent: Uuid,
    request: Uuid,
    proposal: SettingsProposal,
    correlation: Uuid,
) -> Result<Resolution, SubmitError> {
    let base = proposal.base_revision();
    let current = current_revision(conn, owner, &proposal).await?;
    if current != base {
        return Ok(Resolution {
            state: RequestState::SettingChanged,
            reason: "setting_changed",
            text: outcome_text(
                request,
                "not applied",
                &format!(
                    "The setting changed while the card was open (base revision {base}, current {current}). Read it again and propose the change again."
                ),
            ),
        });
    }
    let applied = match proposal {
        SettingsProposal::Routing { after, .. } => {
            let revision = match routing::save_policy(conn, owner, after.as_ref()).await {
                Ok(revision) => revision,
                Err(message) => {
                    return Ok(Resolution {
                        state: RequestState::DecisionNotApplied,
                        reason: "invalid_value",
                        text: outcome_text(request, "not applied", &message),
                    });
                }
            };
            events::append(
                conn,
                NewEvent {
                    scope: Scope::Conversations { owner_id: owner },
                    kind: "routing_policy_changed",
                    conversation_id: None,
                    execution_id: None,
                    payload: json!({"revision": revision}),
                    correlation_id: correlation,
                },
            )
            .await?;
            "the routing policy was updated"
        }
        SettingsProposal::Whitelist { after, .. } => {
            whitelist::replace(conn, owner, after.as_deref(), correlation).await?;
            "the spawn whitelist was updated"
        }
        SettingsProposal::Preset {
            preset_id, after, ..
        } => {
            let update = presets::update_in_tx(
                conn,
                owner,
                preset_id,
                PresetPatch {
                    title: None,
                    description: None,
                    text: Some(after),
                    base_revision: base,
                },
                presets::Actor::Agent {
                    conversation_id: parent,
                },
                correlation,
                PresetPurpose::Instructions,
            )
            .await;
            match update {
                Ok(_) => {}
                Err(presets::PresetError::RevisionConflict { .. }) => {
                    return Ok(Resolution {
                        state: RequestState::SettingChanged,
                        reason: "setting_changed",
                        text: outcome_text(
                            request,
                            "not applied",
                            "The preset changed while the card was open. Read it again.",
                        ),
                    });
                }
                Err(error) => {
                    return Ok(Resolution {
                        state: RequestState::DecisionNotApplied,
                        reason: "invalid_value",
                        text: outcome_text(request, "not applied", &error.to_string()),
                    });
                }
            }
            "the instruction preset was updated"
        }
    };
    Ok(Resolution {
        state: RequestState::DecisionApplied,
        reason: "allowed",
        text: outcome_text(request, "allowed", applied),
    })
}

/// Только ожидаемое состояние закрывается: открытый запрос ждёт своего срока.
async fn close_request_in_tx(
    conn: &mut PgConnection,
    parent: Uuid,
    request: Uuid,
    expected: RequestState,
    next: RequestState,
    reason: &str,
    correlation: Uuid,
) -> Result<bool, sqlx::Error> {
    send::lock_queue(conn, parent).await?;
    let row: Option<(RequestState, Uuid)> = sqlx::query_as(
        "select state,execution_id from requests where id=$1 and conversation_id=$2 for update",
    )
    .bind(request)
    .bind(parent)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((state, execution_id)) = row.filter(|(state, _)| *state == expected) else {
        return Ok(false);
    };
    let _ = state;
    sqlx::query("update requests set state=$2,updated_at=clock_timestamp() where id=$1")
        .bind(request)
        .bind(next.as_str())
        .execute(&mut *conn)
        .await?;
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation { id: parent },
            kind: "request_closed",
            conversation_id: Some(parent),
            execution_id: Some(execution_id),
            payload: json!({"request_id": request, "reason": reason}),
            correlation_id: correlation,
        },
    )
    .await?;
    Ok(true)
}

/// Решение владельца: применить с проверкой ревизии или закрыть отказом, затем уведомить.
pub(crate) async fn run_job(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    parent: Uuid,
    now: DateTime<Utc>,
    correlation: Uuid,
) -> Result<JobOutcome, SubmitError> {
    let Ok(ChangeJob { request_id }) = serde_json::from_value(job.payload.clone()) else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    crate::conversations::lock_owner(conn, job.owner_id).await?;
    // Тот же порядок, что у HTTP-решения: очередь, затем строка запроса.
    send::lock_queue(conn, parent).await?;
    let row: Option<(RequestState, Value, Option<Value>)> = sqlx::query_as(
        "select state,request_payload,decision_payload from requests
          where id=$1 and conversation_id=$2 and kind='settings_change' for update",
    )
    .bind(request_id)
    .bind(parent)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((RequestState::DecisionSaved, payload, decision)) = row else {
        return Ok(JobOutcome::Done(json!({"skipped": true})));
    };
    let Ok(proposal) = serde_json::from_value::<SettingsProposal>(payload) else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    let resolution = match decision
        .and_then(|value| serde_json::from_value::<DecisionPayload>(value).ok())
    {
        Some(DecisionPayload::Permission {
            behavior: PermissionBehavior::Deny,
            message,
        }) => {
            let remark = message
                .filter(|text| !text.trim().is_empty())
                .map_or(String::new(), |text| format!(" The owner said: {text}"));
            Resolution {
                state: RequestState::Declined,
                reason: "declined",
                text: format!(
                    "[canterlot] The owner declined your settings_change request {request_id}. Nothing was changed.{remark}"
                ),
            }
        }
        Some(DecisionPayload::Permission {
            behavior: PermissionBehavior::Allow,
            ..
        }) => {
            apply(
                conn,
                job.owner_id,
                parent,
                request_id,
                proposal,
                correlation,
            )
            .await?
        }
        _ => Resolution {
            state: RequestState::DecisionNotApplied,
            reason: "invalid_payload",
            text: outcome_text(request_id, "not applied", "The decision was malformed."),
        },
    };
    close_request_in_tx(
        conn,
        parent,
        request_id,
        RequestState::DecisionSaved,
        resolution.state,
        resolution.reason,
        correlation,
    )
    .await?;
    subs::insert_job(
        conn,
        job.owner_id,
        parent,
        now,
        &note_job(request_id, &resolution),
    )
    .await?;
    Ok(JobOutcome::Done(
        json!({"request_id": request_id, "outcome": resolution.reason}),
    ))
}

/// Срок карточки истёк: решение не пришло, настройка не тронута.
pub(crate) async fn expire_job(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    parent: Uuid,
    correlation: Uuid,
) -> Result<JobOutcome, SubmitError> {
    let Ok(ChangeJob { request_id }) = serde_json::from_value(job.payload.clone()) else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    crate::conversations::lock_owner(conn, job.owner_id).await?;
    if !close_request_in_tx(
        conn,
        parent,
        request_id,
        RequestState::Open,
        RequestState::Expired,
        "expired",
        correlation,
    )
    .await?
    {
        return Ok(JobOutcome::Done(json!({"skipped": true})));
    }
    let resolution = Resolution {
        state: RequestState::Expired,
        reason: "expired",
        text: outcome_text(
            request_id,
            "expired",
            "The 24-hour deadline passed without an owner decision; nothing was changed.",
        ),
    };
    subs::insert_job(
        conn,
        job.owner_id,
        parent,
        Utc::now(),
        &note_job(request_id, &resolution),
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
    let Ok(ChangeJob { request_id }) = serde_json::from_value(payload.clone()) else {
        return Ok(());
    };
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(conversation)
        .fetch_one(&mut *conn)
        .await?;
    let outcome = if close_request_in_tx(
        conn,
        conversation,
        request_id,
        RequestState::DecisionSaved,
        RequestState::DecisionNotApplied,
        "job_failed",
        correlation,
    )
    .await?
    {
        "not applied"
    } else if close_request_in_tx(
        conn,
        conversation,
        request_id,
        RequestState::Open,
        RequestState::Expired,
        "expired",
        correlation,
    )
    .await?
    {
        "expired"
    } else {
        return Ok(());
    };
    let resolution = Resolution {
        state: RequestState::DecisionNotApplied,
        reason: "job_failed",
        text: outcome_text(request_id, outcome, "The hub could not finish the change."),
    };
    subs::insert_job(
        conn,
        owner,
        conversation,
        Utc::now(),
        &note_job(request_id, &resolution),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn proposal_kind_and_revision_round_trip() {
        let proposal = SettingsProposal::Routing {
            before: None,
            after: Some(routing::Policy::default()),
            reason: "reason".into(),
            base_revision: 4,
        };
        let json = serde_json::to_value(&proposal).unwrap();
        assert_eq!(json["setting"], "routing");
        let back: SettingsProposal = serde_json::from_value(json).unwrap();
        assert_eq!(back, proposal);
        assert_eq!(back.base_revision(), 4);
    }

    #[test]
    fn request_kind_and_states_round_trip() {
        assert_eq!(RequestKind::SettingsChange.as_str(), "settings_change");
        assert_eq!(
            RequestKind::from_str("settings_change").unwrap(),
            RequestKind::SettingsChange
        );
        for state in [
            RequestState::SettingChanged,
            RequestState::Declined,
            RequestState::Expired,
        ] {
            assert_eq!(RequestState::from_str(state.as_str()).unwrap(), state);
        }
    }
}
