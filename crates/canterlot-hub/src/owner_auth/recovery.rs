//! Ссылка восстановления (она же первый вход): выпускается командой на хосте хаба,
//! хранится хешем, одна на владельца, новая заменяет прежнюю, используется один раз.

use sqlx::PgPool;
use uuid::Uuid;

use super::{Secret, digest, generate_token, token_shape_ok};

pub const DEFAULT_MINUTES: u32 = 10;
pub const MAX_MINUTES: u32 = 60;

/// Новый токен на `minutes` минут; прежний неиспользованный перестаёт действовать.
pub async fn issue(pool: &PgPool, owner_id: Uuid, minutes: u32) -> Result<String, sqlx::Error> {
    let token = generate_token();
    sqlx::query(
        "insert into owner_recovery_tokens(owner_id,token_sha256,expires_at)
         values($1,$2,now()+$3*interval '1 minute')
         on conflict(owner_id) do update set token_sha256=excluded.token_sha256,
           created_at=clock_timestamp(),expires_at=excluded.expires_at",
    )
    .bind(owner_id)
    .bind(digest(Secret::Recovery, &token))
    .bind(f64::from(minutes.clamp(1, MAX_MINUTES)))
    .execute(pool)
    .await?;
    Ok(token)
}

/// Погашает токен: владелец, если токен известен и не истёк. Повтор того же токена пуст.
pub async fn redeem(pool: &PgPool, token: &str) -> Result<Option<Uuid>, sqlx::Error> {
    if !token_shape_ok(token) {
        return Ok(None);
    }
    sqlx::query_scalar(
        "delete from owner_recovery_tokens where token_sha256=$1 and expires_at>now()
         returning owner_id",
    )
    .bind(digest(Secret::Recovery, token))
    .fetch_optional(pool)
    .await
}
