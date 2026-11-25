//! Облачные окна в хабе: транзакция создания и деньги, жизнь окна от создания до конца
//! оплаты, снос машины и диска, сверка каталогов и строка раннера `cloud`. Против поддельного
//! Яндекса: счётчики его запросов доказывают, что второго создания не бывает.
//!
//! Время идёт параметром прохода службы (`tick(now)`): сроки проверяются без ожидания.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use canterlot_hub::cloud::rate_limit::RateLimit;
use canterlot_hub::cloud::service::CloudService;
use canterlot_hub::cloud::windows::{
    self, CreateRefusal, CreateRequest, Prepare, Trigger, Window, WindowId,
};
use canterlot_hub::cloud::{YandexClient, YandexConfig, price};
use canterlot_hub::config::{CloudConfig, CloudProfile};
use canterlot_hub::inplace_jobs::place;
use canterlot_hub::machines::{self, RunnerRefusal};
use canterlot_hub::session::Registry;
use canterlot_hub::testkit::{self, FakeSession};
use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use serde_json::json;
use sqlx::PgPool;
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

use crate::support::fake_yandex::{
    CreateMode, FOLDER, FailPage, FakeYandex, ResponseGate, SKU_PREEMPTIBLE_VCPU, SUBNET, ZONE,
    street_price, test_key, write_key_file,
};

const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAMPLE0000";
/// Прерываемая машина 32 vCPU, 64 ГБ по ценам поддельного каталога:
/// 32×0.34 + 64×0.083 + 0.26352 = 16.45552 ₽ → 1646 копеек в час.
const COMPUTE: i64 = 1646;
/// Диск 80 ГБ: 80×0.0199 = 1.592 ₽ → 160 копеек в час.
const DISK: i64 = 160;
/// Обе ставки вместе: цена часа работающей машины.
const HOURLY: i64 = COMPUTE + DISK;
/// Другой каталог: окна прежней конфигурации и чужие машины.
const OTHER_FOLDER: &str = "b1gotherfolder00000";

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// «Сейчас» с точностью до микросекунд, как хранит база: метки из проходов сравниваются с
/// прочитанными из строк без округления.
fn now_micros() -> DateTime<Utc> {
    Utc.timestamp_micros(Utc::now().timestamp_micros())
        .single()
        .unwrap()
}

/// Расход по обеим ставкам за одно и то же время: машина и диск пропали вместе.
fn cost(from: DateTime<Utc>, to: DateTime<Utc>) -> i64 {
    price::ledger_kopecks(COMPUTE, DISK, from, to, to).unwrap()
}

struct Rig {
    pool: PgPool,
    fake: FakeYandex,
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
provider = "yandex"
key_file = "key.json"
folder_id = "{FOLDER}"
zone = "{ZONE}"
subnet_id = "{SUBNET}"
runner_public_key = "{KEY}"
carrier_machine_id = "{carrier}"

[[profile]]
cores = 32
memory_gb = 64
disk_gb = 80
"#
    ))
    .unwrap()
}

/// Клиент Яндекса, который смотрит на поддельный сервер. Каждый вызов даёт новый клиент со
/// своим кэшем цен, как после перезапуска хаба.
fn provider(fake: &FakeYandex, dir: &tempfile::TempDir) -> Arc<YandexClient> {
    let mut config = YandexConfig::new(dir.path().join("yandex-key.json"), FOLDER, ZONE, SUBNET);
    config.iam_url = fake.base_url.clone();
    config.compute_url = fake.base_url.clone();
    config.operation_url = fake.base_url.clone();
    config.billing_url = fake.base_url.clone();
    config.request_timeout = Duration::from_secs(3);
    config.rate_limit = RateLimit {
        max: 10_000,
        per: Duration::from_secs(1),
    };
    config.max_rate_limit_retries = 1;
    config.default_backoff = Duration::from_millis(5);
    config.max_backoff = Duration::from_millis(20);
    Arc::new(YandexClient::new(config).unwrap())
}

