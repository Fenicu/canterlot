use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::HeaderName,
    middleware::{from_fn, from_fn_with_state},
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use utoipa_axum::router::OpenApiRouter;

use crate::{
    attachments::BlobStore,
    config::{
        ActivityConfig, AttachmentsConfig, AuthConfig, GuardsConfig, HttpConfig, LimitsConfig,
        MachinesConfig, SessionToolsConfig,
    },
    dispatcher::Dispatcher,
    installation::Installation,
    session::Registry,
};

pub mod alarms;
pub mod attachments;
pub mod auth;
pub mod bookmarks;
mod connection;
pub mod conversations;
pub mod crunch;
pub mod drafts;
pub mod error;
pub mod extract;
pub mod groups;
pub mod health;
pub mod history;
pub mod layers;
pub mod limit_settings;
pub mod limits;
pub mod machines;
pub mod media;
pub mod nudge;
pub mod openapi;
pub mod plan;
pub mod presets;
pub mod routing;
pub mod scheduled;
pub mod search;
pub mod session_tools;
pub mod signin;
pub mod signin_qr;
pub mod snapshot;
pub mod spawn_queue;
pub mod ws;

pub use crate::attachments::UPLOAD_CHUNK_BYTES;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
const MIN_PROXY_SECRET_BYTES: usize = 32;

pub struct AppDeps {
    pub pool: sqlx::PgPool,
    pub registry: Registry,
    pub installation: Installation,
    pub dispatcher: Dispatcher,
    pub scheduler: crate::scheduler::Scheduler,
    pub http: HttpConfig,
    pub proxy_secret: Option<Vec<u8>>,
    pub limits: LimitsConfig,
    pub activity: ActivityConfig,
    /// Выключатель `[orchestrator.nudge]`: обновление проекции при отметке прочитанного.
    pub nudge_enabled: bool,
    /// Свежесть показаний лимитов для предпросмотра маршрута.
    pub routing_stale: std::time::Duration,
    /// Пороги стража машины перед запуском.
    pub guards: GuardsConfig,
    /// Пределы межсессионных инструментов: пуск из очереди владельцем.
    pub session_tools: SessionToolsConfig,
    /// Очередь запусков: срок элемента и пауза повтора.
    pub spawn_queue: crate::config::SpawnQueueConfig,
    /// Горизонт ожидания лимита для маршрута.
    pub routing_queue_horizon: Duration,
    pub machines: MachinesConfig,
    pub attachments: AttachmentsConfig,
    pub blobs: Arc<dyn BlobStore>,
    pub manifest: PathBuf,
    /// `[auth]` после подстановки значений по умолчанию.
    pub auth: AuthConfig,
    /// Хеширование паролей: боевые параметры argon2, в тестах дешёвые.
    pub passwords: crate::owner_auth::password::Passwords,
    pub shutdown: CancellationToken,
}

#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::PgPool,
    pub registry: Registry,
    pub installation: Installation,
    pub dispatcher: Dispatcher,
    pub scheduler: crate::scheduler::Scheduler,
    pub http: Arc<HttpConfig>,
    pub proxy_secret: Option<Arc<Vec<u8>>>,
    pub limits: Arc<LimitsConfig>,
    pub activity: Arc<ActivityConfig>,
    pub nudge_enabled: bool,
    pub routing_stale: std::time::Duration,
    pub guards: GuardsConfig,
    pub session_tools: SessionToolsConfig,
    pub spawn_queue: crate::config::SpawnQueueConfig,
    pub routing_queue_horizon: Duration,
    pub machines: Arc<MachinesConfig>,
    pub attachments: Arc<AttachmentsConfig>,
    pub blobs: Arc<dyn BlobStore>,
    /// Тела загрузок вложений одновременно в обработке: не больше `max_concurrent_uploads`.
    pub upload_slots: Arc<tokio::sync::Semaphore>,
    pub manifest: Arc<PathBuf>,
    /// Заголовок старого прокси: его приход помечается `X-Canterlot-Proxy-Leak`.
    pub user_header: HeaderName,
    pub auth: Arc<AuthConfig>,
    pub passwords: crate::owner_auth::password::Passwords,
    /// Место по IP из `[auth.geoip].path`; файл перечитывается при смене.
    pub geo: Arc<crate::owner_auth::geo::Geo>,
    /// Одновременные долгие опросы входа по QR в этом процессе.
    pub qr_polls: Arc<tokio::sync::Semaphore>,
    pub ws_tasks: tokio_util::task::TaskTracker,
    #[cfg(feature = "fault-injection")]
    pub ws_faults: crate::fault_injection::Faults,
    pub shutdown: CancellationToken,
}

