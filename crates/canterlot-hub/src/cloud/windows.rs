//! Окна облачных машин: строка `cloud_windows`, транзакция создания с резервом и общие
//! переходы состояний. Вызовы провайдера и циклы живут в `service`.
//!
//! Все деньги в целых копейках. Живое окно одно; создание сериализует замок единственной
//! строки `cloud_budget`, а частичный уникальный индекс держит то же правило на уровне базы.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use chrono::{DateTime, Datelike, FixedOffset, TimeDelta, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{
    host_key::HostKey,
    price::{self, PriceError},
    yandex,
};
use crate::config::CloudConfig;

pub type WindowId = Uuid;

/// Состояния, в которых окно считается живым: сервер может существовать и тарифицироваться.
/// Живое окно одно, его держит частичный уникальный индекс.
pub const LIVE: [&str; 6] = [
    "create_intent",
    "create_unknown",
    "identified",
    "ready",
    "draining",
    "deleting",
];

/// Поздний снос сервера закрытого окна: своего места в правиле одного живого окна он не
/// занимает (у преемника оно своё), но сервер существует и тарифицируется, поэтому снос
/// входит в расход и закрывает новые создания.
pub const LATE_CLEANUP: &str = "late_cleanup";

/// То же списком для `= any($n)`.
pub fn live_states() -> Vec<String> {
    LIVE.iter().map(|state| (*state).to_owned()).collect()
}

/// Состояния, в которых окно ещё владеет сервером: живые окна и поздний снос.
pub fn billing_states() -> Vec<String> {
    let mut states = live_states();
    states.push(LATE_CLEANUP.to_owned());
    states
}

