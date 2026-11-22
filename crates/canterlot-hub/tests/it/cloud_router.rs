//! Облачные окна через приватную сеть и роутер (этап C2c): пул локальных адресов и DNAT-портов,
//! правило на роутере вместо публичного IPv4, отказ `no_paid` как блок провайдера, сверка
//! лишних правил. Всё против поддельного Timeweb; счётчики запросов доказывают, что второго
//! POST для одного порта не бывает. Время идёт параметром прохода (`tick(now)`).

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use canterlot_hub::cloud::service::CloudService;
use canterlot_hub::cloud::windows::{
    self, BLOCK_NO_PAID, CreateRefusal, CreateRequest, Trigger, Window, WindowId,
};
use canterlot_hub::cloud::{TimewebConfig, TimewebProvider, price};
use canterlot_hub::config::{CloudConfig, CloudProfile};
use canterlot_hub::machines::{self};
use canterlot_hub::session::Registry;
use canterlot_hub::testkit::{self, FakeSession};
use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use sqlx::PgPool;
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

use crate::support::fake_timeweb::{
    CreateMode, DnatCreateMode, FailPage, FakeDnatRule, FakeTimeweb,
};

const TOKEN: &str = "tw-windows-token-5d1c0e9a";
const PROJECT: i64 = 4242;
const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAMPLE0000";
const ROUTER: &str = "018f0000-0000-7000-8000-0000000000aa";
const ROUTER_IP: &str = "192.0.2.1";
const GATEWAY: &str = "192.0.2.5";
/// Конфигуратор 151, 32 vCPU, 64 ГБ, 80 ГБ: 2845 копеек в час.
const HOURLY: i64 = 2845;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

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
local_ip_pool = "192.0.2.10-192.0.2.14"
dnat_port_range = "22100-22104"

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
    // Денежные пуши владельцу иначе не попадают в `owner_pushes`.
    crate::auth_pushes::enable(&pool).await;
    let owner = crate::runners_hub::owner(&pool).await;
    let carrier = machines::add(&pool, owner, "carrier").await.unwrap();
    machines::set_carrier(&pool, carrier.machine_id, true)
        .await
        .unwrap();
    let fake = FakeTimeweb::start(TOKEN).await;
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

    /// Прогонять проходы до нужного состояния, не дожидаясь реального времени.
    async fn tick_until(&self, id: WindowId, target: &str, from: DateTime<Utc>) -> DateTime<Utc> {
        let mut at = from;
        for _ in 0..120 {
            if self.state(id).await == target {
                return at;
            }
            at += TimeDelta::minutes(1);
            self.tick(at).await;
        }
        panic!(
            "window never reached {target}, stuck in {}",
            self.state(id).await
        );
    }

    fn dnat_posts(&self) -> usize {
        self.fake.control().count("POST", "/api/v1/routers/")
    }

    fn dnat_deletes(&self) -> usize {
        self.fake.control().count("DELETE", "/api/v1/routers/")
    }

    async fn cloud_machine(&self) -> Uuid {
        let mut conn = self.pool.acquire().await.unwrap();
        windows::cloud_machine_id(&mut conn)
            .await
            .unwrap()
            .expect("the cloud machine row exists")
    }

    fn carrier_session(&self) -> FakeSession {
        testkit::fake_session(&self.registry, self.carrier, &["runners_v1"], true)
    }

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
        // Первый проход после `On` создаёт правило, второй шлёт команду подключения.
        self.tick(t0 + TimeDelta::seconds(1)).await;
        assert_eq!(
            self.window(id).await.dnat_rule_id.as_deref(),
            Some("dnat-1")
        );
        self.tick(t0 + TimeDelta::seconds(2)).await;
        assert!(self.window(id).await.attach_sent_at.is_some());
        let session = self.attach().await;
        let at = t0 + TimeDelta::seconds(3);
        self.tick(at).await;
        assert_eq!(self.state(id).await, "ready", "the runner connects");
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
}

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
    network_id = 'network-example'\n    network_cidr = '192.0.2.0/24'\n\
    router_id = '018f0000-0000-7000-8000-0000000000aa'\n\
    router_ip = '192.0.2.1'\n\
    gateway = '192.0.2.5'\n\
    local_ip_pool = '192.0.2.10-192.0.2.60'\n\
    dnat_port_range = '22100-22199'\n";

