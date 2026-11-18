//! Лента обновлений CLI и моделей: курсорная страница по владельцу с необязательными
//! фильтрами по машине и виду события.

use axum::{Json, extract::State};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::QueryBuilder;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use super::{
    AppState,
    auth::Owner,
    error::{ApiError, ErrorCode},
    extract::ApiQuery,
};

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list))
}

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 100;

#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct UpdatesQuery {
    /// 1–100, по умолчанию 50.
    pub limit: Option<i64>,
    /// `next_cursor` предыдущей страницы.
    pub cursor: Option<String>,
    /// Имя машины; необязательно.
    pub machine: Option<String>,
    /// Вид события; необязательно.
    pub kind: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct UpdateView {
    pub id: Uuid,
    pub kind: String,
    /// Имя машины из каталога владельца.
    pub machine: String,
    pub harness: String,
    pub created_at: String,
    pub payload: Value,
    /// Команда обновления для видов версий; хаб её не выполняет.
    #[schema(value_type=Option<String>)]
    pub update_command: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct UpdatePage {
    pub items: Vec<UpdateView>,
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy)]
struct Cursor {
    at: DateTime<Utc>,
    id: Uuid,
}

impl Cursor {
    fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(format!(
            "{}:{}",
            self.at.timestamp_micros(),
            self.id.simple()
        ))
    }

    fn decode(value: &str) -> Option<Self> {
        let raw = String::from_utf8(URL_SAFE_NO_PAD.decode(value).ok()?).ok()?;
        let (at, id) = raw.split_once(':')?;
        Some(Self {
            at: DateTime::from_timestamp_micros(at.parse().ok()?)?,
            id: Uuid::parse_str(id).ok()?,
        })
    }
}

fn invalid() -> ApiError {
    ApiError::new(ErrorCode::InvalidRequest)
}

async fn page(
    pool: &sqlx::PgPool,
    owner: Uuid,
    query: &UpdatesQuery,
) -> Result<UpdatePage, sqlx::Error> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    let cursor = query.cursor.as_deref().map(Cursor::decode);
    let mut sql = QueryBuilder::new(
        "select u.id,u.kind,m.name,u.harness,u.created_at,u.payload
         from machine_updates u join machines m on m.id=u.machine_id
         where u.owner_id=",
    );
    sql.push_bind(owner);
    if let Some(machine) = &query.machine {
        sql.push(" and m.name=").push_bind(machine);
    }
    if let Some(kind) = &query.kind {
        sql.push(" and u.kind=").push_bind(kind);
    }
    if let Some(Some(cursor)) = cursor {
        sql.push(" and (u.created_at,u.id)<(")
            .push_bind(cursor.at)
            .push(",")
            .push_bind(cursor.id)
            .push(")");
    }
    sql.push(" order by u.created_at desc,u.id desc limit ")
        .push_bind(limit + 1);
    let rows: Vec<(Uuid, String, String, String, DateTime<Utc>, Value)> =
        sql.build_query_as().fetch_all(pool).await?;
    let more = rows.len() as i64 > limit;
    let last_cursor = rows.get((limit - 1) as usize).map(|row| Cursor {
        at: row.4,
        id: row.0,
    });
    let items: Vec<UpdateView> = rows
        .into_iter()
        .take(limit as usize)
        .map(|(id, kind, machine, harness, at, payload)| UpdateView {
            id,
            update_command: payload
                .get("update_command")
                .and_then(Value::as_str)
                .map(str::to_owned),
            kind,
            machine,
            harness,
            created_at: crate::conversations::time(at),
            payload,
        })
        .collect();
    // Курсор строится по последней выданной записи с полной точностью времени:
    // округление до миллисекунд пропустило бы соседние строки.
    let next_cursor = more.then(|| last_cursor.expect("a full page").encode());
    Ok(UpdatePage { items, next_cursor })
}

#[utoipa::path(
    get,
    path="/api/v1/updates",
    tag="machines",
    params(UpdatesQuery),
    responses(
        (status=200,description="Лента обновлений CLI и моделей, свежие первыми",body=UpdatePage),
        (status=400,description="Неверный курсор, фильтр или размер страницы",body=super::error::ErrorEnvelope),
        (status=401,description="Требуется вход владельца",body=super::error::ErrorEnvelope)
    )
)]
async fn list(
    State(state): State<AppState>,
    owner: Owner,
    ApiQuery(query): ApiQuery<UpdatesQuery>,
) -> Result<Json<UpdatePage>, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(invalid());
    }
    if query
        .cursor
        .as_deref()
        .is_some_and(|c| Cursor::decode(c).is_none())
    {
        return Err(invalid());
    }
    if query
        .kind
        .as_deref()
        .is_some_and(|kind| !crate::machine_updates::ALL_KINDS.contains(&kind))
    {
        return Err(invalid());
    }
    if query.machine.as_deref().is_some_and(str::is_empty) {
        return Err(invalid());
    }
    Ok(Json(
        page(&state.pool, owner.owner_id, &query)
            .await
            .map_err(ApiError::from)?,
    ))
}
