//! Служба облачных окон: ведёт каждое живое окно по его состояниям и сверяет проект с базой.
//!
//! Один проход (`tick`) принимает «сейчас» параметром, поэтому сроки проверяются в тестах без
//! ожидания. Проходы идут строго по очереди (замок `runtime`), а в бою службу выполняет один
//! хаб за раз (`singleton::supervise`). Порядок прохода:
//! 1. сверка проекта (первым делом при старте, до любого создания, и раз в
//!    `reconcile_interval`);
//! 2. шаг каждого живого окна по его состоянию.
//!
//! Правила денег, которые держит этот файл:
//! - вызов создания один, без повторов; неопределённый исход разбирается по списку проекта;
//! - список, не прочитанный целиком, ничего не меняет;
//! - открытое обязательство удаления не снимается, пока сервер не пропал из полного списка.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

use super::{
    CloudProvider, CreateOutcome, CreateServer, DeleteOutcome, DnatCreateOutcome, DnatRuleRequest,
    ServerLookup, ServerStatus, ServerSummary,
    cloud_init::{self, CloudInitParams},
    price,
    windows::{
        self, BLOCK_NO_PAID, CreateRefusal, CreateRequest, Pricing, Trigger, Window, WindowId,
        cloud_machine_id, window_id_from_name,
    },
};
use crate::{
    cloud::budget,
    config::{CLOUD_ROOT_DIR, CloudConfig},
    machines::RunnerAvailability,
    session::{Availability, Registry},
    session_tools::cloud_propose::{self, Source},
};

/// Как часто служба проходит по окнам.
const TICK: Duration = Duration::from_secs(10);
/// Раннер `cloud` отключён столько при подключённой несущей: окно удаляется.
const DISCONNECTED_AFTER: TimeDelta = TimeDelta::minutes(10);
/// Деньги: повтор пуша о неудаляемом сервере.
const ALERT_EVERY: TimeDelta = TimeDelta::minutes(30);
/// После стольких неудач удаления идут пуши.
const DELETE_FAILURES_BEFORE_ALERT: i32 = 3;
/// Окно в `draining` задерживается сверх предела на доработку: так долго терпим, прежде чем
/// счесть это просрочкой, а не штатным сносом.
const OVERRUN_SLACK: TimeDelta = TimeDelta::minutes(10);
const SSH_KEY_NAME: &str = "canterlot-runner";

/// Состояние службы между проходами; живёт только в памяти.
#[derive(Default)]
struct Runtime {
    /// Хотя бы одна сверка прошла по полному списку: до этого создавать нельзя.
    reconciled: bool,
    last_reconcile: Option<DateTime<Utc>>,
    ssh_key_id: Option<i64>,
    /// Первый проход после старта хаба: найденная просрочка — это простой хаба.
    first_tick: bool,
}

pub struct CloudService {
    pool: PgPool,
    registry: Registry,
    provider: Arc<dyn CloudProvider>,
    config: CloudConfig,
    runtime: Mutex<Runtime>,
}

fn minutes_between(from: DateTime<Utc>, to: DateTime<Utc>) -> u64 {
    u64::try_from((to - from).num_seconds().max(0))
        .unwrap_or(0)
        .div_ceil(60)
}

/// Причина удаления окна, у которого сервер остался `no_paid`: аккаунт требует предоплату.
pub const REASON_NO_PAID: &str = "no_paid";

/// Итог окна: цена часа удалённого профиля за время от вызова создания до подтверждённого
/// конца, не меньше часа. У окна, которому не звонили, слагаемое 0. Публичный адрес больше
/// не заказывается, поэтому в итог входит только сервер.
fn ledger_cost(window: &Window, server_end: DateTime<Utc>) -> i64 {
    window.create_called_at.map_or(0, |called| {
        price::final_cost_kopecks(
            window.price_hourly_kopecks,
            minutes_between(called, server_end),
        )
        .unwrap_or(window.reservation_kopecks)
    })
}

