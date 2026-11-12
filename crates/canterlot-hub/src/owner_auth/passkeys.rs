//! Ключи доступа (WebAuthn, спецификация §4.2): хранение ключей и одноразовых состояний
//! обряда между start и finish. Обряд проверяет `webauthn-rs` (поток без discoverable:
//! `allowCredentials` со всеми ключами владельца, проверка пользователя обязательна).
//! Состояние, идентификаторы и открытые ключи в журналы не пишутся.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;
use webauthn_rs::prelude::{AuthenticationResult, Passkey};

/// Состояние обряда живёт 5 минут и используется один раз.
pub const CHALLENGE_TTL_SECS: i64 = 300;
/// Начал входа по ключу с одной корзины адресов за минуту.
pub const STARTS_PER_MINUTE: i64 = 10;
/// Неистёкших состояний входа всего: публичный start не раздувает таблицу.
pub const MAX_SIGN_IN_CHALLENGES: i64 = 50;
pub const NAME_CHARS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Register,
    SignIn,
    Reauth,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::SignIn => "sign_in",
            Self::Reauth => "reauth",
        }
    }
}

fn seconds(value: Option<f64>) -> Duration {
    Duration::from_secs_f64(value.unwrap_or(1.0).clamp(1.0, 3600.0).ceil())
}

/// Можно ли начать вход по ключу из корзины `bucket`: `Some(срок)` — нельзя.
pub async fn sign_in_limited(pool: &PgPool, bucket: &str) -> Result<Option<Duration>, sqlx::Error> {
    let (recent, oldest, live, soonest): (i64, Option<f64>, i64, Option<f64>) = sqlx::query_as(
        "select count(*) filter (where bucket=$1 and created_at>now()-interval '1 minute'),
                extract(epoch from min(created_at) filter (where bucket=$1
                  and created_at>now()-interval '1 minute')+interval '1 minute'-now())::float8,
                count(*) filter (where expires_at>now()),
                extract(epoch from min(expires_at) filter (where expires_at>now())-now())::float8
         from owner_webauthn_challenges where kind='sign_in'",
    )
    .bind(bucket)
    .fetch_one(pool)
    .await?;
    Ok(if recent >= STARTS_PER_MINUTE {
        Some(seconds(oldest))
    } else if live >= MAX_SIGN_IN_CHALLENGES {
        Some(seconds(soonest))
    } else {
        None
    })
}

/// Сохраняет состояние начатого обряда; `session` для регистрации и повторного входа.
pub async fn store_challenge(
    pool: &PgPool,
    owner_id: Uuid,
    kind: Kind,
    session: Option<Uuid>,
    bucket: Option<&str>,
    state: Value,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into owner_webauthn_challenges(id,owner_id,kind,session_id,bucket,state,expires_at)
         values($1,$2,$3,$4,$5,$6,now()+$7*interval '1 second')",
    )
    .bind(id)
    .bind(owner_id)
    .bind(kind.as_str())
    .bind(session)
    .bind(bucket)
    .bind(state)
    .bind(CHALLENGE_TTL_SECS as f64)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Забирает состояние один раз: строка удаляется при любой попытке, а состояние
/// возвращается, только если вид, сеанс и срок совпали.
pub async fn take_challenge(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    kind: Kind,
    session: Option<Uuid>,
) -> Result<Option<Value>, sqlx::Error> {
    let row: Option<(String, Option<Uuid>, bool, Value)> = sqlx::query_as(
        "delete from owner_webauthn_challenges where id=$1 and owner_id=$2
         returning kind,session_id,expires_at>now(),state",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(stored, bound, live, state)| {
        (stored == kind.as_str() && bound == session && live).then_some(state)
    }))
}

#[derive(Debug, Clone)]
pub struct Stored {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    /// Ключ синхронизируется между устройствами (менеджер паролей, iCloud, Google).
    pub backed_up: bool,
    pub passkey: Passkey,
}

/// Ключи владельца по порядку добавления.
pub async fn list(pool: &PgPool, owner_id: Uuid) -> Result<Vec<Stored>, sqlx::Error> {
    let rows: Vec<(Uuid, String, DateTime<Utc>, Option<DateTime<Utc>>, Value)> = sqlx::query_as(
        "select id,name,created_at,last_used_at,passkey from owner_passkeys
         where owner_id=$1 order by created_at,id",
    )
    .bind(owner_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, name, created_at, last_used_at, value)| {
            let backed_up = value["cred"]["backup_state"].as_bool().unwrap_or(false);
            let passkey = serde_json::from_value(value)
                .inspect_err(|_| {
                    tracing::warn!(kind = "passkey_unreadable", passkey_id = %id, "stored passkey cannot be read");
                })
                .ok()?;
            Some(Stored {
                id,
                name,
                created_at,
                last_used_at,
                backed_up,
                passkey,
            })
        })
        .collect())
}

pub async fn exists(pool: &PgPool, owner_id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("select exists(select 1 from owner_passkeys where owner_id=$1)")
        .bind(owner_id)
        .fetch_one(pool)
        .await
}

/// Новый ключ. `None`: такой ключ уже есть.
pub async fn add(
    pool: &PgPool,
    owner_id: Uuid,
    passkey: &Passkey,
    name: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    let value = serde_json::to_value(passkey).map_err(|error| sqlx::Error::Encode(error.into()))?;
    sqlx::query_scalar(
        "insert into owner_passkeys(id,owner_id,credential_id,passkey,name) values($1,$2,$3,$4,$5)
         on conflict(credential_id) do nothing returning id",
    )
    .bind(Uuid::now_v7())
    .bind(owner_id)
    .bind(passkey.cred_id().as_slice())
    .bind(value)
    .bind(name)
    .fetch_optional(pool)
    .await
}

/// После успешной проверки: счётчик и состояние резервной копии сохраняются, отмечается
/// последнее использование. `false`: ключ не найден (удалён между start и finish).
pub async fn record_use(
    pool: &PgPool,
    owner_id: Uuid,
    result: &AuthenticationResult,
) -> Result<bool, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    let row: Option<(Uuid, Value)> = sqlx::query_as(
        "select id,passkey from owner_passkeys where owner_id=$1 and credential_id=$2 for update",
    )
    .bind(owner_id)
    .bind(result.cred_id().as_slice())
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, value)) = row else {
        return Ok(false);
    };
    let mut passkey: Passkey =
        serde_json::from_value(value).map_err(|error| sqlx::Error::Decode(error.into()))?;
    passkey.update_credential(result);
    let value =
        serde_json::to_value(&passkey).map_err(|error| sqlx::Error::Encode(error.into()))?;
    sqlx::query("update owner_passkeys set passkey=$2,last_used_at=now() where id=$1")
        .bind(id)
        .bind(value)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// Новое имя. `false`: ключа нет.
pub async fn rename(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    name: &str,
) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query("update owner_passkeys set name=$3 where owner_id=$1 and id=$2")
            .bind(owner_id)
            .bind(id)
            .bind(name)
            .execute(pool)
            .await?
            .rows_affected()
            > 0,
    )
}

/// Удаляет ключ. `false`: ключа нет.
pub async fn delete(pool: &PgPool, owner_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query("delete from owner_passkeys where owner_id=$1 and id=$2")
            .bind(owner_id)
            .bind(id)
            .execute(pool)
            .await?
            .rows_affected()
            > 0,
    )
}
