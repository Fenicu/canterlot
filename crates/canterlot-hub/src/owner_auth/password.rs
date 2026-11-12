//! Пароль владельца: argon2id в строке PHC. Проверка идёт в `spawn_blocking` за семафором
//! процесса; когда сравнивать не с чем, сравнение идёт с фиктивным хешем тех же параметров,
//! чтобы время ответа не выдавало, задан ли пароль и верен ли логин.

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use argon2::{Algorithm, Argon2, Params, PasswordHasher, PasswordVerifier, Version};
use sqlx::{PgConnection, PgPool};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

pub const MIN_BYTES: usize = 10;
pub const MAX_BYTES: usize = 1024;

/// Боевые параметры: 64 МиБ, три прохода, один поток.
pub const M_COST_KIB: u32 = 64 * 1024;
pub const T_COST: u32 = 3;
pub const P_COST: u32 = 1;
const PERMITS: usize = 2;
const PERMIT_WAIT: Duration = Duration::from_secs(5);

/// Длина 10..=1024 байт, без правил состава.
pub fn acceptable(password: &str) -> bool {
    (MIN_BYTES..=MAX_BYTES).contains(&password.len())
}

#[derive(Clone)]
pub struct Passwords {
    inner: Arc<Inner>,
}

struct Inner {
    params: Params,
    permits: Arc<Semaphore>,
    permit_wait: Duration,
    dummy: OnceLock<String>,
    /// Поток, на котором посчитан фиктивный хеш (только тесты).
    #[cfg(test)]
    dummy_thread: std::sync::Mutex<Option<std::thread::ThreadId>>,
}

impl Default for Passwords {
    fn default() -> Self {
        Self::new(M_COST_KIB, T_COST, P_COST, PERMITS, PERMIT_WAIT)
    }
}

impl std::fmt::Debug for Passwords {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Passwords").finish_non_exhaustive()
    }
}

impl Passwords {
    /// Параметры задаются явно только в тестах: там боевые 64 МиБ слишком медленны.
    pub fn new(
        m_cost: u32,
        t_cost: u32,
        p_cost: u32,
        permits: usize,
        permit_wait: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                params: Params::new(m_cost, t_cost, p_cost, None).expect("argon2 parameters"),
                permits: Arc::new(Semaphore::new(permits.max(1))),
                permit_wait,
                dummy: OnceLock::new(),
                #[cfg(test)]
                dummy_thread: std::sync::Mutex::new(None),
            }),
        }
    }

    fn hasher(&self) -> Argon2<'static> {
        Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            self.inner.params.clone(),
        )
    }

    /// Разрешение на одну проверку; `None`, если очередь не продвинулась за отведённое время.
    pub async fn permit(&self) -> Option<OwnedSemaphorePermit> {
        tokio::time::timeout(
            self.inner.permit_wait,
            self.inner.permits.clone().acquire_owned(),
        )
        .await
        .ok()?
        .ok()
    }

    /// Хеш нового пароля. Вызывающий держит разрешение.
    pub async fn hash(&self, password: String) -> Result<String, &'static str> {
        let hasher = self.hasher();
        tokio::task::spawn_blocking(move || {
            hasher
                .hash_password(password.as_bytes())
                .map(|hash| hash.to_string())
                .map_err(|_| "password_hash_error")
        })
        .await
        .map_err(|_| "password_hash_error")?
    }

    /// Проверка против сохранённого хеша, а без него против фиктивного: одна проверка argon2
    /// на любой исход. Фиктивный хеш при первой нужде строится в той же блокирующей задаче,
    /// не на потоке runtime. Вызывающий держит разрешение.
    pub async fn verify(&self, password: String, stored: Option<String>) -> bool {
        let real = stored.is_some();
        let inner = self.inner.clone();
        let hasher = self.hasher();
        let matched = tokio::task::spawn_blocking(move || {
            let phc = match &stored {
                Some(phc) => phc.as_str(),
                None => dummy(&inner, &hasher),
            };
            hasher.verify_password(password.as_bytes(), phc).is_ok()
        })
        .await
        .unwrap_or(false);
        real && matched
    }
}

fn dummy<'a>(inner: &'a Inner, hasher: &Argon2<'static>) -> &'a str {
    inner.dummy.get_or_init(|| {
        #[cfg(test)]
        {
            *inner.dummy_thread.lock().unwrap() = Some(std::thread::current().id());
        }
        let mut salt = [0u8; 16];
        getrandom::fill(&mut salt).expect("OS random source");
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).expect("OS random source");
        hasher
            .hash_password_with_salt(&secret, &salt)
            .expect("dummy argon2 hash")
            .to_string()
    })
}

pub async fn load(pool: &PgPool, owner_id: Uuid) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("select phc from owner_passwords where owner_id=$1")
        .bind(owner_id)
        .fetch_optional(pool)
        .await
}

pub async fn exists(pool: &PgPool, owner_id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("select exists(select 1 from owner_passwords where owner_id=$1)")
        .bind(owner_id)
        .fetch_one(pool)
        .await
}

pub async fn store(conn: &mut PgConnection, owner_id: Uuid, phc: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into owner_passwords(owner_id,phc) values($1,$2)
         on conflict(owner_id) do update set phc=excluded.phc,changed_at=clock_timestamp()",
    )
    .bind(owner_id)
    .bind(phc)
    .execute(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cheap() -> Passwords {
        Passwords::new(8, 1, 1, 2, Duration::from_secs(5))
    }

    #[tokio::test]
    async fn verify_matches_only_the_stored_password() {
        let passwords = cheap();
        let phc = passwords.hash("correct horse".into()).await.unwrap();
        assert!(phc.starts_with("$argon2id$v=19$m=8,t=1,p=1$"));
        assert!(
            passwords
                .verify("correct horse".into(), Some(phc.clone()))
                .await
        );
        assert!(!passwords.verify("wrong horse!".into(), Some(phc)).await);
        assert!(!passwords.verify("correct horse".into(), None).await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_dummy_hash_is_built_off_the_async_thread() {
        let passwords = cheap();
        assert!(!passwords.verify("no password yet".into(), None).await);
        let built = *passwords.inner.dummy_thread.lock().unwrap();
        assert!(built.is_some(), "the dummy hash was built");
        assert_ne!(
            built,
            Some(std::thread::current().id()),
            "64 MiB of argon2 must not block a runtime thread"
        );
    }

    #[test]
    fn default_parameters_are_the_documented_ones() {
        let passwords = Passwords::default();
        let params = &passwords.inner.params;
        assert_eq!(
            (params.m_cost(), params.t_cost(), params.p_cost()),
            (64 * 1024, 3, 1)
        );
    }

    #[test]
    fn length_rule_counts_bytes() {
        assert!(!acceptable("123456789"));
        assert!(acceptable("1234567890"));
        assert!(acceptable(&"x".repeat(1024)));
        assert!(!acceptable(&"x".repeat(1025)));
        assert!(acceptable("пароль12"));
    }

    #[tokio::test]
    async fn permits_time_out_instead_of_queueing_forever() {
        let passwords = Passwords::new(8, 1, 1, 1, Duration::from_millis(50));
        let held = passwords.permit().await.unwrap();
        assert!(passwords.permit().await.is_none());
        drop(held);
        assert!(passwords.permit().await.is_some());
    }
}
