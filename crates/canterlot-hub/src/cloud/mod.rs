//! Облачные серверы под раннеры: интерфейс провайдера и всё, что нужно, чтобы поднять
//! сервер и посчитать его цену. Здесь нет таблиц и нет связи с циклами хаба: окна, деньги и
//! сверку строит поверх этого модуля следующий слой, а в тестах подставляет поддельный
//! провайдер через трейт `CloudProvider`.
//!
//! Исходы создания и удаления разведены так, чтобы вызывающий не гадал: `Rejected` значит
//! «сервера точно нет», `Ambiguous` значит «он мог появиться, ищи по имени в списке проекта».

use std::fmt;

use async_trait::async_trait;

pub mod budget;
pub mod cloud_init;
pub mod host_key;
pub mod price;
pub mod rate_limit;
pub mod service;
pub mod timeweb;
pub mod windows;

pub use timeweb::{TimewebConfig, TimewebProvider};

/// Пределы конфигуратора в тех же единицах, что в запросе создания (память и диск в МБ).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub cpu_min: u32,
    pub cpu_max: u32,
    pub cpu_step: u32,
    pub ram_min_mb: u64,
    pub ram_max_mb: u64,
    pub ram_step_mb: u64,
    pub disk_min_mb: u64,
    pub disk_max_mb: u64,
    pub disk_step_mb: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configurator {
    pub id: i64,
    pub location: String,
    pub cpu_frequency: String,
    /// Цены за единицу ресурса в копейках в месяц.
    pub prices: price::UnitPrices,
    pub limits: Limits,
}

/// Запрос на создание сервера. Облачный скрипт несёт закрытый хостовый ключ, поэтому в
/// `Debug` его нет.
#[derive(Clone, PartialEq, Eq)]
pub struct CreateServer {
    pub name: String,
    pub comment: String,
    pub project_id: i64,
    pub configurator_id: i64,
    pub cpu: u32,
    pub ram_mb: u64,
    pub disk_mb: u64,
    pub os_id: i64,
    pub ssh_key_ids: Vec<i64>,
    pub cloud_init: String,
    pub hostname: String,
    /// Приватная сеть сервера: идентификатор VPC и локальный адрес из пула окна.
    pub network_id: String,
    pub local_ip: String,
    /// Зона доступности сервера (из профиля).
    pub availability_zone: String,
}

impl fmt::Debug for CreateServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateServer")
            .field("name", &self.name)
            .field("project_id", &self.project_id)
            .field("configurator_id", &self.configurator_id)
            .field("cpu", &self.cpu)
            .field("ram_mb", &self.ram_mb)
            .field("disk_mb", &self.disk_mb)
            .field("os_id", &self.os_id)
            .field("ssh_key_ids", &self.ssh_key_ids)
            .field("cloud_init_bytes", &self.cloud_init.len())
            .field("hostname", &self.hostname)
            .field("network_id", &self.network_id)
            .field("local_ip", &self.local_ip)
            .field("availability_zone", &self.availability_zone)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateOutcome {
    /// Сервер создан, у него есть номер.
    Created { server_id: i64 },
    /// Определённый отказ (4xx, кроме 429): сервера нет и не будет.
    /// `error_code` — поле `error_code` тела ответа API (`no_free_node`, …), пусто, если тела нет.
    Rejected {
        code: u16,
        error_code: String,
        message: String,
    },
    /// Таймаут, обрыв после отправки, 5xx или нечитаемый ответ: сервер мог появиться. Не
    /// повторять, искать по имени в списке проекта.
    Ambiguous { why: String },
}

