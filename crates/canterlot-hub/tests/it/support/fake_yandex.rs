//! Поддельный Yandex Cloud для тестов клиента и жизненного цикла облачных окон.
//!
//! Один сервер отвечает за все четыре API: IAM (`/iam/v1/tokens`), Compute
//! (`/compute/v1/…`), операции (`/operations/{id}`) и цены (`/billing/v1/skus`). Формы
//! ответов взяты из ответов живого API; все идентификаторы выдуманы.
//!
//! Ручки сбоев лежат в `FakeState` и меняются прямо в тесте через `fake.control()`.
//!
//! Сервер поднимает свой цикл приёма соединений: так обрыв «принял и закрыл сокет» делается
//! по-настоящему, а не ответом с кодом.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use axum::Extension;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::service::TowerToHyperService;
use ring::signature::{RSA_PSS_2048_8192_SHA256, RsaKeyPair, UnparsedPublicKey};
use serde_json::{Value, json};
use tokio::sync::Notify;

pub const FOLDER: &str = "b1gfakefolder000000";
pub const SUBNET: &str = "e9bfakesubnet000000";
pub const ZONE: &str = "ru-central1-a";
pub const KEY_ID: &str = "ajefakekeyid0000001";
pub const SERVICE_ACCOUNT: &str = "ajefakeserviceacc01";
pub const IMAGE_ID: &str = "fd8fakeimage0000001";
/// Получатель, которого настоящий IAM требует в JWT.
pub const JWT_AUDIENCE: &str = "https://iam.api.cloud.yandex.net/iam/v1/tokens";

/// Номера SKU публичного каталога Яндекса: одни и те же для всех аккаунтов.
pub const SKU_REGULAR_VCPU: &str = "dn2k3vqlk9snp1jv351u";
pub const SKU_REGULAR_RAM: &str = "dn2ilq72mjc3bej6j74p";
pub const SKU_PREEMPTIBLE_VCPU: &str = "dn2e2fphfupugm21k4hv";
pub const SKU_PREEMPTIBLE_RAM: &str = "dn26ur5frjbgdek2a0g5";
pub const SKU_SSD: &str = "dn27ajm6m8mnfcshbi61";
pub const SKU_PUBLIC_IP: &str = "dn229q5mnmp58t58tfel";

/// Одноразовый ключ RSA на весь прогон тестов: создаётся `openssl` один раз.
pub struct TestKey {
    /// Закрытый ключ, PKCS#8 в PEM.
    pub private_pem: String,
    /// Открытый ключ в DER (`RSAPublicKey`): им сервер проверяет подпись JWT.
    pub public_der: Vec<u8>,
}

pub fn test_key() -> &'static TestKey {
    static KEY: OnceLock<TestKey> = OnceLock::new();
    KEY.get_or_init(|| {
        let output = std::process::Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
            ])
            .output()
            .expect("openssl is needed to make a throwaway test key");
        assert!(output.status.success(), "openssl genpkey failed");
        let private_pem = String::from_utf8(output.stdout).unwrap();
        let body: String = private_pem
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        let der = STANDARD.decode(body).unwrap();
        let pair = RsaKeyPair::from_pkcs8(&der).unwrap();
        TestKey {
            public_der: pair.public().as_ref().to_vec(),
            private_pem,
        }
    })
}

/// Записать файл авторизованного ключа в том виде, в каком его выдаёт Яндекс: со строкой
/// перед PEM-блоком.
pub fn write_key_file(dir: &Path) -> PathBuf {
    let path = dir.join("yandex-key.json");
    let private_key = format!(
        "PLEASE DO NOT REMOVE THIS LINE! Yandex.Cloud SA Key ID <{KEY_ID}>\n{}",
        test_key().private_pem
    );
    let body = json!({
        "id": KEY_ID,
        "service_account_id": SERVICE_ACCOUNT,
        "created_at": "2026-11-23T20:00:00.000000000Z",
        "key_algorithm": "RSA_2048",
        "public_key": "-----BEGIN PUBLIC KEY-----\n(not used)\n-----END PUBLIC KEY-----\n",
        "private_key": private_key,
    });
    std::fs::write(&path, body.to_string()).unwrap();
    path
}

/// Что делает создание машины.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateMode {
    /// 200 с операцией, машина записана.
    Accept,
    /// Машина принята и записана, соединение закрыто без ответа.
    AcceptThenDrop,
    /// Машина принята и записана, ответа нет вовсе: клиент упирается в таймаут.
    AcceptThenHang,
    /// Ответ-ошибка в форме `google.rpc.Status`, машина не записана.
    Error {
        http: u16,
        code: i32,
        message: String,
    },
    /// 200 с телом, которое не JSON; машина записана.
    Garbage,
}

impl CreateMode {
    /// 409 с кодом 6, как при занятом имени.
    pub fn name_exists() -> Self {
        Self::Error {
            http: 409,
            code: 6,
            message: "Instance with name \"taken\" already exists".to_owned(),
        }
    }

    /// 429 с кодом 8: квота исчерпана.
    pub fn quota() -> Self {
        Self::Error {
            http: 429,
            code: 8,
            message: "Quota limit compute.instanceCores.count exceeded".to_owned(),
        }
    }

    /// 400 с кодом 9: в зоне нет ресурсов.
    pub fn not_enough_resources() -> Self {
        Self::Error {
            http: 400,
            code: 9,
            message: "Not enough resources in zone ru-central1-a".to_owned(),
        }
    }

    /// 503 с кодом 14.
    pub fn unavailable() -> Self {
        Self::Error {
            http: 503,
            code: 14,
            message: "Service unavailable".to_owned(),
        }
    }
}

