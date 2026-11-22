//! Деньги облака: настройки владельца, остаток бюджета, пороговые оповещения и помощь
//! автоматическому режиму. Все суммы — целые копейки, месяц — календарный по Москве.
//!
//! Настройки живут в единственной строке `cloud_budget` и меняются только владельцем.
//! Ревизия защищает карточку от устаревшего значения, если владелец правил бюджет, пока
//! карточка была открыта.

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use super::windows::{self, Pricing};
use crate::config::CloudConfig;

/// Непрерывная недоступность/пауза раннера, после которой автоматический режим может стартовать.
pub const UNAVAILABLE_FOR: TimeDelta = TimeDelta::minutes(10);
/// Порог ожидающей работы автоматического режима и карточки хаба.
pub const MIN_WAITING_MINUTES: i64 = 60;
/// Жёсткий предел автоматического окна.
pub const AUTO_MAX_CAP_MINUTES: u32 = 240;

/// Профиль в настройках бюджета: цена часа из провайдера на момент чтения.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProfileView {
    pub configurator_id: i64,
    pub cpu: u32,
    pub ram_gb: u32,
    pub disk_gb: u32,
    pub hourly_kopecks: i64,
    pub availability_zone: String,
}

impl From<&windows::PricedProfile> for ProfileView {
    fn from(profile: &windows::PricedProfile) -> Self {
        Self {
            configurator_id: profile.configurator_id,
            cpu: profile.cpu,
            ram_gb: profile.ram_gb,
            disk_gb: profile.disk_gb,
            hourly_kopecks: profile.hourly_kopecks,
            availability_zone: profile.availability_zone.clone(),
        }
    }
}

/// Всё, что показывает экран бюджета владельца и инструмент `runners_list`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BudgetView {
    /// Месяц, за который посчитан расход (`YYYY-MM`, Европа/Москва).
    pub month: String,
    pub monthly_budget_kopecks: i64,
    /// Законченные окна по итоговой цене плюс живые по резерву.
    pub spent_kopecks: i64,
    pub remaining_kopecks: i64,
    pub auto_mode: bool,
    pub revision: i64,
    pub provider_block_code: Option<String>,
    pub provider_block_at: Option<DateTime<Utc>>,
    /// Профили из конфигурации с текущей ценой часа (только чтение).
    pub profiles: Vec<ProfileView>,
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// Владелец изменил настройку, пока карточка была открыта.
    #[error("stale_revision")]
    Stale,
    #[error("invalid_request")]
    Invalid(&'static str),
    #[error("db_error")]
    Db(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub monthly_budget_kopecks: i64,
    pub auto_mode: bool,
    pub base_revision: i64,
}

/// Изменение настроек владельцем под ревизией: устаревшая ревизия ничего не меняет.
pub async fn update_in_tx(conn: &mut PgConnection, update: &Update) -> Result<i64, UpdateError> {
    if update.monthly_budget_kopecks < 0 {
        return Err(UpdateError::Invalid("monthly_budget_kopecks"));
    }
    if update.base_revision < 0 {
        return Err(UpdateError::Invalid("base_revision"));
    }
    let revision: Option<i64> = sqlx::query_scalar(
        "update cloud_budget
            set monthly_budget_kopecks = $1, auto_mode = $2, revision = revision + 1
          where singleton and revision = $3
         returning revision",
    )
    .bind(update.monthly_budget_kopecks)
    .bind(update.auto_mode)
    .bind(update.base_revision)
    .fetch_optional(&mut *conn)
    .await?;
    revision.ok_or(UpdateError::Stale)
}

/// Чтение настроек и расхода для ответа владельцу. Цены профилей передаёт вызывающая
/// сторона: сетевой вызов провайдера не должен идти из транзакции.
pub async fn view(
    conn: &mut PgConnection,
    pricing: Option<&Pricing>,
) -> Result<BudgetView, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Row {
        monthly_budget_kopecks: i64,
        auto_mode: bool,
        revision: i64,
        provider_block_code: Option<String>,
        provider_block_at: Option<DateTime<Utc>>,
    }
    let row: Row = sqlx::query_as(
        "select monthly_budget_kopecks, auto_mode, revision, provider_block_code, provider_block_at
           from cloud_budget where singleton",
    )
    .fetch_one(&mut *conn)
    .await?;
    let month = windows::month_of(Utc::now());
    let spent = windows::spent_in_month(conn, &month).await?;
    let profiles = pricing.map_or_else(Vec::new, |pricing| {
        pricing.profiles.iter().map(ProfileView::from).collect()
    });
    Ok(BudgetView {
        month,
        monthly_budget_kopecks: row.monthly_budget_kopecks,
        spent_kopecks: spent,
        remaining_kopecks: row.monthly_budget_kopecks.saturating_sub(spent),
        auto_mode: row.auto_mode,
        revision: row.revision,
        provider_block_code: row.provider_block_code,
        provider_block_at: row.provider_block_at,
        profiles,
    })
}

