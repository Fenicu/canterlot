use std::time::Duration;

use axum::{
    Json,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use canterlot_proto::limits::{
    MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS_PER_INPUT, MAX_INPUT_META_BYTES, MAX_INPUT_TEXT_BYTES,
};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState, UPLOAD_CHUNK_BYTES,
    auth::Owner,
    error::{ApiError, ErrorEnvelope},
    openapi,
};
use crate::{dberr, delivery::QUEUE_LIMIT, installation};

const READY_TIMEOUT: Duration = Duration::from_secs(1);

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(healthz))
        .routes(routes!(readyz))
        .routes(routes!(me))
        .routes(routes!(config))
        .routes(routes!(openapi_json))
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Ok,
    NotReady,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProbeReason {
    Database,
    Service,
}

#[derive(Serialize, ToSchema)]
pub struct ProbeView {
    pub status: ProbeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub reason: Option<ProbeReason>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<&'static str>,
}

/// Живость процесса: без личности и без обращения к базе.
#[utoipa::path(get, path = "/healthz", tag = "health", responses(
    (status = 200, description = "Процесс жив", body = ProbeView),
))]
async fn healthz() -> Json<ProbeView> {
    Json(ProbeView {
        status: ProbeStatus::Ok,
        reason: None,
        services: Vec::new(),
    })
}

/// Готовность: база отвечает, а запись установки совпадает с состоянием процесса и манифестом.
#[utoipa::path(get, path = "/readyz", tag = "health", responses(
    (status = 200, description = "Хаб готов", body = ProbeView),
    (status = 503, description = "База или фоновая служба недоступна, либо установка не совпадает", body = ProbeView),
))]
async fn readyz(State(state): State<AppState>) -> Response {
    let check = async {
        sqlx::query_scalar::<_, i32>("select 1")
            .fetch_one(&state.pool)
            .await
            .map_err(|error| dberr::describe(&error).to_string())?;
        let stored = installation::check(&state.pool, &state.manifest)
            .await
            .map_err(|error| match error {
                installation::InstallationError::Db(error) => dberr::describe(&error).to_string(),
                other => other.to_string(),
            })?;
        if stored == state.installation {
            Ok(())
        } else {
            Err("installation_changed".to_owned())
        }
    };
    match tokio::time::timeout(READY_TIMEOUT, check).await {
        Ok(Ok(())) => {
            let services = crate::singleton::down_services(&state.pool);
            if services.is_empty() {
                Json(ProbeView {
                    status: ProbeStatus::Ok,
                    reason: None,
                    services,
                })
                .into_response()
            } else {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ProbeView {
                        status: ProbeStatus::NotReady,
                        reason: Some(ProbeReason::Service),
                        services,
                    }),
                )
                    .into_response()
            }
        }
        outcome => {
            let reason = match outcome {
                Ok(Err(reason)) => reason,
                _ => "timeout".to_owned(),
            };
            tracing::warn!(kind = "readyz_failed", reason, "hub is not ready");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ProbeView {
                    status: ProbeStatus::NotReady,
                    reason: Some(ProbeReason::Database),
                    services: Vec::new(),
                }),
            )
                .into_response()
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct MeView {
    pub owner_id: Uuid,
    pub login: String,
}

/// Личность владельца; PWA привязывает к `owner_id` локальные данные.
#[utoipa::path(get, path = "/api/v1/me", tag = "session", responses(
    (status = 200, description = "Владелец", body = MeView),
    (status = 401, description = "Нет личности", body = ErrorEnvelope),
    (status = 403, description = "Чужой пользователь", body = ErrorEnvelope),
))]
async fn me(owner: Owner) -> Json<MeView> {
    Json(MeView {
        owner_id: owner.owner_id,
        login: owner.login,
    })
}

#[derive(Serialize, ToSchema)]
pub struct LimitsView {
    pub max_input_text_bytes: u64,
    pub max_input_meta_bytes: u64,
    pub max_attachment_bytes: u64,
    pub max_attachments_per_input: u64,
    pub queue_limit: u64,
    pub upload_chunk_bytes: u64,
}

#[derive(Serialize, ToSchema)]
pub struct WsView {
    pub path: &'static str,
    pub protocol: &'static str,
    pub delta_window_ms: u64,
}

#[derive(Serialize, ToSchema)]
#[schema(as = ActivityConfigView)]
pub struct ActivityView {
    pub stalled_generating_secs: u64,
    pub stalled_tool_secs: u64,
}

