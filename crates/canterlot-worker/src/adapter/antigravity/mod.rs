//! Адаптер Antigravity: `agy acp` (JSON-RPC 2.0 в stdio) на исполнение.
//! Задача 1: запуск, resume и каталог; ходы, запросы и лимиты приходят позже.

pub mod catalog;
pub mod events;
pub mod input;
pub mod process;
#[cfg(feature = "synthetic-adapter")]
pub mod replay;
pub mod rpc;
pub mod session;
pub mod version;

pub use self::catalog::AntigravityCatalog;

use self::{
    process::{PrepareError, Process, StdoutLine},
    rpc::{Incoming, RpcClient},
    session::{SessionError, SessionInfo},
};
use crate::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, AgentCallResult, Capabilities, CloseMode,
        CompactOutcome, DecisionOutcome, InstructionSpec, InterruptOutcome, StartSpec, Started,
        SteerOutcome, Support, Transmission, TransmitOutcome,
    },
    config::AntigravityConfig,
    containment::Containment,
};
use canterlot_proto::requests::DecisionPayload;
use std::{collections::VecDeque, path::PathBuf, time::Duration};
use tokio::time::timeout;
use uuid::Uuid;

const STEP: Duration = Duration::from_secs(20);
const KILL_WAIT: Duration = Duration::from_secs(2);

pub struct AntigravityAdapter {
    cfg: AntigravityConfig,
    exec_dir: PathBuf,
    #[allow(dead_code)] // нужен задачам 2-4: сроки ходов и передач
    deadline: Duration,
    ready_deadline: Duration,
    stop_grace: Duration,
    proc: Option<Process>,
    rpc: RpcClient,
    events: VecDeque<AdapterEvent>,
    session: Option<SessionInfo>,
    instructions: InstructionSpec,
    workdir: PathBuf,
    #[allow(dead_code)] // нужен задачам 2-4: resume и отчёт
    native_conversation_id: String,
    eof: bool,
    exited: bool,
    exit_reported: bool,
    start_attempted: bool,
}

impl AntigravityAdapter {
    pub fn new(cfg: AntigravityConfig, exec_dir: PathBuf, deadline: Duration) -> Self {
        Self {
            cfg,
            exec_dir,
            deadline,
            ready_deadline: STEP,
            stop_grace: Duration::from_secs(5),
            proc: None,
            rpc: RpcClient::new(),
            events: VecDeque::new(),
            session: None,
            instructions: InstructionSpec {
                mode: crate::adapter::InstructionMode::Owner,
                text: String::new(),
                preset_id: None,
                revision: 0,
                title: String::new(),
            },
            workdir: PathBuf::new(),
            native_conversation_id: String::new(),
            eof: false,
            exited: false,
            exit_reported: false,
            start_attempted: false,
        }
    }

    pub fn with_ready_deadline(mut self, ready_deadline: Duration) -> Self {
        self.ready_deadline = ready_deadline;
        self
    }

    fn diagnostic(&mut self, kind: &'static str, required: bool) {
        self.events.push_back(AdapterEvent::Diagnostic {
            kind,
            event_type: kind.into(),
            required,
            field_names: Vec::new(),
        });
    }

    fn has_diagnostic(&self, kind: &str) -> bool {
        self.events.iter().any(|event| {
            matches!(event, AdapterEvent::Diagnostic { kind: event_kind, .. } if *event_kind == kind)
        })
    }

    async fn abandon(&mut self, group: &dyn Containment) {
        let Some(mut proc) = self.proc.take() else {
            return;
        };
        proc.close_stdin();
        let _ = group.signal_member(&proc.member(), rustix::process::Signal::KILL);
        let _ = timeout(KILL_WAIT, proc.wait()).await;
        self.exited = true;
    }

    fn session_error(error: SessionError) -> AdapterError {
        AdapterError::StartFailed(match error {
            SessionError::Rpc(_) => "acp_request_rejected",
            SessionError::Eof => "process_exited",
            SessionError::Timeout => "acp_timeout",
            SessionError::Protocol => "invalid_response",
            SessionError::Io(_) => "cli_io_failed",
        })
    }

