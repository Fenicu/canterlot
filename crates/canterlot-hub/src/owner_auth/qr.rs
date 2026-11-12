//! Вход по QR (спецификация §7). Новое устройство показывает код со ссылкой
//! `{origin}/qr#<secret>` и опрашивает хаб своим `poll_token`; устройство с сеансом
//! смотрит запрос (`scanned`), одобряет или отклоняет. Сеанс создаёт опрос, первым
//! увидевший `approved`, в одной транзакции с переводом строки в `delivered`.

use std::{net::IpAddr, time::Duration};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    Method, Secret, digest, generate_token,
    geo::Location,
    sessions::{self, ClientInfo, Created},
    token_shape_ok,
};

/// Срок кода: страница входа показывает новый каждые 60 секунд.
pub const TTL_SECS: i64 = 60;
/// Одобренный вход забирается не позже минуты после одобрения.
pub const DELIVERY_SECS: i64 = 60;
/// Запусков с одной корзины адресов за минуту.
pub const STARTS_PER_MINUTE: i64 = 10;
/// Ожидающих кодов всего.
pub const MAX_PENDING: i64 = 20;
/// Одновременных опросов в процессе.
pub const MAX_POLLS: usize = 8;
/// Живых сеансов, вход которых подтвердил один сеанс (вместе с ещё не забранными
/// одобрениями): украденный несвежий сеанс не плодит сеансы без счёта.
pub const MAX_APPROVED_PER_SESSION: i64 = 5;
/// Долгий опрос ждёт не дольше.
pub const POLL_WAIT: Duration = Duration::from_secs(25);
/// Перепроверка строки без уведомления шины.
pub const RECHECK: Duration = Duration::from_millis(500);
/// Вид уведомления шины: код отсканирован, одобрен или отклонён.
pub const BUS_KIND: &str = "qr_changed";

/// Состояние кода, как его видит опрашивающее устройство.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[schema(as = QrState)]
pub enum State {
    Pending,
    Scanned,
    Approved,
    Denied,
    Expired,
}

#[derive(Debug, Clone)]
pub enum Start {
    Started {
        secret: String,
        poll_token: String,
        expires_at: DateTime<Utc>,
    },
    Limited {
        retry_after: Duration,
    },
}

fn seconds(value: Option<f64>) -> Duration {
    Duration::from_secs_f64(value.unwrap_or(1.0).clamp(1.0, 3600.0).ceil())
}

/// Новый код. Над пределами (корзина, всего ожидающих) — `Limited` со сроком повтора.
pub async fn start(
    pool: &PgPool,
    owner_id: Uuid,
    bucket: &str,
    client: &ClientInfo,
) -> Result<Start, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('owner_qr_start'))")
        .execute(&mut *tx)
        .await?;
    let (recent, oldest): (i64, Option<f64>) = sqlx::query_as(
        "select count(*),extract(epoch from min(created_at)+interval '1 minute'-now())::float8
         from owner_qr_logins where bucket=$1 and created_at>now()-interval '1 minute'",
    )
    .bind(bucket)
    .fetch_one(&mut *tx)
    .await?;
    if recent >= STARTS_PER_MINUTE {
        return Ok(Start::Limited {
            retry_after: seconds(oldest),
        });
    }
    let (pending, soonest): (i64, Option<f64>) = sqlx::query_as(
        "select count(*),extract(epoch from min(expires_at)-now())::float8 from owner_qr_logins
         where state in ('pending','scanned') and expires_at>now()",
    )
    .fetch_one(&mut *tx)
    .await?;
    if pending >= MAX_PENDING {
        return Ok(Start::Limited {
            retry_after: seconds(soonest),
        });
    }
    let secret = generate_token();
    let poll_token = generate_token();
    let expires_at: DateTime<Utc> = sqlx::query_scalar(
        "insert into owner_qr_logins(id,owner_id,secret_sha256,poll_sha256,bucket,expires_at,
                                     ip,user_agent,client_hints,location)
         values($1,$2,$3,$4,$5,now()+$6*interval '1 second',$7::inet,$8,$9,$10)
         returning expires_at",
    )
    .bind(Uuid::now_v7())
    .bind(owner_id)
    .bind(digest(Secret::QrSecret, &secret))
    .bind(digest(Secret::QrPoll, &poll_token))
    .bind(bucket)
    .bind(TTL_SECS as f64)
    .bind(client.ip_text())
    .bind(client.user_agent())
    .bind(client.client_hints())
    .bind(client.location_json())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Start::Started {
        secret,
        poll_token,
        expires_at,
    })
}

/// Что видит опрос: состояние и, для `Approved`, строка, которую можно выдать.
pub async fn observe(
    pool: &PgPool,
    poll_token: &str,
) -> Result<(State, Option<Uuid>), sqlx::Error> {
    if !token_shape_ok(poll_token) {
        return Ok((State::Expired, None));
    }
    let row: Option<(Uuid, String, bool, bool)> = sqlx::query_as(
        "select id,state,expires_at<=now(),
                coalesce(approved_at<=now()-$2*interval '1 second',false)
         from owner_qr_logins where poll_sha256=$1",
    )
    .bind(digest(Secret::QrPoll, poll_token))
    .bind(DELIVERY_SECS as f64)
    .fetch_optional(pool)
    .await?;
    let Some((id, state, expired, stale)) = row else {
        return Ok((State::Expired, None));
    };
    Ok(match state.as_str() {
        "approved" if !stale => (State::Approved, Some(id)),
        "denied" => (State::Denied, None),
        "pending" if !expired => (State::Pending, None),
        "scanned" if !expired => (State::Scanned, None),
        _ => (State::Expired, None),
    })
}

