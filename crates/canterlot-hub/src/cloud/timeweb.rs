//! Клиент Timeweb Cloud (`https://api.timeweb.cloud`) на том HTTP-клиенте, что уже есть в
//! хабе. Имена полей и формы ответов взяты из публичного OpenAPI.
//!
//! Токен читается один раз из файла при создании клиента. Он живёт только в заголовке
//! `Authorization` (помеченном секретным), не попадает ни в `Debug`, ни в логи, ни в тексты
//! ошибок: всё, что пришло от сервера, перед выдачей очищается от значения токена.
//!
//! Повторы только там, где они безопасны: на 429 (запрос не обработан) с ожиданием по
//! `Retry-After`. Создание после неоднозначного ответа не повторяется никогда.

use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Method;
use serde_json::{Value, json};

use super::price::{UnitPrices, rubles_to_kopecks};
use super::rate_limit::{RateLimit, RateLimiter};
use super::{
    CloudProvider, Configurator, CreateOutcome, CreateServer, DeleteOutcome, ErrorKind, Limits,
    ListError, ProviderError, ServerLookup, ServerState, ServerStatus, ServerSummary,
};

pub const DEFAULT_BASE_URL: &str = "https://api.timeweb.cloud";

/// Размер страницы списка: умолчание API.
const PAGE_LIMIT: u64 = 100;
/// Страховка от бесконечного обхода.
const MAX_PAGES: u64 = 2_000;
/// Длиннее этого текст ошибки сервера обрезается.
const MAX_MESSAGE_CHARS: usize = 300;

#[derive(Debug, Clone)]
pub struct TimewebConfig {
    /// Адрес API без завершающего `/`; в тестах указывает на поддельный сервер.
    pub base_url: String,
    /// Файл с токеном: читается один раз при создании клиента.
    pub token_file: PathBuf,
    /// Предел на один запрос целиком.
    pub request_timeout: Duration,
    /// Предел частоты на каждую конечную точку.
    pub rate_limit: RateLimit,
    /// Сколько раз повторять запрос после 429.
    pub max_rate_limit_retries: u32,
    /// Пауза после первого 429 без `Retry-After`; дальше удваивается.
    pub default_backoff: Duration,
    /// Верхняя граница паузы. `Retry-After` больше неё не ждётся: попытка сдаётся.
    pub max_backoff: Duration,
}

impl TimewebConfig {
    pub fn new(token_file: impl Into<PathBuf>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            token_file: token_file.into(),
            request_timeout: Duration::from_secs(30),
            rate_limit: RateLimit::default(),
            max_rate_limit_retries: 4,
            default_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimewebInitError {
    /// Файл токена не читается (причина — вид ошибки ввода-вывода, без содержимого).
    TokenFile(String),
    EmptyToken,
    Client,
}

impl fmt::Display for TimewebInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TokenFile(why) => write!(f, "cannot read the token file: {why}"),
            Self::EmptyToken => f.write_str("the token file is empty"),
            Self::Client => f.write_str("cannot build the HTTP client"),
        }
    }
}

impl std::error::Error for TimewebInitError {}

/// Токен. `Debug` не показывает значение.
struct Token(String);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

/// Что случилось с запросом до ответа (или после 429 без успеха).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Timeout,
    Connect,
    Other,
    /// 429 не прошёл за все повторы или `Retry-After` слишком велик.
    RateLimited,
}

