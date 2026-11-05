//! HTTP-маршруты пресетов инструкций (план 2c, задача 2).
//!
//! `GET /api/v1/presets` отдаёт сводки без текста; текст есть только в детальном
//! ответе пользовательского пресета. Изменяющие запросы идут с `Idempotency-Key`.

use super::{
    AppState,
    auth::{Correlation, IdempotencyKey, Owner},
    error::ApiError,
    extract::{ApiJson, ApiPath, ApiQuery},
};
use crate::presets::{
    self, NewPreset, Preset, PresetPatch, PresetPurpose, PresetRef, PresetSummary,
};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

/// Назначение пресета: инструкции по умолчанию, `nudge` — вкладка текста толчка.
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct PurposeQuery {
    pub purpose: Option<PresetPurpose>,
}

impl PurposeQuery {
    fn value(&self) -> PresetPurpose {
        self.purpose.unwrap_or_default()
    }
}

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
    #[serde(default)]
    pub purpose: PresetPurpose,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PresetPatchBody {
    pub title: Option<String>,
    pub description: Option<String>,
    pub text: Option<String>,
    pub base_revision: i64,
}

#[utoipa::path(get,path="/api/v1/presets",tag="presets",params(PurposeQuery),responses((status=200,description="Пресеты владельца без текста",body=Vec<PresetSummary>)))]
async fn list(
    State(s): State<AppState>,
    o: Owner,
    ApiQuery(q): ApiQuery<PurposeQuery>,
) -> Result<Json<Vec<PresetSummary>>, ApiError> {
    Ok(Json(
        presets::list_for(&mut *s.pool.acquire().await?, o.owner_id, q.value()).await?,
    ))
}

#[utoipa::path(get,path="/api/v1/presets/{id}",tag="presets",params(("id"=String,Path),PurposeQuery),responses((status=200,description="Пресет; текст только у пользовательского",body=Preset)))]
async fn get_one(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<String>,
    ApiQuery(q): ApiQuery<PurposeQuery>,
) -> Result<Json<Preset>, ApiError> {
    let purpose = q.value();
    if purpose == PresetPurpose::Nudge && id == presets::BUILTIN_NUDGE_ID {
        return Ok(Json(presets::builtin_nudge_preset()));
    }
    let reference = PresetRef::parse(&id)?;
    Ok(Json(
        presets::get_for(
            &mut *s.pool.acquire().await?,
            o.owner_id,
            &reference,
            purpose,
        )
        .await?,
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
        purpose: b.purpose,
    };
    Ok((
        StatusCode::CREATED,
        Json(presets::create(&s.pool, o.owner_id, &key, body, presets::Actor::Owner, c).await?),
    ))
}

#[utoipa::path(patch,path="/api/v1/presets/{id}",tag="presets",params(("id"=String,Path),("Idempotency-Key"=String,Header),PurposeQuery),request_body=PresetPatchBody,responses((status=200,description="Пресет изменён",body=Preset)))]
async fn update(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<String>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiQuery(q): ApiQuery<PurposeQuery>,
    ApiJson(b): ApiJson<PresetPatchBody>,
) -> Result<Json<Preset>, ApiError> {
    let purpose = q.value();
    if purpose == PresetPurpose::Nudge && id == presets::BUILTIN_NUDGE_ID {
        return Err(presets::PresetError::Builtin.into());
    }
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
        presets::update_for(
            &s.pool,
            o.owner_id,
            uuid,
            &key,
            patch,
            presets::Actor::Owner,
            c,
            purpose,
        )
        .await?,
    ))
}

#[utoipa::path(delete,path="/api/v1/presets/{id}",tag="presets",params(("id"=String,Path),("Idempotency-Key"=String,Header),PurposeQuery),responses((status=204,description="Пресет удалён, сессии держат копию")))]
async fn delete(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<String>,
    IdempotencyKey(key): IdempotencyKey,
    Correlation(c): Correlation,
    ApiQuery(q): ApiQuery<PurposeQuery>,
) -> Result<StatusCode, ApiError> {
    let purpose = q.value();
    if purpose == PresetPurpose::Nudge && id == presets::BUILTIN_NUDGE_ID {
        return Err(presets::PresetError::Builtin.into());
    }
    let PresetRef::Custom(uuid) = PresetRef::parse(&id)? else {
        return Err(presets::PresetError::Builtin.into());
    };
    presets::delete_for(&s.pool, o.owner_id, uuid, &key, c, purpose).await?;
    Ok(StatusCode::NO_CONTENT)
}