/// Отказ создания, после которого окно пробует следующий профиль: сервера нет, а причина в
/// самой конфигурации или её месте, а не в запросе в целом. Проверено у Timeweb 21.11:
/// 409 `no_free_node`; `location_not_available` и `preset_not_found`; 400
/// `configuration_requires_sales_request` (16 vCPU и больше). Любой другой отказ окно не
/// «обходит»: оно закрывается с его кодом.
pub fn profile_fallback(status: u16, error_code: &str) -> bool {
    matches!(
        (status, error_code),
        (409, "no_free_node")
            | (400..=499, "location_not_available" | "preset_not_found")
            | (400, "configuration_requires_sales_request")
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// Удаление принято. `quarantined`: сервер отправлен в карантин, а не стёрт сразу.
    Deleted {
        quarantined: bool,
    },
    /// Сервера уже нет (404).
    AlreadyGone,
    /// Провайдер просит подтверждение (хеш в ответе или 423): у токена нет права удалять
    /// без подтверждения. Не повторять.
    NeedsConfirmation,
    Failed {
        retryable: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerStatus {
    Installing,
    SoftwareInstall,
    Reinstalling,
    On,
    Off,
    TurningOn,
    TurningOff,
    HardTurningOff,
    Rebooting,
    HardRebooting,
    Removing,
    Removed,
    Cloning,
    Transfer,
    Blocked,
    Configuring,
    NoPaid,
    PermanentBlocked,
    /// Статус, которого не было в описании API.
    Other(String),
}

impl ServerStatus {
    pub fn from_api(value: &str) -> Self {
        match value {
            "installing" => Self::Installing,
            "software_install" => Self::SoftwareInstall,
            "reinstalling" => Self::Reinstalling,
            "on" => Self::On,
            "off" => Self::Off,
            "turning_on" => Self::TurningOn,
            "turning_off" => Self::TurningOff,
            "hard_turning_off" => Self::HardTurningOff,
            "rebooting" => Self::Rebooting,
            "hard_rebooting" => Self::HardRebooting,
            "removing" => Self::Removing,
            "removed" => Self::Removed,
            "cloning" => Self::Cloning,
            "transfer" => Self::Transfer,
            "blocked" => Self::Blocked,
            "configuring" => Self::Configuring,
            "no_paid" => Self::NoPaid,
            "permanent_blocked" => Self::PermanentBlocked,
            other => Self::Other(other.to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerState {
    pub id: i64,
    pub name: String,
    /// Проекта в схеме сервера (`vds`) нет: принадлежность проекту доказывает только полный
    /// список проекта, а не точечный ответ.
    pub status: ServerStatus,
    /// Основной публичный IPv4, когда он уже выдан.
    pub ipv4: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerLookup {
    Found(ServerState),
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSummary {
    pub id: i64,
    pub name: String,
    pub status: ServerStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 401 или 403: у токена нет прав или он недействителен.
    Auth,
    NotFound,
    /// 429, и ожидание по `Retry-After` не помогло.
    RateLimited,
    /// Таймаут, обрыв, 5xx.
    Unavailable,
    /// Другой определённый отказ 4xx.
    Rejected,
    /// Имя занято другим содержимым.
    Conflict,
    /// Ответ не похож на описание API.
    Malformed,
}

/// Ошибка вызова провайдера. Текст никогда не содержит токен.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub kind: ErrorKind,
    pub message: String,
}

impl ProviderError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Имеет смысл повторить позже.
    pub fn retryable(&self) -> bool {
        matches!(self.kind, ErrorKind::RateLimited | ErrorKind::Unavailable)
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for ProviderError {}

/// Список проекта не получен целиком. Частичный список успехом не считается никогда.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListError {
    /// Страница с этим смещением не прочиталась.
    Page { offset: u64, error: ProviderError },
    /// Страницы прочитались, но не сложились в заявленное общее число.
    Incomplete { expected: u64, got: u64 },
}

impl ListError {
    pub fn retryable(&self) -> bool {
        match self {
            Self::Page { error, .. } => error.retryable(),
            Self::Incomplete { .. } => true,
        }
    }
}

impl fmt::Display for ListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Page { offset, error } => write!(f, "listing failed at offset {offset}: {error}"),
            Self::Incomplete { expected, got } => {
                write!(f, "listing incomplete: expected {expected}, got {got}")
            }
        }
    }
}

impl std::error::Error for ListError {}

#[async_trait]
pub trait CloudProvider: Send + Sync {
    /// Конфигуратор с ценами и пределами.
    async fn configurator(&self, id: i64) -> Result<Configurator, ProviderError>;

    /// Ключ с таким именем: найти или создать. Повторный вызов с тем же ключом возвращает
    /// тот же номер. Ключи не удаляются никогда: у токена может не быть этого права.
    async fn ensure_ssh_key(&self, name: &str, public_key: &str) -> Result<i64, ProviderError>;

    /// Создание сервера. Один вызов, без повторов: исход разобран в `CreateOutcome`.
    async fn create_server(&self, request: &CreateServer) -> CreateOutcome;

    async fn get_server(&self, id: i64) -> Result<ServerLookup, ProviderError>;

    async fn delete_server(&self, id: i64) -> DeleteOutcome;

    /// Все серверы проекта, все страницы. Любая неудавшаяся страница делает ошибкой весь
    /// вызов.
    async fn list_project_servers(&self, project_id: i64) -> Result<Vec<ServerSummary>, ListError>;

    /// Создать DNAT-правило роутера: `router_ip:public_port -> local_ip:22`, tcp. Один
    /// вызов, без повторов: после неопределённого исхода правило могло появиться.
    async fn create_dnat_rule(&self, router_id: &str, rule: &DnatRuleRequest) -> DnatCreateOutcome;

    /// Все DNAT-правила роутера, все страницы. Частичный список успехом не считается.
    async fn list_dnat_rules(&self, router_id: &str) -> Result<Vec<DnatRule>, ListError>;

    /// Удалить правило DNAT. Исходы те же, что у сервера (`AlreadyGone` — 404).
    async fn delete_dnat_rule(&self, router_id: &str, rule_id: &str) -> DeleteOutcome;
}

/// Запрос на проброс SSH в приватную сеть.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnatRuleRequest {
    pub local_ip: String,
    pub local_port: u16,
    pub public_ip: String,
    pub public_port: u16,
}

/// Правило DNAT, как его возвращает список роутера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnatRule {
    pub id: String,
    pub local_ip: String,
    pub local_port: String,
    pub public_ip: String,
    pub public_port: String,
    pub protocol: String,
}

impl DnatRule {
    /// Это правило пробрасывает именно `router_ip:port -> local_ip:22` по tcp. Все поля
    /// должны совпасть точно: диапазон локальных портов — не наше правило, а неизвестное
    /// поле не доказывает ничего.
    pub fn matches(&self, public_ip: &str, public_port: u16, local_ip: &str) -> bool {
        self.public_ip == public_ip
            && self.public_port == public_port.to_string()
            && self.local_ip == local_ip
            && self.local_port == "22"
            && self.protocol == "tcp"
    }
}

/// Исход создания DNAT-правила.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnatCreateOutcome {
    /// Правило создано, у него есть номер.
    Created { rule_id: String },
    /// Определённый отказ (4xx, кроме 429): правила нет.
    Refused {
        status: u16,
        error_code: String,
        message: String,
    },
    /// Таймаут, обрыв, 5xx или нечитаемый ответ: правило могло появиться. Не создавать
    /// второе для того же порта: полный список решит, что сделать.
    Ambiguous { why: String },
}

#[cfg(test)]
mod tests {
    use super::DnatRule;

    fn rule(public_ip: &str, public_port: &str, local_ip: &str, local_port: &str) -> DnatRule {
        DnatRule {
            id: "dnat-1".to_owned(),
            local_ip: local_ip.to_owned(),
            local_port: local_port.to_owned(),
            public_ip: public_ip.to_owned(),
            public_port: public_port.to_owned(),
            protocol: "tcp".to_owned(),
        }
    }

    #[test]
    fn a_dnat_rule_matches_only_the_exact_tuple() {
        let exact = rule("192.0.2.1", "22100", "192.0.2.10", "22");
        assert!(exact.matches("192.0.2.1", 22100, "192.0.2.10"));
        // Диапазон локальных портов — не наше правило: мы создаём одноточное.
        assert!(
            !rule("192.0.2.1", "22100", "192.0.2.10", "1-65535").matches(
                "192.0.2.1",
                22100,
                "192.0.2.10"
            )
        );
        for wrong in [
            rule("192.0.2.2", "22100", "192.0.2.10", "22"),
            rule("192.0.2.1", "22101", "192.0.2.10", "22"),
            rule("192.0.2.1", "22100", "192.0.2.11", "22"),
            rule("192.0.2.1", "22100", "192.0.2.10", "2222"),
        ] {
            assert!(!wrong.matches("192.0.2.1", 22100, "192.0.2.10"));
        }
        let mut udp = exact.clone();
        udp.protocol = "udp".to_owned();
        assert!(!udp.matches("192.0.2.1", 22100, "192.0.2.10"));
        let mut empty = exact;
        empty.protocol.clear();
        assert!(!empty.matches("192.0.2.1", 22100, "192.0.2.10"));
    }
}
