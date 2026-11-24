//! Вход в Yandex Cloud: авторизованный ключ сервисного аккаунта подписывает JWT (PS256),
//! JWT меняется на IAM-токен.
//!
//! Ключ, JWT и токен не попадают ни в `Debug`, ни в логи, ни в тексты ошибок.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PSS_SHA256, RsaKeyPair};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::Instant;

use super::YandexInitError;
use crate::cloud::{ErrorKind, ProviderError};

/// Получатель JWT: настоящий адрес обмена, каким бы ни был `iam_url`.
pub(super) const JWT_AUDIENCE: &str = "https://iam.api.cloud.yandex.net/iam/v1/tokens";
/// Срок жизни JWT, секунд.
const JWT_LIFETIME: u64 = 3600;
/// Токен из кэша годен не дольше этого. Сам токен живёт до 12 часов, обновлять велено
/// каждый час.
const TOKEN_TTL: Duration = Duration::from_secs(55 * 60);

const PEM_BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
const PEM_END: &str = "-----END PRIVATE KEY-----";

#[derive(Deserialize)]
struct KeyFile {
    id: String,
    service_account_id: String,
    private_key: String,
}

/// Ключ сервисного аккаунта. `Debug` показывает только номера.
pub(super) struct ServiceKey {
    key_id: String,
    service_account_id: String,
    pair: RsaKeyPair,
}

impl fmt::Debug for ServiceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceKey")
            .field("key_id", &self.key_id)
            .field("service_account_id", &self.service_account_id)
            .field("private_key", &"<redacted>")
            .finish()
    }
}

impl ServiceKey {
    pub(super) fn load(path: &Path) -> Result<Self, YandexInitError> {
        let raw = std::fs::read(path)
            .map_err(|error| YandexInitError::KeyFile(format!("{:?}", error.kind())))?;
        Self::parse(&raw)
    }

    fn parse(raw: &[u8]) -> Result<Self, YandexInitError> {
        // Причина ошибки разбора не пересказывается: в ней может оказаться кусок файла.
        let file: KeyFile = serde_json::from_slice(raw).map_err(|_| {
            YandexInitError::KeyFormat("not JSON with id, service_account_id and private_key")
        })?;
        if file.id.is_empty() || file.service_account_id.is_empty() {
            return Err(YandexInitError::KeyFormat("empty id or service_account_id"));
        }
        let der = pkcs8_der(&file.private_key)?;
        let pair = RsaKeyPair::from_pkcs8(&der)
            .map_err(|_| YandexInitError::KeyFormat("private_key is not an RSA PKCS#8 key"))?;
        Ok(Self {
            key_id: file.id,
            service_account_id: file.service_account_id,
            pair,
        })
    }

    /// JWT для обмена на IAM-токен: PS256, `kid` — номер ключа.
    fn jwt(&self, issued_at: u64) -> Option<String> {
        let header = json!({"typ": "JWT", "alg": "PS256", "kid": self.key_id});
        let claims = json!({
            "iss": self.service_account_id,
            "aud": JWT_AUDIENCE,
            "iat": issued_at,
            "exp": issued_at + JWT_LIFETIME,
        });
        let mut token = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut signature = vec![0u8; self.pair.public().modulus_len()];
        self.pair
            .sign(
                &RSA_PSS_SHA256,
                &SystemRandom::new(),
                token.as_bytes(),
                &mut signature,
            )
            .ok()?;
        token.push('.');
        token.push_str(&URL_SAFE_NO_PAD.encode(&signature));
        Some(token)
    }
}

/// Тело PEM-блока закрытого ключа. Перед блоком в ключах Яндекса стоит строка
/// `PLEASE DO NOT REMOVE THIS LINE! …`: всё до начала блока отбрасывается.
fn pkcs8_der(private_key: &str) -> Result<Vec<u8>, YandexInitError> {
    let missing = YandexInitError::KeyFormat("private_key has no PEM private key block");
    let start = private_key.find(PEM_BEGIN).ok_or(missing.clone())? + PEM_BEGIN.len();
    let rest = &private_key[start..];
    let end = rest.find(PEM_END).ok_or(missing)?;
    let body: String = rest[..end].chars().filter(|c| !c.is_whitespace()).collect();
    STANDARD
        .decode(body)
        .map_err(|_| YandexInitError::KeyFormat("private_key PEM block is not base64"))
}

struct Cached {
    token: String,
    fetched: Instant,
}

