//! Клиент Yandex Cloud Compute: вход по ключу сервисного аккаунта, виртуальные машины,
//! операции, диски, образы и цены из каталога SKU.
//!
//! Здесь только слой клиента. Окна, деньги и сверку строит поверх него следующий слой, а
//! трейт `CloudProvider` пока остаётся прежним.
//!
//! Исходы разведены так же, как у прежнего клиента: `Rejected` значит «машины точно нет»,
//! `Ambiguous` значит «она могла появиться, ищи по имени в полном списке каталога».

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use super::ProviderError;
use super::rate_limit::RateLimit;

mod auth;
mod client;
pub mod price;

pub use client::YandexClient;
pub use price::{Price, PriceError, cost_kopecks};

pub const DEFAULT_IAM_URL: &str = "https://iam.api.cloud.yandex.net";
pub const DEFAULT_COMPUTE_URL: &str = "https://compute.api.cloud.yandex.net";
pub const DEFAULT_OPERATION_URL: &str = "https://operation.api.cloud.yandex.net";
pub const DEFAULT_BILLING_URL: &str = "https://billing.api.cloud.yandex.net";

/// Единственная платформа, для которой известны имена SKU.
pub const PLATFORM_STANDARD_V3: &str = "standard-v3";
/// Единственный тип диска, для которого известно имя SKU.
pub const DISK_NETWORK_SSD: &str = "network-ssd";
/// Каталог с общими образами Яндекса.
pub const STANDARD_IMAGES_FOLDER: &str = "standard-images";

fn default_platform() -> String {
    PLATFORM_STANDARD_V3.to_owned()
}

fn default_image_family() -> String {
    "debian-13".to_owned()
}

fn default_preemptible() -> bool {
    true
}

fn default_disk_type() -> String {
    DISK_NETWORK_SSD.to_owned()
}

fn default_iam_url() -> String {
    DEFAULT_IAM_URL.to_owned()
}

fn default_compute_url() -> String {
    DEFAULT_COMPUTE_URL.to_owned()
}

fn default_operation_url() -> String {
    DEFAULT_OPERATION_URL.to_owned()
}

fn default_billing_url() -> String {
    DEFAULT_BILLING_URL.to_owned()
}

fn default_request_timeout() -> Duration {
    Duration::from_secs(30)
}

fn default_max_rate_limit_retries() -> u32 {
    4
}

fn default_backoff() -> Duration {
    Duration::from_secs(1)
}

fn default_max_backoff() -> Duration {
    Duration::from_secs(60)
}

/// Настройки клиента. Секрета здесь нет: `key_file` — только путь к ключу.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct YandexConfig {
    /// Файл авторизованного ключа сервисного аккаунта (JSON): читается один раз при создании
    /// клиента.
    pub key_file: PathBuf,
    /// Каталог по умолчанию. Клиент сам его не читает: каталог каждого вызова называет
    /// вызывающий.
    pub folder_id: String,
    pub zone: String,
    pub subnet_id: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    #[serde(default = "default_image_family")]
    pub image_family: String,
    /// Значение по умолчанию для новых окон; клиент его не читает.
    #[serde(default = "default_preemptible")]
    pub preemptible: bool,
    #[serde(default = "default_disk_type")]
    pub disk_type: String,
    /// Адреса API без завершающего `/`; в тестах указывают на поддельный сервер.
    #[serde(default = "default_iam_url")]
    pub iam_url: String,
    #[serde(default = "default_compute_url")]
    pub compute_url: String,
    #[serde(default = "default_operation_url")]
    pub operation_url: String,
    #[serde(default = "default_billing_url")]
    pub billing_url: String,
    /// Предел на один запрос целиком.
    #[serde(default = "default_request_timeout", with = "humantime_serde")]
    pub request_timeout: Duration,
    /// Предел частоты на каждую конечную точку.
    #[serde(skip)]
    pub rate_limit: RateLimit,
    /// Сколько раз повторять запрос после 429 (кроме создания: там 429 — отказ).
    #[serde(skip, default = "default_max_rate_limit_retries")]
    pub max_rate_limit_retries: u32,
    /// Пауза после первого 429 без `Retry-After`; дальше удваивается.
    #[serde(skip, default = "default_backoff")]
    pub default_backoff: Duration,
    /// Верхняя граница паузы. `Retry-After` больше неё не ждётся: попытка сдаётся.
    #[serde(skip, default = "default_max_backoff")]
    pub max_backoff: Duration,
}

