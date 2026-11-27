//! Коды привязки машины: владелец заводит машину заранее («ожидает подключения») и получает
//! одноразовый код; воркер меняет код на токен через `WorkerEnroll.Join` без токена машины.
//! Хранится только SHA-256 нормализованного кода. Сам код и выданный токен не попадают ни в
//! журнал, ни в тексты ошибок.

use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgConnection, PgPool};
use uuid::Uuid;

/// Срок кода: владелец вставляет его на новой машине сразу после выдачи.
pub const CODE_TTL: Duration = Duration::from_secs(15 * 60);
pub const DEFAULT_INSTALL_URL: &str =
    "https://github.com/Fenicu/canterlot/releases/latest/download/install.sh";
/// Попыток обмена в минуту с одного адреса и всего.
pub const JOIN_PER_PEER_PER_MINUTE: usize = 10;
pub const JOIN_GLOBAL_PER_MINUTE: usize = 60;
const NAME_MAX_CHARS: usize = 64;
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const CODE_CHARS: usize = 26;

/// Что нужно хабу, чтобы выдавать коды и отвечать на обмен.
#[derive(Clone, Debug)]
pub struct EnrollSettings {
    /// DER корневого сертификата из `tls_ca`; без него нет ни `GetCa`, ни отпечатка в команде.
    pub ca_der: Option<Vec<u8>>,
    /// Адрес gRPC для команды и ответа `Join`: `[enroll] grpc_endpoint`.
    pub grpc_endpoint: Option<String>,
    pub install_url: String,
}

impl Default for EnrollSettings {
    fn default() -> Self {
        Self {
            ca_der: None,
            grpc_endpoint: None,
            install_url: DEFAULT_INSTALL_URL.to_owned(),
        }
    }
}

