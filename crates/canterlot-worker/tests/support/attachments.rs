#![allow(dead_code)]

#[path = "fake_exec.rs"]
mod fake_exec;

use canterlot_hub::{
    attachments::{BlobStore, LocalBlobStore, NoTrace, PublishTrace},
    config::{AttachmentsConfig, HubTimers},
    conversations::{self, SubmitMessage},
    delivery::{ContentBlock, SubmittedInput, control},
    executions,
    fault_injection::{Faults, SjFact},
    machines,
    testkit::{self, TestHub},
};
use canterlot_proto::worker::v1::record_body::Body;
use canterlot_worker::{
    config::{ExecutionConfig, SupervisorTimers, WorkerConfig, WorkerTimers},
    fault_injection::{SessionProbe, StoreProbe},
    session::{SessionState, Worker, WorkerError},
    store::WorkerStore,
};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{sleep, timeout},
};
use uuid::Uuid;
#[path = "process_guard.rs"]
mod process_guard;
use process_guard::ProcessGuard;

pub const WAIT: Duration = Duration::from_secs(20);
pub async fn until(mut condition: impl AsyncFnMut() -> bool) {
    timeout(WAIT, async {
        while !condition().await {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition timed out");
}
pub fn sha(bytes: &[u8]) -> String {
    canterlot_hub::attachments::blobstore::hex(&Sha256::digest(bytes))
}
fn timers() -> HubTimers {
    HubTimers {
        dispatch_sweep: Duration::from_millis(20),
        ..Default::default()
    }
}
type RunningWorker = (
    watch::Sender<bool>,
    watch::Receiver<SessionState>,
    JoinHandle<Result<(), WorkerError>>,
);
pub struct Fixture {
    pub pool: PgPool,
    pub root: tempfile::TempDir,
    pub guard: ProcessGuard,
    pub hub: Option<TestHub>,
    pub config: WorkerConfig,
    pub owner: Uuid,
    pub conversation: Uuid,
    pub execution: Uuid,
    pub blobs: Arc<dyn BlobStore>,
    pub blob_config: AttachmentsConfig,
    pub probe: SessionProbe,
    pub store_probe: StoreProbe,
    pub worker: Option<RunningWorker>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some((stop, _, task)) = self.worker.take() {
            let _ = stop.send(true);
            task.abort();
        }
        if let Some(hub) = self.hub.take() {
            let _ = hub.shutdown.send(());
        }
        self.guard.cleanup();
    }
}
impl Fixture {
    pub async fn new(pool: &PgPool) -> Self {
        Self::new_with_gate(pool, false).await
    }
    pub async fn new_with_gate(pool: &PgPool, hold_gate: bool) -> Self {
        Self::build(pool, hold_gate, Arc::new(NoTrace)).await
    }
    /// Хранилище хаба с наблюдателем шагов публикации.
    pub async fn with_trace(pool: &PgPool, trace: Arc<dyn PublishTrace>) -> Self {
        Self::build(pool, false, trace).await
    }
    async fn build(pool: &PgPool, hold_gate: bool, trace: Arc<dyn PublishTrace>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let guard = ProcessGuard::new(root.path());
        let blob_config = AttachmentsConfig {
            blob_dir: root.path().join("hub-files"),
            ..Default::default()
        };
        let blobs: Arc<dyn BlobStore> =
            Arc::new(LocalBlobStore::open(&blob_config, trace).unwrap());
        let hub = testkit::spawn_test_hub_with_attachment_store(
            pool.clone(),
            timers(),
            None,
            testkit::tls::test_pki(),
            Faults::default(),
            (blob_config.clone(), blobs.clone()),
        )
        .await;
        let owner = hub.installation.owner_id;
        let machine = machines::add(pool, owner, "attachment-test").await.unwrap();
        let token_file = root.path().join("token");
        fs::write(&token_file, machine.token).unwrap();
        fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
        let ca_cert = root.path().join("ca");
        fs::write(&ca_cert, &hub.pki.ca_pem).unwrap();
        let work = root.path().join("work");
        fs::create_dir(&work).unwrap();
        let executable = if hold_gate {
            let wrapper = root.path().join("supervisor");
            fake_exec::write_executable(
                &wrapper,
                format!(
                    "#!/bin/sh\nexport CANTERLOT_FAULT_HOLD_AT=before_gate\nexec '{}' \"$@\"\n",
                    env!("CARGO_BIN_EXE_canterlot-worker")
                ),
                0o700,
            );
            wrapper
        } else {
            env!("CARGO_BIN_EXE_canterlot-worker").into()
        };
        let config = WorkerConfig {
            hub_url: format!("https://localhost:{}", hub.addr.port()),
            ca_cert,
            token_file,
            machine_id: machine.machine_id,
            installation_id: hub.installation.installation_id,
            restore_generation: hub.installation.restore_generation,
            data_dir: root.path().join("store"),
            machine: Default::default(),
            attachments: canterlot_worker::config::WorkerAttachmentsConfig {
                fetch_attempts: 2,
                fetch_retry: Duration::from_millis(10),
                ..Default::default()
            },
            claude_code: None,
            codex: None,
            timers: WorkerTimers {
                heartbeat: Duration::from_millis(100),
                heartbeat_ack: Duration::from_secs(2),
                range_ack: Duration::from_secs(2),
                handshake: Duration::from_secs(5),
                backoff_max: Duration::from_millis(500),
                ..Default::default()
            },
            execution: ExecutionConfig {
                allowed_workdir_roots: vec![work.clone()],
                supervisor_exe: Some(executable),
                supervisor: SupervisorTimers {
                    stop_grace: Duration::from_millis(300),
                    stop_retry: Duration::from_millis(200),
                    ..Default::default()
                },
                ..Default::default()
            },
            opencode: None,
        };
        let conversation = testkit::seed_conversation(pool, owner, machine.machine_id).await;
        sqlx::query("update conversations set workdir=$2,harness='synthetic' where id=$1")
            .bind(conversation)
            .bind(work.to_str().unwrap())
            .execute(pool)
            .await
            .unwrap();
        let mut f = Self {
            pool: pool.clone(),
            root,
            guard,
            hub: Some(hub),
            config,
            owner,
            conversation,
            execution: Uuid::nil(),
            blobs,
            blob_config,
            probe: Default::default(),
            store_probe: Default::default(),
            worker: None,
        };
        f.start_worker().await;
        f.execution = executions::start(pool, owner, conversation, Uuid::now_v7())
            .await
            .unwrap()
            .execution_id;
        until(async || {
            sqlx::query_scalar::<_, bool>("select state='running' from executions where id=$1")
                .bind(f.execution)
                .fetch_one(pool)
                .await
                .unwrap()
        })
        .await;
        f
    }
    pub fn hub(&self) -> &TestHub {
        self.hub.as_ref().unwrap()
    }
    pub fn exec_dir(&self) -> PathBuf {
        self.config
            .data_dir
            .join("executions")
            .join(self.execution.to_string())
    }
    pub fn temp(&self, id: Uuid) -> PathBuf {
        self.exec_dir()
            .join("attachments/.tmp")
            .join(format!("{id}.part"))
    }
    pub fn published(&self, id: Uuid) -> PathBuf {
        self.exec_dir()
            .join("attachments")
            .join(canterlot_worker::attachments::safe_file_name(
                id,
                "../fixture.bin",
            ))
    }
    pub async fn start_worker(&mut self) {
        let store = WorkerStore::open(&self.config.data_dir)
            .unwrap()
            .with_probe(self.store_probe.clone());
        let worker = Worker::new(self.config.clone(), store).with_session_probe(self.probe.clone());
        let state = worker.state();
        let (stop, rx) = watch::channel(false);
        self.worker = Some((stop, state, tokio::spawn(worker.run(rx))));
        until(async || {
            *self.worker.as_ref().unwrap().1.borrow() == SessionState::Live
                && self.hub().registry.is_reconciled(self.config.machine_id)
        })
        .await;
    }
    pub async fn stop_worker(&mut self) {
        let (stop, _, task) = self.worker.take().unwrap();
        stop.send(true).unwrap();
        timeout(WAIT, task).await.unwrap().unwrap().unwrap();
    }
    pub async fn ready(&self, bytes: &[u8]) -> Uuid {
        let id = Uuid::now_v7();
        let size = bytes.len() as u64;
        // `sqlx::test` держит хаб и воркер на одном потоке: хеш на нём останавливает heartbeat.
        let owned = bytes.to_vec();
        let sha256 = tokio::task::spawn_blocking(move || sha(&owned))
            .await
            .unwrap();
        let chunks: Vec<_> = bytes
            .chunks(131_072)
            .map(|b| Ok(b.to_vec().into()))
            .collect();
        self.blobs
            .append_staging(id, 0, Box::pin(tokio_stream::iter(chunks)), size)
            .await
            .unwrap();
        self.blobs.publish(id, size, &sha256).await.unwrap();
        sqlx::query("insert into attachments(id,owner_id,conversation_id,name,size_bytes,sha256,state,ready_at) values($1,$2,$3,'../fixture.bin',$4,$5,'ready',clock_timestamp())").bind(id).bind(self.owner).bind(self.conversation).bind(size as i64).bind(sha256).execute(&self.pool).await.unwrap();
        id
    }
    pub async fn submit(&self, ids: &[Uuid], text: &str) -> SubmittedInput {
        let input = conversations::submit_message(
            &self.pool,
            SubmitMessage {
                owner_id: self.owner,
                conversation_id: self.conversation,
                key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text { text: text.into() }],
                attachment_ids: ids.to_vec(),
                retry_of: None,
                received_at: chrono::Utc::now(),
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        self.hub().dispatcher.trigger(self.conversation);
        input
    }
    pub async fn input_state(&self, id: Uuid) -> String {
        sqlx::query_scalar("select state from inputs where id=$1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
    pub fn transmissions(&self, id: Uuid) -> usize {
        self.hub().faults.batches().iter().flat_map(|b|&b.records).filter(|r|matches!(&r.sj_fact,Some(SjFact::TransmissionStarted(c,_)) if *c==id.to_string())).map(|r|r.seq).collect::<std::collections::HashSet<_>>().len()
    }
    pub async fn accepted(&self, input: &SubmittedInput) {
        until(async || self.input_state(input.input_id).await == "accepted").await;
        assert_eq!(self.transmissions(input.command_id), 1);
    }
    pub async fn cj(&self) -> Vec<Body> {
        // Снимок живого журнала: повтор при компакции.
        let directory = self.config.data_dir.join("cj");
        let records = timeout(WAIT, async {
            loop {
                let read = canterlot_journal::Journal::open_read_only(&directory)
                    .and_then(|journal| journal.read_from(journal.first_seq(), usize::MAX));
                match read {
                    Ok(records) => break records,
                    Err(canterlot_journal::JournalError::Io(error))
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::UnexpectedEof
                        ) =>
                    {
                        tokio::task::yield_now().await;
                    }
                    Err(_) => panic!("CJ snapshot failed"),
                }
            }
        })
        .await
        .unwrap();
        use prost::Message;
        records
            .into_iter()
            .filter_map(|r| {
                canterlot_proto::worker::v1::RecordBody::decode(r.payload.as_slice())
                    .unwrap()
                    .body
            })
            .collect()
    }
    pub async fn stop_execution(&self, force: bool) {
        control::request_stop_execution(
            &self.pool,
            self.owner,
            self.conversation,
            force,
            &Uuid::now_v7().to_string(),
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        self.hub().dispatcher.trigger(self.conversation);
    }
}

impl Fixture {
    fn sj_snapshot(&self) -> Option<(tempfile::TempDir, canterlot_journal::Journal)> {
        for _ in 0..100 {
            // Открытый inode переживает compaction; временные файлы ACK не входят в снимок.
            let entries = match fs::read_dir(self.exec_dir().join("sj")) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
                Err(error) => panic!("SJ snapshot failed: {}", error.kind()),
            };
            let sources: std::io::Result<Vec<_>> = entries
                .filter_map(|entry| match entry {
                    Ok(entry) => {
                        let name = entry.file_name();
                        let name_str = name.to_string_lossy();
                        if matches!(name_str.as_ref(), "header" | "ack" | "sealed")
                            || (name_str.starts_with("seg-") && name_str.ends_with(".log"))
                        {
                            Some(fs::File::open(entry.path()).map(|file| (name, file)))
                        } else {
                            None
                        }
                    }
                    Err(error) => Some(Err(error)),
                })
                .collect();
            let sources = match sources {
                Ok(sources) => sources,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => panic!("SJ snapshot open failed: {}", error.kind()),
            };
            let copy = tempfile::tempdir().unwrap();
            for (name, mut source) in sources {
                let mut target = fs::File::create(copy.path().join(name)).unwrap();
                std::io::copy(&mut source, &mut target).unwrap();
            }
            if let Ok((journal, report)) =
                canterlot_journal::Journal::open(copy.path(), Default::default())
                && report.corrupted_tail.is_none()
                && journal.is_sealed().is_none()
            {
                return Some((copy, journal));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("could not obtain a complete SJ snapshot");
    }

    pub fn sj_transmissions(&self, command: Uuid) -> Vec<u64> {
        use prost::Message;
        let mut seqs = std::collections::BTreeSet::new();
        if let Some((_, j)) = self.sj_snapshot() {
            for r in j.read_from(j.first_seq(), usize::MAX).unwrap() {
                if matches!(canterlot_proto::worker::v1::RecordBody::decode(r.payload.as_slice()).unwrap().body,Some(Body::TransmissionStarted(ref t)) if t.command_id==command.to_string())
                {
                    seqs.insert(r.seq);
                }
            }
        }
        if let Some(hub) = &self.hub {
            for r in hub.faults.batches().iter().flat_map(|b| &b.records) {
                if matches!(&r.sj_fact,Some(SjFact::TransmissionStarted(c,_)) if *c==command.to_string())
                {
                    seqs.insert(r.seq);
                }
            }
        }
        seqs.into_iter().collect()
    }
}

pub fn process_guard_for(path: &std::path::Path) -> impl Drop {
    ProcessGuard::new(path)
}
