//! «Добавить машину»: выдача, перевыпуск и отзыв кодов привязки владельцем. Изменяющие
//! маршруты требуют свежего сеанса, как остальные действия управления доступом, и идут мимо
//! слоя идемпотентности: ответ несёт код, а сохранённый для повтора ответ хранил бы его в базе.

use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState,
    auth::{CurrentSession, Owner},
    error::{ApiError, ErrorCode, ErrorEnvelope},
    extract::{ApiJson, ApiPath},
};
use crate::enroll::{self, EnrollError};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(issue))
        .routes(routes!(list_pending))
        .routes(routes!(reissue))
        .routes(routes!(revoke))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinCodeBody {
    /// Имя новой машины, до 64 знаков.
    pub name: String,
}

/// Код показывается один раз; `command` кода не содержит.
#[derive(Serialize, ToSchema)]
pub struct JoinCodeView {
    pub machine_id: Uuid,
    /// 26 знаков Crockford base32 группами по 4 через дефис.
    pub code: String,
    pub expires_at: DateTime<Utc>,
    /// `curl … | bash -s -- join --hub <адрес> --pin sha256:<hex>`.
    pub command: String,
}

#[derive(Serialize, ToSchema)]
pub struct PendingMachineView {
    pub machine_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// `null`: код уже обменян, воркер ещё не подключился.
    pub code_expires_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
pub struct PendingMachinesView {
    pub items: Vec<PendingMachineView>,
}

impl From<EnrollError> for ApiError {
    fn from(error: EnrollError) -> Self {
        match error {
            EnrollError::NotFound => ApiError::new(ErrorCode::MachineNotFound),
            EnrollError::NotPending => ApiError::new(ErrorCode::MachineNotPending),
            EnrollError::InvalidName => ApiError::new(ErrorCode::InvalidRequest),
            // Адрес и ссылка проверены при загрузке конфигурации; сюда попадает только чужое.
            EnrollError::InvalidEndpoint | EnrollError::InvalidInstallUrl => {
                ApiError::new(ErrorCode::EnrollEndpointMissing)
            }
            EnrollError::Database(error) => error.into(),
            EnrollError::Invalid | EnrollError::Random => ApiError::new(ErrorCode::Internal),
        }
    }
}

/// CA и адрес, без которых команду не напечатать.
fn tail(state: &AppState) -> Result<(&[u8], &str), ApiError> {
    let ca = state
        .enroll
        .ca_der
        .as_deref()
        .ok_or_else(|| ApiError::new(ErrorCode::CaUnavailable))?;
    let endpoint = state
        .enroll
        .grpc_endpoint
        .as_deref()
        .ok_or_else(|| ApiError::new(ErrorCode::EnrollEndpointMissing))?;
    Ok((ca, endpoint))
}

fn view(code: enroll::JoinCode) -> JoinCodeView {
    JoinCodeView {
        machine_id: code.machine_id,
        code: code.code,
        expires_at: code.expires_at,
        command: code.command,
    }
}

/// Заводит машину «ожидает подключения» и выдаёт одноразовый код на 15 минут.
#[utoipa::path(post, path = "/api/v1/machines/join-codes", tag = "machines", request_body = JoinCodeBody, responses(
    (status = 200, description = "Машина заведена, код выдан", body = JoinCodeView),
    (status = 400, description = "invalid_request: пустое или слишком длинное имя", body = ErrorEnvelope),
    (status = 403, description = "reauth_required", body = ErrorEnvelope),
    (status = 409, description = "ca_unavailable или enroll_endpoint_missing", body = ErrorEnvelope),
))]
async fn issue(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    ApiJson(body): ApiJson<JoinCodeBody>,
) -> Result<Json<JoinCodeView>, ApiError> {
    session.require_fresh()?;
    let (ca, endpoint) = tail(&state)?;
    let code = enroll::issue(
        &state.pool,
        owner.owner_id,
        &body.name,
        endpoint,
        &state.enroll.install_url,
        ca,
    )
    .await?;
    Ok(Json(view(code)))
}

/// Машины, которые ждут подключения по коду. Кодов в ответе нет.
#[utoipa::path(get, path = "/api/v1/machines/pending", tag = "machines", responses(
    (status = 200, description = "Ожидающие машины, старые первыми", body = PendingMachinesView),
))]
async fn list_pending(
    State(state): State<AppState>,
    owner: Owner,
) -> Result<Json<PendingMachinesView>, ApiError> {
    let items = enroll::list_pending(&state.pool, owner.owner_id)
        .await?
        .into_iter()
        .map(|machine| PendingMachineView {
            machine_id: machine.machine_id,
            name: machine.name,
            created_at: machine.created_at,
            code_expires_at: machine.code_expires_at,
        })
        .collect();
    Ok(Json(PendingMachinesView { items }))
}

/// «Новый код»: прежний код и токен из потерянного ответа перестают действовать.
#[utoipa::path(post, path = "/api/v1/machines/{id}/join-code", tag = "machines", params(("id" = Uuid, Path)), responses(
    (status = 200, description = "Новый код той же машины", body = JoinCodeView),
    (status = 403, description = "reauth_required", body = ErrorEnvelope),
    (status = 404, description = "machine_not_found", body = ErrorEnvelope),
    (status = 409, description = "machine_not_pending, ca_unavailable или enroll_endpoint_missing", body = ErrorEnvelope),
))]
async fn reissue(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<JoinCodeView>, ApiError> {
    session.require_fresh()?;
    let (ca, endpoint) = tail(&state)?;
    let code = enroll::reissue(
        &state.pool,
        owner.owner_id,
        id,
        endpoint,
        &state.enroll.install_url,
        ca,
    )
    .await?;
    Ok(Json(view(code)))
}

/// «Отозвать»: ожидающая машина удаляется вместе с кодом и токеном.
#[utoipa::path(delete, path = "/api/v1/machines/{id}/pending", tag = "machines", params(("id" = Uuid, Path)), responses(
    (status = 204, description = "Ожидающая машина отозвана"),
    (status = 403, description = "reauth_required", body = ErrorEnvelope),
    (status = 404, description = "machine_not_found", body = ErrorEnvelope),
    (status = 409, description = "machine_not_pending", body = ErrorEnvelope),
))]
async fn revoke(
    State(state): State<AppState>,
    owner: Owner,
    session: CurrentSession,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<StatusCode, ApiError> {
    session.require_fresh()?;
    enroll::revoke_pending(&state.pool, owner.owner_id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