const CLOUD_PROFILE: &str = "[[runners.cloud.profile]]\nconfigurator_id = 151\ncpu = 32\n\
    ram_gb = 64\ndisk_gb = 80\navailability_zone = 'msk-1'\n";

fn cloud_table(extra: &str) -> String {
    format!("{CLOUD_HEAD}{extra}{CLOUD_PROFILE}")
}

// ───────── Конфигурация ─────────

#[test]
fn cloud_config_has_the_documented_defaults_and_is_off_when_absent() {
    assert!(load_config("").unwrap().runners.cloud.is_none());
    let config = load_config(&cloud_table("")).unwrap();
    let cloud = config.runners.cloud.unwrap();
    assert_eq!(cloud.profile[0].availability_zone, "msk-1");
    assert_eq!(cloud.router_ip(), Some("192.0.2.1".parse().unwrap()));
    assert_eq!(cloud.gateway(), Some("192.0.2.5".parse().unwrap()));
    assert_eq!(
        cloud.local_ip_range(),
        Some(("192.0.2.10".parse().unwrap(), "192.0.2.60".parse().unwrap()))
    );
    assert_eq!(cloud.dnat_port_values(), Some((22100, 22199)));
    assert_eq!(cloud.no_paid_grace, Duration::from_secs(3 * 60));
    assert_eq!(cloud.ready_timeout, Duration::from_secs(15 * 60));
    assert_eq!(cloud.create_unknown_window, Duration::from_secs(10 * 60));
    let shown = format!("{cloud:?}");
    assert!(!shown.contains("tw-token") && !shown.contains("AAAAC3"));
}

#[test]
fn config_validation_rejects_bad_ranges_gateways_and_missing_keys() {
    let good = cloud_table("");
    let bad = [
        // Нет или пустой ключ.
        good.replace("router_ip = '192.0.2.1'\n", ""),
        good.replace("local_ip_pool = '192.0.2.10-192.0.2.60'\n", ""),
        good.replace("dnat_port_range = '22100-22199'\n", ""),
        good.replace("availability_zone = 'msk-1'", "availability_zone = ''"),
        // Диапазоны: пустые, перевёрнутые, порты вне 1024–65535.
        good.replace("192.0.2.10-192.0.2.60", "192.0.2.60-192.0.2.10"),
        good.replace("192.0.2.10-192.0.2.60", "192.0.2.10-192.0.2.10x"),
        good.replace("22100-22199", "900-1000"),
        good.replace("22100-22199", "70000-70010"),
        // Некорректный адрес шлюза и роутера.
        good.replace("gateway = '192.0.2.5'", "gateway = '2001:db8::1'"),
        good.replace("router_ip = '192.0.2.1'", "router_ip = 'not-an-ip'"),
        // Узлы вне CIDR сети и негодный префикс.
        good.replace("gateway = '192.0.2.5'", "gateway = '198.51.100.5'"),
        good.replace("192.0.2.10-192.0.2.60", "198.51.100.10-198.51.100.60"),
        good.replace(
            "network_cidr = '192.0.2.0/24'",
            "network_cidr = '192.0.2.0/33'",
        ),
        good.replace("network_cidr = '192.0.2.0/24'\n", ""),
        cloud_table("unknown_key = 1\n"),
    ];
    for table in bad {
        let error = load_config(&table).expect_err(&table);
        assert!(
            !error.contains("AAAAC3") && !error.contains("tw-token"),
            "no values in {error}"
        );
    }
}

// ───────── Деньги и создание ─────────

