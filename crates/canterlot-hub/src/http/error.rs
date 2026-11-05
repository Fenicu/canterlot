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
    DecisionConflict => "decision_conflict", 409, Browser, Never, "Решение уже принято с другого устройства";
    RequestNotOpen => "request_not_open", 409, Browser, Never, "Запрос уже закрыт";
    DecisionInvalid => "decision_invalid", 422, Browser, Never, "Ответ не подходит к вопросу";
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
    ProfileUnknown => "profile_unknown", 422, Browser, Never, "Пресет инструкций не найден";
    WorkdirNotAllowed => "workdir_not_allowed", 422, Browser, Never, "Рабочий каталог вне разрешённых корней";
    ResumeHarnessUnsupported => "resume_harness_unsupported", 422, Browser, Never, "Продолжение истории доступно только для Claude Code";
    GroupNotFound => "group_not_found", 404, Browser, Never, "Группа не найдена";
    GroupNameTaken => "group_name_taken", 409, Browser, Never, "Группа с таким именем уже существует";
    ArchiveActive => "archive_active", 409, Browser, Never, "В сессии есть активная работа";
    ConversationDeleted => "conversation_deleted", 404, Browser, Never, "Сессия удалена";
    ConversationLive => "conversation_live", 409, Browser, AfterCondition, "Сначала остановите сессию";
    AttachmentNotReady => "attachment_not_ready", 409, Browser, AfterCondition, "Вложение ещё не готово";
    AttachmentNotUploadable => "attachment_not_uploadable", 409, Browser, Never, "Загрузка вложения уже завершена или прекращена";
    AttachmentUnsupported => "attachment_unsupported", 422, Browser, Never, "Харнесс не принимает такое вложение";
    AttachmentUnavailable => "attachment_unavailable", 409, Browser, Never, "Вложение больше недоступно";
    OffsetMismatch => "offset_mismatch", 409, Browser, AfterCondition, "Смещение загрузки не совпадает с принятым";
    UploadInProgress => "upload_in_progress", 409, Browser, AfterCondition, "Вложение уже загружается";
    ChecksumMismatch => "checksum_mismatch", 422, Browser, Never, "Содержимое не совпало с контрольной суммой";
    AttachmentConflict => "attachment_conflict", 409, Browser, Never, "Вложение уже завершено с другой контрольной суммой";
    PresetNotFound => "preset_not_found", 404, Browser, Never, "Пресет не найден";
    PresetBuiltin => "preset_builtin", 409, Browser, Never, "Встроенный пресет нельзя изменить или удалить";
    PresetRevisionConflict => "preset_revision_conflict", 409, Browser, AfterCondition, "Пресет уже изменён";
    PresetTitleTaken => "preset_title_taken", 409, Browser, Never, "Пресет с таким названием уже есть";
    PresetLimitReached => "preset_limit_reached", 409, Browser, AfterCondition, "Слишком много пресетов";
    PresetInvalid => "preset_invalid", 422, Browser, Never, "Некорректный пресет";
    SpawnWhitelistInvalid => "spawn_whitelist_invalid", 422, Browser, Never, "Некорректный белый список";
    LimitSettingsInvalid => "limit_settings_invalid", 422, Browser, Never, "Некорректные пороги лимитов";
    NotDelegated => "not_delegated", 409, Browser, Never, "Сессия не запущена агентом";
    WorktreeRemoved => "worktree_removed", 409, Delivery, Never, "Рабочая копия уже убрана";
    WorktreeNotRemoved => "worktree_not_removed", 409, Browser, AfterCondition, "Сначала примите результат: рабочая копия ещё на машине";
}

