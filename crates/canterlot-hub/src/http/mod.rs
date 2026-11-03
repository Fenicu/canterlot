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
    config::{ActivityConfig, AttachmentsConfig, HttpConfig, LimitsConfig, MachinesConfig},
    dispatcher::Dispatcher,
    installation::Installation,
    session::Registry,
};

pub mod attachments;
pub mod auth;
pub mod alarms;
mod connection;
pub mod conversations;
pub mod crunch;
pub mod error;
pub mod extract;
pub mod groups;
pub mod health;
pub mod history;
pub mod layers;
pub mod limits;
pub mod machines;
pub mod media;
pub mod openapi;
pub mod presets;
pub mod scheduled;
pub mod session_tools;
pub mod snapshot;
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
    pub machines: MachinesConfig,
    pub attachments: AttachmentsConfig,
    pub blobs: Arc<dyn BlobStore>,
    pub manifest: PathBuf,
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
    pub machines: Arc<MachinesConfig>,
    pub attachments: Arc<AttachmentsConfig>,
    pub blobs: Arc<dyn BlobStore>,
    /// Тела загрузок вложений одновременно в обработке: не больше `max_concurrent_uploads`.
    pub upload_slots: Arc<tokio::sync::Semaphore>,
    pub manifest: Arc<PathBuf>,
    pub user_header: HeaderName,
    pub ws_tasks: tokio_util::task::TaskTracker,
    #[cfg(feature = "fault-injection")]
    pub ws_faults: crate::fault_injection::Faults,
    pub shutdown: CancellationToken,
}

impl AppState {
    pub fn new(deps: AppDeps) -> Self {
        let user_header = HeaderName::from_bytes(deps.http.user_header.as_bytes())
            .expect("user_header is checked by HttpConfig::validate");
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
            machines: Arc::new(deps.machines),
            upload_slots: Arc::new(tokio::sync::Semaphore::new(
                deps.attachments.max_concurrent_uploads.max(1),
            )),
            attachments: Arc::new(deps.attachments),
            blobs: deps.blobs,
            manifest: Arc::new(deps.manifest),
            user_header,
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
        .merge(scheduled::routes())
        .merge(alarms::routes())
        .merge(conversations::routes())
        .merge(crunch::routes())
        .merge(groups::routes())
        .merge(presets::routes())
        .merge(session_tools::routes())
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
