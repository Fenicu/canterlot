use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::ExitCode,
};

use canterlot_journal::JournalError;
use serde::Deserialize;
use uuid::Uuid;

use crate::containment::ContainmentError;

pub use crate::config::SupervisorTimers;

mod blocks;
mod bounds;
pub mod git_facts;
pub mod ipc;
mod run;
mod sj;

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("supervisor I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid supervisor spec")]
    Spec,
    #[error("supervisor secret file is missing, empty or readable by others")]
    Secret,
    #[error("execution journal failed: {0}")]
    Journal(#[from] JournalError),
    #[error("supervisor ipc failed: {0}")]
    Ipc(#[from] ipc::IpcError),
    #[error("process containment failed: {0}")]
    Containment(#[from] ContainmentError),
    #[error("invalid journal record: {0}")]
    Decode(#[from] prost::DecodeError),
}

impl SupervisorError {
    /// Отказ записи журнала исполнения: диск полон, квота, ошибка носителя. Журнал при
    /// этом цел, и запись можно повторить, когда хранилище вернётся.
    fn is_storage_failure(&self) -> bool {
        matches!(self, Self::Journal(JournalError::Io(_)))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorSpec {
    pub execution_id: Uuid,
    pub generation: u64,
    pub conversation_id: Uuid,
    pub protocol_version: u32,
    #[serde(default)]
    pub ipc_socket: Option<PathBuf>,
    /// Общий кэш успешных проб `<cli> --version`; без ключа кэш только в памяти.
    #[serde(default)]
    pub version_cache: Option<PathBuf>,
    /// Понижение приоритета CLI и его потомков; 0 — не понижать.
    #[serde(default)]
    pub agent_nice: u32,
    #[serde(default)]
    pub timers: SupervisorTimers,
    /// Короткие периоды тестов; боевая спека таблицы не содержит (`exec::spawn`).
    #[serde(default)]
    pub periods: crate::config::SupervisorPeriods,
    #[serde(default)]
    pub claude_code: Option<crate::config::ClaudeCodeConfig>,
    #[serde(default)]
    pub codex: Option<crate::config::CodexConfig>,
    #[serde(default)]
    pub opencode: Option<crate::config::OpenCodeConfig>,
    /// `[execution].delegate_mcp_servers` воркера: серверы владельца для делегата.
    #[serde(default)]
    pub delegate_mcp_servers: Vec<String>,
}

impl SupervisorSpec {
    /// Учётная запись лимитов харнесса провайдера: её сопровождение ставит на каждый факт
    /// лимита. Таблицы нет (старая спека) — `owner`.
    pub fn limit_account(&self, provider: &str) -> &str {
        let account = match provider {
            "claude" => self.claude_code.as_ref().map(|c| c.account.as_str()),
            "codex" => self.codex.as_ref().map(|c| c.account.as_str()),
            "opencode" => self.opencode.as_ref().map(|c| c.account.as_str()),
            _ => None,
        };
        account.unwrap_or(canterlot_proto::limits::OWNER_ACCOUNT)
    }
}

/// Спека на диске глазами управляющей службы: она может быть записана другим релизом,
/// поэтому читаются только поля проверки, а незнакомые ключи пропускаются. `SupervisorSpec`
/// читает лишь сопровождение, запущенное тем же релизом, что её записал.
#[derive(Clone, Debug, Deserialize)]
pub struct PersistedSpec {
    pub execution_id: Uuid,
    pub generation: u64,
    #[serde(default)]
    pub ipc_socket: Option<PathBuf>,
}

fn load(exec_dir: &Path) -> Result<(SupervisorSpec, Vec<u8>), SupervisorError> {
    let text = fs::read_to_string(exec_dir.join("spec.toml"))?;
    let spec: SupervisorSpec = toml::from_str(&text).map_err(|_| SupervisorError::Spec)?;
    if spec.protocol_version != canterlot_proto::limits::IPC_PROTOCOL_VERSION {
        return Err(SupervisorError::Spec);
    }
    let path = exec_dir.join("secret");
    let secret = fs::read(&path)?;
    if secret.is_empty() || fs::metadata(&path)?.permissions().mode() & 0o077 != 0 {
        return Err(SupervisorError::Secret);
    }
    Ok((spec, secret))
}

/// Итог исполнения по выходу процесса CLI: `(terminal, outcome)`.
/// Выход с кодом 0 после прерывания (SIGINT у Claude Code) означает остановку пользователем.
pub fn exit_outcome(
    code: Option<i32>,
    signal: Option<i32>,
    interrupted: bool,
) -> (&'static str, &'static str) {
    match (code, signal) {
        (Some(0), None) if interrupted => ("stopped", "user_stop"),
        (Some(0), None) => ("stopped", "normal_exit"),
        _ => ("failed", "crashed"),
    }
}

pub async fn run(exec_dir: PathBuf) -> Result<ExitCode, SupervisorError> {
    run::run(exec_dir).await
}

#[cfg(feature = "fault-injection")]
pub(crate) use crate::fault_injection::SupervisorFaults;

#[cfg(not(feature = "fault-injection"))]
pub(crate) struct SupervisorFaults;

#[cfg(not(feature = "fault-injection"))]
impl SupervisorFaults {
    pub fn from_env() -> Self {
        Self
    }
    pub fn abort_at(&self, _point: &str) {}
    pub fn will_abort(&self, _point: &str) -> bool {
        false
    }
    pub fn gate_held(&self, _exec_dir: &Path) -> bool {
        false
    }
    pub fn attach_held(&self) -> bool {
        false
    }
    pub fn decision_held(&self) -> bool {
        false
    }
    pub fn take_survivor(&self, _exec_dir: &Path) -> bool {
        false
    }
    pub fn take_error(&self) -> bool {
        false
    }
}