impl YandexConfig {
    pub fn new(
        key_file: impl Into<PathBuf>,
        folder_id: impl Into<String>,
        zone: impl Into<String>,
        subnet_id: impl Into<String>,
    ) -> Self {
        Self {
            key_file: key_file.into(),
            folder_id: folder_id.into(),
            zone: zone.into(),
            subnet_id: subnet_id.into(),
            platform: default_platform(),
            image_family: default_image_family(),
            preemptible: default_preemptible(),
            disk_type: default_disk_type(),
            iam_url: default_iam_url(),
            compute_url: default_compute_url(),
            operation_url: default_operation_url(),
            billing_url: default_billing_url(),
            request_timeout: default_request_timeout(),
            rate_limit: RateLimit::default(),
            max_rate_limit_retries: default_max_rate_limit_retries(),
            default_backoff: default_backoff(),
            max_backoff: default_max_backoff(),
        }
    }

    /// Платформа и тип диска вне списка отвергаются: для них нет имён SKU, а значит и цены.
    pub fn validate(&self) -> Result<(), YandexConfigError> {
        for (field, value) in [
            ("folder_id", &self.folder_id),
            ("zone", &self.zone),
            ("subnet_id", &self.subnet_id),
            ("image_family", &self.image_family),
            ("iam_url", &self.iam_url),
            ("compute_url", &self.compute_url),
            ("operation_url", &self.operation_url),
            ("billing_url", &self.billing_url),
        ] {
            if value.trim().is_empty() {
                return Err(YandexConfigError::Empty(field));
            }
        }
        if self.key_file.as_os_str().is_empty() {
            return Err(YandexConfigError::Empty("key_file"));
        }
        if self.platform != PLATFORM_STANDARD_V3 {
            return Err(YandexConfigError::UnknownPlatform(self.platform.clone()));
        }
        if self.disk_type != DISK_NETWORK_SSD {
            return Err(YandexConfigError::UnknownDiskType(self.disk_type.clone()));
        }
        if self.request_timeout.is_zero() {
            return Err(YandexConfigError::ZeroTimeout);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YandexConfigError {
    Empty(&'static str),
    UnknownPlatform(String),
    UnknownDiskType(String),
    ZeroTimeout,
}

impl fmt::Display for YandexConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty(field) => write!(f, "`{field}` must not be empty"),
            Self::UnknownPlatform(platform) => write!(
                f,
                "platform `{platform}` has no known prices; only `{PLATFORM_STANDARD_V3}` is supported"
            ),
            Self::UnknownDiskType(disk_type) => write!(
                f,
                "disk type `{disk_type}` has no known prices; only `{DISK_NETWORK_SSD}` is supported"
            ),
            Self::ZeroTimeout => f.write_str("`request_timeout` must be positive"),
        }
    }
}

impl std::error::Error for YandexConfigError {}

/// Клиент не создан. В тексте нет ни содержимого ключа, ни его частей.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YandexInitError {
    Config(YandexConfigError),
    /// Файл ключа не читается (причина — вид ошибки ввода-вывода, без содержимого).
    KeyFile(String),
    /// Файл прочитан, но это не авторизованный ключ: чего именно не хватает.
    KeyFormat(&'static str),
    Client,
}

impl fmt::Display for YandexInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(f, "invalid configuration: {error}"),
            Self::KeyFile(why) => write!(f, "cannot read the key file: {why}"),
            Self::KeyFormat(why) => write!(f, "the key file is not an authorized key: {why}"),
            Self::Client => f.write_str("cannot build the HTTP client"),
        }
    }
}

impl std::error::Error for YandexInitError {}

/// Запрос на создание машины. Зона, подсеть, платформа и тип диска берутся из настроек
/// клиента; каталог вызывающий называет явно. `user_data` несёт закрытый хостовый ключ,
/// поэтому в `Debug` его нет.
#[derive(Clone, PartialEq, Eq)]
pub struct CreateInstance {
    /// Имя машины; загрузочный диск получает то же имя.
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub cores: u32,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
    /// Прерываемая машина: из снимка окна, а не из настроек на момент вызова.
    pub preemptible: bool,
    /// Образ загрузочного диска, из `latest_image`.
    pub image_id: String,
    pub user_data: String,
}