impl EnrollSettings {
    /// Читает `tls_ca` (PEM) один раз при старте: битый файл — ошибка запуска, а не
    /// молчаливое `ca_unavailable` у каждого воркера.
    pub fn from_config(config: &crate::config::HubConfig) -> Result<Self, &'static str> {
        let ca_der = match &config.tls_ca {
            Some(path) => {
                let pem = std::fs::read_to_string(path).map_err(|_| "tls_ca_file_error")?;
                Some(ca_der_from_pem(&pem).ok_or("tls_ca_invalid")?)
            }
            None => None,
        };
        Ok(Self {
            ca_der,
            grpc_endpoint: config.enroll.grpc_endpoint.clone(),
            install_url: config.enroll.install_url.clone(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EnrollError {
    /// Неизвестный, использованный, истёкший и отозванный код неразличимы.
    #[error("join_code_invalid")]
    Invalid,
    #[error("machine_not_pending")]
    NotPending,
    #[error("machine_not_found")]
    NotFound,
    #[error("machine_name_invalid")]
    InvalidName,
    #[error("enroll_endpoint_invalid")]
    InvalidEndpoint,
    #[error("install_url_invalid")]
    InvalidInstallUrl,
    #[error("random_source_failed")]
    Random,
    #[error("db_error")]
    Database(#[from] sqlx::Error),
}

/// Выданный код. `command` кода не содержит: код вставляют отдельно, когда его спросят.
pub struct JoinCode {
    pub machine_id: Uuid,
    pub code: String,
    pub expires_at: DateTime<Utc>,
    pub command: String,
}

impl std::fmt::Debug for JoinCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinCode")
            .field("machine_id", &self.machine_id)
            .field("code", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .field("command", &self.command)
            .finish()
    }
}

pub struct Redeemed {
    pub machine_id: Uuid,
    pub token: String,
}

impl std::fmt::Debug for Redeemed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redeemed")
            .field("machine_id", &self.machine_id)
            .field("token", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PendingMachine {
    pub machine_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// `None`: код уже обменян, а воркер ещё не подключился (или код отозван перевыпуском).
    pub code_expires_at: Option<DateTime<Utc>>,
}

/// `sha256:` и шестнадцатеричный SHA-256 DER сертификата CA: это значение `--pin`.
pub fn pin_of(ca_der: &[u8]) -> String {
    format!("sha256:{}", lower_hex(&Sha256::digest(ca_der)))
}

fn lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// DER первого блока `CERTIFICATE` из PEM.
pub fn ca_der_from_pem(pem: &str) -> Option<Vec<u8>> {
    let begin = "-----BEGIN CERTIFICATE-----";
    let end = "-----END CERTIFICATE-----";
    let start = pem.find(begin)? + begin.len();
    let stop = start + pem[start..].find(end)?;
    let body: String = pem[start..stop]
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    STANDARD.decode(body).ok().filter(|der| !der.is_empty())
}

/// Команда для новой машины. Адрес и ссылка проверены: в строке нет ничего, что оболочка
/// поняла бы иначе, чем владелец.
pub fn join_command(install_url: &str, endpoint: &str, pin: &str) -> String {
    format!("curl -fsSL {install_url} | bash -s -- join --hub {endpoint} --pin {pin}")
}

/// `host:port`: имя DNS, IPv4 или `[IPv6]`, порт 1–65535.
pub fn valid_endpoint(endpoint: &str) -> bool {
    let Some((host, port)) = endpoint.rsplit_once(':') else {
        return false;
    };
    if port.parse::<u16>().map_or(true, |port| port == 0) {
        return false;
    }
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    }
    !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !host.starts_with(['-', '.'])
}

/// Только `https://` и знаки, которые оболочка не трактует.
pub fn valid_install_url(url: &str) -> bool {
    url.strip_prefix("https://").is_some_and(|rest| {
        !rest.is_empty()
            && url.len() <= 2048
            && url
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~:/%".contains(&b))
    })
}

/// Нормализованный код: 26 знаков Crockford base32 заглавными. Регистр, дефисы и пробелы
/// не важны; `O` читается как `0`, `I` и `L` как `1`, как принято в Crockford.
pub fn normalize(code: &str) -> Option<String> {
    let mut out = String::with_capacity(CODE_CHARS);
    for c in code.chars() {
        if c == '-' || c.is_whitespace() {
            continue;
        }
        let c = match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        if !c.is_ascii() || !CROCKFORD.contains(&(c as u8)) {
            return None;
        }
        out.push(c);
        if out.len() > CODE_CHARS {
            return None;
        }
    }
    (out.len() == CODE_CHARS).then_some(out)
}

fn code_digest(normalized: &str) -> Vec<u8> {
    Sha256::digest(normalized.as_bytes()).to_vec()
}

/// 128 случайных бит в 26 знаках Crockford; показ группами по 4 через дефис.
fn new_code() -> Result<(String, Vec<u8>), EnrollError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| EnrollError::Random)?;
    let value = u128::from_be_bytes(bytes);
    let normalized: String = (0..CODE_CHARS)
        .map(|i| CROCKFORD[((value >> (5 * (CODE_CHARS - 1 - i))) & 31) as usize] as char)
        .collect();
    let shown = normalized
        .as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).expect("ascii"))
        .collect::<Vec<_>>()
        .join("-");
    Ok((shown, code_digest(&normalized)))
}

fn checked_name(name: &str) -> Result<String, EnrollError> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > NAME_MAX_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(EnrollError::InvalidName);
    }
    Ok(name.to_owned())
}

fn checked_tail(endpoint: &str, install_url: &str, ca_der: &[u8]) -> Result<String, EnrollError> {
    if !valid_endpoint(endpoint) {
        return Err(EnrollError::InvalidEndpoint);
    }
    if !valid_install_url(install_url) {
        return Err(EnrollError::InvalidInstallUrl);
    }
    Ok(join_command(install_url, endpoint, &pin_of(ca_der)))
}

async fn insert_code(
    conn: &mut PgConnection,
    machine_id: Uuid,
    digest: &[u8],
) -> Result<DateTime<Utc>, sqlx::Error> {
    sqlx::query_scalar(
        "insert into machine_join_codes (machine_id, code_sha256, expires_at)
         values ($1, $2, now() + make_interval(secs => $3))
         returning expires_at",
    )
    .bind(machine_id)
    .bind(digest)
    .bind(CODE_TTL.as_secs_f64())
    .fetch_one(&mut *conn)
    .await
}

