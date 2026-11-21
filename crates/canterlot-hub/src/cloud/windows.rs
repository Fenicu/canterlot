//! Окна облачных серверов: строка `cloud_windows`, транзакция создания с резервом и общие
//! переходы состояний. Вызовы провайдера и циклы живут в `service`.
//!
//! Все деньги в целых копейках. Живое окно одно; создание сериализует замок единственной
//! строки `cloud_budget`, а частичный уникальный индекс держит то же правило на уровне базы.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use chrono::{DateTime, Datelike, FixedOffset, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    host_key::HostKey,
    price::{self, PriceError},
};
use crate::config::CloudConfig;

pub type WindowId = Uuid;

/// Состояния, в которых окно считается живым: сервер может существовать и тарифицироваться.
pub const LIVE: [&str; 6] = [
    "create_intent",
    "create_unknown",
    "identified",
    "ready",
    "draining",
    "deleting",
];

/// То же списком для `= any($n)`.
pub fn live_states() -> Vec<String> {
    LIVE.iter().map(|state| (*state).to_owned()).collect()
}

/// Состояния, в которых раннер `cloud` числится в списке несущей и берёт работу.
pub const SERVING: [&str; 3] = ["identified", "ready", "draining"];

/// Что запустило окно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Card,
    Owner,
    Auto,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Card => "card",
            Self::Owner => "owner",
            Self::Auto => "auto",
        }
    }
}

/// Окно целиком.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Window {
    pub id: Uuid,
    pub name: String,
    pub state: String,
    pub trigger: String,
    pub proposal_id: Option<Uuid>,
    pub server_id: Option<i64>,
    pub ipv4: Option<String>,
    pub host_key_public: String,
    pub profiles: serde_json::Value,
    pub configurator_id: Option<i64>,
    pub price_hourly_kopecks: i64,
    pub reservation_kopecks: i64,
    pub final_cost_kopecks: Option<i64>,
    pub month: String,
    pub hard_cap_minutes: i32,
    pub created_at: DateTime<Utc>,
    pub create_called_at: Option<DateTime<Utc>>,
    pub ready_at: Option<DateTime<Utc>>,
    pub hard_cap_at: Option<DateTime<Utc>>,
    pub drain_until: Option<DateTime<Utc>>,
    pub teardown_until: Option<DateTime<Utc>>,
    pub delete_requested_at: Option<DateTime<Utc>>,
    pub delete_accepted_at: Option<DateTime<Utc>>,
    pub delete_confirmed_at: Option<DateTime<Utc>>,
    pub delete_reason: Option<String>,
    pub delete_attempts: i32,
    pub next_delete_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub attach_sent_at: Option<DateTime<Utc>>,
    pub idle_since: Option<DateTime<Utc>>,
    pub disconnected_since: Option<DateTime<Utc>>,
    pub cancel_sent_at: Option<DateTime<Utc>>,
    pub alert_state: serde_json::Value,
}

impl Window {
    pub fn is_live(&self) -> bool {
        LIVE.contains(&self.state.as_str())
    }
}

pub async fn load(conn: &mut PgConnection, id: Uuid) -> Result<Option<Window>, sqlx::Error> {
    sqlx::query_as("select * from cloud_windows where id = $1")
        .bind(id)
        .fetch_optional(conn)
        .await
}