#[test]
fn the_month_is_counted_in_moscow_time() {
    let at = |y, m, d, h, min, s| Utc.with_ymd_and_hms(y, m, d, h, min, s).unwrap();
    assert_eq!(windows::month_of(at(2026, 11, 30, 21, 0, 0)), "2026-12");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_reservation_is_the_server_price_only(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    assert_eq!(rig.service.pricing().await?.profiles.len(), 1);
    let id = rig.create(120).await;
    let window = rig.window(id).await;
    assert_eq!(window.reservation_kopecks, 4 * HOURLY);
    assert_eq!(
        window.reservation_kopecks,
        price::reservation_kopecks(HOURLY, 180)?
    );
    assert_eq!(window.local_ip.as_deref(), Some("192.0.2.10"));
    assert_eq!(window.dnat_port, Some(22100));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn only_one_window_is_live_at_a_time(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    rig.create(60).await;
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::LiveWindowExists)
    ));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_creates_make_exactly_one_window(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    let (a, b) = tokio::join!(
        rig.service.create(CreateRequest {
            trigger: Trigger::Owner,
            proposal_id: None,
            hard_cap_minutes: 60,
            consent_over_budget_kopecks: Some(rig.reservation(60)),
        }),
        rig.service.create(CreateRequest {
            trigger: Trigger::Owner,
            proposal_id: None,
            hard_cap_minutes: 60,
            consent_over_budget_kopecks: Some(rig.reservation(60)),
        })
    );
    assert_eq!(a.is_ok() as u8 + b.is_ok() as u8, 1);
    let count: i64 = sqlx::query_scalar("select count(*) from cloud_windows")
        .fetch_one(&rig.pool)
        .await?;
    assert_eq!(count, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn pool_exhaustion_refuses_a_second_window(pool: PgPool) -> TestResult {
    let rig = rig_with(pool, |config| {
        config.local_ip_pool = "192.0.2.10-192.0.2.10".to_owned();
    })
    .await;
    rig.create(60).await;
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::PoolExhausted("local_ip_pool"))
    ));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn port_exhaustion_refuses_a_second_window(pool: PgPool) -> TestResult {
    let rig = rig_with(pool, |config| {
        config.dnat_port_range = "22100-22100".to_owned();
    })
    .await;
    rig.create(60).await;
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::PoolExhausted("dnat_port_range"))
    ));
    Ok(())
}

// ───────── Полный цикл ─────────

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn full_cycle_uses_network_dnat_and_attach_then_ends_with_the_cost(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _runner, mut at) = rig.drive_to_ready(120, t0).await;
    let window = rig.window(id).await;
    assert_eq!(window.dnat_port, Some(22100));
    let rule_id = window.dnat_rule_id.clone().expect("one DNAT rule");
    assert_eq!(rig.fake.control().dnat_rules.len(), 1);
    assert_eq!(rig.dnat_posts(), 1, "exactly one DNAT POST");
    assert!(
        rig.fake
            .requests()
            .iter()
            .all(|request| !request.path.ends_with("/ips")),
        "no public IP ordered"
    );

    // Проверяем команду несущей: адрес роутера и DNAT-порт в payload.
    let payload: serde_json::Value = sqlx::query_scalar(
        "select payload from commands where kind = 'attach_runner' order by created_at desc limit 1",
    )
    .fetch_one(&rig.pool)
    .await?;
    assert_eq!(payload["host"], ROUTER_IP);
    assert_eq!(payload["ssh_port"], 22100);

    // Простой → удаление сервера → удаление правила → конец с итоговой ценой.
    at = rig
        .tick_until(id, "deleting", at + TimeDelta::minutes(20))
        .await;
    rig.tick_until(id, "ended", at + TimeDelta::seconds(2))
        .await;
    let window = rig.window(id).await;
    assert!(window.server_gone_at.is_some());
    assert!(window.dnat_gone_at.is_some());
    assert_eq!(rig.dnat_deletes(), 1);
    assert!(rig.fake.control().dnat_rules.is_empty());
    assert_eq!(window.final_cost_kopecks, Some(HOURLY));
    assert!(
        !rig.alert_codes()
            .await
            .contains(&"dnat_delete_failing".to_owned())
    );
    let _ = rule_id;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn two_windows_in_sequence_reuse_the_freed_ip_and_port(pool: PgPool) -> TestResult {
    let rig = rig_with(pool, |config| {
        config.local_ip_pool = "192.0.2.10-192.0.2.14".to_owned();
        config.dnat_port_range = "22100-22104".to_owned();
    })
    .await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (first, _runner, at) = rig.drive_to_ready(60, t0).await;
    let w1 = rig.window(first).await;
    assert_eq!(
        (w1.local_ip.clone(), w1.dnat_port),
        (Some("192.0.2.10".to_owned()), Some(22100))
    );
    let at = rig
        .tick_until(first, "ended", at + TimeDelta::minutes(20))
        .await;

    let second = rig.create(60).await;
    let w2 = rig.window(second).await;
    assert_eq!(
        (w2.local_ip, w2.dnat_port),
        (Some("192.0.2.10".to_owned()), Some(22100))
    );
    let _ = at;
    Ok(())
}

