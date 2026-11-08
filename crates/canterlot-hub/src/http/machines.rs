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
        .routes(routes!(claude_histories))
        .routes(routes!(owner_overview))
        .routes(routes!(owner_file))
}

/// Дольше воркера: его собственный срок 2 с, ответ о пустом списке ещё должен дойти.
const SUGGEST_WAIT: Duration = Duration::from_secs(3);
/// Чтение файла владельца: воркер читает с диска, срок с запасом.
const OWNER_WAIT: Duration = Duration::from_secs(5);
/// История читается с диска машины; срок с запасом к листингу каталогов.
const HISTORY_WAIT: Duration = Duration::from_secs(4);
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
    /// Последний отчёт сторожа машины; нет — воркер без `watchdog_v1` или отчёта ещё не было.
    pub watch: Option<crate::machine_watch::WatchView>,
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
    /// Открыт эпизод `mem_low`.
    MemoryLow,
    /// Свежий отчёт сторожа называет процессы, пережившие свои исполнения.
    ProcessesLeaked,
}
#[derive(Serialize, ToSchema)]
pub struct Workdirs {
    pub workdirs: Vec<String>,
}
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SuggestParams {
    pub path: String,
}
#[derive(Serialize, ToSchema)]
pub struct ClaudeHistoryView {
    pub id: String,
    pub modified_at_ms: u64,
    pub size: u64,
    pub preview: String,
}
#[derive(Serialize, ToSchema)]
pub struct ClaudeHistoryList {
    pub histories: Vec<ClaudeHistoryView>,
}
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub struct HistoryParams {
    pub workdir: String,
}
#[utoipa::path(get,path="/api/v1/machines/{id}/claude-histories",tag="machines",params(("id"=Uuid,Path),HistoryParams),responses((status=200,description="Беседы Claude Code рабочего каталога, свежие первыми; пусто, если машина не ответила",body=ClaudeHistoryList)))]
async fn claude_histories(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(p): ApiQuery<HistoryParams>,
) -> Result<Json<ClaudeHistoryList>, ApiError> {
    let mut conn = s.pool.acquire().await?;
    require_owned(&mut conn, o.owner_id, id).await?;
    drop(conn);
    if p.workdir.len() > conversations::MAX_WORKDIR_BYTES {
        return Ok(Json(ClaudeHistoryList {
            histories: Vec::new(),
        }));
    }
    let histories = s
        .registry
        .claude_histories(id, &p.workdir, HISTORY_WAIT)
        .await
        .into_iter()
        .map(|item| ClaudeHistoryView {
            id: item.id,
            modified_at_ms: item.modified_at_ms,
            size: item.size,
            preview: item.preview,
        })
        .collect();
    Ok(Json(ClaudeHistoryList { histories }))
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

    let sessions=sqlx::query("select c.id,c.title,coalesce(d.activity_state,'idle') activity_state from conversations c left join conversation_derived d on d.conversation_id=c.id where c.machine_id=$1 and c.owner_id=$2 and c.trashed_at is null order by c.id").bind(id).bind(owner).fetch_all(&mut *conn).await?.into_iter().map(|p|->Result<SessionView,sqlx::Error>{Ok(SessionView{conversation_id:p.try_get("id")?,title:p.try_get("title")?,activity_state:p.try_get("activity_state")?})}).collect::<Result<_,_>>()?;
    let metrics=sqlx::query("select to_jsonb(mm)-'machine_id' value, observed_at from machine_metrics mm where machine_id=$1").bind(id).fetch_optional(&mut *conn).await?;
    let mut warnings = Vec::new();
    let metrics = if let Some(m) = metrics {
        let mut v: Value = m.try_get("value")?;
        let at: DateTime<Utc> = m.try_get("observed_at")?;
        v["observed_at"] = json!(conversations::time(at));
        let disk_low = v["disks"].as_array().is_some_and(|ds| {
            ds.iter().any(|d| {
                d["total_bytes"].as_u64().unwrap_or(0) > 0
                    && d["free_bytes"].as_u64().unwrap_or(u64::MAX) <= config.disk_warn_bytes()
            })
        });
        if disk_low {
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
    if crate::machine_watch::memory_low(&mut *conn, id).await? {
        warnings.push(MachineWarning::MemoryLow);
    }
    let watch = crate::machine_watch::view(&mut *conn, owner, id).await?;
    let watch_fresh = watch.as_ref().is_some_and(|w| {
        DateTime::parse_from_rfc3339(&w.observed_at).is_ok_and(|at| {
            (Utc::now() - at.with_timezone(&Utc)).num_seconds()
                <= crate::machine_watch::WATCH_FRESH_SECS
        })
    });
    if watch_fresh && watch.as_ref().is_some_and(|w| !w.leaked.is_empty()) {
        warnings.push(MachineWarning::ProcessesLeaked);
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
        watch,
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

/// Вид файла инструкций владельца: только из списка, путь строит воркер.
fn valid_owner_kind(kind: &str) -> bool {
    matches!(
        kind,
        "claude_user"
            | "claude_home"
            | "codex_global"
            | "codex_override"
            | "opencode_global"
            | "memory_index"
            | "memory_file"
    )
}

fn valid_owner_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-' || b == b'=')
}

fn valid_owner_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.ends_with(".md")
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

fn owner_mtime(mtime_ms: u64) -> Option<String> {
    if mtime_ms == 0 {
        return None;
    }
    chrono::DateTime::from_timestamp_millis(i64::try_from(mtime_ms).ok()?)
        .map(|time| conversations::time(time.with_timezone(&chrono::Utc)))
}

#[derive(Serialize, ToSchema)]
pub struct OwnerFileView {
    pub kind: String,
    pub harness: String,
    pub path: String,
    pub present: bool,
    pub size: u64,
    pub mtime: Option<String>,
    /// Почему файл не показан при `present = false`: пусто или `symlink`.
    pub reason: String,
}

#[derive(Serialize, ToSchema)]
pub struct OwnerMemoryProjectView {
    pub id: String,
    pub files: u64,
    pub has_index: bool,
}

#[derive(Serialize, ToSchema)]
pub struct OwnerOverviewView {
    pub files: Vec<OwnerFileView>,
    pub memory_projects: Vec<OwnerMemoryProjectView>,
}

#[derive(Serialize, ToSchema)]
pub struct OwnerMemoryFileView {
    pub name: String,
    pub size: u64,
    pub mtime: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct OwnerFileContent {
    pub kind: String,
    pub harness: String,
    pub path: String,
    pub present: bool,
    pub size: u64,
    pub mtime: Option<String>,
    pub truncated: bool,
    pub content: String,
    pub files: Vec<OwnerMemoryFileView>,
    /// Почему файл не показан при `present = false`: пусто или `symlink`.
    pub reason: String,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OwnerFileParams {
    pub kind: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub name: String,
}

fn owner_harness(kind: &str) -> &'static str {
    if kind.starts_with("claude_") || kind.starts_with("memory_") {
        "claude"
    } else if kind.starts_with("codex_") {
        "codex"
    } else {
        "opencode"
    }
}

fn map_owner_error(error: crate::session::OwnerReadError) -> ApiError {
    match error {
        crate::session::OwnerReadError::CapabilityMissing => {
            ApiError::new(ErrorCode::CapabilityMissing)
        }
        crate::session::OwnerReadError::Unavailable => ApiError::new(ErrorCode::MachineUnavailable),
    }
}

#[utoipa::path(get,path="/api/v1/machines/{id}/owner-instructions",tag="machines",params(("id"=Uuid,Path)),responses((status=200,description="Метаданные инструкций владельца и проекты памяти",body=OwnerOverviewView),(status=409,description="Воркер без возможности или машина недоступна")))]
async fn owner_overview(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
) -> Result<Json<OwnerOverviewView>, ApiError> {
    let mut conn = s.pool.acquire().await?;
    require_owned(&mut conn, o.owner_id, id).await?;
    drop(conn);
    let reply = s
        .registry
        .owner_overview(id, OWNER_WAIT)
        .await
        .map_err(map_owner_error)?;
    // Ответ воркера доверен частично: виды только из списка, остальное — данные.
    let mut files = Vec::new();
    for file in reply.files {
        if !valid_owner_kind(&file.kind) {
            continue;
        }
        files.push(OwnerFileView {
            harness: owner_harness(&file.kind).into(),
            kind: file.kind,
            path: file.path,
            present: file.present,
            size: file.size,
            mtime: owner_mtime(file.mtime_ms),
            reason: owner_reason(&file.reason),
        });
    }
    let mut memory_projects = Vec::new();
    for project in reply.projects.into_iter().take(200) {
        if !valid_owner_id(&project.id) {
            continue;
        }
        memory_projects.push(OwnerMemoryProjectView {
            id: project.id,
            files: project.files,
            has_index: project.has_index,
        });
    }
    Ok(Json(OwnerOverviewView {
        files,
        memory_projects,
    }))
}

#[utoipa::path(get,path="/api/v1/machines/{id}/owner-instructions/file",tag="machines",params(("id"=Uuid,Path),OwnerFileParams),responses((status=200,description="Содержимое одного файла инструкций владельца",body=OwnerFileContent),(status=409,description="Воркер без возможности или машина недоступна")))]
async fn owner_file(
    State(s): State<AppState>,
    o: Owner,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(p): ApiQuery<OwnerFileParams>,
) -> Result<Json<OwnerFileContent>, ApiError> {
    let mut conn = s.pool.acquire().await?;
    require_owned(&mut conn, o.owner_id, id).await?;
    drop(conn);
    if !valid_owner_kind(&p.kind) {
        return Err(ApiError::new(ErrorCode::InvalidRequest));
    }
    let needs_project = p.kind == "memory_index" || p.kind == "memory_file";
    if needs_project {
        if !valid_owner_id(&p.project) {
            return Err(ApiError::new(ErrorCode::InvalidRequest));
        }
        if p.kind == "memory_file" && !valid_owner_name(&p.name) {
            return Err(ApiError::new(ErrorCode::InvalidRequest));
        }
        if p.kind == "memory_index" && !p.name.is_empty() {
            return Err(ApiError::new(ErrorCode::InvalidRequest));
        }
    } else if !p.project.is_empty() || !p.name.is_empty() {
        return Err(ApiError::new(ErrorCode::InvalidRequest));
    }
    // Содержимое не кэшируется в базе: только ответ живого воркера.
    let reply = s
        .registry
        .owner_file(id, &p.kind, &p.project, &p.name, OWNER_WAIT)
        .await
        .map_err(map_owner_error)?;
    if reply.kind != p.kind {
        return Err(ApiError::new(ErrorCode::InvalidRequest));
    }
    let mut files = Vec::new();
    for file in reply.files.into_iter().take(50) {
        if !valid_owner_name(&file.name) {
            continue;
        }
        files.push(OwnerMemoryFileView {
            name: file.name,
            size: file.size,
            mtime: owner_mtime(file.mtime_ms),
        });
    }
    Ok(Json(OwnerFileContent {
        harness: owner_harness(&reply.kind).into(),
        kind: reply.kind,
        path: reply.path,
        present: reply.present,
        size: reply.size,
        mtime: owner_mtime(reply.mtime_ms),
        truncated: reply.truncated,
        content: reply.content,
        files,
        reason: owner_reason(&reply.reason),
    }))
}

/// Причина отказа из ответа воркера: только из фиксированного списка,
/// остальное отбрасывается, чтобы наружу не ушёл произвольный текст.
fn owner_reason(reason: &str) -> String {
    if reason == "symlink" {
        "symlink".into()
    } else {
        String::new()
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