fn provider_for(rig: &Rig) -> Arc<YandexClient> {
    provider(&rig.fake, &rig._dir)
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
    let fake = FakeYandex::start().await;
    // Денежные пуши владельцу иначе не попадают в `owner_pushes`.
    crate::auth_pushes::enable(&pool).await;
    let dir = tempfile::tempdir().unwrap();
    write_key_file(dir.path());
    let mut config = cloud_config(carrier.machine_id);
    tweak(&mut config);
    let registry = Registry::new(Default::default());
    let service = CloudService::new(
        pool.clone(),
        registry.clone(),
        provider(&fake, &dir),
        config.clone(),
    );
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
        self.try_create_with(cap, None).await
    }

    async fn try_create_with(
        &self,
        cap: u32,
        prepare: Option<windows::Prepare>,
    ) -> Result<WindowId, CreateRefusal> {
        self.service
            .create(CreateRequest {
                trigger: Trigger::Owner,
                proposal_id: None,
                replaces_window_id: None,
                hard_cap_minutes: cap,
                consent_over_budget_kopecks: Some(self.reservation(cap)),
                prepare,
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

    /// Служба после перезапуска хаба: та же база, новый клиент, пустая память.
    fn restarted(&self) -> Arc<CloudService> {
        self.restarted_with(self.config.clone())
    }

    fn restarted_with(&self, config: CloudConfig) -> Arc<CloudService> {
        CloudService::new(
            self.pool.clone(),
            self.registry.clone(),
            provider_for(self),
            config,
        )
    }

    /// Вызовы создания машины.
    fn posts(&self) -> usize {
        self.fake
            .requests()
            .iter()
            .filter(|r| r.method == "POST" && r.path == "/compute/v1/instances")
            .count()
    }

    /// Вызовы удаления машины.
    fn deletes(&self) -> usize {
        self.fake.control().count("DELETE", "/compute/v1/instances")
    }

    /// Вызовы удаления диска.
    fn disk_deletes(&self) -> usize {
        self.fake.control().count("DELETE", "/compute/v1/disks")
    }

    /// Все машины пропали у провайдера, вместе с дисками.
    fn vanish_instances(&self) {
        let mut state = self.fake.control();
        let ids: Vec<String> = state.instances.keys().cloned().collect();
        for id in ids {
            state.remove_instance(&id);
        }
    }

    /// Машина с именем окна у провайдера, созданная мимо хаба: сразу `RUNNING`, со своим
    /// диском того же имени.
    fn add_instance(&self, name: &str) -> String {
        self.fake.control().add_instance(name)
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
        // Операция создания закончилась, машина работает: уходит команда подключения.
        self.tick(t0 + TimeDelta::seconds(1)).await;
        assert!(self.window(id).await.attach_sent_at.is_some());
        let session = self.attach().await;
        let at = t0 + TimeDelta::seconds(2);
        self.tick(at).await;
        assert_eq!(self.state(id).await, "ready");
        (id, session, at)
    }

    /// Дойти до `ready` из `identified`: команда подключения, токен, воркер на связи.
    async fn attach_when_listed(&self, t0: DateTime<Utc>) -> FakeSession {
        self.tick(t0 + TimeDelta::seconds(1)).await;
        let session = self.attach().await;
        self.tick(t0 + TimeDelta::seconds(2)).await;
        session
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

    async fn attach_command(&self) -> serde_json::Value {
        sqlx::query_scalar(
            "select payload from commands where kind = 'attach_runner' and machine_id = $1",
        )
        .bind(self.carrier)
        .fetch_one(&self.pool)
        .await
        .unwrap()
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

    /// Идущая на облачной машине задача с готовым исходником: после прерывания ей положен
    /// повтор. `retries` — сколько повторов она уже пережила (0 или 1).
    async fn seed_running_sourced(&self, machine: Uuid, retries: i32) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into jobs (id, owner_id, domain, machine_id, remote, workdir, shell,
                               timeout_seconds, state, labels, carrier_id, preemption_retries,
                               source_repo, source_rev, source_domain)
             values ($1, $2, 'personal', $3, true, '/srv/canterlot-runner/jobs/personal/x',
                     'true', 600, 'running', '{cloud}', $4, $5, '/srv/src/app',
                     '0123456789abcdef0123456789abcdef01234567', 'personal')",
        )
        .bind(id)
        .bind(self.owner)
        .bind(machine)
        .bind(self.carrier)
        .bind(retries)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    /// Состояние, попытка и число уже пережитых прерываний задачи.
    async fn job_triple(&self, id: Uuid) -> (String, i32, i32) {
        sqlx::query_as("select state, attempt, preemption_retries from jobs where id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// Окна, которые заменили это окно (их не больше одного).
    async fn replacements_of(&self, parent: Uuid) -> Vec<Window> {
        sqlx::query_as(
            "select * from cloud_windows where replaces_window_id = $1 order by created_at, id",
        )
        .bind(parent)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    /// Причина последнего пуша о пропущенной замене.
    async fn last_skip_reason(&self) -> String {
        self.alerts()
            .await
            .last()
            .and_then(|payload| payload["reason"].as_str().map(str::to_owned))
            .expect("a replacement_skipped push")
    }

    /// Месячный бюджет: без него замена не проходит допуск, а создание идёт по согласию.
    async fn set_budget(&self, kopecks: i64) {
        sqlx::query("update cloud_budget set monthly_budget_kopecks = $1")
            .bind(kopecks)
            .execute(&self.pool)
            .await
            .unwrap();
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

const CLOUD_HEAD: &str = "[runners.cloud]\nprovider = 'yandex'\nkey_file = 'yc-key.json'\n\
    folder_id = 'b1gfakefolder000000'\nzone = 'ru-central1-a'\n\
    subnet_id = 'e9bfakesubnet000000'\n\
    runner_public_key = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAMPLE0000'\n\
    carrier_machine_id = '018f0000-0000-7000-8000-000000000001'\n";

const CLOUD_PROFILE: &str = "[[runners.cloud.profile]]\ncores = 32\nmemory_gb = 64\ndisk_gb = 80\n";

/// Таблица с ключами `extra` до списка профилей (после него ключи попали бы в профиль).
fn cloud_table(extra: &str) -> String {
    format!("{CLOUD_HEAD}{extra}{CLOUD_PROFILE}")
}

#[test]
fn cloud_config_is_off_when_absent_and_has_the_documented_defaults() {
    let config = load_config("").unwrap();
    assert!(config.runners.cloud.is_none());
    assert!(!config.runners.cloud_timeweb);

    let config = load_config(&cloud_table("")).unwrap();
    assert!(config.startup_warnings.is_empty());
    let cloud = config.runners.cloud.unwrap();
    assert_eq!(
        cloud.profile,
        vec![CloudProfile {
            cores: 32,
            memory_gb: 64,
            disk_gb: 80
        }]
    );
    assert_eq!(cloud.folder_id, "b1gfakefolder000000");
    assert_eq!(cloud.zone, "ru-central1-a");
    assert_eq!(cloud.subnet_id, "e9bfakesubnet000000");
    assert_eq!(cloud.platform, "standard-v3");
    assert_eq!(cloud.image_family, "debian-13");
    assert!(cloud.preemptible, "preemptible machines by default");
    assert_eq!(cloud.disk_type, "network-ssd");
    assert_eq!(cloud.labels, vec!["cloud", "kernel-6.12"]);
    assert_eq!(cloud.ready_timeout, Duration::from_secs(15 * 60));
    assert_eq!(cloud.idle_minutes, 15);
    assert_eq!(cloud.drain_grace, Duration::from_secs(30 * 60));
    assert_eq!(cloud.teardown_margin, Duration::from_secs(30 * 60));
    assert_eq!(cloud.reconcile_interval, Duration::from_secs(5 * 60));
    assert_eq!(cloud.create_unknown_window, Duration::from_secs(10 * 60));
    // Путь к файлу ключа считается от каталога конфигурации.
    assert!(cloud.key_file.is_absolute() && cloud.key_file.ends_with("yc-key.json"));
    // Клиент получает те же место и вид машины.
    let client = cloud.yandex();
    assert_eq!(
        (client.folder_id.as_str(), client.zone.as_str()),
        ("b1gfakefolder000000", "ru-central1-a")
    );
    assert_eq!(client.subnet_id, "e9bfakesubnet000000");
    assert_eq!(client.validate(), Ok(()));
    // `Debug` не показывает ни путь к ключу, ни ключ входа.
    let shown = format!("{cloud:?}");
    assert!(!shown.contains("yc-key") && !shown.contains("AAAAC3"));

    let regular = load_config(&cloud_table("preemptible = false\n")).unwrap();
    assert!(!regular.runners.cloud.unwrap().preemptible);
}

#[test]
fn cloud_config_rejects_bad_values_with_a_code_that_has_no_values() {
    let table = cloud_table("");
    let nine = (0..9).fold(cloud_table(""), |all, _| format!("{all}{CLOUD_PROFILE}"));
    let bad = [
        table.replace("'yandex'", "'aws'"),
        // Ядра: ноль, нечётное число, больше предела.
        table.replace("cores = 32", "cores = 0"),
        table.replace("cores = 32", "cores = 3"),
        table.replace("cores = 32", "cores = 82"),
        table.replace("memory_gb = 64", "memory_gb = 0"),
        table.replace("disk_gb = 80", "disk_gb = 0"),
        table.replace("folder_id = 'b1gfakefolder000000'", "folder_id = ''"),
        table.replace("folder_id = 'b1gfakefolder000000'", "folder_id = 'a/b'"),
        table.replace("zone = 'ru-central1-a'\n", ""),
        table.replace("subnet_id = 'e9bfakesubnet000000'", "subnet_id = ' '"),
        table.replace("ssh-ed25519 AAAAC3", "ssh-ed25519 not base64 !"),
        // Платформа и тип диска без известных цен.
        cloud_table("platform = 'standard-v2'\n"),
        cloud_table("disk_type = 'network-hdd'\n"),
        cloud_table("image_family = ''\n"),
        cloud_table("labels = ['has space']\n"),
        cloud_table("labels = []\n"),
        cloud_table("max_running = 0\n"),
        cloud_table("drain_grace = '10s'\n"),
        cloud_table("unknown_key = 1\n"),
        // Нет профилей, их больше допустимого, или размеры стоят прямо в таблице.
        CLOUD_HEAD.to_owned(),
        nine,
        cloud_table("cores = 32\n"),
        format!("{table}unknown_profile_key = 1\n"),
    ];
    for table in bad {
        let error = load_config(&table).expect_err(&table);
        assert!(
            !error.contains("AAAAC3") && !error.contains("yc-key"),
            "no values in {error}"
        );
    }
}

#[test]
fn a_timeweb_table_turns_the_cloud_off_and_the_hub_still_loads() {
    use canterlot_hub::config::StartupWarning;
    let yandex = cloud_table("");
    let timeweb_tables = [
        // Прежняя таблица целиком.
        "[runners.cloud]\nprovider = 'timeweb'\ntoken_file = 'tw-token'\nproject_id = 7\n\
         runner_public_key = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLE'\n\
         carrier_machine_id = '018f0000-0000-7000-8000-000000000001'\n\
         network_id = 'network-example'\nnetwork_cidr = '192.0.2.0/24'\n\
         router_id = '018f0000-0000-7000-8000-0000000000aa'\nrouter_ip = '192.0.2.1'\n\
         gateway = '192.0.2.5'\nlocal_ip_pool = '192.0.2.10-192.0.2.60'\n\
         dnat_port_range = '22100-22199'\n\
         [[runners.cloud.profile]]\nconfigurator_id = 151\ncpu = 32\nram_gb = 64\n\
         disk_gb = 80\navailability_zone = 'msk-1'\n"
            .to_owned(),
        // Провайдер назван прямо, остальное уже новое.
        yandex.replace("'yandex'", "'timeweb'"),
        // Новая таблица, в которой остался один ключ прежнего провайдера.
        cloud_table("token_file = 'tw-token'\n"),
        cloud_table("router_id = 'x'\n"),
        cloud_table("no_paid_grace = '3m'\n"),
        // Ключ прежнего провайдера в профиле.
        format!("{yandex}configurator_id = 151\n"),
        format!("{yandex}availability_zone = 'msk-1'\n"),
    ];
    for table in timeweb_tables {
        let config = load_config(&table).unwrap_or_else(|error| panic!("{error}\n{table}"));
        assert!(config.runners.cloud.is_none(), "cloud stays off\n{table}");
        assert!(config.runners.cloud_timeweb);
        assert_eq!(
            config.startup_warnings,
            vec![StartupWarning::CloudTimewebConfig],
            "exactly one error is logged at startup"
        );
    }
    // Обычная негодная таблица Яндекса по-прежнему ошибка загрузки, а не выключенное облако.
    assert!(load_config(&cloud_table("unknown_key = 1\n")).is_err());
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

#[tokio::test]
async fn the_reservation_follows_config_and_the_price_module() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    // Две ставки часа берутся из каталога SKU провайдера.
    let pricing = rig.service.pricing().await?;
    assert!(
        pricing.preemptible,
        "priced for the configured kind of machine"
    );
    assert_eq!(pricing.profiles.len(), 1);
    assert_eq!(pricing.profiles[0].compute_kopecks_per_hour, COMPUTE);
    assert_eq!(pricing.profiles[0].disk_kopecks_per_hour, DISK);
    assert_eq!(pricing.max_hourly_kopecks(), Some(HOURLY));
    // 60 + 30 + 30 = 120 минут: 2 часа и час запаса.
    let id = rig.create(60).await;
    let window = rig.window(id).await;
    assert_eq!(window.reservation_kopecks, 3 * HOURLY);
    assert_eq!(
        window.reservation_kopecks,
        price::reservation_kopecks(HOURLY, 120)?,
        "both rates are reserved together"
    );
    assert_eq!(window.price_hourly_kopecks, HOURLY);
    assert_eq!(
        (
            window.compute_kopecks_per_hour,
            window.disk_kopecks_per_hour
        ),
        (COMPUTE, DISK)
    );
    assert!(window.preemptible);
    assert_eq!(window.folder_id, FOLDER);
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
    windows::fail_in_tx(&mut *rig.pool.acquire().await?, id, "test").await?;

    // Обычная машина дороже прерываемой, и окно запоминает свой вид вместе с ценой.
    let regular = rig_with(rig.pool.clone(), |config| config.preemptible = false).await;
    let id = regular.create(60).await;
    let window = regular.window(id).await;
    assert!(!window.preemptible);
    // 32×1.24 + 64×0.33 + 0.26352 = 61.06352 ₽ → 6107 копеек; диск тот же.
    assert_eq!(
        (
            window.compute_kopecks_per_hour,
            window.disk_kopecks_per_hour
        ),
        (6107, DISK)
    );
    assert_eq!(window.reservation_kopecks, 3 * (6107 + DISK));
    Ok(())
}

#[tokio::test]
async fn with_a_zero_budget_only_the_exact_consent_passes() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let reservation = rig.reservation(60);
    let attempt = |consent: Option<i64>| {
        let service = rig.service.clone();
        async move {
            service
                .create(CreateRequest {
                    trigger: Trigger::Card,
                    proposal_id: Some(Uuid::now_v7()),
                    replaces_window_id: None,
                    hard_cap_minutes: 60,
                    consent_over_budget_kopecks: consent,
                    prepare: None,
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

#[tokio::test]
async fn the_budget_counts_ended_final_cost_and_live_reservations() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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
            replaces_window_id: None,
            hard_cap_minutes: 60,
            consent_over_budget_kopecks: None,
            prepare: None,
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
            replaces_window_id: None,
            hard_cap_minutes: 60,
            consent_over_budget_kopecks: None,
            prepare: None,
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
            replaces_window_id: None,
            hard_cap_minutes: 60,
            consent_over_budget_kopecks: None,
            prepare: None,
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
            replaces_window_id: None,
            hard_cap_minutes: 60,
            consent_over_budget_kopecks: None,
            prepare: None,
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn only_one_window_is_live_at_a_time() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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
    let live_row = "insert into cloud_windows (id, name, state, trigger, folder_id, preemptible,
                                    host_key_public, price_hourly_kopecks, reservation_kopecks,
                                    month, hard_cap_minutes, created_at, profiles)
         select $1, 'canterlot-runner-' || $1::text, 'ready', 'owner', 'b1gfakefolder000000', true,
                'k', 1, 1, '2026-11', 60, now(),
                '[{\"cores\":2,\"memory_gb\":2,\"disk_gb\":10,\"compute_kopecks_per_hour\":1,\"disk_kopecks_per_hour\":0}]'::jsonb";
    // Предыдущий цикл оставил `ended`, живого нет: вставка проходит один раз, второй нет.
    sqlx::query(live_row)
        .bind(Uuid::now_v7())
        .execute(&rig.pool)
        .await
        .expect("the first live row");
    let second = sqlx::query(live_row)
        .bind(Uuid::now_v7())
        .execute(&rig.pool)
        .await;
    assert!(second.is_err(), "the unique index allows one live row");
    Ok(())
}

#[tokio::test]
async fn concurrent_creates_make_exactly_one_window() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    sqlx::query("update cloud_budget set monthly_budget_kopecks = 10000000")
        .execute(&rig.pool)
        .await?;
    let request = || CreateRequest {
        trigger: Trigger::Owner,
        proposal_id: None,
        replaces_window_id: None,
        hard_cap_minutes: 60,
        consent_over_budget_kopecks: None,
        prepare: None,
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

#[tokio::test]
async fn the_private_host_key_is_never_stored() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn the_full_cycle_create_ready_idle_delete_ended_with_the_final_cost() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let id = rig.create(120).await;
    assert_eq!(rig.state(id).await, "create_intent");
    assert!(windows::has_pending_key(id));
    assert_eq!(rig.reservation(120), 4 * HOURLY);

    // Первый проход: сверка (пустой каталог), затем вызов создания.
    rig.tick(t0).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "identified");
    let instance_id = window.instance_id.clone().expect("instance id");
    assert!(window.create_operation_id.is_some());
    assert_eq!(window.create_called_at, Some(t0));
    assert_eq!(window.hard_cap_at, Some(t0 + TimeDelta::minutes(120)));
    assert_eq!(
        (window.cores, window.memory_gb, window.disk_gb),
        (Some(32), Some(64), Some(80)),
        "the profile that was called is in the row"
    );
    assert_eq!(rig.posts(), 1);
    {
        let state = rig.fake.control();
        let instance = state
            .instances
            .get(&instance_id)
            .expect("the fake instance");
        assert_eq!(instance.name, format!("canterlot-runner-{id}"));
        assert_eq!(instance.folder_id, FOLDER);
        assert!(instance.preemptible, "preemptible by default");
        let request = &instance.request;
        assert_eq!(request["folderId"], FOLDER);
        assert_eq!(request["labels"], json!({"canterlot": "runner"}));
        assert_eq!(request["resourcesSpec"]["cores"], "32");
        assert_eq!(
            request["resourcesSpec"]["memory"],
            (64u64 << 30).to_string()
        );
        // Загрузочный диск носит имя окна и удаляется вместе с машиной.
        let disk = &request["bootDiskSpec"];
        assert_eq!(disk["autoDelete"], true);
        assert_eq!(disk["diskSpec"]["name"], format!("canterlot-runner-{id}"));
        assert_eq!(disk["diskSpec"]["size"], (80u64 << 30).to_string());
        assert_eq!(
            request["networkInterfaceSpecs"][0]["primaryV4AddressSpec"]["oneToOneNatSpec"]["ipVersion"],
            "IPV4"
        );
        // Закрытый хостовый ключ ушёл провайдеру в cloud-init, и только туда.
        let user_data = request["metadata"]["user-data"].as_str().unwrap();
        assert!(user_data.contains("BEGIN OPENSSH PRIVATE KEY"));
        assert!(user_data.contains(&window.host_key_public));
        assert!(!user_data.contains("ip route"), "no gateway workaround");
    }
    let dump: String = sqlx::query_scalar("select cloud_windows::text from cloud_windows")
        .fetch_one(&rig.pool)
        .await?;
    assert!(!dump.contains("PRIVATE KEY"));
    assert!(
        !windows::has_pending_key(id),
        "the private half left memory"
    );

    // Операция создания закончилась, машина работает: несущей ушла команда подключения.
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    let window = rig.window(id).await;
    let public_ip = window.public_ip.clone().expect("the public address");
    assert!(window.boot_disk_id.is_some());
    let attach = rig.attach_command().await;
    let machine = rig.cloud_machine().await;
    assert_eq!(attach["window_id"], id.to_string());
    assert_eq!(attach["machine_id"], machine.to_string());
    assert_eq!(attach["host"], public_ip);
    assert_eq!(attach["ssh_port"], 22);
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
    let end = delete_at + TimeDelta::seconds(1);
    rig.tick(end).await;

    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert_eq!(window.delete_confirmed_at, Some(end));
    assert_eq!(
        (window.server_gone_at, window.disk_gone_at),
        (Some(end), Some(end)),
        "both absences are confirmed"
    );
    assert!(window.delete_operation_id.is_some());
    // 16 минут и секунда от вызова создания, по секундам и без минимума в час.
    assert_eq!(window.final_cost_kopecks, Some(cost(t0, end)));
    assert!(window.final_cost_kopecks.unwrap() < HOURLY);
    assert_eq!(rig.fake.instance_count(), 0);
    assert!(rig.fake.control().disks.is_empty());
    assert_eq!((rig.posts(), rig.deletes()), (1, 1));
    assert_eq!(rig.disk_deletes(), 0, "the boot disk went with the machine");
    Ok(())
}

#[tokio::test]
async fn a_rejected_create_fails_the_window_and_releases_the_reservation() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    rig.fake.control().create = CreateMode::Error {
        http: 400,
        code: 3,
        message: "Illegal argument subnetId".into(),
    };
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    // Вызов был: окно не заканчивается сразу, а уходит на снос.
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.delete_reason.as_deref(), Some("create_failed"));
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::LiveWindowExists)
    ));
    // Полные списки машин и дисков каталога без имени окна: платить было не за что.
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!(window.final_cost_kopecks, Some(0));
    assert_eq!(window.last_error.as_deref(), Some("32x64:400/3"));
    assert!(window.server_gone_at.is_some() && window.disk_gone_at.is_some());
    assert_eq!((rig.posts(), rig.deletes()), (1, 0));
    let mut conn = rig.pool.acquire().await?;
    assert_eq!(windows::spent_in_month(&mut conn, &window.month).await?, 0);
    drop(conn);
    // Окно закрыто: можно создать следующее.
    rig.try_create(60).await.expect("the slot is free again");
    Ok(())
}

#[tokio::test]
async fn a_rejected_create_stays_open_until_both_listings_are_complete() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    rig.fake.control().create = CreateMode::Error {
        http: 403,
        code: 7,
        message: "Permission denied".into(),
    };
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "deleting");
    // Список дисков не читается: окно остаётся открытым, сколько бы проходов ни было.
    rig.fake.control().list_fail_page = Some(FailPage {
        list: "disks",
        page: 0,
        status: 500,
        times: None,
    });
    for seconds in [1, 20, 200] {
        rig.tick(t0 + TimeDelta::seconds(seconds)).await;
        let window = rig.window(id).await;
        assert_eq!(window.state, "deleting", "after {seconds} s");
        assert!(window.server_gone_at.is_some() && window.disk_gone_at.is_none());
    }
    rig.fake.control().list_fail_page = None;
    rig.tick(t0 + TimeDelta::seconds(210)).await;
    let window = rig.window(id).await;
    assert_eq!(
        (window.state.as_str(), window.final_cost_kopecks),
        ("failed", Some(0))
    );
    Ok(())
}

// ───────── Список профилей ─────────

/// Профиль 48 vCPU, 96 ГБ, 160 ГБ: 48×0.34 + 96×0.083 + 0.26352 = 24.55152 ₽ → 2456 и диск
/// 3.184 ₽ → 319.
const DEAR: (u32, u32, u32) = (48, 96, 160);
const DEAR_HOURLY: i64 = 2456 + 319;
/// Профиль по умолчанию: 1646 + 160.
const MID: (u32, u32, u32) = (32, 64, 80);
/// 8 vCPU, 16 ГБ, 40 ГБ: 4.31152 ₽ → 432 и диск 0.796 ₽ → 80.
const CHEAP: (u32, u32, u32) = (8, 16, 40);
const CHEAP_HOURLY: i64 = 432 + 80;

fn profiles(list: &[(u32, u32, u32)]) -> impl FnOnce(&mut CloudConfig) + '_ {
    move |config| {
        config.profile = list
            .iter()
            .map(|&(cores, memory_gb, disk_gb)| CloudProfile {
                cores,
                memory_gb,
                disk_gb,
            })
            .collect();
    }
}

/// Число ядер запросов создания в порядке отправки: по нему виден профиль.
fn posted_cores(rig: &Rig) -> Vec<String> {
    rig.fake
        .requests()
        .iter()
        .filter(|r| r.method == "POST" && r.path == "/compute/v1/instances")
        .map(|r| {
            r.body.as_ref().unwrap()["resourcesSpec"]["cores"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn the_reservation_and_the_budget_use_the_most_expensive_profile_of_the_list() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    // Самый дорогой не первый и не последний.
    let rig = rig_with(pool, profiles(&[MID, DEAR, CHEAP])).await;
    let pricing = rig.service.pricing().await?;
    let hourly: Vec<i64> = pricing
        .profiles
        .iter()
        .map(|profile| profile.hourly_kopecks())
        .collect();
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
        replaces_window_id: None,
        hard_cap_minutes: 60,
        consent_over_budget_kopecks: consent,
        prepare: None,
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
    assert_eq!(
        (
            window.compute_kopecks_per_hour,
            window.disk_kopecks_per_hour
        ),
        (2456, 319)
    );
    assert_eq!(window.cores, None, "no profile was called yet");
    let stored: Vec<i64> = window
        .profiles()
        .iter()
        .map(|profile| profile.hourly_kopecks())
        .collect();
    assert_eq!(stored, hourly, "the snapshot keeps every price in order");
    Ok(())
}

#[tokio::test]
async fn a_price_that_cannot_be_read_refuses_the_window() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    // В каталоге нет SKU диска: цена не угадывается, окно не создаётся.
    rig.fake
        .control()
        .remove_sku(crate::support::fake_yandex::SKU_SSD);
    let fresh = rig.restarted();
    assert!(matches!(
        fresh.pricing().await,
        Err(CreateRefusal::Provider(_))
    ));
    let refused = fresh
        .create(CreateRequest {
            trigger: Trigger::Owner,
            proposal_id: None,
            replaces_window_id: None,
            hard_cap_minutes: 60,
            consent_over_budget_kopecks: Some(rig.reservation(60)),
            prepare: None,
        })
        .await;
    assert!(matches!(refused, Err(CreateRefusal::Provider(_))));
    let rows: i64 = sqlx::query_scalar("select count(*) from cloud_windows")
        .fetch_one(&rig.pool)
        .await?;
    assert_eq!(rows, 0, "no window without a price");
    Ok(())
}

#[tokio::test]
async fn a_definite_refusal_moves_to_the_next_profile_in_the_same_window_and_the_ledger_uses_the_real_price()
-> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig_with(pool, profiles(&[DEAR, MID, CHEAP])).await;
    let _carrier = rig.carrier_session();
    rig.fake
        .control()
        .create_by_cores
        .insert("48".to_owned(), CreateMode::quota());
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
        posted_cores(&rig),
        vec!["48", "32"],
        "in list order, stops at the first success"
    );
    assert_eq!(window.cores, Some(32));
    assert_eq!(
        (
            window.compute_kopecks_per_hour,
            window.disk_kopecks_per_hour
        ),
        (COMPUTE, DISK),
        "the rates of the profile that succeeded"
    );
    assert_eq!(window.price_hourly_kopecks, HOURLY);
    assert_eq!(
        window.reservation_kopecks, reserved,
        "the reservation is unchanged"
    );
    {
        let state = rig.fake.control();
        let instance = state
            .instances
            .get(window.instance_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(instance.cores, "32");
    }
    // Одно окно: создать второе нельзя, пока идёт это.
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::LiveWindowExists)
    ));

    // Конец окна: итог по действительным ставкам, а не по цене резерва.
    let _session = rig.attach_when_listed(t0).await;
    windows::request_delete(
        &rig.pool,
        id,
        windows::REASON_OWNER,
        t0 + TimeDelta::minutes(10),
    )
    .await?;
    let end = t0 + TimeDelta::minutes(10) + TimeDelta::seconds(1);
    rig.tick(end).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert_eq!(window.final_cost_kopecks, Some(cost(t0, end)));
    assert!(window.final_cost_kopecks.unwrap() < reserved);
    Ok(())
}

#[tokio::test]
async fn a_quota_or_capacity_refusal_falls_back_and_any_other_one_closes_the_window() -> TestResult
{
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig_with(pool, profiles(&[DEAR, CHEAP])).await;
    let mut t = now_micros();
    for mode in [CreateMode::quota(), CreateMode::not_enough_resources()] {
        rig.fake
            .control()
            .create_by_cores
            .insert("48".to_owned(), mode.clone());
        let id = rig.create(60).await;
        t += TimeDelta::minutes(1);
        rig.tick(t).await;
        assert_eq!(rig.state(id).await, "identified", "{mode:?}");
        assert_eq!(rig.window(id).await.cores, Some(8));
        windows::request_delete(&rig.pool, id, windows::REASON_OWNER, t).await?;
        t += TimeDelta::seconds(1);
        rig.tick(t).await;
        assert_eq!(rig.state(id).await, "ended");
    }
    // Отказ не про квоту и не про место: следующий профиль не пробуется, окно закрыто с
    // кодом.
    let before = rig.posts();
    rig.fake.control().create_by_cores.insert(
        "48".to_owned(),
        CreateMode::Error {
            http: 403,
            code: 7,
            message: "Permission denied".into(),
        },
    );
    let id = rig.create(60).await;
    t += TimeDelta::minutes(1);
    rig.tick(t).await;
    rig.tick(t + TimeDelta::seconds(1)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!(window.final_cost_kopecks, Some(0));
    assert_eq!(window.last_error.as_deref(), Some("48x96:403/7"));
    assert_eq!(rig.posts(), before + 1, "the cheap profile was not tried");
    Ok(())
}

#[tokio::test]
async fn when_every_profile_is_refused_the_window_fails_with_the_codes_and_a_zero_cost()
-> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig_with(pool, profiles(&[DEAR, MID, CHEAP])).await;
    {
        let mut state = rig.fake.control();
        state
            .create_by_cores
            .insert("48".to_owned(), CreateMode::quota());
        state
            .create_by_cores
            .insert("32".to_owned(), CreateMode::not_enough_resources());
        state
            .create_by_cores
            .insert("8".to_owned(), CreateMode::quota());
    }
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!(window.final_cost_kopecks, Some(0));
    assert_eq!(
        window.last_error.as_deref(),
        Some("all_profiles_rejected: 48x96:429/8,32x64:400/9,8x16:429/8")
    );
    assert_eq!(posted_cores(&rig), vec!["48", "32", "8"]);
    assert_eq!(rig.fake.instance_count(), 0);
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

#[tokio::test]
async fn an_ambiguous_outcome_stops_the_fallback_and_the_attempted_profile_is_billed() -> TestResult
{
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig_with(pool, profiles(&[DEAR, MID])).await;
    // Первый профиль принят провайдером, ответ потерян по дороге.
    rig.fake.control().create = CreateMode::AcceptThenDrop;
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "create_unknown");
    assert_eq!(
        posted_cores(&rig),
        vec!["48"],
        "no other profile after an ambiguous answer"
    );
    assert_eq!(window.cores, Some(48));
    assert_eq!(window.price_hourly_kopecks, DEAR_HOURLY);
    // Пока машина не найдена, оценка расхода уже идёт по ставкам профиля, которому звонили.
    let view = windows::view(&window, t0 + TimeDelta::hours(1));
    assert_eq!(view.cost_so_far_kopecks, DEAR_HOURLY);
    // Позже машина находится по имени: ledger считает цену профиля, который мог создаться.
    rig.tick(t0 + TimeDelta::minutes(6)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "identified");
    assert_eq!(
        window.create_operation_id, None,
        "found by name: there is no operation to poll"
    );
    assert_eq!(posted_cores(&rig), vec!["48"]);
    assert_eq!(rig.fake.instance_count(), 1);
    assert_eq!(window.price_hourly_kopecks, DEAR_HOURLY);

    // 503 на первом профиле тоже неопределён: второй не пробуется.
    let rig2 = rig_with(rig.pool.clone(), profiles(&[DEAR, MID])).await;
    sqlx::query("update cloud_windows set state = 'ended', final_cost_kopecks = 0")
        .execute(&rig.pool)
        .await?;
    rig2.fake
        .control()
        .create_by_cores
        .insert("48".to_owned(), CreateMode::unavailable());
    let id = rig2.create(60).await;
    rig2.tick(t0).await;
    assert_eq!(rig2.state(id).await, "create_unknown");
    assert_eq!(posted_cores(&rig2), vec!["48"]);

    // Занятое имя (409) значит, что машина с ним есть: это тоже не отказ.
    let rig3 = rig_with(rig.pool.clone(), profiles(&[DEAR, MID])).await;
    sqlx::query("update cloud_windows set state = 'ended', final_cost_kopecks = 0")
        .execute(&rig.pool)
        .await?;
    rig3.fake.control().create = CreateMode::name_exists();
    let id = rig3.create(60).await;
    rig3.tick(t0).await;
    assert_eq!(rig3.state(id).await, "create_unknown");
    assert_eq!(posted_cores(&rig3), vec!["48"]);
    Ok(())
}

