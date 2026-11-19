//! API владельца для общей памяти: список, чтение, запись, правка с версией, архив,
//! возврат, удаление навсегда и прошлые версии. Каждый маршрут требует явный `domain`
//! (`work` или `personal`); домены в ответах не смешиваются. Весь SQL живёт в `crate::memory`.
//!
//! Изменения идут без `Idempotency-Key`: повтор правки отсекает `expected_version`, повтор
//! создания даёт `subject_exists`, повтор удаления даёт `not_found`.

use super::{
    AppState,
    auth::Owner,
    error::{ApiError, ErrorCode, ErrorDetails},
    extract::{ApiJson, ApiPath, ApiQuery},
};
use crate::{
    db,
    memory::{
        MemoryDomain, secrets,
        store::{self, Actor, Entry, Kind, Level, MemoryScope, NewEntry, OwnerList, Patch},
    },
};
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

const DEFAULT_PAGE: u32 = 50;
const MAX_PAGE: u32 = 100;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list, create))
        .routes(routes!(get_one, update, hard_delete))
        .routes(routes!(archive))
        .routes(routes!(restore))
        .routes(routes!(revisions))
}

/// Запись памяти для экрана владельца.
#[derive(Debug, Serialize, ToSchema)]
pub struct MemoryEntryView {
    pub id: Uuid,
    /// `work` или `personal`.
    pub domain: String,
    /// `domain`: для всего домена; `project`: для одного проекта.
    pub level: String,
    /// Канонический рабочий каталог проекта; `None` у записи домена.
    pub project: Option<String>,
    pub project_label: Option<String>,
    pub kind: String,
    pub subject: String,
    pub body: String,
    /// `active` или `archived`.
    pub status: String,
    pub valid_until: Option<DateTime<Utc>>,
    pub expired: bool,
    pub version: i32,
    /// Кто создал запись: `{kind: owner | agent | import, ...}`.
    pub created_by: Value,
    pub created_at: DateTime<Utc>,
    /// Кто менял запись последним, в той же форме.
    pub updated_by: Value,
    pub updated_at: DateTime<Utc>,
}

impl From<Entry> for MemoryEntryView {
    fn from(entry: Entry) -> Self {
        Self {
            level: entry.level().as_str().to_owned(),
            project_label: entry.project_label(),
            id: entry.id,
            domain: entry.domain,
            project: entry.project,
            kind: entry.kind,
            subject: entry.subject,
            body: entry.body,
            status: entry.status,
            valid_until: entry.valid_until,
            expired: entry.expired,
            version: entry.version,
            created_by: entry.created_by,
            created_at: entry.created_at,
            updated_by: entry.updated_by,
            updated_at: entry.updated_at,
        }
    }
}

/// Страница списка: новые записи первыми.
#[derive(Debug, Serialize, ToSchema)]
pub struct MemoryListPage {
    pub items: Vec<MemoryEntryView>,
    /// Передайте в `cursor`, чтобы получить следующую страницу; `None` на последней.
    pub next_cursor: Option<String>,
}

/// Прошлое состояние записи.
#[derive(Debug, Serialize, ToSchema)]
pub struct MemoryRevisionView {
    pub id: i64,
    pub version: i32,
    /// `update`, `archive` или `restore`: действие, которое сохранило это состояние.
    pub action: String,
    pub project: Option<String>,
    pub kind: String,
    pub subject: String,
    pub body: String,
    pub status: String,
    pub valid_until: Option<DateTime<Utc>>,
    /// Кто сохранил это состояние, в форме `created_by`.
    pub actor: Value,
    pub at: DateTime<Utc>,
}

fn domain_of(raw: Option<&str>) -> Result<MemoryDomain, ApiError> {
    raw.and_then(MemoryDomain::parse).ok_or_else(|| {
        ApiError::new(ErrorCode::InvalidRequest).with_details(ErrorDetails {
            field: Some("domain".into()),
            ..Default::default()
        })
    })
}

fn invalid(field: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidRequest).with_details(ErrorDetails {
        field: Some(field.to_owned()),
        ..Default::default()
    })
}

fn parse_kind(raw: &str) -> Result<Kind, ApiError> {
    Kind::parse(raw).ok_or_else(|| invalid("kind"))
}

