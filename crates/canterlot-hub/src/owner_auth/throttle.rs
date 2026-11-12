//! Защита от перебора. Корзины живут в базе: переживают перезапуск и общие для процессов.
//!
//! Порядок попытки: разрешение argon2, затем атомарное резервирование (`failures + 1`
//! одним оператором и только у незапертой корзины), проверка, при успехе корзина очищается.
//! Пять неудач за 15 минут запирают корзину на минуту, каждая следующая удваивает срок
//! до часа; 15 минут без неудач и без замка сбрасывают счёт. Больше 30 неудач за 10 минут
//! запирают вход по паролю для всех запросов без cookie известного устройства.

use std::time::Duration;

use sqlx::PgPool;

pub const GLOBAL: &str = "global";
const LOCK_AFTER: i32 = 5;
const GLOBAL_LIMIT: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reservation {
    Granted,
    Refused { retry_after: Duration },
}

/// Итог записанной неудачи: только что включилась общая блокировка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    pub global_locked: bool,
}

/// Резервирует попытку в корзине `bucket`. `global`: попытку держит и общая блокировка
/// (вход по паролю без cookie известного устройства).
pub async fn reserve(
    pool: &PgPool,
    bucket: &str,
    global: bool,
) -> Result<Reservation, sqlx::Error> {
    let reserved: Option<i32> = sqlx::query_scalar(
        "insert into auth_buckets as b(key,failures,last_failure_at,locked_until)
         select $1,1,now(),null
         where not ($2 and exists(select 1 from auth_buckets
                                  where key='global' and locked_until>now()))
         on conflict(key) do update set
           failures = case when greatest(b.last_failure_at,b.locked_until) < now()-interval '15 minutes'
                           then 1 else b.failures+1 end,
           last_failure_at = now(),
           locked_until = case
             when (case when greatest(b.last_failure_at,b.locked_until) < now()-interval '15 minutes'
                        then 1 else b.failures+1 end) >= $3
             then now() + least(60*power(2, (case when greatest(b.last_failure_at,b.locked_until)
                                                     < now()-interval '15 minutes'
                                                   then 1 else b.failures+1 end) - $3), 3600)
                        * interval '1 second'
             else null end
         where b.locked_until is null or b.locked_until<=now()
         returning failures",
    )
    .bind(bucket)
    .bind(global)
    .bind(LOCK_AFTER)
    .fetch_optional(pool)
    .await?;
    if reserved.is_some() {
        return Ok(Reservation::Granted);
    }
    let seconds: Option<f64> = sqlx::query_scalar(
        "select extract(epoch from max(locked_until)-now())::float8 from auth_buckets
         where (key=$1 or ($2 and key='global')) and locked_until>now()",
    )
    .bind(bucket)
    .bind(global)
    .fetch_one(pool)
    .await?;
    let seconds = seconds.unwrap_or(1.0).max(1.0).ceil();
    Ok(Reservation::Refused {
        retry_after: Duration::from_secs_f64(seconds),
    })
}

/// Верный пароль или токен: резерв и счёт корзины снимаются.
pub async fn succeeded(pool: &PgPool, bucket: &str) -> Result<(), sqlx::Error> {
    sqlx::query("delete from auth_buckets where key=$1 and key<>'global'")
        .bind(bucket)
        .execute(pool)
        .await?;
    Ok(())
}

/// Неудача подтверждена: она уже в счёте корзины, остаётся журнал и общая блокировка.
pub async fn failed(pool: &PgPool, bucket: &str) -> Result<Failure, sqlx::Error> {
    sqlx::query("insert into auth_failures(bucket) values($1)")
        .bind(bucket)
        .execute(pool)
        .await?;
    let locked: Option<i32> = sqlx::query_scalar(
        "insert into auth_buckets as b(key,failures,locked_until)
         select 'global',0,now()+interval '10 minutes'
         where (select count(*) from auth_failures where at>now()-interval '10 minutes') > $1
         on conflict(key) do update set locked_until=excluded.locked_until
           where b.locked_until is null or b.locked_until<=now()
         returning 1",
    )
    .bind(GLOBAL_LIMIT)
    .fetch_optional(pool)
    .await?;
    if locked.is_some() {
        tracing::warn!(
            kind = "sign_in_global_lock",
            "password sign-in locked for 10 minutes"
        );
    }
    Ok(Failure {
        global_locked: locked.is_some(),
    })
}

/// Сводка неудач за последний час: число и число разных корзин.
pub async fn last_hour(pool: &PgPool) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as(
        "select count(*),count(distinct bucket) from auth_failures
         where at>now()-interval '1 hour'",
    )
    .fetch_one(pool)
    .await
}