// ───────── Операция создания и подключение ─────────

#[tokio::test]
async fn attach_goes_to_the_public_address_of_the_instance_on_port_22() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let id = rig.create(120).await;
    rig.tick(t0).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "identified");
    assert_eq!(
        (window.public_ip.clone(), window.attach_sent_at),
        (None, None)
    );

    rig.tick(t0 + TimeDelta::seconds(1)).await;
    let window = rig.window(id).await;
    let instance_id = window.instance_id.clone().unwrap();
    let (address, disk_id) = {
        let state = rig.fake.control();
        let instance = &state.instances[&instance_id];
        (instance.public_ip().unwrap(), instance.disk_id.clone())
    };
    assert_eq!(window.public_ip.as_deref(), Some(address.as_str()));
    assert_eq!(window.boot_disk_id.as_deref(), Some(disk_id.as_str()));
    assert_eq!(window.attach_sent_at, Some(t0 + TimeDelta::seconds(1)));
    let attach = rig.attach_command().await;
    assert_eq!(attach["host"], address);
    assert_eq!(attach["ssh_port"], 22);
    assert!(
        attach.get("prepare_script").is_none(),
        "a window without preparation carries no script"
    );
    let view = windows::view(&window, t0 + TimeDelta::seconds(1));
    assert_eq!(view.public_ip.as_deref(), Some(address.as_str()));
    assert_eq!(view.instance_id.as_deref(), Some(instance_id.as_str()));

    // Сколько бы проходов ни было, команда подключения одна.
    for seconds in [2, 3, 30] {
        rig.tick(t0 + TimeDelta::seconds(seconds)).await;
    }
    let sent: i64 =
        sqlx::query_scalar("select count(*) from commands where kind = 'attach_runner'")
            .fetch_one(&rig.pool)
            .await?;
    assert_eq!(sent, 1);
    Ok(())
}

#[tokio::test]
async fn attach_carries_the_prepare_script() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let script = "apt-get install -y jq\njq --version\n";
    let id = rig
        .try_create_with(
            120,
            Some(Prepare {
                packages: Vec::new(),
                script: Some(script.to_owned()),
            }),
        )
        .await
        .expect("window created");
    rig.tick(t0).await;
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    assert_eq!(rig.state(id).await, "identified");
    let attach = rig.attach_command().await;
    assert_eq!(attach["prepare_script"], script);
    Ok(())
}

#[tokio::test]
async fn attach_waits_for_the_create_operation_and_a_running_instance() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    // Машина ещё грузится: и операция создания, и сама машина отвечают «не готово».
    rig.fake.control().boot_polls = 3;
    let t0 = now_micros();
    let id = rig.create(120).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "identified");
    for seconds in [1, 2] {
        rig.tick(t0 + TimeDelta::seconds(seconds)).await;
        let window = rig.window(id).await;
        assert!(window.attach_sent_at.is_none(), "still provisioning");
        assert!(window.public_ip.is_none());
    }
    rig.tick(t0 + TimeDelta::seconds(3)).await;
    let window = rig.window(id).await;
    assert!(window.attach_sent_at.is_some());
    assert!(window.public_ip.is_some());
    Ok(())
}

#[tokio::test]
async fn an_instance_found_by_name_is_checked_directly_without_an_operation() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    rig.fake.control().create = CreateMode::AcceptThenDrop;
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "create_unknown");
    rig.tick(t0 + TimeDelta::minutes(5)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "identified");
    assert_eq!(window.create_operation_id, None);
    // Операции нет: машина проверяется прямым ответом и подключается.
    rig.tick(t0 + TimeDelta::minutes(5) + TimeDelta::seconds(1))
        .await;
    assert!(rig.window(id).await.attach_sent_at.is_some());
    assert_eq!(rig.fake.control().count("GET", "/operations/"), 0);

    // Провайдер не знает операцию из строки окна (404): путь тот же.
    let rig2 = rig_with(rig.pool.clone(), |_| {}).await;
    sqlx::query("update cloud_windows set state = 'ended', final_cost_kopecks = 0")
        .execute(&rig.pool)
        .await?;
    let _carrier2 = rig2.carrier_session();
    let id = rig2.create(60).await;
    rig2.tick(t0).await;
    sqlx::query(
        "update cloud_windows set create_operation_id = 'fhmforgottenop0001' where id = $1",
    )
    .bind(id)
    .execute(&rig.pool)
    .await?;
    rig2.tick(t0 + TimeDelta::seconds(1)).await;
    assert!(rig2.window(id).await.attach_sent_at.is_some());
    assert_eq!(rig2.fake.control().count("GET", "/operations/"), 1);
    Ok(())
}

#[tokio::test]
async fn a_failed_create_operation_cleans_up_the_instance_and_the_disk() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    {
        let mut state = rig.fake.control();
        // Создание принято, а операция потом кончается ошибкой; отката не случилось, и
        // Яндекс сначала два опроса «откатывает».
        state.fail_next_create_operation = Some((8, "Quota limit exceeded".to_owned()));
        state.failed_create_keeps_instance = true;
        state.failed_create_rollback_polls = 2;
    }
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "identified");
    let instance_id = window.instance_id.clone().unwrap();
    // Диск переживёт машину: автоматическое удаление не сработает.
    rig.fake.control().leave_disk(&instance_id);

    // Пока `done: false`, ошибка в операции ещё не окончательна.
    for seconds in [1, 2] {
        rig.tick(t0 + TimeDelta::seconds(seconds)).await;
        assert_eq!(rig.state(id).await, "identified", "the rollback is running");
    }
    rig.tick(t0 + TimeDelta::seconds(3)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.delete_reason.as_deref(), Some("create_failed"));
    assert_eq!(window.last_error.as_deref(), Some("create_failed:8"));
    assert!(window.attach_sent_at.is_none(), "nothing was attached");
    assert_eq!(rig.deletes(), 0, "cleanup starts on the next step");

    // Снос: машина удалена и подтверждена списком, затем удалён оставшийся диск.
    let end = t0 + TimeDelta::seconds(4);
    rig.tick(end).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!((rig.deletes(), rig.disk_deletes()), (1, 1));
    assert_eq!(rig.fake.instance_count(), 0);
    assert!(rig.fake.control().disks.is_empty());
    assert_eq!(
        (window.server_gone_at, window.disk_gone_at),
        (Some(end), Some(end))
    );
    assert!(window.boot_disk_id.is_some(), "the disk was found by name");
    // Машина и диск существовали: их время посчитано, а не обнулено.
    assert_eq!(window.final_cost_kopecks, Some(cost(t0, end)));
    assert_eq!(window.last_error.as_deref(), Some("create_failed:8"));
    rig.try_create(60).await.expect("the slot is free again");
    Ok(())
}

#[tokio::test]
async fn a_failed_create_operation_that_rolled_back_still_confirms_both_absences() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    // Яндекс откатил создание сам: ни машины, ни диска.
    rig.fake.control().fail_next_create_operation = Some((9, "Not enough resources".to_owned()));
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "identified");
    assert_eq!(rig.fake.instance_count(), 0);
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    assert_eq!(rig.state(id).await, "deleting");
    // Списки не читаются: окно не заканчивается на одном только «операция не удалась».
    rig.fake.control().list_fail_page = Some(FailPage {
        list: "instances",
        page: 0,
        status: 500,
        times: None,
    });
    rig.tick(t0 + TimeDelta::seconds(2)).await;
    assert_eq!(rig.state(id).await, "deleting");
    rig.fake.control().list_fail_page = None;
    rig.tick(t0 + TimeDelta::seconds(40)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!((rig.deletes(), rig.disk_deletes()), (0, 0));
    assert!(window.server_gone_at.is_some() && window.disk_gone_at.is_some());
    assert_eq!(window.last_error.as_deref(), Some("create_failed:9"));
    Ok(())
}

// ───────── Прерывание и поломка машины ─────────

#[tokio::test]
async fn a_stopped_preemptible_instance_is_cleaned_up_as_preempted() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, session, at) = rig.drive_to_ready(120, t0).await;
    let window = rig.window(id).await;
    assert!(window.preemptible);
    let instance_id = window.instance_id.clone().unwrap();
    let machine = rig.cloud_machine().await;
    let running = rig.seed_job(Some(machine), "running", &["cloud"]).await;

    // Первый проход готового окна опрашивает машину: она работает.
    let polls = || rig.fake.control().count("GET", "/compute/v1/instances/");
    let polled = at + TimeDelta::seconds(1);
    let before = polls();
    rig.tick(polled).await;
    assert_eq!(polls(), before + 1);
    // Яндекс остановил машину. Опрос идёт не чаще раза в 30 секунд: проходы раньше этого
    // ничего не спрашивают и не замечают.
    rig.fake.control().preempt(&instance_id);
    for seconds in [5, 15, 29] {
        rig.tick(polled + TimeDelta::seconds(seconds)).await;
        assert_eq!(rig.state(id).await, "ready", "after {seconds} s");
    }
    assert_eq!(polls(), before + 1, "no poll inside the 30 s interval");

    let seen = polled + TimeDelta::seconds(30);
    rig.tick(seen).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.delete_reason.as_deref(), Some("preempted"));
    assert_eq!(window.stopped_at, Some(seen));
    assert_eq!(rig.alert_codes().await, vec!["preempted"]);
    // Раннер отозван в той же транзакции: его задача потеряна.
    assert_eq!(rig.job_state(running).await, "lost");
    assert_eq!(
        machines::request_runner_token(&rig.pool, rig.carrier, machine, true).await,
        Err(RunnerRefusal::Revoked)
    );
    drop(session);

    // Снос убирает остановленную машину и её диск; окно заканчивается обычным `ended`.
    let end = seen + TimeDelta::seconds(10);
    rig.tick(end).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert_eq!(window.delete_reason.as_deref(), Some("preempted"));
    assert_eq!(window.stopped_at, Some(seen), "the stop mark is kept");
    assert_eq!(rig.deletes(), 1);
    assert_eq!(rig.fake.instance_count(), 0);
    // Остановленная машина считается по полной ставке до подтверждённого исчезновения.
    assert_eq!(window.final_cost_kopecks, Some(cost(t0, end)));
    let view = windows::view(&window, end);
    assert_eq!(view.stopped_at, Some(seen));
    assert!(view.preemptible);
    // Второй пуш — отказ от замены: задач в очереди нет.
    assert_eq!(
        rig.alert_codes().await,
        vec!["preempted", "replacement_skipped"]
    );
    assert_eq!(rig.last_skip_reason().await, "no_waiting_job");
    Ok(())
}

#[tokio::test]
async fn a_stopped_regular_instance_is_cleaned_up_as_vm_broken() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig_with(pool, |config| config.preemptible = false).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _session, at) = rig.drive_to_ready(120, t0).await;
    let window = rig.window(id).await;
    assert!(!window.preemptible);
    let instance_id = window.instance_id.clone().unwrap();
    assert!(!rig.fake.control().instances[&instance_id].preemptible);

    // Обычная машина сама не останавливается: это поломка, а не прерывание.
    rig.fake
        .control()
        .instances
        .get_mut(&instance_id)
        .unwrap()
        .forced_status = Some("STOPPING".to_owned());
    let seen = at + TimeDelta::seconds(30);
    rig.tick(seen).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.delete_reason.as_deref(), Some("vm_broken"));
    assert_eq!(window.stopped_at, Some(seen));
    assert_eq!(rig.alert_codes().await, vec!["vm_broken"]);
    Ok(())
}

#[tokio::test]
async fn a_crashed_instance_is_vm_broken_even_when_preemptible() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _session, at) = rig.drive_to_ready(60, t0).await;
    let instance_id = rig.window(id).await.instance_id.unwrap();
    // Окно дошло до предела и дорабатывает: опрос машины идёт и в `draining`.
    let machine = rig.cloud_machine().await;
    rig.seed_job(Some(machine), "running", &["cloud"]).await;
    rig.tick(t0 + TimeDelta::minutes(60)).await;
    assert_eq!(rig.state(id).await, "draining");
    assert!(at < t0 + TimeDelta::minutes(60));

    rig.fake
        .control()
        .instances
        .get_mut(&instance_id)
        .unwrap()
        .forced_status = Some("CRASHED".to_owned());
    rig.tick(t0 + TimeDelta::minutes(61)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.delete_reason.as_deref(), Some("vm_broken"));
    assert_eq!(window.stopped_at, None, "a crash is not a stop");
    assert_eq!(rig.alert_codes().await, vec!["vm_broken"]);
    Ok(())
}

#[tokio::test]
async fn a_preempted_window_retries_its_sourced_job_and_gets_a_replacement() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.set_budget(10_000_000).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, session, at) = rig.drive_to_ready(120, t0).await;
    let parent = rig.window(id).await;
    let instance_id = parent.instance_id.clone().unwrap();
    let machine = rig.cloud_machine().await;
    let job = rig.seed_running_sourced(machine, 0).await;

    // Яндекс остановил машину: отзыв держит задачу с исходником до подтверждённой остановки.
    rig.fake.control().preempt(&instance_id);
    let seen = at + TimeDelta::seconds(30);
    rig.tick(seen).await;
    let held = rig.job_triple(job).await;
    assert_eq!(held, ("retry_pending".to_owned(), 1, 0));
    drop(session);
    // Ждущая повтора не размещается даже на том же раннере.
    assert_eq!(
        place::place_carrier(&rig.pool, &rig.registry, rig.carrier)
            .await
            .unwrap(),
        0
    );
    assert_eq!(rig.job_triple(job).await.0, "retry_pending");
    assert_eq!(rig.alert_codes().await, vec!["preempted"]);

    // Снос убирает машину и диск; их отсутствие доказывает, что прежний процесс не идёт.
    let end = seen + TimeDelta::seconds(10);
    rig.tick(end).await;
    let parent = rig.window(id).await;
    assert_eq!(parent.state, "ended");
    assert_eq!(
        rig.job_triple(job).await,
        ("waiting_runner".to_owned(), 2, 1)
    );

    // Ровно одна замена: то же предложение и повод, предел родителя абсолютный.
    let replacements = rig.replacements_of(id).await;
    assert_eq!(replacements.len(), 1);
    let replacement = &replacements[0];
    assert_eq!(replacement.replaces_window_id, Some(id));
    assert_eq!(replacement.state, "create_intent");
    assert_eq!(replacement.proposal_id, parent.proposal_id);
    assert_eq!(replacement.trigger, parent.trigger);
    assert!(replacement.preemptible);
    assert_eq!(replacement.hard_cap_at, parent.hard_cap_at);
    assert_eq!(
        rig.alert_codes().await,
        vec!["preempted", "replacement_started"]
    );
    Ok(())
}

#[tokio::test]
async fn a_replacement_inherits_the_prepare_of_its_parent() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.set_budget(10_000_000).await;
    let _carrier = rig.carrier_session();
    let prepare = Prepare {
        packages: vec!["jq".to_owned()],
        script: Some("echo prepared\n".to_owned()),
    };
    let t0 = now_micros();
    let id = rig
        .try_create_with(120, Some(prepare.clone()))
        .await
        .expect("window created");
    rig.tick(t0).await;
    rig.tick(t0 + TimeDelta::seconds(1)).await;
    let session = rig.attach().await;
    let at = t0 + TimeDelta::seconds(2);
    rig.tick(at).await;
    assert_eq!(rig.state(id).await, "ready");
    let machine = rig.cloud_machine().await;
    rig.seed_running_sourced(machine, 0).await;
    let stopped = rig.window(id).await.instance_id.unwrap();
    rig.fake.control().preempt(&stopped);
    let seen = at + TimeDelta::seconds(30);
    rig.tick(seen).await;
    drop(session);
    rig.tick(seen + TimeDelta::seconds(10)).await;
    assert_eq!(rig.state(id).await, "ended");
    let replacements = rig.replacements_of(id).await;
    assert_eq!(replacements.len(), 1);
    assert_eq!(
        replacements[0].prepare(),
        Some(prepare),
        "the replacement takes the parent's approved preparation as is"
    );
    Ok(())
}