impl Transport {
    fn describe(self) -> &'static str {
        match self {
            Self::Timeout => "request timed out",
            Self::Connect => "could not connect",
            Self::Other => "connection failed",
            Self::RateLimited => "rate limited",
        }
    }

    fn into_error(self) -> ProviderError {
        let kind = if self == Self::RateLimited {
            ErrorKind::RateLimited
        } else {
            ErrorKind::Unavailable
        };
        ProviderError::new(kind, self.describe())
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

pub struct TimewebProvider {
    client: reqwest::Client,
    base_url: String,
    token: Token,
    limiter: RateLimiter,
    max_rate_limit_retries: u32,
    default_backoff: Duration,
    max_backoff: Duration,
}

impl fmt::Debug for TimewebProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimewebProvider")
            .field("base_url", &self.base_url)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl TimewebProvider {
    pub fn new(config: TimewebConfig) -> Result<Self, TimewebInitError> {
        let raw = std::fs::read_to_string(&config.token_file)
            .map_err(|error| TimewebInitError::TokenFile(format!("{:?}", error.kind())))?;
        let token = raw.trim().to_owned();
        if token.is_empty() {
            return Err(TimewebInitError::EmptyToken);
        }
        let client = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| TimewebInitError::Client)?;
        Ok(Self {
            client,
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            token: Token(token),
            limiter: RateLimiter::new(config.rate_limit),
            max_rate_limit_retries: config.max_rate_limit_retries,
            default_backoff: config.default_backoff,
            max_backoff: config.max_backoff,
        })
    }

    /// Один вызов API: предел частоты, таймаут, повторы после 429.
    async fn call(
        &self,
        method: Method,
        endpoint: &'static str,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Reply, Transport> {
        let url = format!("{}{path}", self.base_url);
        let mut attempt = 0u32;
        loop {
            self.limiter.acquire(endpoint).await;
            let mut request = self
                .client
                .request(method.clone(), &url)
                .bearer_auth(&self.token.0);
            if !query.is_empty() {
                request = request.query(query);
            }
            if let Some(body) = body {
                request = request.json(body);
            }
            tracing::debug!(endpoint, attempt, "timeweb request");
            let response = request.send().await.map_err(classify)?;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok());
            let bytes = response.bytes().await.map_err(classify)?;
            tracing::debug!(endpoint, status, "timeweb response");
            if status != 429 {
                return Ok(Reply {
                    status,
                    body: bytes.to_vec(),
                });
            }
            if attempt >= self.max_rate_limit_retries {
                tracing::warn!(endpoint, "timeweb rate limit did not clear, giving up");
                return Err(Transport::RateLimited);
            }
            let wait = match retry_after {
                Some(seconds) => Duration::from_secs(seconds),
                None => self
                    .default_backoff
                    .saturating_mul(2u32.saturating_pow(attempt))
                    .min(self.max_backoff),
            };
            if wait > self.max_backoff {
                tracing::warn!(
                    endpoint,
                    "timeweb Retry-After is beyond the bound, giving up"
                );
                return Err(Transport::RateLimited);
            }
            tracing::debug!(
                endpoint,
                wait_ms = wait.as_millis() as u64,
                "timeweb rate limited, waiting"
            );
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }

    /// Текст ошибки из тела ответа: `message` строкой или списком строк, без токена.
    fn message_of(&self, body: &[u8]) -> String {
        let text = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|value| match &value["message"] {
                Value::String(text) => Some(text.clone()),
                Value::Array(items) => Some(
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
                _ => None,
            })
            .unwrap_or_default();
        let text = text.replace(self.token.0.as_str(), "[redacted]");
        text.chars().take(MAX_MESSAGE_CHARS).collect()
    }

    fn status_error(&self, reply: &Reply) -> ProviderError {
        let kind = match reply.status {
            401 | 403 => ErrorKind::Auth,
            404 => ErrorKind::NotFound,
            409 => ErrorKind::Conflict,
            500.. => ErrorKind::Unavailable,
            _ => ErrorKind::Rejected,
        };
        ProviderError::new(
            kind,
            format!("HTTP {}: {}", reply.status, self.message_of(&reply.body)),
        )
    }

    /// Все страницы списка: элементы по ключу `key`, общее число в `meta.total`. Успехом
    /// считается только обход, давший ровно столько уникальных элементов, сколько заявлено.
    async fn walk_pages(
        &self,
        endpoint: &'static str,
        path: &str,
        key: &str,
    ) -> Result<Vec<Value>, ListError> {
        let mut items: Vec<Value> = Vec::new();
        let mut seen: HashSet<i64> = HashSet::new();
        let mut offset = 0u64;
        for _ in 0..MAX_PAGES {
            let page_error = |error: ProviderError| ListError::Page { offset, error };
            let query = [
                ("limit", PAGE_LIMIT.to_string()),
                ("offset", offset.to_string()),
            ];
            let reply = self
                .call(Method::GET, endpoint, path, &query, None)
                .await
                .map_err(|failure| page_error(failure.into_error()))?;
            if reply.status != 200 {
                return Err(page_error(self.status_error(&reply)));
            }
            let body: Value = serde_json::from_slice(&reply.body).map_err(|_| {
                page_error(ProviderError::new(ErrorKind::Malformed, "page is not JSON"))
            })?;
            let page = body[key].as_array().ok_or_else(|| {
                page_error(ProviderError::new(
                    ErrorKind::Malformed,
                    format!("page has no `{key}` array"),
                ))
            })?;
            let total = body["meta"]["total"].as_u64().ok_or_else(|| {
                page_error(ProviderError::new(
                    ErrorKind::Malformed,
                    "page has no meta.total",
                ))
            })?;

            let mut fresh = 0u64;
            for item in page {
                let id = item["id"].as_i64().ok_or_else(|| {
                    page_error(ProviderError::new(ErrorKind::Malformed, "item has no id"))
                })?;
                if seen.insert(id) {
                    items.push(item.clone());
                    fresh += 1;
                }
            }
            let got = seen.len() as u64;
            if got >= total {
                // Больше заявленного значит, что список менялся во время обхода: страницы
                // могли разъехаться, и целым такой список не считается.
                return if got == total {
                    Ok(items)
                } else {
                    Err(ListError::Incomplete {
                        expected: total,
                        got,
                    })
                };
            }
            if fresh == 0 {
                return Err(ListError::Incomplete {
                    expected: total,
                    got,
                });
            }
            offset += page.len() as u64;
        }
        Err(ListError::Incomplete {
            expected: 0,
            got: seen.len() as u64,
        })
    }
}

