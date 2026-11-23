//! Облачные окна в хабе (этап C2): транзакция создания и деньги, жизнь окна от создания до
//! конца оплаты, сверка проекта и строка раннера `cloud`. Против поддельного Timeweb: счётчики
//! запросов поддельного сервера доказывают, что второго создания не бывает.
//!
//! Время идёт параметром прохода службы (`tick(now)`): сроки проверяются без ожидания.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use canterlot_hub::cloud::service::CloudService;
use canterlot_hub::cloud::windows::{
    self, CreateRefusal, CreateRequest, Trigger, Window, WindowId,
};
use canterlot_hub::cloud::{TimewebConfig, TimewebProvider, price};
use canterlot_hub::config::{CloudConfig, CloudProfile};
use canterlot_hub::inplace_jobs::place;
use canterlot_hub::machines::{self, RunnerRefusal};
use canterlot_hub::session::Registry;
use canterlot_hub::testkit::{self, FakeSession};
use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use sqlx::PgPool;
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

use crate::support::fake_timeweb::{CreateMode, DeleteMode, FailPage, FakeTimeweb};

const TOKEN: &str = "tw-windows-token-5d1c0e9a";
const PROJECT: i64 = 4242;
const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAMPLE0000";
/// Конфигуратор 151, 32 vCPU, 64 ГБ, 80 ГБ: 2845 копеек в час (см. `cloud_timeweb`).
const HOURLY: i64 = 2845;
const ROUTER: &str = "018f0000-0000-7000-8000-0000000000aa";
const ROUTER_IP: &str = "192.0.2.1";
const GATEWAY: &str = "192.0.2.5";

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// «Сейчас» с точностью до микросекунд, как хранит база: метки из проходов сравниваются с
/// прочитанными из строк без округления.
fn now_micros() -> DateTime<Utc> {
    Utc.timestamp_micros(Utc::now().timestamp_micros())
        .single()
        .unwrap()
}

struct Rig {
    pool: PgPool,
    fake: FakeTimeweb,
    service: Arc<CloudService>,
    registry: Registry,
    owner: Uuid,
    carrier: Uuid,
    config: CloudConfig,
    max_hourly: i64,
    _dir: tempfile::TempDir,
}

fn cloud_config(carrier: Uuid) -> CloudConfig {
    toml::from_str(&format!(
        r#"
provider = "timeweb"
token_file = "token"
project_id = {PROJECT}
runner_public_key = "{KEY}"
carrier_machine_id = "{carrier}"
network_id = "network-example"
network_cidr = "192.0.2.0/24"
router_id = "{ROUTER}"
router_ip = "{ROUTER_IP}"
gateway = "{GATEWAY}"
local_ip_pool = "192.0.2.10-192.0.2.60"
dnat_port_range = "22100-22199"

[[profile]]
configurator_id = 151
cpu = 32
ram_gb = 64
disk_gb = 80
availability_zone = "msk-1"
"#
    ))
    .unwrap()
}

async fn rig(pool: PgPool) -> Rig {
    rig_with(pool, |_| {}).await
}