#[tokio::test]
async fn a_replacement_is_never_replaced_and_its_retried_job_is_lost() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.set_budget(10_000_000).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, parent_session, at) = rig.drive_to_ready(120, t0).await;
    let machine = rig.cloud_machine().await;
    let job = rig.seed_running_sourced(machine, 0).await;
    let stopped = rig.window(id).await.instance_id.unwrap();
    rig.fake.control().preempt(&stopped);
    let seen = at + TimeDelta::seconds(30);
    rig.tick(seen).await;
    drop(parent_session);
    let end = seen + TimeDelta::seconds(10);
    rig.tick(end).await;
    let replacement = rig.replacements_of(id).await.remove(0);
    assert_eq!(
        rig.job_triple(job).await,
        ("waiting_runner".to_owned(), 2, 1)
    );

    // Замена поднимается штатно.
    rig.tick(end + TimeDelta::seconds(1)).await;
    assert_eq!(rig.window(replacement.id).await.state, "identified");
    rig.tick(end + TimeDelta::seconds(2)).await;
    let _session = rig.attach().await;
    let ready = end + TimeDelta::seconds(3);
    rig.tick(ready).await;
    assert_eq!(rig.window(replacement.id).await.state, "ready");
    // На замене идёт та же задача второй попытки: её второе прерывание повтора уже не даёт.
    let on_replacement = rig.seed_running_sourced(rig.cloud_machine().await, 1).await;
    let stopped = rig.window(replacement.id).await.instance_id.unwrap();
    rig.fake.control().preempt(&stopped);
    let second = ready + TimeDelta::seconds(30);
    rig.tick(second).await;
    assert_eq!(
        rig.job_triple(on_replacement).await,
        ("lost".to_owned(), 1, 1)
    );
    rig.tick(second + TimeDelta::seconds(10)).await;
    assert_eq!(rig.window(replacement.id).await.state, "ended");
    // Замену не заменяют: третьего окна нет.
    assert!(rig.replacements_of(replacement.id).await.is_empty());
    assert_eq!(
        rig.alert_codes().await,
        vec![
            "preempted",
            "replacement_started",
            "preempted",
            "replacement_skipped"
        ]
    );
    assert_eq!(rig.last_skip_reason().await, "window_exists");
    Ok(())
}

#[tokio::test]
async fn a_canceled_job_is_not_retried_after_preemption() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.set_budget(10_000_000).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, session, at) = rig.drive_to_ready(120, t0).await;
    let machine = rig.cloud_machine().await;
    let job = rig.seed_running_sourced(machine, 0).await;
    // Отмену просили: повтору не бывать.
    sqlx::query("update jobs set cancel_requested_at = clock_timestamp() where id = $1")
        .bind(job)
        .execute(&rig.pool)
        .await?;
    let stopped = rig.window(id).await.instance_id.unwrap();
    rig.fake.control().preempt(&stopped);
    let seen = at + TimeDelta::seconds(30);
    rig.tick(seen).await;
    drop(session);
    assert_eq!(rig.job_triple(job).await, ("lost".to_owned(), 1, 0));
    let end = seen + TimeDelta::seconds(10);
    rig.tick(end).await;
    assert_eq!(rig.window(id).await.state, "ended");
    // Ждущих задач нет: замены тоже.
    assert!(rig.replacements_of(id).await.is_empty());
    assert_eq!(
        rig.alert_codes().await,
        vec!["preempted", "replacement_skipped"]
    );
    assert_eq!(rig.last_skip_reason().await, "no_waiting_job");
    Ok(())
}

#[tokio::test]
async fn a_broken_vm_gets_no_retry_and_no_replacement() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig_with(pool, |config| config.preemptible = false).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, session, at) = rig.drive_to_ready(120, t0).await;
    let machine = rig.cloud_machine().await;
    let job = rig.seed_running_sourced(machine, 0).await;
    // Обычная машина остановилась сама: это поломка, а не прерывание.
    let instance_id = rig.window(id).await.instance_id.unwrap();
    rig.fake
        .control()
        .instances
        .get_mut(&instance_id)
        .unwrap()
        .forced_status = Some("STOPPING".to_owned());
    let seen = at + TimeDelta::seconds(30);
    rig.tick(seen).await;
    drop(session);
    let parent = rig.window(id).await;
    assert_eq!(parent.delete_reason.as_deref(), Some("vm_broken"));
    assert_eq!(rig.job_triple(job).await, ("lost".to_owned(), 1, 0));
    assert_eq!(rig.alert_codes().await, vec!["vm_broken"]);
    rig.tick(seen + TimeDelta::seconds(10)).await;
    assert_eq!(rig.window(id).await.state, "ended");
    assert!(rig.replacements_of(id).await.is_empty());
    assert_eq!(rig.alert_codes().await, vec!["vm_broken"]);
    Ok(())
}

#[tokio::test]
async fn the_budget_refuses_the_replacement_without_over_budget_consent() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    // Бюджета хватает ровно на резерв родителя: после его конца на замену уже нет.
    let reservation = rig.reservation(120);
    sqlx::query("update cloud_budget set monthly_budget_kopecks = $1")
        .bind(reservation)
        .execute(&rig.pool)
        .await?;
    let t0 = now_micros();
    let (id, session, at) = rig.drive_to_ready(120, t0).await;
    let machine = rig.cloud_machine().await;
    let job = rig.seed_running_sourced(machine, 0).await;
    let stopped = rig.window(id).await.instance_id.unwrap();
    rig.fake.control().preempt(&stopped);
    let seen = at + TimeDelta::seconds(30);
    rig.tick(seen).await;
    drop(session);
    rig.tick(seen + TimeDelta::seconds(10)).await;
    assert_eq!(rig.window(id).await.state, "ended");
    assert_eq!(
        rig.job_triple(job).await,
        ("waiting_runner".to_owned(), 2, 1)
    );
    assert!(rig.replacements_of(id).await.is_empty());
    // Бюджетные пуши могли прийти раньше: важно, что отказ замены — последний и про бюджет.
    let codes = rig.alert_codes().await;
    assert!(
        codes.contains(&"replacement_skipped".to_owned()),
        "{codes:?}"
    );
    assert_eq!(rig.last_skip_reason().await, "budget");
    Ok(())
}

#[tokio::test]
async fn too_little_time_to_the_parent_deadline_skips_the_replacement() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.set_budget(10_000_000).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, session, _at) = rig.drive_to_ready(60, t0).await;
    let machine = rig.cloud_machine().await;
    let job = rig.seed_running_sourced(machine, 0).await;
    // Прерывание за десять минут до абсолютного предела: поднимать замену поздно.
    let seen = t0 + TimeDelta::minutes(50);
    let stopped = rig.window(id).await.instance_id.unwrap();
    rig.fake.control().preempt(&stopped);
    rig.tick(seen).await;
    drop(session);
    rig.tick(seen + TimeDelta::seconds(10)).await;
    let parent = rig.window(id).await;
    assert_eq!(parent.state, "ended");
    // Задача всё равно выпущена: снос подтвердил отсутствие прежней машины.
    assert_eq!(
        rig.job_triple(job).await,
        ("waiting_runner".to_owned(), 2, 1)
    );
    assert!(rig.replacements_of(id).await.is_empty());
    assert_eq!(
        rig.alert_codes().await,
        vec!["preempted", "replacement_skipped"]
    );
    assert_eq!(rig.last_skip_reason().await, "no_time");
    Ok(())
}

#[tokio::test]
async fn a_replacement_is_repriced_before_its_first_post() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.set_budget(10_000_000).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, session, _at) = rig.drive_to_ready(120, t0).await;
    let machine = rig.cloud_machine().await;
    let _job = rig.seed_running_sourced(machine, 0).await;
    // Провайдер подорожал уже после того, как родитель получил снимок цен.
    set_vcpu_price(&rig, "0.9");
    let stopped = rig.window(id).await.instance_id.unwrap();
    rig.fake.control().preempt(&stopped);
    // Прерывание через одиннадцать минут: снимок цен родителя старше срока годности.
    let seen = t0 + TimeDelta::minutes(11);
    let restarted = rig.restarted();
    restarted.tick(seen).await;
    drop(session);
    let end = seen + TimeDelta::seconds(10);
    restarted.tick(end).await;
    assert_eq!(rig.state(id).await, "ended");
    let replacements = rig.replacements_of(id).await;
    assert_eq!(replacements.len(), 1);
    let replacement = &replacements[0];
    // Первый POST замены спрашивает цену заново: рост резерва закрывает окно без вызова.
    restarted.tick(end + TimeDelta::seconds(1)).await;
    let replacement = rig.window(replacement.id).await;
    assert_eq!(replacement.state, "failed");
    assert_eq!(replacement.final_cost_kopecks, Some(0));
    assert_eq!(replacement.last_error.as_deref(), Some("reprice_growth"));
    assert_eq!(
        rig.posts(),
        1,
        "only the parent was created at the old rate"
    );
    Ok(())
}

#[tokio::test]
async fn a_window_past_its_deadline_fails_before_any_provider_call() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let now = now_micros();
    let id = raw_window(&rig.pool, "create_intent", None, None).await;
    sqlx::query("update cloud_windows set hard_cap_at = $2 where id = $1")
        .bind(id)
        .bind(now - TimeDelta::minutes(1))
        .execute(&rig.pool)
        .await?;
    rig.tick(now).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!(window.final_cost_kopecks, Some(0));
    assert_eq!(window.create_called_at, None, "nothing was called");
    assert_eq!(window.hard_cap_at, Some(now - TimeDelta::minutes(1)));
    assert_eq!(window.last_error.as_deref(), Some("deadline_passed"));
    assert_eq!(rig.posts(), 0);
    assert_eq!(
        rig.fake
            .control()
            .count("GET", "/compute/v1/images:latestByFamily"),
        0,
        "no provider call at all"
    );
    Ok(())
}

#[tokio::test]
async fn a_replacement_with_less_than_the_lead_left_fails_before_any_provider_call() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let now = now_micros();
    let parent = raw_window(
        &rig.pool,
        "ended",
        Some("fhmfakeinstance9999"),
        Some(now - TimeDelta::minutes(20)),
    )
    .await;
    let replacement = raw_window(&rig.pool, "create_intent", None, None).await;
    sqlx::query("update cloud_windows set replaces_window_id = $2, hard_cap_at = $3 where id = $1")
        .bind(replacement)
        .bind(parent)
        .bind(now + TimeDelta::minutes(10))
        .execute(&rig.pool)
        .await?;
    rig.tick(now).await;
    let window = rig.window(replacement).await;
    assert_eq!(window.state, "failed");
    assert_eq!(window.final_cost_kopecks, Some(0));
    assert_eq!(window.create_called_at, None, "nothing was called");
    assert_eq!(window.last_error.as_deref(), Some("replacement_too_late"));
    assert_eq!(rig.posts(), 0);
    assert_eq!(
        rig.fake
            .control()
            .count("GET", "/compute/v1/images:latestByFamily"),
        0,
        "no provider call at all"
    );
    Ok(())
}

#[tokio::test]
async fn a_retry_pending_job_without_a_preempted_window_is_lost_on_reconcile() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    // Живое окно без прерывания: строка облачной машины уже есть.
    let id = rig.create(60).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "identified");
    let machine = rig.cloud_machine().await;
    let orphan = rig
        .seed_job(Some(machine), "retry_pending", &["cloud"])
        .await;
    // Следующая сверка: прерванного окна нет, значит, повтора не будет.
    rig.tick(t0 + TimeDelta::minutes(5)).await;
    assert_eq!(rig.job_triple(orphan).await.0, "lost");
    assert_eq!(
        rig.state(id).await,
        "identified",
        "the live window is untouched"
    );
    Ok(())
}

#[tokio::test]
async fn a_restart_between_revocation_and_absence_releases_the_job_once() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.set_budget(10_000_000).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, session, at) = rig.drive_to_ready(120, t0).await;
    let machine = rig.cloud_machine().await;
    let job = rig.seed_running_sourced(machine, 0).await;
    let stopped = rig.window(id).await.instance_id.unwrap();
    rig.fake.control().preempt(&stopped);
    let seen = at + TimeDelta::seconds(30);
    rig.tick(seen).await;
    drop(session);
    assert_eq!(rig.job_triple(job).await.0, "retry_pending");

    // Хаб перезапустился между отзывом и подтверждённым отсутствием: память пуста, база та же.
    let restarted = rig.restarted();
    let end = seen + TimeDelta::seconds(10);
    restarted.tick(end).await;
    assert_eq!(rig.window(id).await.state, "ended");
    assert_eq!(
        rig.job_triple(job).await,
        ("waiting_runner".to_owned(), 2, 1)
    );
    let replacements = rig.replacements_of(id).await;
    assert_eq!(replacements.len(), 1);
    assert_eq!(
        replacements[0].hard_cap_at,
        rig.window(id).await.hard_cap_at
    );

    // Повторный проход ничего не выпускает второй раз.
    restarted.tick(end + TimeDelta::seconds(10)).await;
    assert_eq!(
        rig.job_triple(job).await,
        ("waiting_runner".to_owned(), 2, 1)
    );
    assert_eq!(rig.replacements_of(id).await.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_stop_after_an_owner_delete_never_rewrites_the_reason() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _session, at) = rig.drive_to_ready(240, t0).await;
    let instance_id = rig.window(id).await.instance_id.unwrap();
    // Владелец удалил окно сам, а машина после этого ещё и остановилась.
    windows::request_delete(&rig.pool, id, windows::REASON_OWNER, at).await?;
    rig.fake.control().delete_status = Some(500);
    rig.fake.control().preempt(&instance_id);
    for seconds in [1, 31, 62, 200] {
        rig.tick(at + TimeDelta::seconds(seconds)).await;
    }
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(
        window.delete_reason.as_deref(),
        Some("owner"),
        "the owner's reason stays"
    );
    assert_eq!(window.stopped_at, None);
    assert!(
        !rig.alert_codes().await.contains(&"preempted".to_owned()),
        "{:?}",
        rig.alert_codes().await
    );
    Ok(())
}

#[tokio::test]
async fn an_instance_that_vanished_goes_through_cleanup_and_ends_gone() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _session, at) = rig.drive_to_ready(120, t0).await;
    let instance_id = rig.window(id).await.instance_id.unwrap();
    // Машину удалили в консоли, а её диск остался в каталоге.
    rig.fake.control().leave_disk(&instance_id);
    rig.fake.control().remove_instance(&instance_id);
    assert_eq!(rig.fake.control().disks.len(), 1);
    // Список дисков пока не читается: окно не может закончиться сразу.
    rig.fake.control().list_fail_page = Some(FailPage {
        list: "disks",
        page: 0,
        status: 500,
        times: None,
    });
    let seen = at + TimeDelta::seconds(30);
    rig.tick(seen).await;
    let window = rig.window(id).await;
    assert_eq!(
        window.state, "deleting",
        "the window never goes straight to gone"
    );
    assert_eq!(window.delete_reason.as_deref(), Some("gone"));
    assert_eq!(window.server_gone_at, Some(seen));
    assert_eq!(window.disk_gone_at, None);
    assert_eq!(rig.alert_codes().await, vec!["window_gone"]);

    // Список дисков читается: оставшийся диск удалён, и только теперь окно `gone`.
    rig.fake.control().list_fail_page = None;
    let end = seen + TimeDelta::minutes(2);
    rig.tick(end).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "gone");
    assert_eq!(window.last_error.as_deref(), Some("server_missing"));
    assert_eq!((rig.deletes(), rig.disk_deletes()), (0, 1));
    assert!(rig.fake.control().disks.is_empty());
    // Машина посчитана до своего исчезновения, диск — до своего, на две минуты дольше.
    assert_eq!(
        window.final_cost_kopecks,
        Some(price::ledger_kopecks(COMPUTE, DISK, t0, seen, end)?)
    );
    Ok(())
}

// ───────── Неопределённое создание и списки ─────────

#[tokio::test]
async fn an_ambiguous_create_is_found_by_name_and_is_never_posted_twice() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    rig.fake.control().create = CreateMode::AcceptThenDrop;
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "create_unknown");
    assert_eq!(rig.window(id).await.instance_id, None);
    assert_eq!(rig.posts(), 1);
    // Следующий полный список каталога машину ещё не показывает.
    rig.fake.control().listing_hidden_polls = 1;

    // Проходы между сверками ничего не меняют и не звонят повторно.
    for seconds in [10, 60, 120] {
        rig.tick(t0 + TimeDelta::seconds(seconds)).await;
    }
    assert_eq!(rig.state(id).await, "create_unknown");
    // Сверка видит пустой список (машина скрыта) раньше окна ожидания: окно не трогается.
    rig.tick(t0 + TimeDelta::minutes(5)).await;
    assert_eq!(rig.state(id).await, "create_unknown");
    // Следующая сверка видит машину по имени.
    rig.tick(t0 + TimeDelta::minutes(10)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "identified");
    assert!(window.instance_id.is_some());
    assert_eq!(rig.posts(), 1, "there is never a second POST");
    assert_eq!(rig.fake.instance_count(), 1);
    Ok(())
}

#[tokio::test]
async fn an_ambiguous_create_absent_from_full_listings_fails_after_the_window() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    rig.fake.control().create = CreateMode::unavailable();
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "create_unknown");
    rig.tick(t0 + TimeDelta::minutes(5)).await;
    assert_eq!(
        rig.state(id).await,
        "create_unknown",
        "a full listing without the instance is not enough before the window"
    );
    rig.tick(t0 + TimeDelta::minutes(10)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!(window.delete_reason.as_deref(), Some("create_failed"));
    assert_eq!(window.last_error.as_deref(), Some("not_found_in_listing"));
    assert!(window.server_gone_at.is_some() && window.disk_gone_at.is_some());
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

#[tokio::test]
async fn an_absence_timestamp_is_taken_after_the_listing_not_at_the_tick_start() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    // Провайдер отвечает только после того, как тест отпустит запрос: проход уже давно начался.
    let t0 = now_micros();
    let id = raw_window(
        &rig.pool,
        "deleting",
        None,
        Some(t0 - TimeDelta::minutes(20)),
    )
    .await;
    let gate = ResponseGate::new();
    rig.fake.control().gate = Some(gate.clone());
    let waiting = gate.waiting.notified();
    let service = Arc::clone(&rig.service);
    let tick = tokio::spawn(async move { service.tick(t0).await });
    waiting.await;
    let _ = rig.window(id).await;
    let released = now_micros();
    assert!(released > t0, "the tick began before the provider answered");
    gate.release.notify_one();
    tick.await.unwrap();
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert!(
        window.server_gone_at.is_some_and(|at| at >= released),
        "the machine end is not backdated to the tick start: {:?}",
        window.server_gone_at
    );
    assert!(
        window.disk_gone_at.is_some_and(|at| at >= released),
        "the disk end is not backdated to the tick start: {:?}",
        window.disk_gone_at
    );
    Ok(())
}

