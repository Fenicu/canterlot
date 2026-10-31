use super::{
    AppState,
    auth::Owner,
    error::{ApiError, ErrorCode},
    extract::{ApiPath, ApiQuery},
};
use crate::conversations;
use axum::{Json, extract::State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, Row};
use std::time::Duration;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list))
        .routes(routes!(get))
        .routes(routes!(recent))
        .routes(routes!(suggest))
}

/// Дольше воркера: его собственный срок 2 с, ответ о пустом списке ещё должен дойти.
const SUGGEST_WAIT: Duration = Duration::from_secs(3);
#[derive(Serialize, ToSchema)]
pub struct MachineView {
    pub id: Uuid,
    pub name: String,
    pub availability: String,
    pub availability_since: Option<String>,
    pub heartbeat_age_ms: Option<u64>,
    pub capabilities: Vec<String>,
    pub os: Option<String>,
    pub allowed_roots: Vec<String>,
    pub home_dir: Option<String>,
    pub worker_version: Option<String>,
    pub containment: Option<String>,
    pub limits_ok: bool,
    pub harnesses: Vec<HarnessView>,
    #[schema(value_type=Option<MetricsView>)]
    pub metrics: Option<Value>,
    pub sessions: Vec<SessionView>,
    pub warnings: Vec<MachineWarning>,
}
#[derive(Serialize, ToSchema)]
pub struct HarnessView {
    pub id: String,
    pub title: String,
    pub provider: String,
    pub images: bool,
    pub settings_midsession: bool,
    pub settings_midsession_effort: bool,
    #[schema(value_type=Vec<String>)]
    pub permission_modes: Value,
    #[schema(value_type=Vec<ModelView>)]
    pub models: Value,
}
#[derive(Serialize, ToSchema)]
pub struct SessionView {
    pub conversation_id: Uuid,
    pub title: String,
    pub activity_state: String,
}
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MachineWarning {
    DiskUsageHigh,
    MetricsStale,
    LimitsMismatch,
    CjJournalStopped,
    SjJournalStopped,
}
#[derive(Serialize, ToSchema)]
pub struct Workdirs {
    pub workdirs: Vec<String>,
}
#[derive(Deserialize, IntoParams)]
pub struct SuggestParams {
    pub path: String,
}
pub(crate) async fn load_stored(
    conn: &mut PgConnection,
    config: &crate::config::MachinesConfig,
    owner: Uuid,
    id: Uuid,
) -> Result<MachineView, sqlx::Error> {
    let r = sqlx::query("select * from machines where id=$1 and owner_id=$2")
        .bind(id)
        .bind(owner)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    let harnesses =
        sqlx::query("select * from machine_harnesses where machine_id=$1 order by harness")
            .bind(id)
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .map(|h| -> Result<HarnessView, sqlx::Error> {
                Ok(HarnessView {
                    id: h.try_get("harness")?,
                    title: h.try_get("title")?,
                    provider: h.try_get("provider")?,
                    images: h.try_get("images")?,
                    settings_midsession: h.try_get("settings_midsession")?,
                    settings_midsession_effort: h.try_get("settings_midsession_effort")?,
                    permission_modes: h.try_get("permission_modes")?,
                    models: h.try_get("models")?,
                })
            })
            .collect::<Result<_, _>>()?;

    let sessions=sqlx::query("select c.id,c.title,coalesce(d.activity_state,'idle') activity_state from conversations c left join conversation_derived d on d.conversation_id=c.id where c.machine_id=$1 and c.owner_id=$2 order by c.id").bind(id).bind(owner).fetch_all(&mut *conn).await?.into_iter().map(|p|->Result<SessionView,sqlx::Error>{Ok(SessionView{conversation_id:p.try_get("id")?,title:p.try_get("title")?,activity_state:p.try_get("activity_state")?})}).collect::<Result<_,_>>()?;
    let metrics=sqlx::query("select to_jsonb(mm)-'machine_id' value, observed_at from machine_metrics mm where machine_id=$1").bind(id).fetch_optional(&mut *conn).await?;
    let mut warnings = Vec::new();
    let metrics = if let Some(m) = metrics {
        let mut v: Value = m.try_get("value")?;
        let at: DateTime<Utc> = m.try_get("observed_at")?;
        v["observed_at"] = json!(conversations::time(at));
        let disk_high = v["disks"].as_array().is_some_and(|ds| {
            ds.iter().any(|d| {
                let total = d["total_bytes"].as_u64().unwrap_or(0);
                let free = d["free_bytes"].as_u64().unwrap_or(total);
                total > 0
                    && u128::from(total.saturating_sub(free)) * 100
                        >= u128::from(total) * u128::from(config.disk_warn_pct)
            })
        });
        if disk_high {
            warnings.push(MachineWarning::DiskUsageHigh)
        }
        if (Utc::now() - at)
            .to_std()
            .is_ok_and(|age| age >= config.metrics_stale_after)
        {
            warnings.push(MachineWarning::MetricsStale)
        }
        Some(v)
    } else {
        warnings.push(MachineWarning::MetricsStale);
        None
    };
    let limits_ok: bool = r.try_get("limits_ok")?;
    if !limits_ok {
        warnings.push(MachineWarning::LimitsMismatch)
    }
    for kind in crate::ingest::stopped_journals(&mut *conn, id).await? {
        warnings.push(if kind == "cj" {
            MachineWarning::CjJournalStopped
        } else {
            MachineWarning::SjJournalStopped
        });
    }
    let roots: sqlx::types::Json<Vec<String>> = r.try_get("allowed_roots")?;
    Ok(MachineView {
        id,
        name: r.try_get("name")?,
        availability: r.try_get("availability")?,
        availability_since: r
            .try_get::<Option<DateTime<Utc>>, _>("availability_at")?
            .map(conversations::time),
        heartbeat_age_ms: None,
        capabilities: r.try_get("capabilities")?,
        os: r.try_get("os")?,
        allowed_roots: roots.0,
        home_dir: r.try_get("home_dir")?,
        worker_version: r.try_get("worker_version")?,
        containment: r.try_get("containment")?,
        limits_ok,
        harnesses,
        metrics,
        sessions,
        warnings,
    })
}
async fn load(
    conn: &mut PgConnection,
    s: &AppState,
    owner: Uuid,
    id: Uuid,
) -> Result<MachineView, ApiError> {
    let mut view = load_stored(conn, &s.machines, owner, id)
        .await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => ApiError::new(ErrorCode::NotFound),
            e => ApiError::from(e),
        })?;
    view.heartbeat_age_ms = s
        .registry
        .session_info(id)
        .map(|i| i.last_heartbeat.elapsed().as_millis().min(u64::MAX as u128) as u64);
    Ok(view)
}
#[utoipa::path(get,path="/api/v1/machines",tag="machines",responses((status=200,description="Машины владельца",body=Vec<MachineView>)))]
async fn list(State(s): State<AppState>, o: Owner) -> Result<Json<Vec<MachineView>>, ApiError> {
    let sc = s.clone();
    let pool = sc.pool.clone();
    let result = crate::db::in_tx(&pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            let ids: Vec<Uuid> =
                sqlx::query_scalar("select id from machines where owner_id=$1 order by name,id")
                    .bind(o.owner_id)
                    .fetch_all(&mut *conn)
                    .await?;
            let mut result = Vec::new();
            for id in ids {
                result.push(load(&mut *conn, &sc, o.owner_id, id).await?);
            }
            Ok::<_, ApiError>(result)
        })
    })
    .await?;
    Ok(Json(result))
}
#[utoipa::path(get,path="/api/v1/machines/{id}",tag="machines",params(("id"=Uuid,Path)),responses((status=200,description="Машина",body=MachineView)))]
async fn get(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<MachineView>, ApiError> {
    let sc = s.clone();
    let pool = sc.pool.clone();
    let view = crate::db::in_tx(&pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            load(&mut *conn, &sc, o.owner_id, id).await
        })
    })
    .await?;
    Ok(Json(view))
}
#[utoipa::path(get,path="/api/v1/machines/{id}/workdirs/recent",tag="machines",params(("id"=Uuid,Path)),responses((status=200,description="Последние каталоги для экрана новой сессии",body=Workdirs)))]
async fn recent(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<Workdirs>, ApiError> {
    let mut conn = s.pool.acquire().await?;
    require_owned(&mut conn, o.owner_id, id).await?;
    Ok(Json(Workdirs {
        workdirs: conversations::recent_workdirs(&mut conn, o.owner_id, id, 3).await?,
    }))
}
#[utoipa::path(get,path="/api/v1/machines/{id}/workdirs/suggest",tag="machines",params(("id"=Uuid,Path),SuggestParams),responses((status=200,description="Существующие каталоги машины по введённому пути; пустой список, если машина не ответила",body=Workdirs)))]
async fn suggest(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(p): ApiQuery<SuggestParams>,
) -> Result<Json<Workdirs>, ApiError> {
    let mut conn = s.pool.acquire().await?;
    require_owned(&mut conn, o.owner_id, id).await?;
    drop(conn);
    if p.path.len() > conversations::MAX_WORKDIR_BYTES {
        return Ok(Json(Workdirs { workdirs: vec![] }));
    }
    Ok(Json(Workdirs {
        workdirs: s.registry.suggest_workdirs(id, &p.path, SUGGEST_WAIT).await,
    }))
}
async fn require_owned(conn: &mut PgConnection, owner: Uuid, id: Uuid) -> Result<(), ApiError> {
    let owned: bool =
        sqlx::query_scalar("select exists(select 1 from machines where id=$1 and owner_id=$2)")
            .bind(id)
            .bind(owner)
            .fetch_one(conn)
            .await?;
    if owned {
        Ok(())
    } else {
        Err(ApiError::new(ErrorCode::NotFound))
    }
}

#[derive(ToSchema)]
pub struct ModelView {
    pub id: String,
    pub title: String,
    pub efforts: Vec<String>,
    /// Модель принимает картинки; `null`: у харнесса это не свойство модели.
    pub images: Option<bool>,
    /// Окно контекста модели в токенах, если харнесс его знает.
    pub context_window: Option<u64>,
}
#[derive(ToSchema)]
pub struct MetricsView {
    pub observed_at: DateTime<Utc>,
    pub machine_time_ms: i64,
    pub cpu_cores: i32,
    pub cpu_permille: i32,
    pub mem_total_bytes: i64,
    pub mem_available_bytes: i64,
    pub load1_centi: i32,
    pub load5_centi: i32,
    pub load15_centi: i32,
    pub disks: Vec<DiskView>,
    pub executions: Vec<ProcessView>,
}
#[derive(ToSchema)]
pub struct DiskView {
    pub label: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
}
#[derive(ToSchema)]
pub struct ProcessView {
    pub execution_id: Uuid,
    pub alive: bool,
    pub cpu_permille: u32,
    pub rss_bytes: u64,
}
