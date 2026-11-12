//! Сеансы браузера: создание, проверка по хешу токена без кэша, отзыв с каскадом,
//! надгробия, известные устройства и уборка.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{
    Method, RevokeReason, Secret, digest, generate_token,
    geo::{Geo, Location},
    token_shape_ok,
};

/// Не больше 50 живых сеансов: 51-й вытесняет давнее всех виденный. Вход по QR никого
/// не вытесняет: при 50 живых его выдача отклоняется (`qr::deliver`).
pub const MAX_LIVE: i64 = 50;
pub const DEFAULT_IDLE_DAYS: i32 = 90;
pub const IDLE_CHOICES: [i32; 5] = [7, 30, 90, 180, 365];
/// Свежесть после доказательства пароля или ключа.
pub const FRESH_MINUTES: i64 = 10;
pub const USER_AGENT_BYTES: usize = 512;
pub const CLIENT_HINTS_BYTES: usize = 2048;
pub const NAME_CHARS: usize = 64;

/// Данные клиента, которые запоминает сеанс; всё только для показа.
#[derive(Debug, Clone, Default)]
pub struct ClientInfo {
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub client_hints: Option<Value>,
    /// Место по адресу: ищется при первой записи адреса и хранится на строке.
    pub location: Option<Location>,
}

impl ClientInfo {
    /// Данные клиента с местом, найденным по его адресу.
    pub fn located(
        ip: Option<IpAddr>,
        user_agent: Option<String>,
        client_hints: Option<Value>,
        geo: &Geo,
    ) -> Self {
        Self {
            ip,
            user_agent,
            client_hints,
            location: ip.and_then(|ip| geo.lookup(ip)),
        }
    }

    pub(crate) fn location_json(&self) -> Option<Value> {
        self.location
            .as_ref()
            .and_then(|location| serde_json::to_value(location).ok())
    }

    /// Страна входа сразу считается известной: пуш о ней не нужен.
    fn countries(&self) -> Vec<String> {
        self.location
            .as_ref()
            .and_then(|location| location.country_code.clone())
            .into_iter()
            .collect()
    }

    pub(crate) fn ip_text(&self) -> Option<String> {
        self.ip.map(|ip| ip.to_canonical().to_string())
    }

    pub(crate) fn user_agent(&self) -> Option<String> {
        let agent = self.user_agent.as_deref()?.trim();
        if agent.is_empty() {
            return None;
        }
        let mut end = agent.len().min(USER_AGENT_BYTES);
        while !agent.is_char_boundary(end) {
            end -= 1;
        }
        Some(agent[..end].to_owned())
    }

    pub(crate) fn client_hints(&self) -> Option<Value> {
        let hints = self.client_hints.as_ref()?;
        (hints.is_object() && hints.to_string().len() <= CLIENT_HINTS_BYTES).then(|| hints.clone())
    }
}

#[derive(Debug, Clone)]
pub struct Created {
    pub id: Uuid,
    pub token: String,
    pub max_age_secs: i64,
    /// Сеансы, вытесненные пределом.
    pub limited: Vec<Uuid>,
}

/// Новый сеанс. Вход по QR рождается несвежим (`authenticated_at` пуст).
pub async fn create(
    pool: &PgPool,
    owner_id: Uuid,
    method: Method,
    approved_by: Option<Uuid>,
    client: &ClientInfo,
) -> Result<Created, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    let created = create_in_tx(&mut tx, owner_id, method, approved_by, client).await?;
    tx.commit().await?;
    if !created.limited.is_empty() {
        super::notify_changed(pool, "session_revoked").await;
    }
    super::notify_changed(pool, "session_changed").await;
    Ok(created)
}