/// Источник IAM-токена с кэшем.
pub(super) struct Auth {
    key: ServiceKey,
    token_url: String,
    cached: tokio::sync::Mutex<Option<Cached>>,
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth")
            .field("key", &self.key)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl Auth {
    pub(super) fn new(key: ServiceKey, iam_url: &str) -> Self {
        Self {
            key,
            token_url: format!("{}/iam/v1/tokens", iam_url.trim_end_matches('/')),
            cached: tokio::sync::Mutex::new(None),
        }
    }

    /// Токен из кэша или свежий. Обмен идёт под замком: одновременные вызовы ждут один обмен.
    pub(super) async fn token(&self, http: &reqwest::Client) -> Result<String, ProviderError> {
        let mut cached = self.cached.lock().await;
        if let Some(entry) = cached.as_ref()
            && entry.fetched.elapsed() < TOKEN_TTL
        {
            return Ok(entry.token.clone());
        }
        *cached = None;
        let token = self.exchange(http).await?;
        *cached = Some(Cached {
            token: token.clone(),
            fetched: Instant::now(),
        });
        Ok(token)
    }

    /// Забыть токен, на который API ответил 401. Если в кэше уже другой токен, его обновил
    /// соседний вызов, и он остаётся.
    pub(super) async fn forget(&self, rejected: &str) {
        let mut cached = self.cached.lock().await;
        if cached.as_ref().is_some_and(|entry| entry.token == rejected) {
            *cached = None;
        }
    }

    async fn exchange(&self, http: &reqwest::Client) -> Result<String, ProviderError> {
        let issued_at = u64::try_from(chrono::Utc::now().timestamp()).unwrap_or_default();
        let jwt = self
            .key
            .jwt(issued_at)
            .ok_or_else(|| ProviderError::new(ErrorKind::Auth, "cannot sign the IAM request"))?;
        tracing::debug!("yandex IAM token exchange");
        let response = http
            .post(&self.token_url)
            .json(&json!({"jwt": jwt}))
            .send()
            .await
            .map_err(|error| {
                let why = if error.is_timeout() {
                    "timed out"
                } else if error.is_connect() {
                    "could not connect"
                } else {
                    "connection failed"
                };
                ProviderError::new(ErrorKind::Unavailable, format!("IAM token exchange {why}"))
            })?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(|_| {
            ProviderError::new(
                ErrorKind::Unavailable,
                "IAM token exchange answer was cut off",
            )
        })?;
        let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if status != 200 {
            let kind = match status {
                429 => ErrorKind::RateLimited,
                500.. => ErrorKind::Unavailable,
                _ => ErrorKind::Auth,
            };
            let message = scrub(value["message"].as_str().unwrap_or_default(), &[&jwt]);
            tracing::warn!(status, "yandex IAM token exchange refused");
            return Err(ProviderError::new(
                kind,
                format!("IAM token exchange: HTTP {status}: {message}"),
            ));
        }
        value["iamToken"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                ProviderError::new(
                    ErrorKind::Malformed,
                    "IAM token exchange answer has no iamToken",
                )
            })
    }
}

/// Длиннее этого текст ошибки сервера обрезается.
const MAX_MESSAGE_CHARS: usize = 300;

/// Текст от сервера без секретов и не длиннее предела.
pub(super) fn scrub(text: &str, secrets: &[&str]) -> String {
    let mut text = text.to_owned();
    for secret in secrets {
        if !secret.is_empty() {
            text = text.replace(secret, "[redacted]");
        }
    }
    text.chars().take(MAX_MESSAGE_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pem_block_is_found_after_the_yandex_banner() {
        let key = "PLEASE DO NOT REMOVE THIS LINE! Yandex.Cloud SA Key ID <fakekeyid>\n\
                   -----BEGIN PRIVATE KEY-----\nAAEC\nAwQF\n-----END PRIVATE KEY-----\n";
        assert_eq!(pkcs8_der(key).unwrap(), vec![0, 1, 2, 3, 4, 5]);
        let bare = "-----BEGIN PRIVATE KEY-----\r\nAAEC\r\n-----END PRIVATE KEY-----";
        assert_eq!(pkcs8_der(bare).unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn a_key_without_a_pem_block_is_refused_without_quoting_it() {
        for bad in [
            "SECRET-MATERIAL",
            "-----BEGIN PRIVATE KEY-----\nSECRET-MATERIAL",
            "-----BEGIN PRIVATE KEY-----\n!!SECRET!!\n-----END PRIVATE KEY-----",
        ] {
            let error = pkcs8_der(bad).unwrap_err();
            assert!(matches!(error, YandexInitError::KeyFormat(_)));
            assert!(!error.to_string().contains("SECRET"));
            assert!(!format!("{error:?}").contains("SECRET"));
        }
    }

    #[test]
    fn a_key_file_that_is_not_a_key_is_refused_without_quoting_it() {
        for raw in [
            "SECRET-MATERIAL".to_owned(),
            r#"{"id":"k","service_account_id":"sa"}"#.to_owned(),
            r#"{"id":"","service_account_id":"sa","private_key":"SECRET-MATERIAL"}"#.to_owned(),
            // PEM-блок на месте, но внутри не ключ RSA.
            format!(
                r#"{{"id":"k","service_account_id":"sa","private_key":"{PEM_BEGIN}\nU0VDUkVU\n{PEM_END}"}}"#
            ),
        ] {
            let error = ServiceKey::parse(raw.as_bytes()).unwrap_err();
            assert!(matches!(error, YandexInitError::KeyFormat(_)), "{error:?}");
            assert!(!error.to_string().contains("SECRET"));
            assert!(!error.to_string().contains("U0VDUkVU"));
        }
    }

    #[test]
    fn scrub_removes_secrets_and_bounds_the_length() {
        assert_eq!(
            scrub(
                "bad token t1.abc for jwt eyJ.x.y",
                &["t1.abc", "eyJ.x.y", ""]
            ),
            "bad token [redacted] for jwt [redacted]"
        );
        assert_eq!(scrub(&"я".repeat(1000), &[]).chars().count(), 300);
    }
}
