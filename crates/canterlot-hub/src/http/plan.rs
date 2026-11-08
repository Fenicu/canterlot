//! HTTP-чтение доски плана оркестратора владельцем: для глубоких ссылок и перезагрузки.

use super::{
    AppState,
    auth::Owner,
    error::{ApiError, ErrorCode},
    extract::ApiQuery,
};
use crate::session_tools::plan;
use axum::{Json, extract::State};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PlanParams {
    /// Разговор оркестратора.
    pub orchestrator: Uuid,
}

#[derive(serde::Serialize, ToSchema)]
pub struct PlanList {
    /// Задачи по номерам с выведенными полями исполнителя.
    pub tasks: Vec<plan::PlanTaskDetail>,
}

#[utoipa::path(get,path="/api/v1/plan",tag="plan",params(PlanParams),responses(
    (status=200,description="Доска плана оркестратора с коммитами и отчётом исполнителей",body=PlanList),
    (status=404,description="Разговор не найден",body=super::error::ErrorEnvelope)
))]
async fn list(
    State(state): State<AppState>,
    owner: Owner,
    ApiQuery(p): ApiQuery<PlanParams>,
) -> Result<Json<PlanList>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let found: bool = sqlx::query_scalar(
        "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
    )
    .bind(p.orchestrator)
    .bind(owner.owner_id)
    .fetch_one(&mut *conn)
    .await?;
    if !found {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    let rows = plan::rows_of(&mut conn, owner.owner_id, p.orchestrator).await?;
    Ok(Json(PlanList {
        tasks: plan::details(&mut conn, &rows).await?,
    }))
}
