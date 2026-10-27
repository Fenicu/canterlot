use axum::{
    extract::{
        State,
        ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    },
    response::Response,
};
use utoipa_axum::{router::OpenApiRouter, routes};

use super::{
    AppState,
    auth::Owner,
    error::{ApiError, ErrorCode},
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(upgrade))
}

#[utoipa::path(get, path="/api/v1/ws", tag="events", responses((status=101, description="WSS canterlot.v1; кадры ClientOp / ServerOp в events.schema.json")))]
async fn upgrade(
    State(state): State<AppState>,
    owner: Owner,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    let ws = ws
        .map_err(|_| ApiError::new(ErrorCode::InvalidRequest))?
        .protocols(["canterlot.v1"]);
    if ws.selected_protocol().is_none() {
        return Err(ApiError::new(ErrorCode::InvalidRequest));
    }
    let token = state.ws_tasks.token();
    if state.shutdown.is_cancelled() {
        return Err(ApiError::new(ErrorCode::DbUnavailable));
    }
    Ok(ws
        .max_message_size(4096)
        .max_frame_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(1024 * 1024)
        .on_upgrade(move |socket| async move {
            let _token = token;
            crate::subscriptions::handle_socket(state, owner, socket).await;
        }))
}
