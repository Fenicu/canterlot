//! HTTP-клиент Yandex Cloud Compute на том `reqwest`, что уже есть в хабе. Формы запросов
//! и ответов сверены с живым API 23–24.11.2026.
//!
//! Повторы только там, где они безопасны:
//! - один раз после 401 со свежим токеном (запрос не обработан);
//! - на 429 с ожиданием по `Retry-After`, везде, кроме создания машины. У создания 429 —
//!   отказ по квоте, и сам запрос после неоднозначного ответа не повторяется никогда.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Method;
use serde_json::{Value, json};
use tokio::time::Instant;

use super::auth::{Auth, ServiceKey, scrub};
use super::price::{self, Price, PriceError, UnitPrices};
use super::{
    CreateInstance, CreateOutcome, DeleteOutcome, Disk, Instance, InstanceLookup, InstanceStatus,
    InstanceSummary, ListError, OperationState, STANDARD_IMAGES_FOLDER, YandexConfig,
    YandexInitError,
};
use crate::cloud::rate_limit::RateLimiter;
use crate::cloud::{ErrorKind, ProviderError};

/// Размер страницы списка: наибольший, что принимает API.
const PAGE_SIZE: &str = "1000";
/// Больше страниц в каталоге не бывает: дальше обход считается сломанным.
const MAX_LIST_PAGES: u32 = 50;
/// Каталог SKU общий на весь Яндекс и заметно длиннее списка машин.
const MAX_SKU_PAGES: u32 = 200;
/// Сколько живёт прочитанная таблица цен.
const SKU_TTL: Duration = Duration::from_secs(10 * 60);

/// Коды gRPC, по которым клиент принимает решения.
const GRPC_NOT_FOUND: i32 = 5;
const GRPC_ALREADY_EXISTS: i32 = 6;
const GRPC_RESOURCE_EXHAUSTED: i32 = 8;
const GRPC_FAILED_PRECONDITION: i32 = 9;
const GRPC_UNAUTHENTICATED: i32 = 16;

/// Что случилось с запросом до ответа.
#[derive(Debug)]
enum CallError {
    Timeout,
    Connect,
    Other,
    /// 429 не прошёл за все повторы или `Retry-After` слишком велик.
    RateLimited,
    /// IAM-токен не получен: запрос к API не уходил.
    Token(ProviderError),
}

impl CallError {
    fn describe(&self) -> String {
        match self {
            Self::Timeout => "request timed out".to_owned(),
            Self::Connect => "could not connect".to_owned(),
            Self::Other => "connection failed".to_owned(),
            Self::RateLimited => "rate limited".to_owned(),
            Self::Token(error) => error.message.clone(),
        }
    }

    fn into_error(self) -> ProviderError {
        match self {
            Self::Token(error) => error,
            Self::RateLimited => ProviderError::new(ErrorKind::RateLimited, "rate limited"),
            other => ProviderError::new(ErrorKind::Unavailable, other.describe()),
        }
    }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
    /// Токен, с которым ушёл запрос: чтобы вычистить его из текста ошибки.
    token: String,
}

impl Reply {
    fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }

    /// Код gRPC из тела ошибки.
    fn grpc_code(&self) -> Option<i32> {
        self.json()
            .and_then(|value| value["code"].as_i64())
            .and_then(|code| i32::try_from(code).ok())
    }

    fn message(&self) -> String {
        let text = self
            .json()
            .and_then(|value| value["message"].as_str().map(str::to_owned))
            .unwrap_or_default();
        scrub(&text, &[&self.token])
    }

    fn error(&self) -> ProviderError {
        let kind = match self.status {
            401 | 403 => ErrorKind::Auth,
            404 => ErrorKind::NotFound,
            409 => ErrorKind::Conflict,
            429 => ErrorKind::RateLimited,
            500.. => ErrorKind::Unavailable,
            _ => ErrorKind::Rejected,
        };
        let code = self
            .grpc_code()
            .map_or_else(String::new, |code| format!(" code {code}"));
        ProviderError::new(
            kind,
            format!("HTTP {}{code}: {}", self.status, self.message()),
        )
    }
}

