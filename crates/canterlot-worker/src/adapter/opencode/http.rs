//! HTTP к серверу на loopback: Basic-вход, сроки и разбор ошибок. Ответы сервера могут
//! нести текст разговора и ключи провайдеров, поэтому ни тело, ни адрес запроса не
//! попадают ни в ошибку, ни в журнал.

use reqwest::{Client, Method};
use serde_json::Value;
use std::{fmt, time::Duration};

const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;
const MAX_TAG_CHARS: usize = 64;
/// Сервер закрывает простаивающее соединение через 5 с: своё закрываем раньше,
/// иначе запрос на закрытом сокете выглядел бы потерей связи.
const POOL_IDLE: Duration = Duration::from_secs(4);
const CONNECT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpError {
    /// Срок вышел или связь оборвалась: исход запроса неизвестен.
    Lost,
    /// Сервер ответил отказом; `tag` — поле `_tag` без текста сообщения.
    Status {
        status: u16,
        tag: String,
    },
    TooLarge,
    Decode,
}

#[derive(Clone)]
pub struct Http {
    client: Client,
    base: String,
    password: String,
}

impl fmt::Debug for Http {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Http").finish_non_exhaustive()
    }
}

// Имя ошибки из ответа: только ASCII-идентификатор ограниченной длины.
fn error_tag(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.get("_tag")?.as_str().map(str::to_owned))
        .filter(|tag| {
            !tag.is_empty()
                && tag.len() <= MAX_TAG_CHARS
                && tag.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .unwrap_or_else(|| "UnknownError".to_owned())
}

async fn bounded_body(response: &mut reqwest::Response, cap: usize) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| HttpError::Lost)? {
        if body.len() + chunk.len() > cap {
            return Err(HttpError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl Http {
    /// Сборка клиента читает системные сертификаты: не на цикле супервизора.
    pub async fn new(base: String, password: String) -> Result<Self, HttpError> {
        let client = tokio::task::spawn_blocking(|| {
            Client::builder()
                .no_proxy()
                .connect_timeout(CONNECT)
                .pool_idle_timeout(POOL_IDLE)
                .build()
        })
        .await
        .map_err(|_| HttpError::Lost)?
        .map_err(|_| HttpError::Lost)?;
        Ok(Self {
            client,
            base,
            password,
        })
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base))
            .basic_auth("opencode", Some(&self.password))
    }

    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
        limit: Duration,
        cap: usize,
    ) -> Result<Vec<u8>, HttpError> {
        let exchange = async {
            let mut response = builder.send().await.map_err(|_| HttpError::Lost)?;
            let status = response.status();
            if status.is_success() {
                return bounded_body(&mut response, cap).await;
            }
            let body = bounded_body(&mut response, MAX_ERROR_BYTES)
                .await
                .unwrap_or_default();
            Err(HttpError::Status {
                status: status.as_u16(),
                tag: error_tag(&body),
            })
        };
        tokio::time::timeout(limit, exchange)
            .await
            .map_err(|_| HttpError::Lost)?
    }

    async fn json(
        &self,
        builder: reqwest::RequestBuilder,
        limit: Duration,
    ) -> Result<Value, HttpError> {
        let body = self.send(builder, limit, MAX_BODY_BYTES).await?;
        if body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&body).map_err(|_| HttpError::Decode)
    }

    pub async fn get(&self, path: &str, limit: Duration) -> Result<Value, HttpError> {
        self.json(self.request(Method::GET, path), limit).await
    }

    pub async fn post(
        &self,
        path: &str,
        body: &Value,
        limit: Duration,
    ) -> Result<Value, HttpError> {
        self.json(self.request(Method::POST, path).json(body), limit)
            .await
    }

    /// `POST` без тела: прерывание схема описывает без него.
    pub async fn post_empty(&self, path: &str, limit: Duration) -> Result<Value, HttpError> {
        self.json(self.request(Method::POST, path), limit).await
    }

    pub async fn patch(
        &self,
        path: &str,
        body: &Value,
        limit: Duration,
    ) -> Result<Value, HttpError> {
        self.json(self.request(Method::PATCH, path).json(body), limit)
            .await
    }

    pub async fn delete(
        &self,
        path: &str,
        query: &[(&str, &str)],
        limit: Duration,
    ) -> Result<Value, HttpError> {
        self.json(self.request(Method::DELETE, path).query(query), limit)
            .await
    }

    /// Сырые байты ответа, для сверки схемы.
    pub async fn bytes(&self, path: &str, limit: Duration) -> Result<Vec<u8>, HttpError> {
        self.send(self.request(Method::GET, path), limit, MAX_BODY_BYTES)
            .await
    }

    /// Открыть поток событий: срок только на заголовки, сам поток бессрочен.
    pub async fn events(&self, limit: Duration) -> Result<reqwest::Response, HttpError> {
        let response = tokio::time::timeout(limit, self.request(Method::GET, "/api/event").send())
            .await
            .map_err(|_| HttpError::Lost)?
            .map_err(|_| HttpError::Lost)?;
        if !response.status().is_success() {
            return Err(HttpError::Status {
                status: response.status().as_u16(),
                tag: "UnknownError".to_owned(),
            });
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_is_an_identifier_and_never_the_message() {
        assert_eq!(
            error_tag(br#"{"_tag":"ConflictError","message":"secret text"}"#),
            "ConflictError"
        );
        assert_eq!(error_tag(r#"{"_tag":"Текст"}"#.as_bytes()), "UnknownError");
        assert_eq!(error_tag(br#"{"message":"no tag"}"#), "UnknownError");
        assert_eq!(error_tag(b"not json"), "UnknownError");
        let long = format!(r#"{{"_tag":"{}"}}"#, "A".repeat(65));
        assert_eq!(error_tag(long.as_bytes()), "UnknownError");
    }

    #[tokio::test]
    async fn debug_hides_the_password() {
        let http = Http::new("http://127.0.0.1:1".into(), "сырой-пароль".into())
            .await
            .unwrap();
        assert!(!format!("{http:?}").contains("пароль"));
    }
}
