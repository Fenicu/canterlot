#![forbid(unsafe_code)]
#![cfg(feature = "synthetic-adapter")]

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use canterlot_journal::{Journal, JournalOptions};
use canterlot_proto::worker::{
    ipc::{
        Attach, Attached, ControlToSupervisor, DeliverAck, ShipFrom, SjAck, StartCli, StopRequest,
        SupervisorToControl, control_to_supervisor as down, supervisor_to_control as up,
    },
    v1::{
        Command, ContentBlock, DeliverInput, InterruptState, RecordBody, TextBlock, command::Kind,
        content_block, record_body::Body,
    },
};
use canterlot_worker::supervisor::ipc::{read_message, write_message};
use prost::Message;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{
    net::{UnixStream, unix::OwnedWriteHalf},
    process::Child,
    sync::mpsc,
    time::timeout,
};
use uuid::Uuid;

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
                "execution_id = \"{execution_id}\"\ngeneration = 1\nconversation_id = \"{conversation_id}\"\nprotocol_version = 1\n[timers]\nattach_wait = \"30s\"\nready_deadline = \"10s\"\ntransmit_deadline = \"2s\"\nstop_grace = \"1s\"\nstop_retry = \"200ms\"\n"
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

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn survivors_keep_execution_stopping_not_terminal() {
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
    let mut harness = Harness::start(&[]).await;
    harness.attach_and_ship(0).await;
    harness
        .send(down::Msg::StartCli(StartCli {
            profile: "no-such-adapter".into(),
            workdir: harness._root.path().to_string_lossy().into_owned(),
            native_conversation_id: String::new(),
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

#[tokio::test]
async fn sj_ships_from_committed_and_compacts_only_on_ack() {
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
    drop(harness.conn.take());
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
