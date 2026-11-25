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
use crate::machines::RunnerAvailability;

/// Непрерывная недоступность/пауза раннера, после которой автоматический режим может стартовать.
pub const UNAVAILABLE_FOR: TimeDelta = TimeDelta::minutes(10);
/// Порог ожидающей работы автоматического режима и карточки хаба.
pub const MIN_WAITING_MINUTES: i64 = 60;
/// Жёсткий предел автоматического окна.
pub const AUTO_MAX_CAP_MINUTES: u32 = 240;

/// Профиль в настройках бюджета: размеры машины и две ставки часа у провайдера на момент
/// чтения. Цена часа работающей машины — их сумма.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ProfileView {
    pub cores: u32,
    pub memory_gb: u32,
    pub disk_gb: u32,
    /// Машина с публичным адресом: платится, пока машина есть в каталоге.
    pub compute_kopecks_per_hour: i64,
    /// Диск: платится, пока есть диск.
    pub disk_kopecks_per_hour: i64,
}

impl From<&windows::PricedProfile> for ProfileView {
    fn from(profile: &windows::PricedProfile) -> Self {
        Self {
            cores: profile.cores,
            memory_gb: profile.memory_gb,
            disk_gb: profile.disk_gb,
            compute_kopecks_per_hour: profile.compute_kopecks_per_hour,
            disk_kopecks_per_hour: profile.disk_kopecks_per_hour,
        }
    }
}

/// Всё, что показывает экран бюджета владельца и инструмент `runners_list`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BudgetView {
    /// Месяц, за который посчитан расход (`YYYY-MM`, Европа/Москва).
    pub month: String,
    pub monthly_budget_kopecks: i64,
    /// Законченные окна по итоговой цене плюс живые по резерву, а если живое окно уже стоит
    /// дороже резерва, по набежавшей цене.
    pub spent_kopecks: i64,
    pub remaining_kopecks: i64,
    pub auto_mode: bool,
    pub revision: i64,
    pub provider_block_code: Option<String>,
    pub provider_block_at: Option<DateTime<Utc>>,
    /// Облако настроено на хабе: без этого настройки бюджета показывать нечего.
    pub configured: bool,
    /// Профили из конфигурации с текущими ставками часа (только чтение).
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
    configured: bool,
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
    let now = Utc::now();
    let month = windows::month_of(now);
    let spent = windows::spent_in_month(conn, &month, now).await?;
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
        configured,
        profiles,
    })
}

/// Текущая ревизия и бюджет: для карточек и автоматики, без сетевых вызовов.
pub async fn settings(conn: &mut PgConnection) -> Result<(i64, bool, Option<String>), sqlx::Error> {
    sqlx::query_as(
        "select monthly_budget_kopecks, auto_mode, provider_block_code from cloud_budget where singleton",
    )
    .fetch_one(conn)
    .await
}

/// Настройки под замком единственной строки бюджета: остаток и признак перерасхода
/// считаются по тому же снимку, в котором вставляется окно.
pub async fn lock_settings(
    conn: &mut PgConnection,
) -> Result<(i64, bool, Option<String>), sqlx::Error> {
    sqlx::query_as(
        "select monthly_budget_kopecks, auto_mode, provider_block_code from cloud_budget
          where singleton for update",
    )
    .fetch_one(conn)
    .await
}

/// Остаток бюджета в текущем месяце.
pub async fn remaining_in_month(conn: &mut PgConnection) -> Result<i64, sqlx::Error> {
    let now = Utc::now();
    let month = windows::month_of(now);
    let (budget, _, _): (i64, bool, Option<String>) = settings(conn).await?;
    Ok(budget.saturating_sub(windows::spent_in_month(conn, &month, now).await?))
}