/// Отказ одной страницы списка.
#[derive(Debug, Clone)]
pub struct FailPage {
    /// Ключ списка: `instances`, `disks` или `skus`.
    pub list: &'static str,
    /// Номер страницы с нуля.
    pub page: usize,
    pub status: u16,
    /// Сколько раз отказать; `None` — каждый раз.
    pub times: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct FakeInstance {
    pub id: String,
    pub name: String,
    pub folder_id: String,
    pub labels: Value,
    pub memory: String,
    pub cores: String,
    pub preemptible: bool,
    pub disk_id: String,
    /// Номер для адресов.
    pub serial: u32,
    /// Сколько ещё опросов (машины или её операции создания) ответят `PROVISIONING`.
    pub boot_polls_left: u32,
    /// Остановлена: прервана Яндексом.
    pub stopped: bool,
    /// Подменить статус, что бы ни происходило.
    pub forced_status: Option<String>,
    /// Операция удаления, если оно начато.
    pub delete_operation: Option<String>,
    /// Сколько ещё наблюдений (машины, операции удаления, списка) машина видна как
    /// `DELETING`.
    pub delete_polls_left: u32,
    /// Не удалять загрузочный диск вместе с машиной: останется брошенный диск.
    pub leave_disk: bool,
    /// Список каталога машину не показывает, а прямой запрос по номеру показывает.
    pub hidden_in_listing: bool,
    /// Тело запроса создания.
    pub request: Value,
}

impl FakeInstance {
    pub fn status(&self) -> &str {
        if let Some(forced) = &self.forced_status {
            forced
        } else if self.delete_operation.is_some() {
            "DELETING"
        } else if self.stopped {
            "STOPPED"
        } else if self.boot_polls_left > 0 {
            "PROVISIONING"
        } else {
            "RUNNING"
        }
    }

    pub fn public_ip(&self) -> Option<String> {
        (self.status() == "RUNNING" || self.status() == "DELETING")
            .then(|| format!("203.0.113.{}", self.serial % 250 + 1))
    }
}

#[derive(Debug, Clone)]
pub struct FakeDisk {
    pub id: String,
    pub name: String,
    pub size: String,
    pub status: String,
    pub instance_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationKind {
    CreateInstance,
    DeleteInstance,
    DeleteDisk,
}

#[derive(Debug, Clone)]
pub struct FakeOperation {
    pub id: String,
    pub kind: OperationKind,
    /// Машина или диск операции.
    pub target: String,
    /// Операция завершится ошибкой (`google.rpc.Status`).
    pub error: Option<(i32, String)>,
    /// Сколько ещё опросов операция с ошибкой отвечает `done: false`: идёт откат.
    pub rollback_polls_left: u32,
    pub done: bool,
}

#[derive(Debug, Clone)]
pub struct FakeSku {
    pub id: String,
    pub name: String,
    pub unit: String,
    /// Версии цены как есть: `pricingVersions` ответа.
    pub versions: Value,
}

/// Одна розничная версия цены с одной ставкой в рублях.
pub fn street_price(effective: &str, price: &str) -> Value {
    json!({
        "type": "STREET_PRICE",
        "effectiveTime": effective,
        "pricingExpressions": [{
            "rates": [{"startPricingQuantity": "0", "unitPrice": price, "currency": "RUB"}],
        }],
    })
}

impl FakeSku {
    /// SKU, как в настоящем каталоге: старая версия цены, действующая и объявленная на
    /// будущее. Старая и будущая заметно дороже: взятая по ошибке, любая из них видна в сумме.
    pub fn new(id: &str, name: &str, unit: &str, price: &str) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            unit: unit.to_owned(),
            versions: json!([
                street_price("2023-01-01T00:00:00Z", "500"),
                street_price("2025-12-31T21:00:00Z", price),
                street_price("2099-01-01T00:00:00Z", "900"),
            ]),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub body: Option<Value>,
    pub authorization: Option<String>,
}

/// Ворота одного ответа поддельного провайдера: сначала `waiting`, затем тест шлёт `release`.
/// Так тест знает, что проход дошёл до запроса, и отпускает ответ после своего отсчёта времени.
pub struct ResponseGate {
    pub waiting: Notify,
    pub release: Notify,
}

impl ResponseGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            waiting: Notify::new(),
            release: Notify::new(),
        })
    }
}

pub struct FakeState {
    /// Каталог облака. Запрос с другим каталогом в создании отвергается, а список другого
    /// каталога пуст.
    pub folder_id: String,
    /// Токены, которые сервер сейчас принимает.
    pub valid_tokens: HashSet<String>,
    pub tokens_issued: u32,
    /// Не принимать никакой токен, даже только что выданный.
    pub refuse_all_tokens: bool,
    /// Код, которым отвечает обмен JWT на токен; `None` — выдаёт токен.
    pub iam_status: Option<u16>,
    pub instances: BTreeMap<String, FakeInstance>,
    pub disks: BTreeMap<String, FakeDisk>,
    pub operations: BTreeMap<String, FakeOperation>,
    pub next_serial: u32,
    pub create: CreateMode,
    /// Исход создания по числу ядер запроса (`resourcesSpec.cores`): сильнее `create`.
    pub create_by_cores: HashMap<String, CreateMode>,
    /// Удаление машины отвечает этим кодом и ничего не удаляет.
    pub delete_status: Option<u16>,
    /// Операция следующего принятого удаления машины завершится этой ошибкой, а машина
    /// останется.
    pub fail_next_delete_operation: Option<(i32, String)>,
    /// Удаление диска отвечает этим кодом и ничего не удаляет.
    pub disk_delete_status: Option<u16>,
    /// Столько следующих списков машин (по первой странице) приходят пустыми.
    pub listing_hidden_polls: u32,
    /// Списки машин и дисков этого каталога отвечают 500.
    pub list_fail_folder: Option<String>,
    /// Прямой запрос машины называет это имя вместо настоящего.
    pub get_name_override: Option<String>,
    /// Операция следующего принятого создания завершится этой ошибкой.
    pub fail_next_create_operation: Option<(i32, String)>,
    /// Машина неудавшегося создания остаётся в каталоге (откат не случился).
    pub failed_create_keeps_instance: bool,
    /// Неудавшееся создание столько опросов операции отвечает `done: false` уже с `error`
    /// (Яндекс откатывает), и только потом `done: true`.
    pub failed_create_rollback_polls: u32,
    /// Новая машина отвечает `PROVISIONING` на первые столько опросов, потом становится
    /// `RUNNING` с публичным адресом.
    pub boot_polls: u32,
    /// Удаляемая машина видна как `DELETING` столько наблюдений, потом пропадает.
    pub delete_polls: u32,
    /// Образы общего каталога: семейство → номер образа.
    pub images: BTreeMap<String, String>,
    pub skus: Vec<FakeSku>,
    /// Коды, которыми отвечают подряд на любые следующие запросы к API (очередь).
    pub fail_next: VecDeque<u16>,
    /// На сколько следующих запросов к API не отвечать вовсе: клиент упирается в таймаут.
    pub hang_next: u32,
    /// Сколько следующих запросов отвечать 429.
    pub rate_limit_next: u32,
    /// Значение `Retry-After` в ответах 429.
    pub retry_after: Option<String>,
    /// Не больше стольких записей на странице списка машин и дисков, как бы ни просили.
    pub list_page_cap: Option<usize>,
    /// Ворота одного ответа: медленный провайдер, которого отпускает тест.
    pub gate: Option<Arc<ResponseGate>>,
    /// То же для каталога SKU.
    pub sku_page_cap: Option<usize>,
    pub list_fail_page: Option<FailPage>,
    /// Отдавать один и тот же `nextPageToken`: обход зацикливается.
    pub repeat_page_token: bool,
    /// Подмешивать в тексты ошибок заголовок Authorization, как делает неряшливый сервер.
    pub echo_authorization_in_errors: bool,
    pub requests: Vec<RecordedRequest>,
}

