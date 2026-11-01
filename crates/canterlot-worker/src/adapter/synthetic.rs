use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::ChildStdin,
    sync::{mpsc, watch},
};
use uuid::Uuid;

use super::{synthetic_cli::Request, *};
use crate::containment::Member;

enum Frame {
    Decision(String, DecisionOutcome),
    Ready(String),
    Accepted(Uuid),
    Interrupted(Uuid),
    RestartDue(bool),
    Event(AdapterEvent),
}

fn decode(value: Value, blocks: &mut HashMap<Uuid, Uuid>) -> Result<Frame, AdapterError> {
    let string = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or(AdapterError::Protocol)
    };
    let uuid = |key: &str| Uuid::parse_str(&string(key)?).map_err(|_| AdapterError::Protocol);
    let event = match string("type")?.as_str() {
        "ready" => return Ok(Frame::Ready(string("native_conversation_id")?)),
        "accepted" => return Ok(Frame::Accepted(uuid("command_id")?)),
        "interrupted" => return Ok(Frame::Interrupted(uuid("id")?)),
        "decision_applied" => {
            return Ok(Frame::Decision(
                string("request_id")?,
                DecisionOutcome::Applied(DecisionProof {
                    proof_kind: string("proof_kind")?,
                    payload: value["payload"].clone(),
                }),
            ));
        }
        "decision_not_applied" => {
            return Ok(Frame::Decision(
                string("request_id")?,
                DecisionOutcome::NotApplied(string("reason")?),
            ));
        }
        "request_opened" => AdapterEvent::RequestOpened {
            request_id: string("request_id")?,
            kind: string("kind")?,
            payload: value["payload"].clone(),
        },
        "request_closed" => AdapterEvent::RequestClosed {
            request_id: string("request_id")?,
            reason: string("reason")?,
        },
        "todo_updated" => AdapterEvent::TodoUpdated {
            lane: value
                .get("lane")
                .filter(|v| !v.is_null())
                .map(|_| {
                    blocks
                        .get(&uuid("lane")?)
                        .copied()
                        .ok_or(AdapterError::Protocol)
                })
                .transpose()?,
            items: serde_json::from_value(value["items"].clone())
                .map_err(|_| AdapterError::Protocol)?,
        },
        "phase" => AdapterEvent::Phase {
            phase: serde_json::from_value(value["phase"].clone())
                .map_err(|_| AdapterError::Protocol)?,
            tool_name: value["tool_name"].as_str().map(str::to_owned),
            summary: value["summary"].as_str().map(str::to_owned),
        },
        "block_started" => {
            let local_id = uuid("block_id")?;
            if blocks.contains_key(&local_id) {
                return Err(AdapterError::Protocol);
            }
            let parent = value
                .get("parent")
                .filter(|v| !v.is_null())
                .map(|_| {
                    blocks
                        .get(&uuid("parent")?)
                        .copied()
                        .ok_or(AdapterError::Protocol)
                })
                .transpose()?;
            let block_id = Uuid::now_v7();
            blocks.insert(local_id, block_id);
            AdapterEvent::BlockStarted {
                block_id,
                kind: serde_json::from_value(value["kind"].clone())
                    .map_err(|_| AdapterError::Protocol)?,
                input_command_id: value
                    .get("input_command_id")
                    .filter(|v| !v.is_null())
                    .map(|_| uuid("input_command_id"))
                    .transpose()?,
                parent,
                agent: string("agent")?,
                model: string("model")?,
                tool_name: value["tool_name"].as_str().map(str::to_owned),
                summary: value["summary"].as_str().map(str::to_owned),
                approved_by: None,
            }
        }
        "block_delta" => {
            let append = string("append")?;
            if append.len() > canterlot_proto::limits::MAX_BLOCK_DELTA_BYTES {
                return Err(AdapterError::Protocol);
            }
            AdapterEvent::BlockDelta {
                block_id: *blocks
                    .get(&uuid("block_id")?)
                    .ok_or(AdapterError::Protocol)?,
                append,
            }
        }
        "block_final" => AdapterEvent::BlockFinal {
            block_id: *blocks
                .get(&uuid("block_id")?)
                .ok_or(AdapterError::Protocol)?,
            text: string("text")?,
            summary: value["summary"].as_str().map(str::to_owned),
        },
        "media" => {
            let block_id = *blocks
                .get(&uuid("block_id")?)
                .ok_or(AdapterError::Protocol)?;
            let source = match value.get("data").and_then(Value::as_str) {
                Some(data) => {
                    use base64::Engine;
                    MediaSource::Bytes(
                        base64::engine::general_purpose::STANDARD
                            .decode(data)
                            .map_err(|_| AdapterError::Protocol)?,
                    )
                }
                None => MediaSource::Path(PathBuf::from(string("path")?)),
            };
            AdapterEvent::Media { block_id, source }
        }
        "restart_due" => {
            return Ok(Frame::RestartDue(
                value["restart"].as_bool().unwrap_or(false),
            ));
        }
        "background_work" => {
            let background = &value["background"];
            let tasks = background["tasks"]
                .as_array()
                .ok_or(AdapterError::Protocol)?;
            AdapterEvent::BackgroundWork {
                tasks: tasks
                    .iter()
                    .map(|task| {
                        Ok(BackgroundTask {
                            task_id: task["task_id"].as_str().unwrap_or_default().to_owned(),
                            kind: if task["kind"] == "agent" {
                                BackgroundKind::Agent
                            } else {
                                BackgroundKind::Bash
                            },
                            block_id: None,
                            parent_task_id: None,
                            description: task["description"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                            state: match task["state"].as_str() {
                                Some("running") => BackgroundState::Running,
                                Some("completed") => BackgroundState::Completed,
                                Some("failed") => BackgroundState::Failed,
                                Some("stopped") => BackgroundState::Stopped,
                                _ => return Err(AdapterError::Protocol),
                            },
                        })
                    })
                    .collect::<Result<_, _>>()?,
                wake_turn: background["wake_turn"].as_bool().unwrap_or(false),
            }
        }
        "run_adopted" => AdapterEvent::RunAdopted {
            input_command_id: uuid("input_command_id")?,
        },
        "completed" => {
            let input_command_id = uuid("input_command_id")?;
            blocks.clear();
            AdapterEvent::RunFinished {
                input_command_id,
                usage: serde_json::from_value(value["usage"].clone())
                    .map_err(|_| AdapterError::Protocol)?,
                context_tokens: value["context_tokens"]
                    .as_u64()
                    .ok_or(AdapterError::Protocol)?,
                context_window_tokens: value["context_window_tokens"]
                    .as_u64()
                    .ok_or(AdapterError::Protocol)?,
            }
        }
        "agent_call" => AdapterEvent::AgentCall {
            call_id: Uuid::now_v7(),
            tool: string("tool")?,
            arguments: value
                .get("arguments")
                .cloned()
                .filter(|v| v.is_object())
                .ok_or(AdapterError::Protocol)?,
        },
        "limit_observed" | "limit_reached" => {
            let window = serde_json::from_value(value["window"].clone())
                .map_err(|_| AdapterError::Protocol)?;
            let resets_at_ms = value["resets_at_ms"].as_u64();
            AdapterEvent::Limit(if value["type"] == "limit_observed" {
                let used = value["used_permille"]
                    .as_u64()
                    .filter(|v| *v <= 1000)
                    .ok_or(AdapterError::Protocol)?;
                LimitFact::Observed {
                    window,
                    used_permille: used as u16,
                    resets_at_ms,
                }
            } else {
                LimitFact::Reached {
                    window,
                    resets_at_ms,
                }
            })
        }
        "context_exhausted" => AdapterEvent::ContextExhausted,
        _ => AdapterEvent::Diagnostic {
            kind: "unknown_event",
            event_type: string("type")?,
            required: value
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            field_names: Vec::new(),
        },
    };
    Ok(Frame::Event(event))
}

