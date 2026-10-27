use std::{
    future::Future,
    net::SocketAddr,
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
    config::{ActivityConfig, HttpConfig, LimitsConfig, MachinesConfig},
    dispatcher::Dispatcher,
    installation::Installation,
    session::Registry,
};

pub mod auth;
pub mod error;
pub mod extract;
pub mod health;
pub mod layers;
pub mod openapi;

/// Размер одного тела `PATCH` загрузки вложения; объявляется в `GET /api/v1/config`.
pub const UPLOAD_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
const MIN_PROXY_SECRET_BYTES: usize = 32;

pub struct AppDeps {
    pub pool: sqlx::PgPool,
    pub registry: Registry,
    pub installation: Installation,
    pub dispatcher: Dispatcher,
    pub http: HttpConfig,
    pub proxy_secret: Option<Vec<u8>>,
    pub limits: LimitsConfig,
    pub activity: ActivityConfig,
    pub machines: MachinesConfig,
    pub manifest: PathBuf,
    pub shutdown: CancellationToken,
}

#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::PgPool,
    pub registry: Registry,
    pub installation: Installation,
    pub dispatcher: Dispatcher,
    pub http: Arc<HttpConfig>,
    pub proxy_secret: Option<Arc<Vec<u8>>>,
    pub limits: Arc<LimitsConfig>,
    pub activity: Arc<ActivityConfig>,
    pub machines: Arc<MachinesConfig>,
    pub manifest: Arc<PathBuf>,
    pub user_header: HeaderName,
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
            http: Arc::new(deps.http),
            proxy_secret: deps.proxy_secret.map(Arc::new),
            limits: Arc::new(deps.limits),
            activity: Arc::new(deps.activity),
            machines: Arc::new(deps.machines),
            manifest: Arc::new(deps.manifest),
            user_header,
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
    OpenApiRouter::new().merge(health::routes())
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
    serve_router(router(state), listener, shutdown).await
}

/// Обслуживает готовый роутер; адрес собеседника берётся из сокета (`ConnectInfo`).
/// После сигнала остановки открытые запросы получают `SHUTDOWN_GRACE`.
pub async fn serve_router(
    router: Router,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let stop = CancellationToken::new();
    let signal = stop.clone();
    let serving = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { signal.cancelled().await });
    let mut serving = std::pin::pin!(std::future::IntoFuture::into_future(serving));
    tokio::select! {
        result = &mut serving => return result,
        () = shutdown => stop.cancel(),
    }
    match tokio::time::timeout(SHUTDOWN_GRACE, serving).await {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(
                kind = "http_shutdown_timeout",
                "open requests were abandoned"
            );
            Ok(())
        }
    }
}