#[tokio::test]
async fn a_vanished_instance_is_marked_gone_after_the_listing_that_confirmed_it() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let instance = rig.add_instance("placeholder");
    let id = raw_window(
        &rig.pool,
        "ready",
        Some(&instance),
        Some(t0 - TimeDelta::minutes(20)),
    )
    .await;
    {
        let mut state = rig.fake.control();
        state.instances.get_mut(&instance).unwrap().name = windows::server_name(id);
        let disk = state.instances[&instance].disk_id.clone();
        state.disks.get_mut(&disk).unwrap().name = windows::server_name(id);
        // Машина пропала, пока хаба не было: полный список подтверждает это только сейчас.
        state.remove_instance(&instance);
    }
    let gate = ResponseGate::new();
    rig.fake.control().gate = Some(gate.clone());
    let waiting = gate.waiting.notified();
    let service = Arc::clone(&rig.service);
    let tick = tokio::spawn(async move { service.tick(t0).await });
    waiting.await;
    let _ = rig.window(id).await;
    let released = now_micros();
    assert!(released > t0, "the tick began before the provider answered");
    gate.release.notify_one();
    tick.await.unwrap();
    let window = rig.window(id).await;
    assert!(
        window.server_gone_at.is_some_and(|at| at >= released),
        "the machine end is not backdated to the tick start: {:?}",
        window.server_gone_at
    );
    Ok(())
}

#[tokio::test]
async fn an_instance_a_listing_missed_is_still_deleted_because_its_disk_is_attached() -> TestResult
{
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    rig.fake.control().create = CreateMode::AcceptThenDrop;
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "create_unknown");
    assert_eq!(rig.fake.instance_count(), 1, "the instance was created");
    // Списки машин до неё не доходят: ни сверка, ни первый шаг сноса её не видят.
    rig.fake.control().listing_hidden_polls = 2;
    let failed = t0 + TimeDelta::minutes(10);
    rig.tick(failed).await;
    let window = rig.window(id).await;
    assert_eq!(
        window.state, "deleting",
        "the attached disk keeps the window open"
    );
    assert_eq!(window.delete_reason.as_deref(), Some("create_failed"));
    // Диск с именем окна подключён к машине, и машина с этим именем нашлась в списке: снос
    // вернулся к ней.
    assert!(window.instance_id.is_some());
    assert_eq!(window.server_gone_at, None);
    assert!(window.boot_disk_id.is_some());
    assert_eq!(rig.deletes(), 0);

    let end = failed + TimeDelta::seconds(1);
    rig.tick(end).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!(rig.deletes(), 1);
    assert_eq!(rig.fake.instance_count(), 0);
    assert!(rig.fake.control().disks.is_empty());
    assert_eq!(window.last_error.as_deref(), Some("not_found_in_listing"));
    // Машина и диск существовали всё это время: итог не нулевой.
    assert_eq!(window.final_cost_kopecks, Some(cost(t0, end)));
    assert_eq!(rig.posts(), 1);
    Ok(())
}

#[tokio::test]
async fn a_failed_or_partial_listing_changes_nothing() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.fake.control().create = CreateMode::unavailable();
    let t0 = now_micros();
    let unknown = rig.create(60).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(unknown).await, "create_unknown");

    // Страница списка отказывает: окно остаётся, как бы долго ни ждали.
    rig.fake.control().list_fail_page = Some(FailPage {
        list: "instances",
        page: 0,
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
    // Страницы читаются, но обход зациклился на одном токене: тоже не список.
    {
        let mut state = rig.fake.control();
        state.list_fail_page = None;
        state.repeat_page_token = true;
    }
    rig.tick(t0 + TimeDelta::minutes(30)).await;
    assert_eq!(rig.state(unknown).await, "create_unknown");
    // Список машин целый, а список дисков нет: каталог по-прежнему не прочитан.
    {
        let mut state = rig.fake.control();
        state.repeat_page_token = false;
        state.list_fail_page = Some(FailPage {
            list: "disks",
            page: 0,
            status: 503,
            times: None,
        });
    }
    rig.tick(t0 + TimeDelta::minutes(33)).await;
    assert_eq!(rig.state(unknown).await, "create_unknown");
    // Оба списка целые: теперь окно закрывается.
    rig.fake.control().list_fail_page = None;
    rig.tick(t0 + TimeDelta::minutes(36)).await;
    assert_eq!(rig.state(unknown).await, "failed");

    // Живое окно, чья машина пропала: при плохом списке остаётся живым, без пуша.
    rig.fake.control().create = CreateMode::Accept;
    let id = rig.create(60).await;
    let t1 = t0 + TimeDelta::minutes(40);
    rig.tick(t1).await;
    assert_eq!(rig.state(id).await, "identified");
    rig.tick(t1 + TimeDelta::seconds(1)).await;
    rig.vanish_instances();
    rig.fake.control().list_fail_page = Some(FailPage {
        list: "instances",
        page: 0,
        status: 500,
        times: None,
    });
    // Срок ожидания подключения (15 минут) ещё не вышел, а срок появления машины в списках
    // (10 минут) уже вышел: решает только список.
    rig.tick(t1 + TimeDelta::minutes(11)).await;
    assert_eq!(rig.state(id).await, "identified");
    assert!(rig.alert_codes().await.is_empty());
    // Целый список без машины: окно уходит на снос, подтверждает диск и становится `gone`,
    // владельцу пуш, раннер отозван.
    rig.fake.control().list_fail_page = None;
    rig.tick(t1 + TimeDelta::minutes(12)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "gone");
    assert!(window.final_cost_kopecks.is_some());
    assert_eq!(rig.alert_codes().await, vec!["window_gone"]);
    Ok(())
}

#[tokio::test]
async fn a_listing_gap_does_not_close_an_instance_that_still_answers() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    assert_eq!(rig.state(id).await, "identified");
    let instance = rig.window(id).await.instance_id.unwrap();
    // Список каталога машину не показывает, а прямой запрос по номеру — показывает.
    rig.fake
        .control()
        .instances
        .get_mut(&instance)
        .unwrap()
        .hidden_in_listing = true;
    rig.tick(t0 + TimeDelta::minutes(11)).await;
    assert_eq!(
        rig.state(id).await,
        "identified",
        "absence in a listing alone is not proof"
    );
    // Машина действительно исчезла: теперь окно закрывается.
    rig.fake.control().remove_instance(&instance);
    rig.tick(t0 + TimeDelta::minutes(17)).await;
    assert_eq!(rig.state(id).await, "gone");
    Ok(())
}

#[tokio::test]
async fn the_hard_cap_drains_cancels_then_bounds_teardown_and_deletes() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

    // Граница сноса: неподтверждённое становится `lost`, раннер отозван, машина удаляется.
    rig.tick(t0 + TimeDelta::minutes(120)).await;
    assert_eq!(rig.state(id).await, "deleting");
    assert_eq!(rig.job_state(running).await, "lost");
    let end = t0 + TimeDelta::minutes(120) + TimeDelta::seconds(1);
    rig.tick(end).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    // 120 минут и секунда от вызова создания, по секундам: чуть больше двух часов из трёх
    // зарезервированных.
    assert_eq!(window.final_cost_kopecks, Some(cost(t0, end)));
    assert!(window.final_cost_kopecks.unwrap() > 2 * HOURLY);
    assert!(window.final_cost_kopecks.unwrap() <= window.reservation_kopecks);
    Ok(())
}

#[tokio::test]
async fn a_draining_window_with_no_work_left_is_deleted_at_once() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn a_window_found_past_every_bound_after_an_outage_is_deleted_with_an_alert() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, _session, _) = rig.drive_to_ready(60, t0).await;

    // Хаб лежал: новая служба (старт хаба) видит окно далеко за пределом с запасами.
    rig.restarted().tick(t0 + TimeDelta::hours(6)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.delete_reason.as_deref(), Some("overrun"));
    assert_eq!(rig.alert_codes().await, vec!["outage_extra_cost"]);
    let extra = rig.alerts().await[0]["extra_kopecks"].as_i64().unwrap();
    // Пять часов сверх предела по обеим ставкам.
    assert_eq!(extra, 5 * HOURLY);
    Ok(())
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

#[tokio::test]
async fn a_delete_requires_the_expected_name_and_the_folder_of_the_window() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    // Прямой ответ отдаёт не то имя, что у окна: DELETE нет, окно живо, пуш владельцу.
    {
        let rig = rig(pool.clone()).await;
        enable_pushes(&rig.pool).await;
        let now = now_micros();
        let instance = rig.add_instance("someone-else");
        let id = raw_window(
            &rig.pool,
            "deleting",
            Some(&instance),
            Some(now - TimeDelta::minutes(20)),
        )
        .await;
        rig.tick(now).await;
        assert_eq!(rig.deletes(), 0, "no blind DELETE by id");
        let window = rig.window(id).await;
        assert_eq!(window.state, "deleting", "the window stays open");
        assert_eq!(window.last_error.as_deref(), Some("ownership_mismatch"));
        let alerts = rig.alerts().await;
        assert_eq!(alerts[0]["code"], "ownership_mismatch", "{alerts:?}");
        assert_eq!(alerts[0]["instance_id"], instance);
        // Имя машины исправили: обычный путь удаляет.
        rig.fake
            .control()
            .instances
            .get_mut(&instance)
            .unwrap()
            .name = windows::server_name(id);
        rig.tick(now + TimeDelta::minutes(31)).await;
        assert_eq!(rig.deletes(), 1, "the corrected instance is deleted");
        assert_eq!(rig.window(id).await.instance_id, Some(instance));
        close_window(&rig.pool, id).await?;
    }

    // Имя верное, но машина в другом каталоге: её нет в полном списке каталога окна, а
    // прямой ответ называет чужой каталог.
    {
        let rig = rig(pool.clone()).await;
        enable_pushes(&rig.pool).await;
        let now = now_micros();
        let instance = rig.add_instance("placeholder");
        let id = raw_window(
            &rig.pool,
            "deleting",
            Some(&instance),
            Some(now - TimeDelta::minutes(20)),
        )
        .await;
        {
            let mut state = rig.fake.control();
            let entry = state.instances.get_mut(&instance).unwrap();
            entry.name = windows::server_name(id);
            entry.folder_id = OTHER_FOLDER.to_owned();
        }
        rig.tick(now).await;
        assert_eq!(
            rig.deletes(),
            0,
            "an instance outside the stored folder is never deleted"
        );
        let window = rig.window(id).await;
        assert_eq!(window.state, "deleting");
        assert_eq!(window.server_gone_at, None, "it still answers by id");
        assert_eq!(window.last_error.as_deref(), Some("ownership_mismatch"));
        close_window(&rig.pool, id).await?;
    }

    // Имя верное и машина в своём каталоге: обычное удаление.
    {
        let rig = rig(pool.clone()).await;
        let now = now_micros();
        let instance = rig.add_instance("placeholder");
        let id = raw_window(
            &rig.pool,
            "deleting",
            Some(&instance),
            Some(now - TimeDelta::minutes(20)),
        )
        .await;
        rig.fake
            .control()
            .instances
            .get_mut(&instance)
            .unwrap()
            .name = windows::server_name(id);
        rig.tick(now).await;
        assert_eq!(rig.deletes(), 1, "the owned instance is deleted");
        let window = rig.window(id).await;
        assert!(window.delete_accepted_at.is_some());
        assert!(window.delete_operation_id.is_some());
        rig.tick(now + TimeDelta::seconds(40)).await;
        assert!(!rig.fake.control().instances.contains_key(&instance));
        assert_eq!(rig.state(id).await, "ended");
    }
    Ok(())
}

#[tokio::test]
async fn a_direct_404_alone_never_confirms_absence() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let now = now_micros();
    let instance = rig.add_instance("placeholder");
    let id = raw_window(
        &rig.pool,
        "deleting",
        Some(&instance),
        Some(now - TimeDelta::minutes(20)),
    )
    .await;
    rig.fake
        .control()
        .instances
        .get_mut(&instance)
        .unwrap()
        .name = windows::server_name(id);
    // Тёплый проход: сверка проходит, а первая попытка удаления ещё не наступила.
    sqlx::query(
        "update cloud_windows set delete_accepted_at = null, next_delete_at = $2 where id = $1",
    )
    .bind(id)
    .bind(now + TimeDelta::seconds(30))
    .execute(&rig.pool)
    .await?;
    rig.tick(now).await;
    assert_eq!(rig.fake.instance_count(), 1);
    // Точечный ответ 404, но полный список каталога всё ещё показывает машину: отсутствие
    // не подтверждено, оплата машины не закончена.
    rig.fake.control().fail_next.push_back(404);
    rig.tick(now + TimeDelta::seconds(40)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert!(window.server_gone_at.is_none());
    assert_eq!(rig.fake.instance_count(), 1);
    assert_eq!(rig.deletes(), 0);
    Ok(())
}

#[tokio::test]
async fn repeated_lookup_failures_count_as_delete_failures_and_alert() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let now = now_micros();
    let instance = rig.add_instance("placeholder");
    let id = raw_window(
        &rig.pool,
        "deleting",
        Some(&instance),
        Some(now - TimeDelta::minutes(20)),
    )
    .await;
    rig.fake
        .control()
        .instances
        .get_mut(&instance)
        .unwrap()
        .name = windows::server_name(id);
    // Тёплый проход: сверка проходит, первая попытка удаления ещё не наступила.
    sqlx::query(
        "update cloud_windows set delete_accepted_at = null, next_delete_at = $2 where id = $1",
    )
    .bind(id)
    .bind(now + TimeDelta::seconds(30))
    .execute(&rig.pool)
    .await?;
    rig.tick(now).await;
    assert_eq!(rig.window(id).await.delete_attempts, 0);
    assert!(rig.alert_codes().await.is_empty());

    // Точечный ответ не приходит: это неудача удаления, с откатом.
    for (attempt, at, next) in [
        (1, now + TimeDelta::seconds(40), TimeDelta::seconds(30)),
        (2, now + TimeDelta::seconds(130), TimeDelta::seconds(60)),
    ] {
        rig.fake.control().fail_next.push_back(500);
        rig.tick(at).await;
        let window = rig.window(id).await;
        assert_eq!(window.delete_attempts, attempt);
        assert_eq!(window.next_delete_at, Some(at + next));
        assert_eq!(rig.fake.instance_count(), 1, "nothing is deleted blindly");
        assert!(rig.alert_codes().await.is_empty(), "fewer than three");
    }
    // Раньше паузы после второй неудачи прохода нет.
    rig.tick(now + TimeDelta::seconds(160)).await;
    assert_eq!(rig.window(id).await.delete_attempts, 2);
    assert_eq!(rig.fake.instance_count(), 1);
    // Третья неудача: пуш о неудаляемой машине.
    rig.fake.control().fail_next.push_back(500);
    rig.tick(now + TimeDelta::seconds(260)).await;
    let window = rig.window(id).await;
    assert_eq!(window.delete_attempts, 3);
    assert_eq!(window.last_error.as_deref(), Some("delete_lookup_failed"));
    assert_eq!(rig.alert_codes().await, vec!["delete_failing"]);
    assert_eq!(rig.state(id).await, "deleting");

    // Провайдер отвечает: обычное удаление и конец окна.
    rig.tick(now + TimeDelta::minutes(10)).await;
    assert_eq!(rig.state(id).await, "ended");
    Ok(())
}

#[tokio::test]
async fn a_confirmed_absence_skips_the_delete_call() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let now = now_micros();
    let id = raw_window(
        &rig.pool,
        "deleting",
        Some("fhmfakeinstance9999"),
        Some(now - TimeDelta::minutes(20)),
    )
    .await;
    sqlx::query("update cloud_windows set server_gone_at = $2 where id = $1")
        .bind(id)
        .bind(now)
        .execute(&rig.pool)
        .await?;
    rig.tick(now).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert!(
        window.disk_gone_at.is_some_and(|at| at >= now),
        "the disk is confirmed too: {:?}",
        window.disk_gone_at
    );
    assert_eq!(rig.deletes(), 0, "no DELETE once absence is confirmed");
    assert_eq!(
        rig.fake.control().count("GET", "/compute/v1/instances/"),
        0,
        "the instance is not even looked up"
    );
    Ok(())
}

#[tokio::test]
async fn an_instance_deleted_by_hand_while_the_delete_is_failing_ends_the_window() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, when) = drive_to_deleting(&rig, t0).await;
    rig.fake.control().delete_status = Some(500);
    rig.tick(when + TimeDelta::seconds(1)).await;
    assert_eq!(rig.state(id).await, "deleting");
    assert_eq!(
        rig.window(id).await.last_error.as_deref(),
        Some("delete_failed")
    );
    // Владелец удалил машину в консоли: её нет ни по номеру, ни в полном списке.
    rig.vanish_instances();
    rig.tick(when + TimeDelta::minutes(6)).await;
    assert_eq!(rig.state(id).await, "ended");
    assert_eq!(rig.deletes(), 1, "no second DELETE for an absent instance");
    Ok(())
}

#[tokio::test]
async fn a_failing_delete_retries_with_backoff_and_alerts_after_three_failures() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, when) = drive_to_deleting(&rig, t0).await;
    rig.fake.control().delete_status = Some(500);

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
    rig.fake.control().delete_status = None;
    rig.tick(when + TimeDelta::minutes(50)).await;
    assert_eq!(rig.state(id).await, "ended");
    Ok(())
}