impl FakeState {
    /// Пустой каталог и ручки по умолчанию: чистое облако между случаями табличного теста.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    fn new() -> Self {
        Self {
            folder_id: FOLDER.to_owned(),
            valid_tokens: HashSet::new(),
            tokens_issued: 0,
            refuse_all_tokens: false,
            iam_status: None,
            instances: BTreeMap::new(),
            disks: BTreeMap::new(),
            operations: BTreeMap::new(),
            next_serial: 1,
            create: CreateMode::Accept,
            create_by_cores: HashMap::new(),
            delete_status: None,
            fail_next_delete_operation: None,
            disk_delete_status: None,
            listing_hidden_polls: 0,
            list_fail_folder: None,
            get_name_override: None,
            fail_next_create_operation: None,
            failed_create_keeps_instance: false,
            failed_create_rollback_polls: 0,
            boot_polls: 0,
            delete_polls: 1,
            images: BTreeMap::from([("debian-13".to_owned(), IMAGE_ID.to_owned())]),
            skus: default_skus(),
            fail_next: VecDeque::new(),
            hang_next: 0,
            rate_limit_next: 0,
            retry_after: None,
            list_page_cap: None,
            gate: None,
            sku_page_cap: None,
            list_fail_page: None,
            repeat_page_token: false,
            echo_authorization_in_errors: false,
            requests: Vec::new(),
        }
    }

    fn serial(&mut self) -> u32 {
        let serial = self.next_serial;
        self.next_serial += 1;
        serial
    }

    fn new_operation(&mut self, kind: OperationKind, target: &str) -> String {
        let id = format!("fhmfakeoperation{:04}", self.serial());
        self.operations.insert(
            id.clone(),
            FakeOperation {
                id: id.clone(),
                kind,
                target: target.to_owned(),
                error: None,
                rollback_polls_left: 0,
                done: false,
            },
        );
        id
    }

    /// Положить машину с загрузочным диском напрямую, минуя запрос создания. Машина сразу
    /// `RUNNING`.
    pub fn add_instance(&mut self, name: &str) -> String {
        let request = json!({
            "name": name,
            "labels": {"canterlot": "runner"},
            "resourcesSpec": {"memory": "2147483648", "cores": "2", "coreFraction": "100"},
            "bootDiskSpec": {"autoDelete": true, "diskSpec": {"name": name, "size": "10737418240"}},
            "schedulingPolicy": {"preemptible": true},
        });
        self.insert_instance(&request, 0)
    }

    fn insert_instance(&mut self, request: &Value, boot_polls: u32) -> String {
        let serial = self.serial();
        let id = format!("fhmfakeinstance{serial:04}");
        let disk_id = format!("fhmfakedisk{serial:08}");
        let disk_spec = &request["bootDiskSpec"]["diskSpec"];
        self.disks.insert(
            disk_id.clone(),
            FakeDisk {
                id: disk_id.clone(),
                name: disk_spec["name"].as_str().unwrap_or_default().to_owned(),
                size: disk_spec["size"].as_str().unwrap_or("0").to_owned(),
                status: "READY".to_owned(),
                instance_ids: vec![id.clone()],
            },
        );
        self.instances.insert(
            id.clone(),
            FakeInstance {
                id: id.clone(),
                name: request["name"].as_str().unwrap_or_default().to_owned(),
                folder_id: self.folder_id.clone(),
                labels: request["labels"].clone(),
                memory: request["resourcesSpec"]["memory"]
                    .as_str()
                    .unwrap_or("0")
                    .to_owned(),
                cores: request["resourcesSpec"]["cores"]
                    .as_str()
                    .unwrap_or("0")
                    .to_owned(),
                preemptible: request["schedulingPolicy"]["preemptible"]
                    .as_bool()
                    .unwrap_or(false),
                disk_id,
                serial,
                boot_polls_left: boot_polls,
                stopped: false,
                forced_status: None,
                delete_operation: None,
                delete_polls_left: 0,
                leave_disk: false,
                hidden_in_listing: false,
                request: request.clone(),
            },
        );
        id
    }

    /// Положить диск без машины: брошенный.
    pub fn add_stray_disk(&mut self, name: &str) -> String {
        let id = format!("fhmfakedisk{:08}", self.serial());
        self.disks.insert(
            id.clone(),
            FakeDisk {
                id: id.clone(),
                name: name.to_owned(),
                size: "10737418240".to_owned(),
                status: "READY".to_owned(),
                instance_ids: Vec::new(),
            },
        );
        id
    }

    /// Яндекс прервал машину: она `STOPPED` и без публичного адреса.
    pub fn preempt(&mut self, instance_id: &str) {
        self.instances
            .get_mut(instance_id)
            .expect("no such instance")
            .stopped = true;
    }

    /// После удаления этой машины её загрузочный диск останется в каталоге ничьим.
    pub fn leave_disk(&mut self, instance_id: &str) {
        self.instances
            .get_mut(instance_id)
            .expect("no such instance")
            .leave_disk = true;
    }

    /// Все выданные токены перестают приниматься: следующий запрос получит 401, а новый
    /// токен из IAM снова годен.
    pub fn expire_tokens(&mut self) {
        self.valid_tokens.clear();
    }

    /// Убрать машину и её диск, как это делает законченное удаление.
    pub fn remove_instance(&mut self, instance_id: &str) {
        let Some(instance) = self.instances.remove(instance_id) else {
            return;
        };
        if instance.leave_disk {
            if let Some(disk) = self.disks.get_mut(&instance.disk_id) {
                disk.instance_ids.clear();
            }
        } else {
            self.disks.remove(&instance.disk_id);
        }
        if let Some(operation) = instance
            .delete_operation
            .as_ref()
            .and_then(|id| self.operations.get_mut(id))
        {
            operation.done = true;
        }
    }

    /// Одно наблюдение за удаляемой машиной: на последнем она пропадает.
    fn observe_deletion(&mut self, instance_id: &str) {
        let Some(instance) = self.instances.get_mut(instance_id) else {
            return;
        };
        if instance.delete_operation.is_none() {
            return;
        }
        instance.delete_polls_left = instance.delete_polls_left.saturating_sub(1);
        if instance.delete_polls_left == 0 {
            self.remove_instance(instance_id);
        }
    }

    /// Одно наблюдение за загрузкой машины.
    fn observe_boot(&mut self, instance_id: &str) {
        if let Some(instance) = self.instances.get_mut(instance_id) {
            instance.boot_polls_left = instance.boot_polls_left.saturating_sub(1);
        }
    }

    pub fn count(&self, method: &str, path_prefix: &str) -> usize {
        self.requests
            .iter()
            .filter(|r| r.method == method && r.path.starts_with(path_prefix))
            .count()
    }

    pub fn sku_mut(&mut self, id: &str) -> &mut FakeSku {
        self.skus
            .iter_mut()
            .find(|sku| sku.id == id)
            .expect("no such SKU")
    }

    pub fn remove_sku(&mut self, id: &str) {
        self.skus.retain(|sku| sku.id != id);
    }
}