/// Причины отказа в `details.reason` и код, с которым каждая уходит владельцу:
/// конфликт состояния идёт 409, ошибка самого запроса 400.
macro_rules! error_reasons {
    ($($variant:ident => $wire:literal, $code:ident;)+) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, ToSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum ErrorReason {
            $($variant),+
        }

        impl ErrorReason {
            pub const ALL: &'static [ErrorReason] = &[$(ErrorReason::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }

            pub fn code(self) -> ErrorCode {
                match self {
                    $(Self::$variant => ErrorCode::$code),+
                }
            }

            pub fn parse(value: &str) -> Option<Self> {
                match value {
                    $($wire => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

error_reasons! {
    ClosedWithoutDecision => "closed_without_decision", RequestNotOpen;
    GenerationAdvanced => "generation_advanced", RequestNotOpen;
    InvalidIdempotencyKey => "invalid_idempotency_key", InvalidRequest;
    InputNotQueued => "input_not_queued", InvalidState;
    NoRunningTurn => "no_running_turn", InvalidState;
    RequestOpen => "request_open", InvalidState;
    Compacting => "compacting", InvalidState;
    UnsupportedHarness => "unsupported_harness", InvalidState;
    CapabilityMissing => "capability_missing", CapabilityMissing;
    RevisionConflict => "revision_conflict", InvalidState;
    SteerInFlight => "steer_in_flight", InvalidState;
    EmptyContent => "empty_content", InvalidRequest;
    ContentTooLong => "content_too_long", InvalidRequest;
    AttachmentUnavailable => "attachment_unavailable", AttachmentUnavailable;
    AttachmentNotReady => "attachment_not_ready", AttachmentNotReady;
    AttachmentNotFound => "attachment_not_found", AttachmentUnavailable;
    RunInProgress => "run_in_progress", InvalidState;
    NoLiveExecution => "no_live_execution", InvalidState;
    ConversationClosed => "conversation_closed", ConversationClosed;
    NulCharacter => "nul_character", InvalidRequest;
}

/// Отказ команды управления: код по причине, сама причина в `details.reason`.
pub fn refusal(reason: &'static str) -> ApiError {
    match ErrorReason::parse(reason) {
        Some(known) => ApiError::new(known.code()).with_details(ErrorDetails {
            reason: Some(known.as_str().into()),
            ..Default::default()
        }),
        None => {
            tracing::warn!(
                kind = "unlisted_error_reason",
                reason,
                "refusal reason is not in ErrorReason"
            );
            ApiError::new(ErrorCode::InvalidRequest)
        }
    }
}

fn invalid_field(field: &'static str) -> ApiError {
    ApiError::new(ErrorCode::InvalidRequest).with_details(ErrorDetails {
        field: Some(field.into()),
        ..Default::default()
    })
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
            E::PresetUnknown => ErrorCode::ProfileUnknown,
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
            E::InvalidContent(reason) => match ErrorReason::parse(reason) {
                Some(_) => refusal(reason),
                None => invalid_field("content"),
            },
            E::Attachment(error) => error.into(),
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

/// Детали ответа владельцу; содержимое решения не должно попадать в журналы ошибок.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct ErrorDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub reasons: Option<Vec<ArchiveReason>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub winner_decision: Option<super::conversations::DecisionBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(
        nullable = false,
        pattern = "^(open|decision_saved|decision_transmitting|decision_applied|decision_unconfirmed|decision_not_applied|closed_without_decision)$"
    )]
    pub current_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = ErrorReason, nullable = false)]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub question_id: Option<String>,
    /// Поле запроса, которое не прошло проверку (`preset_invalid`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub field: Option<String>,
    /// Текущая версия пресета (`preset_revision_conflict`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub current_revision: Option<i64>,
}

impl ErrorDetails {
    pub fn archive_active(reasons: Vec<ArchiveReason>) -> Self {
        Self {
            reasons: Some(reasons),
            ..Default::default()
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
    pub details: Option<Box<ErrorDetails>>,
    pub correlation_id: Uuid,
    /// Текущее смещение загрузки вложения для заголовка `Upload-Offset`.
    pub upload_offset: Option<u64>,
    /// Ошибка получена из временного сбоя базы (40P01/40001), а не из любого `Internal`.
    transient: bool,
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
    pub details: Option<Box<ErrorDetails>>,
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
            upload_offset: None,
            transient: false,
        }
    }

    pub fn with_details(mut self, details: ErrorDetails) -> Self {
        self.details = Some(Box::new(details));
        self
    }

    pub(crate) fn transient(&self) -> bool {
        self.transient
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
        let mut response = (self.status, Json(body)).into_response();
        if let Some(offset) = self.upload_offset {
            response
                .headers_mut()
                .insert("upload-offset", axum::http::HeaderValue::from(offset));
        }
        response
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        let info = dberr::describe(&error);
        let mut api = ApiError::new(if info.is_unavailable() {
            ErrorCode::DbUnavailable
        } else {
            ErrorCode::Internal
        });
        // Повторять имеет смысл только временный сбой самой базы, а не всякий
        // `Internal`: детерминированный отказ не изменится от второй попытки.
        api.transient = crate::db::is_transient(&error);
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
            SubmitError::Attachment(error) => error.into(),
            SubmitError::InvalidRequest {
                reason: "invalid_retry",
            } => {
                let mut error = ApiError::new(ErrorCode::InvalidRequest);
                error.status = StatusCode::UNPROCESSABLE_ENTITY;
                error
            }
            SubmitError::InvalidRequest { reason } => refusal(reason),
            SubmitError::InvalidContent { .. } => invalid_field("content"),
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
            ControlError::InvalidRequest { reason } => refusal(reason),
            ControlError::Db(error) => error.into(),
        }
    }
}