/// Владеет ли окно сервером в этом состоянии.
pub fn owns_server(state: &str) -> bool {
    LIVE.contains(&state) || state == LATE_CLEANUP
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
    /// Имя машины и её загрузочного диска у провайдера.
    pub name: String,
    pub state: String,
    pub trigger: String,
    pub proposal_id: Option<Uuid>,
    /// Окно, которое это окно заменяет после прерывания.
    pub replaces_window_id: Option<Uuid>,
    /// Каталог провайдера на момент создания: снос и сверка окна идут в нём.
    pub folder_id: String,
    pub instance_id: Option<String>,
    /// Операция создания; пусто, если машину нашли по имени в списке.
    pub create_operation_id: Option<String>,
    /// Операция последнего принятого удаления машины.
    pub delete_operation_id: Option<String>,
    /// Публичный адрес машины: по нему подключается несущая.
    pub public_ip: Option<String>,
    pub boot_disk_id: Option<String>,
    /// Прерываемая машина: снимок на момент создания окна.
    pub preemptible: bool,
    /// Профиль, которому звонили последним.
    pub cores: Option<i32>,
    pub memory_gb: Option<i32>,
    pub disk_gb: Option<i32>,
    /// Ставки часа этого профиля: машина с адресом и диск.
    pub compute_kopecks_per_hour: i64,
    pub disk_kopecks_per_hour: i64,
    /// Машина замечена останавливающейся или остановленной.
    pub stopped_at: Option<DateTime<Utc>>,
    /// Машины нет в полном списке каталога.
    pub server_gone_at: Option<DateTime<Utc>>,
    /// Диска нет в полном списке дисков каталога.
    pub disk_gone_at: Option<DateTime<Utc>>,
    pub host_key_public: String,
    pub profiles: serde_json::Value,
    /// Сумма двух ставок: по ней резерв.
    pub price_hourly_kopecks: i64,
    pub reservation_kopecks: i64,
    pub final_cost_kopecks: Option<i64>,
    pub month: String,
    pub hard_cap_minutes: i32,
    /// Утверждённые сроки на момент создания.
    pub drain_grace_minutes: Option<i32>,
    pub teardown_margin_minutes: Option<i32>,
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
    /// Окно ещё не закончилось: оно живо или держит обязательство позднего сноса.
    pub fn is_live(&self) -> bool {
        owns_server(&self.state)
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

/// Живые окна и поздние сносы: служба ведёт и те и другие.
pub async fn live(conn: &mut PgConnection) -> Result<Vec<Window>, sqlx::Error> {
    sqlx::query_as("select * from cloud_windows where state = any($1) order by created_at, id")
        .bind(billing_states())
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
    /// Окно, которое это окно заменяет после прерывания. Замена не спрашивает согласия на
    /// перерасход и не проходит условия автоматического режима: их заменили предусловия
    /// вызывающего (`on_preempted_cleanup_done_in_tx`).
    pub replaces_window_id: Option<Uuid>,
    pub hard_cap_minutes: u32,
    /// Согласие владельца на перерасход: ровно резерв этого окна. Работает только как
    /// явное разрешение выйти за бюджет.
    pub consent_over_budget_kopecks: Option<i64>,
}

/// Условия автоматического режима, перепроверяемые в транзакции создания под замком
/// `cloud_budget`: реестр нужен правилу доступности раннеров, время — проходу службы. Метка
/// прохода снята до цены у провайдера, поэтому под замком она дополняется часами базы: для
/// проверки берётся более позднее из двух времён.
pub struct AutoGuard<'a> {
    pub registry: &'a crate::session::Registry,
    pub now: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateRefusal {
    /// Живое окно уже есть: окно одно.
    #[error("live_window_exists")]
    LiveWindowExists,
    /// Открыт поздний снос сервера закрытого окна: новый не создаётся, пока старый убирают.
    #[error("late_cleanup_open")]
    LateCleanup,
    /// Блок провайдера (`cloud_budget.provider_block_code`): создание закрыто, пока владелец
    /// не снимет его (`clear_provider_block`).
    #[error("provider_blocked")]
    ProviderBlocked { code: String },
    /// Резерв не помещается в бюджет месяца, а согласие не равно резерву.
    #[error("over_budget")]
    OverBudget {
        reservation_kopecks: i64,
        remaining_kopecks: i64,
    },
    #[error("invalid_request")]
    Invalid(&'static str),
    /// Условия автоматического режима изменились между проверкой и транзакцией создания.
    #[error("auto_conditions_changed")]
    AutoConditionsChanged,
    #[error("price_error")]
    Price(#[from] PriceError),
    #[error("host_key_error")]
    HostKey(#[from] super::host_key::HostKeyError),
    /// Цену не удалось получить у провайдера: создавать нельзя, резерв считать не из чего.
    #[error("provider_unavailable")]
    Provider(yandex::PriceError),
    /// Машина из конфигурации не несущая: подключить раннер некому.
    #[error("carrier_unavailable")]
    CarrierUnavailable,
    #[error("db_error")]
    Database(#[from] sqlx::Error),
}

/// Цена, по которой считается резерв; её получает вызывающая сторона у провайдера заранее,
/// чтобы сетевой вызов не держал замок.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pricing {
    /// Цены посчитаны для прерываемой машины: окно запоминает это вместе с ними.
    pub preemptible: bool,
    /// Профили в порядке попыток, у каждого две ставки часа.
    pub profiles: Vec<PricedProfile>,
}

/// Профиль с двумя ставками часа у провайдера на момент создания окна.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct PricedProfile {
    pub cores: u32,
    pub memory_gb: u32,
    pub disk_gb: u32,
    /// Машина с публичным адресом: платится, пока машина есть в каталоге.
    pub compute_kopecks_per_hour: i64,
    /// Диск: платится, пока есть диск.
    pub disk_kopecks_per_hour: i64,
}

impl PricedProfile {
    /// Обе ставки вместе: цена часа работающей машины.
    pub fn hourly_kopecks(&self) -> i64 {
        self.compute_kopecks_per_hour
            .saturating_add(self.disk_kopecks_per_hour)
    }
}

impl Pricing {
    /// Самый дорогой профиль списка: по нему резерв и проверка бюджета, чтобы любой
    /// принятый профиль уложился в резерв.
    pub fn most_expensive(&self) -> Option<&PricedProfile> {
        self.profiles
            .iter()
            .max_by_key(|profile| profile.hourly_kopecks())
    }

    /// Цена часа самого дорогого профиля, обе ставки вместе.
    pub fn max_hourly_kopecks(&self) -> Option<i64> {
        self.most_expensive().map(PricedProfile::hourly_kopecks)
    }
}

impl Window {
    /// Профили окна в порядке попыток (снимок на момент создания).
    pub fn profiles(&self) -> Vec<PricedProfile> {
        serde_json::from_value(self.profiles.clone()).unwrap_or_default()
    }
}

pub const MAX_HARD_CAP_MINUTES: u32 = 24 * 60;

/// Резерв окна: час × (ceil((предел + срок на доработку + запас на снос) / 60) + 1), где час —
/// обе ставки вместе: машина с публичным адресом и диск.
pub fn reservation_for(
    config: &CloudConfig,
    hourly_kopecks: i64,
    hard_cap_minutes: u32,
) -> Result<i64, PriceError> {
    reservation_for_terms(
        hourly_kopecks,
        hard_cap_minutes,
        config.drain_grace_minutes(),
        config.teardown_margin_minutes(),
    )
}

/// Резерв по утверждённым срокам окна, а не по текущей конфигурации.
pub fn reservation_for_terms(
    hourly_kopecks: i64,
    hard_cap_minutes: u32,
    drain_grace_minutes: u64,
    teardown_margin_minutes: u64,
) -> Result<i64, PriceError> {
    let minutes = price::total_minutes(
        u64::from(hard_cap_minutes),
        drain_grace_minutes,
        teardown_margin_minutes,
    )?;
    price::reservation_kopecks(hourly_kopecks, minutes)
}

/// Расход месяца: законченные окна по итоговой цене, живые и поздние сносы по резерву.
pub async fn spent_in_month(conn: &mut PgConnection, month: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select coalesce(sum(case when state = any($2) then reservation_kopecks
                                  else coalesce(final_cost_kopecks, 0) end), 0)::bigint
         from cloud_windows where month = $1",
    )
    .bind(month)
    .bind(billing_states())
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
    auto: Option<AutoGuard<'_>>,
) -> Result<WindowId, CreateRefusal> {
    if request.hard_cap_minutes == 0 || request.hard_cap_minutes > MAX_HARD_CAP_MINUTES {
        return Err(CreateRefusal::Invalid("hard_cap_minutes"));
    }
    // Новое окно имеет смысл только с живой несущей: без неё сервер некому подключить и он
    // проживёт до `ready_timeout` за деньги. Снос и сверка без неё работают.
    let carrier: bool = sqlx::query_scalar(
        "select exists(select 1 from machines where id = $1 and role = 'worker' and is_carrier)",
    )
    .bind(config.carrier_machine_id)
    .fetch_one(&mut *conn)
    .await?;
    if !carrier {
        return Err(CreateRefusal::CarrierUnavailable);
    }
    let (budget, block, auto_mode): (i64, Option<String>, bool) = sqlx::query_as(
        "select monthly_budget_kopecks, provider_block_code, auto_mode from cloud_budget
         where singleton for update",
    )
    .fetch_one(&mut *conn)
    .await?;
    // Провайдер заблокировал аккаунт правилом против злоупотреблений: новые серверы
    // создавать бесполезно и дорого, пока владелец не снимет блок.
    if let Some(code) = block {
        return Err(CreateRefusal::ProviderBlocked { code });
    }
    // Время — не метка прохода: она снята до цены у провайдера и к проверке недоступности
    // устарела, поэтому для условий автоматического режима берётся более позднее из метки и
    // часов базы внутри этой транзакции.
    let db_now: DateTime<Utc> = sqlx::query_scalar("select clock_timestamp()")
        .fetch_one(&mut *conn)
        .await?;
    let now = auto.as_ref().map_or(db_now, |guard| guard.now.max(db_now));
    // Замена прерванного окна не проходит условия автоматического режима: их заменили
    // предусловия `on_preempted_cleanup_done_in_tx` под тем же замком бюджета. Единственное,
    // что проверяется здесь, — автоматический режим не выключили между прерыванием и концом.
    let replacement = request.replaces_window_id.is_some();
    if replacement {
        if request.trigger == Trigger::Auto && !auto_mode {
            return Err(CreateRefusal::AutoConditionsChanged);
        }
    } else {
        // Автоматический режим проверяет свои условия ещё раз здесь, под тем же замком, что и
        // вставка: между чтением настроек службой и этой транзакцией владелец мог выключить
        // режим, очередь могла опустеть, а раннер — вернуться на связь.
        if let Some(guard) = auto.as_ref()
            && request.trigger == Trigger::Auto
            && (!auto_mode
                || super::budget::waiting_cloud_minutes(conn, config).await?
                    < super::budget::MIN_WAITING_MINUTES
                || !super::budget::static_runners_down(conn, guard.registry, config, now).await?)
        {
            return Err(CreateRefusal::AutoConditionsChanged);
        }
        if auto.is_none() && request.trigger == Trigger::Auto {
            return Err(CreateRefusal::AutoConditionsChanged);
        }
    }
    let existing: bool =
        sqlx::query_scalar("select exists(select 1 from cloud_windows where state = any($1))")
            .bind(live_states())
            .fetch_one(&mut *conn)
            .await?;
    if existing {
        return Err(CreateRefusal::LiveWindowExists);
    }
    // Поздний снос машины закрытого окна: своего места в правиле одного живого окна он не
    // занимает, но пока машина не убрана, новая не создаётся — иначе их будет две.
    let late: bool = sqlx::query_scalar(
        "select exists(select 1 from cloud_windows where state = 'late_cleanup')",
    )
    .fetch_one(&mut *conn)
    .await?;
    if late {
        return Err(CreateRefusal::LateCleanup);
    }
    let Some(top) = pricing.most_expensive().cloned() else {
        return Err(CreateRefusal::Invalid("profiles"));
    };
    let max_hourly = top.hourly_kopecks();
    let reservation = reservation_for(config, max_hourly, request.hard_cap_minutes)?;
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
    let drain_grace = i32::try_from(config.drain_grace_minutes()).unwrap_or(i32::MAX);
    let teardown_margin = i32::try_from(config.teardown_margin_minutes()).unwrap_or(i32::MAX);
    // До вызова создания в строке стоят ставки самого дорогого профиля: по ним резерв.
    sqlx::query(
        "insert into cloud_windows (id, name, state, trigger, proposal_id, replaces_window_id,
                                    folder_id, preemptible, host_key_public,
                                    compute_kopecks_per_hour, disk_kopecks_per_hour,
                                    price_hourly_kopecks, reservation_kopecks, month,
                                    hard_cap_minutes, drain_grace_minutes, teardown_margin_minutes,
                                    created_at, profiles)
         values ($1, $2, 'create_intent', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                 $15, $16, $17, $18)",
    )
    .bind(id)
    .bind(server_name(id))
    .bind(request.trigger.as_str())
    .bind(request.proposal_id)
    .bind(request.replaces_window_id)
    .bind(&config.folder_id)
    .bind(pricing.preemptible)
    .bind(key.public_line())
    .bind(top.compute_kopecks_per_hour)
    .bind(top.disk_kopecks_per_hour)
    .bind(max_hourly)
    .bind(reservation)
    .bind(&month)
    .bind(i32::try_from(request.hard_cap_minutes).expect("bounded by MAX_HARD_CAP_MINUTES"))
    .bind(drain_grace)
    .bind(teardown_margin)
    .bind(now)
    .bind(serde_json::to_value(&pricing.profiles).expect("profiles serialize"))
    .execute(&mut *conn)
    .await?;
    stash_key(id, key);
    Ok(id)
}

/// Имя машины окна и её загрузочного диска: `canterlot-runner-<uuid>`.
pub fn server_name(id: Uuid) -> String {
    format!("canterlot-runner-{id}")
}

/// Окно по имени машины или диска у провайдера. Принимается только строгий вид с
/// каноническим UUID: чужие имена сверка не трогает.
pub fn window_id_from_name(name: &str) -> Option<Uuid> {
    let tail = name.strip_prefix("canterlot-runner-")?;
    let id = Uuid::parse_str(tail).ok()?;
    (id.hyphenated().to_string() == tail).then_some(id)
}

// ───────── Переходы, общие для службы и HTTP ─────────

/// Заявка закончилась до вызова создания: машины не было и быть не могло, резерв снят,
/// итог 0. После вызова создания окно заканчивается только через снос (`begin_cleanup_in_tx`).
pub async fn fail_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
    reason: &str,
) -> Result<bool, sqlx::Error> {
    let changed = sqlx::query(
        "update cloud_windows set state = 'failed', final_cost_kopecks = 0, last_error = $2
         where id = $1 and state = 'create_intent' and create_called_at is null",
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

/// Блок провайдера: код и момент пишутся в `cloud_budget`, пока блок стоит, `create_in_tx`
/// отказывает `ProviderBlocked`. Повторный отказ обновляет момент. Сейчас блок никто не
/// ставит сам: это ручка на случай, когда провайдер закрывает аккаунту создание машин.
pub async fn set_provider_block(
    pool: &PgPool,
    code: &str,
    at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update cloud_budget set provider_block_code = $1, provider_block_at = $2 where singleton",
    )
    .bind(code)
    .bind(at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Снять блок провайдера. Только для владельца: вызывает её его действие, не служба и не
/// автоматика. Возвращает, стоял ли блок.
pub async fn clear_provider_block(conn: &mut PgConnection) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "update cloud_budget set provider_block_code = null, provider_block_at = null
         where singleton and provider_block_code is not null",
    )
    .execute(conn)
    .await?
    .rows_affected()
        > 0)
}

/// Текущий блок провайдера: код и момент.
pub async fn provider_block(
    conn: &mut PgConnection,
) -> Result<Option<(String, DateTime<Utc>)>, sqlx::Error> {
    let row: (Option<String>, Option<DateTime<Utc>>) = sqlx::query_as(
        "select provider_block_code, provider_block_at from cloud_budget where singleton",
    )
    .fetch_one(conn)
    .await?;
    Ok(match row {
        (Some(code), Some(at)) => Some((code, at)),
        _ => None,
    })
}

/// Причины удаления (`delete_reason`), по которым служба и следующие слои принимают решения.
/// Остальные причины (`idle`, `drained`, `hard_cap`, …) — только текст для владельца.
pub const REASON_OWNER: &str = "owner";
/// Прерываемую машину остановил Яндекс.
pub const REASON_PREEMPTED: &str = "preempted";
/// Обычная машина остановилась сама или машина сломалась (`ERROR`, `CRASHED`).
pub const REASON_VM_BROKEN: &str = "vm_broken";
/// Создание не дало рабочей машины: отказ, ошибка операции или пустые списки. Окно после
/// сноса заканчивается `failed`.
pub const REASON_CREATE_FAILED: &str = "create_failed";
/// Машина пропала из каталога без нашего удаления. Окно после сноса заканчивается `gone`.
pub const REASON_GONE: &str = "gone";
/// Машина закрытого окна нашлась позже.
pub const REASON_LATE_CREATE: &str = "late_create";
/// Диск закрытого окна нашёлся позже.
pub const REASON_LATE_DISK: &str = "late_disk";

/// Чем заканчивается окно после сноса: зависит только от причины.
pub fn terminal_state(delete_reason: Option<&str>) -> &'static str {
    match delete_reason {
        Some(REASON_GONE) => "gone",
        Some(REASON_CREATE_FAILED) => "failed",
        _ => "ended",
    }
}