#[derive(Serialize, ToSchema)]
pub struct MachinesView {
    pub disk_warn_pct: u8,
    pub metrics_stale_after_secs: u64,
}

#[derive(Serialize, ToSchema)]
pub struct LimitThresholdsView {
    /// Действующий резерв как расход, %: совместимо с прежним полем (`100 - reserve`).
    pub weekly_reserve_pct: u16,
    /// Устарело: раньше первый порог окрашивал полосу. Оставлено для старых клиентов.
    pub warn_pcts: Vec<u16>,
    /// Жёлтый, когда остаток меньше этого процента.
    pub warn_remaining_pct: u16,
    /// Красный, когда остаток меньше этого процента.
    pub danger_remaining_pct: u16,
    /// Сколько недельного остатка владелец оставляет себе; `0` — без резерва.
    pub reserve_remaining_pct: u16,
    pub stale_after_secs: u64,
}

#[derive(Serialize, ToSchema)]
pub struct DefaultsView {
    pub crunch_resume_text: String,
}

#[derive(Serialize, ToSchema)]
pub struct AuthView {
    pub logout_url: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ConfigView {
    pub version: String,
    pub schema_version: u32,
    pub limits: LimitsView,
    pub ws: WsView,
    pub activity: ActivityView,
    pub machines: MachinesView,
    pub limit_thresholds: LimitThresholdsView,
    pub defaults: DefaultsView,
    pub auth: AuthView,
}

/// Лимиты и пороги, которыми клиент проверяет ввод до отправки.
#[utoipa::path(get, path = "/api/v1/config", tag = "session", responses(
    (status = 200, description = "Настройки для клиента", body = ConfigView),
    (status = 401, description = "Нет личности", body = ErrorEnvelope),
    (status = 403, description = "Чужой пользователь", body = ErrorEnvelope),
))]
async fn config(o: Owner, State(state): State<AppState>) -> Result<Json<ConfigView>, ApiError> {
    let settings = crate::limit_settings::effective(
        &mut *state.pool.acquire().await?,
        o.owner_id,
        &state.limits,
    )
    .await?;
    Ok(Json(ConfigView {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        schema_version: 1,
        limits: LimitsView {
            max_input_text_bytes: MAX_INPUT_TEXT_BYTES as u64,
            max_input_meta_bytes: MAX_INPUT_META_BYTES as u64,
            max_attachment_bytes: MAX_ATTACHMENT_BYTES,
            max_attachments_per_input: MAX_ATTACHMENTS_PER_INPUT as u64,
            queue_limit: QUEUE_LIMIT as u64,
            upload_chunk_bytes: UPLOAD_CHUNK_BYTES,
        },
        ws: WsView {
            path: "/api/v1/ws",
            protocol: "canterlot.v1",
            delta_window_ms: state.http.ws.delta_window.as_millis() as u64,
        },
        activity: ActivityView {
            stalled_generating_secs: state.activity.stalled_generating.as_secs(),
            stalled_tool_secs: state.activity.stalled_tool.as_secs(),
        },
        machines: MachinesView {
            disk_warn_pct: state.machines.disk_warn_pct,
            metrics_stale_after_secs: state.machines.metrics_stale_after.as_secs(),
        },
        limit_thresholds: LimitThresholdsView {
            weekly_reserve_pct: settings.reserve_used_pct(),
            warn_pcts: state.limits.warn_pcts.clone(),
            warn_remaining_pct: settings.warn_remaining_pct,
            danger_remaining_pct: settings.danger_remaining_pct,
            reserve_remaining_pct: settings.reserve_remaining_pct,
            stale_after_secs: state.limits.stale_after.as_secs(),
        },
        defaults: DefaultsView {
            crunch_resume_text: state.limits.default_resume_text.clone(),
        },
        auth: AuthView {
            logout_url: state.http.logout_url.clone(),
        },
    }))
}

/// Машиночитаемый контракт API (`docs/api/openapi.json`).
#[utoipa::path(get, path = "/api/v1/openapi.json", tag = "session", responses(
    (status = 200, description = "Документ OpenAPI", content_type = "application/json"),
    (status = 401, description = "Нет личности", body = ErrorEnvelope),
    (status = 403, description = "Чужой пользователь", body = ErrorEnvelope),
))]
async fn openapi_json(_owner: Owner) -> Result<Response, ApiError> {
    Ok((
        [(header::CONTENT_TYPE, "application/json")],
        openapi::rendered(),
    )
        .into_response())
}