pub async fn load_for_update(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<Window>, sqlx::Error> {
    sqlx::query_as("select * from cloud_windows where id = $1 for update")
        .bind(id)
        .fetch_optional(conn)
        .await
}

pub async fn live(conn: &mut PgConnection) -> Result<Vec<Window>, sqlx::Error> {
    sqlx::query_as("select * from cloud_windows where state = any($1) order by created_at, id")
        .bind(live_states())
        .fetch_all(conn)
        .await
}

/// Месяц окна: календарный месяц по Москве. Москва без перевода часов, смещение постоянное.
pub fn month_of(at: DateTime<Utc>) -> String {
    let moscow = FixedOffset::east_opt(3 * 3600).expect("a valid fixed offset");
    let local = at.with_timezone(&moscow);
    format!("{:04}-{:02}", local.year(), local.month())
}

/// Закрытые половины хостовых ключей между фиксацией окна и вызовом создания. Только в
/// памяти: на диск, в базу и в журнал они не попадают. После перезапуска хаба окно, которому
/// ещё не звонили, получает новый ключ (`take_or_generate`).
static PENDING_KEYS: LazyLock<Mutex<HashMap<Uuid, HostKey>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn stash_key(id: Uuid, key: HostKey) {
    PENDING_KEYS
        .lock()
        .expect("pending host keys lock")
        .insert(id, key);
}

/// Ключ окна для cloud-init: из памяти, а если хаб успел перезапуститься, новый (его
/// открытая часть заменяет сохранённую вызывающим). Второй результат: ключ новый.
pub fn take_or_generate(id: Uuid) -> Result<(HostKey, bool), super::host_key::HostKeyError> {
    let stashed = PENDING_KEYS
        .lock()
        .expect("pending host keys lock")
        .remove(&id);
    match stashed {
        Some(key) => Ok((key, false)),
        None => HostKey::generate().map(|key| (key, true)),
    }
}

/// Ждёт ли закрытый ключ окна вызова создания (для тестов: ключ не должен пережить вызов).
pub fn has_pending_key(id: Uuid) -> bool {
    PENDING_KEYS
        .lock()
        .expect("pending host keys lock")
        .contains_key(&id)
}

pub fn forget_key(id: Uuid) {
    PENDING_KEYS
        .lock()
        .expect("pending host keys lock")
        .remove(&id);
}

#[derive(Debug, Clone)]
pub struct CreateRequest {
    pub trigger: Trigger,
    pub proposal_id: Option<Uuid>,
    pub hard_cap_minutes: u32,
    /// Согласие владельца на перерасход: ровно резерв этого окна. Работает только как
    /// явное разрешение выйти за бюджет.
    pub consent_over_budget_kopecks: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateRefusal {
    /// Живое окно уже есть: окно одно.
    #[error("live_window_exists")]
    LiveWindowExists,
    /// Резерв не помещается в бюджет месяца, а согласие не равно резерву.
    #[error("over_budget")]
    OverBudget {
        reservation_kopecks: i64,
        remaining_kopecks: i64,
    },
    #[error("invalid_request")]
    Invalid(&'static str),
    #[error("price_error")]
    Price(#[from] PriceError),
    #[error("host_key_error")]
    HostKey(#[from] super::host_key::HostKeyError),
    /// Цену не удалось получить у провайдера: создавать нельзя, резерв считать не из чего.
    #[error("provider_unavailable")]
    Provider(super::ProviderError),
    #[error("db_error")]
    Database(#[from] sqlx::Error),
}

/// Цена, по которой считается резерв; её получает вызывающая сторона у провайдера заранее,
/// чтобы сетевой вызов не держал замок.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pricing {
    /// Профили в порядке попыток, у каждого цена часа.
    pub profiles: Vec<PricedProfile>,
}

/// Профиль с ценой часа у провайдера на момент создания окна.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct PricedProfile {
    pub configurator_id: i64,
    pub cpu: u32,
    pub ram_gb: u32,
    pub disk_gb: u32,
    pub hourly_kopecks: i64,
}

impl PricedProfile {
    pub fn ram_mb(&self) -> u64 {
        u64::from(self.ram_gb) * 1024
    }

    pub fn disk_mb(&self) -> u64 {
        u64::from(self.disk_gb) * 1024
    }
}

impl Pricing {
    /// Самая дорогая цена часа в списке: по ней резерв и проверка бюджета, чтобы любой
    /// принятый профиль уложился в резерв.
    pub fn max_hourly_kopecks(&self) -> Option<i64> {
        self.profiles
            .iter()
            .map(|profile| profile.hourly_kopecks)
            .max()
    }
}

impl Window {
    /// Профили окна в порядке попыток (снимок на момент создания).
    pub fn profiles(&self) -> Vec<PricedProfile> {
        serde_json::from_value(self.profiles.clone()).unwrap_or_default()
    }
}

pub const MAX_HARD_CAP_MINUTES: u32 = 24 * 60;

/// Резерв окна: час × (ceil((предел + срок на доработку + запас на снос) / 60) + 1).
pub fn reservation_for(
    config: &CloudConfig,
    hourly_kopecks: i64,
    hard_cap_minutes: u32,
) -> Result<i64, PriceError> {
    let minutes = price::total_minutes(
        u64::from(hard_cap_minutes),
        config.drain_grace_minutes(),
        config.teardown_margin_minutes(),
    )?;
    price::reservation_kopecks(hourly_kopecks, minutes)
}

/// Расход месяца: законченные окна по итоговой цене, живые по резерву.
pub async fn spent_in_month(conn: &mut PgConnection, month: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select coalesce(sum(case when state = any($2) then reservation_kopecks
                                  else coalesce(final_cost_kopecks, 0) end), 0)::bigint
         from cloud_windows where month = $1",
    )
    .bind(month)
    .bind(live_states())
    .fetch_one(conn)
    .await
}

/// Создание окна одной транзакцией, для любого входа (карточка, кнопка, автоматика):
/// замок `cloud_budget`, отказ при живом окне, резерв, проверка бюджета, вставка
/// `create_intent` с хостовым ключом (в базе только открытая часть). Вызов провайдера идёт
/// после фиксации, из службы окон.
pub async fn create_in_tx(
    conn: &mut PgConnection,
    config: &CloudConfig,
    pricing: Pricing,
    request: CreateRequest,
) -> Result<WindowId, CreateRefusal> {
    if request.hard_cap_minutes == 0 || request.hard_cap_minutes > MAX_HARD_CAP_MINUTES {
        return Err(CreateRefusal::Invalid("hard_cap_minutes"));
    }
    let budget: i64 = sqlx::query_scalar(
        "select monthly_budget_kopecks from cloud_budget where singleton for update",
    )
    .fetch_one(&mut *conn)
    .await?;
    let existing: bool =
        sqlx::query_scalar("select exists(select 1 from cloud_windows where state = any($1))")
            .bind(live_states())
            .fetch_one(&mut *conn)
            .await?;
    if existing {
        return Err(CreateRefusal::LiveWindowExists);
    }
    let Some(max_hourly) = pricing.max_hourly_kopecks() else {
        return Err(CreateRefusal::Invalid("profiles"));
    };
    let reservation = reservation_for(config, max_hourly, request.hard_cap_minutes)?;
    let now = Utc::now();
    let month = month_of(now);
    let spent = spent_in_month(&mut *conn, &month).await?;
    let remaining = budget.saturating_sub(spent);
    let fits = spent
        .checked_add(reservation)
        .is_some_and(|total| total <= budget);
    if !fits && request.consent_over_budget_kopecks != Some(reservation) {
        return Err(CreateRefusal::OverBudget {
            reservation_kopecks: reservation,
            remaining_kopecks: remaining,
        });
    }
    let id = Uuid::now_v7();
    let key = HostKey::generate()?;
    sqlx::query(
        "insert into cloud_windows (id, name, state, trigger, proposal_id, host_key_public,
                                    price_hourly_kopecks, reservation_kopecks, month,
                                    hard_cap_minutes, created_at, profiles)
         values ($1, $2, 'create_intent', $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(id)
    .bind(format!("canterlot-runner-{id}"))
    .bind(request.trigger.as_str())
    .bind(request.proposal_id)
    .bind(key.public_line())
    .bind(max_hourly)
    .bind(reservation)
    .bind(&month)
    .bind(i32::try_from(request.hard_cap_minutes).expect("bounded by MAX_HARD_CAP_MINUTES"))
    .bind(now)
    .bind(serde_json::to_value(&pricing.profiles).expect("profiles serialize"))
    .execute(&mut *conn)
    .await?;
    stash_key(id, key);
    Ok(id)
}

/// Имя сервера окна и обратный разбор: `canterlot-runner-<uuid>`.
pub fn server_name(id: Uuid) -> String {
    format!("canterlot-runner-{id}")
}

/// Окно по имени сервера у провайдера. Принимается только строгий вид с каноническим UUID:
/// чужие имена сверка не трогает.
pub fn window_id_from_name(name: &str) -> Option<Uuid> {
    let tail = name.strip_prefix("canterlot-runner-")?;
    let id = Uuid::parse_str(tail).ok()?;
    (id.hyphenated().to_string() == tail).then_some(id)
}

// ───────── Переходы, общие для службы и HTTP ─────────

/// Окно закончилось без сервера: резерв снят, итог 0.
pub async fn fail_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
    reason: &str,
) -> Result<bool, sqlx::Error> {
    let changed = sqlx::query(
        "update cloud_windows set state = 'failed', final_cost_kopecks = 0, last_error = $2
         where id = $1 and state in ('create_intent', 'create_unknown')",
    )
    .bind(id)
    .bind(reason)
    .execute(conn)
    .await?
    .rows_affected()
        > 0;
    if changed {
        forget_key(id);
    }
    Ok(changed)
}

/// Причина удаления окна, которое было запрошено.
pub const REASON_OWNER: &str = "owner";

/// Удалить окно сейчас: ожидающая создания заявка отменяется, окно с сервером уходит в
/// `deleting` (раннер отзывается, сервер удалит служба). Возвращает новое состояние или
/// `None`, если окна нет. `create_unknown` отказывается: сервера ещё нет в известных.
pub async fn request_delete(
    pool: &PgPool,
    id: Uuid,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<DeleteRequested, sqlx::Error> {
    let reason = reason.to_owned();
    let outcome = crate::db::in_tx(pool, move |conn| {
        Box::pin(async move { request_delete_in_tx(conn, id, &reason, now).await })
    })
    .await?;
    if outcome == DeleteRequested::Deleting {
        // Отзыв раннера вне транзакции окна: у него свой порядок замков (машина задач
        // раньше строки машины).
        revoke_cloud_machine(pool).await?;
    }
    Ok(outcome)
}

/// Переход «удалить сейчас» в чужой транзакции. Раннера не отзывает: это делает вызывающая
/// после фиксации (`revoke_cloud_machine`), а при её отказе служба повторяет отзыв перед
/// каждой попыткой удаления.
pub async fn request_delete_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<DeleteRequested, sqlx::Error> {
    let Some(window) = load_for_update(conn, id).await? else {
        return Ok(DeleteRequested::NotFound);
    };
    match window.state.as_str() {
        "create_intent" if window.create_called_at.is_none() => {
            fail_in_tx(conn, id, "canceled").await?;
            Ok(DeleteRequested::Canceled)
        }
        "create_intent" | "create_unknown" => Ok(DeleteRequested::NotYet),
        "identified" | "ready" | "draining" => {
            mark_deleting_in_tx(conn, &window, reason, now).await?;
            Ok(DeleteRequested::Deleting)
        }
        "deleting" => Ok(DeleteRequested::Deleting),
        _ => Ok(DeleteRequested::AlreadyEnded),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeleteRequested {
    NotFound,
    /// Заявка на создание отменена, сервера не было.
    Canceled,
    /// Создание вызвано, сервер ещё не найден: удалять нечего, сверка разберётся.
    NotYet,
    Deleting,
    AlreadyEnded,
}

/// Окно в `deleting`: срок первой попытки — сейчас. Раннер отзывается отдельным вызовом
/// после фиксации (`revoke_cloud_machine`).
pub async fn mark_deleting_in_tx(
    conn: &mut PgConnection,
    window: &Window,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update cloud_windows set state = 'deleting', delete_requested_at = $2, delete_reason = $3,
                next_delete_at = $2, idle_since = null, disconnected_since = null
         where id = $1 and state in ('identified', 'ready', 'draining')",
    )
    .bind(window.id)
    .bind(now)
    .bind(reason)
    .execute(&mut *conn)
    .await?;
    // Команда подключения, которую несущая ещё не исполнила, больше не нужна.
    sqlx::query(
        "update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, now())
         where kind = 'attach_runner' and payload->>'window_id' = $1 and delivery_state <> 'completed'",
    )
    .bind(window.id.to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// Строка машины `cloud`, если она уже заведена.
pub async fn cloud_machine_id(conn: &mut PgConnection) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("select cloud_machine_id from cloud_budget where singleton")
        .fetch_one(conn)
        .await
}

/// Отзыв раннера `cloud`: токен гаснет, его незаконченные задачи становятся `lost`.
pub async fn revoke_cloud_machine(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let machine = cloud_machine_id(&mut conn).await?;
    drop(conn);
    if let Some(machine) = machine {
        crate::machines::revoke(pool, machine).await?;
    }
    Ok(())
}

/// Строка раннера `cloud` создаётся при первом использовании и дальше переиспользуется.
/// `reset_accepting` возвращает ей приём работы: окно только что началось.
pub async fn ensure_cloud_machine(
    conn: &mut PgConnection,
    config: &CloudConfig,
    reset_accepting: bool,
) -> Result<Uuid, sqlx::Error> {
    let existing: Option<Uuid> =
        sqlx::query_scalar("select cloud_machine_id from cloud_budget where singleton for update")
            .fetch_one(&mut *conn)
            .await?;
    if let Some(machine) = existing {
        sqlx::query(
            "update machines set parent_machine_id = $2, root_dir = $3, rank = $4, labels = $5,
                    accepts_remote_work = case when $6 then true else accepts_remote_work end
             where id = $1",
        )
        .bind(machine)
        .bind(config.carrier_machine_id)
        .bind(crate::config::CLOUD_ROOT_DIR)
        .bind(config.rank)
        .bind(&config.labels)
        .bind(reset_accepting)
        .execute(&mut *conn)
        .await?;
        return Ok(machine);
    }
    let owner: Uuid = sqlx::query_scalar("select owner_id from machines where id = $1")
        .bind(config.carrier_machine_id)
        .fetch_one(&mut *conn)
        .await?;
    let machine = Uuid::now_v7();
    sqlx::query(
        "insert into machines (id, owner_id, name, role, parent_machine_id, root_dir, rank,
                               labels, accepts_remote_work)
         values ($1, $2, 'cloud', 'runner', $3, $4, $5, $6, true)",
    )
    .bind(machine)
    .bind(owner)
    .bind(config.carrier_machine_id)
    .bind(crate::config::CLOUD_ROOT_DIR)
    .bind(config.rank)
    .bind(&config.labels)
    .execute(&mut *conn)
    .await?;
    sqlx::query("update cloud_budget set cloud_machine_id = $1 where singleton")
        .bind(machine)
        .execute(&mut *conn)
        .await?;
    Ok(machine)
}

// ───────── Чтение для HTTP ─────────

/// Окно для клиентов: деньги в копейках, время RFC 3339.
#[derive(Debug, Clone, Serialize, serde::Deserialize, ToSchema)]
pub struct CloudWindowView {
    pub id: Uuid,
    pub name: String,
    pub state: String,
    pub trigger: String,
    pub server_id: Option<i64>,
    pub ipv4: Option<String>,
    /// Конфигуратор принятого профиля; пусто, пока сервера нет.
    pub configurator_id: Option<i64>,
    /// Цена часа: до приёма профиля самая дорогая из списка, после действительная.
    pub price_hourly_kopecks: i64,
    pub reservation_kopecks: i64,
    /// Итог закончившегося окна; у живого пусто.
    pub final_cost_kopecks: Option<i64>,
    /// Оценка расхода живого окна: часы с вызова создания, округлённые вверх, по цене часа,
    /// не больше резерва.
    pub cost_so_far_kopecks: i64,
    pub month: String,
    pub created_at: DateTime<Utc>,
    pub ready_at: Option<DateTime<Utc>>,
    pub hard_cap_at: Option<DateTime<Utc>>,
    pub delete_requested_at: Option<DateTime<Utc>>,
    pub delete_confirmed_at: Option<DateTime<Utc>>,
    pub delete_reason: Option<String>,
    pub last_error: Option<String>,
}

pub fn view(window: &Window, now: DateTime<Utc>) -> CloudWindowView {
    let cost_so_far = match (window.final_cost_kopecks, window.create_called_at) {
        (Some(cost), _) => cost,
        (None, Some(called)) => {
            let seconds = u64::try_from((now - called).num_seconds().max(0)).unwrap_or(0);
            price::final_cost_kopecks(window.price_hourly_kopecks, seconds.div_ceil(60))
                .map_or(window.reservation_kopecks, |cost| {
                    cost.min(window.reservation_kopecks)
                })
        }
        (None, None) => 0,
    };
    CloudWindowView {
        id: window.id,
        name: window.name.clone(),
        state: window.state.clone(),
        trigger: window.trigger.clone(),
        server_id: window.server_id,
        ipv4: window.ipv4.clone(),
        configurator_id: window.configurator_id,
        price_hourly_kopecks: window.price_hourly_kopecks,
        reservation_kopecks: window.reservation_kopecks,
        final_cost_kopecks: window.final_cost_kopecks,
        cost_so_far_kopecks: cost_so_far,
        month: window.month.clone(),
        created_at: window.created_at,
        ready_at: window.ready_at,
        hard_cap_at: window.hard_cap_at,
        delete_requested_at: window.delete_requested_at,
        delete_confirmed_at: window.delete_confirmed_at,
        delete_reason: window.delete_reason.clone(),
        last_error: window.last_error.clone(),
    }
}

/// Живые окна и последние `ended_limit` законченных, новые первыми.
pub async fn list(conn: &mut PgConnection, ended_limit: i64) -> Result<Vec<Window>, sqlx::Error> {
    sqlx::query_as(
        "(select * from cloud_windows where state = any($1))
         union all
         (select * from cloud_windows where state <> all($1) order by created_at desc, id desc limit $2)
         order by created_at desc, id desc",
    )
    .bind(live_states())
    .bind(ended_limit)
    .fetch_all(conn)
    .await
}