impl fmt::Debug for CreateInstance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateInstance")
            .field("name", &self.name)
            .field("labels", &self.labels)
            .field("cores", &self.cores)
            .field("memory_bytes", &self.memory_bytes)
            .field("disk_bytes", &self.disk_bytes)
            .field("preemptible", &self.preemptible)
            .field("image_id", &self.image_id)
            .field("user_data_bytes", &self.user_data.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateOutcome {
    /// Создание принято: номер машины известен сразу, операция ещё идёт.
    Accepted {
        instance_id: String,
        operation_id: String,
    },
    /// Определённый отказ: машины нет и не будет.
    ///
    /// - `http == 0` значит, что запрос не уходил вовсе: не удалось получить IAM-токен.
    /// - `grpc_code` пуст, если тело ответа не разобралось.
    /// - `next_profile`: причина в размере или месте (429 с кодом 8; 400 с кодом 9 и текстом
    ///   про квоту или нехватку ресурсов), имеет смысл попробовать следующий профиль.
    Rejected {
        http: u16,
        grpc_code: Option<i32>,
        message: String,
        next_profile: bool,
    },
    /// Машина могла появиться: имя уже занято (409 с кодом 6), 5xx, таймаут, обрыв или
    /// нечитаемый ответ. Не повторять, искать по имени в полном списке каталога.
    Ambiguous { why: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationState {
    /// `done: false`, даже если в операции уже есть `error`: Яндекс сначала откатывает.
    Pending,
    Done,
    /// `done: true` с `error`.
    Failed {
        grpc_code: i32,
        message: String,
    },
    /// Операции с таким номером нет (404).
    Unknown,
}

/// Состояние машины в нейтральных терминах.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceStatus {
    /// `PROVISIONING`, `STARTING`.
    Provisioning,
    /// `RUNNING`.
    Running,
    /// `STOPPING`. Мы машину не останавливаем: это прерывание Яндексом.
    Stopping,
    /// `STOPPED`.
    Stopped,
    /// `ERROR`, `CRASHED`.
    Broken,
    /// `DELETING`.
    Deleting,
    /// Остальное (`RESTARTING`, `UPDATING`) и то, чего не было в описании API.
    Other(String),
}

impl InstanceStatus {
    pub fn from_api(value: &str) -> Self {
        match value {
            "PROVISIONING" | "STARTING" => Self::Provisioning,
            "RUNNING" => Self::Running,
            "STOPPING" => Self::Stopping,
            "STOPPED" => Self::Stopped,
            "ERROR" | "CRASHED" => Self::Broken,
            "DELETING" => Self::Deleting,
            other => Self::Other(other.to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub id: String,
    /// Имя необязательно в API: у чужой машины оно может быть пустым.
    pub name: String,
    pub folder_id: String,
    pub status: InstanceStatus,
    /// Публичный IPv4 (one-to-one NAT), когда он уже выдан.
    pub public_ip: Option<String>,
    pub boot_disk_id: Option<String>,
    pub preemptible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceLookup {
    Found(Instance),
    /// 404 с кодом 5. Сам по себе отсутствия не доказывает: его доказывает полный список.
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceSummary {
    pub id: String,
    pub name: String,
    pub status: InstanceStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    pub id: String,
    pub name: String,
    /// Машины, к которым диск подключён. Пусто у брошенного диска.
    pub instance_ids: Vec<String>,
    /// Как в API: `CREATING`, `READY`, `ERROR`, `DELETING`.
    pub status: String,
}

/// Исход удаления машины или диска.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// Удаление принято. Повтор, пока оно идёт, возвращает ту же операцию.
    Accepted { operation_id: String },
    /// Уже нет (404).
    AlreadyGone,
    /// `http` пуст, если ответа не было (таймаут, обрыв, не получен токен).
    Failed {
        retryable: bool,
        http: Option<u16>,
        grpc_code: Option<i32>,
        message: String,
    },
}

/// Список каталога не получен целиком. Частичный список успехом не считается никогда.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListError {
    /// Страница с этим номером (с нуля) не прочиталась или не разобралась.
    Page { page: u32, error: ProviderError },
    /// Сервер вернул `nextPageToken`, который уже был: обход зациклился.
    RepeatedToken { page: u32 },
    /// Страниц больше предела.
    TooManyPages { limit: u32 },
}

impl ListError {
    pub fn retryable(&self) -> bool {
        match self {
            Self::Page { error, .. } => error.retryable(),
            Self::RepeatedToken { .. } | Self::TooManyPages { .. } => true,
        }
    }
}

impl fmt::Display for ListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Page { page, error } => write!(f, "listing failed at page {page}: {error}"),
            Self::RepeatedToken { page } => {
                write!(f, "listing repeated a page token at page {page}")
            }
            Self::TooManyPages { limit } => write!(f, "listing has more than {limit} pages"),
        }
    }
}

impl std::error::Error for ListError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> YandexConfig {
        YandexConfig::new(
            "/nonexistent/key.json",
            "b1gfakefolder000000",
            "ru-central1-a",
            "e9bfakesubnet000000",
        )
    }

    #[test]
    fn defaults_are_the_real_endpoints_and_a_preemptible_standard_v3() {
        let config = config();
        assert_eq!(config.platform, "standard-v3");
        assert_eq!(config.image_family, "debian-13");
        assert!(config.preemptible);
        assert_eq!(config.disk_type, "network-ssd");
        assert_eq!(config.iam_url, "https://iam.api.cloud.yandex.net");
        assert_eq!(config.compute_url, "https://compute.api.cloud.yandex.net");
        assert_eq!(
            config.operation_url,
            "https://operation.api.cloud.yandex.net"
        );
        assert_eq!(config.billing_url, "https://billing.api.cloud.yandex.net");
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn deserializes_with_defaults_and_refuses_unknown_keys() {
        let parsed: YandexConfig = toml::from_str(
            r#"
            key_file = "/etc/canterlot/yandex-key.json"
            folder_id = "b1gfakefolder000000"
            zone = "ru-central1-a"
            subnet_id = "e9bfakesubnet000000"
            preemptible = false
            request_timeout = "45s"
            "#,
        )
        .unwrap();
        assert!(!parsed.preemptible);
        assert_eq!(parsed.request_timeout, Duration::from_secs(45));
        assert_eq!(parsed.platform, "standard-v3");
        assert_eq!(parsed.max_rate_limit_retries, 4);
        assert_eq!(parsed.default_backoff, Duration::from_secs(1));
        assert_eq!(parsed.max_backoff, Duration::from_secs(60));
        assert_eq!(parsed.validate(), Ok(()));

        let unknown = toml::from_str::<YandexConfig>(
            r#"
            key_file = "/etc/canterlot/yandex-key.json"
            folder_id = "b1gfakefolder000000"
            zone = "ru-central1-a"
            subnet_id = "e9bfakesubnet000000"
            router_id = "x"
            "#,
        );
        assert!(unknown.is_err());
    }

    #[test]
    fn validation_refuses_platforms_and_disks_without_known_prices() {
        let mut other_platform = config();
        other_platform.platform = "standard-v2".to_owned();
        assert_eq!(
            other_platform.validate(),
            Err(YandexConfigError::UnknownPlatform("standard-v2".to_owned()))
        );
        let mut other_disk = config();
        other_disk.disk_type = "network-hdd".to_owned();
        assert_eq!(
            other_disk.validate(),
            Err(YandexConfigError::UnknownDiskType("network-hdd".to_owned()))
        );
        let mut no_folder = config();
        no_folder.folder_id = " ".to_owned();
        assert_eq!(
            no_folder.validate(),
            Err(YandexConfigError::Empty("folder_id"))
        );
    }

    #[test]
    fn statuses_map_into_the_neutral_set() {
        use InstanceStatus::{Broken, Deleting, Other, Provisioning, Running, Stopped, Stopping};
        for (api, expected) in [
            ("PROVISIONING", Provisioning),
            ("STARTING", Provisioning),
            ("RUNNING", Running),
            ("STOPPING", Stopping),
            ("STOPPED", Stopped),
            ("CRASHED", Broken),
            ("ERROR", Broken),
            ("DELETING", Deleting),
            ("RESTARTING", Other("RESTARTING".to_owned())),
            ("UPDATING", Other("UPDATING".to_owned())),
            ("", Other(String::new())),
        ] {
            assert_eq!(InstanceStatus::from_api(api), expected, "{api}");
        }
    }

    #[test]
    fn create_request_debug_hides_the_user_data() {
        let request = CreateInstance {
            name: "canterlot-runner-x".to_owned(),
            labels: BTreeMap::new(),
            cores: 2,
            memory_bytes: 1,
            disk_bytes: 1,
            preemptible: true,
            image_id: "fd8fakeimage0000001".to_owned(),
            user_data: "PRIVATE-HOST-KEY-MATERIAL".to_owned(),
        };
        let shown = format!("{request:?}");
        assert!(!shown.contains("PRIVATE-HOST-KEY-MATERIAL"));
        assert!(shown.contains("user_data_bytes"));
    }
}