/// Заводит ожидающую машину и её код. Учётных данных у машины нет до обмена.
pub async fn issue(
    pool: &PgPool,
    owner_id: Uuid,
    name: &str,
    endpoint: &str,
    install_url: &str,
    ca_der: &[u8],
) -> Result<JoinCode, EnrollError> {
    let name = checked_name(name)?;
    let command = checked_tail(endpoint, install_url, ca_der)?;
    let (code, digest) = new_code()?;
    let machine_id = Uuid::now_v7();
    let expires_at = crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            sqlx::query(
                "insert into machines (id, owner_id, name, pending_enrollment)
                 values ($1, $2, $3, true)",
            )
            .bind(machine_id)
            .bind(owner_id)
            .bind(name)
            .execute(&mut *conn)
            .await?;
            insert_code(conn, machine_id, &digest).await
        })
    })
    .await?;
    tracing::info!(kind = "join_code_issued", machine_id = %machine_id, %expires_at, "join code issued");
    Ok(JoinCode {
        machine_id,
        code,
        expires_at,
        command,
    })
}

/// Ожидающая машина владельца под замком строки: `NotFound` или `NotPending` иначе.
async fn lock_pending(
    conn: &mut PgConnection,
    owner_id: Uuid,
    machine_id: Uuid,
) -> Result<(), EnrollError> {
    let pending: Option<bool> = sqlx::query_scalar(
        "select pending_enrollment from machines where id = $1 and owner_id = $2 for no key update",
    )
    .bind(machine_id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    match pending {
        None => Err(EnrollError::NotFound),
        Some(false) => Err(EnrollError::NotPending),
        Some(true) => Ok(()),
    }
}

/// Прежний код и все токены машины уходят: токен из потерянного ответа больше не годится.
async fn forget_secrets(conn: &mut PgConnection, machine_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("delete from machine_join_codes where machine_id = $1")
        .bind(machine_id)
        .execute(&mut *conn)
        .await?;
    let removed = sqlx::query("delete from machine_credentials where machine_id = $1")
        .bind(machine_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    if removed != 0 {
        crate::machines::notify_revoke(&mut *conn, machine_id).await?;
    }
    Ok(())
}

/// Новый код той же ожидающей машины: выход, когда ответ обмена потерялся.
pub async fn reissue(
    pool: &PgPool,
    owner_id: Uuid,
    machine_id: Uuid,
    endpoint: &str,
    install_url: &str,
    ca_der: &[u8],
) -> Result<JoinCode, EnrollError> {
    let command = checked_tail(endpoint, install_url, ca_der)?;
    let (code, digest) = new_code()?;
    let expires_at = crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            lock_pending(conn, owner_id, machine_id).await?;
            forget_secrets(conn, machine_id).await?;
            Ok::<_, EnrollError>(insert_code(conn, machine_id, &digest).await?)
        })
    })
    .await?;
    tracing::info!(kind = "join_code_reissued", machine_id = %machine_id, %expires_at, "join code reissued");
    Ok(JoinCode {
        machine_id,
        code,
        expires_at,
        command,
    })
}

/// Отзыв ожидающей машины: код, токены и сама машина удаляются. Если на машину уже
/// что-то сослалось (строки, которые держат ключ), строка остаётся без учётных данных и
/// перестаёт ждать подключения.
pub async fn revoke_pending(
    pool: &PgPool,
    owner_id: Uuid,
    machine_id: Uuid,
) -> Result<(), EnrollError> {
    let removed = crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            lock_pending(conn, owner_id, machine_id).await?;
            forget_secrets(conn, machine_id).await?;
            let mut savepoint = conn.begin().await?; // db::begin: savepoint
            let deleted = sqlx::query("delete from machines where id = $1")
                .bind(machine_id)
                .execute(&mut *savepoint)
                .await;
            match deleted {
                Ok(_) => {
                    savepoint.commit().await?;
                    Ok::<_, EnrollError>(true)
                }
                Err(error)
                    if error
                        .as_database_error()
                        .and_then(|e| e.code())
                        .is_some_and(|code| code == "23503") =>
                {
                    savepoint.rollback().await?;
                    sqlx::query("update machines set pending_enrollment = false where id = $1")
                        .bind(machine_id)
                        .execute(&mut *conn)
                        .await?;
                    Ok(false)
                }
                Err(error) => Err(error.into()),
            }
        })
    })
    .await?;
    tracing::info!(kind = "pending_machine_revoked", machine_id = %machine_id, removed, "pending machine revoked");
    Ok(())
}