#[tokio::test]
async fn an_accepted_delete_is_confirmed_only_by_absence_from_a_full_listing() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, when) = drive_to_deleting(&rig, t0).await;
    // Удаление принято, но машина ещё долго видна в каталоге как `DELETING`.
    let instance_id = rig.window(id).await.instance_id.unwrap();
    rig.fake.control().delete_polls = 1_000_000;
    rig.tick(when + TimeDelta::seconds(1)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert!(window.delete_accepted_at.is_some());
    assert!(window.delete_confirmed_at.is_none());
    assert!(window.server_gone_at.is_none());
    rig.tick(when + TimeDelta::seconds(20)).await;
    assert_eq!(
        rig.state(id).await,
        "deleting",
        "still listed: still billing"
    );
    assert_eq!(
        rig.deletes(),
        1,
        "no second DELETE before the confirmation deadline"
    );
    // Машина всё ещё в списке через срок подтверждения: DELETE повторяется, владельцу пуш.
    rig.tick(when + TimeDelta::minutes(11)).await;
    assert_eq!(rig.state(id).await, "deleting");
    assert_eq!(rig.deletes(), 2, "the accepted delete is retried");
    assert!(
        rig.alert_codes()
            .await
            .contains(&"delete_failing".to_owned()),
        "{:?}",
        rig.alert_codes().await
    );
    // Дальше повторы идут с откатом, а окно остаётся живым.
    rig.tick(when + TimeDelta::minutes(23)).await;
    assert!(rig.deletes() >= 3, "the delete keeps retrying");
    rig.fake.control().remove_instance(&instance_id);
    let end = when + TimeDelta::minutes(40);
    rig.tick(end).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    // Оплата идёт до подтверждённого исчезновения, а не до принятого удаления.
    assert_eq!(window.final_cost_kopecks, Some(cost(t0, end)));
    Ok(())
}

#[tokio::test]
async fn a_failed_delete_operation_is_retried_after_a_fresh_ownership_check() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, when) = drive_to_deleting(&rig, t0).await;
    // Удаление принято, а его операция кончается ошибкой: машина осталась.
    rig.fake.control().fail_next_delete_operation = Some((13, "Internal error".to_owned()));
    rig.tick(when + TimeDelta::seconds(1)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.delete_accepted_at, None, "the acceptance is void");
    assert_eq!(window.delete_operation_id, None);
    assert_eq!(window.last_error.as_deref(), Some("delete_failed"));
    assert_eq!(
        window.delete_attempts, 2,
        "the call and its failed operation"
    );
    assert_eq!(rig.fake.instance_count(), 1);
    // Раньше паузы повтора нет.
    rig.tick(when + TimeDelta::seconds(20)).await;
    assert_eq!(rig.deletes(), 1);
    // Повтор снова проверяет принадлежность прямым ответом и удаляет.
    let lookups = rig.fake.control().count("GET", "/compute/v1/instances/");
    rig.tick(when + TimeDelta::minutes(2)).await;
    assert_eq!(rig.deletes(), 2);
    assert!(rig.fake.control().count("GET", "/compute/v1/instances/") > lookups);
    assert_eq!(rig.state(id).await, "ended");
    Ok(())
}

#[tokio::test]
async fn a_boot_disk_that_outlives_the_instance_is_deleted_before_the_window_ends() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, when) = drive_to_deleting(&rig, t0).await;
    let window = rig.window(id).await;
    let instance_id = window.instance_id.clone().unwrap();
    let disk_id = window.boot_disk_id.clone().expect("stored at attach");
    rig.fake.control().leave_disk(&instance_id);
    // Диск пока не удаляется.
    rig.fake.control().disk_delete_status = Some(500);

    let gone = when + TimeDelta::seconds(1);
    rig.tick(gone).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting", "the disk is still there");
    assert_eq!(window.server_gone_at, Some(gone));
    assert_eq!(window.disk_gone_at, None);
    assert_eq!(window.last_error.as_deref(), Some("disk_delete_failed"));
    assert_eq!((rig.deletes(), rig.disk_deletes()), (1, 1));
    assert!(rig.fake.control().disks.contains_key(&disk_id));
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::LiveWindowExists)
    ));
    // Оценка расхода: машина остановилась на своём конце, диск считается дальше.
    let later = gone + TimeDelta::hours(1);
    assert_eq!(
        windows::view(&window, later).cost_so_far_kopecks,
        price::ledger_kopecks(COMPUTE, DISK, t0, gone, later)?
    );

    // Повторы идут с откатом; попытки машины и диска считаются вместе, и после третьей
    // владельцу уходит пуш.
    rig.tick(gone + TimeDelta::seconds(10)).await;
    assert_eq!(rig.disk_deletes(), 1, "not before the backoff");
    rig.tick(gone + TimeDelta::minutes(2)).await;
    rig.tick(gone + TimeDelta::minutes(6)).await;
    assert_eq!(rig.disk_deletes(), 3);
    assert_eq!(rig.alert_codes().await, vec!["disk_delete_failing"]);
    assert_eq!(rig.deletes(), 1, "the instance is not deleted again");

    // Провайдер ожил: диск удалён, его отсутствие подтверждено списком, окно закончилось.
    rig.fake.control().disk_delete_status = None;
    let end = gone + TimeDelta::minutes(20);
    rig.tick(end).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert_eq!(window.disk_gone_at, Some(end));
    assert!(rig.fake.control().disks.is_empty());
    assert_eq!(
        window.final_cost_kopecks,
        Some(price::ledger_kopecks(COMPUTE, DISK, t0, gone, end)?)
    );
    Ok(())
}

#[tokio::test]
async fn a_disk_is_deleted_only_under_the_window_name_and_only_when_detached() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let now = now_micros();
    // Машины окна уже нет, а диск с номером из строки окна носит чужое имя.
    let disk = rig.fake.control().add_stray_disk("someone-elses-data");
    let id = raw_window(
        &rig.pool,
        "deleting",
        Some("fhmfakeinstance9999"),
        Some(now - TimeDelta::minutes(20)),
    )
    .await;
    sqlx::query("update cloud_windows set server_gone_at = $2, boot_disk_id = $3 where id = $1")
        .bind(id)
        .bind(now)
        .bind(&disk)
        .execute(&rig.pool)
        .await?;
    rig.tick(now).await;
    assert_eq!(
        rig.disk_deletes(),
        0,
        "a disk under another name is not ours"
    );
    let window = rig.window(id).await;
    assert_eq!(window.state, "deleting");
    assert_eq!(window.last_error.as_deref(), Some("ownership_mismatch"));
    let alerts = rig.alerts().await;
    assert_eq!(alerts[0]["code"], "ownership_mismatch");
    assert_eq!(alerts[0]["disk_id"], disk);

    // Имя верное, но диск ещё подключён к машине: ждём, не удаляя.
    {
        let mut state = rig.fake.control();
        let entry = state.disks.get_mut(&disk).unwrap();
        entry.name = windows::server_name(id);
        entry.instance_ids = vec!["fhmfakeinstance7777".to_owned()];
    }
    rig.tick(now + TimeDelta::minutes(31)).await;
    assert_eq!(rig.disk_deletes(), 0, "an attached disk is left alone");
    assert_eq!(rig.state(id).await, "deleting");
    // Диск отключился: теперь он удаляется.
    rig.fake
        .control()
        .disks
        .get_mut(&disk)
        .unwrap()
        .instance_ids
        .clear();
    rig.tick(now + TimeDelta::minutes(32)).await;
    assert_eq!(rig.disk_deletes(), 1);
    assert_eq!(rig.state(id).await, "ended");
    Ok(())
}

#[tokio::test]
async fn a_disk_is_adopted_by_its_name_only_when_exactly_one_matches() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    // Без сохранённого номера диск ищется по имени, а имя не доказывает принадлежность: два
    // диска с именем окна — неоднозначность, и удалять первый из них нельзя.
    {
        let rig = rig(pool.clone()).await;
        let now = now_micros();
        let id = raw_window(
            &rig.pool,
            "deleting",
            Some("fhmfakeinstance9999"),
            Some(now - TimeDelta::minutes(20)),
        )
        .await;
        sqlx::query("update cloud_windows set server_gone_at = $2 where id = $1")
            .bind(id)
            .bind(now)
            .execute(&rig.pool)
            .await?;
        let first = rig.fake.control().add_stray_disk(&windows::server_name(id));
        let second = rig.fake.control().add_stray_disk(&windows::server_name(id));
        rig.tick(now).await;
        assert_eq!(rig.disk_deletes(), 0, "two same-name disks are not a match");
        let window = rig.window(id).await;
        assert_eq!(window.state, "deleting");
        assert_eq!(window.boot_disk_id, None);
        assert_eq!(window.last_error.as_deref(), Some("ambiguous_disk"));
        assert!(rig.fake.control().disks.contains_key(&first));
        assert!(rig.fake.control().disks.contains_key(&second));
        // Один из дисков пропал: оставшийся единственный, его номер и запоминается.
        rig.fake.control().disks.remove(&second);
        rig.tick(now + TimeDelta::seconds(10)).await;
        assert_eq!(rig.disk_deletes(), 1);
        assert!(!rig.fake.control().disks.contains_key(&first));
        assert_eq!(rig.state(id).await, "ended");
    }

    // Сохранённый номер главнее имени: диск с другим номером под тем же именем не наш.
    {
        let rig = rig(pool.clone()).await;
        let now = now_micros();
        let other = rig.fake.control().add_stray_disk("placeholder");
        let id = raw_window(
            &rig.pool,
            "deleting",
            Some("fhmfakeinstance9999"),
            Some(now - TimeDelta::minutes(20)),
        )
        .await;
        sqlx::query(
            "update cloud_windows set server_gone_at = $2, boot_disk_id = 'fhmfakedisknotours' where id = $1",
        )
        .bind(id)
        .bind(now)
        .execute(&rig.pool)
        .await?;
        rig.fake.control().disks.get_mut(&other).unwrap().name = windows::server_name(id);
        rig.tick(now).await;
        assert_eq!(
            rig.disk_deletes(),
            0,
            "a name-only disk is not deleted under a stored different id"
        );
        assert!(rig.fake.control().disks.contains_key(&other));
        let window = rig.window(id).await;
        assert_eq!(window.state, "ended");
        assert!(window.disk_gone_at.is_some(), "the stored disk is gone");
        assert_eq!(window.boot_disk_id.as_deref(), Some("fhmfakedisknotours"));
    }
    Ok(())
}

#[tokio::test]
async fn a_name_only_instance_recovery_requires_exactly_one_match() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    // Номера машины нет: снос ищет её по имени. Две машины с именем окна — неоднозначность.
    {
        let rig = rig(pool.clone()).await;
        let now = now_micros();
        let id = raw_window(
            &rig.pool,
            "deleting",
            None,
            Some(now - TimeDelta::minutes(20)),
        )
        .await;
        let first = rig.add_instance(&windows::server_name(id));
        let second = rig.add_instance(&windows::server_name(id));
        rig.tick(now).await;
        assert_eq!(rig.deletes(), 0, "two same-name instances are not a match");
        let window = rig.window(id).await;
        assert_eq!(window.state, "deleting");
        assert_eq!(window.instance_id, None);
        assert_eq!(window.last_error.as_deref(), Some("ambiguous_instance"));
        assert_eq!(rig.fake.instance_count(), 2);
        // Одна машина пропала: оставшаяся — единственная, её номер запоминается и она удаляется.
        rig.fake.control().remove_instance(&second);
        rig.tick(now + TimeDelta::seconds(10)).await;
        assert_eq!(
            rig.window(id).await.instance_id.as_deref(),
            Some(first.as_str())
        );
        rig.tick(now + TimeDelta::seconds(11)).await;
        assert_eq!(rig.deletes(), 1);
        rig.tick(now + TimeDelta::seconds(12)).await;
        assert!(!rig.fake.control().instances.contains_key(&first));
        assert_eq!(rig.state(id).await, "ended");
    }

    // Диск подключён, а машина окна ищется по имени: две машины с этим именем — не повод
    // вернуть снос к любой из них.
    {
        let rig = rig(pool.clone()).await;
        let now = now_micros();
        let id = raw_window(
            &rig.pool,
            "deleting",
            None,
            Some(now - TimeDelta::minutes(20)),
        )
        .await;
        let first = rig.add_instance(&windows::server_name(id));
        let second = rig.add_instance(&windows::server_name(id));
        let second_disk = rig.fake.control().instances[&second].disk_id.clone();
        rig.fake.control().disks.get_mut(&second_disk).unwrap().name = "other-volume".to_owned();
        sqlx::query("update cloud_windows set server_gone_at = $2 where id = $1")
            .bind(id)
            .bind(now)
            .execute(&rig.pool)
            .await?;
        rig.tick(now).await;
        assert_eq!(rig.deletes(), 0);
        let window = rig.window(id).await;
        assert_eq!(window.state, "deleting");
        assert_eq!(
            window.instance_id, None,
            "no same-name instance is chosen while the name is ambiguous"
        );
        assert_eq!(window.last_error.as_deref(), Some("ambiguous_instance"));
        // Осталась одна машина: снос возвращается к ней и убирает её вместе с диском.
        rig.fake.control().remove_instance(&second);
        rig.tick(now + TimeDelta::seconds(10)).await;
        assert_eq!(
            rig.window(id).await.instance_id.as_deref(),
            Some(first.as_str())
        );
        rig.tick(now + TimeDelta::seconds(11)).await;
        assert_eq!(rig.deletes(), 1);
        assert!(!rig.fake.control().instances.contains_key(&first));
        assert_eq!(rig.state(id).await, "ended");
    }
    Ok(())
}

// ───────── Сверка ─────────

fn stray_name() -> String {
    format!("canterlot-runner-{}", Uuid::now_v7())
}

#[tokio::test]
async fn reconcile_deletes_a_stray_instance_and_a_stray_disk_and_touches_nothing_else() -> TestResult
{
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let stray = stray_name();
    let stray_disk_name = stray_name();
    let (ours, other_name, malformed, upper, other_folder) = (
        rig.add_instance(&stray),
        rig.add_instance("web-frontend"),
        rig.add_instance("canterlot-runner-not-a-uuid"),
        rig.add_instance(&format!("canterlot-runner-{}", Uuid::now_v7()).to_uppercase()),
        rig.add_instance(&stray_name()),
    );
    let (lone_disk, data_disk, odd_disk) = {
        let mut state = rig.fake.control();
        state.instances.get_mut(&other_folder).unwrap().folder_id = OTHER_FOLDER.to_owned();
        (
            // Диск с именем окна, которого нет, и без машины: брошенный.
            state.add_stray_disk(&stray_disk_name),
            state.add_stray_disk("data-volume"),
            state.add_stray_disk("canterlot-runner-not-a-uuid"),
        )
    };
    let stray_boot_disk = rig.fake.control().instances[&ours].disk_id.clone();
    rig.tick(now_micros()).await;
    {
        let state = rig.fake.control();
        assert!(!state.instances.contains_key(&ours), "the stray is deleted");
        for kept in [&other_name, &malformed, &upper, &other_folder] {
            assert!(
                state.instances.contains_key(kept),
                "instance {kept} is not ours to touch"
            );
        }
        assert!(
            !state.disks.contains_key(&lone_disk),
            "the stray disk is deleted"
        );
        assert!(
            !state.disks.contains_key(&stray_boot_disk),
            "the boot disk went with its instance"
        );
        for kept in [&data_disk, &odd_disk] {
            assert!(state.disks.contains_key(kept), "disk {kept} is not ours");
        }
        assert_eq!(state.count("DELETE", "/compute/v1/instances"), 1);
        // Диск при машине не удаляется отдельно: подключённый диск не наш к удалению.
        assert_eq!(state.count("DELETE", "/compute/v1/disks"), 1);
    }
    // Владельцу сообщается то, чьё отсутствие подтвердил свежий полный список.
    let mut codes = rig.alert_codes().await;
    codes.sort();
    assert_eq!(codes, vec!["stray_disk_deleted", "stray_server_deleted"]);
    let alerts = rig.alerts().await;
    let disk_alert = alerts
        .iter()
        .find(|alert| alert["code"] == "stray_disk_deleted")
        .unwrap();
    assert_eq!(disk_alert["name"], stray_disk_name);
    assert_eq!(disk_alert["disk_id"], lone_disk);
    Ok(())
}

#[tokio::test]
async fn a_stray_disk_that_cannot_be_deleted_alerts_and_an_attached_one_waits() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let lone = rig.fake.control().add_stray_disk(&stray_name());
    // Диск с именем окна, которого нет, но подключённый к чужой машине: неоднозначно.
    let attached = rig.fake.control().add_stray_disk(&stray_name());
    rig.fake
        .control()
        .disks
        .get_mut(&attached)
        .unwrap()
        .instance_ids = vec!["fhmfakeinstance7777".to_owned()];
    rig.fake.control().disk_delete_status = Some(500);
    let t0 = now_micros();
    rig.tick(t0).await;
    assert_eq!(rig.alert_codes().await, vec!["stray_disk_stuck"]);
    assert_eq!(rig.alerts().await[0]["disk_id"], lone);
    assert_eq!(rig.disk_deletes(), 1, "only the detached one was tried");
    // Провайдер ожил: следующая сверка удаляет диск и сообщает об этом.
    rig.fake.control().disk_delete_status = None;
    rig.tick(t0 + TimeDelta::minutes(5)).await;
    assert!(!rig.fake.control().disks.contains_key(&lone));
    assert!(rig.fake.control().disks.contains_key(&attached));
    assert_eq!(
        rig.alert_codes().await,
        vec!["stray_disk_stuck", "stray_disk_deleted"]
    );
    Ok(())
}

#[tokio::test]
async fn a_late_instance_of_a_failed_window_reopens_it_as_late_cleanup() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    // Окно, которое мы сочли неудавшимся, а машина потом всё же появилась.
    rig.fake.control().create = CreateMode::unavailable();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    rig.tick(t0 + TimeDelta::minutes(10)).await;
    assert_eq!(rig.state(id).await, "failed");
    assert_eq!(
        rig.window(id).await.final_cost_kopecks,
        Some(0),
        "no known instance, no cost"
    );
    // Поздняя машина остаётся видимой в списке и не даёт удалению подтвердиться.
    rig.fake.control().delete_polls = 1_000_000;
    let late = rig.add_instance(&windows::server_name(id));
    rig.tick(t0 + TimeDelta::minutes(15)).await;
    let window = rig.window(id).await;
    assert_eq!(
        window.state, "late_cleanup",
        "reattached to its original window outside the live slot"
    );
    assert_eq!(window.instance_id.as_deref(), Some(late.as_str()));
    assert_eq!(window.delete_reason.as_deref(), Some("late_create"));
    assert_eq!(rig.deletes(), 1, "deleted through the normal path");
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::LateCleanup)
    ));
    // Пока удаление не подтверждено, окно остаётся обязательством, а сверка — не успешной.
    rig.tick(t0 + TimeDelta::minutes(16)).await;
    assert_eq!(rig.state(id).await, "late_cleanup");
    // Машина пропала из каталога: отсутствие подтверждено, итог — от вызова создания.
    rig.fake.control().remove_instance(&late);
    let end = t0 + TimeDelta::minutes(17);
    rig.tick(end).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert_eq!(window.server_gone_at, Some(end));
    assert_eq!(window.final_cost_kopecks, Some(cost(t0, end)));
    Ok(())
}