/// Вход паролем, ключом или ссылкой: сеанс и известное устройство одной транзакцией.
/// Действующая cookie устройства `existing` сохраняется и переносится на новый сеанс,
/// иначе выдаётся новая. Возвращает сеанс и токен cookie устройства.
pub async fn sign_in(
    pool: &PgPool,
    owner_id: Uuid,
    method: Method,
    client: &ClientInfo,
    existing: Option<&str>,
) -> Result<(Created, String), sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    let created = create_in_tx(&mut tx, owner_id, method, None, client).await?;
    let device = remember_device(&mut tx, owner_id, existing, created.id).await?;
    tx.commit().await?;
    if !created.limited.is_empty() {
        super::notify_changed(pool, "session_revoked").await;
    }
    super::notify_changed(pool, "session_changed").await;
    Ok((created, device))
}

/// То же в чужой транзакции (часть 2: вход по QR в одной транзакции с доставкой).
/// Уведомления шины шлёт вызывающий после commit. Вход по QR не вытесняет сеансы:
/// место для него проверяет `qr::deliver`.
pub async fn create_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    method: Method,
    approved_by: Option<Uuid>,
    client: &ClientInfo,
) -> Result<Created, sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock(hashtext('owner_sessions_limit'))")
        .execute(&mut *conn)
        .await?;
    let limited: Vec<Uuid> = if method == Method::Qr {
        Vec::new()
    } else {
        sqlx::query_scalar(
            "update owner_sessions set revoked_at=now(),revoke_reason='limit'
             where id in (select id from owner_sessions
                          where owner_id=$1 and revoked_at is null
                          order by last_seen_at desc,id desc offset $2)
             returning id",
        )
        .bind(owner_id)
        .bind(MAX_LIVE - 1)
        .fetch_all(&mut *conn)
        .await?
    };
    let id = Uuid::now_v7();
    let token = generate_token();
    sqlx::query(
        "insert into owner_sessions(id,owner_id,token_sha256,method,approved_by,authenticated_at,
                                    created_ip,last_ip,user_agent,client_hints,
                                    created_location,last_location,notified_countries)
         values($1,$2,$3,$4,$5,case when $6 then now() end,$7::inet,$7::inet,$8,$9,
                $10,$10,$11)",
    )
    .bind(id)
    .bind(owner_id)
    .bind(digest(Secret::Session, &token))
    .bind(method.as_str())
    .bind(approved_by)
    .bind(method.proves_credential())
    .bind(client.ip_text())
    .bind(client.user_agent())
    .bind(client.client_hints())
    .bind(client.location_json())
    .bind(client.countries())
    .execute(&mut *conn)
    .await?;
    let days = idle_days_in(&mut *conn, owner_id).await?;
    Ok(Created {
        id,
        token,
        max_age_secs: i64::from(days) * 86400,
        limited,
    })
}

/// Живой сеанс запроса.
#[derive(Debug, Clone)]
pub struct Live {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub fresh: bool,
    pub fresh_until: Option<DateTime<Utc>>,
    /// `last_seen_at` записан этим запросом: cookie продлевается.
    pub touched: bool,
    pub max_age_secs: i64,
}

#[derive(Debug, Clone)]
pub enum Lookup {
    Live(Live),
    Revoked(RevokeReason),
    Expired,
    Unknown,
}

#[derive(sqlx::FromRow)]
struct Found {
    id: Uuid,
    owner_id: Uuid,
    fresh: bool,
    fresh_until: Option<DateTime<Utc>>,
    revoke_reason: Option<String>,
    idle: bool,
    stale: bool,
    moved: bool,
    idle_days: i32,
}

