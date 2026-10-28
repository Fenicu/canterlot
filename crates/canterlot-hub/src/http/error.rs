use std::future::Future;

use axum::{Json, http::StatusCode, response::IntoResponse, response::Response};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    dberr,
    delivery::{SubmitError, control::ControlError},
    executions::ExecutionError,
    idempotency::IdempotencyConflict,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorScope {
    Browser,
    Auth,
    HubDb,
    Delivery,
    WorkerDisk,
    CliStart,
    AdapterProtocol,
    AgentService,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Retry {
    SafeSameKey,
    ExplicitNewOperation,
    AfterCondition,
    Never,
}

/// Единый реестр кодов: перечисление, строка `docs/design/api.md` §Errors и схема OpenAPI
/// описывают одно и то же, расхождение ловит тест `error_code_table_in_api_md_...`.
macro_rules! error_codes {
    ($($variant:ident => $wire:literal, $status:literal, $scope:ident, $retry:ident, $message:literal;)+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, ToSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum ErrorCode {
            $($variant),+
        }

        impl ErrorCode {
            pub const ALL: &'static [ErrorCode] = &[$(ErrorCode::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }

            pub fn status(self) -> StatusCode {
                match self {
                    $(Self::$variant => StatusCode::from_u16($status).expect("valid status")),+
                }
            }

            pub fn scope(self) -> ErrorScope {
                match self {
                    $(Self::$variant => ErrorScope::$scope),+
                }
            }

            pub fn retry(self) -> Retry {
                match self {
                    $(Self::$variant => Retry::$retry),+
                }
            }

            pub fn message(self) -> &'static str {
                match self {
                    $(Self::$variant => $message),+
                }
            }
        }
    };
}

error_codes! {
    Unauthenticated => "unauthenticated", 401, Auth, AfterCondition, "Требуется вход через сервис авторизации";
    ForbiddenUser => "forbidden_user", 403, Auth, Never, "Доступ для этого пользователя запрещён";
    OriginRejected => "origin_rejected", 403, Auth, Never, "Запрос отклонён: недопустимый источник";
    InvalidRequest => "invalid_request", 400, Browser, Never, "Некорректный запрос";
    InvalidOffset => "invalid_offset", 400, Browser, Never, "Недопустимое смещение";
    NotFound => "not_found", 404, Browser, Never, "Не найдено";
    MethodNotAllowed => "method_not_allowed", 405, Browser, Never, "Метод не поддерживается";
    IdempotencyConflict => "idempotency_conflict", 409, Browser, Never, "Ключ идемпотентности уже использован с другим запросом";
    ResourceLimit => "resource_limit", 413, Browser, Never, "Превышен допустимый размер данных";
    NotImplemented => "not_implemented", 501, HubDb, Never, "Функция пока не реализована";
    TooManyRequests => "too_many_requests", 429, HubDb, SafeSameKey, "Слишком много запросов, повторите позже";
    DbUnavailable => "db_unavailable", 503, HubDb, SafeSameKey, "База данных временно недоступна";
    Internal => "internal", 500, HubDb, SafeSameKey, "Внутренняя ошибка хаба";
    QueueFull => "queue_full", 409, Delivery, AfterCondition, "Очередь разговора заполнена";
    ConversationClosed => "conversation_closed", 409, Delivery, Never, "Разговор закрыт";
    ExecutionAlreadyLive => "execution_already_live", 409, Delivery, AfterCondition, "Исполнение уже запущено";
    InvalidState => "invalid_state", 409, Delivery, Never, "Действие недопустимо в текущем состоянии";
    NotPending => "not_pending", 409, Browser, Never, "Отложенное сообщение уже обработано";
    ScheduleTimeOutOfRange => "schedule_time_out_of_range", 422, Browser, Never, "Время отправки вне допустимого диапазона";
    TooManyScheduled => "too_many_scheduled", 409, Browser, Never, "Слишком много отложенных сообщений";
    MachineNotFound => "machine_not_found", 404, Browser, Never, "Машина не найдена";
    MachineUnavailable => "machine_unavailable", 409, Delivery, AfterCondition, "Машина недоступна";
    CapabilityMissing => "capability_missing", 409, Delivery, AfterCondition, "Требуется обновление воркера";
    LimitsMismatch => "limits_mismatch", 409, Browser, Never, "Лимиты вложений машины не совпадают";
    HarnessUnknown => "harness_unknown", 422, Browser, Never, "Харнесс отсутствует в каталоге машины";
    ModelUnknown => "model_unknown", 422, Browser, Never, "Модель отсутствует в каталоге харнесса";
    EffortNotAllowed => "effort_not_allowed", 422, Browser, Never, "Усилие недоступно для модели";
    PermissionModeUnknown => "permission_mode_unknown", 422, Browser, Never, "Режим разрешений отсутствует в каталоге";
    SettingsMidsessionUnsupported => "settings_midsession_unsupported", 422, Browser, Never, "Смена настроек во время исполнения не поддерживается";
    ProfileUnknown => "profile_unknown", 422, Browser, Never, "Профиль инструкций отсутствует в каталоге";
    WorkdirNotAllowed => "workdir_not_allowed", 422, Browser, Never, "Рабочий каталог вне разрешённых корней";
    GroupNotFound => "group_not_found", 404, Browser, Never, "Группа не найдена";
    GroupNameTaken => "group_name_taken", 409, Browser, Never, "Группа с таким именем уже существует";
    ArchiveActive => "archive_active", 409, Browser, Never, "В сессии есть активная работа";
}