    async fn handshake(&mut self, spec: &StartSpec) -> Result<SessionInfo, AdapterError> {
        let timeout = self.ready_deadline.max(STEP);
        let proc = self
            .proc
            .as_mut()
            .ok_or(AdapterError::StartFailed("cli_not_started"))?;
        let initialized = session::initialize(proc, &mut self.rpc, timeout)
            .await
            .map_err(Self::session_error)?;
        let agent_version = initialized["agentInfo"]["version"]
            .as_str()
            .unwrap_or_default();
        if !version::is_verified(agent_version) && !self.has_diagnostic("untested_cli_version") {
            self.diagnostic("untested_cli_version", false);
        }
        let proc = self.proc.as_mut().expect("process checked above");
        let info = match &spec.native_conversation_id {
            Some(native_id) => {
                let found = session::contains_session(proc, &mut self.rpc, native_id, timeout)
                    .await
                    .map_err(Self::session_error)?;
                if !found {
                    return Err(AdapterError::StartFailed("conversation not found"));
                }
                session::resume(proc, &mut self.rpc, native_id, &self.instructions, timeout)
                    .await
                    .map_err(Self::session_error)?
            }
            None => session::new_session(
                proc,
                &mut self.rpc,
                &self.workdir,
                &self.instructions,
                timeout,
            )
            .await
            .map_err(Self::session_error)?,
        };
        if !matches!(spec.model.as_str(), "" | "default") {
            session::set_model(proc, &mut self.rpc, &info.session_id, &spec.model, timeout)
                .await
                .map_err(|_| AdapterError::StartFailed("model_rejected"))?;
        }
        if !spec.effort.is_empty() {
            session::set_effort(proc, &mut self.rpc, &info.session_id, &spec.effort, timeout)
                .await
                .map_err(|_| AdapterError::StartFailed("effort_rejected"))?;
        }
        Ok(info)
    }

    pub fn session(&self) -> Option<&SessionInfo> {
        self.session.as_ref()
    }
}

