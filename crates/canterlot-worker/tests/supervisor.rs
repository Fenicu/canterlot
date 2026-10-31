#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

use std::{
    collections::BTreeMap,
    fs,
    future::{Future, poll_fn},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    task::Poll,
    time::Duration,
};

use canterlot_journal::{Journal, JournalOptions};
use canterlot_proto::worker::{
    ipc::{
        Attach, Attached, ControlToSupervisor, DeliverAck, ShipFrom, SjAck, StartCli, StopRequest,
        SupervisorToControl, control_to_supervisor as down, supervisor_to_control as up,
    },
    v1::{
        Command, ContentBlock, DeliverInput, Instructions, InterruptState, RecordBody, TextBlock,
        TodoStatus, command::Kind, content_block, record_body::Body,
    },
};
use canterlot_worker::supervisor::ipc::{read_message, write_message};
use prost::Message;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixStream, unix::OwnedWriteHalf},
    process::Child,
    sync::mpsc,
    time::timeout,
};
use uuid::Uuid;

#[path = "support/e2e_gate.rs"]
mod e2e_gate;
#[path = "support/fake_exec.rs"]
mod fake_exec;
#[path = "support/process_guard.rs"]
mod process_guard;
use process_guard::ProcessGuard;

const WAIT: Duration = Duration::from_secs(15);

fn sha(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn ticks(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat[stat.rfind(')')? + 2..]
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

fn processes() -> Vec<(u32, u32, String, String)> {
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(end) = stat.rfind(')') else { continue };
        let fields: Vec<_> = stat[end + 2..].split_whitespace().collect();
        let comm = stat[stat.find('(').unwrap() + 1..end].to_owned();
        let cmdline = fs::read(format!("/proc/{pid}/cmdline"))
            .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "))
            .unwrap_or_default();
        found.push((pid, fields[1].parse().unwrap_or(0), comm, cmdline));
    }
    found
}

fn children_of(parent: u32, needle: &str) -> Vec<u32> {
    processes()
        .into_iter()
        .filter(|(_, ppid, comm, cmdline)| {
            *ppid == parent && (comm.contains(needle) || cmdline.contains(needle))
        })
        .map(|(pid, ..)| pid)
        .collect()
}

fn alive(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat[stat.rfind(')').unwrap() + 2..].starts_with(|c| c != 'Z' && c != 'X')
    })
}

fn read_sj(exec_dir: &Path) -> Vec<(u64, Body)> {
    let (journal, _) = Journal::open(&exec_dir.join("sj"), JournalOptions::default()).unwrap();
    journal
        .read_from(journal.first_seq(), usize::MAX)
        .unwrap()
        .into_iter()
        .map(|record| {
            (
                record.seq,
                RecordBody::decode(record.payload.as_slice())
                    .unwrap()
                    .body
                    .unwrap(),
            )
        })
        .collect()
}