/// Проверка токена: живой сеанс, отозванный (в том числе надгробие), истёкший по
/// бездействию или неизвестный. `last_seen_at` и адрес пишутся не чаще раза в минуту
/// либо сразу при смене адреса.
pub async fn authenticate(
    pool: &PgPool,
    token: &str,
    client_ip: Option<IpAddr>,
    geo: &Geo,
) -> Result<Lookup, sqlx::Error> {
    if !token_shape_ok(token) {
        return Ok(Lookup::Unknown);
    }
    let hash = digest(Secret::Session, token);
    let ip = client_ip.map(|ip| ip.to_canonical().to_string());
    let found: Option<Found> = sqlx::query_as(
        "select s.id,s.owner_id,s.revoke_reason,
                coalesce(s.authenticated_at > now()-$3*interval '1 minute',false) as fresh,
                s.authenticated_at+$3*interval '1 minute' as fresh_until,
                s.last_seen_at < now()-coalesce(st.idle_timeout_days,$4)*interval '1 day' as idle,
                (s.last_seen_at < now()-interval '60 seconds'
                 or ($2::inet is not null and s.last_ip is distinct from $2::inet)) as stale,
                ($2::inet is not null and s.last_ip is distinct from $2::inet) as moved,
                coalesce(st.idle_timeout_days,$4) as idle_days
         from owner_sessions s left join owner_auth_settings st on st.owner_id=s.owner_id
         where s.token_sha256=$1",
    )
    .bind(&hash)
    .bind(&ip)
    .bind(FRESH_MINUTES as f64)
    .bind(DEFAULT_IDLE_DAYS)
    .fetch_optional(pool)
    .await?;
    let Some(found) = found else {
        let tombstone: Option<String> = sqlx::query_scalar(
            "select revoke_reason from owner_session_tombstones where token_sha256=$1",
        )
        .bind(&hash)
        .fetch_optional(pool)
        .await?;
        return Ok(match tombstone.as_deref().and_then(RevokeReason::parse) {
            Some(RevokeReason::Idle) => Lookup::Expired,
            Some(reason) => Lookup::Revoked(reason),
            None => Lookup::Unknown,
        });
    };
    if let Some(reason) = found.revoke_reason.as_deref() {
        return Ok(match RevokeReason::parse(reason) {
            Some(RevokeReason::Idle) => Lookup::Expired,
            Some(reason) => Lookup::Revoked(reason),
            None => Lookup::Unknown,
        });
    }
    if found.idle {
        revoke(pool, found.id, RevokeReason::Idle, None).await?;
        return Ok(Lookup::Expired);
    }
    let mut touched = false;
    if found.stale {
        // Новый адрес: место ищется один раз и кэшируется на строке.
        let location = client_ip.filter(|_| found.moved).map(|ip| geo.lookup(ip));
        let row: Option<(Option<String>, Option<String>, Option<Value>)> = sqlx::query_as(
            "update owner_sessions set last_seen_at=now(),last_ip=coalesce($2::inet,last_ip),
               last_location=case when $3 then $4 else last_location end
             where id=$1 and revoked_at is null
             returning name,user_agent,client_hints",
        )
        .bind(found.id)
        .bind(&ip)
        .bind(location.is_some())
        .bind(
            location
                .as_ref()
                .and_then(|location| serde_json::to_value(location).ok()),
        )
        .fetch_optional(pool)
        .await?;
        touched = row.is_some();
        if let (Some((name, agent, hints)), Some(Some(now)), Some(ip)) = (row, location, client_ip)
        {
            let title = name
                .or_else(|| super::label(agent.as_deref(), hints.as_ref()))
                .unwrap_or_else(|| "неизвестное устройство".into());
            new_country(pool, found.id, &now, ip, &title).await?;
        }
    }
    Ok(Lookup::Live(Live {
        id: found.id,
        owner_id: found.owner_id,
        fresh: found.fresh,
        fresh_until: found.fresh_until,
        touched,
        max_age_secs: i64::from(found.idle_days) * 86400,
    }))
}

/// Пуш «используется из новой страны»: о каждой стране не больше одного раза на сеанс.
/// Первая известная страна сеанса (обычно страна входа) — точка отсчёта без пуша.
/// Ошибка пуша не мешает запросу.
async fn new_country(
    pool: &PgPool,
    id: Uuid,
    now: &Location,
    ip: IpAddr,
    title: &str,
) -> Result<(), sqlx::Error> {
    let Some(code) = now.country_code.as_deref() else {
        return Ok(());
    };
    let changed: Option<bool> = sqlx::query_scalar(
        "update owner_sessions set notified_countries=array_append(notified_countries,$2)
         where id=$1 and not ($2 = any(notified_countries))
         returning cardinality(notified_countries) > 1",
    )
    .bind(id)
    .bind(code)
    .fetch_optional(pool)
    .await?;
    if changed != Some(true) {
        return Ok(());
    }
    let text = super::country_text(title, now, ip);
    if let Err(error) = crate::notify::on_session_country(pool, id, code, &text).await {
        tracing::warn!(kind = "sign_in_push_failed", error = %crate::dberr::describe(&error), "country push not queued");
    }
    Ok(())
}