pub struct SyntheticAdapter {
    profile: String,
    executable: Option<PathBuf>,
    transmit_deadline: Duration,
    close_grace: Duration,
    stdin: Option<ChildStdin>,
    frames: Option<mpsc::Receiver<(u64, Frame)>>,
    pending: VecDeque<AdapterEvent>,
    exited: Option<watch::Receiver<bool>>,
    cli: Option<Member>,
    started: bool,
    active: bool,
    settings: SettingsChange,
    workdir: PathBuf,
    native_id: Option<String>,
    pending_calls: std::collections::HashSet<Uuid>,
    /// Вводы с `Undetermined`: позднее `accepted` становится `InputAccepted`.
    unconfirmed: std::collections::HashSet<Uuid>,
    /// Отложенная смена настроек перезапускает CLI, как у настоящего адаптера.
    deferred_restart: bool,
    /// Поколение процесса CLI: кадры заменённого процесса отбрасываются,
    /// иначе его выход завершил бы исполнение после перезапуска.
    generation: u64,
}

impl SyntheticAdapter {
    pub fn new(profile: &str) -> Self {
        Self {
            profile: profile.into(),
            executable: None,
            transmit_deadline: DEFAULT_TRANSMIT_DEADLINE,
            close_grace: Duration::from_secs(1),
            stdin: None,
            frames: None,
            pending: VecDeque::new(),
            exited: None,
            cli: None,
            started: false,
            active: false,
            settings: SettingsChange {
                model: Some("synthetic-large".into()),
                effort: Some("medium".into()),
                permission_mode: Some("owner".into()),
                instructions: None,
            },
            workdir: PathBuf::new(),
            native_id: None,
            pending_calls: std::collections::HashSet::new(),
            unconfirmed: std::collections::HashSet::new(),
            deferred_restart: false,
            generation: 0,
        }
    }
    pub fn with_executable(mut self, executable: PathBuf) -> Self {
        self.executable = Some(executable);
        self
    }
    pub fn with_transmit_deadline(mut self, deadline: Duration) -> Self {
        self.transmit_deadline = deadline;
        self
    }
    pub fn with_close_grace(mut self, grace: Duration) -> Self {
        self.close_grace = grace;
        self
    }