/// Удалить окно сейчас: ожидающая создания заявка отменяется, окно с машиной уходит в
/// `deleting` (раннер отзывается в той же транзакции, машину удалит служба). Возвращает новое
/// состояние или `NotFound`, если окна нет. `create_unknown` отказывается: машины ещё нет в
/// известных.
pub async fn request_delete(
    pool: &PgPool,
    id: Uuid,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<DeleteRequested, sqlx::Error> {
    let reason = reason.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move { request_delete_in_tx(conn, id, &reason, now).await })
    })
    .await
}

/// Переход «удалить сейчас» в чужой транзакции. Переход в `deleting` сам отзывает раннера
/// `cloud`: только текущее живое окно владеет его строкой.
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
            begin_cleanup_in_tx(conn, id, Cleanup::because(reason), now).await?;
            Ok(DeleteRequested::Deleting)
        }
        // Поздний снос уже идёт: это то же удаление, только без места в правиле одного окна.
        "deleting" | LATE_CLEANUP => Ok(DeleteRequested::Deleting),
        _ => Ok(DeleteRequested::AlreadyEnded),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeleteRequested {
    NotFound,
    /// Заявка на создание отменена, машины не было.
    Canceled,
    /// Создание вызвано, машина ещё не найдена: удалять нечего, сверка разберётся.
    NotYet,
    Deleting,
    AlreadyEnded,
}