async fn rig_with(pool: PgPool, tweak: impl FnOnce(&mut CloudConfig)) -> Rig {
    let owner = crate::runners_hub::owner(&pool).await;
    let carrier = machines::add(&pool, owner, "carrier").await.unwrap();
    machines::set_carrier(&pool, carrier.machine_id, true)
        .await
        .unwrap();
    let fake = FakeTimeweb::start(TOKEN).await;
    // Денежные пуши владельцу иначе не попадают в `owner_pushes`.
    crate::auth_pushes::enable(&pool).await;
    let dir = tempfile::tempdir().unwrap();
    let token_file = dir.path().join("token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).unwrap();
    let mut provider_config = TimewebConfig::new(&token_file);
    provider_config.base_url = fake.base_url.clone();
    provider_config.request_timeout = Duration::from_secs(3);
    provider_config.max_rate_limit_retries = 1;
    provider_config.default_backoff = Duration::from_millis(5);
    provider_config.max_backoff = Duration::from_millis(20);
    let provider = Arc::new(TimewebProvider::new(provider_config).unwrap());
    let mut config = cloud_config(carrier.machine_id);
    tweak(&mut config);
    let registry = Registry::new(Default::default());
    let service = CloudService::new(pool.clone(), registry.clone(), provider, config.clone());
    // Резерв считается по самому дорогому профилю списка.
    let max_hourly = service
        .pricing()
        .await
        .ok()
        .and_then(|pricing| pricing.max_hourly_kopecks())
        .unwrap_or(HOURLY);
    Rig {
        pool,
        fake,
        service,
        registry,
        owner,
        carrier: carrier.machine_id,
        config,
        max_hourly,
        _dir: dir,
    }
}

impl Rig {
    fn reservation(&self, cap: u32) -> i64 {
        windows::reservation_for(&self.config, self.max_hourly, cap).unwrap()
    }

    async fn create(&self, cap: u32) -> WindowId {
        self.try_create(cap).await.expect("window created")
    }

    async fn try_create(&self, cap: u32) -> Result<WindowId, CreateRefusal> {
        self.service
            .create(CreateRequest {
                trigger: Trigger::Owner,
                proposal_id: None,
                hard_cap_minutes: cap,
                consent_over_budget_kopecks: Some(self.reservation(cap)),
            })
            .await
    }

    async fn window(&self, id: WindowId) -> Window {
        let mut conn = self.pool.acquire().await.unwrap();
        windows::load(&mut conn, id).await.unwrap().unwrap()
    }

    async fn state(&self, id: WindowId) -> String {
        self.window(id).await.state
    }

    async fn tick(&self, at: DateTime<Utc>) {
        self.service.tick(at).await;
    }

    /// Вызовы создания сервера.
    fn posts(&self) -> usize {
        self.fake
            .requests()
            .iter()
            .filter(|r| r.method == "POST" && r.path == "/api/v1/servers")
            .count()
    }

    /// Создания DNAT-правила роутера.
    fn dnat_posts(&self) -> usize {
        self.fake.control().count("POST", "/api/v1/routers/")
    }

    /// Все серверы пропали у провайдера.
    fn vanish_servers(&self) {
        let mut state = self.fake.control();
        let ids: Vec<i64> = state.servers.keys().copied().collect();
        for id in ids {
            state.remove_server(id);
        }
    }

    fn deletes(&self) -> usize {
        self.fake.control().count("DELETE", "/api/v1/servers")
    }

    async fn cloud_machine(&self) -> Uuid {
        let mut conn = self.pool.acquire().await.unwrap();
        windows::cloud_machine_id(&mut conn)
            .await
            .unwrap()
            .expect("the cloud machine row exists")
    }

    /// Несущая на связи с `runners_v1`.
    fn carrier_session(&self) -> FakeSession {
        testkit::fake_session(&self.registry, self.carrier, &["runners_v1"], true)
    }

    /// Подключение раннера, как его сделает несущая: токен по запросу и воркер на связи.
    async fn attach(&self) -> FakeSession {
        let machine = self.cloud_machine().await;
        machines::request_runner_token(&self.pool, self.carrier, machine, false)
            .await
            .expect("the cloud token is issued while the window serves");
        testkit::fake_session(&self.registry, machine, &[], true)
    }

    /// Окно доведено до `ready`; возвращает сессию раннера и момент последнего прохода.
    async fn drive_to_ready(
        &self,
        cap: u32,
        t0: DateTime<Utc>,
    ) -> (WindowId, FakeSession, DateTime<Utc>) {
        let id = self.create(cap).await;
        self.tick(t0).await;
        assert_eq!(self.state(id).await, "identified");
        // Первый проход после `On` создаёт DNAT-правило, второй шлёт команду подключения.
        self.tick(t0 + TimeDelta::seconds(1)).await;
        assert!(self.window(id).await.dnat_rule_id.is_some());
        self.tick(t0 + TimeDelta::seconds(2)).await;
        assert!(self.window(id).await.attach_sent_at.is_some());
        let session = self.attach().await;
        let at = t0 + TimeDelta::seconds(3);
        self.tick(at).await;
        assert_eq!(self.state(id).await, "ready");
        (id, session, at)
    }

    async fn alerts(&self) -> Vec<serde_json::Value> {
        sqlx::query_scalar(
            "select payload from owner_pushes where kind = 'cloud_alert' order by created_at, id",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn alert_codes(&self) -> Vec<String> {
        self.alerts()
            .await
            .iter()
            .map(|payload| payload["code"].as_str().unwrap().to_owned())
            .collect()
    }

    async fn seed_job(&self, machine: Option<Uuid>, state: &str, labels: &[&str]) -> Uuid {
        let id = Uuid::now_v7();
        let labels: Vec<String> = labels.iter().map(|label| (*label).to_owned()).collect();
        sqlx::query(
            "insert into jobs (id, owner_id, domain, machine_id, remote, workdir, shell,
                               timeout_seconds, state, labels, carrier_id)
             values ($1, $2, 'personal', $3, true,
                     case when $3::uuid is null then null else '/srv/canterlot-runner/jobs/personal/x' end,
                     'true', 600, $4, $5, $6)",
        )
        .bind(id)
        .bind(self.owner)
        .bind(machine)
        .bind(state)
        .bind(&labels)
        .bind(self.carrier)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    async fn job_state(&self, id: Uuid) -> String {
        sqlx::query_scalar("select state from jobs where id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// Ждущая задача с видами места и, если задан, названной машиной.
    async fn seed_waiting_job(
        &self,
        labels: &[&str],
        placement: &[&str],
        requested: Option<Uuid>,
    ) -> Uuid {
        let id = Uuid::now_v7();
        let labels: Vec<String> = labels.iter().map(|label| (*label).to_owned()).collect();
        let placement: Vec<String> = placement.iter().map(|kind| (*kind).to_owned()).collect();
        sqlx::query(
            "insert into jobs (id, owner_id, domain, remote, shell, timeout_seconds, state, labels,
                               placement, requested_machine_id, carrier_id, expected_minutes)
             values ($1, $2, 'personal', true, 'true', 600, 'waiting_runner', $3, $4, $5, $6, 30)",
        )
        .bind(id)
        .bind(self.owner)
        .bind(&labels)
        .bind(&placement)
        .bind(requested)
        .bind(self.carrier)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    /// Ждущая задача с исходниками: её можно привязать к раннеру.
    async fn seed_sourced_waiting(&self, labels: &[&str], placement: &[&str]) -> Uuid {
        let id = Uuid::now_v7();
        let labels: Vec<String> = labels.iter().map(|label| (*label).to_owned()).collect();
        let placement: Vec<String> = placement.iter().map(|kind| (*kind).to_owned()).collect();
        sqlx::query(
            "insert into jobs (id, owner_id, domain, remote, shell, timeout_seconds, state, labels,
                               placement, carrier_id, expected_minutes, source_repo, source_rev,
                               source_domain)
             values ($1, $2, 'personal', true, 'true', 600, 'waiting_runner', $3, $4, $5, 30,
                     '/srv/src/app', '0123456789abcdef0123456789abcdef01234567', 'personal')",
        )
        .bind(id)
        .bind(self.owner)
        .bind(&labels)
        .bind(&placement)
        .bind(self.carrier)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }
}

async fn enable_pushes(pool: &PgPool) {
    crate::auth_pushes::enable(pool).await;
}

// ───────── Конфигурация ─────────

fn load_config(extra: &str) -> Result<canterlot_hub::config::HubConfig, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    std::fs::write(
        &path,
        format!(
            "database_url_file = 'db'\ngrpc_listen = '127.0.0.1:7443'\ntls_cert = 'c'\n\
             tls_key = 'k'\nmanifest = 'm'\n{extra}"
        ),
    )
    .unwrap();
    canterlot_hub::config::HubConfig::load(&path).map_err(|error| error.to_string())
}

const CLOUD_HEAD: &str = "[runners.cloud]\nprovider = 'timeweb'\ntoken_file = 'tw-token'\n\
    project_id = 7\n\
    runner_public_key = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAMPLE0000'\n\
    carrier_machine_id = '018f0000-0000-7000-8000-000000000001'\n\
    network_id = 'network-example'\nnetwork_cidr = '192.0.2.0/24'\n\
    router_id = '018f0000-0000-7000-8000-0000000000aa'\n\
    router_ip = '192.0.2.1'\ngateway = '192.0.2.5'\n\
    local_ip_pool = '192.0.2.10-192.0.2.60'\ndnat_port_range = '22100-22199'\n";

const CLOUD_PROFILE: &str = "[[runners.cloud.profile]]\nconfigurator_id = 151\ncpu = 32\n\
    ram_gb = 64\ndisk_gb = 80\navailability_zone = 'msk-1'\n";

/// Таблица с ключами `extra` до списка профилей (после него ключи попали бы в профиль).
fn cloud_table(extra: &str) -> String {
    format!("{CLOUD_HEAD}{extra}{CLOUD_PROFILE}")
}

#[test]
fn cloud_config_is_off_when_absent_and_has_the_documented_defaults() {
    let config = load_config("").unwrap();
    assert!(config.runners.cloud.is_none());

    let config = load_config(&cloud_table("")).unwrap();
    let cloud = config.runners.cloud.unwrap();
    assert_eq!(cloud.profile.len(), 1);
    assert_eq!(cloud.profile[0].configurator_id, 151);
    assert_eq!(
        (
            cloud.profile[0].cpu,
            cloud.profile[0].ram_gb,
            cloud.profile[0].disk_gb
        ),
        (32, 64, 80)
    );
    assert_eq!(cloud.os_id, 109);
    assert_eq!(cloud.labels, vec!["cloud", "kernel-6.12"]);
    assert_eq!(cloud.ready_timeout, Duration::from_secs(15 * 60));
    assert_eq!(cloud.idle_minutes, 15);
    assert_eq!(cloud.drain_grace, Duration::from_secs(30 * 60));
    assert_eq!(cloud.teardown_margin, Duration::from_secs(30 * 60));
    assert_eq!(cloud.reconcile_interval, Duration::from_secs(5 * 60));
    assert_eq!(cloud.create_unknown_window, Duration::from_secs(10 * 60));
    assert_eq!(cloud.no_paid_grace, Duration::from_secs(3 * 60));
    assert_eq!(cloud.profile[0].availability_zone, "msk-1");
    assert_eq!(
        cloud.network_cidr(),
        Some(("192.0.2.0".parse().unwrap(), 24))
    );
    assert!(cloud.in_network("192.0.2.5".parse().unwrap()));
    assert!(!cloud.in_network("198.51.100.5".parse().unwrap()));
    assert_eq!(cloud.dnat_port_values(), Some((22100, 22199)));
    // Путь к файлу токена считается от каталога конфигурации.
    assert!(cloud.token_file.is_absolute() || cloud.token_file.ends_with("tw-token"));
    // `Debug` не показывает ни путь к токену, ни ключ.
    let shown = format!("{cloud:?}");
    assert!(!shown.contains("tw-token") && !shown.contains("AAAAC3"));
}

#[test]
fn cloud_config_rejects_bad_values_with_a_code_that_has_no_values() {
    let table = cloud_table("");
    let nine = (0..9).fold(cloud_table(""), |all, _| format!("{all}{CLOUD_PROFILE}"));
    let bad = [
        table.replace("'timeweb'", "'aws'"),
        table.replace("cpu = 32", "cpu = 0"),
        table.replace("configurator_id = 151", "configurator_id = 0"),
        table.replace("project_id = 7", "project_id = 0"),
        table.replace("ssh-ed25519 AAAAC3", "ssh-ed25519 not base64 !"),
        cloud_table("labels = ['has space']\n"),
        cloud_table("labels = []\n"),
        cloud_table("max_running = 0\n"),
        cloud_table("drain_grace = '10s'\n"),
        // Сеть и роутер: пустые ключи, адреса и пулы вне CIDR, негодные порты.
        table.replace("network_cidr = '192.0.2.0/24'\n", ""),
        table.replace("gateway = '192.0.2.5'", "gateway = '198.51.100.5'"),
        table.replace("192.0.2.10-192.0.2.60", "198.51.100.10-198.51.100.60"),
        table.replace(
            "dnat_port_range = '22100-22199'",
            "dnat_port_range = '100-200'",
        ),
        table.replace("availability_zone = 'msk-1'", "availability_zone = ''"),
        cloud_table("unknown_key = 1\n"),
        // Нет профилей, их больше допустимого, или остались прежние плоские ключи.
        CLOUD_HEAD.to_owned(),
        nine,
        cloud_table("cpu = 32\n"),
    ];
    for table in bad {
        let error = load_config(&table).expect_err(&table);
        assert!(
            !error.contains("AAAAC3") && !error.contains("tw-token"),
            "no values in {error}"
        );
    }
}

// ───────── Деньги: создание ─────────

#[test]
fn the_month_is_counted_in_moscow_time() {
    let at = |y, m, d, h, min, s| Utc.with_ymd_and_hms(y, m, d, h, min, s).unwrap();
    assert_eq!(windows::month_of(at(2026, 11, 30, 20, 59, 59)), "2026-11");
    assert_eq!(windows::month_of(at(2026, 11, 30, 21, 0, 0)), "2026-12");
    assert_eq!(windows::month_of(at(2026, 12, 31, 21, 30, 0)), "2027-01");
    assert_eq!(windows::month_of(at(2026, 6, 15, 0, 0, 0)), "2026-06");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_reservation_follows_config_and_the_price_module(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    // Цена часа берётся у конфигуратора провайдера.
    let pricing = rig.service.pricing().await?;
    assert_eq!(pricing.profiles.len(), 1);
    assert_eq!(pricing.profiles[0].hourly_kopecks, HOURLY);
    assert_eq!(pricing.max_hourly_kopecks(), Some(HOURLY));
    // 60 + 30 + 30 = 120 минут: 2 часа и час запаса.
    let id = rig.create(60).await;
    let window = rig.window(id).await;
    assert_eq!(window.reservation_kopecks, 3 * HOURLY);
    assert_eq!(
        window.reservation_kopecks,
        price::reservation_kopecks(HOURLY, 120)?,
        "only the server is reserved"
    );
    assert_eq!(window.price_hourly_kopecks, HOURLY);
    assert_eq!(window.month, windows::month_of(window.created_at));
    assert_eq!(window.name, format!("canterlot-runner-{id}"));
    windows::fail_in_tx(&mut *rig.pool.acquire().await?, id, "test").await?;
    // 61 минута округляется вверх: 121 минута → 3 часа и час запаса.
    let id = rig.create(61).await;
    assert_eq!(rig.window(id).await.reservation_kopecks, 4 * HOURLY);
    windows::fail_in_tx(&mut *rig.pool.acquire().await?, id, "test").await?;

    // Срок на доработку и запас на снос идут из конфигурации.
    let rig2 = rig_with(rig.pool.clone(), |config| {
        config.drain_grace = Duration::from_secs(45 * 60);
        config.teardown_margin = Duration::from_secs(60);
    })
    .await;
    let id = rig2.create(60).await;
    // 60 + 45 + 1 = 106 минут: 2 часа и час запаса.
    assert_eq!(rig2.window(id).await.reservation_kopecks, 3 * HOURLY);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn with_a_zero_budget_only_the_exact_consent_passes(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let reservation = rig.reservation(60);
    let attempt = |consent: Option<i64>| {
        let service = rig.service.clone();
        async move {
            service
                .create(CreateRequest {
                    trigger: Trigger::Card,
                    proposal_id: Some(Uuid::now_v7()),
                    hard_cap_minutes: 60,
                    consent_over_budget_kopecks: consent,
                })
                .await
        }
    };
    for consent in [None, Some(reservation - 1), Some(reservation + 1), Some(0)] {
        match attempt(consent).await {
            Err(CreateRefusal::OverBudget {
                reservation_kopecks,
                remaining_kopecks,
            }) => {
                assert_eq!(reservation_kopecks, reservation);
                assert_eq!(remaining_kopecks, 0);
            }
            other => panic!("consent {consent:?} must be refused, got {other:?}"),
        }
    }
    let count: i64 = sqlx::query_scalar("select count(*) from cloud_windows")
        .fetch_one(&rig.pool)
        .await?;
    assert_eq!(count, 0, "a refusal leaves no row");
    let id = attempt(Some(reservation)).await?;
    let window = rig.window(id).await;
    assert_eq!(
        (window.trigger.as_str(), window.state.as_str()),
        ("card", "create_intent")
    );
    assert!(window.proposal_id.is_some());
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_budget_counts_ended_final_cost_and_live_reservations(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let reservation = rig.reservation(60);
    sqlx::query("update cloud_budget set monthly_budget_kopecks = $1")
        .bind(2 * reservation)
        .execute(&rig.pool)
        .await?;
    // Помещается без согласия.
    let first = rig
        .service
        .create(CreateRequest {
            trigger: Trigger::Owner,
            proposal_id: None,
            hard_cap_minutes: 60,
            consent_over_budget_kopecks: None,
        })
        .await?;
    // Окно закончилось дешевле резерва: расход месяца — итоговая цена.
    sqlx::query("update cloud_windows set state = 'ended', final_cost_kopecks = $2 where id = $1")
        .bind(first)
        .bind(reservation - 1000)
        .execute(&rig.pool)
        .await?;
    let mut conn = rig.pool.acquire().await?;
    let month = rig.window(first).await.month;
    assert_eq!(
        windows::spent_in_month(&mut conn, &month).await?,
        reservation - 1000
    );
    drop(conn);
    // Второе помещается: (резерв − 1000) + резерв ≤ 2 × резерв.
    let second = rig
        .service
        .create(CreateRequest {
            trigger: Trigger::Owner,
            proposal_id: None,
            hard_cap_minutes: 60,
            consent_over_budget_kopecks: None,
        })
        .await?;
    // Живое окно считается по резерву; в месяц попадает целиком.
    sqlx::query("update cloud_windows set state = 'ended', final_cost_kopecks = $2 where id = $1")
        .bind(second)
        .bind(reservation)
        .execute(&rig.pool)
        .await?;
    // Теперь расход 2 × резерв − 1000: третье окно не помещается.
    let refused = rig
        .service
        .create(CreateRequest {
            trigger: Trigger::Owner,
            proposal_id: None,
            hard_cap_minutes: 60,
            consent_over_budget_kopecks: None,
        })
        .await;
    match refused {
        Err(CreateRefusal::OverBudget {
            remaining_kopecks, ..
        }) => assert_eq!(remaining_kopecks, 1000),
        other => panic!("expected over budget, got {other:?}"),
    }
    // Окно прошлого месяца в расход не входит.
    sqlx::query("update cloud_windows set month = '2000-01'")
        .execute(&rig.pool)
        .await?;
    rig.service
        .create(CreateRequest {
            trigger: Trigger::Owner,
            proposal_id: None,
            hard_cap_minutes: 60,
            consent_over_budget_kopecks: None,
        })
        .await?;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn only_one_window_is_live_at_a_time(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let first = rig.create(60).await;
    for state in [
        "create_intent",
        "create_unknown",
        "identified",
        "ready",
        "draining",
        "deleting",
    ] {
        sqlx::query("update cloud_windows set state = $2 where id = $1")
            .bind(first)
            .bind(state)
            .execute(&rig.pool)
            .await?;
        assert!(
            matches!(
                rig.try_create(60).await,
                Err(CreateRefusal::LiveWindowExists)
            ),
            "{state} is live"
        );
    }
    for state in ["ended", "failed", "gone"] {
        sqlx::query("update cloud_windows set state = $2, final_cost_kopecks = 0 where id = $1")
            .bind(first)
            .bind(state)
            .execute(&rig.pool)
            .await?;
        rig.try_create(60)
            .await
            .expect("a closed window frees the slot");
        sqlx::query(
            "update cloud_windows set state = 'failed', final_cost_kopecks = 0 where id <> $1",
        )
        .bind(first)
        .execute(&rig.pool)
        .await?;
    }
    // И база держит то же правило сама, помимо замка.
    let duplicate = sqlx::query(
        "insert into cloud_windows (id, name, state, trigger, host_key_public, price_hourly_kopecks,
                                    reservation_kopecks, month, hard_cap_minutes, created_at, profiles)
         select $1, 'canterlot-runner-' || $1::text, 'ready', 'owner', 'k', 1, 1, '2026-11', 60, now(),
                '[{\"configurator_id\":151,\"cpu\":1,\"ram_gb\":1,\"disk_gb\":10,\"hourly_kopecks\":1}]'::jsonb",
    )
    .bind(Uuid::now_v7())
    .execute(&rig.pool)
    .await;
    // Предыдущий цикл оставил `ended`, живого нет: вставка проходит один раз, второй нет.
    duplicate.expect("the first live row");
    let second = sqlx::query(
        "insert into cloud_windows (id, name, state, trigger, host_key_public, price_hourly_kopecks,
                                    reservation_kopecks, month, hard_cap_minutes, created_at, profiles)
         select $1, 'canterlot-runner-' || $1::text, 'ready', 'owner', 'k', 1, 1, '2026-11', 60, now(),
                '[{\"configurator_id\":151,\"cpu\":1,\"ram_gb\":1,\"disk_gb\":10,\"hourly_kopecks\":1}]'::jsonb",
    )
    .bind(Uuid::now_v7())
    .execute(&rig.pool)
    .await;
    assert!(second.is_err(), "the unique index allows one live row");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_creates_make_exactly_one_window(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    sqlx::query("update cloud_budget set monthly_budget_kopecks = 10000000")
        .execute(&rig.pool)
        .await?;
    let request = || CreateRequest {
        trigger: Trigger::Owner,
        proposal_id: None,
        hard_cap_minutes: 60,
        consent_over_budget_kopecks: None,
    };
    let results = tokio::join!(
        rig.service.create(request()),
        rig.service.create(request()),
        rig.service.create(request()),
        rig.service.create(request()),
    );
    let all = [results.0, results.1, results.2, results.3];
    let created = all.iter().filter(|result| result.is_ok()).count();
    let refused = all
        .iter()
        .filter(|result| matches!(result, Err(CreateRefusal::LiveWindowExists)))
        .count();
    assert_eq!((created, refused), (1, 3), "{all:?}");
    let rows: i64 = sqlx::query_scalar("select count(*) from cloud_windows")
        .fetch_one(&rig.pool)
        .await?;
    assert_eq!(rows, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_private_host_key_is_never_stored(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let id = rig.create(60).await;
    let window = rig.window(id).await;
    assert!(window.host_key_public.starts_with("ssh-ed25519 "));
    assert!(!window.host_key_public.contains("PRIVATE"));
    let dump: String = sqlx::query_scalar("select cloud_windows::text from cloud_windows")
        .fetch_one(&rig.pool)
        .await?;
    assert!(!dump.contains("PRIVATE KEY") && !dump.contains("BEGIN OPENSSH"));
    Ok(())
}

// ───────── Полный цикл ─────────

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_full_cycle_create_ready_idle_delete_ended_with_the_final_cost(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let id = rig.create(120).await;
    assert_eq!(rig.state(id).await, "create_intent");
    assert!(windows::has_pending_key(id));
    assert_eq!(rig.reservation(120), 4 * HOURLY);

    // Первый проход: сверка (пустой проект), затем вызов создания.
    rig.tick(t0).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "identified");
    let server_id = window.server_id.expect("server id");
    assert_eq!(window.create_called_at, Some(t0));
    assert_eq!(window.hard_cap_at, Some(t0 + TimeDelta::minutes(120)));
    assert_eq!(rig.posts(), 1);
    {
        let state = rig.fake.control();
        let server = state.servers.get(&server_id).expect("the fake server");
        assert_eq!(server.name, format!("canterlot-runner-{id}"));
        assert_eq!(server.project_id, PROJECT);
        // Закрытый хостовый ключ ушёл провайдеру в cloud-init, и только туда.
        let cloud_init = server.request["cloud_init"].as_str().unwrap();
        assert!(cloud_init.contains("BEGIN OPENSSH PRIVATE KEY"));
        assert!(cloud_init.contains(&window.host_key_public));
    }
    let dump: String = sqlx::query_scalar("select cloud_windows::text from cloud_windows")
        .fetch_one(&rig.pool)
        .await?;
    assert!(!dump.contains("PRIVATE KEY"));
    assert!(
        !windows::has_pending_key(id),
        "the private half left memory"
    );

    // DNAT-правило создано, несущей ушла команда подключения: только с `runners_v1`.
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    rig.tick(t0 + TimeDelta::seconds(2)).await;
    let window = rig.window(id).await;
    assert_eq!(window.dnat_rule_id.as_deref(), Some("dnat-1"));
    let attach: serde_json::Value = sqlx::query_scalar(
        "select payload from commands where kind = 'attach_runner' and machine_id = $1",
    )
    .bind(rig.carrier)
    .fetch_one(&rig.pool)
    .await?;
    let machine = rig.cloud_machine().await;
    assert_eq!(attach["window_id"], id.to_string());
    assert_eq!(attach["machine_id"], machine.to_string());
    assert_eq!(attach["host"], ROUTER_IP);
    assert_eq!(attach["ssh_port"], 22100);
    assert_eq!(attach["host_key_public"], window.host_key_public);
    assert_eq!(attach["root_dir"], "/srv/canterlot-runner");
    assert_eq!(attach["max_running"], 4);
    assert_eq!(attach["lease_slots"], 2);
    assert!(!attach.to_string().contains("PRIVATE"));
    // Раннер без подключения воркера окно не готовит.
    rig.tick(t0 + TimeDelta::seconds(2)).await;
    assert_eq!(rig.state(id).await, "identified");

    // Несущая подключила раннер: токен по запросу и воркер на связи.
    let session = rig.attach().await;
    rig.tick(t0 + TimeDelta::seconds(3)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ready");
    assert_eq!(window.ready_at, Some(t0 + TimeDelta::seconds(3)));
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from commands where kind = 'attach_runner' and delivery_state <> 'completed'",
    )
    .fetch_one(&rig.pool)
    .await?;
    assert_eq!(
        pending, 0,
        "the attach command is done once the runner connected"
    );

    // Работы нет: через 15 минут простоя окно удаляется.
    let idle_start = t0 + TimeDelta::seconds(60);
    rig.tick(idle_start).await;
    assert!(rig.window(id).await.idle_since.is_some());
    rig.tick(idle_start + TimeDelta::minutes(14)).await;
    assert_eq!(rig.state(id).await, "ready");
    let delete_at = idle_start + TimeDelta::minutes(15);
    rig.tick(delete_at).await;
    assert_eq!(rig.state(id).await, "deleting");
    assert_eq!(rig.window(id).await.delete_reason.as_deref(), Some("idle"));
    drop(session);
    rig.tick(delete_at + TimeDelta::seconds(1)).await;

    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert_eq!(
        window.delete_confirmed_at,
        Some(delete_at + TimeDelta::seconds(1))
    );
    // 16 минут от вызова создания: один оплаченный час сервера и один адреса.
    assert_eq!(window.final_cost_kopecks, Some(HOURLY));
    assert_eq!(rig.fake.server_count(), 0);
    assert_eq!((rig.posts(), rig.deletes()), (1, 1));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_rejected_create_fails_the_window_and_releases_the_reservation(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    rig.fake.control().create = CreateMode::Reject {
        status: 400,
        message: "no quota".into(),
    };
    let id = rig.create(60).await;
    rig.tick(now_micros()).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!(window.final_cost_kopecks, Some(0));
    assert_eq!(rig.posts(), 1);
    let mut conn = rig.pool.acquire().await?;
    assert_eq!(windows::spent_in_month(&mut conn, &window.month).await?, 0);
    drop(conn);
    // Окно закрыто: можно создать следующее.
    rig.try_create(60).await.expect("the slot is free again");
    Ok(())
}

// ───────── Список профилей ─────────

/// Профиль на конфигураторе 133 (28 vCPU, 112 ГБ, 160 ГБ): 133 600 ₽ в месяц, 18 556 копеек
/// в час.
const DEAR: (i64, u32, u32, u32) = (133, 28, 112, 160);
const DEAR_HOURLY: i64 = 18_556;
/// Профиль по умолчанию на конфигураторе 151: 2845 копеек в час.
const MID: (i64, u32, u32, u32) = (151, 32, 64, 80);
/// 8 vCPU, 16 ГБ, 40 ГБ на конфигураторе 151: 5360 ₽ в месяц, 745 копеек в час.
const CHEAP: (i64, u32, u32, u32) = (151, 8, 16, 40);
const CHEAP_HOURLY: i64 = 745;

fn profiles(list: &[(i64, u32, u32, u32)]) -> impl FnOnce(&mut CloudConfig) + '_ {
    move |config| {
        config.profile = list
            .iter()
            .map(|&(configurator_id, cpu, ram_gb, disk_gb)| CloudProfile {
                configurator_id,
                cpu,
                ram_gb,
                disk_gb,
                availability_zone: "msk-1".to_owned(),
            })
            .collect();
    }
}

fn coded(status: u16, error_code: &str) -> CreateMode {
    CreateMode::Coded {
        status,
        error_code: error_code.to_owned(),
    }
}

/// Конфигураторы запросов создания в порядке отправки.
fn posted_configurators(rig: &Rig) -> Vec<i64> {
    rig.fake
        .requests()
        .iter()
        .filter(|r| r.method == "POST" && r.path == "/api/v1/servers")
        .map(|r| {
            r.body.as_ref().unwrap()["configuration"]["configurator_id"]
                .as_i64()
                .unwrap()
        })
        .collect()
}

#[test]
fn only_the_known_configuration_refusals_fall_back_to_the_next_profile() {
    use canterlot_hub::cloud::profile_fallback;
    for (status, code) in [
        (409, "no_free_node"),
        (404, "location_not_available"),
        (400, "location_not_available"),
        (404, "preset_not_found"),
        (400, "preset_not_found"),
        (400, "configuration_requires_sales_request"),
    ] {
        assert!(profile_fallback(status, code), "{status} {code}");
    }
    for (status, code) in [
        (400, "bad_request"),
        (400, "no_free_node"),
        (409, "conflict"),
        (409, "configuration_requires_sales_request"),
        (403, "forbidden"),
        (401, "unauthorized"),
        (429, "too_many_requests"),
        (400, ""),
    ] {
        assert!(!profile_fallback(status, code), "{status} {code}");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_reservation_and_the_budget_use_the_most_expensive_profile_of_the_list(
    pool: PgPool,
) -> TestResult {
    // Самый дорогой не первый и не последний.
    let rig = rig_with(pool, profiles(&[MID, DEAR, CHEAP])).await;
    let pricing = rig.service.pricing().await?;
    let hourly: Vec<i64> = pricing.profiles.iter().map(|p| p.hourly_kopecks).collect();
    assert_eq!(
        hourly,
        vec![HOURLY, DEAR_HOURLY, CHEAP_HOURLY],
        "in list order"
    );
    assert_eq!(pricing.max_hourly_kopecks(), Some(DEAR_HOURLY));

    // 60 + 30 + 30 = 120 минут: 3 часа резерва по самой дорогой цене.
    let expected = 3 * DEAR_HOURLY;
    sqlx::query("update cloud_budget set monthly_budget_kopecks = $1")
        .bind(expected - 1)
        .execute(&rig.pool)
        .await?;
    let request = |consent| CreateRequest {
        trigger: Trigger::Owner,
        proposal_id: None,
        hard_cap_minutes: 60,
        consent_over_budget_kopecks: consent,
    };
    // Бюджет не хватает на самый дорогой профиль, хотя на первый хватило бы.
    match rig.service.create(request(None)).await {
        Err(CreateRefusal::OverBudget {
            reservation_kopecks,
            ..
        }) => {
            assert_eq!(reservation_kopecks, expected)
        }
        other => panic!("expected over budget, got {other:?}"),
    }
    sqlx::query("update cloud_budget set monthly_budget_kopecks = $1")
        .bind(expected)
        .execute(&rig.pool)
        .await?;
    let id = rig.service.create(request(None)).await?;
    let window = rig.window(id).await;
    assert_eq!(window.reservation_kopecks, expected);
    assert_eq!(
        window.price_hourly_kopecks, DEAR_HOURLY,
        "planned price is the maximum"
    );
    assert_eq!(window.configurator_id, None);
    let stored: Vec<i64> = window.profiles().iter().map(|p| p.hourly_kopecks).collect();
    assert_eq!(stored, hourly, "the snapshot keeps every price in order");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_profile_that_cannot_be_priced_is_left_out_of_the_window(pool: PgPool) -> TestResult {
    // Конфигуратора 999 у провайдера нет: резерва под него не посчитать.
    let rig = rig_with(pool, profiles(&[(999, 4, 4, 20), MID])).await;
    let pricing = rig.service.pricing().await?;
    assert_eq!(pricing.profiles.len(), 1);
    assert_eq!(pricing.profiles[0].configurator_id, 151);
    let none = rig_with(rig.pool.clone(), profiles(&[(999, 4, 4, 20)])).await;
    assert!(none.service.pricing().await.is_err());
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_definite_refusal_moves_to_the_next_profile_in_the_same_window_and_the_ledger_uses_the_real_price(
    pool: PgPool,
) -> TestResult {
    let rig = rig_with(pool, profiles(&[DEAR, MID, CHEAP])).await;
    let _carrier = rig.carrier_session();
    rig.fake
        .control()
        .create_by_configurator
        .insert(133, coded(409, "no_free_node"));
    let t0 = now_micros();
    let id = rig.create(60).await;
    let reserved = rig.window(id).await.reservation_kopecks;
    assert_eq!(reserved, 3 * DEAR_HOURLY);
    rig.tick(t0).await;

    let window = rig.window(id).await;
    assert_eq!(
        window.state, "identified",
        "the same window, no new transaction"
    );
    assert_eq!(
        posted_configurators(&rig),
        vec![133, 151],
        "in list order, stops at the first success"
    );
    assert_eq!(window.configurator_id, Some(151));
    assert_eq!(
        window.price_hourly_kopecks, HOURLY,
        "the price of the profile that succeeded"
    );
    assert_eq!(
        window.reservation_kopecks, reserved,
        "the reservation is unchanged"
    );
    {
        let state = rig.fake.control();
        let server = state.servers.get(&window.server_id.unwrap()).unwrap();
        assert_eq!(server.configurator_id, 151);
        assert_eq!(server.cpu, 32);
    }
    // Одно окно: создать второе нельзя, пока идёт это.
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::LiveWindowExists)
    ));

    // Конец окна: итог по действительной цене, а не по цене резерва.
    let _session = rig.attach_when_listed(t0).await;
    windows::request_delete(
        &rig.pool,
        id,
        windows::REASON_OWNER,
        t0 + TimeDelta::minutes(10),
    )
    .await?;
    rig.tick(t0 + TimeDelta::minutes(10) + TimeDelta::seconds(1))
        .await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert_eq!(window.final_cost_kopecks, Some(HOURLY));
    assert!(window.final_cost_kopecks.unwrap() < reserved);
    Ok(())
}

impl Rig {
    /// Дойти до `ready` из `identified`: адрес, команда подключения, токен, воркер на связи.
    async fn attach_when_listed(&self, t0: DateTime<Utc>) -> FakeSession {
        self.tick(t0 + TimeDelta::seconds(1)).await;
        let session = self.attach().await;
        self.tick(t0 + TimeDelta::seconds(2)).await;
        session
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn each_listed_refusal_code_falls_back_and_any_other_one_closes_the_window(
    pool: PgPool,
) -> TestResult {
    let rig = rig_with(pool, profiles(&[DEAR, CHEAP])).await;
    let mut t = now_micros();
    for (status, code) in [
        (409, "no_free_node"),
        (404, "location_not_available"),
        (404, "preset_not_found"),
        (400, "configuration_requires_sales_request"),
    ] {
        rig.fake
            .control()
            .create_by_configurator
            .insert(133, coded(status, code));
        let id = rig.create(60).await;
        t += TimeDelta::minutes(1);
        rig.tick(t).await;
        assert_eq!(rig.state(id).await, "identified", "{status} {code}");
        assert_eq!(rig.window(id).await.configurator_id, Some(151));
        windows::request_delete(&rig.pool, id, windows::REASON_OWNER, t).await?;
        t += TimeDelta::seconds(1);
        rig.tick(t).await;
        assert_eq!(rig.state(id).await, "ended");
    }
    // Отказ не из списка: следующий профиль не пробуется, окно закрыто с кодом.
    let before = rig.posts();
    rig.fake
        .control()
        .create_by_configurator
        .insert(133, coded(403, "forbidden"));
    let id = rig.create(60).await;
    t += TimeDelta::minutes(1);
    rig.tick(t).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!(window.final_cost_kopecks, Some(0));
    assert_eq!(window.last_error.as_deref(), Some("133:403/forbidden"));
    assert_eq!(rig.posts(), before + 1, "the cheap profile was not tried");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn when_every_profile_is_refused_the_window_fails_with_the_codes_and_a_zero_cost(
    pool: PgPool,
) -> TestResult {
    let rig = rig_with(pool, profiles(&[DEAR, MID, CHEAP])).await;
    {
        let mut state = rig.fake.control();
        state
            .create_by_configurator
            .insert(133, coded(409, "no_free_node"));
        // 151 отвечает одинаково и для среднего, и для дешёвого профиля.
        state
            .create_by_configurator
            .insert(151, coded(400, "configuration_requires_sales_request"));
    }
    let id = rig.create(60).await;
    rig.tick(now_micros()).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!(window.final_cost_kopecks, Some(0));
    assert_eq!(
        window.last_error.as_deref(),
        Some(
            "all_profiles_rejected: 133:409/no_free_node,\
             151:400/configuration_requires_sales_request,\
             151:400/configuration_requires_sales_request"
        )
    );
    assert_eq!(posted_configurators(&rig), vec![133, 151, 151]);
    assert_eq!(rig.fake.server_count(), 0);
    let mut conn = rig.pool.acquire().await?;
    assert_eq!(
        windows::spent_in_month(&mut conn, &window.month).await?,
        0,
        "reservation released"
    );
    drop(conn);
    rig.try_create(60).await.expect("the slot is free");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_ambiguous_outcome_stops_the_fallback_and_the_attempted_profile_is_billed(
    pool: PgPool,
) -> TestResult {
    let rig = rig_with(pool, profiles(&[DEAR, MID])).await;
    {
        let mut state = rig.fake.control();
        // Первый профиль принят провайдером, ответ потерян по дороге.
        state.create = CreateMode::AcceptThenDrop;
    }
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "create_unknown");
    assert_eq!(
        posted_configurators(&rig),
        vec![133],
        "no other profile after an ambiguous answer"
    );
    assert_eq!(window.configurator_id, Some(133));
    assert_eq!(window.price_hourly_kopecks, DEAR_HOURLY);
    // Позже сервер находится по имени: ledger считает цену профиля, который мог создаться.
    rig.tick(t0 + TimeDelta::minutes(6)).await;
    assert_eq!(rig.state(id).await, "identified");
    assert_eq!(posted_configurators(&rig), vec![133]);
    assert_eq!(rig.fake.server_count(), 1);
    assert_eq!(rig.window(id).await.price_hourly_kopecks, DEAR_HOURLY);

    // Сервер 5xx на первом профиле тоже неопределён: второй не пробуется.
    let rig2 = rig_with(rig.pool.clone(), profiles(&[DEAR, MID])).await;
    sqlx::query("update cloud_windows set state = 'ended', final_cost_kopecks = 0")
        .execute(&rig.pool)
        .await?;
    rig2.fake
        .control()
        .create_by_configurator
        .insert(133, CreateMode::Status(500));
    let id = rig2.create(60).await;
    rig2.tick(t0).await;
    assert_eq!(rig2.state(id).await, "create_unknown");
    assert_eq!(posted_configurators(&rig2), vec![133]);
    Ok(())
}

// ───────── Публичный IPv4 ─────────

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_new_server_gets_its_dnat_rule_once_and_attach_uses_the_router(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let id = rig.create(120).await;
    rig.tick(t0).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "identified");
    assert_eq!((window.dnat_rule_id.clone(), rig.dnat_posts()), (None, 0));

    // Первый проход после `On` создаёт правило, второй шлёт команду подключения.
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    let window = rig.window(id).await;
    assert_eq!(rig.dnat_posts(), 1);
    assert_eq!(window.dnat_rule_id.as_deref(), Some("dnat-1"));
    assert_eq!(window.local_ip.as_deref(), Some("192.0.2.10"));
    assert_eq!(window.dnat_port, Some(22100));
    rig.tick(t0 + TimeDelta::seconds(2)).await;
    let attach: serde_json::Value = sqlx::query_scalar(
        "select payload from commands where kind = 'attach_runner' and machine_id = $1",
    )
    .bind(rig.carrier)
    .fetch_one(&rig.pool)
    .await?;
    assert_eq!(attach["host"], ROUTER_IP);
    assert_eq!(attach["ssh_port"], 22100);

    // Сколько бы проходов ни было, правило создаётся один раз.
    for seconds in [2, 3, 30] {
        rig.tick(t0 + TimeDelta::seconds(seconds)).await;
    }
    assert_eq!(rig.dnat_posts(), 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn attach_waits_until_the_server_is_on(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    rig.fake.control().boot_polls = 3;
    let t0 = now_micros();
    let id = rig.create(120).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "identified");
    assert!(rig.window(id).await.attach_sent_at.is_none());
    assert_eq!(rig.dnat_posts(), 0, "the server is still installing");
    for seconds in [1, 2, 3, 4] {
        rig.tick(t0 + TimeDelta::seconds(seconds)).await;
    }
    assert!(rig.window(id).await.dnat_rule_id.is_some());
    assert!(
        rig.window(id).await.attach_sent_at.is_none(),
        "rule not yet sent"
    );
    rig.tick(t0 + TimeDelta::seconds(5)).await;
    assert!(rig.window(id).await.attach_sent_at.is_some());
    assert_eq!(rig.dnat_posts(), 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_no_paid_server_deletes_the_window_charges_the_server_blocks_creation_and_alerts(
    pool: PgPool,
) -> TestResult {
    let rig = rig_with(pool, |config| {
        config.no_paid_grace = Duration::from_secs(60)
    })
    .await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    rig.fake.control().boot_status = Some("no_paid".to_owned());
    let t0 = now_micros();
    let id = rig.create(120).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "identified");

    // Срок считается от вызова создания: до него окно живёт, после — definite отказ.
    rig.tick(t0 + TimeDelta::seconds(30)).await;
    assert_eq!(rig.state(id).await, "identified");
    rig.tick(t0 + TimeDelta::seconds(70)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.delete_reason.as_deref(), Some("no_paid"));
    assert_eq!(window.last_error.as_deref(), Some("no_paid"));
    assert_eq!(window.attach_sent_at, None, "nothing to attach");
    assert_eq!(rig.dnat_posts(), 0);

    // Блок провайдера стоит, о нём видно в базе.
    let mut conn = rig.pool.acquire().await?;
    let (code, at) = windows::provider_block(&mut conn)
        .await?
        .expect("the block is set");
    assert_eq!(code, "no_paid");
    assert!(at >= t0);
    drop(conn);

    // Владельцу пуш сразу.
    let alerts = rig.alerts().await;
    assert_eq!(alerts.len(), 1);
    assert_eq!(
        alerts[0],
        serde_json::json!({
            "code": "no_paid",
            "window_id": window.id,
            "name": window.name,
        })
    );

    // Обычный путь удаления: сервер удалён, отсутствие подтверждено списком, окно кончилось.
    rig.tick(t0 + TimeDelta::seconds(80)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert_eq!(rig.fake.server_count(), 0);
    assert_eq!(rig.deletes(), 1);
    assert_eq!(window.final_cost_kopecks, Some(HOURLY));
    assert_eq!(window.last_error.as_deref(), Some("no_paid"));

    // Следующее окно отказано, пока владелец не снимет блок.
    match rig.try_create(60).await {
        Err(CreateRefusal::ProviderBlocked { code }) => assert_eq!(code, "no_paid"),
        other => panic!("expected the provider block, got {other:?}"),
    }
    let mut conn = rig.pool.acquire().await?;
    assert!(windows::clear_provider_block(&mut conn).await?);
    assert!(!windows::clear_provider_block(&mut conn).await?);
    drop(conn);
    rig.try_create(60)
        .await
        .expect("creation works again once the block is cleared");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_definite_dnat_refusal_closes_the_window_without_a_block(pool: PgPool) -> TestResult {
    for code in ["forbidden", "conflict", "bad_request"] {
        let rig = rig(pool.clone()).await;
        let _carrier = rig.carrier_session();
        rig.fake.control().dnat_create = crate::support::fake_timeweb::DnatCreateMode::Refuse {
            status: 400,
            error_code: code.to_owned(),
        };
        let t0 = now_micros();
        let id = rig.create(120).await;
        rig.tick(t0).await;
        rig.tick(t0 + TimeDelta::seconds(1)).await;
        rig.tick(t0 + TimeDelta::seconds(2)).await;
        let window = rig.window(id).await;
        assert_eq!(window.state, "ended", "{code}");
        assert_eq!(window.last_error.as_deref(), Some("dnat_refused"));
        assert_eq!(rig.dnat_posts(), 1);
        assert_eq!(rig.fake.server_count(), 0);
        let mut conn = rig.pool.acquire().await?;
        assert!(windows::provider_block(&mut conn).await?.is_none());
        drop(conn);
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_ambiguous_dnat_answer_is_adopted_from_the_listing_without_a_second_post(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    rig.fake.control().dnat_create = crate::support::fake_timeweb::DnatCreateMode::AcceptThenDrop;
    let t0 = now_micros();
    let id = rig.create(120).await;
    rig.tick(t0).await;
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    let window = rig.window(id).await;
    assert_eq!(window.dnat_rule_id, None, "the answer was lost");
    assert!(window.dnat_create_attempted_at.is_some(), "no second call");
    rig.tick(t0 + TimeDelta::seconds(2)).await;
    let window = rig.window(id).await;
    assert_eq!(rig.dnat_posts(), 1);
    assert_eq!(window.dnat_rule_id.as_deref(), Some("dnat-1"));
    rig.tick(t0 + TimeDelta::seconds(3)).await;
    assert!(rig.window(id).await.attach_sent_at.is_some());
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn deleting_a_window_also_deletes_its_dnat_rule_and_confirms_it_by_absence(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _session, at) = rig.drive_to_ready(120, t0).await;
    assert_eq!(rig.fake.control().dnat_rules.len(), 1);
    windows::request_delete(&rig.pool, id, windows::REASON_OWNER, at).await?;
    rig.tick(at + TimeDelta::seconds(1)).await;

    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert!(rig.fake.control().dnat_rules.is_empty());
    assert_eq!(rig.fake.server_count(), 0);
    assert!(window.server_gone_at.is_some() && window.dnat_gone_at.is_some());
    assert_eq!(window.dnat_rule_id.as_deref(), Some("dnat-1"));
    // Сервер удаляется раньше правила.
    let requests = rig.fake.requests();
    let delete_server = requests
        .iter()
        .position(|r| r.method == "DELETE" && r.path.starts_with("/api/v1/servers/"))
        .unwrap();
    let delete_rule = requests
        .iter()
        .position(|r| r.method == "DELETE" && r.path.starts_with("/api/v1/routers/"))
        .unwrap();
    assert!(delete_server < delete_rule);
    // Конец окна подтверждён и для правила списком.
    assert!(requests.iter().any(|r| r.method == "GET"
        && r.path.starts_with("/api/v1/routers/")
        && r.path.ends_with("/dnat-rules")));
    assert_eq!(window.final_cost_kopecks, Some(HOURLY));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_window_stays_live_while_the_dnat_rule_cannot_be_deleted(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _session, at) = rig.drive_to_ready(120, t0).await;
    rig.fake.control().dnat_delete_status = Some(500);
    windows::request_delete(&rig.pool, id, windows::REASON_OWNER, at).await?;
    let gone_at = at + TimeDelta::seconds(1);
    rig.tick(gone_at).await;

    // Сервер удалён и подтверждён, правило осталось: окно живо, резерв держится.
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.server_gone_at, Some(gone_at));
    assert_eq!(window.last_error.as_deref(), Some("dnat_delete_failed"));
    assert_eq!(rig.fake.control().dnat_rules.len(), 1);
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::LiveWindowExists)
    ));
    // Повторы с паузой; после трёх неудач пуш.
    rig.tick(gone_at + TimeDelta::seconds(40)).await;
    rig.tick(gone_at + TimeDelta::minutes(3)).await;
    assert_eq!(window_attempts(&rig, id).await, 3);
    assert_eq!(rig.alert_codes().await, vec!["dnat_delete_failing"]);

    // Провайдер ожил: правило удалено, окно кончилось, итог — час сервера.
    rig.fake.control().dnat_delete_status = None;
    let done_at = gone_at + TimeDelta::minutes(90);
    rig.tick(done_at).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert_eq!(window.dnat_gone_at, Some(done_at));
    assert_eq!(window.final_cost_kopecks, Some(HOURLY));
    assert!(rig.fake.control().dnat_rules.is_empty());
    Ok(())
}

async fn window_attempts(rig: &Rig, id: Uuid) -> i32 {
    rig.window(id).await.dnat_delete_attempts
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reconcile_deletes_stray_dnat_rules_in_range_and_leaves_the_others(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let now = now_micros();
    use crate::support::fake_timeweb::FakeDnatRule;
    let rule = |id: &str, port: &str| FakeDnatRule {
        id: id.to_owned(),
        local_ip: "192.0.2.42".to_owned(),
        local_port: "22".to_owned(),
        public_ip: ROUTER_IP.to_owned(),
        public_port: port.to_owned(),
        protocol: "tcp".to_owned(),
    };
    {
        let mut state = rig.fake.control();
        // Три лишних правила внутри диапазона, а также правило вне диапазона.
        state.dnat_rules.push(rule("mine-1", "22101"));
        state.dnat_rules.push(rule("mine-2", "22102"));
        state.dnat_rules.push(rule("mine-3", "22103"));
        state.dnat_rules.push(rule("out-of-range", "22222"));
        state.dnat_rules.push(rule("held-by-live", "22104"));
    }
    // Живое окно держит порт 22104 — его правило трогать нельзя.
    let live = raw_window(
        &rig.pool,
        "identified",
        Some(2),
        Some(now - TimeDelta::minutes(5)),
    )
    .await;
    sqlx::query("update cloud_windows set dnat_port = 22104 where id = $1")
        .bind(live)
        .execute(&rig.pool)
        .await?;
    rig.tick(now).await;
    let left: Vec<String> = rig
        .fake
        .control()
        .dnat_rules
        .iter()
        .map(|r| r.id.clone())
        .collect();
    assert!(!left.iter().any(|id| id.starts_with("mine-")), "{left:?}");
    assert!(left.contains(&"out-of-range".to_owned()));
    assert!(left.contains(&"held-by-live".to_owned()));
    let codes = rig.alert_codes().await;
    assert_eq!(
        codes.iter().filter(|c| *c == "stray_dnat_deleted").count(),
        3,
        "{codes:?}"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_dnat_rule_held_by_a_live_window_is_not_a_stray(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let now = now_micros();
    let live = raw_window(
        &rig.pool,
        "identified",
        Some(2),
        Some(now - TimeDelta::minutes(5)),
    )
    .await;
    sqlx::query("update cloud_windows set dnat_port = 22101 where id = $1")
        .bind(live)
        .execute(&rig.pool)
        .await?;
    {
        let mut state = rig.fake.control();
        state
            .dnat_rules
            .push(crate::support::fake_timeweb::FakeDnatRule {
                id: "held".into(),
                local_ip: "192.0.2.10".into(),
                local_port: "22".into(),
                public_ip: ROUTER_IP.into(),
                public_port: "22101".into(),
                protocol: "tcp".into(),
            });
        state.add_server(PROJECT, &windows::server_name(live));
    }
    rig.tick(now).await;
    assert_eq!(
        rig.fake.control().dnat_rules.len(),
        1,
        "the live rule stays"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_dnat_rule_without_an_answer_never_leaves_an_open_obligation(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    ready_row(&rig).await;
    let now = now_micros();
    let id = raw_window(
        &rig.pool,
        "deleting",
        Some(777),
        Some(now - TimeDelta::minutes(20)),
    )
    .await;
    // Вызова правила не было: удалять нечего, окно кончается без лишнего пуша.
    rig.tick(now).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert_eq!(window.final_cost_kopecks, Some(HOURLY));
    assert!(rig.alert_codes().await.is_empty());
    Ok(())
}

// ───────── Неопределённое создание ─────────

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_ambiguous_create_is_found_by_name_and_is_never_posted_twice(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    {
        let mut state = rig.fake.control();
        state.create = CreateMode::AcceptThenDrop;
        // Первый полный список проекта сервер ещё не показывает.
        state.listing_hidden_polls = 1;
    }
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "create_unknown");
    assert_eq!(rig.window(id).await.server_id, None);
    assert_eq!(rig.posts(), 1);

    // Проходы между сверками ничего не меняют и не звонят повторно.
    for seconds in [10, 60, 120] {
        rig.tick(t0 + TimeDelta::seconds(seconds)).await;
    }
    assert_eq!(rig.state(id).await, "create_unknown");
    // Сверка видит пустой список (сервер скрыт) раньше окна ожидания: окно не трогается.
    rig.tick(t0 + TimeDelta::minutes(5)).await;
    assert_eq!(rig.state(id).await, "create_unknown");
    // Следующая сверка видит сервер по имени.
    rig.tick(t0 + TimeDelta::minutes(10)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "identified");
    assert!(window.server_id.is_some());
    assert_eq!(rig.posts(), 1, "there is never a second POST");
    assert_eq!(rig.fake.server_count(), 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_ambiguous_create_absent_from_full_listings_fails_after_the_window(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    rig.fake.control().create = CreateMode::Status(500);
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "create_unknown");
    rig.tick(t0 + TimeDelta::minutes(5)).await;
    assert_eq!(
        rig.state(id).await,
        "create_unknown",
        "a full listing without the server is not enough before the window"
    );
    rig.tick(t0 + TimeDelta::minutes(10)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!(
        window.final_cost_kopecks,
        Some(0),
        "the reservation is released"
    );
    assert_eq!(rig.posts(), 1, "there is never a second POST");
    // После закрытия можно создать новое окно.
    rig.try_create(60).await.expect("the slot is free");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failed_or_partial_listing_changes_nothing(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.fake.control().create = CreateMode::Status(500);
    let t0 = now_micros();
    let unknown = rig.create(60).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(unknown).await, "create_unknown");

    // Страница списка отказывает: окно остаётся, как бы долго ни ждали.
    rig.fake.control().list_fail_page = Some(FailPage {
        offset: 0,
        status: 500,
        times: None,
    });
    for minutes in [11, 17, 23] {
        rig.tick(t0 + TimeDelta::minutes(minutes)).await;
        assert_eq!(
            rig.state(unknown).await,
            "create_unknown",
            "after {minutes} min"
        );
    }
    // Страницы прочитались, но не сложились в заявленное число: тоже не список.
    {
        let mut state = rig.fake.control();
        state.list_fail_page = None;
        state.list_total_override = Some(99);
    }
    rig.tick(t0 + TimeDelta::minutes(30)).await;
    assert_eq!(rig.state(unknown).await, "create_unknown");
    // Целый список: теперь окно закрывается.
    rig.fake.control().list_total_override = None;
    rig.tick(t0 + TimeDelta::minutes(36)).await;
    assert_eq!(rig.state(unknown).await, "failed");

    // Живое окно, чей сервер пропал: при плохом списке остаётся живым, без пуша.
    rig.fake.control().create = CreateMode::Ok;
    let id = rig.create(60).await;
    let t1 = t0 + TimeDelta::minutes(40);
    rig.tick(t1).await;
    assert_eq!(rig.state(id).await, "identified");
    // Адрес заказан до того, как сервер пропал.
    rig.tick(t1 + TimeDelta::seconds(1)).await;
    rig.vanish_servers();
    rig.fake.control().list_fail_page = Some(FailPage {
        offset: 0,
        status: 500,
        times: None,
    });
    // Срок ожидания подключения (15 минут) ещё не вышел, а срок появления сервера в списках
    // (10 минут) уже вышел: решает только список.
    rig.tick(t1 + TimeDelta::minutes(11)).await;
    assert_eq!(rig.state(id).await, "identified");
    assert!(rig.alert_codes().await.is_empty());
    // Целый список без сервера: окно `gone`, владельцу пуш, раннер отозван.
    rig.fake.control().list_fail_page = None;
    rig.tick(t1 + TimeDelta::minutes(12)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "gone");
    assert!(window.final_cost_kopecks.is_some());
    // Правило исчезнувшего окна становится лишним и убирается сверкой.
    assert_eq!(
        rig.alert_codes().await,
        vec!["window_gone", "stray_dnat_deleted"]
    );
    Ok(())
}

// ───────── Жёсткий предел ─────────

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_hard_cap_drains_cancels_then_bounds_teardown_and_deletes(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _session, at) = rig.drive_to_ready(60, t0).await;
    let machine = rig.cloud_machine().await;
    let running = rig.seed_job(Some(machine), "running", &["cloud"]).await;
    let queued = rig.seed_job(Some(machine), "queued", &["cloud"]).await;

    let window = rig.window(id).await;
    // Предел считается от вызова создания, а не от готовности.
    assert_eq!(window.create_called_at, Some(t0));
    assert!(window.ready_at.unwrap() > t0);
    assert_eq!(window.hard_cap_at, Some(t0 + TimeDelta::minutes(60)));
    assert_eq!(window.drain_until, Some(t0 + TimeDelta::minutes(90)));
    assert_eq!(window.teardown_until, Some(t0 + TimeDelta::minutes(120)));

    // До предела окно живёт, а работа есть: простоя нет.
    rig.tick(at + TimeDelta::minutes(30)).await;
    assert_eq!(rig.state(id).await, "ready");

    // Предел: раннер перестаёт брать работу, идущая дорабатывает.
    rig.tick(t0 + TimeDelta::minutes(60)).await;
    assert_eq!(rig.state(id).await, "draining");
    let accepts: bool =
        sqlx::query_scalar("select accepts_remote_work from machines where id = $1")
            .bind(machine)
            .fetch_one(&rig.pool)
            .await?;
    assert!(!accepts, "draining stops new binds");
    rig.tick(t0 + TimeDelta::minutes(89)).await;
    assert!(rig.window(id).await.cancel_sent_at.is_none());
    assert_eq!(rig.job_state(running).await, "running");

    // После срока на доработку идущая задача получает отмену через общий путь отмены.
    rig.tick(t0 + TimeDelta::minutes(90)).await;
    assert!(rig.window(id).await.cancel_sent_at.is_some());
    let cancel_requested: bool =
        sqlx::query_scalar("select cancel_requested_at is not null from jobs where id = $1")
            .bind(running)
            .fetch_one(&rig.pool)
            .await?;
    assert!(cancel_requested);
    assert_eq!(
        rig.job_state(queued).await,
        "canceled",
        "the queue closes at once"
    );
    let sent: i64 = sqlx::query_scalar(
        "select count(*) from commands where kind = 'cancel_inplace_job' and machine_id = $1",
    )
    .bind(machine)
    .fetch_one(&rig.pool)
    .await?;
    assert_eq!(sent, 1);
    // Подтверждения нет; до границы сноса окно ждёт.
    rig.tick(t0 + TimeDelta::minutes(119)).await;
    assert_eq!(rig.state(id).await, "draining");
    assert_eq!(rig.job_state(running).await, "running");

    // Граница сноса: неподтверждённое становится `lost`, раннер отозван, сервер удаляется.
    rig.tick(t0 + TimeDelta::minutes(120)).await;
    assert_eq!(rig.state(id).await, "deleting");
    assert_eq!(rig.job_state(running).await, "lost");
    rig.tick(t0 + TimeDelta::minutes(120) + TimeDelta::seconds(1))
        .await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    // Сервер: 121 минута от вызова создания, три оплаченных часа. Публичного адреса нет.
    assert_eq!(window.final_cost_kopecks, Some(3 * HOURLY));
    assert!(window.final_cost_kopecks.unwrap() <= window.reservation_kopecks);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_draining_window_with_no_work_left_is_deleted_at_once(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _session, _) = rig.drive_to_ready(60, t0).await;
    rig.tick(t0 + TimeDelta::minutes(60)).await;
    assert_eq!(rig.state(id).await, "draining");
    rig.tick(t0 + TimeDelta::minutes(60) + TimeDelta::seconds(1))
        .await;
    assert_eq!(rig.state(id).await, "deleting");
    assert_eq!(
        rig.window(id).await.delete_reason.as_deref(),
        Some("drained")
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_window_found_past_every_bound_after_an_outage_is_deleted_with_an_alert(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _session, _) = rig.drive_to_ready(60, t0).await;

    // Хаб лежал: новая служба (старт хаба) видит окно далеко за пределом с запасами.
    let restarted = CloudService::new(
        rig.pool.clone(),
        rig.registry.clone(),
        provider_for(&rig),
        rig.config.clone(),
    );
    restarted.tick(t0 + TimeDelta::hours(6)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.delete_reason.as_deref(), Some("overrun"));
    assert_eq!(rig.alert_codes().await, vec!["outage_extra_cost"]);
    let extra = rig.alerts().await[0]["extra_kopecks"].as_i64().unwrap();
    // Пять часов сверх предела по цене часа сервера и адреса.
    assert_eq!(extra, 5 * HOURLY);
    Ok(())
}

fn provider_for(rig: &Rig) -> Arc<TimewebProvider> {
    let mut config = TimewebConfig::new(rig._dir.path().join("token"));
    config.base_url = rig.fake.base_url.clone();
    config.request_timeout = Duration::from_secs(3);
    config.max_rate_limit_retries = 1;
    config.default_backoff = Duration::from_millis(5);
    config.max_backoff = Duration::from_millis(20);
    Arc::new(TimewebProvider::new(config).unwrap())
}

// ───────── Исходы удаления ─────────

/// Окно доведено до `deleting` по простою; возвращает момент, когда оно туда вошло.
async fn drive_to_deleting(rig: &Rig, t0: DateTime<Utc>) -> (WindowId, DateTime<Utc>) {
    let (id, session, at) = rig.drive_to_ready(120, t0).await;
    rig.tick(at + TimeDelta::seconds(1)).await;
    let when = at + TimeDelta::minutes(16);
    rig.tick(when).await;
    assert_eq!(rig.state(id).await, "deleting");
    drop(session);
    (id, when)
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn needs_confirmation_alerts_at_once_and_every_thirty_minutes_while_the_window_stays_live(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, when) = drive_to_deleting(&rig, t0).await;
    rig.fake.control().delete = DeleteMode::WithHash;

    rig.tick(when + TimeDelta::seconds(1)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.last_error.as_deref(), Some("needs_confirmation"));
    assert_eq!(rig.alert_codes().await, vec!["delete_needs_confirmation"]);
    assert_eq!(rig.deletes(), 1);
    // Окно живо: второго создать нельзя, резерв держится.
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::LiveWindowExists)
    ));
    // Раньше срока повтора ничего не происходит.
    rig.tick(when + TimeDelta::minutes(10)).await;
    assert_eq!(rig.alerts().await.len(), 1);
    assert_eq!(rig.deletes(), 1);
    // Через 30 минут: повторная попытка и повторный пуш.
    rig.tick(when + TimeDelta::minutes(32)).await;
    assert_eq!(rig.alerts().await.len(), 2);
    assert_eq!(rig.deletes(), 2);
    rig.tick(when + TimeDelta::minutes(64)).await;
    assert_eq!(rig.alerts().await.len(), 3);
    assert_eq!(rig.state(id).await, "deleting");

    // Владелец исправил флаг токена: следующая попытка удаляет и окно заканчивается.
    rig.fake.control().delete = DeleteMode::Ok;
    rig.tick(when + TimeDelta::minutes(100)).await;
    assert_eq!(rig.state(id).await, "ended");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_server_deleted_by_hand_while_confirmation_is_pending_ends_the_window(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, when) = drive_to_deleting(&rig, t0).await;
    rig.fake.control().delete = DeleteMode::WithHash;
    rig.tick(when + TimeDelta::seconds(1)).await;
    assert_eq!(rig.state(id).await, "deleting");
    // Владелец подтвердил удаление в панели: сервера нет в полном списке.
    rig.vanish_servers();
    rig.tick(when + TimeDelta::minutes(6)).await;
    assert_eq!(rig.state(id).await, "ended");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failing_delete_retries_with_backoff_and_alerts_after_three_failures(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, when) = drive_to_deleting(&rig, t0).await;
    rig.fake.control().delete = DeleteMode::Status(500);

    rig.tick(when + TimeDelta::seconds(1)).await;
    assert_eq!(rig.deletes(), 1);
    // Повтор не раньше паузы.
    rig.tick(when + TimeDelta::seconds(10)).await;
    assert_eq!(rig.deletes(), 1);
    rig.tick(when + TimeDelta::seconds(40)).await;
    assert_eq!(rig.deletes(), 2);
    assert!(
        rig.alert_codes().await.is_empty(),
        "two failures are not alerted"
    );
    rig.tick(when + TimeDelta::minutes(3)).await;
    assert_eq!(rig.deletes(), 3);
    assert_eq!(rig.window(id).await.delete_attempts, 3);
    assert_eq!(rig.alert_codes().await, vec!["delete_failing"]);
    assert_eq!(rig.state(id).await, "deleting");

    // Дальше ретраи идут, а пуш раз в 30 минут.
    rig.tick(when + TimeDelta::minutes(12)).await;
    assert!(rig.deletes() >= 4);
    assert_eq!(rig.alerts().await.len(), 1, "same half hour, same alert");
    rig.tick(when + TimeDelta::minutes(35)).await;
    assert_eq!(rig.alerts().await.len(), 2);

    // Провайдер ожил: удаление проходит.
    rig.fake.control().delete = DeleteMode::Ok;
    rig.tick(when + TimeDelta::minutes(50)).await;
    assert_eq!(rig.state(id).await, "ended");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn quarantine_is_confirmed_only_by_absence_from_a_full_listing(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, when) = drive_to_deleting(&rig, t0).await;
    // Карантин, но сервер ещё виден в списках проекта.
    let server_id = rig.window(id).await.server_id.unwrap();
    rig.fake.control().delete = DeleteMode::QuarantineListed;
    rig.tick(when + TimeDelta::seconds(1)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert!(window.delete_accepted_at.is_some());
    assert!(window.delete_confirmed_at.is_none());
    assert_eq!(window.alert_state["quarantined"], true);
    rig.tick(when + TimeDelta::seconds(20)).await;
    assert_eq!(
        rig.state(id).await,
        "deleting",
        "still listed: still billing"
    );
    assert_eq!(rig.deletes(), 1, "no second DELETE once it was accepted");
    rig.fake.control().remove_server(server_id);
    rig.tick(when + TimeDelta::seconds(40)).await;
    assert_eq!(rig.state(id).await, "ended");
    Ok(())
}

// ───────── Сверка ─────────

fn stray_name() -> String {
    format!("canterlot-runner-{}", Uuid::now_v7())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reconcile_deletes_a_stray_and_never_touches_other_names_or_projects(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let stray = stray_name();
    let (ours, other_name, malformed, upper, other_project) = {
        let mut state = rig.fake.control();
        (
            state.add_server(PROJECT, &stray),
            state.add_server(PROJECT, "web-frontend"),
            state.add_server(PROJECT, "canterlot-runner-not-a-uuid"),
            state.add_server(
                PROJECT,
                &format!("canterlot-runner-{}", Uuid::now_v7()).to_uppercase(),
            ),
            state.add_server(PROJECT + 1, &stray_name()),
        )
    };
    rig.tick(now_micros()).await;
    {
        let state = rig.fake.control();
        assert!(!state.servers.contains_key(&ours), "the stray is deleted");
        for kept in [other_name, malformed, upper, other_project] {
            assert!(
                state.servers.contains_key(&kept),
                "server {kept} is not ours to touch"
            );
        }
        assert_eq!(state.count("DELETE", "/api/v1/servers"), 1);
    }
    assert_eq!(rig.alert_codes().await, vec!["stray_server_deleted"]);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn servers_of_closed_windows_are_strays_too(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    // Окно, которое мы сочли неудавшимся, а сервер потом всё же появился.
    rig.fake.control().create = CreateMode::Status(500);
    let id = rig.create(60).await;
    rig.tick(t0).await;
    rig.tick(t0 + TimeDelta::minutes(10)).await;
    assert_eq!(rig.state(id).await, "failed");
    let late = rig
        .fake
        .control()
        .add_server(PROJECT, &windows::server_name(id));
    rig.tick(t0 + TimeDelta::minutes(15)).await;
    assert!(!rig.fake.control().servers.contains_key(&late));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_stray_that_cannot_be_deleted_alerts_every_half_hour(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.fake.control().add_server(PROJECT, &stray_name());
    rig.fake.control().delete = DeleteMode::WithHash;
    let t0 = now_micros();
    rig.tick(t0).await;
    assert_eq!(rig.alert_codes().await, vec!["stray_server_stuck"]);
    // Новая сверка в тот же момент (слот тот же): второго пуша нет.
    let again = CloudService::new(
        rig.pool.clone(),
        rig.registry.clone(),
        provider_for(&rig),
        rig.config.clone(),
    );
    again.tick(t0).await;
    assert_eq!(rig.alerts().await.len(), 1);
    assert_eq!(rig.deletes(), 2, "the stray is retried by every reconcile");
    // Через 40 минут слот другой: пуш повторяется.
    again.tick(t0 + TimeDelta::minutes(40)).await;
    assert_eq!(rig.alerts().await.len(), 2);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reconcile_runs_at_start_before_any_create(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    rig.fake.control().add_server(PROJECT, &stray_name());
    let id = rig.create(60).await;
    rig.tick(now_micros()).await;
    assert_eq!(rig.state(id).await, "identified");
    let requests = rig.fake.requests();
    let first_listing = requests
        .iter()
        .position(|r| r.method == "GET" && r.path.contains("/resources/servers"))
        .expect("a listing");
    let first_post = requests
        .iter()
        .position(|r| r.method == "POST" && r.path == "/api/v1/servers")
        .expect("a create");
    let first_delete = requests
        .iter()
        .position(|r| r.method == "DELETE")
        .expect("the stray delete");
    assert!(
        first_listing < first_post,
        "the listing comes before any create"
    );
    assert!(
        first_delete < first_post,
        "the stray goes before any create"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn without_a_successful_reconcile_nothing_is_created(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    rig.fake.control().list_fail_page = Some(FailPage {
        offset: 0,
        status: 500,
        times: None,
    });
    let t0 = now_micros();
    let id = rig.create(60).await;
    for seconds in [0, 10, 20] {
        rig.tick(t0 + TimeDelta::seconds(seconds)).await;
    }
    assert_eq!(rig.state(id).await, "create_intent");
    assert_eq!(rig.posts(), 0, "no create before a reconcile succeeded");
    rig.fake.control().list_fail_page = None;
    rig.tick(t0 + TimeDelta::seconds(30)).await;
    assert_eq!(rig.state(id).await, "identified");
    assert_eq!(rig.posts(), 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_create_call_interrupted_by_a_crash_is_never_repeated(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    ready_row(&rig).await;
    let now = now_micros();
    // Метка вызова есть, ответа нет, сервера в списке нет: звонок мог дойти.
    let orphan = raw_window(&rig.pool, "create_intent", None, Some(now)).await;
    let service = CloudService::new(
        rig.pool.clone(),
        rig.registry.clone(),
        provider_for(&rig),
        rig.config.clone(),
    );
    for seconds in [0, 10, 20, 400] {
        service.tick(now + TimeDelta::seconds(seconds)).await;
    }
    assert_eq!(rig.state(orphan).await, "create_unknown");
    assert_eq!(
        rig.posts(),
        0,
        "no POST at all: the window already had its call"
    );
    // Окно ожидания вышло, полный список пуст: окно закрыто, звонка так и не было.
    service.tick(now + TimeDelta::minutes(12)).await;
    assert_eq!(rig.state(orphan).await, "failed");
    assert_eq!(rig.posts(), 0);
    Ok(())
}

/// Строка окна в нужном состоянии, как её оставил бы упавший хаб.
async fn raw_window(
    pool: &PgPool,
    state: &str,
    server_id: Option<i64>,
    called: Option<DateTime<Utc>>,
) -> Uuid {
    let id = Uuid::now_v7();
    let hard_cap = called.map(|at| at + TimeDelta::minutes(60));
    sqlx::query(
        "insert into cloud_windows (id, name, state, trigger, server_id, host_key_public,
                                    price_hourly_kopecks, reservation_kopecks, month,
                                    hard_cap_minutes, created_at, create_called_at, hard_cap_at,
                                    drain_until, teardown_until, local_ip, dnat_port, attach_sent_at,
                                    delete_requested_at, delete_accepted_at, profiles)
         values ($1, $2, $3, 'owner', $4, 'ssh-ed25519 AAAA', $5, $6, $7, 60, $8, $9, $10,
                 $10 + interval '30 minutes', $10 + interval '60 minutes',
                 '192.0.2.10', 22100,
                 case when $3 in ('ready', 'draining') then $9 else null end,
                 case when $3 = 'deleting' then $9 else null end,
                 case when $3 = 'deleting' then $9 else null end,
                 '[{\"configurator_id\":151,\"cpu\":32,\"ram_gb\":64,\"disk_gb\":80,\"hourly_kopecks\":2845,\"availability_zone\":\"msk-1\"}]'::jsonb)",
    )
    .bind(id)
    .bind(windows::server_name(id))
    .bind(state)
    .bind(server_id)
    .bind(HOURLY)
    .bind(3 * HOURLY)
    .bind(windows::month_of(now_micros()))
    .bind(called.unwrap_or_else(Utc::now) - TimeDelta::minutes(1))
    .bind(called)
    .bind(hard_cap)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn ready_row(rig: &Rig) {
    let config = rig.config.clone();
    canterlot_hub::db::in_tx(&rig.pool, move |conn| {
        Box::pin(async move {
            windows::ensure_cloud_machine(conn, &config, true).await?;
            Ok::<_, sqlx::Error>(())
        })
    })
    .await
    .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_hub_restart_resumes_windows_in_every_state(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    ready_row(&rig).await;
    let now = now_micros();
    let restart = || {
        CloudService::new(
            rig.pool.clone(),
            rig.registry.clone(),
            provider_for(&rig),
            rig.config.clone(),
        )
    };
    let close = |id: Uuid| {
        let pool = rig.pool.clone();
        async move {
            sqlx::query(
                "update cloud_windows set state = 'ended', final_cost_kopecks = 0 where id = $1",
            )
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }
    };

    // create_intent без вызова: после сверки создаётся.
    let intent = raw_window(&rig.pool, "create_intent", None, None).await;
    restart().tick(now).await;
    assert_eq!(rig.state(intent).await, "identified");
    assert_eq!(rig.posts(), 1);
    close(intent).await;

    // create_intent с меткой вызова: звонок мог быть, второго не будет; сервер по имени
    // найден сверкой.
    let orphan = raw_window(&rig.pool, "create_intent", None, Some(now)).await;
    let server = rig
        .fake
        .control()
        .add_server(PROJECT, &windows::server_name(orphan));
    restart().tick(now + TimeDelta::seconds(5)).await;
    let window = rig.window(orphan).await;
    assert_eq!(window.state, "identified");
    assert_eq!(window.server_id, Some(server));
    assert_eq!(rig.posts(), 1, "no second POST for an orphaned create");
    close(orphan).await;

    // create_unknown, сервера нет и окно вышло: failed.
    let unknown = raw_window(
        &rig.pool,
        "create_unknown",
        None,
        Some(now - TimeDelta::minutes(30)),
    )
    .await;
    restart().tick(now + TimeDelta::seconds(10)).await;
    assert_eq!(rig.state(unknown).await, "failed");

    // identified и ready с живым сервером остаются.
    for state in ["identified", "ready", "draining"] {
        // Подключение ждём 15 минут: `identified` моложе этого срока.
        let called = now - TimeDelta::minutes(if state == "identified" { 5 } else { 20 });
        let server = rig.fake.control().add_server(PROJECT, "x");
        let id = raw_window(&rig.pool, state, Some(server), Some(called)).await;
        rig.fake.control().servers.get_mut(&server).unwrap().name = windows::server_name(id);
        let _session = rig.carrier_session();
        if state == "draining" {
            // Без работы окно на сносе удаляется сразу; с работой дорабатывает.
            rig.seed_job(Some(rig.cloud_machine().await), "running", &["cloud"])
                .await;
        }
        restart().tick(now + TimeDelta::seconds(20)).await;
        assert_eq!(
            rig.state(id).await,
            state,
            "{state} with a live server stays"
        );
        close(id).await;
        rig.fake.control().servers.remove(&server);
    }

    // identified с уже созданным правилом: после перезапуска второго POST нет, шлём attach.
    let server601 = rig.fake.control().add_server(PROJECT, "x");
    let with_rule = raw_window(
        &rig.pool,
        "identified",
        Some(server601),
        Some(now - TimeDelta::minutes(5)),
    )
    .await;
    rig.fake.control().servers.get_mut(&server601).unwrap().name = windows::server_name(with_rule);
    rig.fake
        .control()
        .dnat_rules
        .push(crate::support::fake_timeweb::FakeDnatRule {
            id: "dnat-restarted".to_owned(),
            local_ip: "192.0.2.10".to_owned(),
            local_port: "22".to_owned(),
            public_ip: ROUTER_IP.to_owned(),
            public_port: "22100".to_owned(),
            protocol: "tcp".to_owned(),
        });
    sqlx::query("update cloud_windows set dnat_rule_id = 'dnat-restarted' where id = $1")
        .bind(with_rule)
        .execute(&rig.pool)
        .await?;
    let posts_before = rig.dnat_posts();
    let _session = rig.carrier_session();
    restart().tick(now + TimeDelta::seconds(25)).await;
    assert!(rig.window(with_rule).await.attach_sent_at.is_some());
    assert_eq!(rig.dnat_posts(), posts_before, "no second DNAT POST");
    close(with_rule).await;
    rig.fake.control().servers.remove(&server601);
    rig.fake.control().dnat_rules.clear();

    // identified с меткой вызова правила: усыновляем его из полного списка.
    let server602 = rig.fake.control().add_server(PROJECT, "x");
    let pending_rule = raw_window(
        &rig.pool,
        "identified",
        Some(server602),
        Some(now - TimeDelta::minutes(5)),
    )
    .await;
    rig.fake.control().servers.get_mut(&server602).unwrap().name =
        windows::server_name(pending_rule);
    rig.fake
        .control()
        .dnat_rules
        .push(crate::support::fake_timeweb::FakeDnatRule {
            id: "dnat-adopted".to_owned(),
            local_ip: "192.0.2.10".to_owned(),
            local_port: "22".to_owned(),
            public_ip: ROUTER_IP.to_owned(),
            public_port: "22100".to_owned(),
            protocol: "tcp".to_owned(),
        });
    sqlx::query("update cloud_windows set dnat_create_attempted_at = $2 where id = $1")
        .bind(pending_rule)
        .bind(now - TimeDelta::minutes(6))
        .execute(&rig.pool)
        .await?;
    let posts_before = rig.dnat_posts();
    restart().tick(now + TimeDelta::seconds(26)).await;
    assert_eq!(
        rig.window(pending_rule).await.dnat_rule_id.as_deref(),
        Some("dnat-adopted")
    );
    assert_eq!(rig.dnat_posts(), posts_before, "no second DNAT POST");
    close(pending_rule).await;
    rig.fake.control().servers.remove(&server602);
    rig.fake.control().dnat_rules.clear();

    // deleting с правилом, ожидающим удаления: после перезапуска правило уходит, окно кончается.
    let deleting_rule = raw_window(
        &rig.pool,
        "deleting",
        Some(603),
        Some(now - TimeDelta::minutes(20)),
    )
    .await;
    rig.fake
        .control()
        .dnat_rules
        .push(crate::support::fake_timeweb::FakeDnatRule {
            id: "dnat-deleting".to_owned(),
            local_ip: "192.0.2.10".to_owned(),
            local_port: "22".to_owned(),
            public_ip: ROUTER_IP.to_owned(),
            public_port: "22100".to_owned(),
            protocol: "tcp".to_owned(),
        });
    sqlx::query("update cloud_windows set dnat_rule_id = 'dnat-deleting' where id = $1")
        .bind(deleting_rule)
        .execute(&rig.pool)
        .await?;
    restart().tick(now + TimeDelta::seconds(27)).await;
    let window = rig.window(deleting_rule).await;
    assert_eq!(window.state, "ended");
    assert!(window.dnat_gone_at.is_some());
    assert!(rig.fake.control().dnat_rules.is_empty());

    // deleting, сервер уже пропал из списка: конец оплаты подтверждён.
    let deleting = raw_window(
        &rig.pool,
        "deleting",
        Some(777),
        Some(now - TimeDelta::minutes(20)),
    )
    .await;
    restart().tick(now + TimeDelta::seconds(30)).await;
    let window = rig.window(deleting).await;
    assert_eq!(window.state, "ended");
    assert_eq!(window.final_cost_kopecks, Some(HOURLY));

    // identified, но сервера нет в полном списке и срок вышел: gone.
    let vanished = raw_window(
        &rig.pool,
        "ready",
        Some(888),
        Some(now - TimeDelta::minutes(20)),
    )
    .await;
    restart().tick(now + TimeDelta::seconds(40)).await;
    assert_eq!(rig.state(vanished).await, "gone");
    Ok(())
}

// ───────── Строка раннера `cloud` ─────────

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_cloud_row_is_reused_reactivated_on_attach_and_revoked_at_the_end(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let id = rig.create(120).await;
    rig.tick(t0).await;
    let machine = rig.cloud_machine().await;
    let row: (
        String,
        String,
        Option<Uuid>,
        Option<String>,
        i32,
        Vec<String>,
    ) = sqlx::query_as(
        "select name, role, parent_machine_id, root_dir, rank, labels from machines where id = $1",
    )
    .bind(machine)
    .fetch_one(&rig.pool)
    .await?;
    assert_eq!(
        row,
        (
            "cloud".into(),
            "runner".into(),
            Some(rig.carrier),
            Some("/srv/canterlot-runner".into()),
            0,
            vec!["cloud".into(), "kernel-6.12".into()]
        )
    );
    // Строки ещё нет токена, но несущей она уже в списке: окно `identified`.
    let listed = |pool: PgPool, carrier: Uuid| async move {
        machines::runners_of(&pool, carrier)
            .await
            .unwrap()
            .into_iter()
            .map(|runner| runner.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(listed(rig.pool.clone(), rig.carrier).await, vec!["cloud"]);

    // Токен выдаётся ротацией с оживлением, состояние воркера сбрасывается.
    sqlx::query("update machines set cj_journal_id = gen_random_uuid() where id = $1")
        .bind(machine)
        .execute(&rig.pool)
        .await?;
    let token = machines::request_runner_token(&rig.pool, rig.carrier, machine, false).await?;
    assert_eq!(machines::verify(&rig.pool, &token).await?, Some(machine));
    let journal: Option<Uuid> =
        sqlx::query_scalar("select cj_journal_id from machines where id = $1")
            .bind(machine)
            .fetch_one(&rig.pool)
            .await?;
    assert_eq!(journal, None, "attach resets the local state");
    // Повторный запрос (сообщение потерялось) снова выдаёт токен и гасит прежний.
    let second = machines::request_runner_token(&rig.pool, rig.carrier, machine, false).await?;
    assert_eq!(machines::verify(&rig.pool, &token).await?, None);
    assert_eq!(machines::verify(&rig.pool, &second).await?, Some(machine));

    // Конец окна: раннер отозван, его задачи `lost`, из списка несущей он ушёл.
    let session = rig.attach().await;
    rig.tick(t0 + TimeDelta::seconds(5)).await;
    rig.tick(t0 + TimeDelta::seconds(6)).await;
    rig.tick(t0 + TimeDelta::seconds(7)).await;
    assert_eq!(rig.state(id).await, "ready");
    let running = rig.seed_job(Some(machine), "running", &["cloud"]).await;
    let queued = rig.seed_job(Some(machine), "queued", &["cloud"]).await;
    let finished = rig.seed_job(Some(machine), "succeeded", &["cloud"]).await;
    let requested = windows::request_delete(
        &rig.pool,
        id,
        windows::REASON_OWNER,
        t0 + TimeDelta::minutes(1),
    )
    .await?;
    assert_eq!(requested, windows::DeleteRequested::Deleting);
    assert_eq!(rig.job_state(running).await, "lost");
    assert_eq!(rig.job_state(queued).await, "lost");
    assert_eq!(rig.job_state(finished).await, "succeeded");
    assert_eq!(machines::verify(&rig.pool, &second).await?, None);
    assert!(listed(rig.pool.clone(), rig.carrier).await.is_empty());
    drop(session);
    rig.tick(t0 + TimeDelta::minutes(1) + TimeDelta::seconds(1))
        .await;
    assert_eq!(rig.state(id).await, "ended");
    // Окно кончилось: оживать нечему, токен не выдаётся.
    let refused = machines::request_runner_token(&rig.pool, rig.carrier, machine, true).await;
    assert_eq!(refused, Err(RunnerRefusal::Revoked));

    // Следующее окно переиспользует ту же строку и оживляет её при подключении.
    let next = rig.create(60).await;
    rig.tick(t0 + TimeDelta::minutes(20)).await;
    assert_eq!(rig.state(next).await, "identified");
    assert_eq!(
        rig.cloud_machine().await,
        machine,
        "one row for every window"
    );
    assert_eq!(listed(rig.pool.clone(), rig.carrier).await, vec!["cloud"]);
    let fresh = machines::request_runner_token(&rig.pool, rig.carrier, machine, false).await?;
    assert_eq!(machines::verify(&rig.pool, &fresh).await?, Some(machine));
    let rows: i64 = sqlx::query_scalar("select count(*) from machines where name = 'cloud'")
        .fetch_one(&rig.pool)
        .await?;
    assert_eq!(rows, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn static_runners_keep_their_token_flags(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let spec = machines::RunnerSpec {
        parent: rig.carrier,
        root_dir: "/srv/runner".into(),
        rank: 20,
        labels: vec!["build-host".into()],
    };
    let runner = machines::add_runner(&rig.pool, rig.owner, "runner-a", &spec).await?;
    machines::request_runner_token(&rig.pool, rig.carrier, runner.machine_id, false).await?;
    machines::revoke(&rig.pool, runner.machine_id).await?;
    // Отозванный статический раннер не оживает запросом, окон у него нет.
    assert_eq!(
        machines::request_runner_token(&rig.pool, rig.carrier, runner.machine_id, true).await,
        Err(RunnerRefusal::Revoked)
    );
    Ok(())
}

#[tokio::test]
async fn attach_runner_is_a_machine_level_command_only_with_runners_v1() {
    use canterlot_hub::outbox::{CommandRow, OutboxError, to_proto, to_proto_for_session};
    use canterlot_proto::worker::v1::command;
    let carrier = Uuid::now_v7();
    let window = Uuid::now_v7();
    let machine = Uuid::now_v7();
    let row = CommandRow {
        id: Uuid::now_v7(),
        machine_id: carrier,
        conversation_id: None,
        execution_id: None,
        execution_generation: None,
        kind: "attach_runner".into(),
        payload: serde_json::json!({
            "window_id": window, "machine_id": machine, "host": "203.0.113.7",
            "host_key_public": "ssh-ed25519 AAAA", "root_dir": "/srv/canterlot-runner",
            "max_running": 4, "lease_slots": 2,
        }),
        dispatch_attempt: 1,
    };
    let command = to_proto(&row).unwrap();
    assert_eq!(command.execution_id, "");
    assert_eq!(command.execution_generation, 0);
    let Some(command::Kind::AttachRunner(attach)) = command.kind else {
        panic!("an attach_runner command");
    };
    assert_eq!(attach.window_id, window.to_string());
    assert_eq!(attach.machine_id, machine.to_string());
    assert_eq!(attach.host, "203.0.113.7");
    assert_eq!(attach.host_key_public, "ssh-ed25519 AAAA");
    assert_eq!(attach.root_dir, "/srv/canterlot-runner");
    assert_eq!((attach.max_running, attach.lease_slots), (4, 2));

    let registry = Registry::new(Default::default());
    {
        let _old = registry.connect_for_tests(carrier, &["jobs_inplace_v1".to_owned()]);
        assert!(matches!(
            to_proto_for_session(&row, &registry),
            Err(OutboxError::CapabilityMissing)
        ));
    }
    let _new = registry.connect_for_tests(carrier, &["runners_v1".to_owned()]);
    assert!(to_proto_for_session(&row, &registry).is_ok());
    // Без обязательных полей команда не собирается.
    let broken = CommandRow {
        payload: serde_json::json!({"window_id": window}),
        ..row
    };
    assert!(matches!(
        to_proto(&broken),
        Err(OutboxError::InvalidCommand)
    ));
}

// ───────── Простой и отключение ─────────

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idle_counts_running_queued_and_takeable_waiting_jobs(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _session, at) = rig.drive_to_ready(240, t0).await;
    let machine = rig.cloud_machine().await;

    // Идущая задача: простоя нет, окно живёт сколько угодно.
    let running = rig.seed_job(Some(machine), "running", &["cloud"]).await;
    rig.tick(at + TimeDelta::minutes(1)).await;
    rig.tick(at + TimeDelta::minutes(40)).await;
    assert_eq!(rig.state(id).await, "ready");
    assert!(rig.window(id).await.idle_since.is_none());
    sqlx::query("update jobs set state = 'succeeded' where id = $1")
        .bind(running)
        .execute(&rig.pool)
        .await?;

    // Ждущая задача с метками, которых у облака нет: окно её не возьмёт, это простой.
    let foreign = rig.seed_job(None, "waiting_runner", &["build-host"]).await;
    rig.tick(at + TimeDelta::minutes(41)).await;
    assert!(rig.window(id).await.idle_since.is_some());
    // Ждущая задача, которую облако взяло бы: простой сбрасывается.
    let takeable = rig
        .seed_job(None, "waiting_runner", &["cloud", "kernel-6.12"])
        .await;
    rig.tick(at + TimeDelta::minutes(50)).await;
    assert!(rig.window(id).await.idle_since.is_none());
    assert_eq!(rig.state(id).await, "ready");
    // Задача ушла: отсчёт простоя начинается заново и доводит до удаления.
    sqlx::query("update jobs set state = 'canceled' where id in ($1, $2)")
        .bind(foreign)
        .bind(takeable)
        .execute(&rig.pool)
        .await?;
    rig.tick(at + TimeDelta::minutes(60)).await;
    rig.tick(at + TimeDelta::minutes(74)).await;
    assert_eq!(rig.state(id).await, "ready");
    rig.tick(at + TimeDelta::minutes(75)).await;
    assert_eq!(rig.state(id).await, "deleting");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idle_ignores_work_the_cloud_runner_cannot_take(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _session, at) = rig.drive_to_ready(240, t0).await;
    let waiting = rig
        .seed_waiting_job(&["cloud", "kernel-6.12"], &["runner"], None)
        .await;

    // Только `runner`: облачный раннер такой заказ не возьмёт, это простой.
    rig.tick(at + TimeDelta::minutes(1)).await;
    assert!(
        rig.window(id).await.idle_since.is_some(),
        "runner-only work does not keep the cloud window alive"
    );
    // Заказ, названный другой машине, тоже не удерживает окно.
    rig.tick(at + TimeDelta::minutes(2)).await;
    sqlx::query(
        "update jobs set placement = '{local,runner,cloud}', requested_machine_id = $2 where id = $1",
    )
    .bind(waiting)
    .bind(rig.carrier)
    .execute(&rig.pool)
    .await?;
    rig.tick(at + TimeDelta::minutes(3)).await;
    assert!(rig.window(id).await.idle_since.is_some());
    // Облачный заказ простой сбрасывает.
    sqlx::query("update jobs set placement = '{cloud}', requested_machine_id = null where id = $1")
        .bind(waiting)
        .execute(&rig.pool)
        .await?;
    rig.tick(at + TimeDelta::minutes(4)).await;
    assert!(rig.window(id).await.idle_since.is_none());
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_cloud_row_is_placement_kind_cloud(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (_id, _session, _at) = rig.drive_to_ready(240, t0).await;
    let machine = rig.cloud_machine().await;
    sqlx::query(
        "update machines set capabilities = array['jobs_inplace_v1'], jobs_max_running = 2
          where id = $1",
    )
    .bind(machine)
    .execute(&rig.pool)
    .await?;

    let cloud_job = rig.seed_sourced_waiting(&["cloud"], &["cloud"]).await;
    let runner_job = rig.seed_sourced_waiting(&["cloud"], &["runner"]).await;
    let placed = place::place_carrier(&rig.pool, &rig.registry, rig.carrier).await?;
    assert_eq!(placed, 1);
    assert_eq!(rig.job_state(cloud_job).await, "staging");
    assert_eq!(
        rig.job_state(runner_job).await,
        "waiting_runner",
        "a runner-only job must not be bound to the cloud machine"
    );
    let bound: Option<Uuid> = sqlx::query_scalar("select machine_id from jobs where id = $1")
        .bind(cloud_job)
        .fetch_one(&rig.pool)
        .await?;
    assert_eq!(bound, Some(machine));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_disconnected_runner_is_deleted_after_ten_minutes_only_while_the_carrier_is_connected(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, session, at) = rig.drive_to_ready(240, t0).await;
    let machine = rig.cloud_machine().await;
    // Работа есть: к простою удаление не относится.
    rig.seed_job(Some(machine), "running", &["cloud"]).await;

    // Раннер отключился, несущая на связи.
    drop(session);
    rig.tick(at + TimeDelta::minutes(1)).await;
    assert!(rig.window(id).await.disconnected_since.is_some());
    rig.tick(at + TimeDelta::minutes(10)).await;
    assert_eq!(rig.state(id).await, "ready");
    rig.tick(at + TimeDelta::minutes(11)).await;
    assert_eq!(rig.state(id).await, "deleting");
    assert_eq!(
        rig.window(id).await.delete_reason.as_deref(),
        Some("disconnected")
    );
    drop(carrier);

    // Несущая не на связи: отключённость раннера ничего не доказывает, окно живёт.
    rig.tick(at + TimeDelta::minutes(12)).await;
    let second = rig.create(240).await;
    assert_eq!(rig.state(second).await, "create_intent");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_disconnected_runner_with_a_disconnected_carrier_is_not_deleted(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, session, at) = rig.drive_to_ready(240, t0).await;
    rig.seed_job(Some(rig.cloud_machine().await), "running", &["cloud"])
        .await;
    drop(session);
    drop(carrier);
    for minutes in [1, 15, 40] {
        rig.tick(at + TimeDelta::minutes(minutes)).await;
        assert_eq!(rig.state(id).await, "ready", "after {minutes} min");
    }
    // Несущая вернулась: отсчёт идёт с этого момента.
    let _carrier = rig.carrier_session();
    rig.tick(at + TimeDelta::minutes(41)).await;
    rig.tick(at + TimeDelta::minutes(50)).await;
    assert_eq!(rig.state(id).await, "ready");
    rig.tick(at + TimeDelta::minutes(52)).await;
    assert_eq!(rig.state(id).await, "deleting");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_runner_that_never_connects_is_deleted_after_the_ready_timeout(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let id = rig.create(120).await;
    rig.tick(t0).await;
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    rig.tick(t0 + TimeDelta::minutes(14)).await;
    assert_eq!(rig.state(id).await, "identified");
    rig.tick(t0 + TimeDelta::minutes(15)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.delete_reason.as_deref(), Some("ready_timeout"));
    // Команда подключения, которую несущая так и не выполнила, больше не нужна.
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from commands where kind = 'attach_runner' and delivery_state <> 'completed'",
    )
    .fetch_one(&rig.pool)
    .await?;
    assert_eq!(pending, 0);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_machine_that_is_not_a_carrier_gets_no_attach_command(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    machines::set_carrier(&rig.pool, rig.carrier, false).await?;
    let t0 = now_micros();
    let id = rig.create(120).await;
    rig.tick(t0).await;
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    rig.tick(t0 + TimeDelta::seconds(2)).await;
    let window = rig.window(id).await;
    assert!(window.attach_sent_at.is_none());
    assert_eq!(window.last_error.as_deref(), Some("carrier_unavailable"));
    let sent: i64 =
        sqlx::query_scalar("select count(*) from commands where kind = 'attach_runner'")
            .fetch_one(&rig.pool)
            .await?;
    assert_eq!(sent, 0);
    Ok(())
}

// ───────── Удалить сейчас ─────────

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_now_cancels_an_unsent_create_and_revokes_a_running_window(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let now = now_micros();
    let id = rig.create(60).await;
    assert_eq!(
        windows::request_delete(&rig.pool, id, windows::REASON_OWNER, now).await?,
        windows::DeleteRequested::Canceled
    );
    let window = rig.window(id).await;
    assert_eq!(
        (window.state.as_str(), window.final_cost_kopecks),
        ("failed", Some(0))
    );
    assert_eq!(rig.posts(), 0);
    assert_eq!(
        windows::request_delete(&rig.pool, Uuid::now_v7(), windows::REASON_OWNER, now).await?,
        windows::DeleteRequested::NotFound
    );
    assert_eq!(
        windows::request_delete(&rig.pool, id, windows::REASON_OWNER, now).await?,
        windows::DeleteRequested::AlreadyEnded
    );

    let (id, _session, at) = rig.drive_to_ready(60, now).await;
    assert_eq!(
        windows::request_delete(&rig.pool, id, windows::REASON_OWNER, at).await?,
        windows::DeleteRequested::Deleting
    );
    assert_eq!(
        windows::request_delete(&rig.pool, id, windows::REASON_OWNER, at).await?,
        windows::DeleteRequested::Deleting,
        "idempotent"
    );
    rig.tick(at + TimeDelta::seconds(1)).await;
    assert_eq!(rig.state(id).await, "ended");
    assert_eq!(rig.deletes(), 1);
    Ok(())
}

// ───────── Включение при старте ─────────

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn startup_enables_the_service_only_with_a_readable_token_and_a_real_carrier(
    pool: PgPool,
) -> TestResult {
    use canterlot_hub::cloud::service::from_config;
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let rig = rig(pool).await;
    let registry = Registry::new(Default::default());
    // Нет таблицы: облако выключено без единого сообщения.
    assert!(from_config(&rig.pool, &registry, None).await.is_none());

    let dir = tempfile::tempdir()?;
    let token_file = dir.path().join("secret-token-file");
    std::fs::write(&token_file, format!("{TOKEN}\n"))?;
    let mut config = rig.config.clone();
    config.token_file = token_file.clone();
    assert!(
        from_config(&rig.pool, &registry, Some(config.clone()))
            .await
            .is_some()
    );

    // Файла токена нет или он пуст.
    let mut missing = config.clone();
    missing.token_file = dir.path().join("absent");
    assert!(
        from_config(&rig.pool, &registry, Some(missing))
            .await
            .is_none()
    );
    let empty = dir.path().join("empty");
    std::fs::write(&empty, "\n")?;
    let mut blank = config.clone();
    blank.token_file = empty;
    assert!(
        from_config(&rig.pool, &registry, Some(blank))
            .await
            .is_none()
    );

    // Машина из конфигурации не несущая: окно без несущей только жгло бы деньги.
    machines::set_carrier(&rig.pool, rig.carrier, false).await?;
    assert!(
        from_config(&rig.pool, &registry, Some(config.clone()))
            .await
            .is_none()
    );
    let mut stranger = config.clone();
    stranger.carrier_machine_id = Uuid::now_v7();
    assert!(
        from_config(&rig.pool, &registry, Some(stranger))
            .await
            .is_none()
    );
    // Негодная конфигурация.
    let mut invalid = config;
    invalid.profile[0].cpu = 0;
    assert!(
        from_config(&rig.pool, &registry, Some(invalid))
            .await
            .is_none()
    );

    let logs = String::from_utf8(capture.0.lock().unwrap().clone())?;
    assert_eq!(logs.matches("cloud_disabled").count(), 5, "{logs}");
    assert!(logs.contains("cloud_enabled") || logs.contains("cloud runner windows enabled"));
    for secret in [TOKEN, "secret-token-file", KEY] {
        assert!(
            !logs.contains(secret),
            "{secret} leaked into the startup log"
        );
    }
    Ok(())
}

// ───────── Секреты ─────────

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn neither_the_token_nor_the_host_private_key_reach_the_logs(pool: PgPool) -> TestResult {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.fake.control().echo_authorization_in_errors = true;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    // Путь с отказами по всем веткам: создание, список, удаление.
    let (id, when) = drive_to_deleting(&rig, t0).await;
    rig.fake.control().delete = DeleteMode::Status(500);
    rig.tick(when + TimeDelta::seconds(1)).await;
    rig.fake.control().delete = DeleteMode::WithHash;
    rig.tick(when + TimeDelta::minutes(40)).await;
    rig.fake.control().list_fail_page = Some(FailPage {
        offset: 0,
        status: 500,
        times: None,
    });
    rig.tick(when + TimeDelta::minutes(50)).await;
    let window = rig.window(id).await;
    let key = std::str::from_utf8(&capture.0.lock().unwrap())
        .unwrap()
        .to_owned();
    assert!(!key.is_empty(), "the capture works");
    for secret in [TOKEN, "BEGIN OPENSSH PRIVATE KEY", "OPENSSH PRIVATE"] {
        assert!(!key.contains(secret), "{secret} leaked into the logs");
    }
    let shown = format!("{window:?}{:?}{:?}", rig.config, rig.alerts().await);
    assert!(!shown.contains(TOKEN) && !shown.contains("PRIVATE KEY"));
    Ok(())
}