fn delete_backoff(attempts: i32) -> TimeDelta {
    let exponent = u32::try_from(attempts.saturating_sub(1))
        .unwrap_or(0)
        .min(5);
    TimeDelta::seconds((30_i64 << exponent).min(600))
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

    /// Цена часа каждого профиля списка по ценам провайдера, в порядке списка. Профиль, цену
    /// которого получить нельзя (конфигуратора нет у провайдера), в список окна не попадает:
    /// резерв под него не посчитать, а вызывать его без резерва нельзя. Нет ни одного
    /// профиля с ценой — отказ.
    pub async fn pricing(&self) -> Result<Pricing, CreateRefusal> {
        let mut profiles = Vec::new();
        let mut last_error = None;
        for profile in &self.config.profile {
            let configurator = match self.provider.configurator(profile.configurator_id).await {
                Ok(configurator) => configurator,
                Err(error) => {
                    tracing::warn!(
                        kind = "cloud_profile_unpriced",
                        configurator_id = profile.configurator_id,
                        error_kind = ?error.kind,
                        "profile skipped: no price from the provider"
                    );
                    last_error = Some(error);
                    continue;
                }
            };
            let sizing = price::Sizing {
                cpu: profile.cpu,
                ram_mb: profile.ram_mb(),
                disk_mb: profile.disk_mb(),
            };
            let monthly = price::monthly_kopecks(&configurator.prices, &sizing)?;
            profiles.push(windows::PricedProfile {
                configurator_id: profile.configurator_id,
                cpu: profile.cpu,
                ram_gb: profile.ram_gb,
                disk_gb: profile.disk_gb,
                hourly_kopecks: price::hourly_kopecks(monthly)?,
                availability_zone: profile.availability_zone.clone(),
            });
        }
        if profiles.is_empty() {
            return Err(
                last_error.map_or(CreateRefusal::Invalid("profiles"), CreateRefusal::Provider)
            );
        }
        Ok(Pricing { profiles })
    }

    /// Создание окна из любого входа: цена у провайдера, затем одна транзакция. Вызов
    /// провайдера для самого сервера делает служба на ближайшем проходе.
    pub async fn create(&self, request: CreateRequest) -> Result<WindowId, CreateRefusal> {
        let pricing = self.pricing().await?;
        let config = self.config.clone();
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move { windows::create_in_tx(conn, &config, pricing, request).await })
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
            match self.reconcile(now).await {
                Ok(()) => runtime.reconciled = true,
                Err(()) => {
                    // Следующий проход попробует снова: пока сверки нет, новых серверов нет.
                    runtime.last_reconcile = None;
                }
            }
        }
        if runtime.reconciled {
            if let Err(error) = self.evaluate(now).await {
                warn_db("cloud_auto", &error);
            }
        }
        let windows = match self.live_windows().await {
            Ok(windows) => windows,
            Err(error) => {
                warn_db("cloud_windows_load", &error);
                return;
            }
        };
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

        if auto_mode && budget_kopecks > 0 && block.is_none() && fits {
            if self.all_static_runners_down(&mut conn, now).await? {
                drop(conn);
                let config = self.config.clone();
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
                        )
                        .await
                    })
                })
                .await;
                match created {
                    Ok(_) => {
                        if let Err(error) = crate::notify::enqueue_cloud_alert(
                            &self.pool,
                            &format!("cloud:auto_started:{}", windows::month_of(now)),
                            &json!({"code": "auto_started", "reservation_kopecks": reservation}),
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
        }

        // Автоматика выключена или резерв не помещается: карточка хаба один раз за эпизод.
        if !auto_mode || !fits {
            if !budget::episode_active(&mut conn).await? {
                let owner: Uuid = sqlx::query_scalar("select owner_id from machines where id = $1")
                    .bind(self.config.carrier_machine_id)
                    .fetch_one(&mut *conn)
                    .await?;
                let mut proposer = cloud_propose::system_proposer(
                    &mut conn,
                    owner,
                    self.config.carrier_machine_id,
                )
                .await?;
                proposer.source = Source::Hub;
                let reason = format!(
                    "Ожидающая облачная работа: {waiting} мин по оценке; автоматический режим {}",
                    if auto_mode {
                        "включён, но резерв не помещается в бюджет"
                    } else {
                        "выключен"
                    }
                );
                let opened = cloud_propose::open_in_tx(
                    &mut conn,
                    &self.config,
                    &pricing,
                    &proposer,
                    4,
                    reason,
                )
                .await;
                match opened {
                    Ok(opened) => {
                        budget::episode_start(&mut conn, opened.request_id).await?;
                    }
                    Err(error) => {
                        tracing::warn!(kind = "cloud_hub_card", error = %error, "card not opened");
                    }
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
        let candidates = budget::static_candidates(conn, &self.config).await?;
        let mut all_down = true;
        for machine in candidates {
            let (availability, _) =
                crate::machines::runner_availability_of(conn, &self.registry, machine, now).await?;
            let paused: bool =
                sqlx::query_scalar("select not accepts_remote_work from machines where id = $1")
                    .bind(machine)
                    .fetch_one(&mut *conn)
                    .await?;
            let down = availability == RunnerAvailability::Offline || paused;
            if !budget::track_unavailable(conn, machine, down, now).await? {
                all_down = false;
                break;
            }
        }
        Ok(all_down)
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
            "ready" => self.step_ready(window, now).await,
            "draining" => self.step_draining(window, now).await,
            "deleting" => self.step_deleting(window, now).await,
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
            // Вызов создания начат, а ответ не записан (хаб упал или проход оборвали): сервер
            // мог появиться. Второго вызова не будет, имя найдёт сверка.
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
        self.start_create(runtime, window, now).await
    }

    async fn fail(&self, id: Uuid, reason: &str) -> Result<(), sqlx::Error> {
        let reason = reason.to_owned();
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move { windows::fail_in_tx(conn, id, &reason).await })
        })
        .await?;
        Ok(())
    }

    async fn start_create(
        &self,
        runtime: &mut Runtime,
        window: Window,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let key_id = match runtime.ssh_key_id {
            Some(id) => id,
            None => match self
                .provider
                .ensure_ssh_key(SSH_KEY_NAME, &self.config.runner_public_key)
                .await
            {
                Ok(id) => {
                    runtime.ssh_key_id = Some(id);
                    id
                }
                Err(error) => {
                    tracing::warn!(kind = "cloud_ssh_key", error_kind = ?error.kind, "ssh key unavailable");
                    // Сервера ещё нет: пока ждём, окно не страдает; слишком долго нельзя.
                    if !error.retryable() || now - window.created_at > TimeDelta::minutes(15) {
                        self.fail(window.id, "ssh_key_unavailable").await?;
                    }
                    return Ok(());
                }
            },
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
            gateway: self.config.gateway.clone(),
        });
        let Ok(cloud_init) = rendered else {
            return self.fail(window.id, "cloud_init_invalid").await;
        };
        // Метка вызова ставится до самого вызова: от неё считаются предел и ledger, и по ней
        // упавший после этого хаб поймёт, что сервер мог появиться.
        let hard_cap = TimeDelta::minutes(i64::from(window.hard_cap_minutes));
        let drain_until = now + hard_cap + grace(self.config.drain_grace);
        let stamped = sqlx::query(
            "update cloud_windows set create_called_at = $2, hard_cap_at = $3, drain_until = $4,
                    teardown_until = $5
             where id = $1 and state = 'create_intent' and create_called_at is null",
        )
        .bind(window.id)
        .bind(now)
        .bind(now + hard_cap)
        .bind(drain_until)
        .bind(drain_until + grace(self.config.teardown_margin))
        .execute(&self.pool)
        .await?
        .rows_affected();
        if stamped == 0 {
            return Ok(());
        }
        // Профили пробуются по очереди внутри одной заявки. Перед каждым вызовом в строке
        // стоят конфигуратор и цена именно этого профиля: если ответ потеряется, сервер мог
        // появиться только у него, и ledger считает его цену.
        let profiles = window.profiles();
        let mut refusals: Vec<String> = Vec::new();
        for profile in &profiles {
            let marked = sqlx::query(
                "update cloud_windows set configurator_id = $2, price_hourly_kopecks = $3
                 where id = $1 and state = 'create_intent'",
            )
            .bind(window.id)
            .bind(profile.configurator_id)
            .bind(profile.hourly_kopecks)
            .execute(&self.pool)
            .await?
            .rows_affected();
            if marked == 0 {
                return Ok(());
            }
            let request = CreateServer {
                name: window.name.clone(),
                comment: "canterlot runner window".to_owned(),
                project_id: self.config.project_id,
                configurator_id: profile.configurator_id,
                cpu: profile.cpu,
                ram_mb: profile.ram_mb(),
                disk_mb: profile.disk_mb(),
                os_id: self.config.os_id,
                ssh_key_ids: vec![key_id],
                cloud_init: cloud_init.clone(),
                hostname: "canterlot-runner".to_owned(),
                network_id: self.config.network_id.clone(),
                local_ip: window
                    .local_ip
                    .clone()
                    .expect("local_ip allocated in create_in_tx"),
                availability_zone: profile.availability_zone.clone(),
            };
            let outcome = self.provider.create_server(&request).await;
            drop(request);
            match outcome {
                CreateOutcome::Created { server_id } => {
                    return self.identify(window.id, server_id, "create_intent").await;
                }
                CreateOutcome::Rejected {
                    code, error_code, ..
                } => {
                    refusals.push(format!("{}:{code}/{error_code}", profile.configurator_id));
                    // Сервера точно нет. Отказ из списка «обходится» следующим профилем,
                    // любой другой закрывает окно.
                    if !super::profile_fallback(code, &error_code) {
                        return self.fail(window.id, &refusals.join(",")).await;
                    }
                }
                // Сервер мог появиться: второго вызова, на другом профиле тоже, не будет.
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
        // Все профили отказали определённо: сервера нет, резерв снят.
        let reason = if refusals.is_empty() {
            "no_profiles".to_owned()
        } else {
            format!("all_profiles_rejected: {}", refusals.join(","))
        };
        self.fail(window.id, &reason).await
    }

    /// Сервер найден: окно становится `identified`, строка раннера `cloud` готова.
    async fn identify(
        &self,
        id: Uuid,
        server_id: i64,
        from: &'static str,
    ) -> Result<(), sqlx::Error> {
        let config = self.config.clone();
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move {
                windows::ensure_cloud_machine(conn, &config, true).await?;
                sqlx::query(
                    "update cloud_windows set state = 'identified', server_id = $2, last_error = null
                     where id = $1 and state = $3",
                )
                .bind(id)
                .bind(server_id)
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
            return self.delete_now(&window, "hard_cap", now).await;
        }
        if let Some(called) = window.create_called_at
            && now - called >= grace(self.config.ready_timeout)
        {
            return self.delete_now(&window, "ready_timeout", now).await;
        }
        let Some(server_id) = window.server_id else {
            return Ok(());
        };
        // Правило уже создано (или усыновлено из списка): подключаться можно сразу, без
        // повторного опроса сервера. Адрес подключения — публичный IP роутера.
        if window.dnat_rule_id.is_some() {
            if window.attach_sent_at.is_none() {
                self.send_attach(&window, now).await?;
            }
            return Ok(());
        }
        match self.provider.get_server(server_id).await {
            Ok(ServerLookup::Found(state)) => match state.status {
                // Сервер не включается, пока аккаунт не переведён на предоплату. Это
                // определённый отказ: ждём только отведенный срок с момента создания.
                ServerStatus::NoPaid => {
                    if window
                        .create_called_at
                        .is_some_and(|called| now - called >= grace(self.config.no_paid_grace))
                    {
                        self.no_paid_refusal(&window, now).await?;
                    }
                }
                ServerStatus::On => self.ensure_dnat(&window, now).await?,
                _ => {}
            },
            Ok(ServerLookup::NotFound) => {}
            Err(error) => {
                tracing::warn!(kind = "cloud_poll_server", error_kind = ?error.kind, "server poll failed");
            }
        }
        Ok(())
    }

    /// DNAT-правило создаётся ровно один раз. Неопределённый ответ не повторяется:
    /// метка вызова остаётся, а следующий проход ищет правило в полном списке роутера и
    /// усыновляет его. Полный список без правила снимает метку — повтор_CREATE безопасен.
    async fn ensure_dnat(&self, window: &Window, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        let (Some(local_ip), Some(port)) = (window.local_ip.as_deref(), window.dnat_port) else {
            return Ok(());
        };
        let port = match u16::try_from(port) {
            Ok(port) => port,
            Err(_) => return Ok(()),
        };
        let router_ip = self.config.router_ip.clone();
        if window.dnat_create_attempted_at.is_none() {
            let stamped = sqlx::query(
                "update cloud_windows set dnat_create_attempted_at = $2
                 where id = $1 and state = 'identified' and dnat_create_attempted_at is null",
            )
            .bind(window.id)
            .bind(now)
            .execute(&self.pool)
            .await?
            .rows_affected();
            if stamped == 0 {
                return Ok(());
            }
            let request = DnatRuleRequest {
                local_ip: local_ip.to_owned(),
                local_port: 22,
                public_ip: router_ip.clone(),
                public_port: port,
            };
            match self
                .provider
                .create_dnat_rule(&self.config.router_id, &request)
                .await
            {
                DnatCreateOutcome::Created { rule_id } => {
                    self.adopt_dnat(window.id, &rule_id).await?;
                }
                DnatCreateOutcome::Refused { .. } => {
                    sqlx::query(
                        "update cloud_windows set last_error = 'dnat_refused' where id = $1",
                    )
                    .bind(window.id)
                    .execute(&self.pool)
                    .await?;
                    return self.delete_now(window, "dnat_refused", now).await;
                }
                DnatCreateOutcome::Ambiguous { .. } => {
                    sqlx::query(
                        "update cloud_windows set last_error = 'dnat_create_unknown' where id = $1",
                    )
                    .bind(window.id)
                    .execute(&self.pool)
                    .await?;
                }
            }
            return Ok(());
        }
        // Вызов был, ответ нечитаем: полный список — единственный источник истины.
        match self.provider.list_dnat_rules(&self.config.router_id).await {
            Ok(rules) => {
                if let Some(rule) = rules
                    .iter()
                    .find(|rule| rule.matches(&router_ip, port, local_ip))
                {
                    self.adopt_dnat(window.id, &rule.id).await?;
                } else {
                    // Полный список подтвердил отсутствие правила: первый вызов точно не
                    // создал его, поэтому повтор для того же порта безопасен.
                    sqlx::query(
                        "update cloud_windows set dnat_create_attempted_at = null where id = $1",
                    )
                    .bind(window.id)
                    .execute(&self.pool)
                    .await?;
                }
            }
            Err(error) => {
                tracing::warn!(kind = "cloud_dnat_listing", error = %error, "dnat listing failed; nothing changed");
            }
        }
        Ok(())
    }

    async fn adopt_dnat(&self, id: Uuid, rule_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("update cloud_windows set dnat_rule_id = $2, last_error = null where id = $1")
            .bind(id)
            .bind(rule_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Сервер остался `no_paid` дольше срока: definite отказ. Окно уходит на удаление по
    /// обычному пути, блок в `cloud_budget` закрывает новые окна, владельцу пуш.
    async fn no_paid_refusal(
        &self,
        window: &Window,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        tracing::warn!(
            kind = "cloud_no_paid",
            "provider did not turn the server on: no_paid"
        );
        sqlx::query("update cloud_windows set last_error = 'no_paid' where id = $1")
            .bind(window.id)
            .execute(&self.pool)
            .await?;
        windows::set_provider_block(&self.pool, BLOCK_NO_PAID, now).await?;
        crate::notify::enqueue_cloud_alert(
            &self.pool,
            &format!("cloud:no_paid:{}", window.id),
            &json!({
                "code": "no_paid",
                "window_id": window.id,
                "name": window.name,
            }),
        )
        .await?;
        self.delete_now(window, REASON_NO_PAID, now).await
    }

    /// Команда несущей подключить раннер. Только если машина из конфигурации действительно
    /// несущая: иначе окно дождётся `ready_timeout` и уйдёт.
    async fn send_attach(&self, window: &Window, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        let config = self.config.clone();
        let window = window.clone();
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move {
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
                    "host": config.router_ip,
                    "ssh_port": window.dnat_port,
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

    // ───────── Работа окна ─────────

    /// Есть ли у раннера `cloud` работа: задачи на нём или ждущая задача, которую он взял бы
    /// (те же метки и допустимое место, что у размещения).
    async fn has_work(&self, machine: Uuid) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            "select exists(select 1 from jobs where machine_id = $1
                           and state in ('staging','queued','dispatched','waiting_lease','running','collecting'))
                 or exists(select 1 from jobs where state = 'waiting_runner' and carrier_id = $2
                           and labels <@ $3 and placement && array['runner','cloud']::text[])",
        )
        .bind(machine)
        .bind(self.config.carrier_machine_id)
        .bind(&self.config.labels)
        .fetch_one(&self.pool)
        .await
    }

    async fn step_ready(&self, window: Window, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
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

    async fn step_draining(&self, window: Window, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
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
        }
        Ok(())
    }

    /// Окно, которое живёт дольше предела с запасами: сервер удаляется, владельцу пуш.
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
        // Сервер сверх предела тарифицируется; публичного адреса больше нет.
        let extra =
            price::final_cost_kopecks(window.price_hourly_kopecks, minutes_between(cap, now))
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

    // ───────── Удаление ─────────

    async fn step_deleting(&self, window: Window, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        let Some(server_id) = window.server_id else {
            return Ok(());
        };
        if window.delete_accepted_at.is_none() {
            // Раннер отозван до удаления; повтор безвреден.
            windows::revoke_cloud_machine(&self.pool).await?;
            if window.next_delete_at.is_none_or(|at| at <= now) {
                self.try_delete(&window, server_id, now).await?;
            }
        }
        let mut window = {
            let mut conn = self.pool.acquire().await?;
            match windows::load(&mut conn, window.id).await? {
                Some(fresh) if fresh.state == "deleting" => fresh,
                _ => return Ok(()),
            }
        };
        if window.server_gone_at.is_none() && window.delete_accepted_at.is_some() {
            // Конец оплаты сервера подтверждает только полный список без него.
            match self
                .provider
                .list_project_servers(self.config.project_id)
                .await
            {
                Ok(list) if !list.iter().any(|server| server.id == server_id) => {
                    self.mark_server_gone(window.id, now).await?;
                    window.server_gone_at = Some(now);
                    window.next_delete_at = None;
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(kind = "cloud_confirm_listing", error = %error, "listing failed");
                }
            }
        }
        if window.server_gone_at.is_some() {
            self.finish_dnat(&window, now).await?;
        }
        Ok(())
    }

    /// Сервер пропал из полного списка (метка ставится один раз).
    async fn mark_server_gone(&self, id: Uuid, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        sqlx::query(
            "update cloud_windows set server_gone_at = coalesce(server_gone_at, $2),
                    next_delete_at = case when server_gone_at is null then null else next_delete_at end
             where id = $1 and state = 'deleting'",
        )
        .bind(id)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// DNAT-правило окна после удаления сервера. Открытое правило — обязательство: окно
    /// остаётся живым, пока полный список правил роутера не подтвердит его отсутствие.
    async fn finish_dnat(&self, window: &Window, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        if window.dnat_rule_id.is_none() || window.dnat_gone_at.is_some() {
            return self.end(&window.id, "ended", now).await;
        }
        let rule_id = window.dnat_rule_id.as_deref().expect("checked above");
        let router = &self.config.router_id;
        let list = match self.provider.list_dnat_rules(router).await {
            Ok(list) => list,
            Err(error) => {
                tracing::warn!(kind = "cloud_dnat_listing", error = %error, "dnat listing failed; nothing changed");
                return Ok(());
            }
        };
        if !list.iter().any(|rule| rule.id == rule_id) {
            // Полный список без правила: обязательство закрыто.
            sqlx::query(
                "update cloud_windows set dnat_gone_at = coalesce(dnat_gone_at, $2)
                 where id = $1",
            )
            .bind(window.id)
            .bind(now)
            .execute(&self.pool)
            .await?;
            return self.end(&window.id, "ended", now).await;
        }
        let retry_due = window.next_dnat_delete_at.is_none_or(|at| at <= now)
            && window
                .dnat_delete_accepted_at
                .is_none_or(|at| now - at >= TimeDelta::minutes(5));
        if !retry_due {
            return Ok(());
        }
        let attempts = window.dnat_delete_attempts.saturating_add(1);
        match self.provider.delete_dnat_rule(router, rule_id).await {
            DeleteOutcome::Deleted { .. } | DeleteOutcome::AlreadyGone => {
                sqlx::query(
                    "update cloud_windows set dnat_delete_accepted_at = $2,
                            dnat_delete_attempts = $3
                     where id = $1",
                )
                .bind(window.id)
                .bind(now)
                .bind(attempts)
                .execute(&self.pool)
                .await?;
                // Подтверждение тем же способом, что у сервера: правила нет в полном списке.
                if let Ok(list) = self.provider.list_dnat_rules(router).await
                    && !list.iter().any(|rule| rule.id == rule_id)
                {
                    sqlx::query(
                        "update cloud_windows set dnat_gone_at = coalesce(dnat_gone_at, $2)
                         where id = $1",
                    )
                    .bind(window.id)
                    .bind(now)
                    .execute(&self.pool)
                    .await?;
                    return self.end(&window.id, "ended", now).await;
                }
            }
            DeleteOutcome::NeedsConfirmation | DeleteOutcome::Failed { .. } => {
                sqlx::query(
                    "update cloud_windows set last_error = 'dnat_delete_failed',
                            dnat_delete_attempts = $2, next_dnat_delete_at = $3
                     where id = $1",
                )
                .bind(window.id)
                .bind(attempts)
                .bind(now + delete_backoff(attempts))
                .execute(&self.pool)
                .await?;
                if attempts >= DELETE_FAILURES_BEFORE_ALERT {
                    let since = window.delete_requested_at.unwrap_or(now);
                    let slot = (now - since).num_seconds().max(0) / ALERT_EVERY.num_seconds();
                    crate::notify::enqueue_cloud_alert(
                        &self.pool,
                        &format!("cloud:dnat_delete_failing:{}:{slot}", window.id),
                        &json!({"code": "dnat_delete_failing", "window_id": window.id, "name": window.name}),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn try_delete(
        &self,
        window: &Window,
        server_id: i64,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let outcome = self.provider.delete_server(server_id).await;
        let attempts = window.delete_attempts.saturating_add(1);
        match outcome {
            DeleteOutcome::Deleted { quarantined } => {
                sqlx::query(
                    "update cloud_windows set delete_accepted_at = $2,
                            last_error = case when delete_reason in ('no_paid','dnat_refused','ipv4_refused') then last_error end,
                            delete_attempts = $3,
                            alert_state = alert_state || jsonb_build_object('quarantined', $4::boolean)
                     where id = $1 and state = 'deleting'",
                )
                .bind(window.id)
                .bind(now)
                .bind(attempts)
                .bind(quarantined)
                .execute(&self.pool)
                .await?;
            }
            DeleteOutcome::AlreadyGone => {
                sqlx::query(
                    "update cloud_windows set delete_accepted_at = $2,
                            last_error = case when delete_reason in ('no_paid','dnat_refused','ipv4_refused') then last_error end,
                            delete_attempts = $3
                     where id = $1 and state = 'deleting'",
                )
                .bind(window.id)
                .bind(now)
                .bind(attempts)
                .execute(&self.pool)
                .await?;
            }
            DeleteOutcome::NeedsConfirmation => {
                // Токен не может удалить, сервер тарифицируется. Пуш сразу и с каждой
                // повторной попыткой раз в 30 минут.
                sqlx::query(
                    "update cloud_windows set last_error = 'needs_confirmation', delete_attempts = $2,
                            next_delete_at = $3
                     where id = $1 and state = 'deleting'",
                )
                .bind(window.id)
                .bind(attempts)
                .bind(now + ALERT_EVERY)
                .execute(&self.pool)
                .await?;
                crate::notify::enqueue_cloud_alert(
                    &self.pool,
                    &format!("cloud:delete_needs_confirmation:{}:{attempts}", window.id),
                    &json!({"code": "delete_needs_confirmation", "window_id": window.id, "name": window.name}),
                )
                .await?;
            }
            DeleteOutcome::Failed { .. } => {
                sqlx::query(
                    "update cloud_windows set last_error = 'delete_failed', delete_attempts = $2,
                            next_delete_at = $3
                     where id = $1 and state = 'deleting'",
                )
                .bind(window.id)
                .bind(attempts)
                .bind(now + delete_backoff(attempts))
                .execute(&self.pool)
                .await?;
                if attempts >= DELETE_FAILURES_BEFORE_ALERT {
                    let since = window.delete_requested_at.unwrap_or(now);
                    let slot = (now - since).num_seconds().max(0) / ALERT_EVERY.num_seconds();
                    crate::notify::enqueue_cloud_alert(
                        &self.pool,
                        &format!("cloud:delete_failing:{}:{slot}", window.id),
                        &json!({"code": "delete_failing", "window_id": window.id, "name": window.name}),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// Конец окна: итоговая цена от вызова создания до подтверждённого конца.
    async fn end(
        &self,
        id: &Uuid,
        to: &'static str,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let id = *id;
        crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move {
                let Some(window) = windows::load_for_update(&mut *conn, id).await? else {
                    return Ok::<_, sqlx::Error>(());
                };
                if !window.is_live() {
                    return Ok(());
                }
                let cost = ledger_cost(&window, window.server_gone_at.unwrap_or(now));
                sqlx::query(
                    "update cloud_windows set state = $2, final_cost_kopecks = $3,
                            delete_confirmed_at = $4,
                            last_error = case when delete_reason in ('no_paid','dnat_refused','ipv4_refused') then last_error end
                     where id = $1",
                )
                .bind(id)
                .bind(to)
                .bind(cost)
                .bind(now)
                .execute(&mut *conn)
                .await?;
                Ok(())
            })
        })
        .await
    }

    // ───────── Сверка ─────────

    /// Сверка проекта со строками окон. `Err(())` — список не получен целиком: ничего не
    /// менялось.
    async fn reconcile(&self, now: DateTime<Utc>) -> Result<(), ()> {
        let listing = match self
            .provider
            .list_project_servers(self.config.project_id)
            .await
        {
            Ok(listing) => listing,
            Err(error) => {
                tracing::warn!(kind = "cloud_reconcile_listing", error = %error, "project listing failed; nothing changed");
                return Err(());
            }
        };
        self.apply_listing(&listing, now).await.map_err(|error| {
            warn_db("cloud_reconcile", &error);
        })?;
        // Правила роутера идут отдельно: список не получен целиком — ничего не меняем.
        if let Err(error) = self.delete_stray_dnat_rules(now).await {
            warn_db("cloud_reconcile_dnat", &error);
        }
        Ok(())
    }

    /// Лишние DNAT-правила роутера. Удаляются только правила с публичным портом внутри
    /// настроенного диапазона, которых не держит ни одно живое окно. Правила вне диапазона
    /// и публичный адрес самого роутера не трогаются никогда. Неполный список не меняет
    /// ничего: удалять можно только то, что прочитано целиком.
    async fn delete_stray_dnat_rules(&self, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        let Some((first_port, last_port)) = self.config.dnat_port_values() else {
            return Ok(());
        };
        let held: Vec<Option<i32>> =
            sqlx::query_scalar("select dnat_port from cloud_windows where state = any($1)")
                .bind(windows::live_states())
                .fetch_all(&self.pool)
                .await?;
        let held: std::collections::HashSet<u16> = held
            .into_iter()
            .filter_map(|port| port.and_then(|port| u16::try_from(port).ok()))
            .collect();
        let list = match self.provider.list_dnat_rules(&self.config.router_id).await {
            Ok(list) => list,
            Err(error) => {
                tracing::warn!(kind = "cloud_dnat_listing", error = %error, "dnat listing failed; nothing changed");
                return Ok(());
            }
        };
        for rule in list.iter().filter(|rule| {
            rule.public_port
                .parse::<u16>()
                .is_ok_and(|port| (first_port..=last_port).contains(&port) && !held.contains(&port))
        }) {
            let payload = |code: &str| json!({"code": code, "rule_id": rule.id, "public_port": rule.public_port});
            match self
                .provider
                .delete_dnat_rule(&self.config.router_id, &rule.id)
                .await
            {
                DeleteOutcome::Deleted { .. } | DeleteOutcome::AlreadyGone => {
                    crate::notify::enqueue_cloud_alert(
                        &self.pool,
                        &format!("cloud:stray_dnat:{}", rule.id),
                        &payload("stray_dnat_deleted"),
                    )
                    .await?;
                }
                DeleteOutcome::NeedsConfirmation | DeleteOutcome::Failed { .. } => {
                    let slot = now.timestamp() / ALERT_EVERY.num_seconds();
                    crate::notify::enqueue_cloud_alert(
                        &self.pool,
                        &format!("cloud:stray_dnat_stuck:{}:{slot}", rule.id),
                        &payload("stray_dnat_stuck"),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn apply_listing(
        &self,
        listing: &[ServerSummary],
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let mut named: std::collections::HashMap<Uuid, &ServerSummary> =
            std::collections::HashMap::new();
        for server in listing {
            if let Some(id) = window_id_from_name(&server.name) {
                named.insert(id, server);
            }
        }
        let ids: Vec<Uuid> = named.keys().copied().collect();
        let windows: Vec<Window> = sqlx::query_as(
            "select * from cloud_windows where state = any($1) or id = any($2) order by created_at, id",
        )
        .bind(windows::live_states())
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?;
        let unknown_for = grace(self.config.create_unknown_window);
        let mut known = std::collections::HashSet::new();
        for window in &windows {
            known.insert(window.id);
            let listed = named.get(&window.id);
            match (window.state.as_str(), listed) {
                ("create_unknown", Some(server)) => {
                    self.identify(window.id, server.id, "create_unknown")
                        .await?;
                }
                // Вызов создания начат, а хаб упал до ответа: сервер уже виден по имени.
                ("create_intent", Some(server)) if window.create_called_at.is_some() => {
                    self.identify(window.id, server.id, "create_intent").await?;
                }
                ("create_unknown", None) => {
                    if window
                        .create_called_at
                        .is_some_and(|called| now - called >= unknown_for)
                    {
                        self.fail(window.id, "not_found_in_listing").await?;
                    }
                }
                ("identified" | "ready" | "draining", None) => {
                    // Только когда сервер уже должен был попасть в список: у провайдера он
                    // может появиться с задержкой.
                    if window
                        .create_called_at
                        .is_some_and(|called| now - called >= unknown_for)
                    {
                        self.gone(window, now).await?;
                    }
                }
                // Сервера нет без принятого удаления (владелец удалил в панели): оплата
                // сервера кончилась; окно закончит шаг удаления, когда уйдёт и адрес.
                ("deleting", None) if window.server_id.is_some() => {
                    self.mark_server_gone(window.id, now).await?;
                }
                (state, Some(server)) if !windows::LIVE.contains(&state) => {
                    self.delete_stray(server, now).await?;
                }
                _ => {}
            }
        }
        for (id, server) in named {
            if !known.contains(&id) {
                self.delete_stray(server, now).await?;
            }
        }
        Ok(())
    }

    /// Сервер окна пропал из полного списка, хотя удаление не запрашивалось.
    async fn gone(&self, window: &Window, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
        let id = window.id;
        let ended = crate::db::in_tx(&self.pool, move |conn| {
            Box::pin(async move {
                let Some(window) = windows::load_for_update(&mut *conn, id).await? else {
                    return Ok::<_, sqlx::Error>(false);
                };
                if !matches!(window.state.as_str(), "identified" | "ready" | "draining") {
                    return Ok(false);
                }
                let cost = ledger_cost(&window, now);
                sqlx::query(
                    "update cloud_windows set state = 'gone', final_cost_kopecks = $2,
                            delete_confirmed_at = $3, last_error = 'server_missing'
                     where id = $1",
                )
                .bind(id)
                .bind(cost)
                .bind(now)
                .execute(&mut *conn)
                .await?;
                sqlx::query(
                    "update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, now())
                     where kind = 'attach_runner' and payload->>'window_id' = $1 and delivery_state <> 'completed'",
                )
                .bind(id.to_string())
                .execute(&mut *conn)
                .await?;
                Ok(true)
            })
        })
        .await?;
        if ended {
            windows::revoke_cloud_machine(&self.pool).await?;
            crate::notify::enqueue_cloud_alert(
                &self.pool,
                &format!("cloud:window_gone:{}", window.id),
                &json!({"code": "window_gone", "window_id": window.id, "name": window.name}),
            )
            .await?;
        }
        Ok(())
    }

    /// Сервер с нашим именем без живого окна: удаляется и сообщается владельцу.
    async fn delete_stray(
        &self,
        server: &ServerSummary,
        now: DateTime<Utc>,
    ) -> Result<(), sqlx::Error> {
        let payload =
            |code: &str| json!({"code": code, "name": server.name, "server_id": server.id});
        match self.provider.delete_server(server.id).await {
            DeleteOutcome::Deleted { .. } | DeleteOutcome::AlreadyGone => {
                crate::notify::enqueue_cloud_alert(
                    &self.pool,
                    &format!("cloud:stray:{}:{}", server.id, server.name),
                    &payload("stray_server_deleted"),
                )
                .await?;
            }
            DeleteOutcome::NeedsConfirmation | DeleteOutcome::Failed { .. } => {
                let slot = now.timestamp() / ALERT_EVERY.num_seconds();
                crate::notify::enqueue_cloud_alert(
                    &self.pool,
                    &format!("cloud:stray_stuck:{}:{slot}", server.id),
                    &payload("stray_server_stuck"),
                )
                .await?;
            }
        }
        Ok(())
    }
}

/// Служба по конфигурации хаба. Нет таблицы `[runners.cloud]` — облако выключено. Токен
/// читается из файла и дальше живёт только внутри клиента. Всё, что мешает включению
/// (негодная конфигурация, нечитаемый токен, несущая не помечена), пишется предупреждением с
/// кодом причины и без значений, а облако остаётся выключенным: окно без несущей только жгло
/// бы деньги до `ready_timeout`.
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
    let provider = match super::TimewebProvider::new(super::TimewebConfig::new(&config.token_file))
    {
        Ok(provider) => provider,
        Err(error) => {
            // Текст ошибки — только вид сбоя ввода-вывода, без содержимого файла.
            disabled(&error.to_string());
            return None;
        }
    };
    let carrier: Result<Option<bool>, sqlx::Error> =
        sqlx::query_scalar("select role = 'worker' and is_carrier from machines where id = $1")
            .bind(config.carrier_machine_id)
            .fetch_optional(pool)
            .await;
    match carrier {
        Ok(Some(true)) => {}
        Ok(_) => {
            disabled("carrier_machine_is_not_a_carrier");
            return None;
        }
        Err(error) => {
            disabled(&format!("database: {}", crate::dberr::describe(&error)));
            return None;
        }
    }
    tracing::info!(
        kind = "cloud_enabled",
        project_id = config.project_id,
        profiles = config.profile.len(),
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