/// Перепроверка сеанса открытого WebSocket: жив ли он сейчас (бездействие тоже отзывает).
pub async fn is_live(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let row: Option<(bool, bool)> = sqlx::query_as(
        "select s.revoked_at is null,
                s.last_seen_at < now()-coalesce(st.idle_timeout_days,$2)*interval '1 day'
         from owner_sessions s left join owner_auth_settings st on st.owner_id=s.owner_id
         where s.id=$1",
    )
    .bind(id)
    .bind(DEFAULT_IDLE_DAYS)
    .fetch_optional(pool)
    .await?;
    match row {
        Some((true, false)) => Ok(true),
        Some((true, true)) => {
            revoke(pool, id, RevokeReason::Idle, None).await?;
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// Отмечает свежее доказательство пароля или ключа доступа.
pub async fn mark_fresh(pool: &PgPool, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update owner_sessions set authenticated_at=now() where id=$1 and revoked_at is null",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Отзывает сеанс; при каскадной причине и сеансы, вход которых он подтвердил по QR,
/// транзитивно. `spare` не отзывается и не служит звеном каскада: это действующий сеанс,
/// только что доказавший пароль или ключ. Возвращает отозванные идентификаторы.
pub async fn revoke(
    pool: &PgPool,
    id: Uuid,
    reason: RevokeReason,
    spare: Option<Uuid>,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    serialize_with_delivery(&mut tx).await?;
    let revoked: Vec<Uuid> = sqlx::query_scalar(
        "with recursive tree(id) as (
           select id from owner_sessions where id=$1 and revoked_at is null
           union
           select s.id from owner_sessions s join tree t on s.approved_by=t.id
           where $3 and s.revoked_at is null and s.id is distinct from $4
         )
         update owner_sessions set revoked_at=now(),
           revoke_reason=case when id=$1 then $2 else 'cascade' end
         where id in (select id from tree) and revoked_at is null
         returning id",
    )
    .bind(id)
    .bind(reason.as_str())
    .bind(reason.cascades())
    .bind(spare)
    .fetch_all(&mut *tx)
    .await?;
    forget_devices_of(&mut tx, reason, &revoked).await?;
    tx.commit().await?;
    if !revoked.is_empty() {
        super::notify_changed(pool, "session_revoked").await;
    }
    Ok(revoked)
}

/// Отзывает все живые сеансы владельца, кроме `keep`, с каскадом, щадящим `keep`.
pub async fn revoke_all_except(
    pool: &PgPool,
    owner_id: Uuid,
    keep: Option<Uuid>,
    reason: RevokeReason,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    serialize_with_delivery(&mut tx).await?;
    let revoked: Vec<Uuid> = sqlx::query_scalar(
        "update owner_sessions set revoked_at=now(),revoke_reason=$3
         where owner_id=$1 and revoked_at is null and id is distinct from $2
         returning id",
    )
    .bind(owner_id)
    .bind(keep)
    .bind(reason.as_str())
    .fetch_all(&mut *tx)
    .await?;
    forget_devices_of(&mut tx, reason, &revoked).await?;
    tx.commit().await?;
    if !revoked.is_empty() {
        super::notify_changed(pool, "session_revoked").await;
    }
    Ok(revoked)
}

/// Устройства, чей последний вход дал отозванные сеансы, забываются при отзыве владельцем,
/// сервером, сменой пароля и каскадом; выход, бездействие и предел их не трогают.
async fn forget_devices_of(
    conn: &mut PgConnection,
    reason: RevokeReason,
    revoked: &[Uuid],
) -> Result<(), sqlx::Error> {
    if reason.cascades() && !revoked.is_empty() {
        sqlx::query("delete from owner_devices where session_id = any($1)")
            .bind(revoked)
            .execute(conn)
            .await?;
    }
    Ok(())
}

/// Забыть все известные устройства владельца: смена пароля и «завершить все сеансы».
pub async fn forget_all_devices(pool: &PgPool, owner_id: Uuid) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("delete from owner_devices where owner_id=$1")
        .bind(owner_id)
        .execute(pool)
        .await?
        .rows_affected())
}

/// Отзыв и выдача входа по QR (`qr::deliver`, `create_in_tx`) берут одну блокировку:
/// сеанс, выданный по одобрению отзываемого, либо уже виден каскаду, либо не выдаётся,
/// потому что подтвердивший уже отозван.
async fn serialize_with_delivery(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock(hashtext('owner_sessions_limit'))")
        .execute(conn)
        .await?;
    Ok(())
}

/// Живые сеансы, которые уйдут каскадом вместе с `id` (без него самого).
pub async fn cascade_of(pool: &PgPool, id: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "with recursive tree(id) as (
           select s.id from owner_sessions s where s.approved_by=$1 and s.revoked_at is null
           union
           select s.id from owner_sessions s join tree t on s.approved_by=t.id
           where s.revoked_at is null
         )
         select id from tree",
    )
    .bind(id)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionRow {
    pub id: Uuid,
    pub name: Option<String>,
    pub user_agent: Option<String>,
    pub client_hints: Option<Value>,
    pub method: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub created_ip: Option<String>,
    pub last_ip: Option<String>,
    pub created_location: Option<Value>,
    pub last_location: Option<Value>,
    pub approved_by: Option<Uuid>,
    pub approved: Vec<Uuid>,
}

