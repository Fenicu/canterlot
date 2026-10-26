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
use crate::containment::{Member, TreeContainment};

enum Frame {
    Ready(String),
    Accepted(Uuid),
    Interrupted(Uuid),
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
        "phase" => AdapterEvent::Phase(
            serde_json::from_value(value["phase"].clone()).map_err(|_| AdapterError::Protocol)?,
        ),
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
        },
        "completed" => {
            let input_command_id = uuid("input_command_id")?;
            blocks.clear();
            AdapterEvent::RunFinished { input_command_id }
        }
        _ => AdapterEvent::Diagnostic {
            kind: "unknown_event",
            event_type: string("type")?,
            required: value
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false),
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
    frames: Option<mpsc::Receiver<Frame>>,
    pending: VecDeque<AdapterEvent>,
    exited: Option<watch::Receiver<bool>>,
    cli: Option<Member>,
    tree: Option<TreeContainment>,
    started: bool,
    active: bool,
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
            tree: None,
            started: false,
            active: false,
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
    async fn frame(&mut self) -> Option<Frame> {
        self.frames.as_mut()?.recv().await
    }
    fn queue(&mut self, frame: Frame) {
        if let Frame::Event(event) = frame {
            self.pending.push_back(event);
        }
    }
    async fn wait_exit(&mut self, grace: Duration) -> Result<(), AdapterError> {
        let Some(exited) = self.exited.as_mut() else {
            return Ok(());
        };
        tokio::time::timeout(grace, async {
            while !*exited.borrow_and_update() {
                tokio::select! {
                    result = exited.changed() => { result.map_err(|_| AdapterError::Protocol)?; }
                    frame = async { match self.frames.as_mut() { Some(frames) => frames.recv().await, None => std::future::pending().await } } => {
                        if let Some(Frame::Event(event)) = frame { self.pending.push_back(event); }
                    }
                }
            }
            Ok(())
        }).await.map_err(|_| AdapterError::Deadline)?
    }
    fn signal(&self, signal: rustix::process::Signal) -> Result<(), AdapterError> {
        if let (Some(tree), Some(member)) = (&self.tree, &self.cli) {
            tree.signal_member(member, signal)?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Adapter for SyntheticAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: Support::Supported,
            resume: Support::Supported,
            interrupt: Support::Supported,
            approvals: Support::Unsupported,
            questions: Support::Unsupported,
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
        self.started = true;
        self.tree = Some(TreeContainment::new()?);
        let executable = match &self.executable {
            Some(path) => path.clone(),
            None => std::env::current_exe()?,
        };
        let mut command = tokio::process::Command::new(executable);
        command
            .args(["synthetic-cli", "--profile", &self.profile])
            .current_dir(&spec.workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(id) = &spec.native_conversation_id {
            command.arg("--native-conversation-id").arg(id);
        }
        group.prepare_command(&mut command);
        let mut child = command.spawn()?;
        let pid = child.id().ok_or(AdapterError::Protocol)?;
        let member = group
            .members()?
            .into_iter()
            .find(|m| m.pid == pid)
            .ok_or(AdapterError::Protocol)?;
        self.cli = Some(member.clone());
        self.stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or(AdapterError::Protocol)?;
        let (sender, receiver) = mpsc::channel(256);
        let (exited, exited_rx) = watch::channel(false);
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
                        })
                    });
                if sender.send(frame).await.is_err() {
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
            let _ = sender.send(Frame::Event(event)).await;
        });
        let ready = tokio::time::timeout(self.transmit_deadline, self.frame()).await;
        match ready {
            Ok(Some(Frame::Ready(native_conversation_id))) => Ok(Started {
                native_conversation_id,
                cli_pid: pid,
                cli_start_ticks: member.start_ticks,
            }),
            _ => {
                self.close(CloseMode::Force).await?;
                Err(AdapterError::Protocol)
            }
        }
    }

    async fn transmit(&mut self, input: &Transmission) -> TransmitOutcome {
        if input.text.len() > canterlot_proto::limits::MAX_INPUT_TEXT_BYTES {
            return TransmitOutcome::Rejected("input size limit exceeded".into());
        }
        if self.stdin.is_none() {
            return TransmitOutcome::Rejected("adapter not running".into());
        }
        let deadline = self.transmit_deadline;
        self.active = true;
        tokio::time::timeout(deadline, async {
            if self
                .send(&Request::Input {
                    command_id: input.command_id,
                    attempt: input.attempt,
                    text: input.text.clone(),
                })
                .await
                .is_err()
            {
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
        .unwrap_or(TransmitOutcome::Undetermined)
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
            let event = if let Some(event) = self.pending.pop_front() {
                Some(event)
            } else {
                match self.frame().await {
                    Some(Frame::Event(event)) => Some(event),
                    Some(_) => None,
                    None => std::future::pending().await,
                }
            };
            if let Some(event) = event {
                if matches!(
                    event,
                    AdapterEvent::RunFinished { .. } | AdapterEvent::ProcessExited { .. }
                ) {
                    self.active = false;
                }
                return event;
            }
        }
    }

    async fn close(&mut self, mode: CloseMode) -> Result<(), AdapterError> {
        self.stdin.take();
        if self.cli.is_none() {
            return Ok(());
        }
        if matches!(mode, CloseMode::Graceful) && self.wait_exit(self.close_grace).await.is_ok() {
            return Ok(());
        }
        self.signal(if matches!(mode, CloseMode::Force) {
            rustix::process::Signal::KILL
        } else {
            rustix::process::Signal::TERM
        })?;
        if self.wait_exit(self.close_grace).await.is_err() {
            self.signal(rustix::process::Signal::KILL)?;
            self.wait_exit(self.close_grace).await?;
        }
        self.active = false;
        Ok(())
    }
}