fn parse_level(raw: &str) -> Result<Level, ApiError> {
    Level::parse(raw).ok_or_else(|| invalid("level"))
}

fn parse_time(raw: &str) -> Result<DateTime<Utc>, ApiError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|_| invalid("valid_until"))
}

/// Детектор секретов для владельца: отказ с классом находки, пока нет `force_secret_warning`.
/// Значение находки наружу не уходит.
fn screen(force: Option<bool>, texts: &[&str]) -> Result<(), ApiError> {
    if force == Some(true) {
        return Ok(());
    }
    for text in texts {
        if let Some(class) = secrets::scan(text) {
            return Err(
                ApiError::new(ErrorCode::LooksLikeSecret).with_details(ErrorDetails {
                    class: Some(class.as_str().to_owned()),
                    ..Default::default()
                }),
            );
        }
    }
    Ok(())
}

fn cursor_encode(entry: &Entry) -> String {
    format!("{}_{}", entry.updated_at.timestamp_micros(), entry.id)
}

fn cursor_decode(raw: &str) -> Result<(DateTime<Utc>, Uuid), ApiError> {
    let (micros, id) = raw.split_once('_').ok_or_else(|| invalid("cursor"))?;
    let at = micros
        .parse::<i64>()
        .ok()
        .and_then(DateTime::from_timestamp_micros)
        .ok_or_else(|| invalid("cursor"))?;
    let id = Uuid::parse_str(id).map_err(|_| invalid("cursor"))?;
    Ok((at, id))
}

fn parse_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| ApiError::new(ErrorCode::NotFound))
}

fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct ListQuery {
    /// Обязателен: `work` или `personal`.
    pub domain: Option<String>,
    /// Только записи этого проекта (канонический каталог).
    pub project: Option<String>,
    pub kind: Option<String>,
    /// Поиск по теме и тексту.
    pub q: Option<String>,
    /// `active` (по умолчанию) или `archived`.
    pub status: Option<String>,
    pub cursor: Option<String>,
    /// До 100, по умолчанию 50.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct DomainQuery {
    /// Обязателен: `work` или `personal`.
    pub domain: Option<String>,
}

#[utoipa::path(get,path="/api/v1/memory",tag="memory",params(ListQuery),responses((status=200,description="Записи одного домена",body=MemoryListPage)))]
async fn list(
    State(s): State<AppState>,
    _o: Owner,
    ApiQuery(q): ApiQuery<ListQuery>,
) -> Result<Json<MemoryListPage>, ApiError> {
    let domain = domain_of(q.domain.as_deref())?;
    let archived = match q.status.as_deref() {
        None | Some("active") => false,
        Some("archived") => true,
        Some(_) => return Err(invalid("status")),
    };
    let kind = q.kind.as_deref().map(parse_kind).transpose()?;
    let after = q.cursor.as_deref().map(cursor_decode).transpose()?;
    let limit = q.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let filter = OwnerList {
        archived,
        kind,
        project: q.project.filter(|project| !project.is_empty()),
        query: q.q.unwrap_or_default(),
        after,
        limit: limit + 1,
    };
    let scope = MemoryScope::owner(domain, None);
    let mut conn = s.pool.acquire().await?;
    let mut rows = store::list(&mut conn, &scope, &filter).await?;
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next_cursor = if more {
        rows.last().map(cursor_encode)
    } else {
        None
    };
    Ok(Json(MemoryListPage {
        items: rows.into_iter().map(Into::into).collect(),
        next_cursor,
    }))
}