const ROW_COLUMNS: &str = "s.id,s.name,s.user_agent,s.client_hints,s.method,s.created_at,
    s.last_seen_at,host(s.created_ip) as created_ip,host(s.last_ip) as last_ip,
    s.created_location,s.last_location,s.approved_by,
    array(select c.id from owner_sessions c where c.approved_by=s.id and c.revoked_at is null
          order by c.created_at) as approved";

/// Живые сеансы: текущий первым, затем по последней активности.
pub async fn list(
    pool: &PgPool,
    owner_id: Uuid,
    current: Option<Uuid>,
) -> Result<Vec<SessionRow>, sqlx::Error> {
    let sql = format!(
        "select {ROW_COLUMNS} from owner_sessions s
         where s.owner_id=$1 and s.revoked_at is null
         order by s.id is not distinct from $2 desc,s.last_seen_at desc,s.id desc"
    );
    sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(owner_id)
        .bind(current)
        .fetch_all(pool)
        .await
}

pub async fn get(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Option<SessionRow>, sqlx::Error> {
    let sql = format!(
        "select {ROW_COLUMNS} from owner_sessions s
         where s.owner_id=$1 and s.id=$2 and s.revoked_at is null"
    );
    sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(owner_id)
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Живой ли сеанс владельца с таким id.
pub async fn live_exists(pool: &PgPool, owner_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(select 1 from owner_sessions where owner_id=$1 and id=$2 and revoked_at is null)",
    )
    .bind(owner_id)
    .bind(id)
    .fetch_one(pool)
    .await
}

/// Имя от владельца; пустое снимает имя. `false`, если живого сеанса нет.
pub async fn rename(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    name: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let updated = sqlx::query(
        "update owner_sessions set name=$3 where owner_id=$1 and id=$2 and revoked_at is null",
    )
    .bind(owner_id)
    .bind(id)
    .bind(name)
    .execute(pool)
    .await?
    .rows_affected()
        > 0;
    if updated {
        super::notify_changed(pool, "session_changed").await;
    }
    Ok(updated)
}

async fn idle_days_in(conn: &mut PgConnection, owner_id: Uuid) -> Result<i32, sqlx::Error> {
    let days: Option<i32> =
        sqlx::query_scalar("select idle_timeout_days from owner_auth_settings where owner_id=$1")
            .bind(owner_id)
            .fetch_optional(conn)
            .await?;
    Ok(days.unwrap_or(DEFAULT_IDLE_DAYS))
}

pub async fn idle_days(pool: &PgPool, owner_id: Uuid) -> Result<i32, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    idle_days_in(&mut conn, owner_id).await
}