#[derive(sqlx::FromRow)]
struct Approved {
    approved_by: Uuid,
    ip: Option<String>,
    user_agent: Option<String>,
    client_hints: Option<Value>,
    location: Option<Value>,
}

/// Выдача одобренного входа: ровно один опрос переводит строку в `delivered` и создаёт
/// сеанс (`qr`, `approved_by`, несвежий) в той же транзакции. Подтвердивший сеанс должен
/// быть жив: блокировка `owner_sessions_limit` упорядочивает выдачу с каскадным отзывом.
/// Выдачи нет, если у подтвердившего уже 5 живых таких сеансов или у владельца 50 живых
/// сеансов: вход по QR никого не вытесняет.
pub async fn deliver(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Option<(Created, ClientInfo)>, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('owner_sessions_limit'))")
        .execute(&mut *tx)
        .await?;
    let row: Option<Approved> = sqlx::query_as(
        "update owner_qr_logins q set state='delivered'
             where id=$1 and owner_id=$2 and state='approved'
               and approved_at>now()-$3*interval '1 second'
               and exists(select 1 from owner_sessions s
                          where s.id=q.approved_by and s.revoked_at is null)
               and (select count(*) from owner_sessions s
                    where s.approved_by=q.approved_by and s.revoked_at is null) < $4
               and (select count(*) from owner_sessions s
                    where s.owner_id=$2 and s.revoked_at is null) < $5
             returning approved_by,host(ip) as ip,user_agent,client_hints,location",
    )
    .bind(id)
    .bind(owner_id)
    .bind(DELIVERY_SECS as f64)
    .bind(MAX_APPROVED_PER_SESSION)
    .bind(sessions::MAX_LIVE)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(Approved {
        approved_by,
        ip,
        user_agent,
        client_hints,
        location,
    }) = row
    else {
        return Ok(None);
    };
    let client = ClientInfo {
        ip: ip.and_then(|ip| ip.parse::<IpAddr>().ok()),
        user_agent,
        client_hints,
        location: location.and_then(|value| serde_json::from_value::<Location>(value).ok()),
    };
    let created =
        sessions::create_in_tx(&mut tx, owner_id, Method::Qr, Some(approved_by), &client).await?;
    sqlx::query("update owner_qr_logins set session_id=$2 where id=$1")
        .bind(id)
        .bind(created.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if !created.limited.is_empty() {
        super::notify_changed(pool, "session_revoked").await;
    }
    super::notify_changed(pool, "session_changed").await;
    Ok(Some((created, client)))
}

/// Запрос на вход для карточки подтверждения. Всё, кроме адреса и возраста, заявлено клиентом.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Request {
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub client_hints: Option<Value>,
    pub location: Option<Value>,
}

/// Устройство с сеансом открыло код: живой запрос переходит в `scanned`.
pub async fn inspect(
    pool: &PgPool,
    owner_id: Uuid,
    secret: &str,
) -> Result<Option<Request>, sqlx::Error> {
    if !token_shape_ok(secret) {
        return Ok(None);
    }
    let found: Option<Request> = sqlx::query_as(
        "update owner_qr_logins set state='scanned'
         where secret_sha256=$1 and owner_id=$2 and state in ('pending','scanned')
           and expires_at>now()
         returning created_at,expires_at,host(ip) as ip,user_agent,client_hints,location",
    )
    .bind(digest(Secret::QrSecret, secret))
    .bind(owner_id)
    .fetch_optional(pool)
    .await?;
    if found.is_some() {
        super::notify_changed(pool, BUS_KIND).await;
    }
    Ok(found)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Done,
    /// Кода нет, он истёк или уже решён.
    Gone,
    /// У подтверждающего уже `MAX_APPROVED_PER_SESSION` живых или ждущих выдачи входов.
    Exhausted,
}

/// Одобрить (`approver`) или отклонить живой запрос.
pub async fn decide(
    pool: &PgPool,
    owner_id: Uuid,
    secret: &str,
    approver: Option<Uuid>,
) -> Result<Decision, sqlx::Error> {
    if !token_shape_ok(secret) {
        return Ok(Decision::Gone);
    }
    if let Some(approver) = approver {
        let approved: i64 = sqlx::query_scalar(
            "select (select count(*) from owner_sessions
                     where approved_by=$1 and revoked_at is null)
                  + (select count(*) from owner_qr_logins
                     where approved_by=$1 and state='approved'
                       and approved_at>now()-$2*interval '1 second')",
        )
        .bind(approver)
        .bind(DELIVERY_SECS as f64)
        .fetch_one(pool)
        .await?;
        if approved >= MAX_APPROVED_PER_SESSION {
            return Ok(Decision::Exhausted);
        }
    }
    let decided = sqlx::query(
        "update owner_qr_logins set
           state=case when $3::uuid is null then 'denied' else 'approved' end,
           approved_by=$3,approved_at=case when $3::uuid is null then null else now() end
         where secret_sha256=$1 and owner_id=$2 and state in ('pending','scanned')
           and expires_at>now()",
    )
    .bind(digest(Secret::QrSecret, secret))
    .bind(owner_id)
    .bind(approver)
    .execute(pool)
    .await?
    .rows_affected()
        > 0;
    if !decided {
        return Ok(Decision::Gone);
    }
    super::notify_changed(pool, BUS_KIND).await;
    Ok(Decision::Done)
}