#[utoipa::path(get,path="/api/v1/memory/{id}",tag="memory",params(("id"=Uuid,Path),DomainQuery),responses((status=200,description="Запись",body=MemoryEntryView)))]
async fn get_one(
    State(s): State<AppState>,
    _o: Owner,
    ApiPath(id): ApiPath<String>,
    ApiQuery(q): ApiQuery<DomainQuery>,
) -> Result<Json<MemoryEntryView>, ApiError> {
    let domain = domain_of(q.domain.as_deref())?;
    let scope = MemoryScope::owner(domain, None);
    let mut conn = s.pool.acquire().await?;
    let entry = store::get(&mut conn, &scope, parse_id(&id)?, true)
        .await?
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound))?;
    Ok(Json(entry.into()))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateMemoryBody {
    /// Обязателен: `work` или `personal`.
    pub domain: Option<String>,
    /// `domain` или `project`; для `project` нужен `project`.
    pub level: String,
    /// Канонический каталог проекта для записи уровня проекта.
    #[serde(default)]
    pub project: Option<String>,
    pub kind: String,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub valid_until: Option<String>,
    /// Сохранить, даже если текст похож на секрет.
    #[serde(default)]
    pub force_secret_warning: Option<bool>,
}

#[utoipa::path(post,path="/api/v1/memory",tag="memory",request_body=CreateMemoryBody,responses((status=201,description="Запись создана",body=MemoryEntryView)))]
async fn create(
    State(s): State<AppState>,
    _o: Owner,
    ApiJson(b): ApiJson<CreateMemoryBody>,
) -> Result<(StatusCode, Json<MemoryEntryView>), ApiError> {
    let domain = domain_of(b.domain.as_deref())?;
    let new = NewEntry {
        kind: parse_kind(&b.kind)?,
        level: parse_level(&b.level)?,
        valid_until: b.valid_until.as_deref().map(parse_time).transpose()?,
        subject: b.subject,
        body: b.body,
    };
    screen(b.force_secret_warning, &[&new.subject, &new.body])?;
    let scope = MemoryScope::owner(domain, b.project.filter(|project| !project.is_empty()));
    let mut tx = db::begin(&s.pool).await?;
    let written = store::write(&mut tx, &scope, Actor::Owner, new, &s.memory).await?;
    let entry = store::get(&mut tx, &scope, written.id, true)
        .await?
        .ok_or_else(|| ApiError::new(ErrorCode::Internal))?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(entry.into())))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateMemoryBody {
    /// Обязателен: `work` или `personal`.
    pub domain: Option<String>,
    /// Версия, с которой правят; при расхождении `409 version_conflict`.
    pub expected_version: Option<i32>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    /// Проект для перевода записи на уровень проекта.
    #[serde(default)]
    pub project: Option<String>,
    /// Срок действия; `null` снимает срок, отсутствие поля оставляет как было.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<String>, nullable)]
    pub valid_until: Option<Option<String>>,
    #[serde(default)]
    pub force_secret_warning: Option<bool>,
}

#[utoipa::path(put,path="/api/v1/memory/{id}",tag="memory",params(("id"=Uuid,Path)),request_body=UpdateMemoryBody,responses((status=200,description="Запись изменена; версия выросла",body=MemoryEntryView)))]
async fn update(
    State(s): State<AppState>,
    _o: Owner,
    ApiPath(id): ApiPath<String>,
    ApiJson(b): ApiJson<UpdateMemoryBody>,
) -> Result<Json<MemoryEntryView>, ApiError> {
    let domain = domain_of(b.domain.as_deref())?;
    let expected = b
        .expected_version
        .ok_or_else(|| invalid("expected_version"))?;
    let id = parse_id(&id)?;
    let mut texts: Vec<&str> = Vec::new();
    texts.extend(b.subject.as_deref());
    texts.extend(b.body.as_deref());
    screen(b.force_secret_warning, &texts)?;
    let patch = Patch {
        kind: b.kind.as_deref().map(parse_kind).transpose()?,
        subject: b.subject,
        body: b.body,
        level: b.level.as_deref().map(parse_level).transpose()?,
        valid_until: match b.valid_until {
            None => None,
            Some(None) => Some(None),
            Some(Some(raw)) => Some(Some(parse_time(&raw)?)),
        },
    };
    let scope = MemoryScope::owner(domain, b.project.filter(|project| !project.is_empty()));
    let mut tx = db::begin(&s.pool).await?;
    store::update(
        &mut tx,
        &scope,
        Actor::Owner,
        id,
        expected,
        patch,
        &s.memory,
    )
    .await?;
    let entry = store::get(&mut tx, &scope, id, true)
        .await?
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound))?;
    tx.commit().await?;
    Ok(Json(entry.into()))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ArchiveMemoryBody {
    /// Обязателен: `work` или `personal`.
    pub domain: Option<String>,
    pub expected_version: Option<i32>,
}

