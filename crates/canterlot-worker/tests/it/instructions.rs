//! Тесты задачи 1 плана 2c: инструкции агента между хабом, воркером и сопровождением.

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
    use uuid::Uuid;

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
    use std::time::Duration;

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

    /// `job_run` держит вызов `wait_seconds` плюс 15 с: ждущий хаба всегда раньше воркера
    /// заказчика, иначе итог ушёл бы в оборванный вызов.
    #[test]
    fn job_run_deadline_is_the_wait_plus_fifteen_seconds() {
        use canterlot_worker::exec::{AGENT_CALL_RPC_TIMEOUT, deadline_for};
        use serde_json::json;

        let job = |arguments| deadline_for("job.run", &arguments);
        for (arguments, seconds) in [
            (json!({"wait_seconds": 60}), 60),
            (json!({"wait_seconds": 7}), 7),
            (json!({"wait_seconds": 0}), 0),
            (json!({}), 20),
            // Ненормальное значение хаб отвергнет сам; срок не растёт выше предела.
            (json!({"wait_seconds": 9999}), 60),
            (json!({"wait_seconds": "abc"}), 20),
        ] {
            let deadlines = job(arguments.clone());
            assert_eq!(
                deadlines.rpc,
                Duration::from_secs(seconds + 15),
                "{arguments}"
            );
            assert!(deadlines.supervisor > deadlines.rpc, "{arguments}");
        }
        // Остальные инструменты задач отвечают быстро.
        for tool in ["job.status", "job.log", "job.cancel", "jobs.list"] {
            assert_eq!(
                deadline_for(tool, &json!({"wait_seconds": 60})).rpc,
                AGENT_CALL_RPC_TIMEOUT,
                "{tool}"
            );
        }
    }
}