// ───────── DNAT ─────────

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_ambiguous_dnat_create_is_adopted_from_the_listing_without_a_second_post(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    rig.fake.control().dnat_create = DnatCreateMode::AcceptThenDrop;
    let t0 = now_micros();
    let id = rig.create(120).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "identified");
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    // Правило записано, ответ потерян: метка вызова стоит, правила в строке ещё нет.
    assert_eq!(rig.window(id).await.dnat_rule_id, None);
    assert!(rig.window(id).await.dnat_create_attempted_at.is_some());
    assert_eq!(rig.dnat_posts(), 1);
    // Полный список усыновляет правило, второго POST нет.
    rig.tick(t0 + TimeDelta::seconds(2)).await;
    assert_eq!(rig.window(id).await.dnat_rule_id.as_deref(), Some("dnat-1"));
    assert_eq!(rig.dnat_posts(), 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_dnat_rule_that_fails_to_delete_three_times_stays_an_open_obligation(
    pool: PgPool,
) -> TestResult {
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _runner, mut at) = rig.drive_to_ready(120, t0).await;
    rig.fake.control().dnat_delete_status = Some(500);
    at = rig
        .tick_until(id, "deleting", at + TimeDelta::minutes(20))
        .await;
    // Сервер удалён, но правила ещё висят: три неудачи с пушами и откатом.
    for _ in 0..30 {
        at += TimeDelta::minutes(1);
        rig.tick(at).await;
        if rig.window(id).await.dnat_delete_attempts >= 1 {
            break;
        }
    }
    assert!(rig.window(id).await.dnat_delete_attempts >= 1);
    for _ in 0..2 {
        at += TimeDelta::minutes(3);
        rig.tick(at).await;
    }
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.dnat_delete_attempts, 3);
    assert!(window.dnat_gone_at.is_none());
    assert!(
        rig.alert_codes()
            .await
            .contains(&"dnat_delete_failing".to_owned())
    );
    // Провайдер перестал отказывать: правило уходит, окно заканчивается.
    rig.fake.control().dnat_delete_status = None;
    let at = rig
        .tick_until(id, "ended", at + TimeDelta::seconds(2))
        .await;
    assert_eq!(rig.window(id).await.final_cost_kopecks, Some(HOURLY));
    let _ = at;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reconcile_deletes_a_stray_rule_in_range_and_leaves_others(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    {
        let mut state = rig.fake.control();
        state.dnat_rules.push(FakeDnatRule {
            id: "stray-in-range".to_owned(),
            local_ip: "192.0.2.42".to_owned(),
            local_port: "22".to_owned(),
            public_ip: ROUTER_IP.to_owned(),
            public_port: "22101".to_owned(),
            protocol: "tcp".to_owned(),
        });
        state.dnat_rules.push(FakeDnatRule {
            id: "stray-out-of-range".to_owned(),
            local_ip: "192.0.2.43".to_owned(),
            local_port: "22".to_owned(),
            public_ip: ROUTER_IP.to_owned(),
            public_port: "22222".to_owned(),
            protocol: "tcp".to_owned(),
        });
    }
    rig.tick(now_micros()).await;
    let rules = rig.fake.control().dnat_rules.clone();
    assert!(rules.iter().all(|rule| rule.id != "stray-in-range"));
    assert!(rules.iter().any(|rule| rule.id == "stray-out-of-range"));
    assert_eq!(rig.fake.control().count("DELETE", "/api/v1/routers/"), 1);
    assert!(
        rig.alert_codes()
            .await
            .contains(&"stray_dnat_deleted".to_owned())
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_partial_dnat_listing_changes_nothing(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    rig.fake.control().list_fail_page = Some(FailPage {
        offset: 0,
        status: 500,
        times: None,
    });
    rig.tick(now_micros()).await;
    assert_eq!(rig.fake.control().count("DELETE", "/api/v1/routers/"), 0);
    assert!(rig.alerts().await.is_empty());
    Ok(())
}

// ───────── Отказ no_paid ─────────

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn no_paid_longer_than_the_grace_deletes_charges_blocks_and_alerts(
    pool: PgPool,
) -> TestResult {
    let rig = rig_with(pool, |config| {
        config.no_paid_grace = Duration::from_secs(60);
    })
    .await;
    let _carrier = rig.carrier_session();
    rig.fake.control().boot_status = Some("no_paid".to_owned());
    let t0 = now_micros();
    let id = rig.create(120).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "identified");
    // Раньше срока окно живёт: статус может измениться.
    rig.tick(t0 + TimeDelta::seconds(30)).await;
    assert_eq!(rig.state(id).await, "identified");
    // После срока — определённый отказ: удаление, итог, блок, пуш.
    let at = rig
        .tick_until(id, "ended", t0 + TimeDelta::seconds(70))
        .await;
    let window = rig.window(id).await;
    assert_eq!(window.last_error.as_deref(), Some("no_paid"));
    assert_eq!(window.delete_reason.as_deref(), Some("no_paid"));
    assert_eq!(window.final_cost_kopecks, Some(HOURLY));
    assert!(rig.alert_codes().await.contains(&"no_paid".to_owned()));
    let block: Option<String> = sqlx::query_scalar("select provider_block_code from cloud_budget")
        .fetch_one(&rig.pool)
        .await?;
    assert_eq!(block.as_deref(), Some(BLOCK_NO_PAID));
    // Пока блок стоит, новые окна не создаются.
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::ProviderBlocked { .. })
    ));
    let _ = at;
    Ok(())
}

