//! Собственный вход владельца (ADR-0005): пароль, сеансы браузера, известные устройства,
//! защита от перебора и ссылка восстановления. HTTP-слой живёт в `http::auth` и
//! `http::signin`; здесь только правила и база.

use std::net::IpAddr;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

pub mod cli;
pub mod device;
pub mod password;
pub mod recovery;
pub mod sessions;
pub mod throttle;

/// Имя cookie сеанса. Префикс `__Host-` запрещает подброс cookie с соседних поддоменов.
pub const SESSION_COOKIE: &str = "__Host-canterlot_session";
/// Имя cookie известного устройства.
pub const DEVICE_COOKIE: &str = "__Host-canterlot_device";
/// Срок cookie известного устройства: 400 дней, предел браузеров.
pub const DEVICE_COOKIE_MAX_AGE_SECS: i64 = 400 * 24 * 3600;
/// Область шины для уведомлений о сеансах.
pub const BUS_SCOPE: &str = "auth";

/// Вид секрета. У каждого свой префикс хеша, поэтому секреты разных видов
/// не взаимозаменяемы по построению.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Secret {
    Session,
    Device,
    QrSecret,
    QrPoll,
    Recovery,
}

impl Secret {
    fn prefix(self) -> &'static [u8] {
        match self {
            Self::Session => b"session\0",
            Self::Device => b"device\0",
            Self::QrSecret => b"qr_secret\0",
            Self::QrPoll => b"qr_poll\0",
            Self::Recovery => b"recovery\0",
        }
    }
}

/// 32 байта из ОС в base64url без выравнивания.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS random source");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// SHA-256 над префиксом вида и токеном.
pub fn digest(kind: Secret, token: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(kind.prefix());
    hasher.update(token.as_bytes());
    hasher.finalize().to_vec()
}

/// Токен выглядит как наш: base64url ровно 43 символа. Чужое значение не доходит до базы.
pub fn token_shape_ok(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Способ входа, создавший сеанс.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = SignInMethod)]
pub enum Method {
    Password,
    Passkey,
    Qr,
    Recovery,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Passkey => "passkey",
            Self::Qr => "qr",
            Self::Recovery => "recovery",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "password" => Self::Password,
            "passkey" => Self::Passkey,
            "qr" => Self::Qr,
            "recovery" => Self::Recovery,
            _ => return None,
        })
    }

    /// Вход по QR ничего не доказывает сам: такой сеанс рождается несвежим.
    pub fn proves_credential(self) -> bool {
        !matches!(self, Self::Qr)
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Password => "по паролю",
            Self::Passkey => "по ключу доступа",
            Self::Qr => "по QR-коду",
            Self::Recovery => "по ссылке восстановления",
        }
    }
}

/// Причина отзыва сеанса.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevokeReason {
    Logout,
    Owner,
    Others,
    PasswordChanged,
    Idle,
    Server,
    Limit,
    Cascade,
}

impl RevokeReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Logout => "logout",
            Self::Owner => "owner",
            Self::Others => "others",
            Self::PasswordChanged => "password_changed",
            Self::Idle => "idle",
            Self::Server => "server",
            Self::Limit => "limit",
            Self::Cascade => "cascade",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "logout" => Self::Logout,
            "owner" => Self::Owner,
            "others" => Self::Others,
            "password_changed" => Self::PasswordChanged,
            "idle" => Self::Idle,
            "server" => Self::Server,
            "limit" => Self::Limit,
            "cascade" => Self::Cascade,
            _ => return None,
        })
    }

    /// Отзыв владельцем уносит и сеансы, вход которых подтвердил отозванный.
    pub fn cascades(self) -> bool {
        matches!(
            self,
            Self::Owner | Self::Others | Self::PasswordChanged | Self::Server
        )
    }
}

/// Ключ корзины перебора: IPv4 как есть (и из `::ffff:a.b.c.d`), прочий IPv6 по /64,
/// неизвестный адрес в одной общей корзине.
pub fn bucket_key(ip: Option<IpAddr>) -> String {
    match ip.map(|ip| ip.to_canonical()) {
        Some(IpAddr::V4(v4)) => format!("ip:{v4}"),
        Some(IpAddr::V6(v6)) => {
            let segments = v6.segments();
            let net = std::net::Ipv6Addr::new(
                segments[0],
                segments[1],
                segments[2],
                segments[3],
                0,
                0,
                0,
                0,
            );
            format!("net:{net}/64")
        }
        None => "unknown".into(),
    }
}

/// Подпись устройства для списка сеансов и пушей: выводится при чтении из user agent
/// и подсказок клиента (`device`), см. `device::label`.
pub fn label(user_agent: Option<&str>, client_hints: Option<&serde_json::Value>) -> Option<String> {
    device::label(user_agent, client_hints)
}

/// Текст пуша о новом входе: «Firefox 159 · Linux · Москва · 203.0.113.9 · по паролю».
pub fn sign_in_text(
    label: Option<&str>,
    location: Option<&str>,
    ip: Option<IpAddr>,
    method: Method,
) -> String {
    let ip = ip.map(|ip| ip.to_canonical().to_string());
    [
        Some(label.unwrap_or("неизвестное устройство")),
        location,
        Some(ip.as_deref().unwrap_or("IP неизвестен")),
        Some(method.title()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

/// Будит читателей шины после commit: в этом процессе сразу, в других через `pg_notify`.
/// Решает база: уведомление только повод перепроверить сеанс.
pub async fn notify_changed(pool: &sqlx::PgPool, kind: &str) {
    crate::bus::EventBus::local().publish(BUS_SCOPE, kind, 0);
    let _ = sqlx::query("select pg_notify('canterlot_events', $1)")
        .bind(format!("{BUS_SCOPE}|{kind}"))
        .execute(pool)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_keys_aggregate_ipv6_and_unmap_ipv4() {
        assert_eq!(
            bucket_key(Some("203.0.113.9".parse().unwrap())),
            "ip:203.0.113.9"
        );
        assert_eq!(
            bucket_key(Some("::ffff:203.0.113.9".parse().unwrap())),
            "ip:203.0.113.9"
        );
        assert_eq!(
            bucket_key(Some("2001:db8:1:2:aaaa::1".parse().unwrap())),
            "net:2001:db8:1:2::/64"
        );
        assert_eq!(
            bucket_key(Some("2001:db8:1:2:ffff::9".parse().unwrap())),
            bucket_key(Some("2001:db8:1:2::77".parse().unwrap()))
        );
        assert_eq!(bucket_key(None), "unknown");
    }

    #[test]
    fn secret_kinds_never_share_a_digest() {
        let token = generate_token();
        assert!(token_shape_ok(&token));
        let kinds = [
            Secret::Session,
            Secret::Device,
            Secret::QrSecret,
            Secret::QrPoll,
            Secret::Recovery,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(digest(*a, &token), digest(*b, &token));
            }
        }
    }

    #[test]
    fn only_owner_revocations_cascade() {
        for reason in ["owner", "others", "password_changed", "server"] {
            assert!(RevokeReason::parse(reason).unwrap().cascades());
        }
        for reason in ["logout", "idle", "limit", "cascade"] {
            assert!(!RevokeReason::parse(reason).unwrap().cascades());
        }
    }
}
