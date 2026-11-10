//! Тесты задачи 1 плана 2c: инструкции агента между хабом, воркером и сопровождением.

use canterlot_proto::{
    limits::{LimitError, MAX_AGENT_CALL_BYTES, MAX_PRESET_TEXT_BYTES, check_deliver_input},
    worker::v1::{
        Command, ContentBlock, DeliverInput, Instructions, SettingsOverride, command, content_block,
    },
};
use canterlot_worker::adapter::{InstructionError, InstructionMode, InstructionSpec};
use uuid::Uuid;

fn instructions(mode: &str, text: &str, preset_id: &str, revision: u64) -> Instructions {
    Instructions {
        mode: mode.into(),
        text: text.into(),
        preset_id: preset_id.into(),
        revision,
        title: "Исполнитель".into(),
    }
}

fn deliver_with_instructions(text: &str, preset_text: &str) -> Command {
    let preset_id = Uuid::now_v7().to_string();
    Command {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 1,
        execution_id: Uuid::now_v7().to_string(),
        execution_generation: 1,
        kind: Some(command::Kind::DeliverInput(DeliverInput {
            input_id: Uuid::now_v7().to_string(),
            conversation_id: Uuid::now_v7().to_string(),
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(
                    canterlot_proto::worker::v1::TextBlock { text: text.into() },
                )),
            }],
            settings: Some(SettingsOverride {
                model: "synthetic-small".into(),
                instructions: Some(instructions("preset", preset_text, &preset_id, 2)),
                ..Default::default()
            }),
        })),
    }
}

#[test]
fn instruction_spec_from_proto_validates_mode_and_text() {
    let id = Uuid::now_v7().to_string();
    // Чистый текст в clean/owner запрещён.
    assert!(matches!(
        InstructionSpec::from_proto(Some(&instructions("clean", "текст", "", 0))),
        Err(InstructionError::UnexpectedText)
    ));
    assert!(matches!(
        InstructionSpec::from_proto(Some(&instructions("owner", "текст", "", 0))),
        Err(InstructionError::UnexpectedText)
    ));
    // Пустой пресет, переполнение, плохой UUID, неизвестный режим.
    assert!(matches!(
        InstructionSpec::from_proto(Some(&instructions("preset", "   ", &id, 1))),
        Err(InstructionError::EmptyText)
    ));
    assert!(matches!(
        InstructionSpec::from_proto(Some(&instructions(
            "preset",
            &"я".repeat(MAX_PRESET_TEXT_BYTES + 1),
            &id,
            1
        ))),
        Err(InstructionError::TextTooLarge)
    ));
    assert!(matches!(
        InstructionSpec::from_proto(Some(&instructions("preset", "текст", "не-uuid", 1))),
        Err(InstructionError::BadPresetId)
    ));
    assert!(matches!(
        InstructionSpec::from_proto(Some(&instructions("preset", "", "", 0))),
        Err(InstructionError::EmptyText)
    ));
    assert!(matches!(
        InstructionSpec::from_proto(Some(&instructions("странный", "", "", 0))),
        Err(InstructionError::UnknownMode)
    ));
    // Корректные значения.
    let clean = InstructionSpec::from_proto(Some(&instructions("clean", "", "", 0))).unwrap();
    assert_eq!(clean.mode, InstructionMode::Clean);
    let preset =
        InstructionSpec::from_proto(Some(&instructions("preset", "текст", &id, 7))).unwrap();
    assert_eq!(preset.mode, InstructionMode::Preset);
    assert_eq!(preset.text, "текст");
    assert_eq!(preset.preset_id, Some(id.parse().unwrap()));
    assert_eq!(preset.revision, 7);
    assert_eq!(preset.title, "Исполнитель");
    assert!(preset.same_effect(&preset.clone()));
    assert!(!preset.same_effect(&clean));
    // Старший хаб молчит — значит конфигурация владельца.
    let owner = InstructionSpec::from_proto(None).unwrap();
    assert_eq!(owner.mode, InstructionMode::Owner);
    // Круговой обход через proto сохраняет смысл.
    let round = InstructionSpec::from_proto(Some(&preset.to_proto())).unwrap();
    assert!(round.same_effect(&preset));
    assert_eq!(round.text, "текст");
}