/// С чем окно уходит на снос.
#[derive(Debug, Clone, Copy)]
pub struct Cleanup<'a> {
    /// `delete_reason`: от неё зависит и конечное состояние (`terminal_state`).
    pub reason: &'a str,
    /// Подробность для владельца; пусто — прежний текст остаётся.
    pub last_error: Option<&'a str>,
    /// Машина замечена останавливающейся или остановленной: ставится `stopped_at`.
    pub stopped: bool,
    /// Полный список каталога уже подтвердил, что машины нет: снос начнёт с диска.
    pub server_gone: bool,
}

impl<'a> Cleanup<'a> {
    pub fn because(reason: &'a str) -> Self {
        Self {
            reason,
            last_error: None,
            stopped: false,
            server_gone: false,
        }
    }
}

/// Окно уходит на снос (`deleting`): единственный путь к концу после вызова создания. Дальше
/// служба убирает машину, затем диск, и только потом заканчивает окно.
///
/// Принимаются окна, которые обслуживают раннер (`identified`, `ready`, `draining`), и окна,
/// у которых создание вызвано, а машина не найдена (`create_intent`, `create_unknown`).
/// Причина, выставленная раньше, не перезаписывается: окно в `deleting` сюда не попадает.
///
/// Здесь же **отзывается раннер `cloud`**, в той же транзакции, что переводит окно: отзыв
/// вне её мог бы погасить токен уже следующего окна. Его незаконченные задачи при этом
/// становятся `lost`; причина сноса (`cleanup.reason`) в этот момент известна.
pub async fn begin_cleanup_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
    cleanup: Cleanup<'_>,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let Some(window) = load_for_update(conn, id).await? else {
        return Ok(false);
    };
    let serving = SERVING.contains(&window.state.as_str());
    let called = matches!(window.state.as_str(), "create_intent" | "create_unknown")
        && window.create_called_at.is_some();
    if !serving && !called {
        return Ok(false);
    }
    sqlx::query(
        "update cloud_windows set state = 'deleting', delete_requested_at = $2, delete_reason = $3,
                next_delete_at = case when $6 then null else $2 end,
                idle_since = null, disconnected_since = null,
                last_error = coalesce($4, last_error),
                stopped_at = case when $5 then coalesce(stopped_at, $2) else stopped_at end,
                server_gone_at = case when $6 then coalesce(server_gone_at, $2)
                                      else server_gone_at end
         where id = $1",
    )
    .bind(id)
    .bind(now)
    .bind(cleanup.reason)
    .bind(cleanup.last_error)
    .bind(cleanup.stopped)
    .bind(cleanup.server_gone)
    .execute(&mut *conn)
    .await?;
    if serving {
        // Прерывание — единственный отзыв, после которого задача может получить один повтор:
        // её выпустит тот, кто подтвердит остановку или отсутствие машины.
        let jobs = if cleanup.reason == REASON_PREEMPTED {
            crate::machines::RevokeJobs::RetryOnce
        } else {
            crate::machines::RevokeJobs::Lose
        };
        revoke_cloud_machine_in_tx(conn, jobs).await?;
    }
    // Команда подключения, которую несущая ещё не исполнила, больше не нужна.
    sqlx::query(
        "update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, now())
         where kind = 'attach_runner' and payload->>'window_id' = $1 and delivery_state <> 'completed'",
    )
    .bind(id.to_string())
    .execute(&mut *conn)
    .await?;
    // Отсутствие машины уже подтверждено полным списком каталога: прежний процесс идти не
    // может, и ждущие повтора задачи можно выпускать сразу.
    if cleanup.server_gone && cleanup.reason == REASON_PREEMPTED {
        release_cloud_retry_pending_in_tx(conn).await?;
    }
    Ok(true)
}

