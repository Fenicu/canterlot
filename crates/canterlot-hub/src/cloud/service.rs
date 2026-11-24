//! Служба облачных окон: ведёт каждое живое окно по его состояниям и сверяет каталоги с базой.
//!
//! Один проход (`tick`) принимает «сейчас» параметром, поэтому сроки проверяются в тестах без
//! ожидания. Проходы идут строго по очереди (замок `runtime`), а в бою службу выполняет один
//! хаб за раз (`singleton::supervise`). Порядок прохода:
//! 1. сверка каталогов (первым делом при старте, до любого создания, и раз в
//!    `reconcile_interval`);
//! 2. шаг каждого живого окна по его состоянию.
//!
//! Правила денег, которые держит этот файл:
//! - вызов создания один, без повторов; неопределённый исход разбирается по списку каталога;
//! - список, не прочитанный целиком, ничего не меняет;
//! - после вызова создания окно заканчивается только через снос: сначала машина, потом её
//!   диск, и оба подтверждаются полными списками каталога окна;
//! - перед каждым удалением проверяется, что удаляется своё: номер, точное имя и каталог.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

use super::{
    CloudProvider, CreateInstance, CreateOutcome, DeleteOutcome, Disk, InstanceLookup,
    InstanceStatus, InstanceSummary, OperationState, YandexClient,
    cloud_init::{self, CloudInitParams},
    price,
    windows::{
        self, Cleanup, CreateRefusal, CreateRequest, PricedProfile, Pricing, Trigger, Window,
        WindowId, cloud_machine_id, window_id_from_name,
    },
};
use crate::{
    cloud::budget,
    config::{CLOUD_ROOT_DIR, CloudConfig},
    session::{Availability, Registry},
    session_tools::cloud_propose::{self, Source},
};

/// Как часто служба проходит по окнам.
const TICK: Duration = Duration::from_secs(10);
/// Раннер `cloud` отключён столько при подключённой несущей: окно удаляется.
const DISCONNECTED_AFTER: TimeDelta = TimeDelta::minutes(10);
/// Машину работающего окна служба опрашивает не чаще: так замечается прерывание.
const STATUS_POLL_EVERY: TimeDelta = TimeDelta::seconds(30);
/// После принятого DELETE машина считается удалённой, только когда её нет в полном списке.
/// Столько ждём подтверждения, прежде чем повторить DELETE.
const DELETE_CONFIRM_AFTER: TimeDelta = TimeDelta::minutes(10);
/// Деньги: повтор пуша о неудаляемой машине.
const ALERT_EVERY: TimeDelta = TimeDelta::minutes(30);
/// После стольких неудач удаления идут пуши.
const DELETE_FAILURES_BEFORE_ALERT: i32 = 3;
/// Цена профилей годна столько с момента создания окна. Перед первым POST более старая
/// цена перепроверяется у провайдера.
const PRICE_QUOTE_TTL: TimeDelta = TimeDelta::minutes(10);
/// Окно в `draining` задерживается сверх предела на доработку: так долго терпим, прежде чем
/// счесть это просрочкой, а не штатным сносом.
const OVERRUN_SLACK: TimeDelta = TimeDelta::minutes(10);
/// Сколько заявка без вызова создания ждёт образ у провайдера.
const IMAGE_WAIT: TimeDelta = TimeDelta::minutes(15);
/// Метка машины раннера у провайдера.
const LABEL_KEY: &str = "canterlot";
const LABEL_VALUE: &str = "runner";
const GIB: u64 = 1 << 30;

/// Лишний ресурс каталога, удаление которого принято, а отсутствие ещё не подтверждено.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stray {
    Instance { name: String },
    Disk { name: String },
}

/// Состояние службы между проходами; живёт только в памяти.
#[derive(Default)]
struct Runtime {
    /// Хотя бы одна сверка прошла по полному списку: до этого создавать нельзя.
    reconciled: bool,
    last_reconcile: Option<DateTime<Utc>>,
    /// Первый проход после старта хаба: найденная просрочка — это простой хаба.
    first_tick: bool,
    /// Когда машину окна опрашивали последний раз.
    status_polled: HashMap<Uuid, DateTime<Utc>>,
    /// Лишние машины и диски, которые удаляются: по номеру у провайдера.
    stray_pending: BTreeMap<String, Stray>,
}

pub struct CloudService {
    pool: PgPool,
    registry: Registry,
    provider: Arc<dyn CloudProvider>,
    config: CloudConfig,
    runtime: Mutex<Runtime>,
}

/// Принадлежность машины окну по прямому ответу провайдера: в нём есть номер, имя и каталог.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ownership {
    /// Машина с этим номером носит имя окна и лежит в его каталоге.
    Owned,
    /// Имя или каталог другие: номер достался чужой машине.
    Mismatch,
    /// Прямой ответ «нет такой», и полный список каталога без номера и без имени: машина
    /// подтверждённо пропала.
    Absent,
    /// Ответа нет или он противоречив: ничего не меняем.
    Unknown,
}

fn delete_backoff(attempts: i32) -> TimeDelta {
    let exponent = u32::try_from(attempts.saturating_sub(1))
        .unwrap_or(0)
        .min(5);
    TimeDelta::seconds((30_i64 << exponent).min(600))
}

/// Номер тридцатиминутного окна от запроса удаления: ключ повтора пуша.
fn alert_slot(window: &Window, now: DateTime<Utc>) -> i64 {
    let since = window.delete_requested_at.unwrap_or(now);
    (now - since).num_seconds().max(0) / ALERT_EVERY.num_seconds()
}

/// Срок наступил: пустая метка значит «сейчас».
fn due(at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    at.is_none_or(|at| at <= now)
}

/// Машина окна в списке каталога: по номеру или по точному имени.
fn listed<'a>(listing: &'a [InstanceSummary], window: &Window) -> Option<&'a InstanceSummary> {
    listing.iter().find(|instance| {
        window.instance_id.as_deref() == Some(instance.id.as_str()) || instance.name == window.name
    })
}