/// Каталог по умолчанию: нужные SKU с ценами 24.11.2026 и соседи с теми же именами, но
/// другими номерами и заметно другими ценами. В именах прерываемых машин, как в настоящем
/// каталоге, стоит неразрывный пробел: сопоставление по имени их не находит, а совпадение
/// по имени вместо номера даёт неверную сумму.
fn default_skus() -> Vec<FakeSku> {
    let core = "core*hour";
    let gb = "gbyte*hour";
    let ip = "fip*hour";
    let preemptible_vcpu = "Preemptible VM\u{a0}computing resources, Intel Ice Lake, 100% vCPU";
    let preemptible_ram = "Preemptible VM\u{a0}computing resources, Intel Ice Lake, RAM";
    let regular_vcpu = "Regular VM computing resources, Intel Ice Lake, 100% vCPU";
    let regular_ram = "Regular VM computing resources, Intel Ice Lake, RAM";
    let ssd = "Fast network drive (SSD)";
    let public_ip = "Public IP address (dynamic or static)";
    vec![
        FakeSku::new("dn2fakesku0000000001", preemptible_vcpu, core, "70"),
        FakeSku::new(SKU_PREEMPTIBLE_VCPU, preemptible_vcpu, core, "0.34"),
        FakeSku::new("dn2fakesku0000000002", regular_vcpu, core, "80"),
        FakeSku::new(SKU_PREEMPTIBLE_RAM, preemptible_ram, gb, "0.083"),
        FakeSku::new(SKU_REGULAR_VCPU, regular_vcpu, core, "1.24"),
        FakeSku::new("dn2fakesku0000000003", preemptible_ram, gb, "60"),
        FakeSku::new(SKU_REGULAR_RAM, regular_ram, gb, "0.33"),
        FakeSku::new(
            "dn2fakesku0000000004",
            "Standard network drive (HDD)",
            gb,
            "50",
        ),
        FakeSku::new(SKU_SSD, ssd, gb, "0.0199"),
        FakeSku::new("dn2fakesku0000000005", ssd, gb, "40"),
        FakeSku::new(SKU_PUBLIC_IP, public_ip, ip, "0.26352"),
        FakeSku::new("dn2fakesku0000000006", public_ip, ip, "30"),
    ]
}

pub struct FakeYandex {
    pub base_url: String,
    state: Arc<Mutex<FakeState>>,
    stop: Arc<Notify>,
}

impl Drop for FakeYandex {
    fn drop(&mut self) {
        self.stop.notify_waiters();
        self.stop.notify_one();
    }
}

/// Сигнал «закрой это соединение»: у каждого соединения свой.
#[derive(Clone)]
struct Kill(Arc<Notify>);

enum Action {
    Respond(Response),
    /// Закрыть соединение, не ответив.
    Drop,
    /// Никогда не отвечать.
    Hang,
}