#[test]
fn instruction_spec_debug_never_contains_text() {
    let marker = "PRIVATE_INSTRUCTION_TEXT_2C1";
    let preset = InstructionSpec::from_proto(Some(&instructions(
        "preset",
        marker,
        &Uuid::now_v7().to_string(),
        3,
    )))
    .unwrap();
    let debug = format!("{preset:?}");
    assert!(!debug.contains(marker), "{debug}");
    assert!(debug.contains(&marker.len().to_string()), "{debug}");
    assert!(debug.contains("text_bytes"), "{debug}");
    let proto = preset.to_proto();
    let debug = format!("{proto:?}");
    assert!(!debug.contains(marker), "{debug}");
}

#[test]
fn deliver_input_meta_budget_excludes_instructions_text() {
    assert_eq!(MAX_PRESET_TEXT_BYTES, 65_536);
    assert_eq!(MAX_AGENT_CALL_BYTES, 262_144);
    // Пресет ровно в лимите плюс короткий ввод проходит.
    let fitting = deliver_with_instructions("привет", &"x".repeat(MAX_PRESET_TEXT_BYTES));
    assert_eq!(check_deliver_input(&fitting), Ok(()));
    // Лишний байт в тексте пресета — отдельная ошибка, а не переполнение меты.
    let over = deliver_with_instructions("привет", &"x".repeat(MAX_PRESET_TEXT_BYTES + 1));
    assert_eq!(
        check_deliver_input(&over),
        Err(LimitError::InstructionsText {
            actual: MAX_PRESET_TEXT_BYTES + 1
        })
    );
}

#[test]
fn worker_config_rejects_instruction_profiles_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.toml");
    let base = format!(
        "hub_url = \"https://hub.example:8443\"\nca_cert = \"ca.pem\"\ntoken_file = \"token\"\nmachine_id = \"{0}\"\ninstallation_id = \"{0}\"\nrestore_generation = \"{0}\"\ndata_dir = \"data\"\n",
        Uuid::nil()
    );
    std::fs::write(
        &path,
        format!(
            "{base}\n[[instruction_profiles]]\nname = \"executor\"\ntitle = \"Исполнитель\"\ndescription = \"Профиль\"\nharnesses = [\"claude_code\"]\n"
        ),
    )
    .unwrap();
    let error = canterlot_worker::config::load(&path).unwrap_err();
    assert_eq!(
        error.category,
        canterlot_worker::config::ConfigErrorCategory::Parse
    );
    // Тот же файл без таблицы читается: отказ именно из-за профилей.
    std::fs::write(&path, base).unwrap();
    assert!(canterlot_worker::config::load(&path).is_ok());
}

#[cfg(feature = "synthetic-adapter")]
#[path = "support/process_guard.rs"]
mod process_guard;

#[cfg(feature = "synthetic-adapter")]
mod harness {
    use super::process_guard::ProcessGuard;
    use canterlot_proto::worker::{
        ipc::{
            Attached, ControlToSupervisor, Deliver, DeliverAck, SupervisorToControl,
            control_to_supervisor as down, supervisor_to_control as up,
        },
        v1::{
            Command, ContentBlock, DeliverInput, SettingsOverride, TextBlock, command,
            content_block,
        },
    };
    use canterlot_worker::supervisor::ipc::{read_message, write_message};
    use std::{
        fs,
        path::{Path, PathBuf},
        time::Duration,
    };
    use tempfile::TempDir;
    use tokio::{
        io::AsyncWriteExt,
        net::{UnixStream, unix::OwnedWriteHalf},
        process::Child,
        sync::mpsc,
        time::timeout,
    };
    use uuid::Uuid;

    pub const WAIT: Duration = Duration::from_secs(20);

    pub struct Harness {
        pub _guard: ProcessGuard,
        pub exec_dir: PathBuf,
        pub _root: TempDir,
        pub secret: Vec<u8>,
        pub execution_id: Uuid,
        pub conversation_id: Uuid,
        pub _child: Child,
        pub conn: Option<Conn>,
    }

    pub struct Conn {
        write: OwnedWriteHalf,
        rx: mpsc::UnboundedReceiver<SupervisorToControl>,
    }