    async fn send(&mut self, request: &Request) -> Result<(), AdapterError> {
        let mut bytes = serde_json::to_vec(request).map_err(|_| AdapterError::Protocol)?;
        bytes.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or(AdapterError::Protocol)?;
        stdin.write_all(&bytes).await?;
        stdin.flush().await?;
        Ok(())
    }
    async fn spawn_cli(
        &mut self,
        group: &dyn Containment,
        workdir: &PathBuf,
        model: &str,
        native_id: Option<String>,
    ) -> Result<(u32, u64, String), AdapterError> {
        let executable = match &self.executable {
            Some(path) => path.clone(),
            None => std::env::current_exe()?,
        };
        // Повтор при ETXTBSY: исполняемым может быть свежезаписанный двойник тестов.
        let mut child = crate::adapter::spawn_with_retry_io(|| {
            let mut command = tokio::process::Command::new(&executable);
            command
                .args([
                    "synthetic-cli",
                    "--profile",
                    &self.profile,
                    "--model",
                    model,
                ])
                .current_dir(workdir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            if let Some(id) = &native_id {
                command.arg("--native-conversation-id").arg(id);
            }
            if self.profile == "synthetic-codex" {
                command.args(["--enable", "default_mode_request_user_input"]);
            }
            group.prepare_command(&mut command);
            async move { command.spawn() }
        })
        .await?;
        let pid = child.id().ok_or(AdapterError::Protocol)?;
        let member = group
            .members()?
            .into_iter()
            .find(|m| m.pid == pid)
            .ok_or(AdapterError::Protocol)?;
        group.register_cli(&member);
        self.cli = Some(member.clone());
        self.stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or(AdapterError::Protocol)?;
        let (sender, receiver) = mpsc::channel(256);
        let (exited, exited_rx) = watch::channel(false);
        let generation = self.generation;
        self.frames = Some(receiver);
        self.exited = Some(exited_rx);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut blocks = HashMap::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let frame = serde_json::from_str(&line)
                    .ok()
                    .and_then(|value| decode(value, &mut blocks).ok())
                    .unwrap_or_else(|| {
                        Frame::Event(AdapterEvent::Diagnostic {
                            kind: "invalid_event",
                            event_type: "invalid_frame".into(),
                            required: true,
                            field_names: Vec::new(),
                        })
                    });
                if sender.send((generation, frame)).await.is_err() {
                    break;
                }
            }
            let event = match child.wait().await {
                Ok(status) => {
                    use std::os::unix::process::ExitStatusExt;
                    AdapterEvent::ProcessExited {
                        code: status.code(),
                        signal: status.signal(),
                    }
                }
                Err(_) => AdapterEvent::ProcessExited {
                    code: None,
                    signal: None,
                },
            };
            exited.send_replace(true);
            let _ = sender.send((generation, Frame::Event(event))).await;
        });
        if self.profile == "synthetic-codex" {
            self.send(&Request::Initialize {
                capabilities: super::synthetic_cli::InitializeCapabilities {
                    experimental_api: true,
                },
            })
            .await?;
        }
        let ready = tokio::time::timeout(self.transmit_deadline, self.frame()).await;
        match ready {
            Ok(Some(Frame::Ready(native_conversation_id))) => {
                Ok((pid, member.start_ticks, native_conversation_id))
            }
            _ => {
                self.close(CloseMode::Force, group).await?;
                Err(AdapterError::Protocol)
            }
        }
    }

    async fn restart_cli(
        &mut self,
        group: &dyn Containment,
        model: &str,
        instructions: &InstructionSpec,
    ) -> Result<(), AdapterError> {
        // Смена инструкций применяется перезапуском CLI между ходами с тем же нативным ID.
        // Поколение растёт до закрытия: кадры заменённого процесса отбрасываются,
        // иначе его выход завершил бы исполнение после перезапуска.
        self.generation += 1;
        let closed = self.close(CloseMode::Graceful, group).await;
        closed?;
        let workdir = self.workdir.clone();
        let spawned = self
            .spawn_cli(group, &workdir, model, self.native_id.clone())
            .await;
        let (pid, start_ticks, _) = spawned?;
        self.instructions(instructions.clone());
        self.pending.push_back(AdapterEvent::CliRestarted {
            cli_pid: pid,
            cli_start_ticks: start_ticks,
        });
        Ok(())
    }

    fn instructions(&mut self, instructions: InstructionSpec) {
        self.settings.instructions = Some(instructions);
    }
    async fn frame(&mut self) -> Option<Frame> {
        loop {
            let (generation, frame) = self.frames.as_mut()?.recv().await?;
            if generation != self.generation {
                continue;
            }
            return Some(frame);
        }
    }
    fn queue(&mut self, frame: Frame) {
        if let Frame::RestartDue(restart) = frame {
            self.deferred_restart = restart;
            self.pending.push_back(AdapterEvent::DeferredRestartDue);
            return;
        }
        if let Frame::Accepted(command_id) = frame
            && self.unconfirmed.remove(&command_id)
        {
            self.pending
                .push_back(AdapterEvent::InputAccepted { command_id });
            return;
        }
        if let Frame::Event(event) = frame {
            if let AdapterEvent::AgentCall { call_id, .. } = &event {
                self.pending_calls.insert(*call_id);
            }
            self.pending.push_back(event);
        }
    }
    async fn wait_exit(&mut self, grace: Duration) -> Result<(), AdapterError> {
        let Some(exited) = self.exited.as_mut() else {
            return Ok(());
        };
        tokio::time::timeout(grace, async {
            while !*exited.borrow_and_update() {
                let current = self.generation;
                tokio::select! {
                    result = exited.changed() => { result.map_err(|_| AdapterError::Protocol)?; }
                    frame = async { match self.frames.as_mut() { Some(frames) => frames.recv().await, None => std::future::pending().await } } => {
                        if let Some((generation, Frame::Event(event))) = frame
                            && generation == current
                        {
                            self.pending.push_back(event);
                        }
                    }
                }
            }
            Ok(())
        }).await.map_err(|_| AdapterError::Deadline)?
    }
    fn signal(
        &self,
        group: &dyn Containment,
        signal: rustix::process::Signal,
    ) -> Result<(), AdapterError> {
        if let Some(member) = &self.cli {
            group.signal_member(member, signal)?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Adapter for SyntheticAdapter {
    fn isolates_mcp(&self) -> bool {
        true
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: Support::Supported,
            resume: Support::Supported,
            interrupt: Support::Supported,
            approvals: Support::Supported,
            questions: Support::Supported,
            background_work: Support::Supported,
            interrupt_keeps_process: self.profile != "synthetic-exit-on-interrupt",
        }
    }

    async fn start(
        &mut self,
        spec: &StartSpec,
        group: &dyn Containment,
    ) -> Result<Started, AdapterError> {
        if self.started {
            return Err(AdapterError::AlreadyStarted);
        }
        if group.populated()? {
            return Err(AdapterError::AlreadyStarted);
        }
        self.settings = SettingsChange {
            model: Some(
                if spec.model.is_empty() {
                    "synthetic-large"
                } else {
                    &spec.model
                }
                .into(),
            ),
            effort: Some(
                if spec.effort.is_empty() {
                    "medium"
                } else {
                    &spec.effort
                }
                .into(),
            ),
            permission_mode: Some(
                if spec.permission_mode.is_empty() {
                    "owner"
                } else {
                    &spec.permission_mode
                }
                .into(),
            ),
            instructions: Some(spec.instructions.clone()),
        };
        self.workdir = spec.workdir.clone();
        self.started = true;
        let model = self.settings.model.clone().unwrap_or_default();
        let (pid, start_ticks, native_conversation_id) = self
            .spawn_cli(
                group,
                &spec.workdir,
                &model,
                spec.native_conversation_id.clone(),
            )
            .await?;
        self.native_id = Some(native_conversation_id.clone());
        Ok(Started {
            native_conversation_id,
            cli_pid: pid,
            cli_start_ticks: start_ticks,
        })
    }

    fn validate_transmission(&self, input: &Transmission) -> Result<(), &'static str> {
        let Some(change) = &input.settings else {
            return Ok(());
        };
        let model = change.model.as_ref().or(self.settings.model.as_ref());
        let effort = change.effort.as_ref().or(self.settings.effort.as_ref());
        let permission = change
            .permission_mode
            .as_ref()
            .or(self.settings.permission_mode.as_ref());
        let valid = catalog()
            .into_iter()
            .find(|c| c.harness == self.profile)
            .is_some_and(|c| {
                permission.is_some_and(|p| c.permission_modes.contains(&p.as_str()))
                    && c.models.iter().any(|m| {
                        Some(m.id.as_str()) == model.map(String::as_str)
                            && effort.is_some_and(|e| m.efforts.iter().any(|known| known == e))
                    })
            });
        if valid {
            Ok(())
        } else {
            Err("settings_unavailable")
        }
    }
    async fn transmit(&mut self, input: &Transmission, group: &dyn Containment) -> TransmitOutcome {
        if let Err(reason) = self.validate_transmission(input) {
            return TransmitOutcome::Rejected(reason.into());
        }

        if input.text.len() > canterlot_proto::limits::MAX_INPUT_TEXT_BYTES {
            return TransmitOutcome::Rejected("input size limit exceeded".into());
        }
        if self.stdin.is_none() {
            return TransmitOutcome::Rejected("adapter not running".into());
        }
        if let Some(change) = &input.settings {
            if change.model.is_some() {
                self.settings.model = change.model.clone();
            }
            if change.effort.is_some() {
                self.settings.effort = change.effort.clone();
            }
            if change.permission_mode.is_some() {
                self.settings.permission_mode = change.permission_mode.clone();
            }
            // Смена инструкций применяется перезапуском CLI до записи ввода.
            if let Some(instructions) = change.instructions.as_ref()
                && self
                    .settings
                    .instructions
                    .as_ref()
                    .is_none_or(|current| !instructions.same_effect(current))
            {
                let model = self.settings.model.clone().unwrap_or_default();
                if let Err(error) = self.restart_cli(group, &model, instructions).await {
                    let _ = error;
                    return TransmitOutcome::Rejected("instructions_change_failed".into());
                }
            }
        }
        let deadline = self.transmit_deadline;
        self.active = true;
        // Двойник работает с инструкциями запуска: ход без своих настроек
        // выполняется с действующими инструкциями CLI (как настоящий CLI
        // с флагами запуска), иначе echo не увидел бы пресет между сменами.
        let mut settings = input.settings.clone().unwrap_or_default();
        if settings.instructions.is_none() {
            settings.instructions = self.settings.instructions.clone();
        }
        let outcome = tokio::time::timeout(deadline, async {
            let sent = self
                .send(&Request::Input {
                    command_id: input.command_id,
                    attempt: input.attempt,
                    text: input.text.clone(),
                    files: input.files.clone(),
                    settings: Some(settings),
                })
                .await;
            if sent.is_err() {
                return TransmitOutcome::Undetermined;
            }
            while let Some(frame) = self.frame().await {
                match frame {
                    Frame::Accepted(command_id) if command_id == input.command_id => {
                        return TransmitOutcome::Accepted(Proof {
                            command_id,
                            kind: "cli_accepted",
                        });
                    }
                    Frame::Event(event @ AdapterEvent::ProcessExited { .. }) => {
                        self.pending.push_back(event);
                        return TransmitOutcome::Undetermined;
                    }
                    other => self.queue(other),
                }
            }
            TransmitOutcome::Undetermined
        })
        .await
        .unwrap_or(TransmitOutcome::Undetermined);
        if matches!(outcome, TransmitOutcome::Undetermined) {
            self.unconfirmed.insert(input.command_id);
        }
        outcome
    }

    async fn agent_call_result(&mut self, call_id: Uuid, result: AgentCallResult) {
        if !self.pending_calls.remove(&call_id) {
            return;
        }
        let text = match result {
            AgentCallResult::Ok(value) => serde_json::to_string(&value).unwrap_or_default(),
            AgentCallResult::Err { code, message } => format!("{code}: {message}"),
        };
        let block_id = Uuid::now_v7();
        let model = self.settings.model.clone().unwrap_or_default();
        self.pending.push_back(AdapterEvent::BlockStarted {
            block_id,
            kind: BlockKind::ToolResult,
            input_command_id: None,
            parent: None,
            agent: "main".into(),
            model,
            tool_name: None,
            summary: None,
            approved_by: None,
        });
        self.pending.push_back(AdapterEvent::BlockFinal {
            block_id,
            text,
            summary: None,
        });
    }

    async fn deliver_decision(
        &mut self,
        request_id: &str,
        decision: &DecisionPayload,
    ) -> DecisionOutcome {
        if !self.active || self.stdin.is_none() {
            return DecisionOutcome::NotApplied("request_closed".into());
        }
        tokio::time::timeout(self.transmit_deadline, async {
            if self
                .send(&Request::Decision {
                    request_id: request_id.into(),
                    decision: decision.clone(),
                })
                .await
                .is_err()
            {
                return DecisionOutcome::Undetermined;
            }
            while let Some(frame) = self.frame().await {
                match frame {
                    Frame::Decision(id, outcome) if id == request_id => return outcome,
                    Frame::Event(event @ AdapterEvent::ProcessExited { .. }) => {
                        self.pending.push_back(event);
                        return DecisionOutcome::Undetermined;
                    }
                    other => self.queue(other),
                }
            }
            DecisionOutcome::Undetermined
        })
        .await
        .unwrap_or(DecisionOutcome::Undetermined)
    }

    async fn refuse_request(&mut self, request_id: &str, message: &str) {
        let decision = DecisionPayload::Permission {
            behavior: canterlot_proto::requests::PermissionBehavior::Deny,
            message: Some(message.to_owned()),
        };
        if self.active {
            let _ = self.deliver_decision(request_id, &decision).await;
        } else {
            let request_id = request_id.to_owned();
            let _ = self
                .send(&Request::Decision {
                    request_id,
                    decision,
                })
                .await;
        }
    }

    async fn apply_deferred(&mut self, group: &dyn Containment) {
        if std::mem::take(&mut self.deferred_restart) {
            let model = self.settings.model.clone().unwrap_or_default();
            let Some(instructions) = self.settings.instructions.clone() else {
                return;
            };
            if self
                .restart_cli(group, &model, &instructions)
                .await
                .is_err()
            {
                return;
            }
        }
        self.pending.push_back(AdapterEvent::Diagnostic {
            kind: "deferred_restart_applied",
            event_type: "deferred_restart_applied".into(),
            required: false,
            field_names: Vec::new(),
        });
    }

    async fn interrupt(&mut self) -> InterruptOutcome {
        if !self.active || self.stdin.is_none() {
            return InterruptOutcome::NotApplicable;
        }
        let id = Uuid::now_v7();
        tokio::time::timeout(self.transmit_deadline, async {
            if self.send(&Request::Interrupt { id }).await.is_err() {
                return InterruptOutcome::Undetermined;
            }
            while let Some(frame) = self.frame().await {
                match frame {
                    Frame::Interrupted(reply) if reply == id => {
                        return InterruptOutcome::Acknowledged;
                    }
                    Frame::Event(event @ AdapterEvent::ProcessExited { .. }) => {
                        self.pending.push_back(event);
                        return InterruptOutcome::Undetermined;
                    }
                    other => self.queue(other),
                }
            }
            InterruptOutcome::Undetermined
        })
        .await
        .unwrap_or(InterruptOutcome::Undetermined)
    }

    async fn next_event(&mut self) -> AdapterEvent {
        loop {
            if self.pending.is_empty() {
                match self.frame().await {
                    Some(frame) => self.queue(frame),
                    None => std::future::pending().await,
                }
            }
            if let Some(event) = self.pending.pop_front() {
                if matches!(
                    event,
                    AdapterEvent::RunFinished { .. } | AdapterEvent::ProcessExited { .. }
                ) {
                    self.active = false;
                }
                if matches!(event, AdapterEvent::RunAdopted { .. }) {
                    self.active = true;
                }
                return event;
            }
        }
    }

    async fn close(
        &mut self,
        mode: CloseMode,
        group: &dyn Containment,
    ) -> Result<(), AdapterError> {
        self.stdin.take();
        if self.cli.is_none() {
            return Ok(());
        }
        if matches!(mode, CloseMode::Graceful) && self.wait_exit(self.close_grace).await.is_ok() {
            return Ok(());
        }
        self.signal(
            group,
            if matches!(mode, CloseMode::Force) {
                rustix::process::Signal::KILL
            } else {
                rustix::process::Signal::TERM
            },
        )?;
        if self.wait_exit(self.close_grace).await.is_err() {
            self.signal(group, rustix::process::Signal::KILL)?;
            self.wait_exit(self.close_grace).await?;
        }
        self.active = false;
        Ok(())
    }
}