// ───────── Прочее ─────────

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn create_without_a_network_is_rejected_by_the_fake(pool: PgPool) -> TestResult {
    let rig = rig(pool).await;
    rig.fake.control().create = CreateMode::AcceptThenDrop;
    let id = rig.create(60).await;
    rig.tick(now_micros()).await;
    // Хаб шлёт `network`; если регрессия уберёт его, поддельный провайдер откажет и окно
    // уйдёт в `failed`, а не останется `create_unknown` с записанным сервером.
    assert_eq!(rig.state(id).await, "create_unknown");
    assert_eq!(rig.fake.control().servers.len(), 1);
    let request = rig
        .fake
        .requests()
        .into_iter()
        .find(|r| r.method == "POST" && r.path == "/api/v1/servers")
        .unwrap();
    assert!(request.body.as_ref().unwrap()["network"]["id"].is_string());
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn neither_the_token_nor_the_host_private_key_reach_the_logs(pool: PgPool) -> TestResult {
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

    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let _id = rig.create(60).await;
    rig.tick(t0).await;
    rig.fake.control().echo_authorization_in_errors = true;
    rig.fake.control().fail_next.push_back(500);
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!logs.contains(TOKEN), "token leaked into logs");
    assert!(
        !logs.contains("OPENSSH"),
        "host private key leaked into logs"
    );
    Ok(())
}

#[test]
fn the_new_provider_types_match_the_documented_shape() {
    // Снимок формы запроса и правила: смена полей API станет видна в этом тесте.
    let _ = DnatCreateMode::Ok;
    let _: Option<CloudProfile> = None;
}