impl FakeYandex {
    pub async fn start() -> Self {
        let state = Arc::new(Mutex::new(FakeState::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(Notify::new());

        let app = {
            let state = state.clone();
            Router::new().fallback(move |Extension(kill): Extension<Kill>, request: Request| {
                let state = state.clone();
                async move { dispatch(state, kill, request).await }
            })
        };

        let stopper = stop.clone();
        tokio::spawn(async move {
            loop {
                let stream = tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => stream,
                        Err(_) => continue,
                    },
                    () = stopper.notified() => return,
                };
                let kill = Kill(Arc::new(Notify::new()));
                let service = TowerToHyperService::new(app.clone().layer(Extension(kill.clone())));
                let stopper = stopper.clone();
                tokio::spawn(async move {
                    let builder =
                        hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                    let connection = builder.serve_connection(TokioIo::new(stream), service);
                    tokio::select! {
                        _ = connection => {}
                        () = kill.0.notified() => {}
                        () = stopper.notified() => {}
                    }
                });
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            state,
            stop,
        }
    }

    /// Ручки сбоев и содержимое поддельного облака.
    pub fn control(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.control().requests.clone()
    }

    pub fn instance_count(&self) -> usize {
        self.control().instances.len()
    }
}

fn status_body(code: i32, message: &str) -> Value {
    json!({
        "code": code,
        "message": message,
        "details": [{
            "@type": "type.googleapis.com/google.rpc.RequestInfo",
            "requestId": "00000000-0000-4000-8000-000000000000",
        }],
    })
}

fn json_response(status: u16, body: &Value) -> Response {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap())
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Код gRPC, которым REST Яндекса сопровождает статус HTTP.
fn grpc_for(status: u16) -> i32 {
    match status {
        400 => 3,
        401 => 16,
        403 => 7,
        404 => 5,
        409 => 10,
        429 => 8,
        503 => 14,
        504 => 4,
        _ => 13,
    }
}

fn error_response(
    st: &FakeState,
    status: u16,
    code: i32,
    message: &str,
    auth: Option<&str>,
) -> Response {
    let message = match (st.echo_authorization_in_errors, auth) {
        (true, Some(auth)) => format!("{message} (authorization: {auth})"),
        _ => message.to_owned(),
    };
    json_response(status, &status_body(code, &message))
}

fn instance_json(instance: &FakeInstance) -> Value {
    let mut address = json!({"address": format!("192.0.2.{}", instance.serial % 250 + 1)});
    if let Some(public) = instance.public_ip() {
        address["oneToOneNat"] = json!({"address": public, "ipVersion": "IPV4"});
    }
    json!({
        "id": instance.id,
        "folderId": instance.folder_id,
        "createdAt": "2026-11-23T23:24:00Z",
        "name": instance.name,
        "labels": instance.labels,
        "zoneId": ZONE,
        "platformId": "standard-v3",
        "resources": {"memory": instance.memory, "cores": instance.cores, "coreFraction": "100"},
        "status": instance.status(),
        "bootDisk": {
            "mode": "READ_WRITE",
            "deviceName": instance.disk_id,
            "autoDelete": !instance.leave_disk,
            "diskId": instance.disk_id,
        },
        "networkInterfaces": [{
            "index": "0",
            "macAddress": "d0:0d:00:00:00:01",
            "subnetId": SUBNET,
            "primaryV4Address": address,
        }],
        "fqdn": format!("{}.auto.internal", instance.id),
        "schedulingPolicy": {"preemptible": instance.preemptible},
        "networkSettings": {"type": "STANDARD"},
        "placementPolicy": {},
    })
}

fn disk_json(st: &FakeState, disk: &FakeDisk) -> Value {
    json!({
        "id": disk.id,
        "folderId": st.folder_id,
        "createdAt": "2026-11-23T23:24:11Z",
        "name": disk.name,
        "typeId": "network-ssd",
        "zoneId": ZONE,
        "size": disk.size,
        "blockSize": "4096",
        "status": disk.status,
        "sourceImageId": IMAGE_ID,
        "instanceIds": disk.instance_ids,
        "diskPlacementPolicy": {},
    })
}

fn operation_json(st: &FakeState, operation: &FakeOperation) -> Value {
    let (description, metadata_type, id_field) = match operation.kind {
        OperationKind::CreateInstance => {
            ("Create instance", "CreateInstanceMetadata", "instanceId")
        }
        OperationKind::DeleteInstance => {
            ("Delete instance", "DeleteInstanceMetadata", "instanceId")
        }
        OperationKind::DeleteDisk => ("Delete disk", "DeleteDiskMetadata", "diskId"),
    };
    let mut out = json!({
        "id": operation.id,
        "description": description,
        "createdAt": "2026-11-23T23:24:01Z",
        "createdBy": SERVICE_ACCOUNT,
        "modifiedAt": "2026-11-23T23:24:01Z",
        "done": operation.done,
        "metadata": {
            "@type": format!("type.googleapis.com/yandex.cloud.compute.v1.{metadata_type}"),
            id_field: operation.target,
        },
    });
    // Ошибка может стоять в операции раньше `done`: откат ещё идёт.
    if let Some((code, message)) = &operation.error {
        out["error"] = json!({"code": code, "message": message, "details": []});
        return out;
    }
    if !operation.done {
        return out;
    }
    if operation.kind == OperationKind::CreateInstance {
        let mut response = st
            .instances
            .get(&operation.target)
            .map(instance_json)
            .unwrap_or_else(|| json!({}));
        response["@type"] = json!("type.googleapis.com/yandex.cloud.compute.v1.Instance");
        out["response"] = response;
    } else {
        out["response"] = json!({"@type": "type.googleapis.com/google.protobuf.Empty"});
    }
    out
}

fn parse_query(raw: Option<&str>) -> HashMap<String, String> {
    raw.unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (key.to_owned(), percent_decode(value))
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 3 <= bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn dispatch(state: Arc<Mutex<FakeState>>, kill: Kill, request: Request) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let query = parse_query(request.uri().query());
    let auth = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = to_bytes(request.into_body(), 8 << 20)
        .await
        .unwrap_or_default();
    let body: Option<Value> = serde_json::from_slice(&bytes).ok();

    let action = {
        let mut st = state.lock().unwrap();
        st.requests.push(RecordedRequest {
            method: method.to_string(),
            path: path.clone(),
            query: query.clone(),
            body: body.clone(),
            authorization: auth.clone(),
        });
        let action = route(&mut st, &method, &path, &query, body, auth.as_deref());
        // Ворота снимаются первым же запросом: тест отпускает ответ, когда сам решит.
        (action, st.gate.clone())
    };

    if let Some(gate) = action.1 {
        {
            let mut st = state.lock().unwrap();
            st.gate = None;
        }
        gate.waiting.notify_one();
        gate.release.notified().await;
    }

    match action.0 {
        Action::Respond(response) => response,
        Action::Drop => {
            kill.0.notify_one();
            std::future::pending().await
        }
        Action::Hang => std::future::pending().await,
    }
}

/// Проверка JWT так, как её делает IAM: заголовок, срок, издатель, получатель и подпись
/// PS256 открытым ключом.
fn check_jwt(jwt: &str) -> Result<(), &'static str> {
    let parts: Vec<&str> = jwt.split('.').collect();
    let [header, claims, signature] = parts.as_slice() else {
        return Err("JWT must have three parts");
    };
    let decode = |part: &str| -> Option<Value> {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).ok()?).ok()
    };
    let header_json = decode(header).ok_or("JWT header is not base64url JSON")?;
    let claims_json = decode(claims).ok_or("JWT claims are not base64url JSON")?;
    if header_json["alg"] != "PS256" {
        return Err("JWT alg must be PS256");
    }
    if header_json["kid"] != KEY_ID {
        return Err("JWT kid is not a known key");
    }
    if claims_json["iss"] != SERVICE_ACCOUNT {
        return Err("JWT iss is not the service account");
    }
    if claims_json["aud"] != JWT_AUDIENCE {
        return Err("JWT aud is wrong");
    }
    let (Some(iat), Some(exp)) = (claims_json["iat"].as_i64(), claims_json["exp"].as_i64()) else {
        return Err("JWT has no iat or exp");
    };
    if exp <= iat || exp - iat > 3600 {
        return Err("JWT lifetime must be within an hour");
    }
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| "JWT signature is not base64url")?;
    let signed = format!("{header}.{claims}");
    UnparsedPublicKey::new(&RSA_PSS_2048_8192_SHA256, &test_key().public_der)
        .verify(signed.as_bytes(), &signature)
        .map_err(|_| "JWT signature does not verify")
}