    impl Conn {
        async fn detach(mut self) {
            self.write.shutdown().await.unwrap();
            timeout(WAIT, async { while self.rx.recv().await.is_some() {} })
                .await
                .expect("no drain");
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
        .unwrap();
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
        pub async fn start() -> Harness {
            let root = TempDir::new().unwrap();
            let exec_dir = root.path().join("exec");
            fs::create_dir(&exec_dir).unwrap();
            fs::set_permissions(
                &exec_dir,
                std::os::unix::fs::PermissionsExt::from_mode(0o700),
            )
            .unwrap();
            let secret: Vec<u8> = (0..32).map(|_| rand_byte()).collect();
            fs::write(exec_dir.join("secret"), &secret).unwrap();
            fs::set_permissions(
                exec_dir.join("secret"),
                std::os::unix::fs::PermissionsExt::from_mode(0o600),
            )
            .unwrap();
            let execution_id = Uuid::now_v7();
            let conversation_id = Uuid::now_v7();
            fs::write(
                exec_dir.join("spec.toml"),
                format!(
                    "execution_id = \"{execution_id}\"\ngeneration = 1\nconversation_id = \"{conversation_id}\"\nprotocol_version = 1\n[timers]\nattach_wait = \"30s\"\nready_deadline = \"10s\"\ntransmit_deadline = \"2s\"\nstop_grace = \"1s\"\nstop_retry = \"200ms\"\n"
                ),
            )
            .unwrap();
            let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"));
            command
                .args(["supervise", "--exec-dir"])
                .arg(&exec_dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .kill_on_drop(true);
            let child = command.spawn().unwrap();
            Harness {
                _guard: ProcessGuard::new(root.path()),
                exec_dir,
                _root: root,
                secret,
                execution_id,
                conversation_id,
                _child: child,
                conn: None,
            }
        }

        pub async fn attach(&mut self) {
            use canterlot_proto::worker::ipc::{Attach, ShipFrom};
            let mut conn = connect(&self.exec_dir).await;
            let message = ControlToSupervisor {
                msg: Some(down::Msg::Attach(Attach {
                    protocol_version: 1,
                    secret: self.secret.clone(),
                    execution_id: self.execution_id.to_string(),
                    generation: 1,
                })),
            };
            write_message(&mut conn.write, &message).await.unwrap();
            let reply = timeout(WAIT, conn.rx.recv())
                .await
                .unwrap()
                .expect("no Attached");
            assert!(matches!(
                reply.msg,
                Some(up::Msg::Attached(Attached { .. }))
            ));
            self.conn = Some(conn);
            self.send(down::Msg::ShipFrom(ShipFrom { committed_seq: 0 }))
                .await;
        }

        pub async fn send(&mut self, message: down::Msg) {
            let conn = self.conn.as_mut().expect("not attached");
            write_message(&mut conn.write, &ControlToSupervisor { msg: Some(message) })
                .await
                .unwrap();
        }

        pub async fn next(&mut self) -> SupervisorToControl {
            let conn = self.conn.as_mut().expect("not attached");
            timeout(WAIT, conn.rx.recv())
                .await
                .unwrap()
                .expect("control closed")
        }

        pub async fn next_matching(
            &mut self,
            want: impl Fn(&SupervisorToControl) -> bool,
        ) -> SupervisorToControl {
            loop {
                let message = self.next().await;
                if want(&message) {
                    return message;
                }
            }
        }

        pub fn deliver(
            &self,
            id: Uuid,
            attempt: u32,
            text: &str,
            settings: Option<SettingsOverride>,
        ) -> Command {
            Command {
                command_id: id.to_string(),
                dispatch_attempt: attempt,
                execution_id: self.execution_id.to_string(),
                execution_generation: 1,
                kind: Some(command::Kind::DeliverInput(DeliverInput {
                    input_id: Uuid::now_v7().to_string(),
                    conversation_id: self.conversation_id.to_string(),
                    content: vec![ContentBlock {
                        block: Some(content_block::Block::Text(TextBlock { text: text.into() })),
                    }],
                    settings,
                })),
            }
        }

        pub async fn deliver_command(&mut self, command: Command) -> DeliverAck {
            self.send(down::Msg::Deliver(Deliver {
                command: Some(command),
                ..Default::default()
            }))
            .await;
            match self
                .next_matching(|m| matches!(m.msg, Some(up::Msg::DeliverAck(_))))
                .await
                .msg
            {
                Some(up::Msg::DeliverAck(ack)) => ack,
                _ => unreachable!(),
            }
        }

        pub async fn stop(self) {
            if let Some(conn) = self.conn {
                conn.detach().await;
            }
        }
    }

    fn rand_byte() -> u8 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut hasher);
        std::thread::current().id().hash(&mut hasher);
        hasher.finish() as u8
    }