/// Оповещения о 80 % и 100 % бюджета: каждое не чаще одного раза в месяц. Нулевой бюджет
/// не оповещает никогда. Метка месяца фиксируется в той же транзакции, что и строка очереди:
/// сбой пуша не съедает оповещение. Возвращает количество поставленных в очередь пушей.
pub async fn maybe_alert(
    pool: &PgPool,
    now: DateTime<Utc>,
    budget_kopecks: i64,
) -> Result<usize, sqlx::Error> {
    if budget_kopecks <= 0 {
        return Ok(0);
    }
    let month = windows::month_of(now);
    let mut sent = 0;
    // Две фиксированные колонки: динамический SQL здесь не нужен и не разрешён sqlx.
    for (percent, is_80) in [(80_i64, true), (100_i64, false)] {
        let month = month.clone();
        let marked = crate::db::in_tx(pool, move |conn| {
            Box::pin(async move {
                let (budget, mark_80, mark_100): (i64, Option<String>, Option<String>) =
                    sqlx::query_as(
                        "select monthly_budget_kopecks, alert_80_month, alert_100_month
                           from cloud_budget where singleton for update",
                    )
                    .fetch_one(&mut *conn)
                    .await?;
                if budget <= 0 {
                    return Ok::<_, sqlx::Error>(false);
                }
                let current = if is_80 { mark_80 } else { mark_100 };
                if current.as_deref() == Some(month.as_str()) {
                    return Ok(false);
                }
                let spent = windows::spent_in_month(&mut *conn, &month, now).await?;
                if spent.saturating_mul(100) < budget.saturating_mul(percent) {
                    return Ok(false);
                }
                let inserted = crate::notify::enqueue_cloud_alert_in_tx(
                    &mut *conn,
                    &format!("cloud:budget:{percent}:{month}"),
                    &serde_json::json!({
                        "code": format!("budget_{percent}"),
                        "month": month,
                        "percent": percent,
                        "spent_kopecks": spent,
                        "budget_kopecks": budget,
                    }),
                )
                .await?;
                if !inserted {
                    return Ok(false);
                }
                if is_80 {
                    sqlx::query("update cloud_budget set alert_80_month = $1 where singleton")
                        .bind(&month)
                        .execute(&mut *conn)
                        .await?;
                } else {
                    sqlx::query("update cloud_budget set alert_100_month = $1 where singleton")
                        .bind(&month)
                        .execute(&mut *conn)
                        .await?;
                }
                Ok(true)
            })
        })
        .await?;
        if marked {
            sent += 1;
        }
    }
    Ok(sent)
}

/// Отметить или сбросить непрерывную недоступность раннера. `true` значит, что он
/// недоступен или на паузе уже не меньше [`UNAVAILABLE_FOR`]. Свежая доступность сбрасывает
/// период. Пропуск проходов дольше срока свежести наблюдения тоже сбрасывает: время, которое
/// хаб не наблюдал, не доказывает непрерывность.
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
    let row: Option<(DateTime<Utc>, DateTime<Utc>)> = sqlx::query_as(
        "select since, checked_at from cloud_auto_unavailable where machine_id = $1 for update",
    )
    .bind(machine)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((since, checked_at)) = row
        && now - checked_at < crate::machines::OBSERVATION_FRESH
    {
        sqlx::query("update cloud_auto_unavailable set checked_at = $2 where machine_id = $1")
            .bind(machine)
            .bind(now)
            .execute(&mut *conn)
            .await?;
        return Ok(now - since >= UNAVAILABLE_FOR);
    }
    // Нет отметки или проход пропустил больше трёх минут: период начинается сейчас.
    sqlx::query(
        "insert into cloud_auto_unavailable (machine_id, since, checked_at)
         values ($1, $2, $2)
         on conflict (machine_id) do update set since = $2, checked_at = $2",
    )
    .bind(machine)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(false)
}

/// Старт хаба: периоды недоступности прошлого запуска не продолжаются.
pub async fn reset_unavailable(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("delete from cloud_auto_unavailable")
        .execute(pool)
        .await?;
    Ok(())
}