impl From<crate::conversations::SettingsError> for ApiError {
    fn from(error: crate::conversations::SettingsError) -> Self {
        use crate::conversations::SettingsError as E;
        Self::new(match error {
            E::NotFound => ErrorCode::NotFound,
            E::ModelUnknown => ErrorCode::ModelUnknown,
            E::CapabilityMissing => ErrorCode::CapabilityMissing,
            E::EffortNotAllowed => ErrorCode::EffortNotAllowed,
            E::PermissionModeUnknown => ErrorCode::PermissionModeUnknown,
            E::MidsessionUnsupported => ErrorCode::SettingsMidsessionUnsupported,
            E::IdempotencyConflict => ErrorCode::IdempotencyConflict,
            E::Db(error) => return error.into(),
        })
    }
}

impl From<crate::crunch::CrunchError> for ApiError {
    fn from(error: crate::crunch::CrunchError) -> Self {
        use crate::crunch::CrunchError as E;
        match error {
            E::NotFound => Self::new(ErrorCode::NotFound),
            E::IdempotencyConflict => Self::new(ErrorCode::IdempotencyConflict),
            E::Db(error) => error.into(),
        }
    }
}

impl From<crate::scheduler::ScheduleError> for ApiError {
    fn from(error: crate::scheduler::ScheduleError) -> Self {
        use crate::scheduler::ScheduleError as E;
        match error {
            E::NotFound => Self::new(ErrorCode::NotFound),
            E::NotPending => Self::new(ErrorCode::NotPending),
            E::TimeOutOfRange => Self::new(ErrorCode::ScheduleTimeOutOfRange),
            E::TooManyPending => Self::new(ErrorCode::TooManyScheduled),
            E::InvalidContent(_) => Self::new(ErrorCode::InvalidRequest),
            E::AttachmentsUnavailable => Self::new(ErrorCode::NotImplemented),
            E::IdempotencyConflict => Self::new(ErrorCode::IdempotencyConflict),
            E::Db(error) => error.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveReason {
    Crunch,
    Scheduled,
    LiveExecution,
}

/// Закрытый словарь дополнительных полей: только перечисления и числа, без пользовательского текста.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct ErrorDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub reasons: Option<Vec<ArchiveReason>>,
}

impl ErrorDetails {
    pub fn archive_active(reasons: Vec<ArchiveReason>) -> Self {
        Self {
            reasons: Some(reasons),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: ErrorCode,
    pub scope: ErrorScope,
    pub retry: Retry,
    pub message: &'static str,
    pub details: Option<ErrorDetails>,
    pub correlation_id: Uuid,
}

#[derive(Serialize, ToSchema)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: &'static str,
    pub scope: ErrorScope,
    pub retry: Retry,
    pub correlation_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub details: Option<ErrorDetails>,
}

#[derive(Serialize, ToSchema)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

tokio::task_local! {
    static CORRELATION: Uuid;
}

/// Идентификатор корреляции текущего запроса; вне запроса его нет.
pub fn current_correlation() -> Option<Uuid> {
    CORRELATION.try_with(|id| *id).ok()
}

pub(crate) async fn with_correlation<F: Future>(id: Uuid, future: F) -> F::Output {
    CORRELATION.scope(id, future).await
}

impl ApiError {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            status: code.status(),
            code,
            scope: code.scope(),
            retry: code.retry(),
            message: code.message(),
            details: None,
            correlation_id: current_correlation().unwrap_or(Uuid::nil()),
        }
    }

    pub fn with_details(mut self, details: ErrorDetails) -> Self {
        self.details = Some(details);
        self
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code.as_str())
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let correlation_id = if self.correlation_id.is_nil() {
            current_correlation().unwrap_or_else(Uuid::now_v7)
        } else {
            self.correlation_id
        };
        let body = ErrorEnvelope {
            error: ErrorBody {
                code: self.code,
                message: self.message,
                scope: self.scope,
                retry: self.retry,
                correlation_id,
                details: self.details,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        let info = dberr::describe(&error);
        let api = ApiError::new(if info.is_unavailable() {
            ErrorCode::DbUnavailable
        } else {
            ErrorCode::Internal
        });
        tracing::error!(
            kind = "db_error",
            sqlstate = info.sqlstate.as_deref().unwrap_or("-"),
            constraint = info.constraint.as_deref().unwrap_or("-"),
            class = info.class,
            correlation_id = %api.correlation_id,
            "database error"
        );
        api
    }
}

impl From<IdempotencyConflict> for ApiError {
    fn from(_: IdempotencyConflict) -> Self {
        ApiError::new(ErrorCode::IdempotencyConflict)
    }
}

impl From<SubmitError> for ApiError {
    fn from(error: SubmitError) -> Self {
        match error {
            SubmitError::IdempotencyConflict => ApiError::new(ErrorCode::IdempotencyConflict),
            SubmitError::KeyRaceLost => ApiError::new(ErrorCode::Internal),
            SubmitError::InvalidRequest {
                reason: "attachments_unavailable",
            } => ApiError::new(ErrorCode::NotImplemented),
            SubmitError::InvalidRequest {
                reason: "invalid_retry",
            } => {
                let mut error = ApiError::new(ErrorCode::InvalidRequest);
                error.status = StatusCode::UNPROCESSABLE_ENTITY;
                error
            }
            SubmitError::InvalidRequest { .. } | SubmitError::InvalidContent { .. } => {
                ApiError::new(ErrorCode::InvalidRequest)
            }
            SubmitError::QueueFull { .. } => ApiError::new(ErrorCode::QueueFull),
            SubmitError::ResourceLimit { .. } => ApiError::new(ErrorCode::ResourceLimit),
            SubmitError::ConversationNotFound => ApiError::new(ErrorCode::NotFound),
            SubmitError::ConversationClosed => ApiError::new(ErrorCode::ConversationClosed),
            SubmitError::Db(error) => error.into(),
        }
    }
}

impl From<ControlError> for ApiError {
    fn from(error: ControlError) -> Self {
        match error {
            ControlError::NotFound => ApiError::new(ErrorCode::NotFound),
            ControlError::IdempotencyConflict => ApiError::new(ErrorCode::IdempotencyConflict),
            ControlError::InvalidRequest { .. } => ApiError::new(ErrorCode::InvalidRequest),
            ControlError::Db(error) => error.into(),
        }
    }
}

impl From<ExecutionError> for ApiError {
    fn from(error: ExecutionError) -> Self {
        match error {
            ExecutionError::IdempotencyConflict => ApiError::new(ErrorCode::IdempotencyConflict),
            ExecutionError::AlreadyLive { .. } => ApiError::new(ErrorCode::ExecutionAlreadyLive),
            ExecutionError::ConversationNotFound => ApiError::new(ErrorCode::NotFound),
            ExecutionError::ConversationClosed => ApiError::new(ErrorCode::ConversationClosed),
            ExecutionError::InvalidTransition { .. } => ApiError::new(ErrorCode::InvalidState),
            ExecutionError::Db(error) => error.into(),
        }
    }
}

impl From<crate::conversations::CreateError> for ApiError {
    fn from(e: crate::conversations::CreateError) -> Self {
        use crate::conversations::CreateError as E;
        Self::new(match e {
            E::MachineNotFound => ErrorCode::MachineNotFound,
            E::MachineUnavailable => ErrorCode::MachineUnavailable,
            E::CapabilityMissing => ErrorCode::CapabilityMissing,
            E::HarnessUnknown => ErrorCode::HarnessUnknown,
            E::ModelUnknown => ErrorCode::ModelUnknown,
            E::EffortNotAllowed => ErrorCode::EffortNotAllowed,
            E::PermissionModeUnknown => ErrorCode::PermissionModeUnknown,
            E::ProfileUnknown => ErrorCode::ProfileUnknown,
            E::WorkdirNotAllowed => ErrorCode::WorkdirNotAllowed,
            E::GroupNotFound => ErrorCode::GroupNotFound,
            E::Invalid(_) => ErrorCode::InvalidRequest,
            E::IdempotencyConflict => ErrorCode::IdempotencyConflict,
            E::Db(e) => return e.into(),
        })
    }
}
impl From<crate::conversations::UpdateError> for ApiError {
    fn from(e: crate::conversations::UpdateError) -> Self {
        use crate::conversations::UpdateError as E;
        Self::new(match e {
            E::NotFound => ErrorCode::NotFound,
            E::GroupNotFound => ErrorCode::GroupNotFound,
            E::Invalid(_) => ErrorCode::InvalidRequest,
            E::IdempotencyConflict => ErrorCode::IdempotencyConflict,
            E::ArchiveActive(reasons) => {
                return Self::new(ErrorCode::ArchiveActive).with_details(
                    ErrorDetails::archive_active(
                        reasons
                            .into_iter()
                            .filter_map(|r| match r {
                                "crunch" => Some(ArchiveReason::Crunch),
                                "scheduled" => Some(ArchiveReason::Scheduled),
                                "live_execution" => Some(ArchiveReason::LiveExecution),
                                _ => None,
                            })
                            .collect(),
                    ),
                );
            }
            E::Db(e) => return e.into(),
        })
    }
}
impl From<crate::groups::GroupError> for ApiError {
    fn from(e: crate::groups::GroupError) -> Self {
        use crate::groups::GroupError as E;
        Self::new(match e {
            E::NotFound => ErrorCode::NotFound,
            E::NameTaken => ErrorCode::GroupNameTaken,
            E::Invalid => ErrorCode::InvalidRequest,
            E::IdempotencyConflict => ErrorCode::IdempotencyConflict,
            E::Db(e) => return e.into(),
        })
    }
}
