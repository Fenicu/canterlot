//! HTTP-маршруты пресетов инструкций (план 2c, задача 2).
//!
//! `GET /api/v1/presets` отдаёт сводки без текста; текст есть только в детальном
//! ответе пользовательского пресета. Изменяющие запросы идут с `Idempotency-Key`.

use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::ApiError,
    extract::{ApiJson, ApiPath},
};
use crate::presets::{self, NewPreset, Preset, PresetPatch, PresetRef, PresetSummary};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get_one, update, delete))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PresetBody {
    pub title: String,
    pub description: Option<String>,
    pub text: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PresetPatchBody {
    pub title: Option<String>,
    pub description: Option<String>,
    pub text: Option<String>,
    pub base_revision: i64,
}

#[utoipa::path(get,path="/api/v1/presets",tag="presets",responses((status=200,description="Пресеты владельца без текста",body=Vec<PresetSummary>)))]
async fn list(State(s): State<AppState>, o: Owner) -> Result<Json<Vec<PresetSummary>>, ApiError> {
    Ok(Json(
        presets::list(&mut *s.pool.acquire().await?, o.owner_id).await?,
    ))
}

#[utoipa::path(get,path="/api/v1/presets/{id}",tag="presets",params(("id"=String,Path)),responses((status=200,description="Пресет; текст только у пользовательского",body=Preset)))]
async fn get_one(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<String>,
) -> Result<Json<Preset>, ApiError> {
    let reference = PresetRef::parse(&id)?;
    Ok(Json(
        presets::get(&mut *s.pool.acquire().await?, o.owner_id, &reference).await?,
    ))
}

#[utoipa::path(post,path="/api/v1/presets",tag="presets",params(("Idempotency-Key"=String,Header)),request_body=PresetBody,responses((status=201,description="Пресет создан",body=Preset)))]
async fn create(
    State(s): State<AppState>,
    o: Owner,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<PresetBody>,
) -> Result<(StatusCode, Json<Preset>), ApiError> {
    let body = NewPreset {
        title: b.title,
        description: b.description,
        text: b.text,
    };
    Ok((
        StatusCode::CREATED,
        Json(presets::create(&s.pool, o.owner_id, &key, body, presets::Actor::Owner, c).await?),
    ))
}

#[utoipa::path(patch,path="/api/v1/presets/{id}",tag="presets",params(("id"=String,Path),("Idempotency-Key"=String,Header)),request_body=PresetPatchBody,responses((status=200,description="Пресет изменён",body=Preset)))]
async fn update(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<String>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiJson(b): ApiJson<PresetPatchBody>,
) -> Result<Json<Preset>, ApiError> {
    let PresetRef::Custom(uuid) = PresetRef::parse(&id)? else {
        return Err(presets::PresetError::Builtin.into());
    };
    let patch = PresetPatch {
        title: b.title,
        description: b.description,
        text: b.text,
        base_revision: b.base_revision,
    };
    Ok(Json(
        presets::update(
            &s.pool,
            o.owner_id,
            uuid,
            &key,
            patch,
            presets::Actor::Owner,
            c,
        )
        .await?,
    ))
}

#[utoipa::path(delete,path="/api/v1/presets/{id}",tag="presets",params(("id"=String,Path),("Idempotency-Key"=String,Header)),responses((status=204,description="Пресет удалён, сессии держат копию")))]
async fn delete(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<String>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
) -> Result<StatusCode, ApiError> {
    let PresetRef::Custom(uuid) = PresetRef::parse(&id)? else {
        return Err(presets::PresetError::Builtin.into());
    };
    presets::delete(&s.pool, o.owner_id, uuid, &key, c).await?;
    Ok(StatusCode::NO_CONTENT)
}