/// Полный список каталога подтвердил отсутствие машины прерванного окна: метка
/// `server_gone_at` и, в той же транзакции, выпуск ждущих повтора задач облачной машины.
/// Отсутствие в полном списке доказывает, что прежний процесс идти не может, — это и есть
/// «подтверждённая остановка или отсутствие».
///
/// Возвращает выпущенные задачи. Повторный вызов ничего не меняет: метка уже стоит, а выпуск
/// берёт только ждущие повтора строки.
pub async fn confirm_server_gone_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let Some(window) = load_for_update(conn, id).await? else {
        return Ok(Vec::new());
    };
    if window.server_gone_at.is_some() {
        return Ok(Vec::new());
    }
    let marked = sqlx::query(
        "update cloud_windows set server_gone_at = $2, next_delete_at = null
         where id = $1 and state in ('deleting', 'late_cleanup') and server_gone_at is null",
    )
    .bind(id)
    .bind(now)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if marked == 0 || window.delete_reason.as_deref() != Some(REASON_PREEMPTED) {
        return Ok(Vec::new());
    }
    release_cloud_retry_pending_in_tx(conn).await
}

/// Выпуск ждущих повтора задач облачной машины. Вызывается только там, где прежний процесс
/// уже не может идти: отсутствие машины подтверждено или окно сносится с той же мыслью.
pub async fn release_cloud_retry_pending_in_tx(
    conn: &mut PgConnection,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let Some(machine) = cloud_machine_id(&mut *conn).await? else {
        return Ok(Vec::new());
    };
    crate::inplace_jobs::retry::release_retry_pending_in_tx(conn, machine).await
}