fn issue_token(st: &mut FakeState, body: Option<&Value>) -> Response {
    if let Some(status) = st.iam_status {
        return json_response(status, &status_body(grpc_for(status), "IAM is not well"));
    }
    let jwt = body
        .and_then(|body| body["jwt"].as_str())
        .unwrap_or_default();
    if let Err(why) = check_jwt(jwt) {
        return json_response(401, &status_body(16, why));
    }
    st.tokens_issued += 1;
    let token = format!("t1.fake-iam-token-{:04}-5f2c9a71d3", st.tokens_issued);
    st.valid_tokens.insert(token.clone());
    json_response(
        200,
        &json!({"iamToken": token, "expiresAt": "2026-11-24T22:00:00.000000000Z"}),
    )
}

/// Страница списка. Токен страницы непрозрачен для клиента: `p<номер>-<смещение>`.
fn page(
    st: &mut FakeState,
    key: &'static str,
    items: Vec<Value>,
    query: &HashMap<String, String>,
    auth: Option<&str>,
) -> Action {
    let (index, offset) = query
        .get("pageToken")
        .and_then(|token| token.strip_prefix('p'))
        .and_then(|token| token.split_once('-'))
        .and_then(|(index, offset)| Some((index.parse().ok()?, offset.parse().ok()?)))
        .unwrap_or((0usize, 0usize));
    if let Some(fail) = st.list_fail_page.as_mut()
        && fail.list == key
        && fail.page == index
        && fail.times != Some(0)
    {
        if let Some(times) = fail.times.as_mut() {
            *times -= 1;
        }
        let status = fail.status;
        return Action::Respond(error_response(
            st,
            status,
            grpc_for(status),
            "Injected page failure",
            auth,
        ));
    }
    let cap = if key == "skus" {
        st.sku_page_cap
    } else {
        st.list_page_cap
    };
    let size = query
        .get("pageSize")
        .and_then(|size| size.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(1, 1000)
        .min(cap.unwrap_or(usize::MAX));
    let chunk: Vec<Value> = items.iter().skip(offset).take(size).cloned().collect();
    let next_offset = offset + chunk.len();
    let mut body = json!({});
    if !chunk.is_empty() {
        body[key] = Value::Array(chunk);
    }
    if st.repeat_page_token {
        body["nextPageToken"] = json!("p1-0");
    } else if next_offset < items.len() {
        body["nextPageToken"] = json!(format!("p{}-{next_offset}", index + 1));
    }
    Action::Respond(json_response(200, &body))
}

/// Проверка тела создания так, как её делает API: int64 строками, имя по образцу.
fn check_create(st: &FakeState, body: &Value) -> Result<(), String> {
    if body["folderId"] != st.folder_id.as_str() {
        return Err("Illegal argument folderId".to_owned());
    }
    let name = body["name"].as_str().unwrap_or_default();
    let name_ok = !name.is_empty()
        && name.len() <= 63
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && !name.ends_with(['-', '_'])
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !name_ok {
        return Err("Illegal argument name".to_owned());
    }
    for (what, value) in [
        ("resourcesSpec.memory", &body["resourcesSpec"]["memory"]),
        ("resourcesSpec.cores", &body["resourcesSpec"]["cores"]),
        (
            "resourcesSpec.coreFraction",
            &body["resourcesSpec"]["coreFraction"],
        ),
        (
            "bootDiskSpec.diskSpec.size",
            &body["bootDiskSpec"]["diskSpec"]["size"],
        ),
    ] {
        if value
            .as_str()
            .and_then(|text| text.parse::<u64>().ok())
            .is_none()
        {
            return Err(format!("Illegal argument {what}"));
        }
    }
    if body["zoneId"].as_str().is_none_or(str::is_empty) {
        return Err("Illegal argument zoneId".to_owned());
    }
    if body["networkInterfaceSpecs"][0]["subnetId"]
        .as_str()
        .is_none_or(str::is_empty)
    {
        return Err("Illegal argument networkInterfaceSpecs".to_owned());
    }
    Ok(())
}

fn create_instance(st: &mut FakeState, body: Option<Value>, auth: Option<&str>) -> Action {
    let body = body.unwrap_or(Value::Null);
    let mode = body["resourcesSpec"]["cores"]
        .as_str()
        .and_then(|cores| st.create_by_cores.get(cores))
        .unwrap_or(&st.create)
        .clone();
    if let CreateMode::Error {
        http,
        code,
        message,
    } = &mode
    {
        return Action::Respond(error_response(st, *http, *code, message, auth));
    }
    if let Err(message) = check_create(st, &body) {
        return Action::Respond(error_response(st, 400, 3, &message, auth));
    }
    let name = body["name"].as_str().unwrap_or_default().to_owned();
    // Имя уникально в каталоге: второе создание с тем же именем не даёт вторую машину.
    if st.instances.values().any(|instance| instance.name == name) {
        let message = format!("Instance with name \"{name}\" already exists");
        return Action::Respond(error_response(st, 409, 6, &message, auth));
    }
    let boot_polls = st.boot_polls;
    let instance_id = st.insert_instance(&body, boot_polls);
    let operation_id = st.new_operation(OperationKind::CreateInstance, &instance_id);
    if let Some(error) = st.fail_next_create_operation.take() {
        let rollback_polls = st.failed_create_rollback_polls;
        let operation = st.operations.get_mut(&operation_id).expect("just made");
        operation.error = Some(error);
        operation.rollback_polls_left = rollback_polls;
        operation.done = rollback_polls == 0;
        if !st.failed_create_keeps_instance {
            st.remove_instance(&instance_id);
        }
    }
    match mode {
        CreateMode::AcceptThenDrop => Action::Drop,
        CreateMode::AcceptThenHang => Action::Hang,
        CreateMode::Garbage => Action::Respond(
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::from("<html>upstream said something odd</html>"))
                .unwrap(),
        ),
        CreateMode::Accept | CreateMode::Error { .. } => {
            // Ответ создания — операция, ещё не завершённая, с номером машины в метаданных.
            let mut pending = st.operations[&operation_id].clone();
            pending.done = false;
            pending.error = None;
            Action::Respond(json_response(200, &operation_json(st, &pending)))
        }
    }
}