impl From<crate::deletion::DeleteError> for ApiError {
    fn from(error: crate::deletion::DeleteError) -> Self {
        use crate::deletion::DeleteError;
        match error {
            DeleteError::NotFound => ApiError::new(ErrorCode::NotFound),
            DeleteError::Deleted => ApiError::new(ErrorCode::ConversationDeleted),
            DeleteError::Live => ApiError::new(ErrorCode::ConversationLive),
            DeleteError::WorktreeNotRemoved => ApiError::new(ErrorCode::WorktreeNotRemoved),
            DeleteError::Referenced | DeleteError::NotTrashed => {
                ApiError::new(ErrorCode::InvalidState)
            }
            DeleteError::IdempotencyConflict => ApiError::new(ErrorCode::IdempotencyConflict),
            DeleteError::Db(error) => error.into(),
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
            ExecutionError::WorktreeRemoved => ApiError::new(ErrorCode::WorktreeRemoved),
            ExecutionError::InvalidTransition { .. } => ApiError::new(ErrorCode::InvalidState),
            ExecutionError::Db(error) => error.into(),
        }
    }
}

impl From<crate::session_tools::accept::AcceptError> for ApiError {
    fn from(error: crate::session_tools::accept::AcceptError) -> Self {
        use crate::session_tools::accept::AcceptError as E;
        match error {
            E::NotFound | E::NotParent => ApiError::new(ErrorCode::NotFound),
            E::NotDelegated => ApiError::new(ErrorCode::NotDelegated),
            E::IdempotencyConflict => ApiError::new(ErrorCode::IdempotencyConflict),
            E::Db(error) => error.into(),
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
            E::ResumeHarnessUnsupported => ErrorCode::ResumeHarnessUnsupported,
            E::GroupNotFound => ErrorCode::GroupNotFound,
            E::Invalid(field) => return invalid_field(field),
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
            E::Invalid(field) => return invalid_field(field),
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

impl From<crate::presets::PresetError> for ApiError {
    fn from(e: crate::presets::PresetError) -> Self {
        use crate::presets::PresetError as E;
        match e {
            E::NotFound => Self::new(ErrorCode::PresetNotFound),
            E::Builtin => Self::new(ErrorCode::PresetBuiltin),
            E::RevisionConflict { current } => Self::new(ErrorCode::PresetRevisionConflict)
                .with_details(ErrorDetails {
                    current_revision: Some(current),
                    ..Default::default()
                }),
            E::TitleTaken => Self::new(ErrorCode::PresetTitleTaken),
            E::LimitReached => Self::new(ErrorCode::PresetLimitReached),
            E::Invalid { field } => {
                Self::new(ErrorCode::PresetInvalid).with_details(ErrorDetails {
                    field: Some(field.to_owned()),
                    ..Default::default()
                })
            }
            E::IdempotencyConflict => Self::new(ErrorCode::IdempotencyConflict),
            E::Db(e) => e.into(),
        }
    }
}

impl From<crate::attachments::AttachmentError> for ApiError {
    fn from(error: crate::attachments::AttachmentError) -> Self {
        use crate::attachments::AttachmentError as E;
        let offset = error.upload_offset();
        let code = match error {
            E::NotFound => ErrorCode::NotFound,
            E::TooLarge | E::ExcessBytes { .. } | E::TooMany => ErrorCode::ResourceLimit,
            E::InvalidName | E::Invalid | E::BodyInterrupted { .. } => ErrorCode::InvalidRequest,
            E::ChecksumMismatch => ErrorCode::ChecksumMismatch,
            E::OffsetMismatch { .. } => ErrorCode::OffsetMismatch,
            E::UploadInProgress { .. } => ErrorCode::UploadInProgress,
            E::NotUploadable => ErrorCode::AttachmentNotUploadable,
            E::NotReady => ErrorCode::AttachmentNotReady,
            E::Unavailable => ErrorCode::AttachmentUnavailable,
            E::Unsupported => ErrorCode::AttachmentUnsupported,
            E::Conflict => ErrorCode::AttachmentConflict,
            E::InvalidState => ErrorCode::InvalidState,
            E::TooManyUploads => ErrorCode::TooManyRequests,
            E::CapabilityMissing => ErrorCode::CapabilityMissing,
            E::LimitsMismatch => ErrorCode::LimitsMismatch,
            E::IdempotencyConflict => ErrorCode::IdempotencyConflict,
            E::Db(error) => return error.into(),
            E::Blob(error) => {
                let api = ApiError::new(ErrorCode::Internal);
                tracing::error!(
                    kind = "attachment_blob_error",
                    error = %error,
                    correlation_id = %api.correlation_id,
                    "blob store failed"
                );
                return api;
            }
        };
        let mut api = ApiError::new(code);
        api.upload_offset = offset;
        api
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted_after<'a>(source: &'a str, marker: &str) -> Vec<&'a str> {
        source
            .match_indices(marker)
            .filter_map(|(at, _)| {
                let rest = &source[at + marker.len()..];
                rest.find('"').map(|end| &rest[..end])
            })
            .collect()
    }

    /// Причины, которые `control.rs` действительно отдаёт наружу: литералы в
    /// полях `reason` литералов `ControlError::InvalidRequest`. Совпадения в
    /// шаблонах `match` (`SubmitError::InvalidContent { reason: "empty" }`)
    /// отказами не являются.
    fn control_refusal_reasons(source: &str) -> Vec<&str> {
        const MARKER: &str = "ControlError::InvalidRequest";
        let mut reasons = Vec::new();
        let mut rest = source;
        while let Some(at) = rest.find(MARKER) {
            rest = &rest[at + MARKER.len()..];
            let Some(open) = rest.find('{') else { break };
            rest = &rest[open + 1..];
            let Some(field) = rest.trim_start().strip_prefix("reason:") else {
                continue;
            };
            let Some(literal) = field.trim_start().strip_prefix('"') else {
                continue;
            };
            if let Some(end) = literal.find('"') {
                reasons.push(&literal[..end]);
            }
        }
        reasons
    }

    #[test]
    fn control_refusals_ignore_match_patterns() {
        let source = r#"
            .map_err(|error| match error {
                SubmitError::InvalidContent { reason: "empty" } => {
                    ControlError::InvalidRequest {
                        reason: "empty_content",
                    }
                }
                _ => ControlError::InvalidRequest {
                    reason: "content_too_long",
                },
            })
        "#;
        assert_eq!(
            control_refusal_reasons(source),
            vec!["empty_content", "content_too_long"]
        );
    }

    #[test]
    fn every_control_refusal_reason_is_published() {
        let control = include_str!("../delivery/control.rs");
        let idempotency = include_str!("../idempotency.rs");
        let mut found = control_refusal_reasons(control);
        found.extend(quoted_after(idempotency, "Err(\""));
        assert!(!found.is_empty());
        let unlisted: Vec<_> = found
            .into_iter()
            .filter(|reason| ErrorReason::parse(reason).is_none())
            .collect();
        assert!(unlisted.is_empty(), "add to error_reasons!: {unlisted:?}");
    }

    #[test]
    fn state_conflicts_answer_409_and_request_errors_400() {
        for reason in ErrorReason::ALL {
            let error = refusal(reason.as_str());
            assert_eq!(error.code, reason.code(), "{reason:?}");
            assert_eq!(
                error.details.as_ref().and_then(|d| d.reason.as_deref()),
                Some(reason.as_str())
            );
        }
        for reason in [
            "input_not_queued",
            "revision_conflict",
            "steer_in_flight",
            "no_running_turn",
            "request_open",
            "compacting",
            "unsupported_harness",
            "run_in_progress",
            "no_live_execution",
            "attachment_not_ready",
            "attachment_unavailable",
            "attachment_not_found",
            "capability_missing",
            "conversation_closed",
        ] {
            assert_eq!(refusal(reason).status, StatusCode::CONFLICT, "{reason}");
        }
        for reason in [
            "invalid_idempotency_key",
            "empty_content",
            "nul_character",
            "content_too_long",
        ] {
            assert_eq!(refusal(reason).status, StatusCode::BAD_REQUEST, "{reason}");
        }
    }

    #[test]
    fn unknown_reason_stays_a_bare_invalid_request() {
        let error = refusal("something_new");
        assert_eq!(error.code, ErrorCode::InvalidRequest);
        assert!(error.details.is_none());
    }
}