/// Минимум до абсолютного предела родителя, при котором замена ещё успеет принести пользу.
pub const REPLACEMENT_MIN_LEAD: TimeDelta = TimeDelta::minutes(15);

/// Снос окна с причиной `preempted` закончен: машины и диска нет, окно в конечном состоянии.
/// Вызывается в транзакции, которая заканчивает окно (`CloudService::end`), под замком его
/// строки, уже после перевода в конечное состояние: место единственного живого окна свободно.
/// `window` — строка до перехода: в ней причина, сроки, предложение и вид машины.
///
/// Здесь окно-замена: то же предложение и повод, предел — абсолютный предел родителя,
/// свежий допуск по бюджету без согласия на перерасход. Отказ (бюджет, мало времени, нет
/// задач, место занято) — не ошибка: родитель всё равно заканчивается, владельцу уходит пуш.
/// Замену не заменяют: у окна с `replaces_window_id` второй замены не будет.
pub async fn on_preempted_cleanup_done_in_tx(
    conn: &mut PgConnection,
    config: &CloudConfig,
    window: &Window,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    // Снос машины мог подтвердиться раньше, чем окно закончилось, и тогда задачи уже
    // выпущены. Если же хаб упал между отзывом и отметкой, pending остались: выпускаем их
    // здесь, в транзакции конца. Выпуск идемпотентен.
    release_cloud_retry_pending_in_tx(conn).await?;
    if window.replaces_window_id.is_some() {
        enqueue_replacement_skipped(conn, window, "window_exists").await?;
        return Ok(());
    }
    let Some(deadline) = window.hard_cap_at else {
        enqueue_replacement_skipped(conn, window, "no_time").await?;
        return Ok(());
    };
    if deadline - now < REPLACEMENT_MIN_LEAD {
        enqueue_replacement_skipped(conn, window, "no_time").await?;
        return Ok(());
    }
    if !has_released_waiting(conn, window).await? {
        enqueue_replacement_skipped(conn, window, "no_waiting_job").await?;
        return Ok(());
    }
    // Минуты нужны только для резерва: сам предел замены пишется абсолютным ниже.
    let minutes = u32::try_from(((deadline - now).num_seconds() + 59) / 60)
        .unwrap_or(MAX_HARD_CAP_MINUTES)
        .min(MAX_HARD_CAP_MINUTES);
    if minutes == 0 {
        enqueue_replacement_skipped(conn, window, "no_time").await?;
        return Ok(());
    }
    let trigger = match window.trigger.as_str() {
        "card" => Trigger::Card,
        "owner" => Trigger::Owner,
        // Колонка под проверкой `trigger in ('card', 'owner', 'auto')`.
        _ => Trigger::Auto,
    };
    let pricing = Pricing {
        preemptible: window.preemptible,
        profiles: window.profiles(),
    };
    let request = CreateRequest {
        trigger,
        proposal_id: window.proposal_id,
        replaces_window_id: Some(window.id),
        hard_cap_minutes: minutes,
        // Замена не наследует согласие на перерасход: допуск спрашивается заново.
        consent_over_budget_kopecks: None,
    };
    // Вставка идёт под точкой сохранения: нарушение уникальности замены — не ошибка, а
    // «замена уже есть». Без неё транзакция конца окна стала бы непригодной.
    sqlx::query("savepoint cloud_replacement")
        .execute(&mut *conn)
        .await?;
    match create_in_tx(&mut *conn, config, pricing, request, None).await {
        Ok(id) => {
            sqlx::query("release savepoint cloud_replacement")
                .execute(&mut *conn)
                .await?;
            // Предел замены — абсолютный предел родителя, а не минуты от вызова создания.
            sqlx::query(
                "update cloud_windows set hard_cap_at = $2
                 where id = $1 and hard_cap_at is null",
            )
            .bind(id)
            .bind(deadline)
            .execute(&mut *conn)
            .await?;
            crate::notify::enqueue_cloud_alert_in_tx(
                conn,
                &format!("cloud:replacement_started:{id}"),
                &serde_json::json!({
                    "code": "replacement_started",
                    "window_id": id,
                    "parent_id": window.id,
                    "name": window.name,
                    "deadline": deadline.to_rfc3339(),
                }),
            )
            .await?;
        }
        Err(CreateRefusal::Database(error)) if unique_violation(&error) => {
            sqlx::query("rollback to savepoint cloud_replacement")
                .execute(&mut *conn)
                .await?;
            enqueue_replacement_skipped(conn, window, "window_exists").await?;
        }
        // Настоящая ошибка базы: транзакция конца окна не должна притворяться успешной.
        Err(CreateRefusal::Database(error)) => return Err(error),
        Err(refusal) => {
            sqlx::query("rollback to savepoint cloud_replacement")
                .execute(&mut *conn)
                .await?;
            enqueue_replacement_skipped(conn, window, replacement_skip_reason(&refusal)).await?;
        }
    }
    Ok(())
}