fn route(
    st: &mut FakeState,
    method: &Method,
    path: &str,
    query: &HashMap<String, String>,
    body: Option<Value>,
    auth: Option<&str>,
) -> Action {
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    if let ("POST", ["iam", "v1", "tokens"]) = (method.as_str(), segments.as_slice()) {
        return Action::Respond(issue_token(st, body.as_ref()));
    }

    let token_ok = auth
        .and_then(|auth| auth.strip_prefix("Bearer "))
        .is_some_and(|token| st.valid_tokens.contains(token));
    if !token_ok || st.refuse_all_tokens {
        return Action::Respond(error_response(
            st,
            401,
            16,
            "The token is invalid or has expired",
            auth,
        ));
    }
    if st.hang_next > 0 {
        st.hang_next -= 1;
        return Action::Hang;
    }
    if st.rate_limit_next > 0 {
        st.rate_limit_next -= 1;
        let mut response = error_response(st, 429, 8, "Too many requests", auth);
        if let Some(after) = &st.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_str(after).unwrap());
        }
        return Action::Respond(response);
    }
    if let Some(status) = st.fail_next.pop_front() {
        return Action::Respond(error_response(
            st,
            status,
            grpc_for(status),
            "Injected failure",
            auth,
        ));
    }

    // Каталог списка: без него запрос негоден, а в чужом каталоге пусто.
    let folder = query.get("folderId").filter(|folder| !folder.is_empty());
    match (method.as_str(), segments.as_slice()) {
        ("POST", ["compute", "v1", "instances"]) => create_instance(st, body, auth),

        ("GET", ["compute", "v1", "instances"]) => {
            let Some(folder) = folder else {
                return Action::Respond(error_response(
                    st,
                    400,
                    3,
                    "Illegal argument folderId",
                    auth,
                ));
            };
            if st.list_fail_folder.as_ref() == Some(folder) {
                return Action::Respond(error_response(st, 500, 13, "Injected failure", auth));
            }
            if !query.contains_key("pageToken") && st.listing_hidden_polls > 0 {
                st.listing_hidden_polls -= 1;
                return Action::Respond(json_response(200, &json!({})));
            }
            // Листинг (его первая страница) — наблюдение за удаляемыми машинами.
            if !query.contains_key("pageToken") {
                let deleting: Vec<String> = st
                    .instances
                    .values()
                    .filter(|instance| instance.delete_operation.is_some())
                    .map(|instance| instance.id.clone())
                    .collect();
                for id in deleting {
                    st.observe_deletion(&id);
                }
            }
            let items: Vec<Value> = st
                .instances
                .values()
                .filter(|instance| &instance.folder_id == folder && !instance.hidden_in_listing)
                .map(instance_json)
                .collect();
            page(st, "instances", items, query, auth)
        }

        ("GET", ["compute", "v1", "instances", id]) => {
            st.observe_boot(id);
            st.observe_deletion(id);
            match st.instances.get(*id) {
                Some(instance) => {
                    let mut body = instance_json(instance);
                    if let Some(name) = &st.get_name_override {
                        body["name"] = json!(name);
                    }
                    Action::Respond(json_response(200, &body))
                }
                None => {
                    let message = format!("The instance \"{id}\" not found");
                    Action::Respond(error_response(st, 404, 5, &message, auth))
                }
            }
        }

        ("DELETE", ["compute", "v1", "instances", id]) => {
            let Some(instance) = st.instances.get(*id) else {
                let message = format!("The instance \"{id}\" not found");
                return Action::Respond(error_response(st, 404, 5, &message, auth));
            };
            if let Some(status) = st.delete_status {
                return Action::Respond(error_response(
                    st,
                    status,
                    grpc_for(status),
                    "Injected delete failure",
                    auth,
                ));
            }
            if let Some(error) = st.fail_next_delete_operation.take() {
                let operation_id = st.new_operation(OperationKind::DeleteInstance, id);
                let answer = operation_json(st, &st.operations[&operation_id]);
                let operation = st.operations.get_mut(&operation_id).expect("just made");
                operation.error = Some(error);
                operation.done = true;
                return Action::Respond(json_response(200, &answer));
            }
            // Повтор, пока удаление идёт, возвращает ту же операцию.
            let operation_id = match instance.delete_operation.clone() {
                Some(existing) => existing,
                None => {
                    let operation_id = st.new_operation(OperationKind::DeleteInstance, id);
                    let polls = st.delete_polls;
                    let instance = st.instances.get_mut(*id).expect("checked above");
                    instance.delete_operation = Some(operation_id.clone());
                    instance.delete_polls_left = polls;
                    operation_id
                }
            };
            let answer = operation_json(st, &st.operations[&operation_id]);
            if st.delete_polls == 0 {
                st.remove_instance(id);
            }
            Action::Respond(json_response(200, &answer))
        }

        ("GET", ["operations", id]) => {
            let Some(operation) = st.operations.get(*id).cloned() else {
                let message = format!("Operation \"{id}\" not found");
                return Action::Respond(error_response(st, 404, 5, &message, auth));
            };
            match operation.kind {
                OperationKind::CreateInstance if operation.error.is_some() => {
                    let operation = st.operations.get_mut(*id).expect("checked above");
                    if operation.rollback_polls_left == 0 {
                        operation.done = true;
                    } else {
                        operation.rollback_polls_left -= 1;
                    }
                }
                OperationKind::CreateInstance => {
                    st.observe_boot(&operation.target);
                    let running = st
                        .instances
                        .get(&operation.target)
                        .is_some_and(|instance| instance.boot_polls_left == 0);
                    if running && let Some(operation) = st.operations.get_mut(*id) {
                        operation.done = true;
                    }
                }
                OperationKind::DeleteInstance => st.observe_deletion(&operation.target),
                OperationKind::DeleteDisk => {}
            }
            let answer = operation_json(st, &st.operations[*id]);
            Action::Respond(json_response(200, &answer))
        }

        ("GET", ["compute", "v1", "disks"]) => {
            let Some(folder) = folder else {
                return Action::Respond(error_response(
                    st,
                    400,
                    3,
                    "Illegal argument folderId",
                    auth,
                ));
            };
            if st.list_fail_folder.as_ref() == Some(folder) {
                return Action::Respond(error_response(st, 500, 13, "Injected failure", auth));
            }
            let items: Vec<Value> = st
                .disks
                .values()
                .filter(|_| folder == &st.folder_id)
                .map(|disk| disk_json(st, disk))
                .collect();
            page(st, "disks", items, query, auth)
        }

        ("DELETE", ["compute", "v1", "disks", id]) => {
            let Some(disk) = st.disks.get(*id) else {
                let message = format!("Disk {id} not found");
                return Action::Respond(error_response(st, 404, 5, &message, auth));
            };
            if !disk.instance_ids.is_empty() {
                let message = format!("Disk {id} is attached to an instance");
                return Action::Respond(error_response(st, 400, 9, &message, auth));
            }
            if let Some(status) = st.disk_delete_status {
                return Action::Respond(error_response(
                    st,
                    status,
                    grpc_for(status),
                    "Injected delete failure",
                    auth,
                ));
            }
            st.disks.remove(*id);
            let operation_id = st.new_operation(OperationKind::DeleteDisk, id);
            let answer = operation_json(st, &st.operations[&operation_id]);
            st.operations
                .get_mut(&operation_id)
                .expect("just made")
                .done = true;
            Action::Respond(json_response(200, &answer))
        }

        ("GET", ["compute", "v1", "images:latestByFamily"]) => {
            let family = query.get("family").cloned().unwrap_or_default();
            let found = (query.get("folderId").map(String::as_str) == Some("standard-images"))
                .then(|| st.images.get(&family))
                .flatten();
            match found {
                Some(image_id) => Action::Respond(json_response(
                    200,
                    &json!({
                        "id": image_id,
                        "folderId": "standard-images",
                        "createdAt": "2026-11-17T10:00:00Z",
                        "name": format!("{family}-v20261117"),
                        "family": family,
                        "minDiskSize": "3221225472",
                        "status": "READY",
                    }),
                )),
                None => {
                    let message = format!("Image with family \"{family}\" not found");
                    Action::Respond(error_response(st, 404, 5, &message, auth))
                }
            }
        }

        ("GET", ["billing", "v1", "skus"]) => {
            if query.get("currency").map(String::as_str) != Some("RUB") {
                return Action::Respond(error_response(
                    st,
                    400,
                    3,
                    "Illegal argument currency",
                    auth,
                ));
            }
            let items: Vec<Value> = st
                .skus
                .iter()
                .map(|sku| {
                    json!({
                        "id": sku.id,
                        "name": sku.name,
                        "description": "",
                        "serviceId": "dn2fakeservice000001",
                        "pricingUnit": sku.unit,
                        "pricingVersions": sku.versions,
                    })
                })
                .collect();
            page(st, "skus", items, query, auth)
        }

        _ => Action::Respond(error_response(st, 404, 5, "Not found", auth)),
    }
}
