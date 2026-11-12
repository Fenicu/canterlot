//! Общая обвязка тестов входа владельца (`auth_*`): хаб с секретом прокси, чтобы
//! адрес клиента из `X-Canterlot-Client-Ip` учитывался, и клиенты с нужными cookie.

use axum::Router;
use canterlot_hub::{
    config::HttpConfig,
    http::{self, AppState},
    owner_auth::{self, Method as SignInMethod, password},
    testkit::{
        Method, TEST_ORIGIN, TEST_OWNER_LOGIN, TEST_SESSION_TOKEN, TestClient, TestResponse,
        test_app_deps, test_http_config,
    },
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

pub const SECRET: &str = "auth-test-proxy-secret-0123456789-abcdefghij";
pub const PASSWORD: &str = "correct horse battery";

pub struct AuthHub {
    pub pool: PgPool,
    pub state: AppState,
    pub router: Router,
    pub addr: Option<std::net::SocketAddr>,
    _dir: tempfile::TempDir,
}

impl AuthHub {
    pub async fn new(pool: PgPool) -> Self {
        Self::with(pool, |_| {}, true).await
    }

    /// `secret`: настроен ли секрет прокси (без него адрес клиента неизвестен).
    pub async fn with(pool: PgPool, edit: impl FnOnce(&mut HttpConfig), secret: bool) -> Self {
        Self::build(pool, edit, secret, None).await
    }

    pub async fn build(
        pool: PgPool,
        edit: impl FnOnce(&mut HttpConfig),
        secret: bool,
        passwords: Option<password::Passwords>,
    ) -> Self {
        let mut config = test_http_config();
        edit(&mut config);
        let (mut deps, dir) = test_app_deps(
            pool.clone(),
            config,
            secret.then(|| SECRET.as_bytes().to_vec()),
        )
        .await;
        if let Some(passwords) = passwords {
            deps.passwords = passwords;
        }
        let state = AppState::new(deps);
        let router = http::router(state.clone());
        Self {
            pool,
            state,
            router,
            addr: None,
            _dir: dir,
        }
    }

    /// Тот же хаб на настоящем сокете (для WebSocket).
    pub async fn serve(mut self) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.addr = Some(listener.local_addr().unwrap());
        let state = self.state.clone();
        let stop = state.shutdown.clone();
        tokio::spawn(http::serve(state, listener, stop.cancelled_owned()));
        self
    }

    pub fn owner_id(&self) -> Uuid {
        self.state.installation.owner_id
    }

    /// Клиент за прокси без сеанса: секрет, Origin, CSRF-заголовок и адрес клиента.
    pub fn anonymous(&self, ip: Option<&str>) -> TestClient {
        let mut client = TestClient::in_process(self.router.clone())
            .with_header("Origin", TEST_ORIGIN)
            .with_header("X-Canterlot-Client", "1")
            .with_header("X-Canterlot-Proxy", SECRET);
        if let Some(ip) = ip {
            client = client.with_header("X-Canterlot-Client-Ip", ip);
        }
        client
    }

    pub fn with_session(&self, token: &str, ip: Option<&str>) -> TestClient {
        self.anonymous(ip).with_session(token)
    }

    /// Владелец с заранее заведённым сеансом.
    pub fn owner(&self) -> TestClient {
        self.with_session(TEST_SESSION_TOKEN, Some("198.51.100.1"))
    }

    pub async fn set_password(&self, value: &str) {
        let phc = self.state.passwords.hash(value.to_owned()).await.unwrap();
        let mut conn = self.pool.acquire().await.unwrap();
        password::store(&mut conn, self.owner_id(), &phc)
            .await
            .unwrap();
    }

    pub async fn sign_in(&self, client: &TestClient, login: &str, secret: &str) -> TestResponse {
        client
            .send(
                Method::POST,
                "/api/v1/auth/password",
                None,
                Some(json!({"login": login, "password": secret})),
            )
            .await
    }

    pub async fn sign_in_owner(&self, client: &TestClient) -> TestResponse {
        self.sign_in(client, TEST_OWNER_LOGIN, PASSWORD).await
    }

    /// Сеанс, созданный напрямую: способ, кем подтверждён, адрес.
    pub async fn session(&self, method: SignInMethod, approved_by: Option<Uuid>) -> (Uuid, String) {
        let created = owner_auth::sessions::create(
            &self.pool,
            self.owner_id(),
            method,
            approved_by,
            &Default::default(),
        )
        .await
        .unwrap();
        (created.id, created.token)
    }

    pub async fn revoke_reason(&self, id: Uuid) -> Option<String> {
        sqlx::query_scalar("select revoke_reason from owner_sessions where id=$1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// Сеанс несвеж: доказательство пароля было давно.
    pub async fn make_stale(&self, id: Uuid) {
        sqlx::query(
            "update owner_sessions set authenticated_at=now()-interval '11 minutes' where id=$1",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
    }
}

/// Значение cookie из `Set-Cookie` ответа и её атрибуты.
pub fn set_cookie(response: &TestResponse, name: &str) -> Option<(String, String)> {
    response
        .headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, attributes) = value.split_once(';').unwrap_or((value, ""));
            let (key, cookie) = pair.split_once('=')?;
            (key == name).then(|| (cookie.to_owned(), attributes.trim().to_owned()))
        })
}

pub fn session_token(response: &TestResponse) -> String {
    set_cookie(response, owner_auth::SESSION_COOKIE)
        .expect("session cookie")
        .0
}

pub fn error_code(response: &TestResponse) -> String {
    let body: Value = response.json();
    body["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Поддельный Gotify: запоминает тела сообщений. Сервер живёт до конца теста.
pub async fn fake_gotify() -> (std::sync::Arc<std::sync::Mutex<Vec<Value>>>, String) {
    use axum::{extract::State, routing::post};
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/gotify/message",
            post(
                |State(seen): State<std::sync::Arc<std::sync::Mutex<Vec<Value>>>>,
                 body: axum::body::Bytes| async move {
                    seen.lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                    axum::http::StatusCode::OK
                },
            ),
        )
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (seen, format!("http://{addr}/gotify"))
}