/// Есть ли ещё ждущая раннера задача, выпущенная из этого окна. Выпуск ставит
/// `placement_waiting_since`; окно создано раньше своего прерывания, поэтому задача,
/// выпущенная из него, ждёт позже его создания, а задача прежнего окна — раньше: новое окно
/// не создаётся, пока прежнее живое.
async fn has_released_waiting(
    conn: &mut PgConnection,
    window: &Window,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(select 1 from jobs
                        where state = 'waiting_runner' and preemption_retries > 0
                          and placement_waiting_since >= $1)",
    )
    .bind(window.created_at)
    .fetch_one(conn)
    .await
}

/// Уникальность замены: у окна не больше одной.
fn unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.code().as_deref() == Some("23505"))
}

/// Код причины отказа от замены для пуша. Русский текст — в `notify::cloud_alert_text`.
fn replacement_skip_reason(refusal: &CreateRefusal) -> &'static str {
    match refusal {
        CreateRefusal::OverBudget { .. } => "budget",
        CreateRefusal::LiveWindowExists | CreateRefusal::LateCleanup => "window_exists",
        CreateRefusal::AutoConditionsChanged => "auto_off",
        CreateRefusal::Database(_)
        | CreateRefusal::ProviderBlocked { .. }
        | CreateRefusal::CarrierUnavailable
        | CreateRefusal::Price(_)
        | CreateRefusal::Provider(_)
        | CreateRefusal::HostKey(_)
        | CreateRefusal::Invalid(_) => "unavailable",
    }
}

async fn enqueue_replacement_skipped(
    conn: &mut PgConnection,
    window: &Window,
    reason: &str,
) -> Result<(), sqlx::Error> {
    crate::notify::enqueue_cloud_alert_in_tx(
        conn,
        &format!("cloud:replacement_skipped:{}", window.id),
        &serde_json::json!({
            "code": "replacement_skipped",
            "window_id": window.id,
            "name": window.name,
            "reason": reason,
        }),
    )
    .await?;
    Ok(())
}

/// Строка машины `cloud`, если она уже заведена.
pub async fn cloud_machine_id(conn: &mut PgConnection) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("select cloud_machine_id from cloud_budget where singleton")
        .fetch_one(conn)
        .await
}

/// Отзыв раннера `cloud`: токен гаснет, его незаконченные задачи заканчиваются по `jobs`.
pub async fn revoke_cloud_machine(
    pool: &PgPool,
    jobs: crate::machines::RevokeJobs,
) -> Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;
    revoke_cloud_machine_in_tx(&mut conn, jobs).await
}