fn malformed(what: &str) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, what.to_owned())
}

/// Идентификатор идёт в путь запроса: только то, что похоже на ID. Так значение из базы или
/// из ответа провайдера не может изменить путь.
fn safe_id(id: &str) -> Result<&str, ProviderError> {
    let safe = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    if safe {
        Ok(id)
    } else {
        Err(ProviderError::new(ErrorKind::Rejected, "invalid id"))
    }
}

/// Пауза перед повтором после 429, номер повтора с нуля. `Retry-After` берётся как есть;
/// без него пауза `default × 2^attempt`, не больше `max`. `None`: `Retry-After` больше
/// границы, ждать столько не будем.
fn rate_limit_wait(
    retry_after_seconds: Option<u64>,
    attempt: u32,
    default: Duration,
    max: Duration,
) -> Option<Duration> {
    match retry_after_seconds {
        Some(seconds) => Some(Duration::from_secs(seconds)).filter(|wait| *wait <= max),
        None => Some(
            default
                .saturating_mul(2u32.saturating_pow(attempt))
                .min(max),
        ),
    }
}

fn classify(error: &reqwest::Error) -> CallError {
    if error.is_timeout() {
        CallError::Timeout
    } else if error.is_connect() {
        CallError::Connect
    } else {
        CallError::Other
    }
}

/// Отказ создания говорит про квоту или нехватку ресурсов зоны.
fn is_capacity_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("quota") || lower.contains("not enough resources")
}

fn text(value: &Value, field: &str) -> String {
    value[field].as_str().unwrap_or_default().to_owned()
}