#[tokio::test]
async fn a_late_disk_reopens_its_closed_window_and_is_billed_to_it() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    // Обычное окно от начала до конца.
    let (id, session, at) = rig.drive_to_ready(60, t0).await;
    windows::request_delete(&rig.pool, id, windows::REASON_OWNER, at).await?;
    drop(session);
    let ended = at + TimeDelta::seconds(1);
    rig.tick(ended).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    let first_cost = window.final_cost_kopecks.unwrap();
    assert_eq!(first_cost, cost(t0, ended));

    // Позже в каталоге находится диск с именем этого окна и без машины: он тарифицируется.
    let disk = rig.fake.control().add_stray_disk(&windows::server_name(id));
    rig.fake.control().disk_delete_status = Some(500);
    let found = t0 + TimeDelta::minutes(10);
    rig.tick(found).await;
    let window = rig.window(id).await;
    assert_eq!(
        window.state, "late_cleanup",
        "the window is reopened, the disk is not a stray"
    );
    assert_eq!(window.delete_reason.as_deref(), Some("late_disk"));
    assert_eq!(window.boot_disk_id.as_deref(), Some(disk.as_str()));
    assert_eq!(window.final_cost_kopecks, None);
    assert_eq!(
        (window.server_gone_at, window.disk_gone_at),
        (Some(ended), None),
        "the instance end is kept, the disk end is open again"
    );
    assert_eq!(rig.alert_codes().await, vec!["late_disk"]);
    assert_eq!(rig.disk_deletes(), 1, "deleted by the normal cleanup step");
    assert_eq!(rig.deletes(), 1, "the instance is not deleted again");
    // Открытый поздний снос считается в расходе по резерву и закрывает новые создания.
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::LateCleanup)
    ));
    let mut conn = rig.pool.acquire().await?;
    assert_eq!(
        windows::spent_in_month(&mut conn, &window.month).await?,
        window.reservation_kopecks
    );
    drop(conn);

    // Диск удалён: окно снова закончено, а итог вырос на время диска.
    rig.fake.control().disk_delete_status = None;
    let end = found + TimeDelta::minutes(5);
    rig.tick(end).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert!(rig.fake.control().disks.is_empty());
    assert_eq!(window.disk_gone_at, Some(end));
    assert_eq!(
        window.final_cost_kopecks,
        Some(price::ledger_kopecks(COMPUTE, DISK, t0, ended, end)?),
        "the same ledger: the instance until its end, the disk until the new one"
    );
    assert!(window.final_cost_kopecks.unwrap() > first_cost);
    assert!(
        !rig.alert_codes()
            .await
            .iter()
            .any(|code| code.starts_with("stray_")),
        "{:?}",
        rig.alert_codes().await
    );
    rig.try_create(60).await.expect("the slot is free again");
    Ok(())
}

#[tokio::test]
async fn a_late_disk_of_a_failed_window_is_cleaned_and_makes_it_cost() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let t0 = now_micros();
    rig.fake.control().create = CreateMode::unavailable();
    let id = rig.create(60).await;
    rig.tick(t0).await;
    let failed_at = t0 + TimeDelta::minutes(10);
    rig.tick(failed_at).await;
    let window = rig.window(id).await;
    assert_eq!(
        (window.state.as_str(), window.final_cost_kopecks),
        ("failed", Some(0))
    );
    // Создание всё же оставило диск.
    rig.fake.control().add_stray_disk(&windows::server_name(id));
    let end = t0 + TimeDelta::minutes(15);
    rig.tick(end).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended", "reopened and cleaned in one pass");
    assert_eq!(window.delete_reason.as_deref(), Some("late_disk"));
    assert!(rig.fake.control().disks.is_empty());
    assert_eq!(
        window.final_cost_kopecks,
        Some(price::ledger_kopecks(COMPUTE, DISK, t0, failed_at, end)?)
    );
    Ok(())
}

#[tokio::test]
async fn a_folder_change_keeps_cleaning_the_old_folder() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let (id, session, at) = rig.drive_to_ready(120, t0).await;
    let window = rig.window(id).await;
    assert_eq!(window.folder_id, FOLDER);
    let instance_id = window.instance_id.clone().unwrap();
    rig.fake.control().leave_disk(&instance_id);
    // В прежнем каталоге есть и чужое с именем окна: после смены каталога оно не наше.
    let foreign = rig.add_instance(&stray_name());

    // Владелец сменил каталог в конфигурации и перезапустил хаб.
    let mut changed = rig.config.clone();
    changed.folder_id = OTHER_FOLDER.to_owned();
    let moved = rig.restarted_with(changed);
    let before = rig.fake.requests().len();
    windows::request_delete(&rig.pool, id, windows::REASON_OWNER, at).await?;
    drop(session);
    let end = at + TimeDelta::seconds(1);
    moved.tick(end).await;

    // Машина и её диск убраны в каталоге окна, а не в новом.
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert_eq!(window.folder_id, FOLDER);
    assert!(!rig.fake.control().instances.contains_key(&instance_id));
    assert_eq!((rig.deletes(), rig.disk_deletes()), (1, 1));
    assert_eq!(
        rig.fake.control().disks.len(),
        1,
        "only the foreign boot disk"
    );
    assert_eq!(window.final_cost_kopecks, Some(cost(t0, end)));
    let listed: Vec<(String, String)> = rig.fake.requests()[before..]
        .iter()
        .filter(|r| r.method == "GET" && r.query.contains_key("folderId"))
        .map(|r| (r.path.clone(), r.query["folderId"].clone()))
        .collect();
    for path in ["/compute/v1/instances", "/compute/v1/disks"] {
        for folder in [FOLDER, OTHER_FOLDER] {
            assert!(
                listed.contains(&(path.to_owned(), folder.to_owned())),
                "{path} of {folder} is listed: {listed:?}"
            );
        }
    }
    // Лишнее ищется только в каталоге из конфигурации: прежний каталог не вычищается.
    assert!(rig.fake.control().instances.contains_key(&foreign));
    assert!(rig.alert_codes().await.is_empty());

    // Незаконченное окно прежнего каталога находит следующая сверка: он остаётся в списке
    // каталогов, пока есть такие окна.
    let orphan = raw_window(
        &rig.pool,
        "create_unknown",
        None,
        Some(end - TimeDelta::minutes(1)),
    )
    .await;
    let late = rig.add_instance(&windows::server_name(orphan));
    moved.tick(end + TimeDelta::minutes(5)).await;
    let window = rig.window(orphan).await;
    assert_eq!(window.state, "identified");
    assert_eq!(window.instance_id.as_deref(), Some(late.as_str()));
    Ok(())
}

#[tokio::test]
async fn a_late_disk_in_a_historical_folder_is_found_after_a_folder_change() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    // Обычное окно прежнего каталога, законченное до смены конфигурации.
    let (id, session, at) = rig.drive_to_ready(60, t0).await;
    windows::request_delete(&rig.pool, id, windows::REASON_OWNER, at).await?;
    drop(session);
    let ended = at + TimeDelta::seconds(1);
    rig.tick(ended).await;
    assert_eq!(rig.state(id).await, "ended");

    // Каталог в конфигурации сменился, хаб перезапущен, а в прежнем каталоге появился диск
    // с именем законченного окна. Его окно уже `ended`, но вызов создания был.
    let mut changed = rig.config.clone();
    changed.folder_id = OTHER_FOLDER.to_owned();
    let moved = rig.restarted_with(changed);
    let disk = rig.fake.control().add_stray_disk(&windows::server_name(id));
    let found = ended + TimeDelta::minutes(5);
    moved.tick(found).await;
    let window = rig.window(id).await;
    assert_eq!(
        window.state, "ended",
        "the historical folder is visited and the disk is cleaned in the same pass"
    );
    assert_eq!(window.delete_reason.as_deref(), Some("late_disk"));
    assert!(!rig.fake.control().disks.contains_key(&disk));
    assert_eq!(rig.alert_codes().await, vec!["late_disk"]);
    Ok(())
}

#[tokio::test]
async fn a_late_cleanup_leaves_the_live_slot_to_the_successor_and_is_billed() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let now = now_micros();
    let called = now - TimeDelta::minutes(30);
    let old = raw_window(&rig.pool, "failed", None, Some(called)).await;
    let late = rig.add_instance(&windows::server_name(old));
    // Преемник уже занял единственное живое место.
    let successor = raw_window(&rig.pool, "ready", None, Some(now - TimeDelta::minutes(5))).await;
    // Удаление принято, но машина ещё видна в списке: обязательство остаётся открытым.
    rig.fake.control().delete_polls = 1_000_000;
    rig.tick(now).await;
    let window = rig.window(old).await;
    assert_eq!(
        window.state, "late_cleanup",
        "the cleanup does not compete for the live slot"
    );
    assert!(window.delete_accepted_at.is_some());
    assert_eq!(rig.deletes(), 1, "deleted through the normal path");
    assert_eq!(
        rig.state(successor).await,
        "ready",
        "the successor keeps its window"
    );
    assert_eq!(rig.alert_codes().await, vec!["late_create"]);
    // Поздний снос считается в расходе по резерву рядом с резервом преемника.
    let mut conn = rig.pool.acquire().await?;
    let spent = windows::spent_in_month(&mut conn, &windows::month_of(now)).await?;
    drop(conn);
    assert_eq!(spent, 2 * 3 * HOURLY, "both reservations are spent");
    // Машина пропала из полного списка: окно заканчивается итогом от вызова создания.
    rig.fake.control().remove_instance(&late);
    let end = now + TimeDelta::minutes(1);
    rig.tick(end).await;
    let window = rig.window(old).await;
    assert_eq!(window.state, "ended");
    assert!(window.server_gone_at.is_some());
    assert_eq!(
        window.final_cost_kopecks,
        Some(cost(called, end)),
        "the cost runs from the create call to the confirmed absence"
    );
    assert_eq!(rig.state(successor).await, "ready");
    Ok(())
}

#[tokio::test]
async fn an_open_late_cleanup_blocks_new_creates() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let now = now_micros();
    let old = raw_window(
        &rig.pool,
        "failed",
        None,
        Some(now - TimeDelta::minutes(30)),
    )
    .await;
    rig.add_instance(&windows::server_name(old));
    rig.fake.control().delete_polls = 1_000_000;
    rig.tick(now).await;
    assert_eq!(rig.state(old).await, "late_cleanup");
    // Живого окна нет, но машина старого окна ещё оплачивается: новая не создаётся.
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::LateCleanup)
    ));
    Ok(())
}

#[tokio::test]
async fn a_stray_that_cannot_be_deleted_alerts_every_half_hour() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.add_instance(&stray_name());
    rig.fake.control().delete_status = Some(500);
    let t0 = now_micros();
    rig.tick(t0).await;
    assert_eq!(rig.alert_codes().await, vec!["stray_server_stuck"]);
    // Новая сверка в тот же момент (слот тот же): второго пуша нет.
    let again = rig.restarted();
    again.tick(t0).await;
    assert_eq!(rig.alerts().await.len(), 1);
    assert_eq!(rig.deletes(), 2, "the stray is retried by every reconcile");
    // Через 40 минут слот другой: пуш повторяется.
    again.tick(t0 + TimeDelta::minutes(40)).await;
    assert_eq!(rig.alerts().await.len(), 2);
    Ok(())
}

#[tokio::test]
async fn a_stray_is_deleted_only_when_the_direct_get_shows_its_own_name() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    rig.add_instance(&stray_name());
    // Номер в списке, но прямой ответ называет другую машину: номер мог достаться чужой.
    rig.fake.control().get_name_override = Some("someone-else".to_owned());
    let t0 = now_micros();
    rig.tick(t0).await;
    assert_eq!(rig.deletes(), 0, "a mismatched name is never deleted");
    assert_eq!(rig.fake.instance_count(), 1);
    assert!(
        rig.alert_codes()
            .await
            .contains(&"stray_server_stuck".to_owned()),
        "{:?}",
        rig.alert_codes().await
    );
    // Провайдер отдаёт настоящее имя: лишняя машина удаляется.
    rig.fake.control().get_name_override = None;
    rig.tick(t0 + TimeDelta::minutes(40)).await;
    assert_eq!(rig.fake.instance_count(), 0);
    assert_eq!(rig.deletes(), 1);
    Ok(())
}

#[tokio::test]
async fn a_folder_listing_failure_does_not_stop_another_folder() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let now = now_micros();
    // Окно из старой конфигурации: его каталог — не текущий.
    let id = raw_window(
        &rig.pool,
        "create_unknown",
        None,
        Some(now - TimeDelta::minutes(1)),
    )
    .await;
    let instance = rig.add_instance(&windows::server_name(id));
    rig.fake
        .control()
        .instances
        .get_mut(&instance)
        .unwrap()
        .folder_id = OTHER_FOLDER.to_owned();
    sqlx::query("update cloud_windows set folder_id = $2 where id = $1")
        .bind(id)
        .bind(OTHER_FOLDER)
        .execute(&rig.pool)
        .await?;
    // Список текущего каталога отвечает ошибкой: это не должно мешать окну другого.
    rig.fake.control().list_fail_folder = Some(FOLDER.to_owned());
    rig.tick(now).await;
    let window = rig.window(id).await;
    assert_eq!(
        window.state, "identified",
        "the other folder's window is confirmed by its own full listing"
    );
    assert_eq!(window.instance_id, Some(instance));
    // Но сверка в целом не удалась: новых созданий нет.
    sqlx::query("update cloud_windows set state = 'ended', final_cost_kopecks = 0")
        .execute(&rig.pool)
        .await?;
    let fresh = rig.create(60).await;
    rig.tick(now + TimeDelta::seconds(10)).await;
    assert_eq!(rig.state(fresh).await, "create_intent");
    assert_eq!(rig.posts(), 0);
    Ok(())
}

#[tokio::test]
async fn reconcile_runs_at_start_before_any_create() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    rig.add_instance(&stray_name());
    let id = rig.create(60).await;
    rig.tick(now_micros()).await;
    assert_eq!(rig.state(id).await, "identified");
    let requests = rig.fake.requests();
    let first = |method: &str, path: &str| {
        requests
            .iter()
            .position(|r| r.method == method && r.path == path)
    };
    let first_listing = first("GET", "/compute/v1/instances").expect("an instance listing");
    let first_disks = first("GET", "/compute/v1/disks").expect("a disk listing");
    let first_post = first("POST", "/compute/v1/instances").expect("a create");
    let first_delete = requests
        .iter()
        .position(|r| r.method == "DELETE")
        .expect("the stray delete");
    assert!(
        first_listing < first_post && first_disks < first_post,
        "both listings come before any create"
    );
    assert!(
        first_delete < first_post,
        "the stray goes before any create"
    );
    Ok(())
}

#[tokio::test]
async fn without_a_successful_reconcile_nothing_is_created() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    rig.fake.control().list_fail_page = Some(FailPage {
        list: "disks",
        page: 0,
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

#[tokio::test]
async fn a_create_call_interrupted_by_a_crash_is_never_repeated() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    ready_row(&rig).await;
    let now = now_micros();
    // Метка вызова есть, ответа нет, машины в списке нет: звонок мог дойти.
    let orphan = raw_window(&rig.pool, "create_intent", None, Some(now)).await;
    let service = rig.restarted();
    for seconds in [0, 10, 20, 400] {
        service.tick(now + TimeDelta::seconds(seconds)).await;
    }
    assert_eq!(rig.state(orphan).await, "create_unknown");
    assert_eq!(
        rig.posts(),
        0,
        "no POST at all: the window already had its call"
    );
    // Окно ожидания вышло, полные списки пусты: окно закрыто, звонка так и не было.
    service.tick(now + TimeDelta::minutes(12)).await;
    let window = rig.window(orphan).await;
    assert_eq!(
        (window.state.as_str(), window.final_cost_kopecks),
        ("failed", Some(0))
    );
    assert_eq!(rig.posts(), 0);
    Ok(())
}

#[tokio::test]
async fn the_cost_is_per_second_and_each_rate_stops_at_its_own_end() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let now = now_micros();
    let called = now - TimeDelta::hours(5);
    let id = raw_window(
        &rig.pool,
        "deleting",
        Some("fhmfakeinstance9999"),
        Some(called),
    )
    .await;
    // Машина подтверждённо пропала два часа назад, а её диск ещё в каталоге.
    let disk = rig.fake.control().add_stray_disk(&windows::server_name(id));
    sqlx::query("update cloud_windows set server_gone_at = $2, boot_disk_id = $3 where id = $1")
        .bind(id)
        .bind(now - TimeDelta::hours(2))
        .bind(&disk)
        .execute(&rig.pool)
        .await?;
    let window = rig.window(id).await;
    assert_eq!(window.final_cost_kopecks, None, "the window is still live");
    let view = windows::view(&window, now);
    assert_eq!(
        view.cost_so_far_kopecks,
        3 * COMPUTE + 5 * DISK,
        "the machine stopped costing when it was gone, the disk still costs"
    );
    assert_eq!(
        (view.compute_kopecks_per_hour, view.disk_kopecks_per_hour),
        (COMPUTE, DISK)
    );
    assert_eq!(view.price_hourly_kopecks, HOURLY);
    assert_eq!(view.disk_gone_at, None);
    // Полтора часа назад живое окно стоило меньше: оценка растёт по секундам.
    assert!(
        windows::view(&window, now - TimeDelta::minutes(90)).cost_so_far_kopecks
            < 3 * COMPUTE + 5 * DISK
    );

    // Диск удалён через 90 секунд после этого: итог считается до его конца, без округления
    // до часа и без минимума.
    let end = now + TimeDelta::seconds(90);
    rig.tick(end).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "ended");
    assert_eq!(window.disk_gone_at, Some(end));
    // Диск: 5 ч и 90 с по 160 копеек = 800 + 4 ровно.
    assert_eq!(window.final_cost_kopecks, Some(3 * COMPUTE + 5 * DISK + 4));
    assert_eq!(
        windows::view(&window, end + TimeDelta::hours(9)).cost_so_far_kopecks,
        3 * COMPUTE + 5 * DISK + 4,
        "a finished window costs its final amount"
    );

    // Короткое окно: 90 секунд машины и 150 секунд диска, каждая часть вверх до копейки.
    close_window(&rig.pool, id).await?;
    let called = end;
    let short = raw_window(
        &rig.pool,
        "deleting",
        Some("fhmfakeinstance9998"),
        Some(called),
    )
    .await;
    let disk = rig
        .fake
        .control()
        .add_stray_disk(&windows::server_name(short));
    sqlx::query("update cloud_windows set server_gone_at = $2, boot_disk_id = $3 where id = $1")
        .bind(short)
        .bind(called + TimeDelta::seconds(90))
        .bind(&disk)
        .execute(&rig.pool)
        .await?;
    rig.tick(called + TimeDelta::seconds(150)).await;
    let window = rig.window(short).await;
    assert_eq!(window.state, "ended");
    // 1646×90/3600 = 41.15 → 42; 160×150/3600 = 6.67 → 7.
    assert_eq!(window.final_cost_kopecks, Some(42 + 7));
    Ok(())
}

