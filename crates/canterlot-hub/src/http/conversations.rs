use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::{ApiError, ErrorCode},
    extract::{ApiJson, ApiPath, ApiQuery},
};
use crate::conversations::{
    self, Attributes, ConversationDetail, Created, CrunchSetting, ListCursor, ListFilter, ListPage,
    ListQuery, NewConversation,
};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(get, update, remove))
        .routes(routes!(set_groups))
        .routes(routes!(set_crunch))
        .routes(routes!(read))
        .routes(routes!(decision))
        .routes(routes!(interrupt))
        .routes(routes!(compact))
        .routes(routes!(stop))
        .routes(routes!(continue_queue))
        .routes(routes!(resolve_owner_lost))
        .routes(routes!(cancel_input))
        .routes(routes!(settings))
        .routes(routes!(submit))
        .routes(routes!(start_execution))
        .routes(routes!(resume))
}
/// Удалённый разговор отвечает своим кодом: клиент не ждёт его возвращения и не отправляет ввод.
pub(super) async fn deleted_or<T>(
    s: &AppState,
    owner: Uuid,
    id: Uuid,
    result: Result<T, ApiError>,
) -> Result<T, ApiError> {
    match result {
        Err(error)
            if error.code == ErrorCode::NotFound
                && crate::deletion::tombstoned(&s.pool, owner, id).await? =>
        {
            Err(ApiError::new(ErrorCode::ConversationDeleted))
        }
        other => other,
    }
}

/// Живой сеанс без `instructions_v1` запустил бы не-владельческий пресет как «Как у
/// владельца»; без сеанса проверку повторяет outbox перед отправкой старта.
async fn require_instructions_capability(
    s: &AppState,
    owner: Uuid,
    id: Uuid,
) -> Result<(), ApiError> {
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "select machine_id, instruction_profile from conversations where id=$1 and owner_id=$2",
    )
    .bind(id)
    .bind(owner)
    .fetch_optional(&s.pool)
    .await?;
    if let Some((machine, profile)) = row
        && profile != "owner"
        && s.registry.session_info(machine).is_some()
        && !s
            .registry
            .has_capability(machine, crate::session::CAPABILITY_INSTRUCTIONS_V1)
    {
        return Err(ApiError::new(ErrorCode::CapabilityMissing));
    }
    Ok(())
}