/// Ожидающие машины владельца, старые первыми.
pub async fn list_pending(
    pool: &PgPool,
    owner_id: Uuid,
) -> Result<Vec<PendingMachine>, sqlx::Error> {
    sqlx::query_as(
        "select m.id as machine_id, m.name, m.created_at, c.expires_at as code_expires_at
         from machines m left join machine_join_codes c on c.machine_id = m.id
         where m.owner_id = $1 and m.pending_enrollment
         order by m.created_at, m.id",
    )
    .bind(owner_id)
    .fetch_all(pool)
    .await
}

/// Обмен кода на токен машины. Код одноразовый: строка кода удаляется в той же транзакции,
/// что выдаёт токен. Машина остаётся ожидающей до первого рукопожатия.
pub async fn redeem(pool: &PgPool, code: &str) -> Result<Redeemed, EnrollError> {
    let digest = code_digest(&normalize(code).ok_or(EnrollError::Invalid)?);
    let token = crate::machines::new_token()?;
    let token_digest = Sha256::digest(token.as_bytes()).to_vec();
    let machine_id = crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            let machine_id: Option<Uuid> = sqlx::query_scalar(
                "select machine_id from machine_join_codes where code_sha256 = $1",
            )
            .bind(&digest)
            .fetch_optional(&mut *conn)
            .await?;
            let machine_id = machine_id.ok_or(EnrollError::Invalid)?;
            // Замок строки машины раньше строки кода: тот же порядок, что у перевыпуска.
            let pending: Option<bool> = sqlx::query_scalar(
                "select pending_enrollment from machines where id = $1 for no key update",
            )
            .bind(machine_id)
            .fetch_optional(&mut *conn)
            .await?;
            if pending != Some(true) {
                return Err(EnrollError::Invalid);
            }
            let consumed = sqlx::query(
                "delete from machine_join_codes
                 where machine_id = $1 and code_sha256 = $2 and expires_at > now()",
            )
            .bind(machine_id)
            .bind(&digest)
            .execute(&mut *conn)
            .await?
            .rows_affected();
            if consumed == 0 {
                return Err(EnrollError::Invalid);
            }
            sqlx::query("delete from machine_credentials where machine_id = $1")
                .bind(machine_id)
                .execute(&mut *conn)
                .await?;
            sqlx::query(
                "insert into machine_credentials (machine_id, token_sha256) values ($1, $2)",
            )
            .bind(machine_id)
            .bind(&token_digest)
            .execute(&mut *conn)
            .await?;
            Ok(machine_id)
        })
    })
    .await?;
    Ok(Redeemed { machine_id, token })
}

/// Предел попыток обмена кода. Память процесса: хаб по умолчанию один процесс, и при
/// нескольких процессах каждый считает сам (общий предел умножается на их число). Код
/// несёт 128 бит, так что предел бережёт базу и журнал, а не стойкость кода.
pub struct JoinLimiter {
    epoch: Instant,
    per_peer: usize,
    global: usize,
    window: Duration,
    state: Mutex<LimiterState>,
}

#[derive(Default)]
struct LimiterState {
    global: VecDeque<Duration>,
    peers: HashMap<Option<IpAddr>, VecDeque<Duration>>,
}

impl Default for JoinLimiter {
    fn default() -> Self {
        Self::new(
            JOIN_PER_PEER_PER_MINUTE,
            JOIN_GLOBAL_PER_MINUTE,
            Duration::from_secs(60),
        )
    }
}

impl JoinLimiter {
    pub fn new(per_peer: usize, global: usize, window: Duration) -> Self {
        Self {
            epoch: Instant::now(),
            per_peer,
            global,
            window,
            state: Mutex::default(),
        }
    }