/// Все статичные раннеры, которые могли бы взять ожидающую облачную работу, недоступны или на
/// паузе непрерывно [`UNAVAILABLE_FOR`]. `unknown` не считается недоступным и сбрасывает
/// период: без свежего наблюдения недоступность не доказана.
pub async fn static_runners_down(
    conn: &mut PgConnection,
    registry: &crate::session::Registry,
    config: &CloudConfig,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    for machine in static_candidates(conn, config).await? {
        let (availability, _) =
            crate::machines::runner_availability_of(conn, registry, machine, now).await?;
        let paused: bool =
            sqlx::query_scalar("select not accepts_remote_work from machines where id = $1")
                .bind(machine)
                .fetch_one(&mut *conn)
                .await?;
        let down = availability == RunnerAvailability::Offline || paused;
        if !track_unavailable(conn, machine, down, now).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Кандидаты статичных раннеров, которые могли бы взять ожидающую облачную работу: метки
/// задачи — подмножество меток раннера, место допускает `runner`, а названная машина — сам
/// раннер (или не названа). Облачная строка исключена: у неё свой вид места.
pub async fn static_candidates(
    conn: &mut PgConnection,
    config: &CloudConfig,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let cloud: Option<Uuid> =
        sqlx::query_scalar("select cloud_machine_id from cloud_budget where singleton")
            .fetch_one(&mut *conn)
            .await?;
    sqlx::query_scalar(
        "select distinct m.id from machines m
           join jobs j on j.state = 'waiting_runner' and j.labels <@ m.labels
          where m.role = 'runner' and m.owner_id = $1
            and ($2::uuid is null or m.id <> $2)
            and j.carrier_id = $3 and j.labels <@ $4
            and 'runner' = any(j.placement)
            and (j.requested_machine_id is null or j.requested_machine_id = m.id)",
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

/// Сумма минут ожидающей облачной работы. Метки, вид места и названная машина проверяются
/// так же, как при размещении; работа других несущих не считается.
pub async fn waiting_cloud_minutes(
    conn: &mut PgConnection,
    config: &CloudConfig,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select coalesce(sum(expected_minutes), 0)::bigint from jobs
          where state = 'waiting_runner' and carrier_id = $1 and labels <@ $2
            and 'cloud' = any(placement)
            and (requested_machine_id is null
                 or requested_machine_id = (select cloud_machine_id from cloud_budget
                                              where singleton))",
    )
    .bind(config.carrier_machine_id)
    .bind(&config.labels)
    .fetch_one(conn)
    .await
}

/// Первая пауза автоматического режима после окна, которое само закончилось, не став готовым.
pub const AUTO_RETRY_BASE: TimeDelta = TimeDelta::minutes(30);
/// Потолок паузы: при постоянной причине неготовности остаётся одна попытка в сутки.
pub const AUTO_RETRY_MAX: TimeDelta = TimeDelta::hours(24);
/// Окна старше этого срока в серию неудач не входят.
const AUTO_FAILURE_MEMORY: TimeDelta = TimeDelta::days(7);
/// Столько последних законченных окон просматривается ради серии.
const AUTO_FAILURE_SCAN: i64 = 32;

/// Пауза перед следующим автоматическим окном после `failures` неудач подряд: каждая
/// следующая вдвое длиннее, до потолка.
pub fn auto_retry_delay(failures: u32) -> TimeDelta {
    let doublings = failures.saturating_sub(1).min(8);
    (AUTO_RETRY_BASE * 2_i32.pow(doublings)).min(AUTO_RETRY_MAX)
}

/// Серия автоматических окон, которые закончились сами, не дождавшись готового раннера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoBackoff {
    /// Последнее неудачное окно серии.
    pub window_id: Uuid,
    pub name: String,
    /// Чем оно закончилось: причина сноса или ошибка создания.
    pub reason: String,
    pub failures: u32,
    /// Раньше этого момента автоматический режим новое окно не создаёт.
    pub retry_at: DateTime<Utc>,
}

/// Память автоматического режима о неудаче живёт в самих окнах. Серию составляют последние
/// автоматические окна без `ready_at`; её обрывает окно, которое стало готовым, от кого бы
/// оно ни было: значит, машины снова поднимаются. Окно, которое удалил владелец, неудачей не
/// считается. Без такой памяти постоянная причина неготовности (не проходит SSH несущей,
/// сломан образ, отказ создания) сменяла бы окно окном на каждом проходе, пока есть бюджет.
pub async fn auto_backoff(
    conn: &mut PgConnection,
    now: DateTime<Utc>,
) -> Result<Option<AutoBackoff>, sqlx::Error> {
    type Row = (Uuid, String, String, bool, bool, DateTime<Utc>, String);
    let rows: Vec<Row> = sqlx::query_as(
        "select id, name, trigger, ready_at is not null,
                delete_reason is not distinct from $3
                  or (state = 'failed' and last_error is not distinct from 'canceled'),
                coalesce(delete_confirmed_at, delete_requested_at, create_called_at, created_at),
                coalesce(delete_reason, last_error, state)
           from cloud_windows
          where state <> all($1) and created_at >= $2
          order by created_at desc
          limit $4",
    )
    .bind(windows::billing_states())
    .bind(now - AUTO_FAILURE_MEMORY)
    .bind(windows::REASON_OWNER)
    .bind(AUTO_FAILURE_SCAN)
    .fetch_all(conn)
    .await?;
    let mut failures = 0_u32;
    let mut last: Option<(Uuid, String, String, DateTime<Utc>)> = None;
    for (id, name, trigger, ready, by_owner, ended_at, reason) in rows {
        if ready {
            break;
        }
        if trigger != windows::Trigger::Auto.as_str() || by_owner {
            continue;
        }
        failures += 1;
        last.get_or_insert((id, name, reason, ended_at));
    }
    Ok(last.map(|(window_id, name, reason, ended_at)| AutoBackoff {
        window_id,
        name,
        reason,
        failures,
        retry_at: ended_at + auto_retry_delay(failures),
    }))
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