/// Закрыть окно, чтобы следующая строка теста могла занять единственное живое место.
async fn close_window(pool: &PgPool, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("update cloud_windows set state = 'ended', final_cost_kopecks = 0 where id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Строка окна в нужном состоянии, как её оставил бы упавший хаб.
async fn raw_window(
    pool: &PgPool,
    state: &str,
    instance_id: Option<&str>,
    called: Option<DateTime<Utc>>,
) -> Uuid {
    let id = Uuid::now_v7();
    let hard_cap = called.map(|at| at + TimeDelta::minutes(60));
    sqlx::query(
        "insert into cloud_windows (id, name, state, trigger, folder_id, preemptible, instance_id,
                                    host_key_public, compute_kopecks_per_hour,
                                    disk_kopecks_per_hour, price_hourly_kopecks,
                                    reservation_kopecks, month,
                                    hard_cap_minutes, created_at, create_called_at, hard_cap_at,
                                    drain_until, teardown_until, attach_sent_at,
                                    delete_requested_at, delete_accepted_at, profiles)
         values ($1, $2, $3, 'owner', $4, true, $5, 'ssh-ed25519 AAAA', $6, $7, $8, $9, $10, 60,
                 $11, $12, $13,
                 $13 + interval '30 minutes', $13 + interval '60 minutes',
                 case when $3 in ('ready', 'draining') then $12 else null end,
                 case when $3 = 'deleting' then $12 else null end,
                 case when $3 = 'deleting' then $12 else null end,
                 '[{\"cores\":32,\"memory_gb\":64,\"disk_gb\":80,\"compute_kopecks_per_hour\":1646,\"disk_kopecks_per_hour\":160}]'::jsonb)",
    )
    .bind(id)
    .bind(windows::server_name(id))
    .bind(state)
    .bind(FOLDER)
    .bind(instance_id)
    .bind(COMPUTE)
    .bind(DISK)
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

#[tokio::test]
async fn a_hub_restart_resumes_windows_in_every_state() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    ready_row(&rig).await;
    let now = now_micros();
    let restart = || rig.restarted();
    let close = |id: Uuid| {
        let pool = rig.pool.clone();
        async move {
            close_window(&pool, id).await.unwrap();
        }
    };
    /// Машина с именем окна, которая уже была у провайдера до перезапуска.
    fn rename(rig: &Rig, instance: &str, id: Uuid) {
        rig.fake.control().instances.get_mut(instance).unwrap().name = windows::server_name(id);
        let disk = rig.fake.control().instances[instance].disk_id.clone();
        rig.fake.control().disks.get_mut(&disk).unwrap().name = windows::server_name(id);
    }

    // create_intent без вызова: после сверки создаётся.
    let intent = raw_window(&rig.pool, "create_intent", None, None).await;
    restart().tick(now).await;
    assert_eq!(rig.state(intent).await, "identified");
    assert_eq!(rig.posts(), 1);
    close(intent).await;
    rig.vanish_instances();

    // create_intent с меткой вызова: звонок мог быть, второго не будет; машина по имени
    // найдена сверкой.
    let orphan = raw_window(&rig.pool, "create_intent", None, Some(now)).await;
    let instance = rig.add_instance(&windows::server_name(orphan));
    restart().tick(now + TimeDelta::seconds(5)).await;
    let window = rig.window(orphan).await;
    assert_eq!(window.state, "identified");
    assert_eq!(window.instance_id.as_deref(), Some(instance.as_str()));
    assert_eq!(rig.posts(), 1, "no second POST for an orphaned create");
    close(orphan).await;
    rig.vanish_instances();

    // create_unknown, машины нет и окно вышло: снос подтверждает пустые списки, failed.
    let unknown = raw_window(
        &rig.pool,
        "create_unknown",
        None,
        Some(now - TimeDelta::minutes(30)),
    )
    .await;
    restart().tick(now + TimeDelta::seconds(10)).await;
    assert_eq!(rig.state(unknown).await, "failed");

    // identified, ready и draining с живой машиной остаются.
    for state in ["identified", "ready", "draining"] {
        // Подключение ждём 15 минут: `identified` моложе этого срока.
        let called = now - TimeDelta::minutes(if state == "identified" { 5 } else { 20 });
        let instance = rig.add_instance("x");
        let id = raw_window(&rig.pool, state, Some(&instance), Some(called)).await;
        rename(&rig, &instance, id);
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
            "{state} with a live instance stays"
        );
        if state == "identified" {
            // Машина работает: после перезапуска окно сразу шлёт команду подключения.
            let window = rig.window(id).await;
            assert!(window.attach_sent_at.is_some());
            assert!(window.public_ip.is_some());
        }
        close(id).await;
        rig.vanish_instances();
    }

    // deleting с принятым удалением, машина уже пропала из списка: конец оплаты подтверждён.
    let called = now - TimeDelta::minutes(20);
    let deleting = raw_window(
        &rig.pool,
        "deleting",
        Some("fhmfakeinstance0777"),
        Some(called),
    )
    .await;
    let end = now + TimeDelta::seconds(30);
    restart().tick(end).await;
    let window = rig.window(deleting).await;
    assert_eq!(window.state, "ended");
    assert_eq!(window.final_cost_kopecks, Some(cost(called, end)));

    // deleting, машина есть: после перезапуска она удаляется, а за ней оставшийся диск.
    let instance = rig.add_instance("x");
    let with_disk = raw_window(
        &rig.pool,
        "deleting",
        Some(&instance),
        Some(now - TimeDelta::minutes(20)),
    )
    .await;
    rename(&rig, &instance, with_disk);
    rig.fake.control().leave_disk(&instance);
    sqlx::query("update cloud_windows set delete_accepted_at = null where id = $1")
        .bind(with_disk)
        .execute(&rig.pool)
        .await?;
    restart().tick(now + TimeDelta::seconds(35)).await;
    assert_eq!(rig.state(with_disk).await, "ended");
    assert_eq!(rig.fake.instance_count(), 0);
    assert!(rig.fake.control().disks.is_empty());

    // ready, но машины нет в полном списке и срок вышел: снос подтверждает диск, gone.
    let vanished = raw_window(
        &rig.pool,
        "ready",
        Some("fhmfakeinstance0888"),
        Some(now - TimeDelta::minutes(20)),
    )
    .await;
    restart().tick(now + TimeDelta::seconds(40)).await;
    assert_eq!(rig.state(vanished).await, "gone");
    Ok(())
}

// ───────── Строка раннера `cloud` ─────────

#[tokio::test]
async fn the_cloud_row_is_reused_reactivated_on_attach_and_revoked_at_the_end() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn static_runners_keep_their_token_flags() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn idle_counts_running_queued_and_takeable_waiting_jobs() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn idle_ignores_work_the_cloud_runner_cannot_take() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn the_cloud_row_is_placement_kind_cloud() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn a_disconnected_runner_is_deleted_after_ten_minutes_only_while_the_carrier_is_connected()
-> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn a_disconnected_runner_with_a_disconnected_carrier_is_not_deleted() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn the_runner_that_never_connects_is_deleted_after_the_ready_timeout() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn a_machine_that_is_not_a_carrier_gets_no_attach_command() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let t0 = now_micros();
    // Окно создаётся при живой несущей, а теряет её до подключения.
    let id = rig.create(120).await;
    rig.tick(t0).await;
    machines::set_carrier(&rig.pool, rig.carrier, false).await?;
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

#[tokio::test]
async fn delete_now_cancels_an_unsent_create_and_revokes_a_running_window() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn startup_enables_the_service_only_with_a_readable_key_and_a_valid_table() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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
    let key_dir = dir.path().join("secret-key-dir");
    std::fs::create_dir(&key_dir)?;
    let key_file = write_key_file(&key_dir);
    let mut config = rig.config.clone();
    config.key_file = key_file;
    // Служба создаётся без единого запроса в сеть: токен берётся при первом вызове.
    assert!(
        from_config(&rig.pool, &registry, Some(config.clone()))
            .await
            .is_some()
    );

    // Файла ключа нет, или в нём не авторизованный ключ.
    let mut missing = config.clone();
    missing.key_file = dir.path().join("absent");
    assert!(
        from_config(&rig.pool, &registry, Some(missing))
            .await
            .is_none()
    );
    let not_a_key = dir.path().join("not-a-key");
    std::fs::write(&not_a_key, "{\"id\": \"x\"}\n")?;
    let mut broken = config.clone();
    broken.key_file = not_a_key;
    assert!(
        from_config(&rig.pool, &registry, Some(broken))
            .await
            .is_none()
    );

    // Машина из конфигурации не несущая: служба всё равно нужна для сноса и сверки,
    // несущая проверяется только при создании нового окна.
    machines::set_carrier(&rig.pool, rig.carrier, false).await?;
    assert!(
        from_config(&rig.pool, &registry, Some(config.clone()))
            .await
            .is_some()
    );
    let mut stranger = config.clone();
    stranger.carrier_machine_id = Uuid::now_v7();
    assert!(
        from_config(&rig.pool, &registry, Some(stranger))
            .await
            .is_some()
    );
    // Негодная конфигурация.
    let mut invalid = config;
    invalid.profile[0].cores = 0;
    assert!(
        from_config(&rig.pool, &registry, Some(invalid))
            .await
            .is_none()
    );

    let logs = String::from_utf8(capture.0.lock().unwrap().clone())?;
    assert_eq!(logs.matches("cloud_disabled").count(), 3, "{logs}");
    assert!(logs.contains("cloud_enabled") || logs.contains("cloud runner windows enabled"));
    for secret in [private_key_sample().as_str(), "secret-key-dir", KEY] {
        assert!(
            !logs.contains(secret),
            "{secret} leaked into the startup log"
        );
    }
    Ok(())
}

/// Кусок закрытого ключа сервисного аккаунта: его не должно быть нигде, кроме файла.
fn private_key_sample() -> String {
    test_key()
        .private_pem
        .lines()
        .nth(1)
        .expect("the key body")
        .chars()
        .take(40)
        .collect()
}

#[tokio::test]
async fn without_a_carrier_only_new_creates_stop_and_cleanup_still_runs() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    machines::set_carrier(&rig.pool, rig.carrier, false).await?;
    assert!(matches!(
        rig.try_create(60).await,
        Err(CreateRefusal::CarrierUnavailable)
    ));
    assert_eq!(rig.posts(), 0, "no instance for a window without a carrier");

    // Незаконченное удаление старого окна идёт до конца без несущей.
    let now = now_micros();
    let instance = rig.add_instance("x");
    let id = raw_window(
        &rig.pool,
        "deleting",
        Some(&instance),
        Some(now - TimeDelta::minutes(20)),
    )
    .await;
    rig.fake
        .control()
        .instances
        .get_mut(&instance)
        .unwrap()
        .name = windows::server_name(id);
    rig.tick(now).await;
    rig.tick(now + TimeDelta::seconds(40)).await;
    assert_eq!(rig.state(id).await, "ended");
    assert_eq!(
        rig.fake.instance_count(),
        0,
        "the outstanding instance is deleted"
    );
    Ok(())
}

#[tokio::test]
async fn a_restart_keeps_the_terms_approved_at_create() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let id = rig.create(60).await;
    let mut changed = rig.config.clone();
    changed.drain_grace = Duration::from_secs(5 * 60);
    changed.teardown_margin = Duration::from_secs(5 * 60);
    // И вид машины в конфигурации сменился: окно создаёт ту, что была одобрена.
    changed.preemptible = false;
    let restarted = rig.restarted_with(changed);
    restarted.tick(t0).await;
    assert_eq!(rig.state(id).await, "identified");
    let window = rig.window(id).await;
    assert_eq!(window.hard_cap_at, Some(t0 + TimeDelta::minutes(60)));
    assert_eq!(window.drain_until, Some(t0 + TimeDelta::minutes(90)));
    assert_eq!(window.teardown_until, Some(t0 + TimeDelta::minutes(120)));
    assert!(window.preemptible);
    assert_eq!(
        (
            window.compute_kopecks_per_hour,
            window.disk_kopecks_per_hour
        ),
        (COMPUTE, DISK)
    );
    let instance = &rig.fake.control().instances[window.instance_id.as_deref().unwrap()];
    assert!(instance.preemptible, "the snapshot decides, not the config");
    Ok(())
}

/// Цена ядра прерываемой машины в поддельном каталоге становится такой.
fn set_vcpu_price(rig: &Rig, price: &str) {
    rig.fake.control().sku_mut(SKU_PREEMPTIBLE_VCPU).versions =
        json!([street_price("2025-12-31T21:00:00Z", price)]);
}

#[tokio::test]
async fn a_stale_quote_that_would_grow_the_reservation_cancels_the_create() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    enable_pushes(&rig.pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let id = rig.create(60).await;
    let approved = rig.window(id).await.reservation_kopecks;
    // Через срок годности цены провайдер подорожал: тратить больше одобренного нельзя.
    set_vcpu_price(&rig, "0.9");
    rig.restarted().tick(t0 + TimeDelta::minutes(11)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "failed");
    assert_eq!(window.final_cost_kopecks, Some(0));
    assert_eq!(window.reservation_kopecks, approved);
    assert_eq!(window.create_called_at, None, "failed before any call");
    assert_eq!(rig.posts(), 0, "no POST over the approved amount");
    assert!(
        rig.alert_codes()
            .await
            .contains(&"reprice_growth".to_owned()),
        "{:?}",
        rig.alert_codes().await
    );
    Ok(())
}

#[tokio::test]
async fn a_stale_quote_is_replaced_by_the_new_lower_price() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let rig = rig(pool).await;
    let _carrier = rig.carrier_session();
    let t0 = now_micros();
    let id = rig.create(60).await;
    let approved = rig.window(id).await.reservation_kopecks;
    set_vcpu_price(&rig, "0.1");
    rig.restarted().tick(t0 + TimeDelta::minutes(11)).await;
    let window = rig.window(id).await;
    assert_eq!(window.state, "identified");
    assert_eq!(rig.posts(), 1);
    assert!(
        window.reservation_kopecks < approved,
        "the approved ceiling falls with the price"
    );
    // 32×0.1 + 64×0.083 + 0.26352 = 8.77552 ₽ → 878.
    assert_eq!(window.profiles()[0].compute_kopecks_per_hour, 878);
    assert_eq!(
        (
            window.compute_kopecks_per_hour,
            window.disk_kopecks_per_hour
        ),
        (878, DISK),
        "the ledger uses the price of the call"
    );
    assert_eq!(window.reservation_kopecks, 3 * (878 + DISK));
    Ok(())
}

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

#[tokio::test]
async fn neither_the_account_key_nor_the_token_nor_the_host_key_reach_the_logs() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
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
    // Путь с отказами по всем веткам: создание, список, удаление машины и диска.
    let (id, when) = drive_to_deleting(&rig, t0).await;
    let instance_id = rig.window(id).await.instance_id.unwrap();
    rig.fake.control().leave_disk(&instance_id);
    rig.fake.control().delete_status = Some(500);
    rig.tick(when + TimeDelta::seconds(1)).await;
    rig.fake.control().delete_status = Some(403);
    rig.tick(when + TimeDelta::minutes(40)).await;
    rig.fake.control().list_fail_page = Some(FailPage {
        list: "instances",
        page: 0,
        status: 500,
        times: None,
    });
    rig.tick(when + TimeDelta::minutes(50)).await;
    {
        let mut state = rig.fake.control();
        state.list_fail_page = None;
        state.delete_status = None;
        state.disk_delete_status = Some(500);
    }
    rig.tick(when + TimeDelta::minutes(70)).await;
    let window = rig.window(id).await;
    assert_eq!(window.last_error.as_deref(), Some("disk_delete_failed"));
    let logs = std::str::from_utf8(&capture.0.lock().unwrap())
        .unwrap()
        .to_owned();
    assert!(!logs.is_empty(), "the capture works");
    let key = private_key_sample();
    for secret in [
        key.as_str(),
        "t1.fake-iam-token",
        "BEGIN OPENSSH PRIVATE KEY",
        "OPENSSH PRIVATE",
        "PRIVATE KEY",
    ] {
        assert!(!logs.contains(secret), "{secret} leaked into the logs");
    }
    let shown = format!("{window:?}{:?}{:?}", rig.config, rig.alerts().await);
    assert!(
        !shown.contains(&key)
            && !shown.contains("t1.fake-iam-token")
            && !shown.contains("PRIVATE KEY")
    );
    Ok(())
}