    pub fn admit(&self, peer: Option<IpAddr>) -> bool {
        self.admit_at(peer, self.epoch.elapsed())
    }

    /// `at` — время от создания предела. Учитываются только пропущенные попытки, поэтому
    /// таблица адресов не больше общего предела за окно.
    pub fn admit_at(&self, peer: Option<IpAddr>, at: Duration) -> bool {
        // `::ffff:a.b.c.d` и `a.b.c.d` — один адрес.
        let peer = peer.map(|ip| match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            IpAddr::V4(_) => ip,
        });
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let window = self.window;
        let fresh = |seen: &Duration| at.saturating_sub(*seen) < window;
        while state.global.front().is_some_and(|seen| !fresh(seen)) {
            state.global.pop_front();
        }
        state.peers.retain(|_, seen| {
            while seen.front().is_some_and(|first| !fresh(first)) {
                seen.pop_front();
            }
            !seen.is_empty()
        });
        if state.global.len() >= self.global
            || state.peers.get(&peer).map_or(0, VecDeque::len) >= self.per_peer
        {
            return false;
        }
        state.global.push_back(at);
        state.peers.entry(peer).or_default().push_back(at);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_round_trips_through_normalize() {
        let (shown, digest) = new_code().unwrap();
        assert_eq!(shown.len(), CODE_CHARS + 6);
        let normalized = normalize(&shown).unwrap();
        assert_eq!(code_digest(&normalized), digest);
        assert_eq!(
            normalize(&shown.to_lowercase().replace('-', " ")),
            Some(normalized)
        );
        assert_eq!(normalize("too-short"), None);
        assert_eq!(normalize(&format!("{shown}0")), None);
        assert_eq!(normalize("UUUU-UUUU-UUUU-UUUU-UUUU-UUUU-UU"), None);
    }

    #[test]
    fn limiter_counts_per_peer_and_globally() {
        let limiter = JoinLimiter::new(2, 3, Duration::from_secs(60));
        let a = Some("192.0.2.1".parse().unwrap());
        let b = Some("192.0.2.2".parse().unwrap());
        let t = Duration::from_secs(1);
        assert!(limiter.admit_at(a, t));
        assert!(limiter.admit_at(a, t));
        assert!(!limiter.admit_at(a, t), "per-peer limit");
        assert!(limiter.admit_at(b, t));
        assert!(!limiter.admit_at(b, t), "global limit");
        let later = Duration::from_secs(62);
        assert!(limiter.admit_at(a, later), "window slides");
        let mapped = Some("::ffff:192.0.2.1".parse().unwrap());
        assert!(limiter.admit_at(mapped, later));
        assert!(
            !limiter.admit_at(a, later),
            "mapped address is the same peer"
        );
    }

    #[test]
    fn endpoint_and_url_checks_keep_the_command_shell_safe() {
        for good in [
            "hub.example.internal:7443",
            "192.0.2.10:7443",
            "[2001:db8::1]:7443",
        ] {
            assert!(valid_endpoint(good), "{good}");
        }
        for bad in [
            "hub.example.internal",
            "hub:0",
            "hub;rm:7443",
            "$(id):7443",
            "-x:7443",
            ":7443",
        ] {
            assert!(!valid_endpoint(bad), "{bad}");
        }
        assert!(valid_install_url(DEFAULT_INSTALL_URL));
        for bad in [
            "http://example.com/install.sh",
            "https://",
            "https://example.com/i.sh;id",
            "https://example.com/$(id)",
        ] {
            assert!(!valid_install_url(bad), "{bad}");
        }
    }

    #[test]
    fn pem_to_der_and_pin() {
        let pem = "-----BEGIN CERTIFICATE-----\nMAA=\n-----END CERTIFICATE-----\n";
        let der = ca_der_from_pem(pem).unwrap();
        assert_eq!(der, [0x30, 0x00]);
        let pin = pin_of(&der);
        assert!(pin.starts_with("sha256:"));
        assert_eq!(pin.len(), 7 + 64);
        assert_eq!(ca_der_from_pem("no pem"), None);
    }
}