#[utoipa::path(post,path="/api/v1/memory/{id}/archive",tag="memory",params(("id"=Uuid,Path)),request_body=ArchiveMemoryBody,responses((status=200,description="Запись в архиве",body=MemoryEntryView)))]
async fn archive(
    State(s): State<AppState>,
    _o: Owner,
    ApiPath(id): ApiPath<String>,
    ApiJson(b): ApiJson<ArchiveMemoryBody>,
) -> Result<Json<MemoryEntryView>, ApiError> {
    let domain = domain_of(b.domain.as_deref())?;
    let expected = b
        .expected_version
        .ok_or_else(|| invalid("expected_version"))?;
    let id = parse_id(&id)?;
    let scope = MemoryScope::owner(domain, None);
    let mut tx = db::begin(&s.pool).await?;
    store::archive(&mut tx, &scope, Actor::Owner, id, expected, &s.memory).await?;
    let entry = store::get(&mut tx, &scope, id, true)
        .await?
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound))?;
    tx.commit().await?;
    Ok(Json(entry.into()))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreMemoryBody {
    /// Обязателен: `work` или `personal`.
    pub domain: Option<String>,
}

#[utoipa::path(post,path="/api/v1/memory/{id}/restore",tag="memory",params(("id"=Uuid,Path)),request_body=RestoreMemoryBody,responses((status=200,description="Запись снова активна",body=MemoryEntryView)))]
async fn restore(
    State(s): State<AppState>,
    _o: Owner,
    ApiPath(id): ApiPath<String>,
    ApiJson(b): ApiJson<RestoreMemoryBody>,
) -> Result<Json<MemoryEntryView>, ApiError> {
    let domain = domain_of(b.domain.as_deref())?;
    let id = parse_id(&id)?;
    let scope = MemoryScope::owner(domain, None);
    let mut tx = db::begin(&s.pool).await?;
    store::restore(&mut tx, &scope, Actor::Owner, id, &s.memory).await?;
    let entry = store::get(&mut tx, &scope, id, true)
        .await?
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound))?;
    tx.commit().await?;
    Ok(Json(entry.into()))
}

#[utoipa::path(delete,path="/api/v1/memory/{id}",tag="memory",params(("id"=Uuid,Path),DomainQuery),responses((status=204,description="Запись и её прошлые версии удалены навсегда")))]
async fn hard_delete(
    State(s): State<AppState>,
    _o: Owner,
    ApiPath(id): ApiPath<String>,
    ApiQuery(q): ApiQuery<DomainQuery>,
) -> Result<StatusCode, ApiError> {
    let domain = domain_of(q.domain.as_deref())?;
    let id = parse_id(&id)?;
    let scope = MemoryScope::owner(domain, None);
    let mut tx = db::begin(&s.pool).await?;
    store::hard_delete(&mut tx, &scope, Actor::Owner, id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get,path="/api/v1/memory/{id}/revisions",tag="memory",params(("id"=Uuid,Path),DomainQuery),responses((status=200,description="Прошлые версии записи, новые первыми",body=Vec<MemoryRevisionView>)))]
async fn revisions(
    State(s): State<AppState>,
    _o: Owner,
    ApiPath(id): ApiPath<String>,
    ApiQuery(q): ApiQuery<DomainQuery>,
) -> Result<Json<Vec<MemoryRevisionView>>, ApiError> {
    let domain = domain_of(q.domain.as_deref())?;
    let id = parse_id(&id)?;
    let scope = MemoryScope::owner(domain, None);
    let mut conn = s.pool.acquire().await?;
    let rows = store::revisions(&mut conn, &scope, id).await?;
    Ok(Json(
        rows.into_iter()
            .map(|row| MemoryRevisionView {
                id: row.id,
                version: row.version,
                action: row.action,
                project: row.project,
                kind: row.kind,
                subject: row.subject,
                body: row.body,
                status: row.status,
                valid_until: row.valid_until,
                actor: row.actor,
                at: row.at,
            })
            .collect(),
    ))
}