pub async fn set_idle_days(pool: &PgPool, owner_id: Uuid, days: i32) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into owner_auth_settings(owner_id,idle_timeout_days) values($1,$2)
         on conflict(owner_id) do update set idle_timeout_days=excluded.idle_timeout_days,
           updated_at=clock_timestamp()",
    )
    .bind(owner_id)
    .bind(days)
    .execute(pool)
    .await?;
    super::notify_changed(pool, "session_changed").await;
    Ok(())
}

/// Cookie известного устройства действительна: запись есть, отметка использования обновлена.
pub async fn device_known(pool: &PgPool, owner_id: Uuid, token: &str) -> Result<bool, sqlx::Error> {
    if !token_shape_ok(token) {
        return Ok(false);
    }
    Ok(sqlx::query(
        "update owner_devices set last_used_at=now() where token_sha256=$1 and owner_id=$2",
    )
    .bind(digest(Secret::Device, token))
    .bind(owner_id)
    .execute(pool)
    .await?
    .rows_affected()
        > 0)
}

/// Ключ корзины известного устройства: по хешу, сам токен в базу не попадает.
pub fn device_bucket(token: &str) -> String {
    let hash = digest(Secret::Device, token);
    let hex: String = hash[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("device:{hex}")
}

/// Запоминает устройство входа `session`: действующая cookie сохраняется и переносится на
/// этот сеанс, иначе новая.
async fn remember_device(
    conn: &mut PgConnection,
    owner_id: Uuid,
    existing: Option<&str>,
    session: Uuid,
) -> Result<String, sqlx::Error> {
    if let Some(token) = existing.filter(|token| token_shape_ok(token)) {
        let kept = sqlx::query(
            "update owner_devices set last_used_at=now(),session_id=$3
             where token_sha256=$1 and owner_id=$2",
        )
        .bind(digest(Secret::Device, token))
        .bind(owner_id)
        .bind(session)
        .execute(&mut *conn)
        .await?
        .rows_affected()
            > 0;
        if kept {
            return Ok(token.to_owned());
        }
    }
    let token = generate_token();
    sqlx::query("insert into owner_devices(token_sha256,owner_id,session_id) values($1,$2,$3)")
        .bind(digest(Secret::Device, &token))
        .bind(owner_id)
        .bind(session)
        .execute(&mut *conn)
        .await?;
    Ok(token)
}

/// Уборка: полные отозванные строки старше 30 дней уходят в надгробия, надгробия живут
/// 400 дней; журнал неудач сутки; корзины без неудач и замка сутки; просроченные ссылки
/// восстановления, состояния обрядов WebAuthn и коды QR; устройства, не встречавшиеся
/// 400 дней.
pub async fn purge(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    let mut removed = 0;
    sqlx::query(
        "insert into owner_session_tombstones(token_sha256,revoked_at,revoke_reason)
         select token_sha256,revoked_at,revoke_reason from owner_sessions
         where revoked_at < now()-interval '30 days'
         on conflict(token_sha256) do nothing",
    )
    .execute(&mut *tx)
    .await?;
    for sql in [
        "delete from owner_sessions where revoked_at < now()-interval '30 days'",
        "delete from owner_session_tombstones where revoked_at < now()-interval '400 days'",
        "delete from auth_failures where at < now()-interval '1 day'",
        "delete from auth_buckets where coalesce(last_failure_at,'-infinity') < now()-interval '1 day'
           and coalesce(locked_until,'-infinity') < now()",
        "delete from owner_recovery_tokens where expires_at < now()",
        "delete from owner_devices where last_used_at < now()-interval '400 days'",
        "delete from owner_webauthn_challenges where expires_at < now()",
        // Строки QR ещё минуту после срока считают запуски корзины.
        "delete from owner_qr_logins where expires_at < now()-interval '10 minutes'",
    ] {
        removed += sqlx::query(sql).execute(&mut *tx).await?.rows_affected();
    }
    tx.commit().await?;
    Ok(removed)
}
