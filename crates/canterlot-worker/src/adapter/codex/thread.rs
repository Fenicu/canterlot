// Рукопожатие и параметры потоков `codex app-server`: initialize, чтение
// конфигурации и аккаунта, developerInstructions и thread/start|resume.
// Ответы config/read и account/read не логируются и не хранятся.
use super::process::LaunchError;
use crate::adapter::{InstructionMode, InstructionSpec};
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
// `thread_*` — политика, которую сервер показал при старте треда в режиме владельца:
// запас, когда конфигурация владельца её не задаёт.
pub struct OwnerDefaults {
    pub approval_policy: Option<Value>,
    pub sandbox_mode: Option<String>,
    pub sandbox_workspace_write: Option<Value>,
    pub developer_instructions: Option<String>,
    thread_approval_policy: Option<Value>,
    thread_sandbox: Option<Value>,
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
            .field("thread_policy", &self.thread_approval_policy.is_some())
            .finish()
    }
}

impl OwnerDefaults {
    /// Политика из ответа thread/start, запущенного без полей политики (режим владельца).
    pub fn observe_thread(&mut self, approval: &Value, sandbox: &Value) {
        self.thread_approval_policy = (!approval.is_null()).then(|| approval.clone());
        self.thread_sandbox = (!sandbox.is_null()).then(|| sandbox.clone());
    }

    /// Новое чтение config/read сохраняет наблюдение старта треда.
    pub fn reread(&mut self, result: &Value) {
        let mut next = owner_defaults_from(result);
        next.thread_approval_policy = self.thread_approval_policy.take();
        next.thread_sandbox = self.thread_sandbox.take();
        *self = next;
    }
}

// Ответ config/read — `{config, origins, layers}` (схема ConfigReadResponse).
pub fn owner_defaults_from(result: &Value) -> OwnerDefaults {
    let config = result.get("config").unwrap_or(&Value::Null);
    OwnerDefaults {
        approval_policy: config
            .get("approval_policy")
            .filter(|value| !value.is_null())
            .cloned(),
        sandbox_mode: config
            .get("sandbox_mode")
            .and_then(Value::as_str)
            .map(str::to_owned),
        sandbox_workspace_write: config
            .get("sandbox_workspace_write")
            .filter(|value| value.is_object())
            .cloned(),
        developer_instructions: config
            .get("developer_instructions")
            .and_then(Value::as_str)
            .map(str::to_owned),
        thread_approval_policy: None,
        thread_sandbox: None,
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
    ResumeFailed,
    Protocol,
}

impl StartError {
    pub fn reason(&self) -> &'static str {
        match self {
            StartError::AuthRequired => "auth_required",
            StartError::ResumeFailed => "resume_failed",
            StartError::Protocol => "protocol",
        }
    }
}

pub fn probe_initialize_params(worker_version: &str) -> Value {
    serde_json::json!({
        "clientInfo": {"name": "canterlot", "title": "canterlot", "version": worker_version},
        "capabilities": {"experimentalApi": false},
    })
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

/// Примечание о смене пресета в начале developerInstructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeNote<'a> {
    /// Смены нет (первый запуск).
    None,
    /// Новый пресет: название и версия.
    To { title: &'a str, revision: u64 },
    /// Возврат к «Без инструкций».
    ToClean,
    /// Переход на «Как у владельца».
    ToOwner,
}

/// Текст инструкций по режиму пресета: владелец хранит свой текст и блок сервера,
/// «Без инструкций» — пусто (перекрывает owner developer_instructions из конфига),
/// пресет — его текст. Примечание о смене идёт первой строкой.
pub fn compose_developer_instructions(
    spec: &InstructionSpec,
    owner: &OwnerDefaults,
    server_block: &str,
    note: ChangeNote<'_>,
) -> String {
    // Текст пресета — данные сообщения: в логи и Debug не попадает.
    let mut body = match spec.mode {
        InstructionMode::Owner => match owner.developer_instructions.as_deref() {
            Some(text) if !text.is_empty() => format!("{text}\n\n{server_block}"),
            _ => server_block.to_owned(),
        },
        // Пусто перекрывает owner developer_instructions из связанного конфига.
        InstructionMode::Clean => String::new(),
        InstructionMode::Preset => spec.text.clone(),
    };
    let prefix = match note {
        ChangeNote::None => None,
        ChangeNote::To { title, revision } => Some(format!(
            "[canterlot] The instructions preset of this conversation changed to «{title}» (revision {revision}); it replaces any earlier canterlot preset instructions in this thread."
        )),
        ChangeNote::ToClean => Some("[canterlot] This conversation no longer uses an instructions preset; disregard earlier canterlot preset instructions in this thread.".to_owned()),
        ChangeNote::ToOwner => Some("[canterlot] This conversation now uses the owner's own configuration; disregard earlier canterlot preset instructions in this thread.".to_owned()),
    };
    if let Some(prefix) = prefix {
        if body.is_empty() {
            body = prefix;
        } else {
            body = format!("{prefix}\n{body}");
        }
    }
    body
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

// SandboxPolicy (схема turn/start) из sandbox_mode и [sandbox_workspace_write] конфигурации.
fn sandbox_policy_from_config(mode: &str, write: Option<&Value>) -> Option<Value> {
    match mode {
        "read-only" => Some(serde_json::json!({"type": "readOnly"})),
        "danger-full-access" => Some(serde_json::json!({"type": "dangerFullAccess"})),
        "workspace-write" => {
            let mut policy = serde_json::Map::new();
            policy.insert("type".to_owned(), "workspaceWrite".into());
            for (toml, schema) in [
                ("writable_roots", "writableRoots"),
                ("network_access", "networkAccess"),
                ("exclude_tmpdir_env_var", "excludeTmpdirEnvVar"),
                ("exclude_slash_tmp", "excludeSlashTmp"),
            ] {
                if let Some(value) = write.and_then(|write| write.get(toml)) {
                    policy.insert(schema.to_owned(), value.clone());
                }
            }
            Some(Value::Object(policy))
        }
        _ => None,
    }
}

// Политика для turn/start. Владелец: значения его конфигурации, иначе то, что
// сервер показал при старте треда владельцем, иначе умолчания Codex для
// недоверенного проекта (on-request, только чтение): возврат к «владельцу»
// всегда снимает режим, выбранный раньше.
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
            let approval = owner
                .approval_policy
                .clone()
                .or_else(|| owner.thread_approval_policy.clone())
                .unwrap_or_else(|| "on-request".into());
            let sandbox = owner
                .sandbox_mode
                .as_deref()
                .and_then(|mode| {
                    sandbox_policy_from_config(mode, owner.sandbox_workspace_write.as_ref())
                })
                .or_else(|| owner.thread_sandbox.clone())
                .unwrap_or_else(|| serde_json::json!({"type": "readOnly", "networkAccess": false}));
            (Some(approval), Some(sandbox))
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
        // По схеме поле необязательно (default: []).
        instruction_sources: result
            .get("instructionSources")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        cli_version: thread
            .get("cliVersion")
            .and_then(Value::as_str)
            .ok_or(StartError::Protocol)?
            .to_owned(),
    })
}