fn script(steps: &str) -> String {
    format!(r#"{{"synthetic":[{steps}]}}"#)
}

struct Harness {
    _guard: ProcessGuard,
    exec_dir: PathBuf,
    _root: TempDir,
    secret: Vec<u8>,
    execution_id: Uuid,
    conversation_id: Uuid,
    child: Child,
    conn: Option<Conn>,
    pid: u32,
    records: BTreeMap<u64, Body>,
    others: Vec<SupervisorToControl>,
    auto_ack: bool,
    stdin_log: PathBuf,
}

struct Conn {
    write: OwnedWriteHalf,
    rx: mpsc::UnboundedReceiver<SupervisorToControl>,
}

impl Conn {
    async fn detach(mut self) {
        self.write.shutdown().await.unwrap();
        // The supervisor queues Closed after the frames and closes its writer on Closed.
        timeout(WAIT, async { while self.rx.recv().await.is_some() {} })
            .await
            .expect("supervisor did not drain the control connection");
    }
}

async fn connect(exec_dir: &Path) -> Conn {
    let socket = exec_dir.join("ipc.sock");
    timeout(WAIT, async {
        while !socket.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("ipc socket never appeared");
    let stream = UnixStream::connect(&socket).await.unwrap();
    let (mut read, write) = stream.into_split();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok(message) = read_message::<SupervisorToControl, _>(&mut read).await {
            if tx.send(message).is_err() {
                break;
            }
        }
    });
    Conn { write, rx }
}

impl Harness {
    async fn start(extra_env: &[(&str, &str)]) -> Harness {
        Self::start_with(extra_env, "1s", "").await
    }

    async fn start_with(extra_env: &[(&str, &str)], stop_grace: &str, extra_spec: &str) -> Harness {
        let root = TempDir::new().unwrap();
        let exec_dir = root.path().join("exec");
        fs::create_dir(&exec_dir).unwrap();
        fs::set_permissions(&exec_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let secret: Vec<u8> = (0..32).map(|_| rand_byte()).collect();
        fs::write(exec_dir.join("secret"), &secret).unwrap();
        fs::set_permissions(exec_dir.join("secret"), fs::Permissions::from_mode(0o600)).unwrap();
        let execution_id = Uuid::now_v7();
        let conversation_id = Uuid::now_v7();
        fs::write(
            exec_dir.join("spec.toml"),
            format!(
                "execution_id = \"{execution_id}\"\ngeneration = 1\nconversation_id = \"{conversation_id}\"\nprotocol_version = 1\n[timers]\nattach_wait = \"30s\"\nready_deadline = \"10s\"\ntransmit_deadline = \"2s\"\nstop_grace = \"{stop_grace}\"\nstop_retry = \"200ms\"\n{extra_spec}"
            ),
        )
        .unwrap();
        let stdin_log = root.path().join("cli-stdin.log");
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"));
        command
            .args(["supervise", "--exec-dir"])
            .arg(&exec_dir)
            .env("CANTERLOT_SYNTHETIC_STDIN_LOG", &stdin_log)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let child = command.spawn().unwrap();
        Harness {
            _guard: ProcessGuard::new(root.path()),
            exec_dir,
            _root: root,
            secret,
            execution_id,
            conversation_id,
            child,
            conn: None,
            pid: 0,
            records: BTreeMap::new(),
            others: Vec::new(),
            auto_ack: true,
            stdin_log,
        }
    }

    fn attach_message(
        &self,
        secret: &[u8],
        version: u32,
        generation: u64,
        execution: Uuid,
    ) -> ControlToSupervisor {
        ControlToSupervisor {
            msg: Some(down::Msg::Attach(Attach {
                protocol_version: version,
                secret: secret.to_vec(),
                execution_id: execution.to_string(),
                generation,
            })),
        }
    }

    async fn attach(&mut self) -> Attached {
        let mut conn = connect(&self.exec_dir).await;
        let message = self.attach_message(&self.secret.clone(), 1, 1, self.execution_id);
        write_message(&mut conn.write, &message).await.unwrap();
        let reply = timeout(WAIT, conn.rx.recv())
            .await
            .unwrap()
            .expect("no Attached");
        let Some(up::Msg::Attached(attached)) = reply.msg else {
            panic!("expected Attached");
        };
        self.pid = attached.pid;
        self.conn = Some(conn);
        attached
    }

    async fn attach_and_ship(&mut self, committed: u64) -> Attached {
        let attached = self.attach().await;
        self.send(down::Msg::ShipFrom(ShipFrom {
            committed_seq: committed,
        }))
        .await;
        attached
    }

    async fn send(&mut self, message: down::Msg) {
        let conn = self.conn.as_mut().expect("not attached");
        write_message(&mut conn.write, &ControlToSupervisor { msg: Some(message) })
            .await
            .unwrap();
    }

    async fn start_cli(&mut self, profile: &str) -> up::Msg {
        let workdir = self._root.path().to_string_lossy().into_owned();
        self.send(down::Msg::StartCli(StartCli {
            profile: profile.into(),
            workdir,
            native_conversation_id: String::new(),
            instructions: Some(Instructions {
                mode: "clean".into(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await;
        self.next_message(|message| matches!(message, up::Msg::CliStarted(_)))
            .await
    }

    fn command(&self, id: Uuid, attempt: u32, text: &str, generation: u64) -> Command {
        Command {
            command_id: id.to_string(),
            dispatch_attempt: attempt,
            execution_id: self.execution_id.to_string(),
            execution_generation: generation,
            kind: Some(Kind::DeliverInput(DeliverInput {
                input_id: Uuid::now_v7().to_string(),
                conversation_id: self.conversation_id.to_string(),
                content: vec![ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock { text: text.into() })),
                }],
                ..Default::default()
            })),
        }
    }

    async fn deliver(&mut self, id: Uuid, attempt: u32, text: &str) -> DeliverAck {
        let command = self.command(id, attempt, text, 1);
        self.deliver_command(command).await
    }

    async fn deliver_command(&mut self, command: Command) -> DeliverAck {
        self.send(down::Msg::Deliver(canterlot_proto::worker::ipc::Deliver {
            command: Some(command),
            ..Default::default()
        }))
        .await;
        match self
            .next_message(|message| matches!(message, up::Msg::DeliverAck(_)))
            .await
        {
            up::Msg::DeliverAck(ack) => ack,
            _ => unreachable!(),
        }
    }

    async fn absorb(&mut self, message: SupervisorToControl) {
        match message.msg {
            Some(up::Msg::SjBatch(batch)) => {
                let last = batch.records.last().map(|record| record.seq);
                for record in batch.records {
                    self.records
                        .insert(record.seq, record.body.unwrap().body.unwrap());
                }
                if let (true, Some(last)) = (self.auto_ack, last) {
                    self.send(down::Msg::SjAck(SjAck { up_to_seq: last })).await;
                }
            }
            other => self.others.push(SupervisorToControl { msg: other }),
        }
    }

    async fn pump(&mut self, within: Duration) -> bool {
        let received = {
            let conn = self.conn.as_mut().expect("not attached");
            timeout(within, conn.rx.recv()).await
        };
        match received {
            Ok(Some(message)) => {
                self.absorb(message).await;
                true
            }
            _ => false,
        }
    }

    async fn next_message(&mut self, wanted: impl Fn(&up::Msg) -> bool) -> up::Msg {
        timeout(WAIT, async {
            loop {
                if let Some(index) = self
                    .others
                    .iter()
                    .position(|m| m.msg.as_ref().is_some_and(&wanted))
                {
                    return self.others.remove(index).msg.unwrap();
                }
                assert!(self.pump(WAIT).await, "connection closed while waiting");
            }
        })
        .await
        .expect("timed out waiting for a supervisor message")
    }

    async fn wait_records(&mut self, what: &str, wanted: impl Fn(&[Body]) -> bool) -> Vec<Body> {
        let outcome = timeout(WAIT, async {
            loop {
                let bodies: Vec<_> = self.records.values().cloned().collect();
                if wanted(&bodies) {
                    return bodies;
                }
                if !self.pump(WAIT).await {
                    return Vec::new();
                }
            }
        })
        .await;
        match outcome {
            Ok(bodies) if !bodies.is_empty() => bodies,
            _ => panic!(
                "timed out or connection closed waiting for: {what}; records so far: {:?}",
                self.records.values().map(kind).collect::<Vec<_>>()
            ),
        }
    }

    async fn wait_exit(&mut self) -> std::process::ExitStatus {
        loop {
            if self.conn.is_some() {
                self.pump(Duration::from_millis(20)).await;
            } else {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
        }
    }

    async fn shutdown(mut self) {
        if self.conn.is_some() {
            self.send(down::Msg::Stop(StopRequest {
                command_id: Uuid::now_v7().to_string(),
                dispatch_attempt: 1,
                force: true,
            }))
            .await;
            let _ = timeout(WAIT, self.wait_exit()).await;
        }
    }
}

fn rand_byte() -> u8 {
    rand::random()
}

fn kind(body: &Body) -> &'static str {
    match body {
        Body::JournalOpened(_) => "journal_opened",
        Body::ExecutionState(_) => "execution_state",
        Body::PhaseChanged(_) => "phase_changed",
        Body::TransmissionStarted(_) => "transmission_started",
        Body::InputAccepted(_) => "input_accepted",
        Body::AcceptanceUnconfirmed(_) => "acceptance_unconfirmed",
        Body::InputRejected(_) => "input_rejected",
        Body::CancelWon(_) => "cancel_won",
        Body::CancelLost(_) => "cancel_lost",
        Body::InputWithdrawn(_) => "input_withdrawn",
        Body::WithdrawRefused(_) => "withdraw_refused",
        Body::InterruptStatus(_) => "interrupt_status",
        Body::BlockStarted(_) => "block_started",
        Body::BlockDelta(_) => "block_delta",
        Body::BlockFinal(_) => "block_final",
        Body::BlockIncomplete(_) => "block_incomplete",
        Body::RunFinished(_) => "run_finished",
        Body::AdapterDiagnostic(_) => "adapter_diagnostic",
        Body::DispatchDeferred(_) => "dispatch_deferred",
        Body::JournalClosed(_) => "journal_closed",
        _ => "other",
    }
}

fn state(body: &Body, wanted: &str) -> bool {
    matches!(body, Body::ExecutionState(fact) if fact.state == wanted)
}

fn index(bodies: &[Body], wanted: impl Fn(&Body) -> bool) -> Option<usize> {
    bodies.iter().position(wanted)
}

fn run_finished(bodies: &[Body], id: Uuid) -> bool {
    bodies
        .iter()
        .any(|b| matches!(b, Body::RunFinished(f) if f.input_command_id == id.to_string()))
}

fn transmissions(bodies: &[Body], id: Uuid) -> usize {
    bodies
        .iter()
        .filter(|b| matches!(b, Body::TransmissionStarted(f) if f.command_id == id.to_string()))
        .count()
}

#[tokio::test]
async fn attach_requires_the_secret_and_same_uid() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    let bad: Vec<ControlToSupervisor> = vec![
        harness.attach_message(&[7; 32], 1, 1, harness.execution_id),
        harness.attach_message(&harness.secret.clone(), 2, 1, harness.execution_id),
        harness.attach_message(&harness.secret.clone(), 1, 2, harness.execution_id),
        harness.attach_message(&harness.secret.clone(), 1, 1, Uuid::now_v7()),
    ];
    for message in bad {
        let mut conn = connect(&harness.exec_dir).await;
        write_message(&mut conn.write, &message).await.unwrap();
        let reply = timeout(WAIT, conn.rx.recv()).await.unwrap();
        assert!(
            reply.is_none(),
            "a rejected attach must only close the connection"
        );
    }
    let attached = harness.attach().await;
    assert_eq!(attached.pid, harness.child.id().unwrap());
    assert_eq!(attached.protocol_version, 1);
    assert!(attached.start_ticks > 0 && Some(attached.start_ticks) == ticks(attached.pid));
    assert!(Uuid::parse_str(&attached.sj_journal_id).is_ok());
    assert_eq!(
        attached.last_sj_seq, 2,
        "rejected attaches must not touch the SJ"
    );
    assert_eq!(attached.acked_sj_seq, 0);
    assert!(attached.received.is_empty());
    let mode = fs::metadata(harness.exec_dir.join("ipc.sock"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "socket must be private");
    harness.shutdown().await;
}

#[tokio::test]
async fn ipc_server_rejects_other_uid_and_open_directories() {
    use canterlot_worker::supervisor::ipc::IpcServer;
    let _gate = e2e_gate::acquire().await;
    let root = TempDir::new().unwrap();
    let open = root.path().join("open");
    fs::create_dir(&open).unwrap();
    fs::set_permissions(&open, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(IpcServer::bind(&open, vec![1; 32]).await.is_err());
    let private = root.path().join("private");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    let own = rustix::process::getuid().as_raw();
    let server = IpcServer::bind_for_uid(&private, vec![1; 32], own.wrapping_add(1))
        .await
        .unwrap();
    let socket = private.join("ipc.sock");
    let accepting = tokio::spawn(async move { server.accept().await.map(|_| ()) });
    let mut stranger = UnixStream::connect(&socket).await.unwrap();
    let mut byte = [0u8; 1];
    let closed = timeout(
        Duration::from_secs(2),
        tokio::io::AsyncReadExt::read(&mut stranger, &mut byte),
    )
    .await
    .unwrap();
    assert!(
        matches!(closed, Ok(0) | Err(_)),
        "a foreign uid must be disconnected"
    );
    assert!(
        timeout(Duration::from_millis(300), accepting)
            .await
            .is_err(),
        "accept must keep waiting"
    );
}

#[tokio::test]
async fn start_sequence_is_journal_opened_starting_running_ready() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    let up::Msg::CliStarted(started) = harness.start_cli("synthetic").await else {
        unreachable!()
    };
    assert!(started.pid > 0 && Some(started.start_ticks) == ticks(started.pid));
    assert!(started.native_conversation_id.starts_with("synthetic-"));
    let bodies = harness
        .wait_records("four lifecycle records", |b| b.len() >= 4)
        .await;
    let Body::JournalOpened(opened) = &bodies[0] else {
        panic!("seq 1 must be journal_opened")
    };
    assert_eq!(opened.execution_id, harness.execution_id.to_string());
    assert_eq!(
        (opened.generation, opened.supervisor_protocol_version),
        (1, 1)
    );
    assert!(
        matches!(&bodies[1], Body::ExecutionState(f) if f.state == "starting" && f.generation == 1 && f.execution_id == harness.execution_id.to_string())
    );
    assert!(
        matches!(&bodies[2], Body::ExecutionState(f) if f.state == "running" && f.native_conversation_id == started.native_conversation_id)
    );
    assert!(
        matches!(&bodies[3], Body::PhaseChanged(f) if f.phase == "awaiting_input" && f.execution_id == harness.execution_id.to_string())
    );
    harness.shutdown().await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn intent_is_durable_before_first_byte_and_never_repeated() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    let id = Uuid::now_v7();
    assert_eq!(
        harness.deliver(id, 1, "once").await.result,
        "accepted_for_gate"
    );
    assert_eq!(harness.deliver(id, 1, "once").await.result, "duplicate");
    harness
        .wait_records("run finished", |b| run_finished(b, id))
        .await;
    assert_eq!(harness.deliver(id, 2, "once").await.result, "duplicate");
    let bodies = harness
        .wait_records("settled", |b| run_finished(b, id))
        .await;
    assert_eq!(transmissions(&bodies, id), 1);
    harness.shutdown().await;

    let mut harness = Harness::start(&[("CANTERLOT_FAULT_ABORT_AT", "before_first_byte")]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    let id = Uuid::now_v7();
    let command = harness.command(id, 1, "never delivered", 1);
    harness
        .send(down::Msg::Deliver(canterlot_proto::worker::ipc::Deliver {
            command: Some(command),
            ..Default::default()
        }))
        .await;
    let status = harness.wait_exit().await;
    assert!(!status.success(), "the supervisor must have aborted");
    let bodies: Vec<_> = read_sj(&harness.exec_dir)
        .into_iter()
        .map(|(_, b)| b)
        .collect();
    assert_eq!(
        transmissions(&bodies, id),
        1,
        "the intent record must be durable"
    );
    assert!(!bodies.iter().any(|b| matches!(b, Body::InputAccepted(_))));
    assert_eq!(
        bodies
            .iter()
            .filter(|b| matches!(b, Body::TransmissionStarted(_)))
            .count(),
        1
    );
    let log = fs::read(&harness.stdin_log).unwrap_or_default();
    assert!(
        !log.windows(b"never delivered".len())
            .any(|w| w == b"never delivered")
            && log.is_empty(),
        "the CLI must not have received a single input byte"
    );
}

#[tokio::test]
async fn second_input_waits_for_awaiting_input_after_first_turn() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    let (first, second) = (Uuid::now_v7(), Uuid::now_v7());
    harness
        .deliver(first, 1, &script(r#"{"delay_ms":300},{"delta":"a"}"#))
        .await;
    harness.deliver(second, 1, "second").await;
    let bodies = harness
        .wait_records("second accepted", |b| {
            b.iter()
                .any(|x| matches!(x, Body::InputAccepted(f) if f.command_id == second.to_string()))
        })
        .await;
    let first_done = index(
        &bodies,
        |b| matches!(b, Body::RunFinished(f) if f.input_command_id == first.to_string()),
    )
    .unwrap();
    let ready = bodies
        .iter()
        .enumerate()
        .skip(first_done)
        .find(|(_, b)| matches!(b, Body::PhaseChanged(f) if f.phase == "awaiting_input"))
        .map(|(i, _)| i)
        .unwrap();
    let second_started = index(
        &bodies,
        |b| matches!(b, Body::TransmissionStarted(f) if f.command_id == second.to_string()),
    )
    .unwrap();
    let first_started = index(
        &bodies,
        |b| matches!(b, Body::TransmissionStarted(f) if f.command_id == first.to_string()),
    )
    .unwrap();
    assert!(
        first_started < first_done && first_done < ready && ready < second_started,
        "Q-2: second transmission only after readiness following the first turn"
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn cancel_before_transmission_wins_and_after_loses() {
    use canterlot_proto::worker::v1::CancelInput;
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    let (first, second) = (Uuid::now_v7(), Uuid::now_v7());
    harness
        .deliver(first, 1, &script(r#"{"delta":"a"},{"delay_ms":800}"#))
        .await;
    harness.deliver(second, 1, "queued").await;
    harness
        .send(down::Msg::Cancel(CancelInput {
            target_command_id: second.to_string(),
            target_dispatch_attempt: 1,
        }))
        .await;
    harness.wait_records("cancel_won", |b| b.iter().any(|x| matches!(x, Body::CancelWon(f) if f.command_id == second.to_string() && f.dispatch_attempt == 1))).await;
    harness
        .send(down::Msg::Cancel(CancelInput {
            target_command_id: first.to_string(),
            target_dispatch_attempt: 1,
        }))
        .await;
    harness
        .wait_records("cancel_lost", |b| {
            b.iter()
                .any(|x| matches!(x, Body::CancelLost(f) if f.command_id == first.to_string()))
        })
        .await;
    harness.deliver(second, 2, "later attempt").await;
    let bodies = harness
        .wait_records("first finished", |b| run_finished(b, first))
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    harness.pump(Duration::from_millis(200)).await;
    let bodies2: Vec<_> = harness.records.values().cloned().collect();
    assert!(bodies2.len() >= bodies.len());
    assert_eq!(
        transmissions(&bodies2, second),
        0,
        "a cancelled input is never transmitted, later attempts included"
    );
    assert!(
        !bodies2
            .iter()
            .any(|b| matches!(b, Body::CancelWon(f) if f.command_id == first.to_string()))
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn withdraw_after_transmission_is_refused_too_late() {
    use canterlot_proto::worker::v1::WithdrawInput;
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    let (first, second) = (Uuid::now_v7(), Uuid::now_v7());
    harness
        .deliver(first, 1, &script(r#"{"delta":"a"},{"delay_ms":800}"#))
        .await;
    harness.deliver(second, 1, "queued").await;
    harness
        .send(down::Msg::Withdraw(WithdrawInput {
            target_command_id: first.to_string(),
            target_dispatch_attempt: 1,
        }))
        .await;
    harness.wait_records("too_late", |b| b.iter().any(|x| matches!(x, Body::WithdrawRefused(f) if f.command_id == first.to_string() && f.reason == "too_late"))).await;
    harness
        .send(down::Msg::Withdraw(WithdrawInput {
            target_command_id: second.to_string(),
            target_dispatch_attempt: 1,
        }))
        .await;
    harness.wait_records("withdrawn", |b| b.iter().any(|x| matches!(x, Body::InputWithdrawn(f) if f.command_id == second.to_string() && f.dispatch_attempt == 1))).await;
    let bodies = harness
        .wait_records("first finished", |b| run_finished(b, first))
        .await;
    assert_eq!(transmissions(&bodies, second), 0);
    assert!(
        !bodies
            .iter()
            .any(|b| matches!(b, Body::InputWithdrawn(f) if f.command_id == first.to_string()))
    );
    harness.shutdown().await;
}

fn interrupt_states(bodies: &[Body], id: Uuid) -> Vec<i32> {
    bodies
        .iter()
        .filter_map(|b| match b {
            Body::InterruptStatus(f) if f.command_id == id.to_string() => Some(f.state),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn interrupt_not_applicable_without_a_turn() {
    use canterlot_proto::worker::ipc::InterruptRequest;
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    let id = Uuid::now_v7();
    for _ in 0..2 {
        harness
            .send(down::Msg::Interrupt(InterruptRequest {
                command_id: id.to_string(),
                dispatch_attempt: 1,
            }))
            .await;
    }
    harness
        .wait_records("not_applicable", |b| !interrupt_states(b, id).is_empty())
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    harness.pump(Duration::from_millis(100)).await;
    let bodies: Vec<_> = harness.records.values().cloned().collect();
    assert_eq!(
        interrupt_states(&bodies, id),
        vec![InterruptState::NotApplicable as i32],
        "idempotent and never sent to the agent"
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn interrupt_keeps_or_exits_process_by_profile() {
    use canterlot_proto::worker::ipc::InterruptRequest;
    let _gate = e2e_gate::acquire().await;
    let long = script(r#"{"delta":"partial"},{"delay_ms":60000}"#);
    let mut keeps = Harness::start(&[]).await;
    keeps.attach_and_ship(0).await;
    keeps.start_cli("synthetic").await;
    let (first, interrupt, next) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    keeps.deliver(first, 1, &long).await;
    keeps
        .wait_records("block started", |b| {
            b.iter().any(|x| matches!(x, Body::BlockDelta(_)))
        })
        .await;
    keeps
        .send(down::Msg::Interrupt(InterruptRequest {
            command_id: interrupt.to_string(),
            dispatch_attempt: 1,
        }))
        .await;
    let bodies = keeps
        .wait_records("interrupt completed", |b| {
            interrupt_states(b, interrupt).contains(&(InterruptState::Completed as i32))
        })
        .await;
    assert_eq!(
        interrupt_states(&bodies, interrupt),
        vec![
            InterruptState::SentToAgent as i32,
            InterruptState::Acknowledged as i32,
            InterruptState::Completed as i32
        ]
    );
    let finished = index(&bodies, |b| matches!(b, Body::RunFinished(_))).unwrap();
    let incomplete = index(&bodies, |b| matches!(b, Body::BlockIncomplete(_)))
        .expect("the interrupted block must be closed incomplete");
    assert!(incomplete < finished);
    assert!(
        !bodies.iter().any(|b| state(b, "stopping")),
        "the process profile keeps the execution running"
    );
    keeps.deliver(next, 1, "after interrupt").await;
    keeps
        .wait_records("next turn", |b| run_finished(b, next))
        .await;
    keeps.shutdown().await;

    let mut exits = Harness::start(&[]).await;
    exits.attach_and_ship(0).await;
    exits.start_cli("synthetic-exit-on-interrupt").await;
    let (first, interrupt) = (Uuid::now_v7(), Uuid::now_v7());
    exits.deliver(first, 1, &long).await;
    exits
        .wait_records("delta", |b| {
            b.iter().any(|x| matches!(x, Body::BlockDelta(_)))
        })
        .await;
    exits
        .send(down::Msg::Interrupt(InterruptRequest {
            command_id: interrupt.to_string(),
            dispatch_attempt: 1,
        }))
        .await;
    let bodies = exits
        .wait_records("closed", |b| {
            b.iter().any(|x| matches!(x, Body::JournalClosed(_)))
        })
        .await;
    assert!(interrupt_states(&bodies, interrupt).ends_with(&[InterruptState::Completed as i32]));
    let stopping = index(&bodies, |b| state(b, "stopping")).unwrap();
    let stopped = index(&bodies, |b| matches!(b, Body::ExecutionState(f) if f.state == "stopped" && f.outcome == "user_stop")).expect("stopped(user_stop)");
    assert!(stopping < stopped);
    assert!(matches!(bodies.last(), Some(Body::JournalClosed(_))));
    assert!(exits.wait_exit().await.success());
}

#[tokio::test]
async fn stop_waits_for_group_empty_including_escaped_children() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    let id = Uuid::now_v7();
    harness
        .deliver(
            id,
            1,
            &script(r#"{"detach_child":true},{"delta":"working"},{"hang":true}"#),
        )
        .await;
    harness
        .wait_records("delta", |b| {
            b.iter().any(|x| matches!(x, Body::BlockDelta(_)))
        })
        .await;
    let supervisor = harness.pid;
    let escaped = children_of(supervisor, "sleep");
    assert_eq!(
        escaped.len(),
        1,
        "the escaped tool process must be adopted by the supervisor"
    );
    harness
        .send(down::Msg::Stop(StopRequest {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            force: false,
        }))
        .await;
    let bodies = harness
        .wait_records("closed", |b| {
            b.iter().any(|x| matches!(x, Body::JournalClosed(_)))
        })
        .await;
    let stopping = index(&bodies, |b| state(b, "stopping")).unwrap();
    let stopped = index(&bodies, |b| matches!(b, Body::ExecutionState(f) if f.state == "stopped" && f.outcome == "user_stop")).expect("stopped(user_stop)");
    assert!(stopping < stopped);
    assert!(
        escaped.iter().all(|pid| !alive(*pid)),
        "terminal state only after the escaped child is gone"
    );
    assert!(
        bodies.iter().any(|b| matches!(b, Body::BlockIncomplete(_))),
        "open blocks end incomplete"
    );
    assert!(matches!(bodies.last(), Some(Body::JournalClosed(_))));
    assert!(harness.wait_exit().await.success());
}

#[tokio::test]
async fn force_stop_preempts_a_soft_close_in_progress() {
    const STUBBORN: &str = r#"#!/usr/bin/python3
import signal, sys, time
if '--version' in sys.argv:
    print('2.1.311'); sys.exit(0)
signal.signal(signal.SIGTERM, signal.SIG_IGN)
while True:
    time.sleep(0.2)
"#;
    let _gate = e2e_gate::acquire().await;
    let tools = TempDir::new().unwrap();
    let exe = tools.path().join("fake-cli");
    fake_exec::write_executable(&exe, STUBBORN, 0o700);
    let spec = format!(
        "[claude_code]\nexecutable = \"{}\"\noauth_poll_interval_s = 1800\nallow_paid_model_poll = false\n",
        exe.display()
    );
    let mut harness = Harness::start_with(&[], "60s", &spec).await;
    harness.attach_and_ship(0).await;
    let workdir = harness._root.path().to_string_lossy().into_owned();
    harness
        .send(down::Msg::StartCli(StartCli {
            profile: "claude_code".into(),
            workdir,
            model: "haiku".into(),
            effort: "low".into(),
            permission_mode: "ask".into(),
            instructions: Some(Instructions {
                mode: "owner".into(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await;
    harness
        .next_message(|message| matches!(message, up::Msg::CliStarted(_)))
        .await;
    harness
        .send(down::Msg::Stop(StopRequest {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            force: false,
        }))
        .await;
    // Мягкая остановка ждёт CLI, а цикл супервизора продолжает обслуживать поток и команды.
    harness
        .wait_records("stopping", |b| b.iter().any(|x| state(x, "stopping")))
        .await;
    harness
        .send(down::Msg::Stop(StopRequest {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 2,
            force: true,
        }))
        .await;
    let bodies = harness
        .wait_records("closed", |b| {
            b.iter().any(|x| matches!(x, Body::JournalClosed(_)))
        })
        .await;
    assert!(
        bodies.iter().any(
            |b| matches!(b, Body::ExecutionState(f) if f.state == "stopped" && f.outcome == "force_stopped")
        ),
        "{bodies:?}"
    );
    assert!(harness.wait_exit().await.success());
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn survivors_keep_execution_stopping_not_terminal() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[("CANTERLOT_FAULT_TERMINATE_SURVIVORS", "2")]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    harness
        .send(down::Msg::Stop(StopRequest {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            force: true,
        }))
        .await;
    let bodies = harness
        .wait_records("survivors diagnostic", |b| {
            b.iter().any(
                |x| matches!(x, Body::AdapterDiagnostic(f) if f.kind == "containment_survivors"),
            )
        })
        .await;
    assert!(index(&bodies, |b| state(b, "stopping")).is_some());
    assert!(
        !bodies.iter().any(
            |b| matches!(b, Body::ExecutionState(f) if f.state == "stopped" || f.state == "failed")
        ),
        "E-3"
    );
    assert!(harness.child.try_wait().unwrap().is_none());
    let bodies = harness
        .wait_records("terminal after the survivors are gone", |b| {
            b.iter().any(|x| matches!(x, Body::JournalClosed(_)))
        })
        .await;
    assert!(index(&bodies, |b| state(b, "stopped")).is_some());
    assert!(harness.wait_exit().await.success());
}

#[tokio::test]
async fn process_exit_by_itself_closes_open_blocks_and_group() {
    let _gate = e2e_gate::acquire().await;
    for (code, state_name, outcome) in [(0, "stopped", "normal_exit"), (3, "failed", "crashed")] {
        let mut harness = Harness::start(&[]).await;
        harness.attach_and_ship(0).await;
        harness.start_cli("synthetic").await;
        let id = Uuid::now_v7();
        harness
            .deliver(
                id,
                1,
                &script(&format!(
                    r#"{{"delta":"partial"}},{{"detach_child":true}},{{"exit":{code}}}"#
                )),
            )
            .await;
        let supervisor = harness.pid;
        let bodies = harness
            .wait_records("closed", |b| {
                b.iter().any(|x| matches!(x, Body::JournalClosed(_)))
            })
            .await;
        let incomplete = index(&bodies, |b| matches!(b, Body::BlockIncomplete(_)))
            .expect("open block must end incomplete");
        let stopping = index(&bodies, |b| state(b, "stopping")).unwrap();
        let terminal = index(&bodies, |b| matches!(b, Body::ExecutionState(f) if f.state == state_name && f.outcome == outcome)).expect("terminal state");
        assert!(incomplete < terminal && stopping < terminal);
        assert!(!bodies.iter().any(|b| matches!(b, Body::RunFinished(_))));
        assert!(
            children_of(supervisor, "sleep").is_empty(),
            "the escaped child must be terminated"
        );
        assert!(harness.wait_exit().await.success());
    }
}

#[tokio::test]
async fn unknown_required_event_fails_with_protocol_incompatible() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    let optional = Uuid::now_v7();
    harness
        .deliver(
            optional,
            1,
            &script(
                r#"{"unknown":{"event_type":"future_optional","required":false}},{"delta":"fine"}"#,
            ),
        )
        .await;
    let bodies = harness
        .wait_records("optional is only diagnosed", |b| run_finished(b, optional))
        .await;
    assert!(bodies.iter().any(|b| matches!(b, Body::AdapterDiagnostic(f) if f.kind == "unknown_optional_event" && f.event_type == "future_optional")));
    assert!(!bodies.iter().any(|b| state(b, "stopping")));
    harness
        .deliver(
            Uuid::now_v7(),
            1,
            &script(r#"{"unknown":{"event_type":"future_required","required":true}}"#),
        )
        .await;
    let bodies = harness
        .wait_records("closed", |b| {
            b.iter().any(|x| matches!(x, Body::JournalClosed(_)))
        })
        .await;
    assert!(bodies.iter().any(|b| matches!(b, Body::AdapterDiagnostic(f) if f.kind == "unknown_required_event" && f.event_type == "future_required")));
    assert!(bodies.iter().any(|b| matches!(b, Body::ExecutionState(f) if f.state == "failed" && f.outcome == "protocol_incompatible")));
    assert!(harness.wait_exit().await.success());
}

#[tokio::test]
async fn unknown_profile_fails_start() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness
        .send(down::Msg::StartCli(StartCli {
            profile: "no-such-adapter".into(),
            workdir: harness._root.path().to_string_lossy().into_owned(),
            native_conversation_id: String::new(),
            ..Default::default()
        }))
        .await;
    let bodies = harness
        .wait_records("closed", |b| {
            b.iter().any(|x| matches!(x, Body::JournalClosed(_)))
        })
        .await;
    assert!(index(&bodies, |b| state(b, "stopping")).is_some());
    assert!(bodies.iter().any(|b| matches!(b, Body::ExecutionState(f) if f.state == "failed" && f.outcome == "start_failed")));
    assert!(harness.wait_exit().await.success());
}

#[tokio::test]
async fn oversized_answer_final_is_checksum_only() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    let big = "a".repeat(16_384);
    let steps = vec![format!(r#"{{"delta":"{big}"}}"#); 5].join(",");
    let id = Uuid::now_v7();
    harness.deliver(id, 1, &script(&steps)).await;
    harness
        .wait_records("run finished", |b| run_finished(b, id))
        .await;
    let bodies: Vec<_> = harness.records.values().cloned().collect();
    let accumulated = "a".repeat(16_384 * 5);
    let Some(Body::BlockFinal(fin)) = bodies.iter().find(|b| matches!(b, Body::BlockFinal(_)))
    else {
        panic!("no final")
    };
    assert!(fin.text_omitted && fin.text.is_empty());
    assert_eq!(fin.checksum, sha(&accumulated));
    let mut rev = None;
    let mut total = 0;
    for body in &bodies {
        match body {
            Body::BlockStarted(f) if f.kind == "text" => rev = Some(f.rev),
            Body::BlockDelta(f) => {
                assert!(f.append.len() <= 16_384);
                assert_eq!(Some(f.base_rev), rev);
                assert_eq!(f.rev, f.base_rev + 1);
                rev = Some(f.rev);
                total += f.append.len();
            }
            _ => {}
        }
    }
    assert_eq!(total, accumulated.len());
    assert_eq!(fin.rev, rev.unwrap() + 1);
    let small = Uuid::now_v7();
    harness.deliver(small, 1, "short answer").await;
    harness
        .wait_records("small finished", |b| run_finished(b, small))
        .await;
    let bodies: Vec<_> = harness.records.values().cloned().collect();
    let finals: Vec<_> = bodies
        .iter()
        .filter_map(|b| match b {
            Body::BlockFinal(f) => Some(f),
            _ => None,
        })
        .collect();
    let last = finals.last().unwrap();
    assert!(
        !last.text_omitted && last.text == "short answer" && last.checksum == sha("short answer")
    );
    harness.shutdown().await;
}

/// Что хаб соберёт по блоку из журнала: дельты подряд от ревизии старта и сам финал.
struct Shipped {
    deltas: String,
    biggest_delta: usize,
    contiguous: bool,
    fin: canterlot_proto::worker::v1::BlockFinal,
}

fn shipped(bodies: &[Body], block: &str) -> Shipped {
    let mut rev = None;
    let mut deltas = String::new();
    let mut biggest_delta = 0;
    let mut contiguous = true;
    let mut fin = None;
    for body in bodies {
        match body {
            Body::BlockStarted(f) if f.block_id == block => rev = Some(f.rev),
            Body::BlockDelta(f) if f.block_id == block => {
                contiguous &= Some(f.base_rev) == rev && f.rev == f.base_rev + 1;
                rev = Some(f.rev);
                biggest_delta = biggest_delta.max(f.append.len());
                deltas.push_str(&f.append);
            }
            Body::BlockFinal(f) if f.block_id == block => {
                contiguous &= Some(f.rev) == rev.map(|rev| rev + 1);
                fin = Some(f.clone());
            }
            _ => {}
        }
    }
    Shipped {
        deltas,
        biggest_delta,
        contiguous,
        fin: fin.expect("block without a final"),
    }
}

fn result_blocks(bodies: &[Body]) -> Vec<String> {
    bodies
        .iter()
        .filter_map(|b| match b {
            Body::BlockStarted(f) if f.kind == "tool_result" => Some(f.block_id.clone()),
            _ => None,
        })
        .collect()
}

async fn run_whole_result(harness: &mut Harness, spec: &str) -> (Vec<Body>, String) {
    let id = Uuid::now_v7();
    harness
        .deliver(id, 1, &script(&format!(r#"{{"whole_result":{spec}}}"#)))
        .await;
    let bodies = harness
        .wait_records("run finished", |b| run_finished(b, id))
        .await;
    let block = result_blocks(&bodies).pop().expect("result block");
    (bodies, block)
}

#[tokio::test]
async fn result_delivered_as_one_record_reaches_the_hub_through_deltas() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    // Порог финала 65 536 байт: на нём и вокруг него текст идёт по-разному.
    let mut problems = Vec::new();
    for (unit, times) in [
        ("a", 65_535),
        ("a", 65_536),
        ("a", 65_537),
        ("я", 32_768),
        ("я", 32_769),
        ("я", 100_000),
        ("😀", 50_001),
    ] {
        let text = unit.repeat(times);
        let (bodies, block) = run_whole_result(
            &mut harness,
            &format!(r#"{{"repeat":"{unit}","times":{times}}}"#),
        )
        .await;
        let sent = shipped(&bodies, &block);
        let label = format!("{times} x {unit} ({} bytes)", text.len());
        let omitted = text.len() > 65_536;
        if sent.fin.checksum != sha(&text) || sent.fin.text_omitted != omitted {
            problems.push(format!("{label}: final {:?}", sent.fin.text_omitted));
        } else if omitted && (sent.deltas != text || !sent.fin.text.is_empty()) {
            problems.push(format!(
                "{label}: final carries no text and deltas hold {} of {} bytes",
                sent.deltas.len(),
                text.len()
            ));
        } else if !omitted && (!sent.deltas.is_empty() || sent.fin.text != text) {
            problems.push(format!("{label}: inline final changed"));
        }
        if sent.biggest_delta > 16_384 || !sent.contiguous {
            problems.push(format!(
                "{label}: biggest delta {}, contiguous {}",
                sent.biggest_delta, sent.contiguous
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    harness.shutdown().await;
}

#[tokio::test]
async fn late_tail_continues_the_streamed_prefix_and_never_replaces_it() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;

    let text = "я".repeat(100_000);
    let prefix = "я".repeat(35_000);
    let (bodies, block) = run_whole_result(
        &mut harness,
        &format!(r#"{{"repeat":"я","times":100000,"streamed":"{prefix}"}}"#),
    )
    .await;
    let sent = shipped(&bodies, &block);
    assert_eq!(
        sent.deltas.len(),
        text.len(),
        "the tail follows the streamed prefix without repeating it"
    );
    assert!(sent.deltas == text && sent.contiguous && sent.biggest_delta <= 16_384);
    assert!(sent.fin.text_omitted && sent.fin.checksum == sha(&text));

    // Начало, которого нет в финале, дописывать нечем: хаб увидит расхождение сумм.
    let other = "ж".repeat(50_000);
    let (bodies, block) = run_whole_result(
        &mut harness,
        &format!(r#"{{"repeat":"я","times":100000,"streamed":"{other}"}}"#),
    )
    .await;
    let sent = shipped(&bodies, &block);
    assert_eq!(sent.deltas, other, "nothing is added to a diverging stream");
    assert!(sent.fin.text_omitted && sent.fin.checksum == sha(&text));
    harness.shutdown().await;
}

#[tokio::test]
async fn detach_waits_for_supervisor_to_drain_sj_acks() {
    let root = TempDir::new().unwrap();
    let listener = tokio::net::UnixListener::bind(root.path().join("ipc.sock")).unwrap();
    let mut conn = connect(root.path()).await;
    let (mut peer, _) = listener.accept().await.unwrap();
    for seq in [5, 1_000_000] {
        write_message(
            &mut conn.write,
            &ControlToSupervisor {
                msg: Some(down::Msg::SjAck(SjAck { up_to_seq: seq })),
            },
        )
        .await
        .unwrap();
    }

    let detached = conn.detach();
    tokio::pin!(detached);
    assert!(
        poll_fn(|cx| Poll::Ready(detached.as_mut().poll(cx).is_pending())).await,
        "detach returned before the supervisor read the queued acks"
    );
    for seq in [5, 1_000_000] {
        let message = read_message::<ControlToSupervisor, _>(&mut peer)
            .await
            .unwrap();
        assert_eq!(
            message.msg,
            Some(down::Msg::SjAck(SjAck { up_to_seq: seq }))
        );
    }
    assert_eq!(
        timeout(WAIT, peer.read(&mut [0]))
            .await
            .expect("client write EOF")
            .unwrap(),
        0,
        "write side is closed"
    );
    assert!(
        poll_fn(|cx| Poll::Ready(detached.as_mut().poll(cx).is_pending())).await,
        "detach must also wait for the supervisor to finish handling the acks"
    );
    drop(peer);
    timeout(WAIT, detached).await.expect("supervisor EOF");
}

#[tokio::test]
async fn sj_ships_from_committed_and_compacts_only_on_ack() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.auto_ack = false;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    let id = Uuid::now_v7();
    harness.deliver(id, 1, "ship me").await;
    while harness.pump(Duration::from_millis(700)).await {}
    assert_eq!(*harness.records.keys().next().unwrap(), 1);
    assert!(
        harness
            .records
            .keys()
            .zip(1..)
            .all(|(seq, expected)| *seq == expected),
        "contiguous from seq 1"
    );
    drop(harness.conn.take());

    let attached = harness.attach().await;
    let last = attached.last_sj_seq;
    assert!(last >= 8, "the whole turn must be in the journal");
    assert_eq!(attached.acked_sj_seq, 0, "no ack, no compaction");
    harness.records.clear();
    harness
        .send(down::Msg::ShipFrom(ShipFrom { committed_seq: 2 }))
        .await;
    harness
        .wait_records("replay from committed + 1", |b| {
            b.len() >= (last as usize - 2)
        })
        .await;
    assert_eq!(*harness.records.keys().next().unwrap(), 3);
    harness.send(down::Msg::SjAck(SjAck { up_to_seq: 5 })).await;
    harness
        .send(down::Msg::SjAck(SjAck {
            up_to_seq: 1_000_000,
        }))
        .await;
    harness.conn.take().unwrap().detach().await;
    let attached = harness.attach().await;
    assert_eq!(
        attached.acked_sj_seq, 5,
        "the ack is applied, an ack beyond the journal end is ignored"
    );

    harness.records.clear();
    harness.auto_ack = true;
    harness
        .send(down::Msg::ShipFrom(ShipFrom { committed_seq: 5 }))
        .await;
    harness
        .send(down::Msg::Stop(StopRequest {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            force: false,
        }))
        .await;
    let bodies = harness
        .wait_records("closed", |b| {
            b.iter().any(|x| matches!(x, Body::JournalClosed(_)))
        })
        .await;
    assert!(matches!(bodies.last(), Some(Body::JournalClosed(_))));
    let closed = *harness.records.keys().last().unwrap();
    harness
        .send(down::Msg::SjAck(SjAck { up_to_seq: closed }))
        .await;
    let exec_dir = harness.exec_dir.clone();
    assert!(harness.wait_exit().await.success());
    let (journal, _) = Journal::open(&exec_dir.join("sj"), JournalOptions::default()).unwrap();
    assert_eq!(journal.acked_seq(), closed);
    assert_eq!(
        journal.first_seq(),
        closed + 1,
        "everything acknowledged is compacted away"
    );
}

#[tokio::test]
async fn control_detach_does_not_stop_the_execution() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.auto_ack = false;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    let id = Uuid::now_v7();
    harness
        .deliver(
            id,
            1,
            &script(
                r#"{"delta":"a"},{"delay_ms":300},{"delta":"b"},{"delay_ms":300},{"delta":"c"}"#,
            ),
        )
        .await;
    harness
        .wait_records("accepted", |b| {
            b.iter().any(|x| matches!(x, Body::InputAccepted(_)))
        })
        .await;
    drop(harness.conn.take());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        harness.child.try_wait().unwrap().is_none(),
        "the supervisor outlives the control connection"
    );
    harness.records.clear();
    harness.auto_ack = true;
    let attached = harness.attach_and_ship(0).await;
    assert!(
        attached.last_sj_seq >= 8,
        "the answer completed without a control connection"
    );
    let bodies = harness
        .wait_records("replay of everything", |b| run_finished(b, id))
        .await;
    assert_eq!(*harness.records.keys().next().unwrap(), 1);
    assert!(
        harness
            .records
            .keys()
            .zip(1..)
            .all(|(seq, expected)| *seq == expected),
        "no gap in the replay"
    );
    assert!(matches!(&bodies[0], Body::JournalOpened(_)));
    let Some(Body::BlockFinal(fin)) = bodies.iter().find(|b| matches!(b, Body::BlockFinal(_)))
    else {
        panic!("no final")
    };
    assert_eq!(fin.text, "abc");
    assert!(!bodies.iter().any(|b| state(b, "stopping")));
    harness.shutdown().await;
}

#[tokio::test]
async fn no_second_cli_is_ever_started() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    let up::Msg::CliStarted(first) = harness.start_cli("synthetic").await else {
        unreachable!()
    };
    let up::Msg::CliStarted(second) = harness.start_cli("synthetic").await else {
        unreachable!()
    };
    assert_eq!(
        (first.pid, first.start_ticks),
        (second.pid, second.start_ticks)
    );
    assert_eq!(children_of(harness.pid, "synthetic-cli").len(), 1);
    let id = Uuid::now_v7();
    harness
        .deliver(id, 1, &script(r#"{"delta":"x"},{"exit":0}"#))
        .await;
    harness
        .wait_records("closed", |b| {
            b.iter().any(|x| matches!(x, Body::JournalClosed(_)))
        })
        .await;
    assert!(harness.wait_exit().await.success());
    assert!(children_of(harness.pid, "synthetic-cli").is_empty());
}

#[tokio::test]
async fn wave2_n1_draining_answers_without_extending_closed_journal() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    let transmitted = Uuid::now_v7();
    harness
        .deliver(transmitted, 1, &script(r#"{"delta":"x"},{"hang":true}"#))
        .await;
    harness
        .wait_records("transmitted", |b| {
            b.iter().any(|b| matches!(b, Body::TransmissionStarted(_)))
        })
        .await;
    harness.auto_ack = false;
    harness
        .send(down::Msg::Stop(StopRequest {
            force: true,
            ..Default::default()
        }))
        .await;
    timeout(WAIT, async {
        while !harness
            .records
            .values()
            .any(|b| matches!(b, Body::JournalClosed(_)))
        {
            let committed_seq = harness.records.last_key_value().map_or(0, |(seq, _)| *seq);
            harness
                .send(down::Msg::ShipFrom(ShipFrom { committed_seq }))
                .await;
            harness.pump(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();

    let closed = *harness.records.last_key_value().unwrap().0;
    for _ in 0..50 {
        let target = Uuid::now_v7();
        let ack = harness.deliver(target, 1, "late").await;
        assert_eq!(
            (ack.command_id, ack.dispatch_attempt, ack.result.as_str()),
            (target.to_string(), 1, "execution_closed")
        );
        assert_eq!(
            harness.deliver(transmitted, 1, "duplicate").await.result,
            "duplicate"
        );
        for (target, cancel, expected) in [
            (target, true, "cancel_won"),
            (Uuid::now_v7(), false, "input_withdrawn"),
            (transmitted, true, "not_applicable"),
            (transmitted, false, "not_applicable"),
        ] {
            let message = if cancel {
                down::Msg::Cancel(canterlot_proto::worker::v1::CancelInput {
                    target_command_id: target.to_string(),
                    target_dispatch_attempt: 1,
                })
            } else {
                down::Msg::Withdraw(canterlot_proto::worker::v1::WithdrawInput {
                    target_command_id: target.to_string(),
                    target_dispatch_attempt: 1,
                })
            };
            harness.send(message).await;
            let up::Msg::DeliverAck(ack) = harness
                .next_message(|m| matches!(m, up::Msg::DeliverAck(_)))
                .await
            else {
                unreachable!()
            };
            assert_eq!(
                (ack.command_id, ack.dispatch_attempt, ack.result.as_str()),
                (target.to_string(), 1, expected)
            );
        }
    }
    harness
        .send(down::Msg::ShipFrom(ShipFrom {
            committed_seq: closed - 1,
        }))
        .await;
    while harness.pump(Duration::from_millis(100)).await {}
    assert_eq!(*harness.records.last_key_value().unwrap().0, closed);
    harness
        .send(down::Msg::SjAck(SjAck { up_to_seq: closed }))
        .await;
    assert!(timeout(WAIT, harness.wait_exit()).await.unwrap().success());
}

#[tokio::test]
async fn wave_w_closed_is_last_record_and_detached_drain_waits_for_ack() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    harness.auto_ack = false;
    harness
        .send(down::Msg::Stop(StopRequest {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            force: true,
        }))
        .await;
    harness
        .wait_records("closed", |b| {
            b.iter().any(|b| matches!(b, Body::JournalClosed(_)))
        })
        .await;
    let closed = *harness.records.last_key_value().unwrap().0;
    for target in [Uuid::now_v7(), Uuid::now_v7()] {
        harness
            .send(down::Msg::Cancel(
                canterlot_proto::worker::v1::CancelInput {
                    target_command_id: target.to_string(),
                    target_dispatch_attempt: 1,
                },
            ))
            .await;
        harness
            .send(down::Msg::Withdraw(
                canterlot_proto::worker::v1::WithdrawInput {
                    target_command_id: target.to_string(),
                    target_dispatch_attempt: 1,
                },
            ))
            .await;
    }
    harness
        .send(down::Msg::Interrupt(
            canterlot_proto::worker::ipc::InterruptRequest {
                command_id: Uuid::now_v7().to_string(),
                dispatch_attempt: 1,
            },
        ))
        .await;
    harness
        .send(down::Msg::ShipFrom(ShipFrom {
            committed_seq: closed,
        }))
        .await;
    while harness.pump(Duration::from_millis(100)).await {}
    assert_eq!(
        harness.records.last_key_value().unwrap().0,
        &closed,
        "records appended after journal_closed"
    );
    harness.conn = None;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(harness.child.try_wait().unwrap().is_none());
    harness.auto_ack = true;
    harness.attach_and_ship(closed - 1).await;
    assert!(timeout(WAIT, harness.wait_exit()).await.unwrap().success());
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn wave_w_containment_errors_report_survivors_before_retry() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[("CANTERLOT_FAULT_TERMINATE_ERRORS", "2")]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    harness
        .send(down::Msg::Stop(StopRequest {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            force: true,
        }))
        .await;
    let bodies = harness
        .wait_records("containment error diagnostic", |b| {
            b.iter().any(
                |b| matches!(b, Body::AdapterDiagnostic(f) if f.kind == "containment_survivors"),
            )
        })
        .await;
    assert!(
        !bodies
            .iter()
            .any(|b| state(b, "stopped") || state(b, "failed"))
    );
    assert!(harness.child.try_wait().unwrap().is_none());
    harness
        .wait_records("closed after retry", |b| {
            b.iter().any(|b| matches!(b, Body::JournalClosed(_)))
        })
        .await;
    assert!(timeout(WAIT, harness.wait_exit()).await.unwrap().success());
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn wave_w_group_commit_keeps_stopped_crash_boundary_durable() {
    let _gate = e2e_gate::acquire().await;
    let mut harness =
        Harness::start(&[("CANTERLOT_FAULT_ABORT_AT", "after_stopped_before_closed")]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli("synthetic").await;
    harness
        .send(down::Msg::Stop(StopRequest {
            command_id: Uuid::now_v7().to_string(),
            dispatch_attempt: 1,
            force: true,
        }))
        .await;
    assert!(!timeout(WAIT, harness.wait_exit()).await.unwrap().success());
    let bodies = read_sj(&harness.exec_dir);
    assert!(matches!(bodies.last(), Some((_, Body::ExecutionState(f))) if f.state == "stopped"));
    assert!(
        !bodies
            .iter()
            .any(|(_, b)| matches!(b, Body::JournalClosed(_)))
    );
}

async fn pilot_supervisor_case(case: &str) {
    let mut h = Harness::start(&[]).await;
    h.attach_and_ship(0).await;
    h.send(down::Msg::StartCli(StartCli {
        profile: "synthetic".into(),
        workdir: h._root.path().to_string_lossy().into_owned(),
        model: "synthetic-large".into(),
        pilot_v1: case != "limit_facts_are_dropped_and_counted_without_pilot_v1",
        instructions: Some(Instructions {
            mode: "clean".into(),
            ..Default::default()
        }),
        ..Default::default()
    }))
    .await;
    h.next_message(|m| matches!(m, up::Msg::CliStarted(_)))
        .await;
    let id = Uuid::now_v7();
    let steps = match case {
        "context_exhausted_fails_the_execution_with_its_outcome" => r#"{"context_exhausted":true}"#,
        "deliver_files_reach_the_adapter_as_local_files" => r#"{"read_attachment":0}"#,
        "phase_detail_and_block_fields_reach_sj" => {
            r#"{"tool":{"name":"Read","summary":"private summary"}}"#
        }
        "run_finished_carries_usage_context_and_duration" => {
            r#"{"delta":"answer"},{"delay_ms":30},{"usage":{"model":"synthetic-small","agent":"Explore","input":120,"output":40}}"#
        }
        "unsupported_setting_is_rejected_before_any_byte_reaches_the_cli_sj" => {
            r#"{"delta":"must not run"}"#
        }
        _ => {
            r#"{"delta":"before"},{"limit_observed":{"window":"weekly","used_permille":960,"resets_in_ms":1000}},{"delta":"after"},{"limit_reached":{"window":"five_hour","resets_in_ms":5000}}"#
        }
    };
    let mut command = h.command(id, 1, &script(steps), 1);
    let mut files = vec![];
    if case == "deliver_files_reach_the_adapter_as_local_files" {
        let path = h.exec_dir.join("file");
        fs::write(&path, b"attachment").unwrap();
        files.push(canterlot_proto::worker::ipc::LocalFile {
            path: path.to_string_lossy().into_owned(),
            name: "file".into(),
            mime: "text/plain".into(),
            size: 10,
            image: false,
        });
    }
    if case == "unsupported_setting_is_rejected_before_any_byte_reaches_the_cli_sj" {
        let Some(Kind::DeliverInput(input)) = &mut command.kind else {
            panic!()
        };
        input.settings = Some(canterlot_proto::worker::v1::SettingsOverride {
            model: "invalid".into(),
            ..Default::default()
        });
    }
    h.send(down::Msg::Deliver(canterlot_proto::worker::ipc::Deliver {
        command: Some(command),
        files,
    }))
    .await;
    let bodies = h
        .wait_records("pilot result", |b| {
            if case == "context_exhausted_fails_the_execution_with_its_outcome" {
                b.iter().any(|r| state(r, "failed"))
            } else if case == "unsupported_setting_is_rejected_before_any_byte_reaches_the_cli_sj" {
                b.iter().any(|r| matches!(r, Body::InputRejected(_)))
            } else {
                run_finished(b, id)
            }
        })
        .await;
    match case {
        "phase_detail_and_block_fields_reach_sj" => {
            assert!(bodies.iter().any(|b| matches!(b,
                Body::PhaseChanged(f) if f.tool_name == "Read" && f.summary == "private summary")));
            assert!(bodies.iter().any(|b| matches!(b,
                Body::BlockStarted(f) if f.agent == "main"
                    && f.model == "synthetic-large"
                    && f.tool_name == "Read"
                    && f.summary == "private summary")));
            assert!(bodies.iter().any(|b| matches!(b,
                Body::BlockFinal(f) if f.summary == "private summary")));
        }
        "run_finished_carries_usage_context_and_duration" => {
            let f = bodies
                .iter()
                .find_map(|b| {
                    if let Body::RunFinished(f) = b {
                        Some(f)
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(f.usage.len(), 1);
            assert_eq!(f.usage[0].input_tokens, 120);
            assert_eq!(f.usage[0].agent, "Explore");
            assert_eq!(f.context_tokens, 160);
            assert_eq!(f.context_window_tokens, 200000);
            assert!(f.duration_ms >= 30);
            assert!(f.machine_time_ms > 1_700_000_000_000);
        }
        "limit_reached_precedes_run_finished_in_sj" => {
            let reached = index(&bodies, |b| matches!(b, Body::LimitReached(_))).unwrap();
            let finished = index(&bodies, |b| matches!(b, Body::RunFinished(_))).unwrap();
            assert!(reached < finished);
            let Body::LimitReached(f) = &bodies[reached] else {
                panic!()
            };
            assert_eq!(f.input_command_id, id.to_string());
            assert_eq!(f.provider, "claude");
            assert!(f.resets_at_ms > 0);
        }
        "limit_observed_is_journaled_in_order_with_blocks" => {
            let before = index(
                &bodies,
                |b| matches!(b, Body::BlockDelta(f) if f.append == "before"),
            );
            let observed = index(
                &bodies,
                |b| matches!(b, Body::LimitObserved(f) if f.used_permille == 960 && f.provider == "claude"),
            );
            let after = index(
                &bodies,
                |b| matches!(b, Body::BlockDelta(f) if f.append == "after"),
            );
            let (before, observed, after) = (before.unwrap(), observed.unwrap(), after.unwrap());
            assert!(before < observed && observed < after);
        }
        "context_exhausted_fails_the_execution_with_its_outcome" => {
            assert!(bodies.iter().any(|b| matches!(b,
                Body::ExecutionState(f) if f.state == "failed" && f.outcome == "context_exhausted")));
        }
        "deliver_files_reach_the_adapter_as_local_files" => {
            let hash = format!("sha256:{}", sha("attachment"));
            assert!(
                bodies
                    .iter()
                    .any(|b| matches!(b, Body::BlockDelta(f) if f.append == hash))
            );
        }
        "limit_facts_are_dropped_and_counted_without_pilot_v1" => {
            assert!(
                !bodies
                    .iter()
                    .any(|b| matches!(b, Body::LimitObserved(_) | Body::LimitReached(_)))
            );
            let dropped = bodies
                .iter()
                .filter(|b| matches!(b,
                    Body::AdapterDiagnostic(f) if f.kind == "capability_missing" && f.event_type == "limit_fact_dropped"))
                .count();
            assert_eq!(dropped, 2);
        }
        "unsupported_setting_is_rejected_before_any_byte_reaches_the_cli_sj" => {
            assert_eq!(transmissions(&bodies, id), 0);
            assert!(bodies.iter().any(|b| matches!(b,
                Body::InputRejected(f) if f.reason == "settings_unavailable")));
            assert!(!h.stdin_log.exists() || fs::read_to_string(&h.stdin_log).unwrap().is_empty());
        }
        _ => unreachable!(),
    }
    h.shutdown().await;
}
macro_rules! pilot_supervisor_tests {
    ($($name:ident),* $(,)?) => {$(
        #[tokio::test]
        async fn $name() {
            let _gate = e2e_gate::acquire().await;
            pilot_supervisor_case(stringify!($name)).await;
        }
    )*};
}
pilot_supervisor_tests!(
    phase_detail_and_block_fields_reach_sj,
    run_finished_carries_usage_context_and_duration,
    limit_reached_precedes_run_finished_in_sj,
    limit_observed_is_journaled_in_order_with_blocks,
    context_exhausted_fails_the_execution_with_its_outcome,
    deliver_files_reach_the_adapter_as_local_files,
    limit_facts_are_dropped_and_counted_without_pilot_v1,
    unsupported_setting_is_rejected_before_any_byte_reaches_the_cli_sj
);

/// PNG с заголовком IHDR: сопровождению важны сигнатура и размеры.
fn png(width: u32, height: u32, tail: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend((0..tail).map(|i| (i % 251) as u8));
    bytes
}

impl Harness {
    async fn start_cli_todo(&mut self, todo_v1: bool) {
        let workdir = self._root.path().to_string_lossy().into_owned();
        self.send(down::Msg::StartCli(StartCli {
            profile: "synthetic".into(),
            workdir,
            pilot_v1: true,
            todo_v1,
            instructions: Some(Instructions {
                mode: "clean".into(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await;
        self.next_message(|message| matches!(message, up::Msg::CliStarted(_)))
            .await;
    }

    async fn start_cli_pilot(&mut self, pilot_v1: bool) {
        let workdir = self._root.path().to_string_lossy().into_owned();
        self.send(down::Msg::StartCli(StartCli {
            profile: "synthetic".into(),
            workdir,
            pilot_v1,
            instructions: Some(Instructions {
                mode: "clean".into(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await;
        self.next_message(|message| matches!(message, up::Msg::CliStarted(_)))
            .await;
    }

    async fn media_upload(&mut self) -> canterlot_proto::worker::ipc::MediaUpload {
        match self
            .next_message(|message| matches!(message, up::Msg::MediaUpload(_)))
            .await
        {
            up::Msg::MediaUpload(request) => request,
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn media_reference_is_written_only_after_media_stored_and_resent_after_reattach() {
    use canterlot_proto::worker::ipc::MediaResult;
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli_pilot(true).await;
    let image = png(12, 7, 300_000);
    let path = harness._root.path().join("screen.png");
    fs::write(&path, &image).unwrap();
    let id = Uuid::now_v7();
    let ack = harness
        .deliver(
            id,
            1,
            &script(r#"{"view_image":"screen.png"},{"delta":"готово"}"#),
        )
        .await;
    assert_eq!(ack.result, "accepted_for_gate");
    let request = harness.media_upload().await;
    let bodies = harness
        .wait_records("tool call block and the answer", |b| run_finished(b, id))
        .await;
    let call = bodies
        .iter()
        .find_map(|b| match b {
            Body::BlockStarted(f) if f.kind == "tool_call" => Some(f.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (call.tool_name.as_str(), call.summary.as_str()),
        ("Read", "screen.png")
    );
    assert!(
        bodies.iter().any(
            |b| matches!(b, Body::BlockFinal(f) if f.text == format!("{} bytes", image.len()))
        )
    );
    assert_eq!(request.block_id, call.block_id);
    assert_eq!(request.mime, "image/png");
    assert_eq!(request.size, image.len() as u64);
    assert_eq!(request.sha256, sha256_hex(&image));
    let copy = harness.exec_dir.join("media").join(&request.media_id);
    assert_eq!(fs::read(&copy).unwrap(), image);
    assert_eq!(
        fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Правка исходного файла после события не меняет загружаемую копию.
    fs::write(&path, b"changed").unwrap();
    // Без MediaStored ссылки нет, даже когда ход закончился.
    while harness.pump(Duration::from_millis(300)).await {}
    assert!(
        !harness
            .records
            .values()
            .any(|b| matches!(b, Body::BlockMedia(_)))
    );

    // Новая управляющая служба получает незавершённый запрос заново.
    harness.conn = None;
    harness
        .attach_and_ship(harness.records.keys().last().copied().unwrap_or(0))
        .await;
    let again = harness.media_upload().await;
    assert_eq!(again, request);
    harness
        .send(down::Msg::MediaResult(MediaResult {
            media_id: request.media_id.clone(),
            stored: true,
            reason: String::new(),
        }))
        .await;
    let bodies = harness
        .wait_records("block media reference", |b| {
            b.iter().any(|b| matches!(b, Body::BlockMedia(_)))
        })
        .await;
    let reference: Vec<_> = bodies
        .iter()
        .filter_map(|b| match b {
            Body::BlockMedia(f) => Some(f.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        reference,
        [canterlot_proto::worker::v1::BlockMedia {
            block_id: call.block_id.clone(),
            media_id: request.media_id.clone(),
            mime: "image/png".into(),
            size: image.len() as u64,
            sha256: sha256_hex(&image),
            width: 12,
            height: 7,
        }]
    );
    assert!(!copy.exists());
    // Повторный ответ для той же картинки второй ссылки не даёт.
    harness
        .send(down::Msg::MediaResult(MediaResult {
            media_id: request.media_id.clone(),
            stored: true,
            reason: String::new(),
        }))
        .await;
    while harness.pump(Duration::from_millis(300)).await {}
    assert_eq!(
        harness
            .records
            .values()
            .filter(|b| matches!(b, Body::BlockMedia(_)))
            .count(),
        1
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn media_is_dropped_without_pilot_v1_and_failed_upload_writes_no_reference() {
    use canterlot_proto::worker::ipc::MediaResult;
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli_pilot(false).await;
    fs::write(harness._root.path().join("a.png"), png(1, 1, 10)).unwrap();
    let id = Uuid::now_v7();
    harness
        .deliver(id, 1, &script(r#"{"view_image":"a.png"}"#))
        .await;
    let bodies = harness.wait_records("turn", |b| run_finished(b, id)).await;
    assert!(bodies.iter().any(|b| matches!(b, Body::AdapterDiagnostic(f) if f.kind == "capability_missing" && f.event_type == "media_dropped")));
    while harness.pump(Duration::from_millis(300)).await {}
    assert!(
        !harness
            .others
            .iter()
            .any(|m| matches!(m.msg, Some(up::Msg::MediaUpload(_))))
    );
    assert!(!harness.exec_dir.join("media").exists());
    harness.shutdown().await;

    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli_pilot(true).await;
    fs::write(harness._root.path().join("b.png"), png(2, 2, 10)).unwrap();
    fs::write(
        harness._root.path().join("c.gif"),
        b"GIF89a\x03\x00\x04\x00\x00\x00\x00",
    )
    .unwrap();
    let id = Uuid::now_v7();
    harness
        .deliver(
            id,
            1,
            &script(r#"{"view_image":"b.png"},{"view_image":"c.gif"}"#),
        )
        .await;
    let first = harness.media_upload().await;
    harness
        .send(down::Msg::MediaResult(MediaResult {
            media_id: first.media_id.clone(),
            stored: false,
            reason: "conflict".into(),
        }))
        .await;
    // Следующая картинка уходит только после ответа о предыдущей.
    let second = harness.media_upload().await;
    assert_ne!(second.media_id, first.media_id);
    assert_eq!(second.mime, "image/gif");
    assert!(
        !harness
            .exec_dir
            .join("media")
            .join(&first.media_id)
            .exists()
    );
    harness
        .send(down::Msg::MediaResult(MediaResult {
            media_id: second.media_id.clone(),
            stored: true,
            reason: String::new(),
        }))
        .await;
    let bodies = harness
        .wait_records("gif reference", |b| {
            b.iter().any(|b| matches!(b, Body::BlockMedia(_)))
        })
        .await;
    let references: Vec<_> = bodies
        .iter()
        .filter_map(|b| match b {
            Body::BlockMedia(f) => Some((f.media_id.clone(), f.width, f.height)),
            _ => None,
        })
        .collect();
    assert_eq!(references, [(second.media_id.clone(), 3, 4)]);
    harness.shutdown().await;
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn todos(bodies: &[Body]) -> Vec<&canterlot_proto::worker::v1::TodoUpdated> {
    bodies
        .iter()
        .filter_map(|body| match body {
            Body::TodoUpdated(fact) => Some(fact),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn todo_list_reaches_sj_whole_with_a_growing_revision_per_lane() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli_todo(true).await;
    let id = Uuid::now_v7();
    let steps = script(
        r#"{"todo":[{"text":"план","status":"in_progress"},{"text":"тесты","status":"pending"}]},
           {"subagent":{"name":"Explore","model":"synthetic-small","task":"t","steps":[
             {"todo":[{"text":"читать","status":"completed"}]}]}},
           {"todo":[{"text":"план","status":"completed"},{"text":"тесты","status":"in_progress"}]}"#,
    );
    harness.deliver(id, 1, &steps).await;
    let bodies = harness.wait_records("turn", |b| run_finished(b, id)).await;
    let facts = todos(&bodies);
    assert_eq!(facts.len(), 3, "{facts:?}");
    let revisions: Vec<u64> = facts.iter().map(|fact| fact.revision).collect();
    assert!(
        revisions.is_sorted() && revisions[0] < revisions[2],
        "{revisions:?}"
    );
    assert_eq!(facts[0].generation, 1);
    assert_eq!(facts[0].execution_id, harness.execution_id.to_string());
    assert_eq!(facts[0].lane_block_id, "");
    assert_eq!(facts[2].lane_block_id, "");
    assert_eq!(facts[2].items.len(), 2);
    assert_eq!(facts[2].items[1].text, "тесты");
    assert_eq!(facts[2].items[1].status, TodoStatus::InProgress as i32);
    let call = bodies
        .iter()
        .find_map(|body| match body {
            Body::BlockStarted(fact) if fact.tool_name == "Agent" => Some(fact.block_id.clone()),
            _ => None,
        })
        .expect("subagent call block");
    assert_eq!(facts[1].lane_block_id, call);
    harness.shutdown().await;
}

#[tokio::test]
async fn oversized_todo_list_is_cut_with_a_marker_and_the_stream_goes_on() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli_todo(true).await;
    let id = Uuid::now_v7();
    let steps =
        script(r#"{"large_todo":{"repeat":"щ","times":3000,"count":1000}},{"delta":"дальше"}"#);
    harness.deliver(id, 1, &steps).await;
    let bodies = harness.wait_records("turn", |b| run_finished(b, id)).await;
    let facts = todos(&bodies);
    assert_eq!(facts.len(), 1, "{bodies:?}");
    assert!(facts[0].omitted > 0);
    assert_eq!(facts[0].items.len() + facts[0].omitted as usize, 1000);
    assert!(
        !bodies
            .iter()
            .any(|b| matches!(b, Body::AdapterDiagnostic(f) if f.event_type == "record_too_large"))
    );
    assert!(
        bodies
            .iter()
            .any(|b| matches!(b, Body::BlockFinal(f) if f.text == "дальше"))
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn todo_is_dropped_once_with_a_diagnostic_without_todo_v1() {
    let _gate = e2e_gate::acquire().await;
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness.start_cli_todo(false).await;
    let id = Uuid::now_v7();
    let steps = script(
        r#"{"todo":[{"text":"а","status":"pending"}]},{"todo":[{"text":"б","status":"pending"}]}"#,
    );
    harness.deliver(id, 1, &steps).await;
    let bodies = harness.wait_records("turn", |b| run_finished(b, id)).await;
    assert!(todos(&bodies).is_empty());
    let drops = bodies
        .iter()
        .filter(|b| {
            matches!(b, Body::AdapterDiagnostic(f)
                if f.kind == "capability_missing" && f.event_type == "todo_dropped")
        })
        .count();
    assert_eq!(drops, 1);
    harness.shutdown().await;
}
