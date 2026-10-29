// Рукопожатие и параметры потоков `codex app-server`: initialize, чтение
// конфигурации и аккаунта, developerInstructions и thread/start|resume.
// Ответы config/read и account/read не логируются и не хранятся.
use super::process::LaunchError;
use serde_json::Value;
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Owner,
    Ask,
    Bypass,
    ReadOnly,
}

impl PermissionMode {
    pub fn parse(mode: &str) -> Result<Self, LaunchError> {
        match mode {
            "owner" => Ok(PermissionMode::Owner),
            "ask" => Ok(PermissionMode::Ask),
            "bypass" => Ok(PermissionMode::Bypass),
            "read_only" => Ok(PermissionMode::ReadOnly),
            _ => Err(LaunchError::UnknownPermissionMode),
        }
    }
}

// Действующие значения владельца из config/read; отладка показывает только наличие полей.
pub struct OwnerDefaults {
    pub approval_policy: Option<Value>,
    pub sandbox_mode: Option<String>,
    pub sandbox_workspace_write: Option<Value>,
    pub developer_instructions: Option<String>,
}

impl fmt::Debug for OwnerDefaults {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnerDefaults")
            .field("approval_policy", &self.approval_policy.is_some())
            .field("sandbox_mode", &self.sandbox_mode.is_some())
            .field(
                "sandbox_workspace_write",
                &self.sandbox_workspace_write.is_some(),
            )
            .field(
                "developer_instructions",
                &self.developer_instructions.is_some(),
            )
            .finish()
    }
}