#[derive(Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResumeBody {
    #[serde(default)]
    continue_queue: bool,
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/executions",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=201,description="Исполнение создано",body=crate::executions::StartedExecution)))]
async fn start_execution(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<(StatusCode, Json<crate::executions::StartedExecution>), ApiError> {
    require_instructions_capability(&s, o.owner_id, id).await?;
    let value = crate::executions::start_with_key(
        &s.pool,
        o.owner_id,
        id,
        &key,
        (false, false),
        c,
        #[cfg(feature = "fault-injection")]
        &s.scheduler.faults,
    )
    .await
    .map_err(ApiError::from);
    let value = deleted_or(&s, o.owner_id, id, value).await?;
    s.dispatcher.trigger(id);
    Ok((StatusCode::CREATED, Json(value)))
}
#[utoipa::path(post,path="/api/v1/conversations/{id}/resume",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=ResumeBody,responses((status=201,description="Исполнение возобновлено",body=crate::executions::StartedExecution)))]
async fn resume(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(body): ApiJson<ResumeBody>,
) -> Result<(StatusCode, Json<crate::executions::StartedExecution>), ApiError> {
    require_instructions_capability(&s, o.owner_id, id).await?;
    let value = crate::executions::start_with_key(
        &s.pool,
        o.owner_id,
        id,
        &key,
        (true, body.continue_queue),
        c,
        #[cfg(feature = "fault-injection")]
        &s.scheduler.faults,
    )
    .await
    .map_err(ApiError::from);
    let value = deleted_or(&s, o.owner_id, id, value).await?;
    s.dispatcher.trigger(id);
    Ok((StatusCode::CREATED, Json(value)))
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitBody {
    content: Vec<crate::delivery::ContentBlock>,
    #[serde(default)]
    attachment_ids: Vec<Uuid>,
    retry_of: Option<Uuid>,
}
#[utoipa::path(post,path="/api/v1/conversations/{id}/inputs",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=SubmitBody,responses((status=201,description="Сообщение в очереди",body=crate::delivery::SubmittedInput)))]
async fn submit(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(body): ApiJson<SubmitBody>,
) -> Result<(StatusCode, Json<crate::delivery::SubmittedInput>), ApiError> {
    let result = conversations::submit_message(
        &s.pool,
        conversations::SubmitMessage {
            owner_id: o.owner_id,
            conversation_id: id,
            key,
            content: body.content,
            attachment_ids: body.attachment_ids,
            retry_of: body.retry_of,
            received_at: chrono::Utc::now(),
        },
        c,
    )
    .await
    .map_err(ApiError::from);
    let result = deleted_or(&s, o.owner_id, id, result).await?;
    s.dispatcher.trigger(id);
    Ok((StatusCode::CREATED, Json(result)))
}
#[utoipa::path(patch,path="/api/v1/conversations/{id}/settings",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=conversations::SettingsPatch,responses((status=200,description="Настройки изменены",body=conversations::SettingsView)))]
async fn settings(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(patch): ApiJson<conversations::SettingsPatch>,
) -> Result<Json<conversations::SettingsView>, ApiError> {
    Ok(Json(
        conversations::change_settings(&s.pool, o.owner_id, id, &key, patch, c).await?,
    ))
}
#[derive(Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StopBody {
    #[serde(default)]
    force: bool,
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/interrupt",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=200,description="Остановка запрошена",body=crate::delivery::control::StopOutcome)))]
async fn interrupt(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<Json<crate::delivery::control::StopOutcome>, ApiError> {
    let result = crate::delivery::control::request_stop(&s.pool, o.owner_id, id, &key, c).await?;
    s.dispatcher.trigger(id);
    Ok(Json(result))
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/compact",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=200,description="Сжатие контекста запрошено",body=crate::delivery::control::CompactOutcome)))]
async fn compact(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<Json<crate::delivery::control::CompactOutcome>, ApiError> {
    let result =
        crate::delivery::control::request_compaction(&s.pool, o.owner_id, id, &key, c).await?;
    Ok(Json(result))
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/stop",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=StopBody,responses((status=200,description="Остановка исполнения запрошена",body=crate::delivery::control::StopExecutionOutcome)))]
async fn stop(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(body): ApiJson<StopBody>,
) -> Result<Json<crate::delivery::control::StopExecutionOutcome>, ApiError> {
    let result = crate::delivery::control::request_stop_execution(
        &s.pool, o.owner_id, id, body.force, &key, c,
    )
    .await?;
    s.dispatcher.trigger(id);
    Ok(Json(result))
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/resolve-owner-lost",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=200,description="Решение по потерянному исполнению передано машине",body=crate::delivery::control::ResolveOwnerLostOutcome)))]
async fn resolve_owner_lost(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
) -> Result<Json<crate::delivery::control::ResolveOwnerLostOutcome>, ApiError> {
    let result =
        crate::delivery::control::request_resolve_owner_lost(&s.pool, o.owner_id, id, &key).await?;
    Ok(Json(result))
}

#[utoipa::path(post,path="/api/v1/conversations/{id}/queue/continue",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=200,description="Очередь продолжена",body=crate::delivery::control::ContinueOutcome)))]
async fn continue_queue(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<Json<crate::delivery::control::ContinueOutcome>, ApiError> {
    let result = crate::delivery::control::continue_queue(&s.pool, o.owner_id, id, &key, c).await?;
    s.dispatcher.trigger(id);
    Ok(Json(result))
}

#[utoipa::path(post,path="/api/v1/inputs/{id}/cancel",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),responses((status=200,description="Отмена запрошена",body=crate::delivery::control::CancelOutcome)))]
async fn cancel_input(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<Json<crate::delivery::control::CancelOutcome>, ApiError> {
    let conversation: Option<Uuid> =
        sqlx::query_scalar("select conversation_id from inputs where id=$1 and owner_id=$2")
            .bind(id)
            .bind(o.owner_id)
            .fetch_optional(&s.pool)
            .await?;
    let result = crate::delivery::control::cancel_input(&s.pool, o.owner_id, id, &key, c).await?;
    if let Some(id) = conversation {
        s.dispatcher.trigger(id);
    }
    Ok(Json(result))
}
#[derive(Clone, PartialEq, Eq, Deserialize, serde::Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionBody {
    Permission {
        behavior: DecisionBehavior,
        message: Option<String>,
    },
    Question {
        answers: std::collections::BTreeMap<String, DecisionAnswer>,
    },
}

impl std::fmt::Debug for DecisionBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DecisionBody([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, serde::Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionBehavior {
    Allow,
    Deny,
}

#[derive(Clone, PartialEq, Eq, Deserialize, serde::Serialize, ToSchema)]
#[serde(untagged)]
pub enum DecisionAnswer {
    Single(String),
    Multiple(Vec<String>),
}

impl From<DecisionBody> for canterlot_proto::requests::DecisionPayload {
    fn from(body: DecisionBody) -> Self {
        use canterlot_proto::requests::{AnswerValue, PermissionBehavior};
        match body {
            DecisionBody::Permission { behavior, message } => Self::Permission {
                behavior: match behavior {
                    DecisionBehavior::Allow => PermissionBehavior::Allow,
                    DecisionBehavior::Deny => PermissionBehavior::Deny,
                },
                message,
            },
            DecisionBody::Question { answers } => Self::Question {
                answers: answers
                    .into_iter()
                    .map(|(id, answer)| {
                        (
                            id,
                            match answer {
                                DecisionAnswer::Single(value) => AnswerValue::Single(value),
                                DecisionAnswer::Multiple(values) => AnswerValue::Multiple(values),
                            },
                        )
                    })
                    .collect(),
            },
        }
    }
}

#[derive(Deserialize, serde::Serialize, ToSchema)]
pub struct DecisionOutcomeView {
    pub request_id: Uuid,
    pub winner_decision_id: Uuid,
    #[schema(pattern = "^decision_saved$")]
    pub state: String,
    pub decision: DecisionBody,
}

fn invalid_decision(question_id: Option<&str>) -> ApiError {
    ApiError::new(ErrorCode::DecisionInvalid).with_details(super::error::ErrorDetails {
        question_id: question_id.map(str::to_owned),
        ..Default::default()
    })
}

fn validate_decision(
    row: &crate::requests::RequestRow,
    decision: &DecisionBody,
) -> Result<(), ApiError> {
    use crate::requests::RequestKind;
    match (row.kind, decision) {
        // jsonb не принимает \u0000: без проверки решение падало бы в 500.
        (
            RequestKind::Permission | RequestKind::SpawnApproval,
            DecisionBody::Permission { message, .. },
        ) if message.as_deref().is_some_and(|text| text.contains('\0')) => {
            Err(invalid_decision(None))
        }
        (RequestKind::Permission | RequestKind::SpawnApproval, DecisionBody::Permission { .. }) => {
            Ok(())
        }
        (RequestKind::Question, DecisionBody::Question { answers }) => {
            let questions = row
                .request_payload
                .get("questions")
                .and_then(serde_json::Value::as_array)
                .filter(|q| (1..=4).contains(&q.len()))
                .ok_or_else(|| invalid_decision(None))?;
            let mut ids = std::collections::BTreeSet::new();
            for question in questions {
                let id = question
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| invalid_decision(None))?;
                if !ids.insert(id) {
                    return Err(invalid_decision(Some(id)));
                }
                let answer = answers.get(id).ok_or_else(|| invalid_decision(Some(id)))?;
                let multi = ["multiSelect", "multi_select"]
                    .iter()
                    .any(|key| question.get(key) == Some(&serde_json::Value::Bool(true)));
                let custom = ["allow_custom", "isOther", "is_other"]
                    .iter()
                    .any(|key| question.get(key) == Some(&serde_json::Value::Bool(true)));
                let values = match answer {
                    DecisionAnswer::Single(value) if !multi => std::slice::from_ref(value),
                    DecisionAnswer::Multiple(values) if multi && !values.is_empty() => {
                        values.as_slice()
                    }
                    _ => return Err(invalid_decision(Some(id))),
                };
                let mut selected = std::collections::BTreeSet::new();
                for value in values {
                    let known = question
                        .get("options")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|options| {
                            options.iter().any(|option| {
                                option.get("label").and_then(serde_json::Value::as_str)
                                    == Some(value.as_str())
                            })
                        });
                    if value.is_empty()
                        || value.contains('\0')
                        || !selected.insert(value)
                        || (!known && !custom)
                    {
                        return Err(invalid_decision(Some(id)));
                    }
                }
            }
            if let Some(id) = answers.keys().find(|id| !ids.contains(id.as_str())) {
                return Err(invalid_decision(Some(id)));
            }
            Ok(())
        }
        _ => Err(invalid_decision(None)),
    }
}

#[utoipa::path(
    post, path="/api/v1/conversations/{id}/requests/{request_id}/decision", tag="conversations",
    params(("id"=Uuid,Path),("request_id"=Uuid,Path),("Idempotency-Key"=String,Header),("X-Canterlot-Client"=String,Header,description="1")),
    request_body=DecisionBody,
    responses(
        (status=200,description="Решение сохранено; повтор возвращает первый ответ",body=DecisionOutcomeView),
        (status=400,description="Некорректный запрос",body=super::error::ErrorEnvelope),
        (status=404,description="Запрос или сессия не найдены",body=super::error::ErrorEnvelope),
        (status=409,description="decision_conflict, request_not_open или idempotency_conflict",body=super::error::ErrorEnvelope),
        (status=422,description="decision_invalid: ответ не подходит к контракту",body=super::error::ErrorEnvelope)
    )
)]
async fn decision(
    State(s): State<AppState>,
    o: Owner,
    ApiPath((id, request_id)): ApiPath<(Uuid, Uuid)>,
    IdempotencyKey(key): IdempotencyKey,
    ApiJson(body): ApiJson<DecisionBody>,
) -> Result<Json<DecisionOutcomeView>, ApiError> {
    use crate::{
        idempotency,
        requests::{self, DecisionResult, RequestDomainError, RequestRow, RequestState},
    };
    use sha2::{Digest, Sha256};

    let operation = "request_decision";
    let hash = idempotency::request_hash(operation, &(id, request_id, &body));
    // Пустой повтор тоже фиксируется: записей нет, блокировка снята в конце.
    let (view, trigger) = crate::db::in_tx(&s.pool, move |conn| {
        Box::pin(async move {
            let mut lock = Sha256::new();
            lock.update(b"canterlot:idempotency:");
            lock.update(o.owner_id.as_bytes());
            lock.update(key.as_bytes());
            let lock_id = i64::from_be_bytes(lock.finalize()[..8].try_into().expect("eight bytes"));
            sqlx::query("select pg_advisory_xact_lock($1)")
                .bind(lock_id)
                .execute(&mut *conn)
                .await?;
            // Пакет SJ держит очередь с первой записи: без неё решение встретило бы его
            // на строке запроса и счётчике событий в обратном порядке.
            sqlx::query(
                "select q.conversation_id from queue_state q
                 join conversations c on c.id = q.conversation_id
                 where c.id=$1 and c.owner_id=$2 for update of q",
            )
            .bind(id)
            .bind(o.owner_id)
            .fetch_optional(&mut *conn)
            .await?;
            let owned: Option<Uuid> = sqlx::query_scalar(
                "select id from conversations where id=$1 and owner_id=$2 for no key update",
            )
            .bind(id)
            .bind(o.owner_id)
            .fetch_optional(&mut *conn)
            .await?;
            if owned.is_none() {
                return Err(ApiError::new(ErrorCode::NotFound));
            }
            match idempotency::lookup(&mut *conn, o.owner_id, &key, operation, &hash).await? {
                idempotency::Begin::Replay(value) => {
                    let view = serde_json::from_value(value)
                        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
                    return Ok((view, false));
                }
                idempotency::Begin::Conflict => {
                    return Err(ApiError::new(ErrorCode::IdempotencyConflict));
                }
                idempotency::Begin::New => {}
            }
            let row: RequestRow = sqlx::query_as(
                "select * from requests where id=$1 and conversation_id=$2 for update",
            )
            .bind(request_id)
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound))?;
            if row.state == RequestState::Open {
                validate_decision(&row, &body)?;
            }
            let decision = body.into();
            let outcome = requests::save_decision_in_tx(
                &mut *conn,
                id,
                request_id,
                row.generation,
                &decision,
                &key,
            )
            .await
            .map_err(|error| match error {
                RequestDomainError::NotFound(_) | RequestDomainError::ConversationNotFound(_) => {
                    ApiError::new(ErrorCode::NotFound)
                }
                RequestDomainError::Database(error) => error.into(),
                RequestDomainError::Serialization(_) => ApiError::new(ErrorCode::Internal),
            })?;
            let row = match outcome {
                DecisionResult::Committed { row, .. } => row,
                DecisionResult::IdempotentReplay { row } => {
                    // The request outlives the generic idempotency key's retention period.
                    let submitted = serde_json::to_value(&decision)
                        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
                    let submitted = requests::redact_decision(&row.request_payload, &submitted);
                    if row.decision_payload.as_ref() != Some(&submitted) {
                        return Err(ApiError::new(ErrorCode::IdempotencyConflict));
                    }
                    row
                }
                DecisionResult::Conflict {
                    winner_decision,
                    current_state,
                } => {
                    let winner_decision = serde_json::from_value(winner_decision)
                        .map_err(|_| ApiError::new(ErrorCode::Internal))?;
                    return Err(ApiError::new(ErrorCode::DecisionConflict).with_details(
                        super::error::ErrorDetails {
                            winner_decision: Some(winner_decision),
                            current_state: Some(current_state.as_str().into()),
                            ..Default::default()
                        },
                    ));
                }
                DecisionResult::AlreadyClosed {
                    current_state,
                    reason,
                } => {
                    return Err(ApiError::new(ErrorCode::RequestNotOpen).with_details(
                        super::error::ErrorDetails {
                            current_state: Some(current_state.as_str().into()),
                            reason: Some(reason),
                            ..Default::default()
                        },
                    ));
                }
                DecisionResult::StaleGeneration => {
                    return Err(ApiError::new(ErrorCode::RequestNotOpen).with_details(
                        super::error::ErrorDetails {
                            current_state: Some(row.state.as_str().into()),
                            reason: Some("generation_advanced".into()),
                            ..Default::default()
                        },
                    ));
                }
            };
            let view = DecisionOutcomeView {
                request_id: row.id,
                winner_decision_id: row
                    .winner_decision_id
                    .ok_or_else(|| ApiError::new(ErrorCode::Internal))?,
                state: "decision_saved".into(),
                decision: serde_json::from_value(
                    row.decision_payload
                        .ok_or_else(|| ApiError::new(ErrorCode::Internal))?,
                )
                .map_err(|_| ApiError::new(ErrorCode::Internal))?,
            };
            let response =
                serde_json::to_value(&view).map_err(|_| ApiError::new(ErrorCode::Internal))?;
            if !idempotency::record(
                &mut *conn,
                o.owner_id,
                &key,
                operation,
                &hash,
                &response,
                Some(id),
            )
            .await?
            {
                return Err(ApiError::new(ErrorCode::IdempotencyConflict));
            }
            Ok((view, true))
        })
    })
    .await?;
    if trigger {
        s.dispatcher.trigger(id);
        // Решение об одобрении запуска ждёт в заданиях, а не в команде машине.
        crate::scheduler::notify_pool(&s.pool);
    }
    Ok(Json(view))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateBody {
    pub machine_id: Uuid,
    pub harness: String,
    pub workdir: String,
    pub model: String,
    pub effort: String,
    pub permission_mode: Option<String>,
    pub instruction_profile: Option<String>,
    pub title: Option<String>,
    pub color: Option<String>,
    #[serde(default)]
    pub group_ids: Vec<Uuid>,
    #[serde(default)]
    pub crunch: CrunchSetting,
    #[serde(default)]
    pub start: bool,
}
#[derive(Default, Deserialize, IntoParams)]
pub struct ListParams {
    pub filter: Option<String>,
    pub q: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupsBody {
    pub group_ids: Vec<Uuid>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadBody {
    #[schema(pattern = "^[0-9]+$")]
    pub seq: String,
}
#[utoipa::path(post,path="/api/v1/conversations",tag="conversations",params(("Idempotency-Key"=String,Header)),request_body=CreateBody,responses((status=201,description="Сессия создана",body=Created)))]
async fn create(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<CreateBody>,
) -> Result<(StatusCode, Json<Created>), ApiError> {
    let registry = s.registry.clone();
    let result = conversations::create(
        &s.pool,
        &key,
        NewConversation {
            owner_id: o.owner_id,
            machine_id: b.machine_id,
            harness: b.harness,
            workdir: b.workdir,
            model: b.model,
            effort: b.effort,
            permission_mode: b.permission_mode,
            instruction_profile: b.instruction_profile,
            title: b.title,
            color: b.color,
            group_ids: b.group_ids,
            crunch: b.crunch,
            start: b.start,
            parent: None,
            worktree_source: None,
        },
        &s.limits,
        c,
        &registry,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(result)))
}
#[utoipa::path(get,path="/api/v1/conversations",tag="conversations",params(ListParams),responses((status=200,description="Список сессий",body=ListPage)))]
async fn list(
    State(s): State<AppState>,
    o: Owner,
    ApiQuery(p): ApiQuery<ListParams>,
) -> Result<Json<ListPage>, ApiError> {
    let filter = match p.filter.as_deref().unwrap_or("all") {
        "all" => ListFilter::All,
        "pinned" => ListFilter::Pinned,
        "archived" => ListFilter::Archived,
        other => ListFilter::Group(
            other
                .strip_prefix("group:")
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| ApiError::new(ErrorCode::InvalidRequest))?,
        ),
    };
    let cursor = p
        .cursor
        .as_deref()
        .map(|v| ListCursor::decode(v).ok_or_else(|| ApiError::new(ErrorCode::InvalidRequest)))
        .transpose()?;
    let result = crate::db::in_tx(&s.pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            if let ListFilter::Group(id) = filter {
                let owned: bool = sqlx::query_scalar(
                    "select exists(select 1 from conversation_groups where owner_id=$1 and id=$2)",
                )
                .bind(o.owner_id)
                .bind(id)
                .fetch_one(&mut *conn)
                .await?;
                if !owned {
                    return Err(ApiError::new(ErrorCode::NotFound));
                }
            }
            conversations::list(
                &mut *conn,
                &ListQuery {
                    owner_id: o.owner_id,
                    filter,
                    q: p.q,
                    cursor,
                    limit: p.limit.unwrap_or(50),
                },
            )
            .await
            .map_err(query_error)
        })
    })
    .await?;
    Ok(Json(result))
}
pub(super) fn query_error(e: sqlx::Error) -> ApiError {
    if matches!(e, sqlx::Error::InvalidArgument(_)) {
        ApiError::new(ErrorCode::InvalidRequest)
    } else {
        e.into()
    }
}
#[utoipa::path(get,path="/api/v1/conversations/{id}",tag="conversations",params(("id"=Uuid,Path)),responses((status=200,description="Сессия",body=ConversationDetail)))]
async fn get(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<ConversationDetail>, ApiError> {
    let d = crate::db::in_tx(&s.pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            conversations::detail(&mut *conn, o.owner_id, id)
                .await?
                .ok_or_else(|| ApiError::new(ErrorCode::NotFound))
        })
    })
    .await;
    let mut d = deleted_or(&s, o.owner_id, id, d).await?;
    if !s.registry.is_reconciled(d.attributes.machine.id) {
        d.holds.push(crate::model::HoldView::MachineNotReconciled);
    }
    if !s
        .registry
        .has_capability(d.attributes.machine.id, "pilot_v1")
        && !d
            .holds
            .iter()
            .any(|h| matches!(h, crate::model::HoldView::CapabilityMissing { .. }))
    {
        let mut conn = s.pool.acquire().await?;
        let input = conversations::capability_hold_input(&mut conn, id).await?;
        if let Some(input_id) = input {
            d.holds
                .push(crate::model::HoldView::CapabilityMissing { input_id });
            if let Some(reasons) = d.queue.hold_reasons.as_array_mut() {
                reasons
                    .push(serde_json::json!({"reason":"capability_missing","input_id":input_id}));
            }
        }
    }
    Ok(Json(d))
}
#[utoipa::path(patch,path="/api/v1/conversations/{id}",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=Attributes,responses((status=200,description="Атрибуты изменены",body=ConversationDetail)))]
async fn update(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<Attributes>,
) -> Result<Json<ConversationDetail>, ApiError> {
    Ok(Json(
        conversations::update_attributes(&s.pool, o.owner_id, id, &key, b, c).await?,
    ))
}
#[utoipa::path(
    delete, path="/api/v1/conversations/{id}", tag="conversations",
    params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),
    responses(
        (status=204,description="Сессия удалена; повтор с тем же ключом даёт тот же ответ"),
        (status=404,description="not_found или conversation_deleted",body=super::error::ErrorEnvelope),
        (status=409,description="conversation_live: исполнение не остановлено",body=super::error::ErrorEnvelope)
    )
)]
async fn remove(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<StatusCode, ApiError> {
    crate::deletion::delete(&s.pool, &*s.blobs, o.owner_id, id, &key, c).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(put,path="/api/v1/conversations/{id}/groups",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=GroupsBody,responses((status=200,description="Группы изменены",body=ConversationDetail)))]
async fn set_groups(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<GroupsBody>,
) -> Result<Json<ConversationDetail>, ApiError> {
    Ok(Json(
        conversations::set_groups(&s.pool, o.owner_id, id, &key, b.group_ids, c).await?,
    ))
}
#[utoipa::path(put,path="/api/v1/conversations/{id}/crunch",tag="conversations",params(("id"=Uuid,Path),("Idempotency-Key"=String,Header)),request_body=CrunchSetting,responses((status=200,description="Кранч изменён",body=ConversationDetail)))]
async fn set_crunch(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<CrunchSetting>,
) -> Result<Json<ConversationDetail>, ApiError> {
    Ok(Json(
        conversations::set_crunch(&s.pool, o.owner_id, id, &key, b, &s.limits, c).await?,
    ))
}
#[utoipa::path(post,path="/api/v1/conversations/{id}/read",tag="conversations",params(("id"=Uuid,Path)),request_body=ReadBody,responses((status=204,description="Прочитанность сохранена")))]
async fn read(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<ReadBody>,
) -> Result<StatusCode, ApiError> {
    if b.seq.is_empty() || !b.seq.bytes().all(|c| c.is_ascii_digit()) {
        return Err(ApiError::new(ErrorCode::InvalidRequest));
    }
    let seq = b
        .seq
        .parse()
        .map_err(|_| ApiError::new(ErrorCode::InvalidRequest))?;
    conversations::mark_read(
        &s.pool,
        o.owner_id,
        id,
        seq,
        &crate::activity::Thresholds::from(&*s.activity),
        s.machines.metrics_stale_after,
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