fn required_id(value: &Value, what: &str) -> Result<String, ProviderError> {
    value["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| malformed(&format!("{what} has no id")))
}

fn parse_summary(value: &Value) -> Result<InstanceSummary, ProviderError> {
    Ok(InstanceSummary {
        id: required_id(value, "instance")?,
        name: text(value, "name"),
        status: InstanceStatus::from_api(value["status"].as_str().unwrap_or_default()),
    })
}

fn parse_instance(value: &Value) -> Result<Instance, ProviderError> {
    let summary = parse_summary(value)?;
    let public_ip = value["networkInterfaces"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|interface| {
            interface["primaryV4Address"]["oneToOneNat"]["address"]
                .as_str()
                .filter(|address| !address.is_empty())
        })
        .map(str::to_owned);
    Ok(Instance {
        id: summary.id,
        name: summary.name,
        folder_id: value["folderId"]
            .as_str()
            .filter(|folder| !folder.is_empty())
            .ok_or_else(|| malformed("instance has no folderId"))?
            .to_owned(),
        status: summary.status,
        public_ip,
        boot_disk_id: value["bootDisk"]["diskId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned),
        preemptible: value["schedulingPolicy"]["preemptible"]
            .as_bool()
            .unwrap_or(false),
    })
}

fn parse_disk(value: &Value) -> Result<Disk, ProviderError> {
    let instance_ids = match &value["instanceIds"] {
        Value::Null => Vec::new(),
        Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| malformed("disk instanceIds are not strings"))?,
        // По непонятному полю нельзя решить, что диск ничей.
        _ => return Err(malformed("disk instanceIds is not a list")),
    };
    Ok(Disk {
        id: required_id(value, "disk")?,
        name: text(value, "name"),
        instance_ids,
        status: text(value, "status"),
    })
}

type SkuTable = HashMap<String, Vec<Value>>;

pub struct YandexClient {
    http: reqwest::Client,
    config: YandexConfig,
    auth: Auth,
    limiter: RateLimiter,
    /// Нужные SKU каталога и время, когда каталог прочитан.
    skus: tokio::sync::Mutex<Option<(Instant, Arc<SkuTable>)>>,
}

impl fmt::Debug for YandexClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("YandexClient")
            .field("config", &self.config)
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl YandexClient {
    /// Проверяет настройки и читает ключ. В сеть не ходит: токен берётся при первом вызове.
    pub fn new(mut config: YandexConfig) -> Result<Self, YandexInitError> {
        config.validate().map_err(YandexInitError::Config)?;
        for url in [
            &mut config.iam_url,
            &mut config.compute_url,
            &mut config.operation_url,
            &mut config.billing_url,
        ] {
            *url = url.trim().trim_end_matches('/').to_owned();
        }
        let key = ServiceKey::load(&config.key_file)?;
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| YandexInitError::Client)?;
        Ok(Self {
            auth: Auth::new(key, &config.iam_url),
            limiter: RateLimiter::new(config.rate_limit),
            skus: tokio::sync::Mutex::new(None),
            http,
            config,
        })
    }

    pub fn config(&self) -> &YandexConfig {
        &self.config
    }

    /// Один вызов API: предел частоты, токен, таймаут, один повтор после 401 и, если
    /// `retry_rate_limit`, повторы после 429.
    async fn call(
        &self,
        method: Method,
        endpoint: &'static str,
        url: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
        retry_rate_limit: bool,
    ) -> Result<Reply, CallError> {
        let mut attempt = 0u32;
        let mut refreshed = false;
        loop {
            self.limiter.acquire(endpoint).await;
            let token = self
                .auth
                .token(&self.http)
                .await
                .map_err(CallError::Token)?;
            let mut request = self.http.request(method.clone(), url).bearer_auth(&token);
            if !query.is_empty() {
                request = request.query(query);
            }
            if let Some(body) = body {
                request = request.json(body);
            }
            tracing::debug!(endpoint, attempt, "yandex request");
            let response = request.send().await.map_err(|error| classify(&error))?;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok());
            let bytes = response.bytes().await.map_err(|error| classify(&error))?;
            tracing::debug!(endpoint, status, "yandex response");
            if status == 401 && !refreshed {
                // Токен отозван или истёк раньше срока: запрос не обработан, берём новый.
                tracing::debug!(endpoint, "yandex token refused, refreshing once");
                self.auth.forget(&token).await;
                refreshed = true;
                continue;
            }
            if status != 429 || !retry_rate_limit {
                return Ok(Reply {
                    status,
                    body: bytes.to_vec(),
                    token,
                });
            }
            if attempt >= self.config.max_rate_limit_retries {
                tracing::warn!(endpoint, "yandex rate limit did not clear, giving up");
                return Err(CallError::RateLimited);
            }
            let Some(wait) = rate_limit_wait(
                retry_after,
                attempt,
                self.config.default_backoff,
                self.config.max_backoff,
            ) else {
                tracing::warn!(
                    endpoint,
                    "yandex Retry-After is beyond the bound, giving up"
                );
                return Err(CallError::RateLimited);
            };
            tracing::debug!(
                endpoint,
                wait_ms = wait.as_millis() as u64,
                "yandex rate limited, waiting"
            );
            tokio::time::sleep(wait).await;
            attempt += 1;
        }
    }

    /// Создание машины. Один вызов, без повторов: исход разобран в `CreateOutcome`.
    pub async fn create(&self, folder_id: &str, request: &CreateInstance) -> CreateOutcome {
        let config = &self.config;
        // int64 в REST Яндекса — строки JSON.
        let body = json!({
            "folderId": folder_id,
            "name": request.name,
            "zoneId": config.zone,
            "platformId": config.platform,
            "labels": request.labels,
            "resourcesSpec": {
                "memory": request.memory_bytes.to_string(),
                "cores": request.cores.to_string(),
                "coreFraction": "100",
            },
            "metadata": {"user-data": request.user_data},
            "bootDiskSpec": {
                "autoDelete": true,
                "diskSpec": {
                    "name": request.name,
                    "typeId": config.disk_type,
                    "size": request.disk_bytes.to_string(),
                    "imageId": request.image_id,
                },
            },
            "networkInterfaceSpecs": [{
                "subnetId": config.subnet_id,
                "primaryV4AddressSpec": {"oneToOneNatSpec": {"ipVersion": "IPV4"}},
            }],
            "schedulingPolicy": {"preemptible": request.preemptible},
        });
        let url = format!("{}/compute/v1/instances", config.compute_url);
        let reply = match self
            .call(
                Method::POST,
                "POST /compute/v1/instances",
                &url,
                &[],
                Some(&body),
                false,
            )
            .await
        {
            Ok(reply) => reply,
            // Токен не получен: запрос создания не уходил, машины нет.
            Err(CallError::Token(error)) => {
                return CreateOutcome::Rejected {
                    http: 0,
                    grpc_code: Some(GRPC_UNAUTHENTICATED),
                    message: error.message,
                    next_profile: false,
                };
            }
            Err(failure) => {
                return CreateOutcome::Ambiguous {
                    why: failure.describe(),
                };
            }
        };
        let grpc_code = reply.grpc_code();
        match reply.status {
            200 => {
                let value = reply.json().unwrap_or(Value::Null);
                let operation_id = value["id"].as_str().filter(|id| !id.is_empty());
                let instance_id = value["metadata"]["instanceId"]
                    .as_str()
                    .filter(|id| !id.is_empty());
                match (instance_id, operation_id) {
                    (Some(instance_id), Some(operation_id)) => CreateOutcome::Accepted {
                        instance_id: instance_id.to_owned(),
                        operation_id: operation_id.to_owned(),
                    },
                    _ => CreateOutcome::Ambiguous {
                        why: "success answer without an operation or an instance id".to_owned(),
                    },
                }
            }
            // Имя занято: машина с ним есть, возможно наша от прошлой попытки. Без кода в
            // теле 409 тоже ничего не доказывает.
            409 if matches!(grpc_code, Some(GRPC_ALREADY_EXISTS) | None) => {
                CreateOutcome::Ambiguous {
                    why: match grpc_code {
                        Some(_) => "the name already exists".to_owned(),
                        None => "HTTP 409 with an unreadable body".to_owned(),
                    },
                }
            }
            status @ 400..=499 => {
                let message = reply.message();
                let next_profile = match (status, grpc_code) {
                    (429, Some(GRPC_RESOURCE_EXHAUSTED)) => true,
                    (400, Some(GRPC_FAILED_PRECONDITION)) => is_capacity_message(&message),
                    _ => false,
                };
                CreateOutcome::Rejected {
                    http: status,
                    grpc_code,
                    message,
                    next_profile,
                }
            }
            status => CreateOutcome::Ambiguous {
                why: format!("HTTP {status}"),
            },
        }
    }

    /// Состояние операции создания или удаления.
    pub async fn operation(&self, id: &str) -> Result<OperationState, ProviderError> {
        let url = format!("{}/operations/{}", self.config.operation_url, safe_id(id)?);
        let reply = self
            .call(Method::GET, "GET /operations/{id}", &url, &[], None, true)
            .await
            .map_err(CallError::into_error)?;
        match reply.status {
            200 => {}
            404 => return Ok(OperationState::Unknown),
            _ => return Err(reply.error()),
        }
        let value = reply.json().ok_or_else(|| malformed("not JSON"))?;
        if value["id"].as_str().is_none() {
            return Err(malformed("operation has no id"));
        }
        // `done: false` в ответе может быть опущено, как любое значение по умолчанию.
        let done = match &value["done"] {
            Value::Bool(done) => *done,
            Value::Null => false,
            _ => return Err(malformed("operation `done` is not a boolean")),
        };
        if !done {
            // Ошибка может появиться раньше `done`: откат ещё идёт, исход не окончателен.
            return Ok(OperationState::Pending);
        }
        match value.get("error").filter(|error| !error.is_null()) {
            None => Ok(OperationState::Done),
            Some(error) => Ok(OperationState::Failed {
                grpc_code: error["code"]
                    .as_i64()
                    .and_then(|code| i32::try_from(code).ok())
                    .ok_or_else(|| malformed("operation error has no code"))?,
                message: scrub(
                    error["message"].as_str().unwrap_or_default(),
                    &[&reply.token],
                ),
            }),
        }
    }

    pub async fn get(&self, instance_id: &str) -> Result<InstanceLookup, ProviderError> {
        let url = format!(
            "{}/compute/v1/instances/{}",
            self.config.compute_url,
            safe_id(instance_id)?
        );
        let reply = self
            .call(
                Method::GET,
                "GET /compute/v1/instances/{id}",
                &url,
                &[],
                None,
                true,
            )
            .await
            .map_err(CallError::into_error)?;
        match reply.status {
            200 => {
                let value = reply.json().ok_or_else(|| malformed("not JSON"))?;
                Ok(InstanceLookup::Found(parse_instance(&value)?))
            }
            404 if reply.grpc_code() == Some(GRPC_NOT_FOUND) => Ok(InstanceLookup::NotFound),
            _ => Err(reply.error()),
        }
    }

    /// Удаление машины. Повтор, пока удаление идёт, возвращает ту же операцию.
    pub async fn delete(&self, instance_id: &str) -> DeleteOutcome {
        self.delete_resource(
            "DELETE /compute/v1/instances/{id}",
            "instances",
            instance_id,
        )
        .await
    }

    /// Удаление диска. Подключённый диск API удалить не даст.
    pub async fn delete_disk(&self, disk_id: &str) -> DeleteOutcome {
        self.delete_resource("DELETE /compute/v1/disks/{id}", "disks", disk_id)
            .await
    }

    async fn delete_resource(
        &self,
        endpoint: &'static str,
        collection: &str,
        id: &str,
    ) -> DeleteOutcome {
        let Ok(id) = safe_id(id) else {
            return DeleteOutcome::Failed {
                retryable: false,
                http: None,
                grpc_code: None,
                message: "invalid id".to_owned(),
            };
        };
        let url = format!("{}/compute/v1/{collection}/{id}", self.config.compute_url);
        let reply = match self
            .call(Method::DELETE, endpoint, &url, &[], None, true)
            .await
        {
            Ok(reply) => reply,
            Err(failure) => {
                let error = failure.into_error();
                return DeleteOutcome::Failed {
                    retryable: error.retryable(),
                    http: None,
                    grpc_code: None,
                    message: error.message,
                };
            }
        };
        let failed = |retryable: bool, message: String| DeleteOutcome::Failed {
            retryable,
            http: Some(reply.status),
            grpc_code: reply.grpc_code(),
            message,
        };
        match reply.status {
            200 => {
                let operation_id = reply
                    .json()
                    .and_then(|value| value["id"].as_str().map(str::to_owned))
                    .filter(|id| !id.is_empty());
                match operation_id {
                    Some(operation_id) => DeleteOutcome::Accepted { operation_id },
                    // Удаление, скорее всего, принято: повтор вернёт ту же операцию.
                    None => failed(true, "success answer without an operation id".to_owned()),
                }
            }
            404 => DeleteOutcome::AlreadyGone,
            409 | 429 | 500.. => failed(true, reply.message()),
            _ => failed(false, reply.message()),
        }
    }

    /// Все страницы списка: элементы по ключу `key`. Пустой список API отдаёт как `{}`.
    async fn walk(
        &self,
        endpoint: &'static str,
        url: &str,
        fixed: &[(&str, &str)],
        key: &str,
        max_pages: u32,
    ) -> Result<Vec<Value>, ListError> {
        let mut items: Vec<Value> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut token = String::new();
        for page in 0..max_pages {
            let page_error = |error: ProviderError| ListError::Page { page, error };
            let mut query: Vec<(&str, &str)> = fixed.to_vec();
            query.push(("pageSize", PAGE_SIZE));
            if !token.is_empty() {
                query.push(("pageToken", &token));
            }
            let reply = self
                .call(Method::GET, endpoint, url, &query, None, true)
                .await
                .map_err(|failure| page_error(failure.into_error()))?;
            if reply.status != 200 {
                return Err(page_error(reply.error()));
            }
            let mut body = reply
                .json()
                .filter(Value::is_object)
                .ok_or_else(|| page_error(malformed("page is not a JSON object")))?;
            match body[key].take() {
                Value::Null => {}
                Value::Array(mut page_items) => items.append(&mut page_items),
                _ => return Err(page_error(malformed(&format!("`{key}` is not a list")))),
            }
            let next = match &body["nextPageToken"] {
                Value::Null => "",
                Value::String(next) => next.as_str(),
                _ => return Err(page_error(malformed("nextPageToken is not a string"))),
            };
            if next.is_empty() {
                return Ok(items);
            }
            if !seen.insert(next.to_owned()) {
                return Err(ListError::RepeatedToken { page });
            }
            token = next.to_owned();
        }
        Err(ListError::TooManyPages { limit: max_pages })
    }

    /// Все машины каталога, все страницы. Любая неудавшаяся страница делает ошибкой весь
    /// вызов.
    pub async fn list_instances(&self, folder_id: &str) -> Result<Vec<InstanceSummary>, ListError> {
        let url = format!("{}/compute/v1/instances", self.config.compute_url);
        let items = self
            .walk(
                "GET /compute/v1/instances",
                &url,
                &[("folderId", folder_id)],
                "instances",
                MAX_LIST_PAGES,
            )
            .await?;
        items
            .iter()
            .map(parse_summary)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ListError::Page { page: 0, error })
    }

    /// Все диски каталога, все страницы, с тем же правилом.
    pub async fn list_disks(&self, folder_id: &str) -> Result<Vec<Disk>, ListError> {
        let url = format!("{}/compute/v1/disks", self.config.compute_url);
        let items = self
            .walk(
                "GET /compute/v1/disks",
                &url,
                &[("folderId", folder_id)],
                "disks",
                MAX_LIST_PAGES,
            )
            .await?;
        items
            .iter()
            .map(parse_disk)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ListError::Page { page: 0, error })
    }

    /// Номер свежего образа семейства из общих образов Яндекса.
    pub async fn latest_image(&self, family: &str) -> Result<String, ProviderError> {
        let url = format!(
            "{}/compute/v1/images:latestByFamily",
            self.config.compute_url
        );
        let reply = self
            .call(
                Method::GET,
                "GET /compute/v1/images:latestByFamily",
                &url,
                &[("folderId", STANDARD_IMAGES_FOLDER), ("family", family)],
                None,
                true,
            )
            .await
            .map_err(CallError::into_error)?;
        if reply.status != 200 {
            return Err(reply.error());
        }
        let value = reply.json().ok_or_else(|| malformed("not JSON"))?;
        required_id(&value, "image")
    }

    /// Нужные SKU каталога: из кэша не старше десяти минут или заново, все страницы.
    /// Неудачное чтение не кэшируется.
    async fn sku_table(&self) -> Result<Arc<SkuTable>, ListError> {
        let mut cached = self.skus.lock().await;
        if let Some((read_at, table)) = cached.as_ref()
            && read_at.elapsed() < SKU_TTL
        {
            return Ok(table.clone());
        }
        let url = format!("{}/billing/v1/skus", self.config.billing_url);
        let items = self
            .walk(
                "GET /billing/v1/skus",
                &url,
                &[("currency", "RUB")],
                "skus",
                MAX_SKU_PAGES,
            )
            .await?;
        let mut table = SkuTable::new();
        for item in items {
            // Сопоставление по номеру: в именах каталога стоят неразрывные пробелы.
            if let Some(id) = item["id"].as_str()
                && price::SKU_IDS.contains(&id)
            {
                table.entry(id.to_owned()).or_default().push(item);
            }
        }
        let table = Arc::new(table);
        *cached = Some((Instant::now(), table.clone()));
        Ok(table)
    }

    /// Ставки часа машины с публичным адресом и сетевым SSD по ценам каталога, действующим
    /// сейчас: `cores×vCPU + memory_gb×RAM + IP` и `disk_gb×SSD`, каждая вверх до целой
    /// копейки. Любая неясность в каталоге — ошибка, а не догадка.
    pub async fn price(
        &self,
        cores: u32,
        memory_gb: u64,
        disk_gb: u64,
        preemptible: bool,
    ) -> Result<Price, PriceError> {
        let table = self.sku_table().await.map_err(PriceError::Catalog)?;
        let now = chrono::Utc::now();
        let unit = |id: &str, unit: &str| -> Result<i128, PriceError> {
            let malformed = |why| PriceError::MalformedSku {
                id: id.to_owned(),
                why,
            };
            match table.get(id).map(Vec::as_slice) {
                None | Some([]) => Err(PriceError::MissingSku(id.to_owned())),
                Some([sku]) => price::sku_unit_price(sku, unit, now).map_err(malformed),
                Some(_) => Err(malformed("listed more than once")),
            }
        };
        let (vcpu, ram) = if preemptible {
            (price::SKU_PREEMPTIBLE_VCPU, price::SKU_PREEMPTIBLE_RAM)
        } else {
            (price::SKU_REGULAR_VCPU, price::SKU_REGULAR_RAM)
        };
        let prices = UnitPrices {
            vcpu: unit(vcpu, price::UNIT_CORE_HOUR)?,
            ram_gb: unit(ram, price::UNIT_GB_HOUR)?,
            ssd_gb: unit(price::SKU_SSD, price::UNIT_GB_HOUR)?,
            public_ip: unit(price::SKU_PUBLIC_IP, price::UNIT_IP_HOUR)?,
        };
        price::hourly(&prices, cores, memory_gb, disk_gb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_is_honoured_up_to_the_bound() {
        let default = Duration::from_millis(100);
        let max = Duration::from_secs(60);
        assert_eq!(
            rate_limit_wait(Some(7), 3, default, max),
            Some(Duration::from_secs(7))
        );
        assert_eq!(rate_limit_wait(Some(61), 0, default, max), None);
        assert_eq!(
            rate_limit_wait(None, 2, default, max),
            Some(Duration::from_millis(400))
        );
        assert_eq!(rate_limit_wait(None, u32::MAX, default, max), Some(max));
    }

    #[test]
    fn ids_that_could_change_the_path_are_refused() {
        assert!(safe_id("fhmfakeinstance0001").is_ok());
        for bad in ["", "a/b", "..", "a?b=c", "a b", "a%2Fb", &"x".repeat(65)] {
            assert!(safe_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn capacity_refusals_are_recognised_by_the_message() {
        assert!(is_capacity_message(
            "Quota limit compute.instanceCores.count exceeded"
        ));
        assert!(is_capacity_message(
            "Not enough resources in zone ru-central1-a"
        ));
        assert!(!is_capacity_message("Illegal argument subnetId"));
    }

    #[test]
    fn a_disk_with_an_unreadable_attachment_list_is_refused() {
        let disk = json!({"id": "fhmfakedisk00000001", "name": "d", "status": "READY"});
        assert_eq!(
            parse_disk(&disk).unwrap().instance_ids,
            Vec::<String>::new()
        );
        let odd = json!({"id": "fhmfakedisk00000001", "instanceIds": "none"});
        assert!(parse_disk(&odd).is_err());
        let mixed = json!({"id": "fhmfakedisk00000001", "instanceIds": ["a", 1]});
        assert!(parse_disk(&mixed).is_err());
    }
}