impl AppState {
    pub fn new(deps: AppDeps) -> Self {
        let user_header = deps.http.leak_header();
        Self {
            pool: deps.pool,
            registry: deps.registry,
            installation: deps.installation,
            dispatcher: deps.dispatcher,
            scheduler: deps.scheduler,
            http: Arc::new(deps.http),
            proxy_secret: deps.proxy_secret.map(Arc::new),
            limits: Arc::new(deps.limits),
            activity: Arc::new(deps.activity),
            nudge_enabled: deps.nudge_enabled,
            routing_stale: deps.routing_stale,
            guards: deps.guards,
            session_tools: deps.session_tools,
            spawn_queue: deps.spawn_queue,
            routing_queue_horizon: deps.routing_queue_horizon,
            machines: Arc::new(deps.machines),
            upload_slots: Arc::new(tokio::sync::Semaphore::new(
                deps.attachments.max_concurrent_uploads.max(1),
            )),
            attachments: Arc::new(deps.attachments),
            blobs: deps.blobs,
            manifest: Arc::new(deps.manifest),
            user_header,
            geo: Arc::new(crate::owner_auth::geo::Geo::open(&deps.auth.geoip.path)),
            qr_polls: Arc::new(tokio::sync::Semaphore::new(
                crate::owner_auth::qr::MAX_POLLS,
            )),
            auth: Arc::new(deps.auth),
            passwords: deps.passwords,
            ws_tasks: tokio_util::task::TaskTracker::new(),
            #[cfg(feature = "fault-injection")]
            ws_faults: Default::default(),
            shutdown: deps.shutdown,
        }
    }
}

/// Читает секрет прокси: пробелы и перевод строки по краям отбрасываются,
/// остаются видимые ASCII-символы длиной не менее 32 байт, чтобы заголовок мог его нести.
pub fn load_proxy_secret(path: &Path) -> Result<Vec<u8>, &'static str> {
    let bytes = std::fs::read(path).map_err(|_| "proxy_secret_file_error")?;
    let secret = bytes.trim_ascii();
    if secret.len() < MIN_PROXY_SECRET_BYTES || !secret.iter().all(|b| (0x21..=0x7e).contains(b)) {
        return Err("proxy_secret_invalid");
    }
    Ok(secret.to_vec())
}

/// Все маршруты, которые попадают и в роутер, и в документ OpenAPI; новые задачи сливают сюда свои.
pub fn api_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(health::routes())
        .merge(signin::routes())
        .merge(signin_qr::routes())
        .merge(scheduled::routes())
        .merge(alarms::routes())
        .merge(plan::routes())
        .merge(nudge::routes())
        .merge(routing::routes())
        .merge(search::routes())
        .merge(conversations::routes())
        .merge(drafts::routes())
        .merge(bookmarks::routes())
        .merge(crunch::routes())
        .merge(groups::routes())
        .merge(presets::routes())
        .merge(session_tools::routes())
        .merge(spawn_queue::routes())
        .merge(limit_settings::routes())
        .merge(machines::routes())
        .merge(limits::routes())
        .merge(snapshot::routes())
        .merge(history::routes())
        .merge(ws::routes())
        .merge(attachments::routes())
        .merge(media::routes())
}

pub fn router(state: AppState) -> Router {
    assemble(state, Router::new())
}

/// Точка расширения для тестов: пробные маршруты проходят через те же барьеры.
#[cfg(any(test, feature = "testkit"))]
pub fn router_with(state: AppState, extra: Router<AppState>) -> Router {
    assemble(state, extra)
}

fn assemble(state: AppState, extra: Router<AppState>) -> Router {
    Router::<AppState>::from(api_routes())
        .merge(extra)
        .method_not_allowed_fallback(layers::method_not_allowed)
        .fallback(layers::not_found)
        .layer(DefaultBodyLimit::max(state.http.max_json_body_bytes))
        .layer(from_fn_with_state(state.clone(), auth::identity_gate))
        .layer(from_fn_with_state(state.clone(), auth::csrf_gate))
        .layer(from_fn(auth::options_gate))
        .layer(from_fn(layers::panic_guard))
        .layer(from_fn(layers::response_headers))
        .layer(from_fn(layers::request_context))
        .with_state(state)
}

pub async fn serve(
    state: AppState,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let config = (*state.http).clone();
    let stopping = state.shutdown.clone();
    let drain = state.ws_tasks.clone();
    let tasks = state.ws_tasks.clone();
    let stop_after = stopping.clone();
    let result = serve_router(router(state), listener, config, async move {
        shutdown.await;
        stopping.cancel();
        drain.close();
        drain.wait().await;
    })
    .await;
    stop_after.cancel();
    tasks.close();
    tasks.wait().await;
    result
}

/// Обслуживает готовый роутер; адрес собеседника берётся из сокета (`ConnectInfo`).
/// После сигнала остановки открытые запросы получают `SHUTDOWN_GRACE`.
pub async fn serve_router(
    router: Router,
    listener: TcpListener,
    config: HttpConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    connection::serve(router, listener, config, shutdown).await
}