impl CloudService {
    pub fn new(
        pool: PgPool,
        registry: Registry,
        provider: Arc<dyn CloudProvider>,
        config: CloudConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            pool,
            registry,
            provider,
            config,
            runtime: Mutex::new(Runtime {
                first_tick: true,
                ..Runtime::default()
            }),
        })
    }

    pub fn config(&self) -> &CloudConfig {
        &self.config
    }

    /// Две ставки часа каждого профиля списка по ценам провайдера, в порядке списка, для
    /// вида машины из конфигурации. Цену любого профиля получить нельзя — отказ целиком:
    /// резерв без цены не посчитать, а вызывать создание без резерва нельзя.
    pub async fn pricing(&self) -> Result<Pricing, CreateRefusal> {
        self.pricing_for(self.config.preemptible).await
    }

    async fn pricing_for(&self, preemptible: bool) -> Result<Pricing, CreateRefusal> {
        let mut profiles = Vec::new();
        for profile in &self.config.profile {
            let price = self
                .provider
                .price(profile, preemptible)
                .await
                .map_err(|error| {
                    tracing::warn!(
                        kind = "cloud_profile_unpriced",
                        cores = profile.cores,
                        memory_gb = profile.memory_gb,
                        error = %error,
                        "no price from the provider"
                    );
                    CreateRefusal::Provider(error)
                })?;
            profiles.push(PricedProfile {
                cores: profile.cores,
                memory_gb: profile.memory_gb,
                disk_gb: profile.disk_gb,
                compute_kopecks_per_hour: price.compute_kopecks_per_hour,
                disk_kopecks_per_hour: price.disk_kopecks_per_hour,
            });
        }
        if profiles.is_empty() {
            return Err(CreateRefusal::Invalid("profiles"));
        }
        Ok(Pricing {
            preemptible,
            profiles,
        })
    }

    /// Создание окна из любого входа: цена у провайдера, затем одна транзакция. Вызов
    /// провайдера для самой машины делает служба на ближайшем проходе.
    pub async fn create(&self, request: CreateRequest) -> Result<WindowId, CreateRefusal> {
        let pricing = self.pricing().await?;
        let config = self.config.clone();
        let registry = self.registry.clone();
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move {
                windows::create_in_tx(
                    conn,
                    &config,
                    pricing,
                    request,
                    Some(windows::AutoGuard {
                        registry: &registry,
                        now: Utc::now(),
                    }),
                )
                .await
            })
        })
        .await
    }

    /// Цикл службы: проход каждые `TICK`. Один хаб за раз.
    pub async fn run(self: Arc<Self>, shutdown: watch::Receiver<bool>) {
        let pool = self.pool.clone();
        crate::singleton::supervise(&pool, "cloud_windows", shutdown, move || {
            let service = self.clone();
            async move {
                let mut tick = tokio::time::interval(TICK);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tick.tick().await;
                    service.tick(Utc::now()).await;
                }
            }
        })
        .await;
    }

    /// Один проход. Ошибка одного окна или одного вызова не мешает остальным.
    pub async fn tick(&self, now: DateTime<Utc>) {
        let mut runtime = self.runtime.lock().await;
        let due = runtime.last_reconcile.is_none_or(|at| {
            now - at
                >= TimeDelta::from_std(self.config.reconcile_interval)
                    .unwrap_or(TimeDelta::minutes(5))
        });
        if due {
            runtime.last_reconcile = Some(now);
            match self.reconcile(&mut runtime, now).await {
                Ok(true) => runtime.reconciled = true,
                // Удаление ждёт подтверждения: новые создания закрыты, но следующую сверку
                // ждём обычный срок, а не каждый проход.
                Ok(false) => runtime.reconciled = false,
                Err(()) => {
                    // Следующий проход попробует снова: пока сверки нет, новых машин нет.
                    runtime.reconciled = false;
                    runtime.last_reconcile = None;
                }
            }
        }
        if runtime.first_tick
            && let Err(error) = budget::reset_unavailable(&self.pool).await
        {
            warn_db("cloud_auto_reset", &error);
        }
        if runtime.reconciled
            && let Err(error) = self.evaluate(now).await
        {
            warn_db("cloud_auto", &error);
        }
        let windows = match self.live_windows().await {
            Ok(windows) => windows,
            Err(error) => {
                warn_db("cloud_windows_load", &error);
                return;
            }
        };
        let live: HashSet<Uuid> = windows.iter().map(|window| window.id).collect();
        runtime.status_polled.retain(|id, _| live.contains(id));
        for window in windows {
            if let Err(error) = self.step(&mut runtime, window, now).await {
                warn_db("cloud_window_step", &error);
            }
        }
        runtime.first_tick = false;
    }

    async fn live_windows(&self) -> Result<Vec<Window>, sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        windows::live(&mut conn).await
    }

    /// Один проход автоматического режима, карточки хаба и денежных оповещений. Все условия
    /// проверяются под обычными чтениями; создание идёт через ту же транзакцию `create_in_tx`.
    async fn evaluate(&self, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        let (budget_kopecks, auto_mode, block) = budget::settings(&mut conn).await?;
        drop(conn);
        if let Err(error) = budget::maybe_alert(&self.pool, now, budget_kopecks).await {
            warn_db("cloud_budget_alert", &error);
        }

        let mut conn = self.pool.acquire().await?;
        let waiting = budget::waiting_cloud_minutes(&mut conn, &self.config).await?;
        if waiting < budget::MIN_WAITING_MINUTES {
            if budget::episode_active(&mut conn).await? {
                budget::episode_end(&mut conn).await?;
            }
            return Ok(());
        }
        if budget::has_live_window(&mut conn).await? || budget::has_open_card(&mut conn).await? {
            return Ok(());
        }
        let pricing = match self.pricing().await {
            Ok(pricing) => pricing,
            Err(error) => {
                tracing::warn!(
                    kind = "cloud_auto_pricing",
                    error = %error,
                    "no price from the provider; automatic start skipped"
                );
                return Ok(());
            }
        };
        let Some(max_hourly) = pricing.max_hourly_kopecks() else {
            return Ok(());
        };
        let estimate = u32::try_from(waiting.saturating_add(30)).unwrap_or(u32::MAX);
        let cap = budget::AUTO_MAX_CAP_MINUTES.min(estimate);
        let reservation = match windows::reservation_for(&self.config, max_hourly, cap) {
            Ok(reservation) => reservation,
            Err(error) => {
                tracing::warn!(kind = "cloud_auto_price", error = %error, "cannot reserve the window");
                return Ok(());
            }
        };
        let remaining = budget::remaining_in_month(&mut conn).await?;
        let fits = reservation <= remaining;

        if auto_mode
            && budget_kopecks > 0
            && block.is_none()
            && fits
            && self.all_static_runners_down(&mut conn, now).await?
        {
            drop(conn);
            let config = self.config.clone();
            let registry = self.registry.clone();
            let created = crate::db::in_tx(&self.pool, move |tx| {
                Box::pin(async move {
                    windows::create_in_tx(
                        tx,
                        &config,
                        pricing,
                        CreateRequest {
                            trigger: Trigger::Auto,
                            proposal_id: None,
                            hard_cap_minutes: cap,
                            consent_over_budget_kopecks: None,
                        },
                        Some(windows::AutoGuard {
                            registry: &registry,
                            now,
                        }),
                    )
                    .await
                })
            })
            .await;
            match created {
                Ok(window_id) => {
                    if let Err(error) = crate::notify::enqueue_cloud_alert(
                        &self.pool,
                        &format!("cloud:auto_started:{window_id}"),
                        &json!({
                            "code": "auto_started",
                            "window_id": window_id,
                            "reservation_kopecks": reservation,
                        }),
                    )
                    .await
                    {
                        warn_db("cloud_auto_push", &error);
                    }
                }
                Err(refusal) => {
                    tracing::warn!(
                        kind = "cloud_auto_create",
                        refusal = %refusal,
                        "automatic window was not created"
                    );
                }
            }
            return Ok(());
        }

        // Автоматика выключена или резерв не помещается: карточка хаба один раз за эпизод.
        if (!auto_mode || !fits) && !budget::episode_active(&mut conn).await? {
            let reason = format!(
                "Ожидающая облачная работа: {waiting} мин по оценке; автоматический режим {}",
                if auto_mode {
                    "включён, но резерв не помещается в бюджет"
                } else {
                    "выключен"
                }
            );
            let config = self.config.clone();
            let carrier = self.config.carrier_machine_id;
            drop(conn);
            // Запрос, задание его срока и признак эпизода — одна транзакция: сбой на любом
            // шаге не оставляет половину карточки и не теряет эпизод.
            let opened: Result<cloud_propose::Opened, cloud_propose::CardError> =
                crate::db::in_tx(&self.pool, move |tx| {
                    Box::pin(async move {
                        let owner: Uuid =
                            sqlx::query_scalar("select owner_id from machines where id = $1")
                                .bind(carrier)
                                .fetch_one(&mut *tx)
                                .await?;
                        let mut proposer =
                            cloud_propose::system_proposer(tx, owner, carrier).await?;
                        proposer.source = Source::Hub;
                        let opened = cloud_propose::open_in_tx(
                            tx, &config, &pricing, &proposer, cap, reason,
                        )
                        .await?;
                        budget::episode_start(tx, opened.request_id).await?;
                        Ok(opened)
                    })
                })
                .await;
            match opened {
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(kind = "cloud_hub_card", error = %error, "card not opened");
                }
            }
        }
        Ok(())
    }

    /// Все статичные раннеры, которые могли бы взять ожидающую работу, недоступны или на
    /// паузе непрерывно десять минут. `unknown` не считается недоступным и сбрасывает период.
    async fn all_static_runners_down(
        &self,
        conn: &mut sqlx::PgConnection,
        now: DateTime<Utc>,
    ) -> Result<bool, sqlx::Error> {
        budget::static_runners_down(conn, &self.registry, &self.config, now).await
    }

    fn connected(&self, machine: Uuid) -> bool {
        self.registry.availability(machine) != Availability::Unavailable
    }

    async fn step(
        &self,
        runtime: &mut Runtime,
        window: Window,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        if matches!(window.state.as_str(), "identified" | "ready" | "draining")
            && self.overrun(runtime, &window, now).await?
        {
            return Ok(());
        }
        match window.state.as_str() {
            "create_intent" => self.step_create_intent(runtime, window, now).await,
            "identified" => self.step_identified(window, now).await,
            "ready" => self.step_ready(runtime, window, now).await,
            "draining" => self.step_draining(runtime, window, now).await,
            // Поздний снос машины или диска закрытого окна идёт тем же шагом.
            "deleting" | windows::LATE_CLEANUP => self.step_deleting(window, now).await,
            // `create_unknown` разбирает только сверка по полному списку.
            _ => Ok(()),
        }
    }

    // ───────── Создание ─────────

    async fn step_create_intent(
        &self,
        runtime: &mut Runtime,
        window: Window,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        if window.create_called_at.is_some() {
            // Вызов создания начат, а ответ не записан (хаб упал или проход оборвали): машина
            // могла появиться. Второго вызова не будет, имя найдёт сверка.
            sqlx::query(
                "update cloud_windows set state = 'create_unknown', last_error = 'interrupted'
                 where id = $1 and state = 'create_intent'",
            )
            .bind(window.id)
            .execute(&self.pool)
            .await?;
            return Ok(());
        }
        if !runtime.reconciled {
            return Ok(());
        }
        self.start_create(window, now).await
    }

    /// Заявка заканчивается до вызова создания: машины нет и быть не могло.
    async fn fail(&self, id: Uuid, reason: &str) -> Result<(), sqlx::Error> {
        let reason = reason.to_owned();
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move { windows::fail_in_tx(conn, id, &reason).await })
        })
        .await?;
        Ok(())
    }

    /// Сроки окна из его строки, а не из текущей конфигурации.
    fn drain_minutes(&self, window: &Window) -> u64 {
        window
            .drain_grace_minutes
            .and_then(|minutes| u64::try_from(minutes).ok())
            .unwrap_or_else(|| self.config.drain_grace_minutes())
    }

    fn teardown_minutes(&self, window: &Window) -> u64 {
        window
            .teardown_margin_minutes
            .and_then(|minutes| u64::try_from(minutes).ok())
            .unwrap_or_else(|| self.config.teardown_margin_minutes())
    }

    /// Создание не дало рабочей машины. Вызов уже был, поэтому окно уходит на снос: машину и
    /// диск с его именем надо найти или подтвердить их отсутствие, и только потом окно
    /// закончится `failed`.
    async fn fail_after_call(
        &self,
        id: Uuid,
        detail: &str,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let detail = detail.to_owned();
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move {
                windows::begin_cleanup_in_tx(
                    conn,
                    id,
                    Cleanup {
                        last_error: Some(&detail),
                        ..Cleanup::because(windows::REASON_CREATE_FAILED)
                    },
                    now,
                )
                .await
            })
        })
        .await?;
        Ok(())
    }

    /// Перед первым POST: если снимок цен старше срока, спросить провайдера заново. Рост
    /// резерва выше одобренного закрывает окно без POST и без расхода; снижение заменяет
    /// снимок и освобождает разницу в бюджете.
    async fn reprice_if_stale(
        &self,
        window: &mut Window,
        now: DateTime<Utc>,
    ) -> Result<bool, sqlx::Error> {
        if now - window.created_at < PRICE_QUOTE_TTL {
            return Ok(true);
        }
        // Вид машины окна уже решён при его создании: цена спрашивается под него.
        let pricing = match self.pricing_for(window.preemptible).await {
            Ok(pricing) => pricing,
            Err(error) => {
                tracing::warn!(kind = "cloud_reprice", error = %error, "price quote is stale and cannot be refreshed");
                return Ok(false);
            }
        };
        let Some(top) = pricing.most_expensive().cloned() else {
            return self.fail(window.id, "price_error").await.map(|_| false);
        };
        let max_hourly = top.hourly_kopecks();
        let hard_cap = u32::try_from(window.hard_cap_minutes).unwrap_or(u32::MAX);
        let reservation = match windows::reservation_for_terms(
            max_hourly,
            hard_cap,
            self.drain_minutes(window),
            self.teardown_minutes(window),
        ) {
            Ok(reservation) => reservation,
            Err(error) => {
                return self
                    .fail(window.id, &format!("price_error:{error}"))
                    .await
                    .map(|_| false);
            }
        };
        if reservation > window.reservation_kopecks {
            crate::notify::enqueue_cloud_alert(
                &self.pool,
                &format!("cloud:reprice_growth:{}", window.id),
                &json!({
                    "code": "reprice_growth",
                    "window_id": window.id,
                    "name": window.name,
                    "reservation_kopecks": reservation,
                    "approved_kopecks": window.reservation_kopecks,
                }),
            )
            .await?;
            return self.fail(window.id, "reprice_growth").await.map(|_| false);
        }
        let profiles = serde_json::to_value(&pricing.profiles).expect("profiles serialize");
        let updated = sqlx::query(
            "update cloud_windows set profiles = $2, price_hourly_kopecks = $3,
                    reservation_kopecks = $4, compute_kopecks_per_hour = $5,
                    disk_kopecks_per_hour = $6
             where id = $1 and state = 'create_intent' and create_called_at is null
               and reservation_kopecks >= $4",
        )
        .bind(window.id)
        .bind(&profiles)
        .bind(max_hourly)
        .bind(reservation)
        .bind(top.compute_kopecks_per_hour)
        .bind(top.disk_kopecks_per_hour)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated == 0 {
            return Ok(false);
        }
        window.profiles = profiles;
        window.price_hourly_kopecks = max_hourly;
        window.compute_kopecks_per_hour = top.compute_kopecks_per_hour;
        window.disk_kopecks_per_hour = top.disk_kopecks_per_hour;
        window.reservation_kopecks = reservation;
        Ok(true)
    }

    async fn start_create(
        &self,
        mut window: Window,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        if !self.reprice_if_stale(&mut window, now).await? {
            return Ok(());
        }
        // Образ спрашивается до метки вызова: пока его нет, машины тоже нет, и окно может
        // подождать или закончиться без расхода.
        let image_id = match self.provider.latest_image(&self.config.image_family).await {
            Ok(image_id) => image_id,
            Err(error) => {
                tracing::warn!(kind = "cloud_image", error_kind = ?error.kind, "boot image unavailable");
                if !error.retryable() || now - window.created_at > IMAGE_WAIT {
                    self.fail(window.id, "image_unavailable").await?;
                }
                return Ok(());
            }
        };
        let Ok((key, regenerated)) = windows::take_or_generate(window.id) else {
            return self.fail(window.id, "host_key_unavailable").await;
        };
        if regenerated {
            sqlx::query("update cloud_windows set host_key_public = $2 where id = $1")
                .bind(window.id)
                .bind(key.public_line())
                .execute(&self.pool)
                .await?;
        }
        let rendered = cloud_init::render(&CloudInitParams {
            host_key_ed25519_private: key.private_openssh().to_owned(),
            host_key_ed25519_public: key.public_line().to_owned(),
            runner_authorized_key: self.config.runner_public_key.clone(),
            root_dir: CLOUD_ROOT_DIR.to_owned(),
        });
        let Ok(user_data) = rendered else {
            return self.fail(window.id, "cloud_init_invalid").await;
        };
        // Метка вызова ставится до самого вызова: от неё считаются предел и ledger, и по ней
        // упавший после этого хаб поймёт, что машина могла появиться.
        let hard_cap = TimeDelta::minutes(i64::from(window.hard_cap_minutes));
        let drain =
            TimeDelta::minutes(i64::try_from(self.drain_minutes(&window)).unwrap_or(i64::MAX));
        let teardown =
            TimeDelta::minutes(i64::try_from(self.teardown_minutes(&window)).unwrap_or(i64::MAX));
        let drain_until = now + hard_cap + drain;
        let stamped = sqlx::query(
            "update cloud_windows set create_called_at = $2, hard_cap_at = $3, drain_until = $4,
                    teardown_until = $5
             where id = $1 and state = 'create_intent' and create_called_at is null",
        )
        .bind(window.id)
        .bind(now)
        .bind(now + hard_cap)
        .bind(drain_until)
        .bind(drain_until + teardown)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if stamped == 0 {
            return Ok(());
        }
        // Профили пробуются по очереди внутри одной заявки. Перед каждым вызовом в строке
        // стоят размеры и обе ставки именно этого профиля: если ответ потеряется, машина
        // могла появиться только у него, и ledger считает его цену.
        let profiles = window.profiles();
        let mut refusals: Vec<String> = Vec::new();
        for profile in &profiles {
            let marked = sqlx::query(
                "update cloud_windows set cores = $2, memory_gb = $3, disk_gb = $4,
                        compute_kopecks_per_hour = $5, disk_kopecks_per_hour = $6,
                        price_hourly_kopecks = $7
                 where id = $1 and state = 'create_intent'",
            )
            .bind(window.id)
            .bind(i32::try_from(profile.cores).unwrap_or(i32::MAX))
            .bind(i32::try_from(profile.memory_gb).unwrap_or(i32::MAX))
            .bind(i32::try_from(profile.disk_gb).unwrap_or(i32::MAX))
            .bind(profile.compute_kopecks_per_hour)
            .bind(profile.disk_kopecks_per_hour)
            .bind(profile.hourly_kopecks())
            .execute(&self.pool)
            .await?
            .rows_affected();
            if marked == 0 {
                return Ok(());
            }
            let request = CreateInstance {
                name: window.name.clone(),
                labels: BTreeMap::from([(LABEL_KEY.to_owned(), LABEL_VALUE.to_owned())]),
                cores: profile.cores,
                memory_bytes: u64::from(profile.memory_gb) * GIB,
                disk_bytes: u64::from(profile.disk_gb) * GIB,
                // Вид машины — из снимка окна, а не из конфигурации на момент вызова.
                preemptible: window.preemptible,
                image_id: image_id.clone(),
                user_data: user_data.clone(),
            };
            let outcome = self.provider.create(&window.folder_id, &request).await;
            drop(request);
            match outcome {
                CreateOutcome::Accepted {
                    instance_id,
                    operation_id,
                } => {
                    return self
                        .identify(
                            window.id,
                            &instance_id,
                            Some(&operation_id),
                            "create_intent",
                        )
                        .await;
                }
                CreateOutcome::Rejected {
                    http,
                    grpc_code,
                    next_profile,
                    ..
                } => {
                    let grpc = grpc_code.map_or_else(|| "-".to_owned(), |code| code.to_string());
                    refusals.push(format!(
                        "{}x{}:{http}/{grpc}",
                        profile.cores, profile.memory_gb
                    ));
                    // Машины точно нет. Отказ по квоте или месту «обходится» следующим
                    // профилем, любой другой закрывает окно.
                    if !next_profile {
                        return self
                            .fail_after_call(window.id, &refusals.join(","), now)
                            .await;
                    }
                }
                // Машина могла появиться: второго вызова, на другом профиле тоже, не будет.
                CreateOutcome::Ambiguous { .. } => {
                    sqlx::query(
                        "update cloud_windows set state = 'create_unknown', last_error = 'ambiguous'
                         where id = $1 and state = 'create_intent'",
                    )
                    .bind(window.id)
                    .execute(&self.pool)
                    .await?;
                    return Ok(());
                }
            }
        }
        // Все профили отказали определённо.
        let reason = if refusals.is_empty() {
            "no_profiles".to_owned()
        } else {
            format!("all_profiles_rejected: {}", refusals.join(","))
        };
        self.fail_after_call(window.id, &reason, now).await
    }

    /// Машина найдена: окно становится `identified`, строка раннера `cloud` готова. Операции
    /// создания нет, когда машину нашли по имени в списке.
    async fn identify(
        &self,
        id: Uuid,
        instance_id: &str,
        operation_id: Option<&str>,
        from: &'static str,
    ) -> Result<(), sqlx::Error> {
        let config = self.config.clone();
        let instance_id = instance_id.to_owned();
        let operation_id = operation_id.map(str::to_owned);
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move {
                windows::ensure_cloud_machine(conn, &config, true).await?;
                sqlx::query(
                    "update cloud_windows set state = 'identified', instance_id = $2,
                            create_operation_id = $3, last_error = null
                     where id = $1 and state = $4",
                )
                .bind(id)
                .bind(&instance_id)
                .bind(&operation_id)
                .bind(from)
                .execute(&mut *conn)
                .await?;
                Ok::<_, sqlx::Error>(())
            })
        })
        .await
    }

    // ───────── Готовность и подключение ─────────

    async fn step_identified(&self, window: Window, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        let machine = cloud_machine_id(&mut conn).await?;
        drop(conn);
        if window.attach_sent_at.is_some() && machine.is_some_and(|id| self.connected(id)) {
            return self.mark_ready(&window, now).await;
        }
        if let Some(cap) = window.hard_cap_at
            && now >= cap
        {
            crate::notify::enqueue_cloud_alert(
                &self.pool,
                &format!("cloud:hard_cap:{}", window.id),
                &json!({"code": "hard_cap", "window_id": window.id, "name": window.name}),
            )
            .await?;
            return self.delete_now(&window, "hard_cap", now).await;
        }
        if let Some(called) = window.create_called_at
            && now - called >= grace(self.config.ready_timeout)
        {
            return self.delete_now(&window, "ready_timeout", now).await;
        }
        let Some(instance_id) = window.instance_id.as_deref() else {
            return Ok(());
        };
        // Команда подключения уже ушла: дальше ждём раннера.
        if window.attach_sent_at.is_some() {
            return Ok(());
        }
        // Пока операция создания идёт, машину не трогаем. Её ошибка окончательна: Яндекс
        // откатил создание, но машину и диск всё равно подтверждает снос. Операции нет или
        // провайдер её не знает — машина проверяется прямым ответом.
        if let Some(operation_id) = window.create_operation_id.as_deref() {
            match self.provider.operation(operation_id).await {
                Ok(OperationState::Pending) => return Ok(()),
                Ok(OperationState::Failed { grpc_code, .. }) => {
                    tracing::warn!(
                        kind = "cloud_create_failed",
                        grpc_code,
                        "create operation failed"
                    );
                    return self
                        .fail_after_call(window.id, &format!("create_failed:{grpc_code}"), now)
                        .await;
                }
                Ok(OperationState::Done | OperationState::Unknown) => {}
                Err(error) => {
                    tracing::warn!(kind = "cloud_poll_operation", error_kind = ?error.kind, "create operation poll failed");
                    return Ok(());
                }
            }
        }
        let instance = match self.provider.get(instance_id).await {
            Ok(InstanceLookup::Found(instance)) => instance,
            // Точечное «нет такой» отсутствия не доказывает: его разберёт сверка по списку.
            Ok(InstanceLookup::NotFound) => return Ok(()),
            Err(error) => {
                tracing::warn!(kind = "cloud_poll_instance", error_kind = ?error.kind, "instance poll failed");
                return Ok(());
            }
        };
        if instance.name != window.name || instance.folder_id != window.folder_id {
            // Номер достался чужой машине: подключаться к ней нельзя. Окно дождётся срока
            // готовности, а удаление остановится на той же проверке и сообщит владельцу.
            sqlx::query(
                "update cloud_windows set last_error = 'ownership_mismatch'
                 where id = $1 and state = 'identified'",
            )
            .bind(window.id)
            .execute(&self.pool)
            .await?;
            return Ok(());
        }
        match instance.status {
            InstanceStatus::Running => {
                if let Some(public_ip) = instance.public_ip.as_deref() {
                    self.send_attach(&window, public_ip, instance.boot_disk_id.as_deref(), now)
                        .await?;
                }
            }
            InstanceStatus::Stopping | InstanceStatus::Stopped => {
                self.stopped(&window, false, now).await?;
            }
            InstanceStatus::Broken => self.stopped(&window, true, now).await?,
            InstanceStatus::Provisioning | InstanceStatus::Deleting | InstanceStatus::Other(_) => {}
        }
        Ok(())
    }

    /// Команда несущей подключить раннер по публичному адресу машины, порт 22. В той же
    /// транзакции окно запоминает адрес и загрузочный диск. Только если машина из
    /// конфигурации действительно несущая: иначе окно дождётся `ready_timeout` и уйдёт.
    async fn send_attach(
        &self,
        window: &Window,
        public_ip: &str,
        boot_disk_id: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let config = self.config.clone();
        let window = window.clone();
        let public_ip = public_ip.to_owned();
        let boot_disk_id = boot_disk_id.map(str::to_owned);
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move {
                sqlx::query(
                    "update cloud_windows set public_ip = $2,
                            boot_disk_id = coalesce($3, boot_disk_id)
                     where id = $1 and state = 'identified'",
                )
                .bind(window.id)
                .bind(&public_ip)
                .bind(&boot_disk_id)
                .execute(&mut *conn)
                .await?;
                let carrier: Option<bool> = sqlx::query_scalar(
                    "select role = 'worker' and is_carrier from machines where id = $1 for share",
                )
                .bind(config.carrier_machine_id)
                .fetch_optional(&mut *conn)
                .await?;
                if carrier != Some(true) {
                    sqlx::query(
                        "update cloud_windows set last_error = 'carrier_unavailable' where id = $1",
                    )
                    .bind(window.id)
                    .execute(&mut *conn)
                    .await?;
                    return Ok::<_, sqlx::Error>(());
                }
                let machine = cloud_machine_id(&mut *conn).await?;
                let Some(machine) = machine else {
                    return Ok(());
                };
                sqlx::query(
                    "insert into commands (id, machine_id, conversation_id, execution_id,
                                           execution_generation, kind, payload, delivery_state)
                     values ($1, $2, null, null, null, 'attach_runner', $3, 'pending')",
                )
                .bind(Uuid::now_v7())
                .bind(config.carrier_machine_id)
                .bind(json!({
                    "window_id": window.id,
                    "machine_id": machine,
                    "host": public_ip,
                    "ssh_port": 22,
                    "host_key_public": window.host_key_public,
                    "root_dir": CLOUD_ROOT_DIR,
                    "max_running": config.max_running,
                    "lease_slots": config.lease_slots,
                }))
                .execute(&mut *conn)
                .await?;
                sqlx::query(
                    "update cloud_windows set attach_sent_at = $2, last_error = null where id = $1",
                )
                .bind(window.id)
                .bind(now)
                .execute(&mut *conn)
                .await?;
                sqlx::query("select pg_notify('canterlot_outbox', $1)")
                    .bind(config.carrier_machine_id.to_string())
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
    }

    async fn mark_ready(&self, window: &Window, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        sqlx::query(
            "update cloud_windows set state = 'ready', ready_at = $2, last_error = null
             where id = $1 and state = 'identified'",
        )
        .bind(window.id)
        .bind(now)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, now())
             where kind = 'attach_runner' and payload->>'window_id' = $1 and delivery_state <> 'completed'",
        )
        .bind(window.id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ───────── Остановка машины ─────────

    /// Машина остановилась не по нашей воле. Прерываемую остановил Яндекс (`preempted`);
    /// обычная так не останавливается, а `ERROR` и `CRASHED` — поломка в любом случае
    /// (`vm_broken`). Окно уходит на снос, раннер отзывается в той же транзакции, владельцу
    /// пуш. Задачи раннера при этом становятся `lost`.
    async fn stopped(
        &self,
        window: &Window,
        broken: bool,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let reason = if window.preemptible && !broken {
            windows::REASON_PREEMPTED
        } else {
            windows::REASON_VM_BROKEN
        };
        let id = window.id;
        let name = window.name.clone();
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move {
                let moved = windows::begin_cleanup_in_tx(
                    conn,
                    id,
                    Cleanup {
                        stopped: !broken,
                        ..Cleanup::because(reason)
                    },
                    now,
                )
                .await?;
                if moved {
                    crate::notify::enqueue_cloud_alert_in_tx(
                        conn,
                        &format!("cloud:{reason}:{id}"),
                        &json!({"code": reason, "window_id": id, "name": name}),
                    )
                    .await?;
                }
                Ok::<_, sqlx::Error>(())
            })
        })
        .await
    }

    /// Опрос машины работающего окна, не чаще раза в `STATUS_POLL_EVERY`. `true`: окно ушло
    /// на снос, шаг закончен.
    async fn poll_instance(
        &self,
        runtime: &mut Runtime,
        window: &Window,
        now: DateTime<Utc>,
    ) -> Result<bool, sqlx::Error> {
        let Some(instance_id) = window.instance_id.as_deref() else {
            return Ok(false);
        };
        if runtime
            .status_polled
            .get(&window.id)
            .is_some_and(|at| now - *at < STATUS_POLL_EVERY)
        {
            return Ok(false);
        }
        runtime.status_polled.insert(window.id, now);
        match self.provider.get(instance_id).await {
            Ok(InstanceLookup::Found(instance)) => match instance.status {
                InstanceStatus::Stopping | InstanceStatus::Stopped => {
                    self.stopped(window, false, now).await?;
                    Ok(true)
                }
                InstanceStatus::Broken => {
                    self.stopped(window, true, now).await?;
                    Ok(true)
                }
                _ => Ok(false),
            },
            // Машины нет: это подтверждает только полный список каталога окна. Окно не
            // заканчивается сразу: сносу остаётся подтвердить диск.
            Ok(InstanceLookup::NotFound) => {
                match self.provider.list_instances(&window.folder_id).await {
                    Ok(listing) if listed(&listing, window).is_none() => {
                        self.gone(window, now).await?;
                        Ok(true)
                    }
                    Ok(_) => Ok(false),
                    Err(error) => {
                        tracing::warn!(kind = "cloud_poll_listing", error = %error, "listing failed; nothing changed");
                        Ok(false)
                    }
                }
            }
            Err(error) => {
                tracing::warn!(kind = "cloud_poll_instance", error_kind = ?error.kind, "instance poll failed");
                Ok(false)
            }
        }
    }

    // ───────── Работа окна ─────────

    /// Есть ли у раннера `cloud` работа: задачи на нём или ждущая задача, которую он взял бы
    /// по тому же правилу размещения: метки, вид места `cloud` и названная машина.
    async fn has_work(&self, machine: Uuid) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            "select exists(select 1 from jobs where machine_id = $1
                           and state in ('staging','queued','dispatched','waiting_lease','running','collecting'))
                 or exists(select 1 from jobs where state = 'waiting_runner' and carrier_id = $2
                           and labels <@ $3 and 'cloud' = any(placement)
                           and (requested_machine_id is null or requested_machine_id = $1))",
        )
        .bind(machine)
        .bind(self.config.carrier_machine_id)
        .bind(&self.config.labels)
        .fetch_one(&self.pool)
        .await
    }

    async fn step_ready(
        &self,
        runtime: &mut Runtime,
        window: Window,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        if self.poll_instance(runtime, &window, now).await? {
            return Ok(());
        }
        if let Some(cap) = window.hard_cap_at
            && now >= cap
        {
            return self.start_draining(&window).await;
        }
        let mut conn = self.pool.acquire().await?;
        let machine = cloud_machine_id(&mut conn).await?;
        drop(conn);
        let Some(machine) = machine else {
            return Ok(());
        };
        // Отключённый раннер: считается, только пока несущая на связи.
        if self.connected(machine) || !self.connected(self.config.carrier_machine_id) {
            if window.disconnected_since.is_some() {
                self.set_timer(window.id, "disconnected_since", None)
                    .await?;
            }
        } else {
            match window.disconnected_since {
                None => {
                    self.set_timer(window.id, "disconnected_since", Some(now))
                        .await?
                }
                Some(since) if now - since >= DISCONNECTED_AFTER => {
                    return self.delete_now(&window, "disconnected", now).await;
                }
                Some(_) => {}
            }
        }
        // Простой.
        if self.has_work(machine).await? {
            if window.idle_since.is_some() {
                self.set_timer(window.id, "idle_since", None).await?;
            }
        } else {
            match window.idle_since {
                None => self.set_timer(window.id, "idle_since", Some(now)).await?,
                Some(since)
                    if now - since >= TimeDelta::minutes(i64::from(self.config.idle_minutes)) =>
                {
                    return self.delete_now(&window, "idle", now).await;
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    async fn set_timer(
        &self,
        id: Uuid,
        column: &'static str,
        value: Option<DateTime<Utc>>,
    ) -> Result<(), sqlx::Error> {
        // Имя колонки только из двух литералов выше.
        let sql = match column {
            "idle_since" => {
                "update cloud_windows set idle_since = $2 where id = $1 and state = 'ready'"
            }
            _ => {
                "update cloud_windows set disconnected_since = $2 where id = $1 and state = 'ready'"
            }
        };
        sqlx::query(sql)
            .bind(id)
            .bind(value)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Жёсткий предел: раннер перестаёт брать работу, идущая дорабатывает.
    async fn start_draining(&self, window: &Window) -> Result<(), sqlx::Error> {
        let id = window.id;
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move {
                let moved = sqlx::query(
                    "update cloud_windows set state = 'draining', idle_since = null
                     where id = $1 and state = 'ready'",
                )
                .bind(id)
                .execute(&mut *conn)
                .await?
                .rows_affected();
                if moved > 0
                    && let Some(machine) = cloud_machine_id(&mut *conn).await?
                {
                    sqlx::query("update machines set accepts_remote_work = false where id = $1")
                        .bind(machine)
                        .execute(&mut *conn)
                        .await?;
                }
                Ok::<_, sqlx::Error>(())
            })
        })
        .await
    }

    async fn step_draining(
        &self,
        runtime: &mut Runtime,
        window: Window,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        if self.poll_instance(runtime, &window, now).await? {
            return Ok(());
        }
        let mut conn = self.pool.acquire().await?;
        let machine = cloud_machine_id(&mut conn).await?;
        drop(conn);
        let Some(machine) = machine else {
            return Ok(());
        };
        let live: i64 = sqlx::query_scalar(
            "select count(*) from jobs where machine_id = $1
               and state in ('staging','queued','dispatched','waiting_lease','running','collecting')",
        )
        .bind(machine)
        .fetch_one(&self.pool)
        .await?;
        if live == 0 {
            return self.delete_now(&window, "drained", now).await;
        }
        if window.teardown_until.is_some_and(|until| now >= until) {
            // Отзыв раннера закончит неподтверждённые задачи как `lost`.
            return self.delete_now(&window, "teardown", now).await;
        }
        if window.cancel_sent_at.is_none() && window.drain_until.is_some_and(|until| now >= until) {
            let id = window.id;
            crate::db::in_tx(&self.pool, move |conn| {
                Box::pin(async move {
                    let jobs: Vec<Uuid> = sqlx::query_scalar(
                        "select id from jobs where machine_id = $1
                           and state in ('staging','queued','dispatched','waiting_lease','running')
                         order by created_at, id",
                    )
                    .bind(machine)
                    .fetch_all(&mut *conn)
                    .await?;
                    for job in jobs {
                        crate::inplace_jobs::cancel_in_tx(
                            &mut *conn,
                            job,
                            "cloud_window_hard_cap",
                            Uuid::now_v7(),
                        )
                        .await?;
                    }
                    sqlx::query(
                        "update cloud_windows set cancel_sent_at = clock_timestamp() where id = $1",
                    )
                    .bind(id)
                    .execute(&mut *conn)
                    .await?;
                    Ok::<_, sqlx::Error>(())
                })
            })
            .await?;
            crate::notify::enqueue_cloud_alert(
                &self.pool,
                &format!("cloud:cloud_window_hard_cap:{}", window.id),
                &json!({"code": "cloud_window_hard_cap", "window_id": window.id, "name": window.name}),
            )
            .await?;
        }
        Ok(())
    }

    /// Окно, которое живёт дольше предела с запасами: машина удаляется, владельцу пуш.
    /// Найденное на первом проходе после старта — простой хаба с лишним расходом.
    async fn overrun(
        &self,
        runtime: &Runtime,
        window: &Window,
        now: DateTime<Utc>,
    ) -> Result<bool, sqlx::Error> {
        let (Some(cap), Some(limit)) = (window.hard_cap_at, window.teardown_until) else {
            return Ok(false);
        };
        let slack = if window.state == "draining" {
            OVERRUN_SLACK
        } else {
            TimeDelta::zero()
        };
        if now < limit + slack {
            return Ok(false);
        }
        // Сверх предела тарифицируются и машина, и диск: тот же счёт по секундам.
        let extra = price::ledger_kopecks(
            window.compute_kopecks_per_hour,
            window.disk_kopecks_per_hour,
            cap,
            now,
            now,
        )
        .ok()
        .map(|cost| cost.min(i64::MAX / 2));
        let code = if runtime.first_tick {
            "outage_extra_cost"
        } else {
            "window_overrun"
        };
        crate::notify::enqueue_cloud_alert(
            &self.pool,
            &format!("cloud:{code}:{}", window.id),
            &json!({"code": code, "window_id": window.id, "name": window.name, "extra_kopecks": extra}),
        )
        .await?;
        self.delete_now(window, "overrun", now).await?;
        Ok(true)
    }

    async fn delete_now(
        &self,
        window: &Window,
        reason: &str,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        windows::request_delete(&self.pool, window.id, reason, now).await?;
        Ok(())
    }

    // ───────── Снос: машина, затем диск ─────────

    /// Шаг сноса. Порядок жёсткий: сначала машина, потом её загрузочный диск, и только когда
    /// полные списки каталога окна подтвердили отсутствие обоих, окно заканчивается.
    async fn step_deleting(
        &self,
        mut window: Window,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        if window.server_gone_at.is_none() && !self.remove_instance(&mut window, now).await? {
            return Ok(());
        }
        if window.disk_gone_at.is_none() && !self.remove_disk(&mut window, now).await? {
            return Ok(());
        }
        self.end(window.id, now).await
    }

    /// Машина окна: удалить и подтвердить отсутствие. `true`: полный список каталога окна
    /// без её номера и без имени окна, `server_gone_at` стоит.
    async fn remove_instance(
        &self,
        window: &mut Window,
        now: DateTime<Utc>,
    ) -> Result<bool, sqlx::Error> {
        let Some(instance_id) = window.instance_id.clone() else {
            // Номера нет: создание не дало ответа или отказало. Машину ищет имя окна в
            // полном списке его каталога.
            let listing = match self.provider.list_instances(&window.folder_id).await {
                Ok(listing) => listing,
                Err(error) => {
                    tracing::warn!(kind = "cloud_confirm_listing", error = %error, "listing failed; nothing changed");
                    return Ok(false);
                }
            };
            return match listing.iter().find(|instance| instance.name == window.name) {
                Some(found) => {
                    // Машина всё-таки появилась: дальше она удаляется по номеру.
                    sqlx::query(
                        "update cloud_windows set instance_id = $2
                         where id = $1 and state in ('deleting', 'late_cleanup')
                           and instance_id is null",
                    )
                    .bind(window.id)
                    .bind(&found.id)
                    .execute(&self.pool)
                    .await?;
                    Ok(false)
                }
                None => {
                    self.mark_server_gone(window, now).await?;
                    Ok(true)
                }
            };
        };
        if window.delete_accepted_at.is_none() {
            if !due(window.next_delete_at, now) {
                return Ok(false);
            }
            // Раннер отозван переходом в `deleting`; повторять отзыв не нужно.
            self.try_delete(window, &instance_id, now).await?;
            if !self.refresh(window).await? {
                return Ok(false);
            }
        }
        if window.server_gone_at.is_some() {
            return Ok(true);
        }
        if window.delete_accepted_at.is_none() {
            return Ok(false);
        }
        // Удаление принято. Ошибка его операции значит, что машина осталась: удаление
        // повторяется с откатом и новой проверкой принадлежности.
        if let Some(operation_id) = window.delete_operation_id.as_deref()
            && let Ok(OperationState::Failed { grpc_code, .. }) =
                self.provider.operation(operation_id).await
        {
            tracing::warn!(
                kind = "cloud_delete_failed",
                grpc_code,
                "delete operation failed"
            );
            let attempts = window.delete_attempts.saturating_add(1);
            sqlx::query(
                "update cloud_windows set delete_accepted_at = null, delete_operation_id = null,
                        last_error = 'delete_failed', delete_attempts = $2, next_delete_at = $3
                 where id = $1 and state in ('deleting', 'late_cleanup')",
            )
            .bind(window.id)
            .bind(attempts)
            .bind(now + delete_backoff(attempts))
            .execute(&self.pool)
            .await?;
            if attempts >= DELETE_FAILURES_BEFORE_ALERT {
                self.alert_delete_failing(window, now).await?;
            }
            return Ok(false);
        }
        // Конец оплаты машины подтверждает только полный список каталога окна без неё.
        match self.provider.list_instances(&window.folder_id).await {
            Ok(listing) if listed(&listing, window).is_none() => {
                self.mark_server_gone(window, now).await?;
                return Ok(true);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(kind = "cloud_confirm_listing", error = %error, "listing failed; nothing changed");
            }
        }
        if due(window.next_delete_at, now) {
            // Принятое удаление не подтвердилось за срок: DELETE повторяется с откатом, а
            // владелец получает пуш о неудаляемой машине раз в 30 минут.
            self.try_delete(window, &instance_id, now).await?;
        }
        Ok(false)
    }

    /// Свежая строка окна после шага, который мог её изменить. `false`: окно уже не на сносе.
    async fn refresh(&self, window: &mut Window) -> Result<bool, sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        match windows::load(&mut conn, window.id).await? {
            Some(fresh) if matches!(fresh.state.as_str(), "deleting" | windows::LATE_CLEANUP) => {
                *window = fresh;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Принадлежность машины перед удалением. Прямой ответ провайдера несёт номер, имя и
    /// каталог: всё должно совпасть с окном. «Нет такой» отсутствия не доказывает: его
    /// подтверждает полный список каталога без номера и без имени.
    async fn ownership(&self, instance_id: &str, name: &str, folder_id: &str) -> Ownership {
        match self.provider.get(instance_id).await {
            Ok(InstanceLookup::Found(instance)) => {
                if instance.id == instance_id
                    && instance.name == name
                    && instance.folder_id == folder_id
                {
                    Ownership::Owned
                } else {
                    Ownership::Mismatch
                }
            }
            Ok(InstanceLookup::NotFound) => match self.provider.list_instances(folder_id).await {
                Ok(listing)
                    if !listing
                        .iter()
                        .any(|instance| instance.id == instance_id || instance.name == name) =>
                {
                    Ownership::Absent
                }
                Ok(_) => Ownership::Unknown,
                Err(error) => {
                    tracing::warn!(kind = "cloud_ownership_listing", error = %error, "listing failed; absence is unknown");
                    Ownership::Unknown
                }
            },
            Err(error) => {
                tracing::warn!(kind = "cloud_delete_lookup", error_kind = ?error.kind, "instance lookup before delete failed");
                Ownership::Unknown
            }
        }
    }

    /// Машины нет в полном списке каталога (метка ставится один раз). Срок следующей попытки
    /// сбрасывается: дальше идёт диск.
    async fn mark_server_gone(
        &self,
        window: &mut Window,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "update cloud_windows set server_gone_at = coalesce(server_gone_at, $2),
                    next_delete_at = case when server_gone_at is null then null else next_delete_at end
             where id = $1 and state in ('deleting', 'late_cleanup')",
        )
        .bind(window.id)
        .bind(now)
        .execute(&self.pool)
        .await?;
        if window.server_gone_at.is_none() {
            window.server_gone_at = Some(now);
            window.next_delete_at = None;
        }
        Ok(())
    }

    /// Загрузочный диск окна: удалить, если он пережил машину, и подтвердить отсутствие.
    /// Диск ищется по номеру, а без номера — по точному имени окна в полном списке дисков
    /// его каталога. `true`: диска в списке нет, `disk_gone_at` стоит.
    async fn remove_disk(
        &self,
        window: &mut Window,
        now: DateTime<Utc>,
    ) -> Result<bool, sqlx::Error> {
        let disks = match self.provider.list_disks(&window.folder_id).await {
            Ok(disks) => disks,
            Err(error) => {
                tracing::warn!(kind = "cloud_disk_listing", error = %error, "disk listing failed; nothing changed");
                return Ok(false);
            }
        };
        let by_id = disks
            .iter()
            .find(|disk| window.boot_disk_id.as_deref() == Some(disk.id.as_str()));
        let Some(disk) = by_id.or_else(|| disks.iter().find(|disk| disk.name == window.name))
        else {
            self.mark_disk_gone(window, now).await?;
            return Ok(true);
        };
        if window.boot_disk_id.is_none() {
            // С этого момента окну было за что платить: итог считается по ledger.
            sqlx::query(
                "update cloud_windows set boot_disk_id = $2
                 where id = $1 and state in ('deleting', 'late_cleanup') and boot_disk_id is null",
            )
            .bind(window.id)
            .bind(&disk.id)
            .execute(&self.pool)
            .await?;
            window.boot_disk_id = Some(disk.id.clone());
        }
        // Список только что прочитан из каталога окна: номер и каталог доказаны им. Имя
        // должно быть именем окна: диск с нашим номером и чужим именем не удаляется.
        if disk.name != window.name {
            sqlx::query(
                "update cloud_windows set last_error = 'ownership_mismatch', next_delete_at = $2
                 where id = $1 and state in ('deleting', 'late_cleanup')",
            )
            .bind(window.id)
            .bind(now + ALERT_EVERY)
            .execute(&self.pool)
            .await?;
            crate::notify::enqueue_cloud_alert(
                &self.pool,
                &format!(
                    "cloud:ownership_mismatch:{}:{}",
                    window.id,
                    alert_slot(window, now)
                ),
                &json!({
                    "code": "ownership_mismatch",
                    "window_id": window.id,
                    "name": window.name,
                    "disk_id": disk.id,
                }),
            )
            .await?;
            return Ok(false);
        }
        // Диск ещё подключён или уже удаляется: ждём.
        if !disk.instance_ids.is_empty() || disk.status == "DELETING" {
            return Ok(false);
        }
        if !due(window.next_delete_at, now) {
            return Ok(false);
        }
        let attempts = window.delete_attempts.saturating_add(1);
        match self.provider.delete_disk(&disk.id).await {
            DeleteOutcome::Accepted { .. } | DeleteOutcome::AlreadyGone => {
                // Подтверждение тем же способом, что у машины: диска нет в полном списке.
                if let Ok(after) = self.provider.list_disks(&window.folder_id).await
                    && !after
                        .iter()
                        .any(|other| other.id == disk.id || other.name == window.name)
                {
                    self.mark_disk_gone(window, now).await?;
                    return Ok(true);
                }
                sqlx::query(
                    "update cloud_windows set delete_attempts = $2, next_delete_at = $3
                     where id = $1 and state in ('deleting', 'late_cleanup')",
                )
                .bind(window.id)
                .bind(attempts)
                .bind(now + delete_backoff(attempts))
                .execute(&self.pool)
                .await?;
            }
            DeleteOutcome::Failed { .. } => {
                sqlx::query(
                    "update cloud_windows set last_error = 'disk_delete_failed',
                            delete_attempts = $2, next_delete_at = $3
                     where id = $1 and state in ('deleting', 'late_cleanup')",
                )
                .bind(window.id)
                .bind(attempts)
                .bind(now + delete_backoff(attempts))
                .execute(&self.pool)
                .await?;
                if attempts >= DELETE_FAILURES_BEFORE_ALERT {
                    crate::notify::enqueue_cloud_alert(
                        &self.pool,
                        &format!(
                            "cloud:disk_delete_failing:{}:{}",
                            window.id,
                            alert_slot(window, now)
                        ),
                        &json!({
                            "code": "disk_delete_failing",
                            "window_id": window.id,
                            "name": window.name,
                        }),
                    )
                    .await?;
                }
            }
        }
        Ok(false)
    }

    async fn mark_disk_gone(
        &self,
        window: &mut Window,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "update cloud_windows set disk_gone_at = coalesce(disk_gone_at, $2), next_delete_at = null
             where id = $1 and state in ('deleting', 'late_cleanup')",
        )
        .bind(window.id)
        .bind(now)
        .execute(&self.pool)
        .await?;
        window.disk_gone_at.get_or_insert(now);
        Ok(())
    }

    async fn try_delete(
        &self,
        window: &Window,
        instance_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        if window.server_gone_at.is_some() {
            // Отсутствие уже подтверждено: DELETE больше не нужен.
            return Ok(());
        }
        // Удаление только по номеру опасно: номер мог достаться чужой машине.
        match self
            .ownership(instance_id, &window.name, &window.folder_id)
            .await
        {
            Ownership::Owned => {}
            Ownership::Mismatch => {
                sqlx::query(
                    "update cloud_windows set last_error = 'ownership_mismatch',
                            next_delete_at = $2
                     where id = $1 and state in ('deleting', 'late_cleanup')",
                )
                .bind(window.id)
                .bind(now + ALERT_EVERY)
                .execute(&self.pool)
                .await?;
                crate::notify::enqueue_cloud_alert(
                    &self.pool,
                    &format!(
                        "cloud:ownership_mismatch:{}:{}",
                        window.id,
                        alert_slot(window, now)
                    ),
                    &json!({
                        "code": "ownership_mismatch",
                        "window_id": window.id,
                        "name": window.name,
                        "instance_id": instance_id,
                    }),
                )
                .await?;
                return Ok(());
            }
            // Полный список каталога без машины — единственное доказательство отсутствия.
            Ownership::Absent => {
                sqlx::query(
                    "update cloud_windows set server_gone_at = coalesce(server_gone_at, $2),
                            next_delete_at = null
                     where id = $1 and state in ('deleting', 'late_cleanup')",
                )
                .bind(window.id)
                .bind(now)
                .execute(&self.pool)
                .await?;
                return Ok(());
            }
            Ownership::Unknown => {
                // Ответа нет: это неудача удаления, как и отказ провайдера. Повтор идёт с
                // откатом, а после третьей неудачи владельцу уходит «deletion failing».
                let attempts = window.delete_attempts.saturating_add(1);
                sqlx::query(
                    "update cloud_windows set last_error = 'delete_lookup_failed',
                            delete_attempts = $2, next_delete_at = $3
                     where id = $1 and state in ('deleting', 'late_cleanup')",
                )
                .bind(window.id)
                .bind(attempts)
                .bind(now + delete_backoff(attempts))
                .execute(&self.pool)
                .await?;
                if attempts >= DELETE_FAILURES_BEFORE_ALERT {
                    self.alert_delete_failing(window, now).await?;
                }
                return Ok(());
            }
        }
        let repeat = window.delete_accepted_at.is_some();
        let outcome = self.provider.delete(instance_id).await;
        let attempts = window.delete_attempts.saturating_add(1);
        match outcome {
            // Повтор, пока удаление идёт, возвращает ту же операцию. «Уже нет» тоже ждёт
            // подтверждения списком.
            DeleteOutcome::Accepted { .. } | DeleteOutcome::AlreadyGone => {
                let operation_id = match outcome {
                    DeleteOutcome::Accepted { operation_id } => Some(operation_id),
                    _ => None,
                };
                let next = if repeat {
                    now + delete_backoff(attempts)
                } else {
                    now + DELETE_CONFIRM_AFTER
                };
                // Подробность неудавшегося создания остаётся: по ней владелец поймёт,
                // почему окно закончилось `failed`.
                sqlx::query(
                    "update cloud_windows set delete_accepted_at = coalesce(delete_accepted_at, $2),
                            delete_operation_id = coalesce($5, delete_operation_id),
                            last_error = case when delete_reason = 'create_failed' then last_error end,
                            delete_attempts = $3,
                            next_delete_at = $4
                     where id = $1 and state in ('deleting', 'late_cleanup')",
                )
                .bind(window.id)
                .bind(now)
                .bind(attempts)
                .bind(next)
                .bind(operation_id)
                .execute(&self.pool)
                .await?;
                if repeat {
                    self.alert_delete_still_listed(window, now).await?;
                }
            }
            DeleteOutcome::Failed { .. } => {
                sqlx::query(
                    "update cloud_windows set last_error = 'delete_failed', delete_attempts = $2,
                            next_delete_at = $3
                     where id = $1 and state in ('deleting', 'late_cleanup')",
                )
                .bind(window.id)
                .bind(attempts)
                .bind(now + delete_backoff(attempts))
                .execute(&self.pool)
                .await?;
                if attempts >= DELETE_FAILURES_BEFORE_ALERT {
                    self.alert_delete_failing(window, now).await?;
                }
            }
        }
        Ok(())
    }

    /// Пуш о машине, которая не исчезает из списка после принятого удаления. Не чаще раза
    /// в 30 минут на окно.
    async fn alert_delete_still_listed(
        &self,
        window: &Window,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        crate::notify::enqueue_cloud_alert(
            &self.pool,
            &format!(
                "cloud:delete_still_listed:{}:{}",
                window.id,
                alert_slot(window, now)
            ),
            &json!({"code": "delete_failing", "window_id": window.id, "name": window.name}),
        )
        .await?;
        Ok(())
    }

    async fn alert_delete_failing(
        &self,
        window: &Window,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let since = window.delete_requested_at.unwrap_or(now);
        let slot = (now - since).num_seconds().max(0) / ALERT_EVERY.num_seconds();
        crate::notify::enqueue_cloud_alert(
            &self.pool,
            &format!("cloud:delete_failing:{}:{slot}", window.id),
            &json!({"code": "delete_failing", "window_id": window.id, "name": window.name}),
        )
        .await?;
        Ok(())
    }

    /// Конец окна после сноса: машины и диска нет в полных списках. Итог — ledger из двух
    /// частей, каждая до своего подтверждённого конца. Конечное состояние зависит от причины
    /// сноса (`windows::terminal_state`).
    async fn end(&self, id: Uuid, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move {
                let Some(window) = windows::load_for_update(&mut *conn, id).await? else {
                    return Ok::<_, sqlx::Error>(());
                };
                let cleaning = matches!(window.state.as_str(), "deleting" | windows::LATE_CLEANUP);
                if !cleaning || window.server_gone_at.is_none() || window.disk_gone_at.is_none() {
                    return Ok(());
                }
                let to = windows::terminal_state(window.delete_reason.as_deref());
                let cost = windows::final_cost(&window, now);
                sqlx::query(
                    "update cloud_windows set state = $2, final_cost_kopecks = $3,
                            delete_confirmed_at = $4,
                            last_error = case when delete_reason in ('create_failed', 'gone')
                                              then last_error end
                     where id = $1",
                )
                .bind(id)
                .bind(to)
                .bind(cost)
                .bind(now)
                .execute(&mut *conn)
                .await?;
                windows::forget_key(id);
                if window.delete_reason.as_deref() == Some(windows::REASON_PREEMPTED) {
                    windows::on_preempted_cleanup_done_in_tx(conn, &window, now).await?;
                }
                Ok(())
            })
        })
        .await
    }

    // ───────── Сверка ─────────

    /// Сверка каталогов со строками окон. `Ok(false)` — сверка прошла, но какое-то удаление
    /// ещё ждёт подтверждения: успешной она не считается. `Err(())` — список какого-то
    /// каталога не получен целиком; для этого каталога ничего не менялось.
    ///
    /// Каталоги: тот, что в конфигурации (только в нём ищутся лишние машины и диски), и
    /// каталоги всех окон, которые ещё владеют ресурсом или закончились `failed`. Так окно
    /// из прежнего каталога доубирается и после смены конфигурации.
    async fn reconcile(&self, runtime: &mut Runtime, now: DateTime<Utc>) -> Result<bool, ()> {
        let mut folders = vec![self.config.folder_id.clone()];
        let others: Vec<String> = sqlx::query_scalar(
            "select distinct folder_id from cloud_windows
              where state = any($1) or state = 'failed'",
        )
        .bind(windows::billing_states())
        .fetch_all(&self.pool)
        .await
        .map_err(|error| warn_db("cloud_reconcile_folders", &error))?;
        for folder in others {
            if !folder.is_empty() && !folders.contains(&folder) {
                folders.push(folder);
            }
        }
        // Каждый каталог разбирается сам: ошибка списка одного не мешает подтверждению и
        // сносу окон другого. Новых созданий при этом не будет: успешной такая сверка не
        // считается.
        let mut complete = true;
        for folder in folders {
            let instances = match self.provider.list_instances(&folder).await {
                Ok(instances) => instances,
                Err(error) => {
                    tracing::warn!(kind = "cloud_reconcile_listing", error = %error, "instance listing failed; nothing changed for this folder");
                    complete = false;
                    continue;
                }
            };
            let disks = match self.provider.list_disks(&folder).await {
                Ok(disks) => disks,
                Err(error) => {
                    tracing::warn!(kind = "cloud_reconcile_listing", error = %error, "disk listing failed; nothing changed for this folder");
                    complete = false;
                    continue;
                }
            };
            if let Err(error) = self
                .apply_listing(runtime, &folder, &instances, &disks, now)
                .await
            {
                warn_db("cloud_reconcile", &error);
                complete = false;
            }
        }
        if !complete {
            return Err(());
        }
        let pending: bool = sqlx::query_scalar(
            "select exists(select 1 from cloud_windows
                            where state in ('deleting', 'late_cleanup')
                              and delete_accepted_at is not null
                              and server_gone_at is null)",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|error| warn_db("cloud_reconcile_pending", &error))?;
        Ok(!pending)
    }

    async fn apply_listing(
        &self,
        runtime: &mut Runtime,
        folder: &str,
        instances: &[InstanceSummary],
        disks: &[Disk],
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        // Только точные имена окон: `canterlot-runner-<канонический uuid>`.
        let mut named_instances: HashMap<Uuid, Vec<&InstanceSummary>> = HashMap::new();
        for instance in instances {
            if let Some(id) = window_id_from_name(&instance.name) {
                named_instances.entry(id).or_default().push(instance);
            }
        }
        let mut named_disks: HashMap<Uuid, Vec<&Disk>> = HashMap::new();
        for disk in disks {
            if let Some(id) = window_id_from_name(&disk.name) {
                named_disks.entry(id).or_default().push(disk);
            }
        }
        let ids: Vec<Uuid> = named_instances
            .keys()
            .chain(named_disks.keys())
            .copied()
            .collect();
        let windows: Vec<Window> = sqlx::query_as(
            "select * from cloud_windows where state = any($1) or id = any($2) order by created_at, id",
        )
        .bind(windows::billing_states())
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?;
        let unknown_for = grace(self.config.create_unknown_window);
        let mut known = HashSet::new();
        for window in &windows {
            known.insert(window.id);
            if window.folder_id != folder {
                continue;
            }
            // Два ресурса с одним именем окна: по имени не решить, какой из них наш.
            let instance = match named_instances.get(&window.id).map(Vec::as_slice) {
                None => None,
                Some([instance]) => Some(*instance),
                Some(_) => {
                    tracing::warn!(
                        kind = "cloud_reconcile_ambiguous",
                        "several instances share a window name; skipped"
                    );
                    continue;
                }
            };
            let old_enough = window
                .create_called_at
                .is_some_and(|called| now - called >= unknown_for);
            match window.state.as_str() {
                "create_unknown" => match instance {
                    Some(instance) => {
                        self.identify(window.id, &instance.id, None, "create_unknown")
                            .await?;
                    }
                    // Машина так и не появилась в списках. Вызов был, поэтому окно уходит
                    // на снос: тот подтвердит ещё и диск.
                    None if old_enough => {
                        self.fail_after_call(window.id, "not_found_in_listing", now)
                            .await?;
                    }
                    None => {}
                },
                // Вызов создания начат, а хаб упал до ответа: машина уже видна по имени.
                "create_intent" if window.create_called_at.is_some() => {
                    if let Some(instance) = instance {
                        self.identify(window.id, &instance.id, None, "create_intent")
                            .await?;
                    }
                }
                // Только когда машина уже должна была попасть в список: у провайдера она
                // может появиться с задержкой.
                "identified" | "ready" | "draining" => {
                    if old_enough && listed(instances, window).is_none() {
                        self.gone(window, now).await?;
                    }
                }
                "failed" | "ended" | "gone" if window.create_called_at.is_some() => {
                    if let Some(instance) = instance {
                        // Машина закрытого окна появилась позже: это не лишняя машина, а
                        // обязательство исходного окна. Возвращаем её окну, чтобы удалить
                        // обычным путём и посчитать итог от вызова создания.
                        self.reattach_late(window, instance, now).await?;
                    } else if let Some(disks) = named_disks.get(&window.id) {
                        match disks.as_slice() {
                            // Диск закрытого окна остался без машины: он тарифицируется.
                            [disk] if disk.instance_ids.is_empty() && disk.status != "DELETING" => {
                                self.reopen_for_disk(window, disk, now).await?;
                            }
                            [_] => {}
                            _ => {
                                tracing::warn!(
                                    kind = "cloud_reconcile_ambiguous",
                                    "several disks share a window name; skipped"
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Лишние машины и диски ищутся только в каталоге из конфигурации: он отведён под
        // раннеры целиком.
        if folder == self.config.folder_id {
            self.delete_strays(runtime, &named_instances, &named_disks, &known, now)
                .await?;
        }
        Ok(())
    }

    /// Поздняя машина возвращается своему окну как обязательство сноса: `late_cleanup` вне
    /// правила одного живого окна, поэтому преемник своё место не теряет. Снос считается в
    /// бюджете и закрывает новые создания, а итог — от вызова создания до подтверждённого
    /// отсутствия.
    async fn reattach_late(
        &self,
        window: &Window,
        instance: &InstanceSummary,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let updated = sqlx::query(
            "update cloud_windows set state = 'late_cleanup', instance_id = $2,
                    final_cost_kopecks = null, delete_confirmed_at = null, server_gone_at = null,
                    disk_gone_at = null, delete_requested_at = $3, delete_reason = 'late_create',
                    delete_accepted_at = null, delete_operation_id = null,
                    delete_attempts = 0, next_delete_at = $3, last_error = 'late_create'
              where id = $1 and state in ('failed', 'ended', 'gone')",
        )
        .bind(window.id)
        .bind(&instance.id)
        .bind(now)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated > 0 {
            crate::notify::enqueue_cloud_alert(
                &self.pool,
                &format!("cloud:late_create:{}", window.id),
                &json!({"code": "late_create", "window_id": window.id, "name": window.name}),
            )
            .await?;
        }
        Ok(())
    }

    /// Диск закрытого окна нашёлся в каталоге без машины: окно открывается заново как
    /// `late_cleanup` и убирает его обычным шагом сноса. Ledger остаётся тот же: машина
    /// посчитана до своего прежнего конца, диск — до нового.
    async fn reopen_for_disk(
        &self,
        window: &Window,
        disk: &Disk,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let updated = sqlx::query(
            "update cloud_windows set state = 'late_cleanup', boot_disk_id = $2,
                    final_cost_kopecks = null, delete_confirmed_at = null, disk_gone_at = null,
                    delete_requested_at = $3, delete_reason = 'late_disk',
                    delete_attempts = 0, next_delete_at = $3, last_error = 'late_disk'
              where id = $1 and state in ('failed', 'ended', 'gone')",
        )
        .bind(window.id)
        .bind(&disk.id)
        .bind(now)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated > 0 {
            crate::notify::enqueue_cloud_alert(
                &self.pool,
                &format!("cloud:late_disk:{}:{}", window.id, disk.id),
                &json!({"code": "late_disk", "window_id": window.id, "name": window.name}),
            )
            .await?;
        }
        Ok(())
    }

    /// Машина окна пропала из полного списка каталога, хотя удаление не запрашивалось. Окно
    /// не заканчивается сразу: оно уходит на снос с уже подтверждённым отсутствием машины, и
    /// тому остаётся диск. Закончится оно `gone`.
    async fn gone(&self, window: &Window, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        let id = window.id;
        let moved = crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move {
                let Some(window) = windows::load_for_update(&mut *conn, id).await? else {
                    return Ok::<_, sqlx::Error>(false);
                };
                if !windows::SERVING.contains(&window.state.as_str()) {
                    return Ok(false);
                }
                windows::begin_cleanup_in_tx(
                    conn,
                    id,
                    Cleanup {
                        last_error: Some("server_missing"),
                        server_gone: true,
                        ..Cleanup::because(windows::REASON_GONE)
                    },
                    now,
                )
                .await
            })
        })
        .await?;
        if moved {
            crate::notify::enqueue_cloud_alert(
                &self.pool,
                &format!("cloud:window_gone:{}", window.id),
                &json!({"code": "window_gone", "window_id": window.id, "name": window.name}),
            )
            .await?;
        }
        Ok(())
    }

    /// Лишние машины и диски каталога из конфигурации: имя точно как у окна, а строки окна
    /// нет вовсе. Каждое удаление идёт после свежей проверки этого же номера, а сообщение
    /// владельцу — когда полный список подтвердил отсутствие. Неоднозначное пропускается.
    async fn delete_strays(
        &self,
        runtime: &mut Runtime,
        named_instances: &HashMap<Uuid, Vec<&InstanceSummary>>,
        named_disks: &HashMap<Uuid, Vec<&Disk>>,
        known: &HashSet<Uuid>,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let folder = self.config.folder_id.as_str();
        let slot = now.timestamp() / ALERT_EVERY.num_seconds();
        let stray_instances: Vec<&InstanceSummary> = named_instances
            .iter()
            .filter(|(id, _)| !known.contains(id))
            .filter_map(|(_, found)| match found.as_slice() {
                [instance] => Some(*instance),
                _ => None,
            })
            .collect();
        let stray_disks: Vec<&Disk> = named_disks
            .iter()
            .filter(|(id, _)| !known.contains(id))
            .filter_map(|(_, found)| match found.as_slice() {
                // Диск при машине уйдёт вместе с ней; подключённый не удалить.
                [disk] if disk.instance_ids.is_empty() => Some(*disk),
                _ => None,
            })
            .collect();
        for instance in stray_instances {
            let stuck = json!({
                "code": "stray_server_stuck",
                "name": instance.name,
                "instance_id": instance.id,
            });
            let stuck_key = format!("cloud:stray_stuck:{}:{slot}", instance.id);
            if instance.status == InstanceStatus::Deleting {
                runtime.stray_pending.insert(
                    instance.id.clone(),
                    Stray::Instance {
                        name: instance.name.clone(),
                    },
                );
                continue;
            }
            // Перед удалением прямой ответ должен показать тот же номер с тем же именем в
            // том же каталоге: номер мог достаться чужой машине.
            match self.ownership(&instance.id, &instance.name, folder).await {
                Ownership::Owned => {}
                // Машины уже нет: удалять нечего.
                Ownership::Absent => continue,
                Ownership::Mismatch | Ownership::Unknown => {
                    crate::notify::enqueue_cloud_alert(&self.pool, &stuck_key, &stuck).await?;
                    continue;
                }
            }
            match self.provider.delete(&instance.id).await {
                DeleteOutcome::Accepted { .. } | DeleteOutcome::AlreadyGone => {
                    runtime.stray_pending.insert(
                        instance.id.clone(),
                        Stray::Instance {
                            name: instance.name.clone(),
                        },
                    );
                }
                DeleteOutcome::Failed { .. } => {
                    crate::notify::enqueue_cloud_alert(&self.pool, &stuck_key, &stuck).await?;
                }
            }
        }
        if !stray_disks.is_empty() {
            // Свежий список дисков перед удалением: диск должен стоять в нём с тем же
            // номером, тем же именем и без машины.
            match self.provider.list_disks(folder).await {
                Ok(fresh) => {
                    for disk in stray_disks {
                        let still_stray = fresh.iter().any(|other| {
                            other.id == disk.id
                                && other.name == disk.name
                                && other.instance_ids.is_empty()
                        });
                        if !still_stray {
                            continue;
                        }
                        match self.provider.delete_disk(&disk.id).await {
                            DeleteOutcome::Accepted { .. } | DeleteOutcome::AlreadyGone => {
                                runtime.stray_pending.insert(
                                    disk.id.clone(),
                                    Stray::Disk {
                                        name: disk.name.clone(),
                                    },
                                );
                            }
                            DeleteOutcome::Failed { .. } => {
                                crate::notify::enqueue_cloud_alert(
                                    &self.pool,
                                    &format!("cloud:stray_disk_stuck:{}:{slot}", disk.id),
                                    &json!({
                                        "code": "stray_disk_stuck",
                                        "name": disk.name,
                                        "disk_id": disk.id,
                                    }),
                                )
                                .await?;
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(kind = "cloud_stray_listing", error = %error, "disk listing failed; no stray disk deleted");
                }
            }
        }
        self.confirm_strays(runtime).await
    }

    /// Удалённое лишнее сообщается владельцу, когда его нет в свежем полном списке.
    async fn confirm_strays(&self, runtime: &mut Runtime) -> Result<(), sqlx::Error> {
        if runtime.stray_pending.is_empty() {
            return Ok(());
        }
        let folder = self.config.folder_id.as_str();
        let any = |instance: bool| {
            runtime
                .stray_pending
                .values()
                .any(|stray| matches!(stray, Stray::Instance { .. }) == instance)
        };
        let instances = if any(true) {
            self.provider.list_instances(folder).await.ok()
        } else {
            None
        };
        let disks = if any(false) {
            self.provider.list_disks(folder).await.ok()
        } else {
            None
        };
        let mut confirmed = Vec::new();
        for (id, stray) in &runtime.stray_pending {
            let (code, name, id_key, absent) = match stray {
                Stray::Instance { name } => (
                    "stray_server_deleted",
                    name,
                    "instance_id",
                    instances
                        .as_ref()
                        .is_some_and(|list| !list.iter().any(|instance| &instance.id == id)),
                ),
                Stray::Disk { name } => (
                    "stray_disk_deleted",
                    name,
                    "disk_id",
                    disks
                        .as_ref()
                        .is_some_and(|list| !list.iter().any(|disk| &disk.id == id)),
                ),
            };
            if !absent {
                continue;
            }
            crate::notify::enqueue_cloud_alert(
                &self.pool,
                &format!("cloud:stray:{id}:{name}"),
                &json!({"code": code, "name": name, id_key: id}),
            )
            .await?;
            confirmed.push(id.clone());
        }
        for id in confirmed {
            runtime.stray_pending.remove(&id);
        }
        Ok(())
    }
}

/// Служба по конфигурации хаба. Нет таблицы `[runners.cloud]` — облако выключено. Ключ
/// сервисного аккаунта читается из файла и дальше живёт только внутри клиента. Всё, что
/// мешает включению (негодная конфигурация, нечитаемый ключ), пишется предупреждением с
/// причиной и без значений, а облако остаётся выключенным.
///
/// Несущая здесь не проверяется: снос и сверка обязательств нужны и без неё, а база может
/// быть временно недоступна на старте. Проверка несущей стоит только на пути нового
/// создания (`create_in_tx`) и повторяется на каждой попытке.
pub async fn from_config(
    pool: &PgPool,
    registry: &Registry,
    config: Option<CloudConfig>,
) -> Option<Arc<CloudService>> {
    let config = config?;
    let disabled = |reason: &str| {
        tracing::warn!(
            kind = "cloud_disabled",
            reason,
            "[runners.cloud] is configured but cloud windows are off"
        );
    };
    if let Err(code) = config.validate() {
        disabled(code);
        return None;
    }
    let provider = match YandexClient::new(config.yandex()) {
        Ok(provider) => provider,
        Err(error) => {
            // Текст ошибки — только вид сбоя, без содержимого файла ключа.
            disabled(&error.to_string());
            return None;
        }
    };
    tracing::info!(
        kind = "cloud_enabled",
        folder_id = %config.folder_id,
        profiles = config.profile.len(),
        preemptible = config.preemptible,
        carrier = %config.carrier_machine_id,
        "cloud runner windows enabled"
    );
    Some(CloudService::new(
        pool.clone(),
        registry.clone(),
        Arc::new(provider),
        config,
    ))
}

fn grace(duration: Duration) -> TimeDelta {
    TimeDelta::from_std(duration).unwrap_or(TimeDelta::hours(24))
}

fn warn_db(kind: &'static str, error: &sqlx::Error) {
    tracing::warn!(kind, error = %crate::dberr::describe(error), "cloud windows database error");
}