#[async_trait::async_trait]
impl Adapter for AntigravityAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: Support::Supported,
            resume: Support::Supported,
            interrupt: Support::Supported,
            approvals: Support::Unavailable("antigravity: task 3"),
            questions: Support::Unavailable("antigravity: task 3"),
            background_work: Support::Unavailable("antigravity: task 3"),
            interrupt_keeps_process: true,
        }
    }

    async fn start(
        &mut self,
        spec: &StartSpec,
        group: &dyn Containment,
    ) -> Result<Started, AdapterError> {
        if self.start_attempted {
            return Err(AdapterError::AlreadyStarted);
        }
        self.start_attempted = true;
        if !self.cfg.enabled {
            return Err(AdapterError::StartFailed("harness_disabled"));
        }
        let pinned = match process::prepare_pinned(&self.cfg).await {
            Ok(pinned) => pinned,
            Err(PrepareError::Version(version::VersionError::NotFound)) => {
                return Err(AdapterError::StartFailed("cli_not_found"));
            }
            Err(PrepareError::Version(version::VersionError::BelowMinimum(_))) => {
                return Err(AdapterError::StartFailed("cli_too_old"));
            }
            Err(PrepareError::Version(_)) => {
                return Err(AdapterError::StartFailed("cli_version_failed"));
            }
            Err(PrepareError::MissingBinary) => {
                return Err(AdapterError::StartFailed("cli_not_found"));
            }
            Err(PrepareError::MissingDataDir) => {
                return Err(AdapterError::StartFailed("data_dir_missing"));
            }
            Err(PrepareError::Io(_)) => {
                return Err(AdapterError::StartFailed("cli_pin_failed"));
            }
        };
        if matches!(pinned.version, version::CliVersion::UntestedNewer(_)) {
            self.diagnostic("untested_cli_version", false);
        }
        self.instructions = spec.instructions.clone();
        self.workdir = spec.workdir.clone();
        // Аргументы проверяются до spawn: неверный режим не оставляет процесс.
        if process::launch_args(&self.exec_dir, &spec.permission_mode).is_err() {
            return Err(AdapterError::StartFailed("unknown_permission_mode"));
        }
        self.proc = Some(
            Process::spawn(
                &pinned.executable,
                &self.exec_dir,
                &spec.workdir,
                &spec.permission_mode,
                group,
            )
            .map_err(|error| {
                AdapterError::StartFailed(match error {
                    process::LaunchError::UnknownPermissionMode => "unknown_permission_mode",
                    process::LaunchError::WorkdirNotDir => "workdir_not_dir",
                    process::LaunchError::CliNotFoundInContainment
                    | process::LaunchError::SpawnFailed(_) => "cli_launch_failed",
                    process::LaunchError::DataDirMissing
                    | process::LaunchError::SourceNotFound
                    | process::LaunchError::Io(_) => "cli_launch_failed",
                })
            })?,
        );
        match self.handshake(spec).await {
            Ok(info) => {
                self.native_conversation_id = info.session_id.clone();
                let proc = self.proc.as_ref().expect("spawned before handshake");
                let started = Started {
                    native_conversation_id: info.session_id.clone(),
                    cli_pid: proc.pid,
                    cli_start_ticks: proc.start_ticks,
                };
                self.session = Some(info);
                Ok(started)
            }
            Err(error) => {
                self.abandon(group).await;
                Err(error)
            }
        }
    }

    async fn transmit(
        &mut self,
        _input: &Transmission,
        _group: &dyn Containment,
    ) -> TransmitOutcome {
        TransmitOutcome::Rejected("antigravity: task 2".into())
    }

    async fn deliver_decision(
        &mut self,
        _request_id: &str,
        _decision: &DecisionPayload,
    ) -> DecisionOutcome {
        DecisionOutcome::NotApplied("antigravity: task 3".into())
    }

    async fn interrupt(&mut self) -> InterruptOutcome {
        InterruptOutcome::NotApplicable
    }

    async fn steer(&mut self, _input: &Transmission) -> SteerOutcome {
        SteerOutcome::NotApplicable("antigravity: task 2".into())
    }

    async fn compact(&mut self, _command: Uuid) -> CompactOutcome {
        CompactOutcome::NotApplicable
    }

    async fn agent_call_result(&mut self, _call_id: Uuid, _result: AgentCallResult) {}

    async fn next_event(&mut self) -> AdapterEvent {
        loop {
            if let Some(event) = self.events.pop_front() {
                return event;
            }
            if self.exit_reported {
                return std::future::pending().await;
            }
            let Some(proc) = self.proc.as_mut() else {
                self.exit_reported = true;
                return AdapterEvent::ProcessExited {
                    code: None,
                    signal: None,
                };
            };
            match proc.next_line().await {
                StdoutLine::Line(line) => match rpc::classify(&line) {
                    Incoming::Notification { .. } => {}
                    Incoming::ServerRequest { id, .. } => {
                        // Запросы сервера появятся в задаче 3; сейчас отвечаем отказом.
                        let message = RpcClient::error(&id, -32601, "Method not found");
                        let _ = proc.write_message(&message).await;
                    }
                    Incoming::Response { .. } => {}
                    Incoming::Invalid => self.diagnostic("invalid_frame", true),
                },
                StdoutLine::Oversized(_) => self.diagnostic("oversized_frame", true),
                StdoutLine::Eof => {
                    use std::os::unix::process::ExitStatusExt;
                    self.eof = true;
                    let status = proc.wait().await.ok();
                    self.exited = true;
                    self.exit_reported = true;
                    let (code, signal) =
                        status.map_or((None, None), |status| (status.code(), status.signal()));
                    return AdapterEvent::ProcessExited { code, signal };
                }
            }
        }
    }

    fn set_stop_grace(&mut self, grace: Duration) {
        self.stop_grace = grace;
    }

    async fn close(
        &mut self,
        mode: CloseMode,
        group: &dyn Containment,
    ) -> Result<(), AdapterError> {
        let Some(mut proc) = self.proc.take() else {
            return Ok(());
        };
        proc.close_stdin();
        if !self.exited {
            if matches!(mode, CloseMode::Force) {
                let _ = group.signal_member(&proc.member(), rustix::process::Signal::KILL);
                timeout(KILL_WAIT, proc.wait())
                    .await
                    .map_err(|_| AdapterError::Deadline)??;
            } else if timeout(self.stop_grace, proc.wait()).await.is_err() {
                let _ = group.signal_member(&proc.member(), rustix::process::Signal::TERM);
                if timeout(self.stop_grace.min(KILL_WAIT), proc.wait())
                    .await
                    .is_err()
                {
                    let _ = group.signal_member(&proc.member(), rustix::process::Signal::KILL);
                    timeout(KILL_WAIT, proc.wait())
                        .await
                        .map_err(|_| AdapterError::Deadline)??;
                }
            }
            self.exited = true;
        }
        Ok(())
    }
}

/// Профиль показывается только при таблице `[antigravity]`; каталог — после пробы.
pub fn catalog_entry(catalog: &AntigravityCatalog) -> crate::adapter::HarnessCatalog {
    catalog.harness()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_reserve_requests_for_task_3() {
        let adapter = AntigravityAdapter::new(
            AntigravityConfig::default(),
            PathBuf::from("/tmp"),
            Duration::from_millis(100),
        );
        let capabilities = adapter.capabilities();
        assert_eq!(capabilities.streaming, Support::Supported);
        assert_eq!(capabilities.resume, Support::Supported);
        assert_eq!(capabilities.interrupt, Support::Supported);
        assert_eq!(
            capabilities.approvals,
            Support::Unavailable("antigravity: task 3")
        );
        assert_eq!(
            capabilities.questions,
            Support::Unavailable("antigravity: task 3")
        );
        assert_eq!(
            capabilities.background_work,
            Support::Unavailable("antigravity: task 3")
        );
        assert!(capabilities.interrupt_keeps_process);
    }
}