pub fn owner_defaults_from(result: &Value) -> OwnerDefaults {
    OwnerDefaults {
        approval_policy: result.get("approval_policy").cloned(),
        sandbox_mode: result
            .get("sandbox_mode")
            .and_then(Value::as_str)
            .map(str::to_owned),
        sandbox_workspace_write: result.get("sandbox_workspace_write").cloned(),
        developer_instructions: result
            .get("developer_instructions")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

// Действующие параметры потока из ответа thread/start|resume.
pub struct Effective {
    pub thread_id: String,
    pub model: String,
    pub approval_policy: Value,
    pub sandbox: Value,
    pub instruction_sources: usize,
    pub cli_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    AuthRequired,
    UnsupportedInstructions,
    ResumeFailed,
    Protocol,
}

impl StartError {
    pub fn reason(&self) -> &'static str {
        match self {
            StartError::AuthRequired => "auth_required",
            StartError::UnsupportedInstructions => "unsupported_instructions",
            StartError::ResumeFailed => "resume_failed",
            StartError::Protocol => "protocol",
        }
    }
}

pub fn initialize_params(worker_version: &str) -> Value {
    serde_json::json!({
        "clientInfo": {"name": "canterlot", "title": "canterlot", "version": worker_version},
        "capabilities": {
            "experimentalApi": true,
            "requestAttestation": false,
            "optOutNotificationMethods": [
                "turn/moderationMetadata",
                "remoteControl/status/changed",
                "thread/readState/changed",
                "turn/diff/updated",
            ],
        },
    })
}

pub fn account_read_params() -> Value {
    // Без обновления токена: платного запроса нет.
    serde_json::json!({"refreshToken": false})
}

pub fn config_read_params(cwd: &Path) -> Value {
    serde_json::json!({"cwd": cwd.to_string_lossy(), "includeLayers": false})
}

// Нет аккаунта — запуск невозможен, владелец должен войти сам.
pub fn check_account(result: &Value) -> Result<(), StartError> {
    if result.get("account").is_some_and(|a| !a.is_null()) {
        Ok(())
    } else {
        Err(StartError::AuthRequired)
    }
}

// Профиль владельца сохраняет его текст и добавляет блок сервера; остальные
// профили до задачи 7 плана 2c не поддерживаются.
pub fn compose_developer_instructions(
    profile: &str,
    owner: &OwnerDefaults,
    server_block: &str,
) -> Result<String, StartError> {
    if profile == "owner" || profile.is_empty() {
        Ok(match owner.developer_instructions.as_deref() {
            Some(text) if !text.is_empty() => format!("{text}\n\n{server_block}"),
            _ => server_block.to_owned(),
        })
    } else {
        Err(StartError::UnsupportedInstructions)
    }
}

fn thread_policy(mode: PermissionMode) -> (Option<&'static str>, Option<&'static str>) {
    match mode {
        // Везде yolo, кроме чтения: песочница только в read_only.
        PermissionMode::Ask => (Some("on-request"), Some("danger-full-access")),
        PermissionMode::Bypass => (Some("never"), Some("danger-full-access")),
        PermissionMode::ReadOnly => (Some("never"), Some("read-only")),
        // Владелец: действует его конфигурация, поля опускаем.
        PermissionMode::Owner => (None, None),
    }
}

fn base_params(
    mode: PermissionMode,
    model: Option<&str>,
    workdir: &Path,
    developer_instructions: &str,
) -> serde_json::Map<String, Value> {
    let mut params = serde_json::Map::new();
    let (approval, sandbox) = thread_policy(mode);
    if let Some(policy) = approval {
        params.insert("approvalPolicy".to_owned(), policy.into());
    }
    params.insert("approvalsReviewer".to_owned(), "user".into());
    params.insert("cwd".to_owned(), workdir.to_string_lossy().into());
    if let Some(sandbox) = sandbox {
        params.insert("sandbox".to_owned(), sandbox.into());
    }
    if let Some(model) = model {
        params.insert("model".to_owned(), model.into());
    }
    params.insert(
        "developerInstructions".to_owned(),
        developer_instructions.into(),
    );
    params
}

pub fn thread_start_params(
    mode: PermissionMode,
    model: Option<&str>,
    workdir: &Path,
    developer_instructions: &str,
) -> Value {
    let mut params = base_params(mode, model, workdir, developer_instructions);
    params.insert("ephemeral".to_owned(), false.into());
    params.insert("serviceName".to_owned(), "canterlot".into());
    Value::Object(params)
}

pub fn thread_resume_params(
    thread_id: &str,
    mode: PermissionMode,
    model: Option<&str>,
    workdir: &Path,
    developer_instructions: &str,
) -> Value {
    let mut params = base_params(mode, model, workdir, developer_instructions);
    params.insert("threadId".to_owned(), thread_id.into());
    params.insert("excludeTurns".to_owned(), true.into());
    Value::Object(params)
}

// Имя типа песочницы в стиле схемы (readOnly) из значения конфигурации (read-only).
fn sandbox_type_name(mode: &str) -> &str {
    match mode {
        "read-only" => "readOnly",
        "danger-full-access" => "dangerFullAccess",
        "workspace-write" => "workspaceWrite",
        other => other,
    }
}

// Политика для turn/start: у владельца восстанавливаем его значения.
pub fn turn_policy(mode: PermissionMode, owner: &OwnerDefaults) -> (Option<Value>, Option<Value>) {
    match mode {
        PermissionMode::Ask => (
            Some("on-request".into()),
            Some(serde_json::json!({"type": "dangerFullAccess"})),
        ),
        PermissionMode::Bypass => (
            Some("never".into()),
            Some(serde_json::json!({"type": "dangerFullAccess"})),
        ),
        PermissionMode::ReadOnly => (
            Some("never".into()),
            Some(serde_json::json!({"type": "readOnly", "networkAccess": false})),
        ),
        PermissionMode::Owner => {
            let sandbox = match (&owner.sandbox_mode, &owner.sandbox_workspace_write) {
                (None, None) => None,
                (mode, write) => {
                    let mut obj = write
                        .as_ref()
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    if let Some(mode) = mode {
                        obj.insert("type".to_owned(), sandbox_type_name(mode).into());
                    }
                    Some(Value::Object(obj))
                }
            };
            (owner.approval_policy.clone(), sandbox)
        }
    }
}

pub fn parse_thread_result(result: &Value) -> Result<Effective, StartError> {
    let thread = result.get("thread").ok_or(StartError::Protocol)?;
    Ok(Effective {
        thread_id: thread
            .get("id")
            .and_then(Value::as_str)
            .ok_or(StartError::Protocol)?
            .to_owned(),
        model: result
            .get("model")
            .and_then(Value::as_str)
            .ok_or(StartError::Protocol)?
            .to_owned(),
        approval_policy: result
            .get("approvalPolicy")
            .cloned()
            .ok_or(StartError::Protocol)?,
        sandbox: result.get("sandbox").cloned().ok_or(StartError::Protocol)?,
        instruction_sources: result
            .get("instructionSources")
            .and_then(Value::as_array)
            .ok_or(StartError::Protocol)?
            .len(),
        cli_version: thread
            .get("cliVersion")
            .and_then(Value::as_str)
            .ok_or(StartError::Protocol)?
            .to_owned(),
    })
}