/// Текущая ревизия и бюджет: для карточек и автоматики, без сетевых вызовов.
pub async fn settings(
    conn: &mut PgConnection,
) -> Result<(i64, bool, Option<String>), sqlx::Error> {
    sqlx::query_as(
        "select monthly_budget_kopecks, auto_mode, provider_block_code from cloud_budget where singleton",
    )
    .fetch_one(conn)
    .await
}

/// Остаток бюджета в текущем месяце.
pub async fn remaining_in_month(conn: &mut PgConnection) -> Result<i64, sqlx::Error> {
    let month = windows::month_of(Utc::now());
    let (budget, _, _): (i64, bool, Option<String>) = settings(conn).await?;
    Ok(budget.saturating_sub(windows::spent_in_month(conn, &month).await?))
}

/// Оповещения о 80 % и 100 % бюджета: каждое не чаще одного раза в месяц. Нулевой бюджет
/// не оповещает никогда. Возвращает количество поставленных в очередь пушей.
pub async fn maybe_alert(
    pool: &PgPool,
    now: DateTime<Utc>,
    budget_kopecks: i64,
) -> Result<usize, sqlx::Error> {
    if budget_kopecks <= 0 {
        return Ok(0);
    }
    let month = windows::month_of(now);
    let mut conn = pool.acquire().await?;
    let spent = windows::spent_in_month(&mut conn, &month).await?;
    let (mark_80, mark_100): (Option<String>, Option<String>) = sqlx::query_as(
        "select alert_80_month, alert_100_month from cloud_budget where singleton",
    )
    .fetch_one(&mut *conn)
    .await?;
    let mut sent = 0;
    // Две фиксированные колонки: динамический SQL здесь не нужен и не разрешён sqlx.
    for (percent, current, is_80) in [
        (80_i64, mark_80, true),
        (100_i64, mark_100, false),
    ] {
        if current.as_deref() == Some(month.as_str())
            || spent.saturating_mul(100) < budget_kopecks.saturating_mul(percent)
        {
            continue;
        }
        let changed = if is_80 {
            sqlx::query(
                "update cloud_budget set alert_80_month = $1
                  where singleton and coalesce(alert_80_month, '') <> $1",
            )
            .bind(&month)
            .execute(&mut *conn)
            .await?
            .rows_affected()
                > 0
        } else {
            sqlx::query(
                "update cloud_budget set alert_100_month = $1
                  where singleton and coalesce(alert_100_month, '') <> $1",
            )
            .bind(&month)
            .execute(&mut *conn)
            .await?
            .rows_affected()
                > 0
        };
        if changed {
            drop(conn);
            crate::notify::enqueue_cloud_alert(
                pool,
                &format!("cloud:budget:{percent}:{month}"),
                &serde_json::json!({
                    "code": format!("budget_{percent}"),
                    "month": month,
                    "percent": percent,
                    "spent_kopecks": spent,
                    "budget_kopecks": budget_kopecks,
                }),
            )
            .await?;
            sent += 1;
            conn = pool.acquire().await?;
        }
    }
    Ok(sent)
}