/// Отзыв в транзакции, которая переводит окно: только текущее живое окно владеет строкой.
pub async fn revoke_cloud_machine_in_tx(
    conn: &mut PgConnection,
    jobs: crate::machines::RevokeJobs,
) -> Result<(), sqlx::Error> {
    let machine = cloud_machine_id(&mut *conn).await?;
    if let Some(machine) = machine {
        crate::machines::revoke_jobs_in_tx(conn, machine, jobs).await?;
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

/// Расход окна на момент `now`: машина до подтверждённого исчезновения, диск до своего, у
/// ещё живых концом служит `now`. У окна, которому не звонили, 0. Ошибка счёта даёт резерв.
pub fn ledger_cost(window: &Window, now: DateTime<Utc>) -> i64 {
    window.create_called_at.map_or(0, |called| {
        price::ledger_kopecks(
            window.compute_kopecks_per_hour,
            window.disk_kopecks_per_hour,
            called,
            window.server_gone_at.unwrap_or(now),
            window.disk_gone_at.unwrap_or(now),
        )
        .unwrap_or(window.reservation_kopecks)
    })
}

/// Итог окна после сноса. Если за всё время не нашлось ни машины, ни диска, платить было не
/// за что: итог 0.
pub fn final_cost(window: &Window, now: DateTime<Utc>) -> i64 {
    if window.instance_id.is_none() && window.boot_disk_id.is_none() {
        0
    } else {
        ledger_cost(window, now)
    }
}

/// Окно для клиентов: деньги в копейках, время RFC 3339.
#[derive(Debug, Clone, Serialize, serde::Deserialize, ToSchema)]
pub struct CloudWindowView {
    pub id: Uuid,
    pub name: String,
    pub state: String,
    pub trigger: String,
    /// Номер машины у провайдера; пусто, пока машина не найдена.
    pub instance_id: Option<String>,
    /// Публичный адрес машины; пусто, пока он не выдан.
    pub public_ip: Option<String>,
    /// Прерываемая машина: Яндекс может остановить её в любой момент.
    pub preemptible: bool,
    /// Окно, которое это окно заменяет после прерывания.
    pub replaces_window_id: Option<Uuid>,
    /// Ставка часа машины с публичным адресом: платится, пока машина есть в каталоге.
    pub compute_kopecks_per_hour: i64,
    /// Ставка часа диска: платится, пока есть диск.
    pub disk_kopecks_per_hour: i64,
    /// Обе ставки вместе: до вызова создания у самого дорогого профиля, после — у того,
    /// которому звонили.
    pub price_hourly_kopecks: i64,
    pub reservation_kopecks: i64,
    /// Итог закончившегося окна; у живого пусто.
    pub final_cost_kopecks: Option<i64>,
    /// Оценка расхода живого окна по секундам с вызова создания, по каждой ставке отдельно.
    /// Оценка осторожная: остановленная машина считается по полной ставке.
    pub cost_so_far_kopecks: i64,
    pub month: String,
    pub created_at: DateTime<Utc>,
    pub ready_at: Option<DateTime<Utc>>,
    pub hard_cap_at: Option<DateTime<Utc>>,
    /// Машина замечена останавливающейся или остановленной.
    pub stopped_at: Option<DateTime<Utc>>,
    /// Диска машины больше нет в каталоге.
    pub disk_gone_at: Option<DateTime<Utc>>,
    pub delete_requested_at: Option<DateTime<Utc>>,
    pub delete_confirmed_at: Option<DateTime<Utc>>,
    pub delete_reason: Option<String>,
    pub last_error: Option<String>,
}

pub fn view(window: &Window, now: DateTime<Utc>) -> CloudWindowView {
    // Показываем настоящий расход, а не резерв: окно могло прожить дольше предела из-за
    // простоя хаба, и владелец должен видеть цену.
    let cost_so_far = window
        .final_cost_kopecks
        .unwrap_or_else(|| ledger_cost(window, now));
    CloudWindowView {
        id: window.id,
        name: window.name.clone(),
        state: window.state.clone(),
        trigger: window.trigger.clone(),
        instance_id: window.instance_id.clone(),
        public_ip: window.public_ip.clone(),
        preemptible: window.preemptible,
        replaces_window_id: window.replaces_window_id,
        compute_kopecks_per_hour: window.compute_kopecks_per_hour,
        disk_kopecks_per_hour: window.disk_kopecks_per_hour,
        price_hourly_kopecks: window.price_hourly_kopecks,
        reservation_kopecks: window.reservation_kopecks,
        final_cost_kopecks: window.final_cost_kopecks,
        cost_so_far_kopecks: cost_so_far,
        month: window.month.clone(),
        created_at: window.created_at,
        ready_at: window.ready_at,
        hard_cap_at: window.hard_cap_at,
        stopped_at: window.stopped_at,
        disk_gone_at: window.disk_gone_at,
        delete_requested_at: window.delete_requested_at,
        delete_confirmed_at: window.delete_confirmed_at,
        delete_reason: window.delete_reason.clone(),
        last_error: window.last_error.clone(),
    }
}

/// Живые окна, поздние сносы и последние `ended_limit` законченных, новые первыми.
pub async fn list(conn: &mut PgConnection, ended_limit: i64) -> Result<Vec<Window>, sqlx::Error> {
    sqlx::query_as(
        "(select * from cloud_windows where state = any($1))
         union all
         (select * from cloud_windows where state <> all($1) order by created_at desc, id desc limit $2)
         order by created_at desc, id desc",
    )
    .bind(billing_states())
    .bind(ended_limit)
    .fetch_all(conn)
    .await
}
