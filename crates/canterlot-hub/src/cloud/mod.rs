//! Облачные машины под раннеры: интерфейс провайдера и всё, что нужно, чтобы поднять машину
//! и посчитать её цену. Провайдер один, Yandex Cloud. Окна, деньги и сверку строят `windows`
//! и `service`, а в тестах клиент смотрит на поддельный Яндекс или подменяется целиком через
//! трейт `CloudProvider`.
//!
//! Исходы создания и удаления разведены так, чтобы вызывающий не гадал: `Rejected` значит
//! «машины точно нет», `Ambiguous` значит «она могла появиться, ищи по имени в полном списке
//! каталога».

use std::fmt;

use async_trait::async_trait;

pub mod budget;
pub mod cloud_init;
pub mod host_key;
pub mod price;
pub mod rate_limit;
pub mod service;
pub mod windows;
pub mod yandex;

pub use yandex::{
    CreateInstance, CreateOutcome, DeleteOutcome, Disk, Instance, InstanceLookup, InstanceStatus,
    InstanceSummary, ListError, OperationState, Price, YandexClient, YandexConfig,
};

use crate::config::CloudProfile;

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

/// Провайдер облачных машин. Каталог каждого вызова называет вызывающий: у окна это каталог
/// из его строки, у поиска лишних машин — каталог из конфигурации. Сам провайдер каталог
/// ниоткуда не берёт.
#[async_trait]
pub trait CloudProvider: Send + Sync {
    /// Две ставки часа профиля по ценам каталога SKU, действующим сейчас. Любая неясность
    /// в каталоге — ошибка, а не догадка.
    async fn price(
        &self,
        profile: &CloudProfile,
        preemptible: bool,
    ) -> Result<Price, yandex::PriceError>;

    /// Создание машины. Один вызов, без повторов: исход разобран в `CreateOutcome`.
    async fn create(&self, folder_id: &str, request: &CreateInstance) -> CreateOutcome;

    /// Состояние операции создания или удаления.
    async fn operation(&self, operation_id: &str) -> Result<OperationState, ProviderError>;

    async fn get(&self, instance_id: &str) -> Result<InstanceLookup, ProviderError>;

    async fn delete(&self, instance_id: &str) -> DeleteOutcome;

    /// Все машины каталога, все страницы. Любая неудавшаяся страница делает ошибкой весь
    /// вызов.
    async fn list_instances(&self, folder_id: &str) -> Result<Vec<InstanceSummary>, ListError>;

    /// Все диски каталога, с тем же правилом.
    async fn list_disks(&self, folder_id: &str) -> Result<Vec<Disk>, ListError>;

    /// Удаление диска. Подключённый диск провайдер удалить не даст.
    async fn delete_disk(&self, disk_id: &str) -> DeleteOutcome;

    /// Номер свежего образа семейства.
    async fn latest_image(&self, family: &str) -> Result<String, ProviderError>;
}

#[async_trait]
impl CloudProvider for YandexClient {
    async fn price(
        &self,
        profile: &CloudProfile,
        preemptible: bool,
    ) -> Result<Price, yandex::PriceError> {
        YandexClient::price(
            self,
            profile.cores,
            u64::from(profile.memory_gb),
            u64::from(profile.disk_gb),
            preemptible,
        )
        .await
    }

    async fn create(&self, folder_id: &str, request: &CreateInstance) -> CreateOutcome {
        YandexClient::create(self, folder_id, request).await
    }

    async fn operation(&self, operation_id: &str) -> Result<OperationState, ProviderError> {
        YandexClient::operation(self, operation_id).await
    }

    async fn get(&self, instance_id: &str) -> Result<InstanceLookup, ProviderError> {
        YandexClient::get(self, instance_id).await
    }

    async fn delete(&self, instance_id: &str) -> DeleteOutcome {
        YandexClient::delete(self, instance_id).await
    }

    async fn list_instances(&self, folder_id: &str) -> Result<Vec<InstanceSummary>, ListError> {
        YandexClient::list_instances(self, folder_id).await
    }

    async fn list_disks(&self, folder_id: &str) -> Result<Vec<Disk>, ListError> {
        YandexClient::list_disks(self, folder_id).await
    }

    async fn delete_disk(&self, disk_id: &str) -> DeleteOutcome {
        YandexClient::delete_disk(self, disk_id).await
    }

    async fn latest_image(&self, family: &str) -> Result<String, ProviderError> {
        YandexClient::latest_image(self, family).await
    }
}