/// Отметить или сбросить непрерывную недоступность раннера. `true` значит, что он
/// недоступен или на паузе уже не меньше [`UNAVAILABLE_FOR`]. Свежая доступность сбрасывает
/// период одной командой, как требует правило автоматического режима.
pub async fn track_unavailable(
    conn: &mut PgConnection,
    machine: Uuid,
    unavailable: bool,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    if !unavailable {
        sqlx::query("delete from cloud_auto_unavailable where machine_id = $1")
            .bind(machine)
            .execute(conn)
            .await?;
        return Ok(false);
    }
    sqlx::query(
        "insert into cloud_auto_unavailable (machine_id, since) values ($1, $2)
         on conflict (machine_id) do nothing",
    )
    .bind(machine)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    let since: Option<DateTime<Utc>> =
        sqlx::query_scalar("select since from cloud_auto_unavailable where machine_id = $1")
            .bind(machine)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(since.is_some_and(|since| now - since >= UNAVAILABLE_FOR))
}

/// Кандидаты статичных раннеров, которые могли бы взять ожидающую облачную работу:
/// метки задачи — подмножество меток раннера, сам раннер не облачный.
pub async fn static_candidates(
    conn: &mut PgConnection,
    config: &CloudConfig,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let cloud: Option<Uuid> = sqlx::query_scalar(
        "select cloud_machine_id from cloud_budget where singleton",
    )
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query_scalar(
        "select distinct m.id from machines m
           join jobs j on j.state = 'waiting_runner' and j.labels <@ m.labels
          where m.role = 'runner' and m.owner_id = $1
            and ($2::uuid is null or m.id <> $2)
            and j.carrier_id = $3 and j.labels <@ $4
            and j.placement && array['runner','cloud']::text[]",
    )
    .bind(owner_of(conn, config.carrier_machine_id).await?)
    .bind(cloud)
    .bind(config.carrier_machine_id)
    .bind(&config.labels)
    .fetch_all(&mut *conn)
    .await
}

async fn owner_of(conn: &mut PgConnection, carrier: Uuid) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar("select owner_id from machines where id = $1")
        .bind(carrier)
        .fetch_one(conn)
        .await
}

/// Сумма минут ожидающей облачной работы. Метки и место проверяются так же, как при
/// размещении; работа других несущих не считается.
pub async fn waiting_cloud_minutes(
    conn: &mut PgConnection,
    config: &CloudConfig,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select coalesce(sum(expected_minutes), 0)::bigint from jobs
          where state = 'waiting_runner' and carrier_id = $1 and labels <@ $2
            and placement && array['runner','cloud']::text[]",
    )
    .bind(config.carrier_machine_id)
    .bind(&config.labels)
    .fetch_one(conn)
    .await
}

/// Живое окно уже есть?
pub async fn has_live_window(conn: &mut PgConnection) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("select exists(select 1 from cloud_windows where state = any($1))")
        .bind(windows::live_states())
        .fetch_one(conn)
        .await
}

/// Открытая карточка облачного окна уже есть?
pub async fn has_open_card(conn: &mut PgConnection) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(select 1 from requests
           where kind = 'cloud_window' and state in ('open', 'decision_saved'))",
    )
    .fetch_one(conn)
    .await
}

/// Признак эпизода ожидающей работы: карточка хаба выдаётся один раз, пока очередь не пуста.
pub async fn episode_active(conn: &mut PgConnection) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("select active from cloud_auto_episode where singleton")
        .fetch_one(conn)
        .await
}

pub async fn episode_start(conn: &mut PgConnection, card: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("update cloud_auto_episode set active = true, card_id = $1 where singleton")
        .bind(card)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn episode_end(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("update cloud_auto_episode set active = false, card_id = null where singleton")
        .execute(&mut *conn)
        .await?;
    // Очередь опустела: карточка хаба больше не актуальна. Иначе она блокировала бы
    // выдачу новой карточки при следующем наполнении очереди.
    sqlx::query(
        "update requests set state = 'closed_without_decision', updated_at = clock_timestamp()
          where kind = 'cloud_window' and state = 'open'
            and request_payload->>'source' = 'hub'",
    )
    .execute(conn)
    .await?;
    Ok(())
}