fn classify(error: reqwest::Error) -> Transport {
    if error.is_timeout() {
        Transport::Timeout
    } else if error.is_connect() {
        Transport::Connect
    } else {
        Transport::Other
    }
}

/// Целое из JSON-числа, которое API может записать и как `1024`, и как `1024.0`.
fn number_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        let float = value.as_f64()?;
        (float >= 0.0 && float.fract() == 0.0 && float < 1.8e19).then_some(float as u64)
    })
}

fn malformed(what: &str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, what.to_owned())
}

fn parse_configurator(value: &Value) -> Result<Configurator, ProviderError> {
    let price = |name: &str| -> Result<i64, ProviderError> {
        let rubles = value["prices"][name]
            .as_f64()
            .ok_or_else(|| malformed("configurator has no per-resource prices"))?;
        rubles_to_kopecks(rubles).map_err(|_| malformed("configurator price is out of range"))
    };
    let requirement = |name: &str| -> Result<u64, ProviderError> {
        number_u64(&value["requirements"][name])
            .ok_or_else(|| malformed("configurator has no requirements"))
    };
    let small = |name: &str| -> Result<u32, ProviderError> {
        u32::try_from(requirement(name)?)
            .map_err(|_| malformed("configurator limit is out of range"))
    };
    Ok(Configurator {
        id: value["id"]
            .as_i64()
            .ok_or_else(|| malformed("configurator has no id"))?,
        location: value["location"].as_str().unwrap_or_default().to_owned(),
        cpu_frequency: value["cpu_frequency"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        prices: UnitPrices {
            cpu: price("cpu")?,
            ram_gb: price("ram")?,
            disk_gb: price("disk")?,
        },
        limits: Limits {
            cpu_min: small("cpu_min")?,
            cpu_max: small("cpu_max")?,
            cpu_step: small("cpu_step")?,
            ram_min_mb: requirement("ram_min")?,
            ram_max_mb: requirement("ram_max")?,
            ram_step_mb: requirement("ram_step")?,
            disk_min_mb: requirement("disk_min")?,
            disk_max_mb: requirement("disk_max")?,
            disk_step_mb: requirement("disk_step")?,
        },
    })
}

/// Основной публичный IPv4 сервера, если он уже выдан.
fn public_ipv4(server: &Value) -> Option<String> {
    let mut first = None;
    for network in server["networks"].as_array()? {
        if network["type"].as_str() != Some("public") {
            continue;
        }
        for ip in network["ips"].as_array().into_iter().flatten() {
            if ip["type"].as_str() != Some("ipv4") {
                continue;
            }
            let Some(address) = ip["ip"].as_str() else {
                continue;
            };
            if ip["is_main"].as_bool() == Some(true) {
                return Some(address.to_owned());
            }
            first.get_or_insert_with(|| address.to_owned());
        }
    }
    first
}

fn parse_summary(value: &Value) -> Result<ServerSummary, ProviderError> {
    Ok(ServerSummary {
        id: value["id"]
            .as_i64()
            .ok_or_else(|| malformed("server has no id"))?,
        name: value["name"].as_str().unwrap_or_default().to_owned(),
        status: ServerStatus::from_api(value["status"].as_str().unwrap_or_default()),
    })
}

fn parse_state(value: &Value) -> Result<ServerState, ProviderError> {
    let summary = parse_summary(value)?;
    Ok(ServerState {
        id: summary.id,
        name: summary.name,
        status: summary.status,
        ipv4: public_ipv4(value),
        created_at: value["created_at"].as_str().map(str::to_owned),
    })
}

/// Ключ без комментария: тип и тело. Так «тот же ключ» узнаётся независимо от подписи.
fn key_identity(key: &str) -> (&str, &str) {
    let mut words = key.split_whitespace();
    (
        words.next().unwrap_or_default(),
        words.next().unwrap_or_default(),
    )
}

#[async_trait]
impl CloudProvider for TimewebProvider {
    async fn configurator(&self, id: i64) -> Result<Configurator, ProviderError> {
        let reply = self
            .call(
                Method::GET,
                "GET /api/v1/configurator/servers",
                "/api/v1/configurator/servers",
                &[],
                None,
            )
            .await
            .map_err(Transport::into_error)?;
        if reply.status != 200 {
            return Err(self.status_error(&reply));
        }
        let body: Value = serde_json::from_slice(&reply.body).map_err(|_| malformed("not JSON"))?;
        let found = body["server_configurators"]
            .as_array()
            .ok_or_else(|| malformed("no `server_configurators` array"))?
            .iter()
            .find(|item| item["id"].as_i64() == Some(id))
            .ok_or_else(|| {
                ProviderError::new(
                    ErrorKind::NotFound,
                    format!("configurator {id} does not exist"),
                )
            })?;
        parse_configurator(found)
    }

    async fn ensure_ssh_key(&self, name: &str, public_key: &str) -> Result<i64, ProviderError> {
        let existing = self
            .walk_pages("GET /api/v1/ssh-keys", "/api/v1/ssh-keys", "ssh-keys")
            .await
            .map_err(|error| match error {
                ListError::Page { error, .. } => error,
                ListError::Incomplete { .. } => {
                    ProviderError::new(ErrorKind::Unavailable, "key listing was incomplete")
                }
            })?;
        if let Some(found) = existing
            .iter()
            .find(|key| key["name"].as_str() == Some(name))
        {
            let same = key_identity(found["body"].as_str().unwrap_or_default())
                == key_identity(public_key);
            return if same {
                found["id"]
                    .as_i64()
                    .ok_or_else(|| malformed("key has no id"))
            } else {
                Err(ProviderError::new(
                    ErrorKind::Conflict,
                    format!("ssh key name `{name}` is taken by a different key"),
                ))
            };
        }
        let body = json!({"name": name, "body": public_key.trim(), "is_default": false});
        let reply = self
            .call(
                Method::POST,
                "POST /api/v1/ssh-keys",
                "/api/v1/ssh-keys",
                &[],
                Some(&body),
            )
            .await
            .map_err(Transport::into_error)?;
        if !matches!(reply.status, 200 | 201) {
            return Err(self.status_error(&reply));
        }
        let created: Value =
            serde_json::from_slice(&reply.body).map_err(|_| malformed("not JSON"))?;
        created["ssh-key"]["id"]
            .as_i64()
            .ok_or_else(|| malformed("created key has no id"))
    }

    async fn create_server(&self, request: &CreateServer) -> CreateOutcome {
        let mut body = json!({
            "name": request.name,
            "comment": request.comment,
            "project_id": request.project_id,
            "configuration": {
                "configurator_id": request.configurator_id,
                "cpu": request.cpu,
                "ram": request.ram_mb,
                "disk": request.disk_mb,
            },
            "os_id": request.os_id,
            "ssh_keys_ids": request.ssh_key_ids,
            "cloud_init": request.cloud_init,
        });
        if !request.hostname.is_empty() {
            body["hostname"] = json!(request.hostname);
        }
        let reply = match self
            .call(
                Method::POST,
                "POST /api/v1/servers",
                "/api/v1/servers",
                &[],
                Some(&body),
            )
            .await
        {
            Ok(reply) => reply,
            Err(failure) => {
                return CreateOutcome::Ambiguous {
                    why: failure.describe().to_owned(),
                };
            }
        };
        match reply.status {
            200 | 201 => {
                let id = serde_json::from_slice::<Value>(&reply.body)
                    .ok()
                    .and_then(|value| value["server"]["id"].as_i64());
                match id {
                    Some(server_id) => CreateOutcome::Created { server_id },
                    None => CreateOutcome::Ambiguous {
                        why: "success answer without a server id".to_owned(),
                    },
                }
            }
            status @ 400..=499 => CreateOutcome::Rejected {
                code: status,
                message: self.message_of(&reply.body),
            },
            status => CreateOutcome::Ambiguous {
                why: format!("HTTP {status}"),
            },
        }
    }

    async fn get_server(&self, id: i64) -> Result<ServerLookup, ProviderError> {
        let path = format!("/api/v1/servers/{id}");
        let reply = self
            .call(Method::GET, "GET /api/v1/servers/{id}", &path, &[], None)
            .await
            .map_err(Transport::into_error)?;
        match reply.status {
            200 => {
                let body: Value =
                    serde_json::from_slice(&reply.body).map_err(|_| malformed("not JSON"))?;
                Ok(ServerLookup::Found(parse_state(&body["server"])?))
            }
            404 => Ok(ServerLookup::NotFound),
            _ => Err(self.status_error(&reply)),
        }
    }

    async fn delete_server(&self, id: i64) -> DeleteOutcome {
        let path = format!("/api/v1/servers/{id}");
        let reply = match self
            .call(
                Method::DELETE,
                "DELETE /api/v1/servers/{id}",
                &path,
                &[],
                None,
            )
            .await
        {
            Ok(reply) => reply,
            Err(_) => return DeleteOutcome::Failed { retryable: true },
        };
        match reply.status {
            204 => DeleteOutcome::Deleted { quarantined: false },
            200 => {
                let body: Value = serde_json::from_slice(&reply.body).unwrap_or(Value::Null);
                let answer = &body["server_delete"];
                if answer["hash"].as_str().is_some_and(|hash| !hash.is_empty()) {
                    DeleteOutcome::NeedsConfirmation
                } else {
                    DeleteOutcome::Deleted {
                        quarantined: answer["is_moved_in_quarantine"].as_bool().unwrap_or(false),
                    }
                }
            }
            423 => DeleteOutcome::NeedsConfirmation,
            404 => DeleteOutcome::AlreadyGone,
            409 | 500.. => DeleteOutcome::Failed { retryable: true },
            _ => DeleteOutcome::Failed { retryable: false },
        }
    }

    async fn list_project_servers(&self, project_id: i64) -> Result<Vec<ServerSummary>, ListError> {
        let path = format!("/api/v1/projects/{project_id}/resources/servers");
        let items = self
            .walk_pages(
                "GET /api/v1/projects/{id}/resources/servers",
                &path,
                "servers",
            )
            .await?;
        items
            .iter()
            .map(parse_summary)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ListError::Page { offset: 0, error })
    }
}