    pub fn script(steps: &str) -> String {
        format!(r#"{{"synthetic":[{steps}]}}"#)
    }
}

#[cfg(feature = "synthetic-adapter")]
#[tokio::test]
async fn start_cli_carries_instructions_to_the_adapter() {
    use canterlot_proto::worker::{
        ipc::{SjAck, StartCli, control_to_supervisor as down, supervisor_to_control as up},
        v1::{Instructions, record_body::Body},
    };
    use harness::*;

    let mut h = Harness::start().await;
    h.attach().await;
    let preset_id = Uuid::now_v7().to_string();
    let started_instructions = Instructions {
        mode: "preset".into(),
        text: "привет".into(),
        preset_id: preset_id.clone(),
        revision: 3,
        title: "Исполнитель".into(),
    };
    h.send(down::Msg::StartCli(StartCli {
        profile: "synthetic".into(),
        workdir: h._root.path().to_string_lossy().into_owned(),
        instructions: Some(started_instructions.clone()),
        pilot_v1: false,
        ..Default::default()
    }))
    .await;
    h.next_matching(|m| matches!(m.msg, Some(up::Msg::CliStarted(_))))
        .await;
    use canterlot_proto::worker::v1::SettingsOverride;
    h.deliver_command(h.deliver(
        Uuid::now_v7(),
        1,
        &script(r#"{"echo_instructions":true}"#),
        Some(SettingsOverride {
            instructions: Some(started_instructions),
            ..Default::default()
        }),
    ))
    .await;
    let deadline = std::time::Instant::now() + WAIT;
    let hit = loop {
        assert!(std::time::Instant::now() < deadline, "нет блока echo");
        let message = h.next().await;
        let Some(up::Msg::SjBatch(batch)) = message.msg else {
            continue;
        };
        let mut hit = None;
        for record in &batch.records {
            let Some(Body::BlockFinal(final_block)) =
                record.body.clone().and_then(|body| body.body)
            else {
                continue;
            };
            if final_block
                .text
                .starts_with("instructions mode=preset revision=3 ")
            {
                hit = Some(final_block.text);
                break;
            }
        }
        // Окно отгрузки SJ ограничено: подтверждаем каждый кадр.
        if let Some(last) = batch.records.last() {
            h.send(down::Msg::SjAck(SjAck {
                up_to_seq: last.seq,
            }))
            .await;
        }
        if let Some(hit) = hit {
            break hit;
        }
    };
    assert!(hit.contains("bytes=12"), "{hit}");
    assert!(hit.contains("sha256="), "{hit}");
    assert!(!hit.contains("привет"), "текст пресета не в блоке: {hit}");
    h.stop().await;
}

#[cfg(feature = "synthetic-adapter")]
mod manager_tests {
    use super::harness::script;
    use canterlot_proto::worker::v1::{
        AgentCallRequest, AgentCallResponse, Command, ContentBlock, DeliverInput, SettingsOverride,
        StartExecution, TextBlock, command::Kind, content_block, record_body::Body,
    };
    use canterlot_worker::{
        config::{ExecutionConfig, SupervisorTimers},
        exec::{AgentCaller, ExecutionManager, SjRelay, agent_call},
        session_store::StoreThread,
        store::{Accept, WorkerStore},
    };
    use std::{
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    };
    use tempfile::TempDir;
    use tokio::{sync::mpsc, time::timeout};
    use uuid::Uuid;

    use super::process_guard::ProcessGuard;

    const WAIT: Duration = Duration::from_secs(20);

    fn command(id: Uuid, attempt: u32, execution: Uuid, generation: u64, kind: Kind) -> Command {
        Command {
            command_id: id.to_string(),
            dispatch_attempt: attempt,
            execution_id: execution.to_string(),
            execution_generation: generation,
            kind: Some(kind),
        }
    }

    fn start_command(
        execution: Uuid,
        generation: u64,
        conversation: Uuid,
        workdir: &Path,
        instructions: Option<canterlot_proto::worker::v1::Instructions>,
    ) -> Command {
        command(
            Uuid::now_v7(),
            1,
            execution,
            generation,
            Kind::StartExecution(StartExecution {
                conversation_id: conversation.to_string(),
                harness: "synthetic".into(),
                workdir: workdir.to_string_lossy().into_owned(),
                native_conversation_id: String::new(),
                model: "synthetic-small".into(),
                effort: "medium".into(),
                permission_mode: "owner".into(),
                instructions,
                capabilities: vec!["pilot_v1".into()],
                worktree: None,
                isolate_mcp: false,
            }),
        )
    }

    fn deliver_command(
        id: Uuid,
        attempt: u32,
        execution: Uuid,
        generation: u64,
        conversation: Uuid,
        text: &str,
        settings: Option<SettingsOverride>,
    ) -> Command {
        command(
            id,
            attempt,
            execution,
            generation,
            Kind::DeliverInput(DeliverInput {
                input_id: Uuid::now_v7().to_string(),
                conversation_id: conversation.to_string(),
                content: vec![ContentBlock {
                    block: Some(content_block::Block::Text(TextBlock { text: text.into() })),
                }],
                settings,
            }),
        )
    }

    struct Env {
        #[allow(dead_code)]
        guard: ProcessGuard,
        root: TempDir,
        work: PathBuf,
        thread: Option<StoreThread>,
        manager: Option<ExecutionManager>,
        #[allow(dead_code)]
        runtime: TempDir,
        relay: mpsc::Receiver<SjRelay>,
        execution: Uuid,
        conversation: Uuid,
        seen_texts: Vec<String>,
    }
    impl Env {
        async fn new() -> Env {
            let root = TempDir::new().unwrap();
            let work = root.path().join("work");
            fs::create_dir(&work).unwrap();
            let config = ExecutionConfig {
                allowed_workdir_roots: vec![work.clone()],
                supervisor_exe: Some(PathBuf::from(env!("CARGO_BIN_EXE_canterlot-worker"))),
                spawn_timeout: Duration::from_secs(10),
                build_jobs: 6,
                supervisor: SupervisorTimers {
                    stop_grace: Duration::from_secs(1),
                    stop_retry: Duration::from_millis(200),
                    ..Default::default()
                },
            };
            let thread = StoreThread::new(WorkerStore::open(&root.path().join("store")).unwrap());
            let (sender, relay) = mpsc::channel(64);
            let runtime = tempfile::Builder::new()
                .prefix("cl-rt-")
                .tempdir_in("/tmp")
                .unwrap();
            let manager = ExecutionManager::new_with_runtime_dir(
                config,
                thread.client(),
                sender,
                Some(runtime.path().to_path_buf()),
            );
            manager.on_hub_session(HashMap::new());
            Env {
                guard: ProcessGuard::new(root.path()),
                root,
                work,
                thread: Some(thread),
                manager: Some(manager),
                runtime,
                relay,
                execution: Uuid::now_v7(),
                conversation: Uuid::now_v7(),
                seen_texts: Vec::new(),
            }
        }

        fn manager(&self) -> &ExecutionManager {
            self.manager.as_ref().unwrap()
        }

        async fn send(&self, cmd: Command) {
            let copy = cmd.clone();
            let accept = self
                .thread
                .as_ref()
                .unwrap()
                .call(move |store| Ok(store.as_mut().unwrap().accept_command(&copy)?))
                .await
                .unwrap();
            assert!(matches!(
                accept,
                Accept::Stored { .. } | Accept::Redelivered { .. } | Accept::Duplicate
            ));
            self.manager().handle(cmd, accept).await.unwrap();
        }

        async fn start(&self, instructions: Option<canterlot_proto::worker::v1::Instructions>) {
            self.send(start_command(
                self.execution,
                1,
                self.conversation,
                &self.work,
                instructions,
            ))
            .await;
        }

        fn logs(&self) -> String {
            let mut text = String::new();
            if let Ok(entries) = std::fs::read_dir(self.root.path().join("store/executions")) {
                for entry in entries.flatten() {
                    text += &std::fs::read_to_string(entry.path().join("supervisor.log"))
                        .unwrap_or_default();
                }
            }
            text
        }

        async fn pump(&mut self, within: Duration) -> Vec<Body> {
            let mut out = Vec::new();
            while let Ok(Some(batch)) = timeout(within, self.relay.recv()).await {
                let mut last = 0;
                for record in batch.records {
                    last = last.max(record.seq);
                    let Some(body) = record.body.and_then(|body| body.body) else {
                        continue;
                    };
                    if let Body::BlockFinal(f) = &body {
                        self.seen_texts.push(f.text.clone());
                    }
                    out.push(body);
                }
                self.manager().on_sj_ack(batch.journal_id, last);
                if out.iter().any(|b| matches!(b, Body::RunFinished(_))) {
                    break;
                }
            }
            out
        }

        async fn cj(&self) -> Vec<Body> {
            self.thread
                .as_ref()
                .unwrap()
                .call(|store| {
                    Ok(store
                        .as_ref()
                        .unwrap()
                        .cj_records_from(1, usize::MAX)
                        .unwrap()
                        .into_iter()
                        .filter_map(|record| record.body.and_then(|body| body.body))
                        .collect())
                })
                .await
                .unwrap()
        }

        async fn wait_text(&mut self, needle: &str) -> String {
            self.wait_text_within(needle, WAIT).await
        }

        async fn wait_text_within(&mut self, needle: &str, limit: Duration) -> String {
            let deadline = tokio::time::Instant::now() + limit;
            loop {
                let bodies = self.pump(Duration::from_millis(200)).await;
                for body in bodies {
                    if let Body::BlockFinal(f) = body
                        && f.text.contains(needle)
                    {
                        return f.text;
                    }
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "нет блока {needle}; cj={:?}; supervisor: {}",
                    self.cj().await,
                    self.logs()
                );
            }
        }

        async fn ownership_cli_pid(&self) -> Option<u32> {
            let id = self.execution;
            self.thread
                .as_ref()
                .unwrap()
                .call(move |store| {
                    Ok(store
                        .as_ref()
                        .unwrap()
                        .ownership()
                        .get(id)
                        .map(|record| record.cli_pid))
                })
                .await
                .unwrap()
        }

        async fn wait_cli_pid(&self, different_from: Option<u32>) -> u32 {
            timeout(WAIT, async {
                loop {
                    if let Some(pid) = self.ownership_cli_pid().await
                        && pid != 0
                        && Some(pid) != different_from
                    {
                        return pid;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "ownership pid не обновился (ждали отлично от {different_from:?}); log: {}",
                    self.logs()
                )
            })
        }
    }

    fn preset_instructions(
        preset_id: &str,
        revision: u64,
        text: &str,
    ) -> canterlot_proto::worker::v1::Instructions {
        canterlot_proto::worker::v1::Instructions {
            mode: "preset".into(),
            text: text.into(),
            preset_id: preset_id.into(),
            revision,
            title: "Исполнитель".into(),
        }
    }

    fn preset_settings(preset_id: &str, revision: u64, text: &str) -> SettingsOverride {
        SettingsOverride {
            instructions: Some(preset_instructions(preset_id, revision, text)),
            ..Default::default()
        }
    }

    struct JsonCaller {
        result_json: String,
    }

    #[async_trait::async_trait]
    impl AgentCaller for JsonCaller {
        async fn call(
            &self,
            _request: AgentCallRequest,
        ) -> Result<AgentCallResponse, tonic::Status> {
            Ok(AgentCallResponse {
                result_json: self.result_json.clone(),
                ..Default::default()
            })
        }
    }

    /// Хаб отвечает не сразу: так выглядит долгое ожидание сессии.
    struct SlowCaller {
        delay: Duration,
    }

    #[async_trait::async_trait]
    impl AgentCaller for SlowCaller {
        async fn call(
            &self,
            _request: AgentCallRequest,
        ) -> Result<AgentCallResponse, tonic::Status> {
            tokio::time::sleep(self.delay).await;
            Ok(AgentCallResponse {
                result_json: r#"{"state":"idle","timed_out":false}"#.into(),
                ..Default::default()
            })
        }
    }

    struct HangCaller;

    #[async_trait::async_trait]
    impl AgentCaller for HangCaller {
        async fn call(
            &self,
            _request: AgentCallRequest,
        ) -> Result<AgentCallResponse, tonic::Status> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn settings_change_with_instructions_restarts_synthetic_cli_and_updates_ownership_pid() {
        let mut env = Env::new().await;
        let preset_id = Uuid::now_v7().to_string();
        env.start(Some(preset_instructions(&preset_id, 1, "один")))
            .await;
        env.send(deliver_command(
            Uuid::now_v7(),
            1,
            env.execution,
            1,
            env.conversation,
            &script(r#"{"echo_instructions":true}"#),
            Some(preset_settings(&preset_id, 1, "один")),
        ))
        .await;
        let first = env.wait_text("revision=1").await;
        assert!(
            first.starts_with("instructions mode=preset revision=1 "),
            "{first}"
        );
        let pid_before = env.wait_cli_pid(None).await;
        // Смена версии пресета действует со следующего хода: CLI перезапускается.
        env.send(deliver_command(
            Uuid::now_v7(),
            1,
            env.execution,
            1,
            env.conversation,
            &script(r#"{"echo_instructions":true}"#),
            Some(preset_settings(&preset_id, 2, "два")),
        ))
        .await;
        let pid_after = env.wait_cli_pid(Some(pid_before)).await;
        assert_ne!(pid_after, pid_before);
        // Каждый ввод обработан ровно один раз: ждём эхо второго и считаем оба.
        let deadline = tokio::time::Instant::now() + WAIT;
        while tokio::time::Instant::now() < deadline
            && !env
                .seen_texts
                .iter()
                .any(|t| t.contains("revision=2 bytes="))
        {
            env.pump(Duration::from_millis(200)).await;
        }
        let rev1 = env
            .seen_texts
            .iter()
            .filter(|t| t.contains("revision=1"))
            .count();
        let rev2 = env
            .seen_texts
            .iter()
            .filter(|t| t.contains("revision=2 bytes="))
            .count();
        assert_eq!(
            (rev1, rev2),
            (1, 1),
            "ввод дошёл один раз: {:?}",
            env.seen_texts
        );
    }

    #[tokio::test]
    async fn agent_call_round_trip_through_supervisor_and_manager() {
        let mut env = Env::new().await;
        env.manager().set_agent_caller(Some(Arc::new(JsonCaller {
            result_json: r#"{"presets":[]}"#.into(),
        })));
        let preset_id = Uuid::now_v7().to_string();
        env.start(Some(preset_instructions(&preset_id, 1, "один")))
            .await;
        env.send(deliver_command(
            Uuid::now_v7(),
            1,
            env.execution,
            1,
            env.conversation,
            &script(r#"{"agent_call":{"tool":"canterlot_presets_list","arguments":{}}}"#),
            None,
        ))
        .await;
        let text = env.wait_text("presets").await;
        assert!(text.contains(r#"{"presets":[]}"#), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn agent_call_without_hub_session_fails_fast_and_times_out_otherwise() {
        use canterlot_worker::adapter::AgentCallResult;
        use serde_json::json;

        // Без сессии хаба — быстрый отказ без ожидания.
        let fast = agent_call(
            None,
            Uuid::now_v7(),
            1,
            Uuid::now_v7(),
            "canterlot_presets_list".into(),
            json!({}),
        )
        .await;
        assert!(
            matches!(fast, AgentCallResult::Err { ref code, .. } if code == "hub_unreachable"),
            "{fast:?}"
        );
        // Висящий клиент — таймаут RPC по часам теста.
        let hanging = tokio::spawn(agent_call(
            Some(Arc::new(HangCaller) as Arc<dyn AgentCaller>),
            Uuid::now_v7(),
            1,
            Uuid::now_v7(),
            "canterlot_presets_list".into(),
            json!({}),
        ));
        tokio::time::advance(Duration::from_secs(20)).await;
        let slow = hanging.await.unwrap();
        assert!(
            matches!(slow, AgentCallResult::Err { ref code, .. } if code == "hub_timeout"),
            "{slow:?}"
        );
    }

    /// Ожидание дольше прежних 20 с проходит и через управляющую службу, и через менеджер.
    async fn long_wait_round_trip(hub_delay: Duration, timeout_seconds: u64) {
        let mut env = Env::new().await;
        env.manager()
            .set_agent_caller(Some(Arc::new(SlowCaller { delay: hub_delay })));
        let preset_id = Uuid::now_v7().to_string();
        env.start(Some(preset_instructions(&preset_id, 1, "один")))
            .await;
        let step = format!(
            r#"{{"agent_call":{{"tool":"sessions.wait","arguments":{{"session_id":"{}","timeout_seconds":{timeout_seconds}}}}}}}"#,
            Uuid::now_v7()
        );
        env.send(deliver_command(
            Uuid::now_v7(),
            1,
            env.execution,
            1,
            env.conversation,
            &script(&step),
            None,
        ))
        .await;
        let text = env
            .wait_text_within("timed_out", hub_delay + Duration::from_secs(30))
            .await;
        assert!(text.contains(r#""state":"idle""#), "{text}");
        assert!(!text.contains("hub_timeout"), "{text}");
    }

    #[tokio::test]
    async fn wait_uses_longer_supervisor_deadline() {
        // Больше AGENT_CALL_TIMEOUT (20 с) и срока RPC (15 с), но меньше срока ожидания.
        long_wait_round_trip(Duration::from_secs(22), 30).await;
    }

    #[tokio::test]
    #[ignore = "60 с реального времени; запускать вручную"]
    async fn sixty_second_wait_returns_without_hub_timeout() {
        long_wait_round_trip(Duration::from_secs(60), 70).await;
    }

    #[test]
    fn deadlines_follow_the_wait_timeout_and_nothing_else() {
        use canterlot_worker::exec::{AGENT_CALL_RPC_TIMEOUT, AGENT_CALL_TIMEOUT, deadline_for};
        use serde_json::json;

        let wait = |arguments| deadline_for("sessions.wait", &arguments);
        let at_least = |deadlines: canterlot_worker::exec::CallDeadlines, seconds: u64| {
            deadlines.rpc >= Duration::from_secs(seconds + 10)
                && deadlines.supervisor > deadlines.rpc
        };
        assert!(at_least(wait(json!({"timeout_seconds": 60})), 60));
        assert!(at_least(wait(json!({"timeout_seconds": 300})), 300));
        assert!(at_least(wait(json!({})), 120), "default is 120 s");
        assert!(at_least(wait(json!({"timeout_seconds": 1})), 1));
        // Ненормальные значения хаб отвергнет сам; срок не должен ни расти, ни падать ниже разумного.
        assert!(at_least(wait(json!({"timeout_seconds": 9999})), 300));
        assert!(at_least(wait(json!({"timeout_seconds": "abc"})), 120));
        assert!(at_least(wait(json!({"timeout_seconds": -5})), 120));
        for tool in [
            "sessions.list",
            "sessions.send",
            "presets.list",
            "sessions.waitx",
        ] {
            let short = deadline_for(tool, &json!({"timeout_seconds": 300}));
            assert_eq!(short.rpc, AGENT_CALL_RPC_TIMEOUT, "{tool}");
            assert_eq!(short.supervisor, AGENT_CALL_TIMEOUT, "{tool}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn other_tools_keep_short_deadline() {
        use canterlot_worker::adapter::AgentCallResult;
        use serde_json::json;

        let hanging = |tool: &str, arguments| {
            tokio::spawn(agent_call(
                Some(Arc::new(HangCaller) as Arc<dyn AgentCaller>),
                Uuid::now_v7(),
                1,
                Uuid::now_v7(),
                tool.into(),
                arguments,
            ))
        };
        let short = hanging("sessions.list", json!({}));
        let wait = hanging(
            "sessions.wait",
            json!({"session_id": Uuid::now_v7(), "timeout_seconds": 60}),
        );
        tokio::time::advance(Duration::from_secs(16)).await;
        let slow = short.await.unwrap();
        assert!(
            matches!(slow, AgentCallResult::Err { ref code, .. } if code == "hub_timeout"),
            "{slow:?}"
        );
        tokio::time::advance(Duration::from_secs(50)).await;
        assert!(
            !wait.is_finished(),
            "wait still has its own 60 s plus slack"
        );
        tokio::time::advance(Duration::from_secs(20)).await;
        let late = wait.await.unwrap();
        assert!(
            matches!(late, AgentCallResult::Err { ref code, .. } if code == "hub_timeout"),
            "{late:?}"
        );
    }

    #[tokio::test]
    async fn agent_call_logs_contain_no_arguments() {
        use canterlot_worker::adapter::AgentCallResult;
        use serde_json::json;
        use std::sync::{Mutex, OnceLock};

        static BUFFER: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
        #[derive(Clone)]
        struct Capture;
        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                BUFFER
                    .get_or_init(|| Mutex::new(Vec::new()))
                    .lock()
                    .unwrap()
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
            type Writer = Capture;
            fn make_writer(&'a self) -> Capture {
                Capture
            }
        }
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(Capture)
            .try_init();
        let marker = format!("PRIVATE_AGENT_ARGS_{}", Uuid::now_v7());
        let call_id = Uuid::now_v7();
        let tool = "canterlot_preset_save";
        let result = agent_call(
            Some(Arc::new(JsonCaller {
                result_json: r#"{"ok":true}"#.into(),
            }) as Arc<dyn AgentCaller>),
            Uuid::now_v7(),
            1,
            call_id,
            tool.into(),
            json!({"text": marker}),
        )
        .await;
        assert!(matches!(result, AgentCallResult::Ok(_)));
        let logs = String::from_utf8(
            BUFFER
                .get()
                .expect("capture installed")
                .lock()
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert!(!logs.contains(&marker), "аргументы в журнале: {logs}");
        assert!(logs.contains(tool), "инструмент в журнале: {logs}");
        assert!(
            logs.contains(&call_id.to_string()),
            "вызов в журнале: {logs}"
        );
    }

    #[tokio::test]
    async fn start_without_instructions_fails_with_instructions_missing() {
        use super::harness::*;
        // Старший хаб без снимка инструкций: молчаливый откат на конфигурацию
        // владельца запрещён, запуск отклоняется до выбора пресета в хабе.
        let mut env = Env::new().await;
        env.start(None).await;
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            for body in env.pump(Duration::from_millis(200)).await {
                if let canterlot_proto::worker::v1::record_body::Body::ExecutionState(state) = body
                    && state.state == "failed"
                {
                    assert_eq!(state.outcome, "invalid_instructions");
                    return;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "нет отказа запуска; supervisor: {}",
                env.logs()
            );
        }
    }
}
