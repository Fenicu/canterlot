#![forbid(unsafe_code)]
#![cfg(all(feature = "synthetic-adapter", feature = "fault-injection"))]

#[path = "support/attachments.rs"]
mod fixture;
#[allow(dead_code)]
mod support;
#[path = "support/trace.rs"]
mod trace;
use canterlot_hub::{delivery::control, fault_injection::Point, testkit};
use canterlot_proto::worker::v1::{StreamKind, record_body::Body};
use fixture::{Fixture, WAIT, until};
use sqlx::PgPool;
use std::{
    fs,
    time::{Duration, Instant},
};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn input_is_not_transmitted_until_all_attachments_published(pool: PgPool) {
    transmission_waits_for_attachments(pool).await;
}

// sqlx называет базу теста по его пути и перед созданием удаляет одноимённую:
// дочерний процесс под strace с тем же именем теста сносил базу родителя.
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
#[ignore = "запускается под strace из execution_directory_parents_are_synced_before_ownership_and_readiness"]
async fn transmission_waits_for_attachments_under_strace(pool: PgPool) {
    transmission_waits_for_attachments(pool).await;
}

async fn transmission_waits_for_attachments(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let first = f.ready(&vec![3; 1024 * 1024]).await;
    let second = f.ready(&vec![7; 1024 * 1024]).await;
    let pause = f.hub().faults.pause_next(Point::BeforeFetchChunk);
    let input = f.submit(&[first, second], "hello").await;
    let release = timeout(WAIT, pause).await.unwrap().unwrap();
    until(async || f.input_state(input.input_id).await == "on_machine").await;
    assert_eq!(f.transmissions(input.command_id), 0);
    assert!(f.sj_transmissions(input.command_id).is_empty());
    assert!(!f.published(first).exists() || !f.published(second).exists());
    release.send(()).unwrap();
    f.accepted(&input).await;
    assert!(f.published(first).exists() && f.published(second).exists());
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn hub_restart_during_fetch_resumes_after_reconnect_without_rejecting(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    let bytes = vec![11; 4 * 1024 * 1024];
    let id = f.ready(&bytes).await;
    let pause = f.hub().faults.pause_next(Point::BeforeFetchChunk);
    let second = f.hub().faults.pause_next(Point::BeforeFetchChunk);
    let input = f.submit(&[id], "hello").await;
    timeout(WAIT, pause)
        .await
        .unwrap()
        .unwrap()
        .send(())
        .unwrap();
    let _release = timeout(WAIT, second).await.unwrap().unwrap();
    until(async || fs::metadata(f.temp(id)).is_ok_and(|m| m.len() > 0)).await;
    let offset = fs::metadata(f.temp(id)).unwrap().len();
    assert!(offset < bytes.len() as u64);
    until(async || f.input_state(input.input_id).await == "on_machine").await;
    let hub = f.hub.take().unwrap();
    let addr = hub.addr;
    let pki = hub.pki.clone();
    let faults = hub.faults.clone();
    hub.shutdown.send(()).unwrap();
    timeout(WAIT, hub.join).await.unwrap().unwrap().unwrap();
    let failed = async |f: &Fixture| {
        f.cj()
            .await
            .into_iter()
            .filter_map(|b| {
                if let Body::AttachmentFailed(v) = b {
                    Some(v)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    };
    until(async || !failed(&f).await.is_empty()).await;
    // Простой хаба после отказа: ввод не отвергается и вложение не терзается повторами.
    sleep(Duration::from_secs(3)).await;
    assert_eq!(f.input_state(input.input_id).await, "on_machine");
    let failures = failed(&f).await;
    assert_eq!(failures.len(), 1);
    assert!(failures[0].retryable);
    assert_eq!(failures[0].reason, "hub_unreachable");
    assert!(
        !f.cj()
            .await
            .iter()
            .any(|b| matches!(b, Body::CommandRejected(_)))
    );
    assert_eq!(fs::metadata(f.temp(id)).unwrap().len(), offset);
    f.hub = Some(
        testkit::spawn_test_hub_with_attachment_store(
            pool.clone(),
            Default::default(),
            Some(addr),
            pki,
            faults,
            (f.blob_config.clone(), f.blobs.clone()),
        )
        .await,
    );
    f.accepted(&input).await;
    assert_eq!(fs::read(f.published(id)).unwrap(), bytes);
    assert_eq!(f.sj_transmissions(input.command_id).len(), 1);
    // CJ после подтверждения сжимается: считаем записи, дошедшие до хаба, по (журнал, seq).
    let shipped = |kind: Body| {
        let kind = std::mem::discriminant(&kind);
        f.hub()
            .faults
            .batches()
            .iter()
            .flat_map(|b| b.records.iter().map(move |r| (b.journal_id.clone(), r)))
            .filter(|(_, r)| r.kind == Some(kind))
            .map(|(journal, r)| (journal, r.seq))
            .collect::<std::collections::HashSet<_>>()
            .len()
    };
    until(async || shipped(Body::AttachmentReady(Default::default())) == 1).await;
    assert_eq!(shipped(Body::AttachmentFailed(Default::default())), 1);
    assert_eq!(shipped(Body::CommandRejected(Default::default())), 0);
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn stop_execution_is_handled_while_a_fetch_is_in_progress(pool: PgPool) {
    for force in [false, true] {
        let f = Fixture::new(&pool).await;
        let id = f.ready(&vec![1; 1024 * 1024]).await;
        let pause = f.hub().faults.pause_next(Point::BeforeFetchChunk);
        let input = f.submit(&[id], "hello").await;
        let _release = timeout(WAIT, pause).await.unwrap().unwrap();
        let at = Instant::now();
        f.stop_execution(force).await;
        timeout(
            Duration::from_secs(2),
            until(async || {
                sqlx::query_scalar::<_, bool>("select state='stopped' from executions where id=$1")
                    .bind(f.execution)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }),
        )
        .await
        .unwrap();
        assert!(at.elapsed() < Duration::from_secs(2));
        assert_eq!(f.transmissions(input.command_id), 0);
        assert!(!f.temp(id).exists());
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn cancel_while_waiting_for_attachments_wins_and_nothing_is_transmitted(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let id = f.ready(&vec![1; 1024 * 1024]).await;
    let pause = f.hub().faults.pause_next(Point::BeforeFetchChunk);
    let input = f.submit(&[id], "hello").await;
    let _release = timeout(WAIT, pause).await.unwrap().unwrap();
    control::cancel_input(
        &pool,
        f.owner,
        input.input_id,
        &Uuid::now_v7().to_string(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    f.hub().dispatcher.trigger(f.conversation);
    until(async || f.input_state(input.input_id).await == "cancelled").await;
    assert_eq!(f.transmissions(input.command_id), 0);
    assert!(!f.temp(id).exists());
    assert!(!f.published(id).exists());
    assert!(
        !f.cj()
            .await
            .iter()
            .any(|b| matches!(b, Body::CommandRejected(_)))
    );
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn redelivered_attempt_does_not_refetch_published_files(pool: PgPool) {
    let mut steps = trace::Steps::new();
    let mut f = Fixture::new_with_gate(&pool, true).await;
    let _dump = SupervisorLogOnPanic(f.exec_dir());
    steps.step("fixture running");
    let id = f.ready(b"published").await;
    let input = f.submit(&[id], "hello").await;
    steps.step("submitted");
    until(async || f.published(id).exists()).await;
    steps.step("published");
    sleep(Duration::from_millis(200)).await;
    assert!(f.sj_transmissions(input.command_id).is_empty());
    use std::os::unix::fs::MetadataExt;
    let inode = fs::metadata(f.published(id)).unwrap().ino();
    f.stop_worker().await;
    steps.step("worker stopped");
    let no_fetch = f.hub().faults.pause_next(Point::BeforeFetchChunk);
    f.start_worker().await;
    steps.step("worker live again");
    sleep(Duration::from_millis(300)).await;
    assert!(timeout(Duration::from_millis(100), no_fetch).await.is_err());
    assert_eq!(fs::metadata(f.published(id)).unwrap().ino(), inode);
    steps.step("no refetch");
    // Хаб видит каждую запись CJ до подтверждения; снимки хранилища делаются только при
    // приёме команды и подтверждении, и запись, сжатая между ними, в них не попадает.
    until(async || shipped_ready(&f).len() >= 2).await;
    steps.step("second AttachmentReady");
    fs::write(f.exec_dir().join("fault_release"), b"1").unwrap();
    f.accepted(&input).await;
    steps.step("accepted");
    assert_eq!(f.sj_transmissions(input.command_id).len(), 1);
}

/// Записи `AttachmentReady` из CJ, дошедшие до хаба: (журнал, seq).
fn shipped_ready(f: &Fixture) -> std::collections::BTreeSet<(String, u64)> {
    let ready = std::mem::discriminant(&Body::AttachmentReady(Default::default()));
    f.hub()
        .faults
        .batches()
        .iter()
        .flat_map(|b| b.records.iter().map(move |r| (b.journal_id.clone(), r)))
        .filter(|(_, r)| r.kind == Some(ready))
        .map(|(journal, r)| (journal, r.seq))
        .collect()
}

/// Журнал сопровождения при падении теста: оно живёт отдельным процессом.
struct SupervisorLogOnPanic(std::path::PathBuf);

impl Drop for SupervisorLogOnPanic {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "supervisor log:\n{}",
                fs::read_to_string(self.0.join("supervisor.log")).unwrap_or_default()
            );
        }
    }
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn large_attachment_does_not_starve_control_session(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let id = f.ready(&vec![37; 48 * 1024 * 1024]).await;
    let first = f.hub().faults.pause_next(Point::BeforeFetchChunk);
    let second = f.hub().faults.pause_next(Point::BeforeFetchChunk);
    let input = f.submit(&[id], "hello").await;
    timeout(WAIT, first)
        .await
        .unwrap()
        .unwrap()
        .send(())
        .unwrap();
    let release = timeout(WAIT, second).await.unwrap().unwrap();
    until(async || fs::metadata(f.temp(id)).is_ok_and(|m| m.len() > 0)).await;
    let before = f.probe.heartbeat_acks().len();
    sleep(Duration::from_millis(350)).await;
    assert!(f.probe.heartbeat_acks().len() > before);
    let at = Instant::now();
    let interrupt = Uuid::now_v7();
    sqlx::query("insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state) select $1,c.machine_id,c.id,e.id,e.generation,'interrupt',jsonb_build_object('execution_id',e.id,'execution_generation',e.generation),'pending' from conversations c join executions e on e.conversation_id=c.id where e.id=$2").bind(interrupt).bind(f.execution).execute(&pool).await.unwrap();
    f.hub().dispatcher.trigger(f.conversation);
    timeout(Duration::from_secs(5),until(async ||sqlx::query_scalar::<_,bool>("select delivery_state in ('stored_on_machine','completed') from commands where id=$1").bind(interrupt).fetch_one(&pool).await.unwrap())).await.unwrap();
    assert!(at.elapsed() < Duration::from_secs(5));
    release.send(()).unwrap();
    f.accepted(&input).await;
    assert_eq!(
        fs::metadata(f.published(id)).unwrap().len(),
        48 * 1024 * 1024
    );
    assert!(
        f.probe
            .heartbeat_acks()
            .iter()
            .all(|d| *d < f.config.timers.heartbeat_ack)
    );
    assert!(
        f.probe
            .heartbeat_gaps()
            .iter()
            .all(|d| *d < f.config.timers.heartbeat_ack / 2)
    );
    assert_eq!(
        f.hub()
            .faults
            .hellos()
            .iter()
            .filter(|h| h.stream_kind == StreamKind::Control as i32)
            .count(),
        1
    );
    assert_eq!(f.transmissions(input.command_id), 1);
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn agent_reads_the_published_file_checksum(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let bytes = b"agent verifies attachment";
    let id = f.ready(bytes).await;
    let input = f
        .submit(&[id], r#"{"synthetic":[{"read_attachment":0}]}"#)
        .await;
    f.accepted(&input).await;
    until(async ||sqlx::query_scalar::<_,bool>("select exists(select 1 from blocks where execution_id=$1 and state='final' and content=$2)").bind(f.execution).bind(format!("sha256:{}",fixture::sha(bytes))).fetch_one(&pool).await.unwrap()).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn attachment_dir_is_under_execution_dir_not_workdir(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let id = f.ready(b"private").await;
    let input = f.submit(&[id], "hello").await;
    f.accepted(&input).await;
    assert!(
        f.published(id)
            .starts_with(f.exec_dir().join("attachments"))
    );
    assert_eq!(fs::read_dir(f.root.path().join("work")).unwrap().count(), 0);
    f.hub().faults.drop_next_sj_acks(100);
    f.stop_execution(false).await;
    until(async || {
        sqlx::query_scalar::<_, bool>("select state='stopped' from executions where id=$1")
            .bind(f.execution)
            .fetch_one(&pool)
            .await
            .unwrap()
    })
    .await;
    assert!(f.published(id).exists());
    f.hub().faults.drop_next_sj_acks(0);
    until(async || !f.exec_dir().join("attachments").exists()).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn attachment_failure_is_reported_and_input_rejected_without_transmission(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let id = f.ready(&vec![1; 1024 * 1024]).await;
    let pause = f.hub().faults.pause_next(Point::BeforeFetchChunk);
    let input = f.submit(&[id], "hello").await;
    let release = timeout(WAIT, pause).await.unwrap().unwrap();
    sqlx::query("update attachments set state='released' where id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let _release = release;
    // Следующий fetch получает NOT_FOUND.
    f.hub().faults.fail_control(tonic::Code::Unavailable);
    until(async || f.input_state(input.input_id).await == "rejected").await;
    assert_eq!(f.transmissions(input.command_id), 0);
    assert!(!f.temp(id).exists());
    let state: (String, Option<bool>) =
        sqlx::query_as("select machine_state,retryable from input_attachments where input_id=$1")
            .bind(input.input_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, ("failed".into(), Some(false)));
}

#[tokio::test]
async fn attachment_records_are_not_written_for_a_session_without_pilot_v1() {
    use canterlot_proto::worker::v1::*;
    use canterlot_worker::{session::Worker, store::WorkerStore};
    let (mut hub, _) = support::ProtocolHub::start(support::Behavior {
        capabilities: vec!["exec_v1".into()],
        ..Default::default()
    })
    .await;
    let root = tempfile::tempdir().unwrap();
    let _guard = fixture::process_guard_for(root.path());
    let mut config = hub.config.clone();
    config.data_dir = root.path().join("store");
    config.execution.allowed_workdir_roots = vec![root.path().to_path_buf()];
    config.execution.supervisor_exe = Some(env!("CARGO_BIN_EXE_canterlot-worker").into());
    let worker = Worker::new(config.clone(), WorkerStore::open(&config.data_dir).unwrap())
        .with_runtime_dir(config.data_dir.join("runtime"));
    let mut state = worker.state();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(worker.run(rx));
    timeout(WAIT, async {
        while *state.borrow_and_update() != canterlot_worker::session::SessionState::Live {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let execution = Uuid::now_v7().to_string();
    let command = Command {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 1,
        execution_id: execution.clone(),
        execution_generation: 1,
        kind: Some(command::Kind::StartExecution(StartExecution {
            conversation_id: Uuid::now_v7().to_string(),
            harness: "synthetic".into(),
            workdir: root.path().to_str().unwrap().into(),
            instructions: Some(Instructions {
                mode: "clean".into(),
                ..Default::default()
            }),
            ..Default::default()
        })),
    };
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(command)),
    })
    .await;
    timeout(WAIT,async{loop {if let support::Observed::Data(DataUp{msg:Some(data_up::Msg::SjRecords(batch))})=hub.event().await && batch.records.iter().any(|r|matches!(r.body.as_ref().and_then(|b|b.body.as_ref()),Some(Body::ExecutionState(s)) if s.state=="running")){break;}}}).await.unwrap();
    let id = Uuid::now_v7().to_string();
    hub.send(ControlDown {
        msg: Some(control_down::Msg::Command(Command {
            command_id: id.clone(),
            dispatch_attempt: 1,
            execution_id: execution,
            execution_generation: 1,
            kind: Some(command::Kind::DeliverInput(DeliverInput {
                content: vec![ContentBlock {
                    block: Some(content_block::Block::Attachment(AttachmentRef {
                        attachment_id: Uuid::now_v7().to_string(),
                        size: 3,
                        sha256: fixture::sha(b"abc"),
                        ..Default::default()
                    })),
                }],
                ..Default::default()
            })),
        })),
    })
    .await;
    let mut bodies = Vec::new();
    timeout(WAIT, async {
        loop {
            if let support::Observed::Control(ControlUp {
                msg: Some(control_up::Msg::CjRecords(batch)),
            }) = hub.event().await
            {
                bodies.extend(
                    batch
                        .records
                        .into_iter()
                        .filter_map(|r| r.body.and_then(|b| b.body)),
                );
                if bodies
                    .iter()
                    .any(|b| matches!(b,Body::CommandRejected(r) if r.command_id==id))
                {
                    break;
                }
            }
        }
    })
    .await
    .unwrap();
    assert!(
        !bodies
            .iter()
            .any(|b| matches!(b, Body::AttachmentReady(_) | Body::AttachmentFailed(_)))
    );
    stop.send(true).unwrap();
    task.await.unwrap().unwrap();
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn withdraw_while_fetching_removes_temp_and_never_transmits(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let id = f.ready(&vec![1; 1024 * 1024]).await;
    let first = f.hub().faults.pause_next(Point::BeforeFetchChunk);
    let second = f.hub().faults.pause_next(Point::BeforeFetchChunk);
    let input = f.submit(&[id], "hello").await;
    timeout(WAIT, first)
        .await
        .unwrap()
        .unwrap()
        .send(())
        .unwrap();
    let _release = timeout(WAIT, second).await.unwrap().unwrap();
    until(async || {
        fs::metadata(f.temp(id)).is_ok_and(|m| m.len() > 0)
            && f.input_state(input.input_id).await == "on_machine"
    })
    .await;
    let control::StopOutcome::Requested {
        withdraw_command_id,
        ..
    } = control::request_stop(
        &pool,
        f.owner,
        f.conversation,
        &Uuid::now_v7().to_string(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(withdraw_command_id.is_some());
    until(async || f.input_state(input.input_id).await == "queued").await;
    assert!(!f.temp(id).exists());
    assert!(!f.published(id).exists());
    assert!(f.sj_transmissions(input.command_id).is_empty());
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn worker_restart_mid_fetch_preserves_temp_and_resumes(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    let bytes = vec![91; 4 * 1024 * 1024];
    let id = f.ready(&bytes).await;
    let first = f.hub().faults.pause_next(Point::BeforeFetchChunk);
    let second = f.hub().faults.pause_next(Point::BeforeFetchChunk);
    let input = f.submit(&[id], "hello").await;
    timeout(WAIT, first)
        .await
        .unwrap()
        .unwrap()
        .send(())
        .unwrap();
    let _release = timeout(WAIT, second).await.unwrap().unwrap();
    until(async || {
        fs::metadata(f.temp(id)).is_ok_and(|m| m.len() > 0)
            && f.input_state(input.input_id).await == "on_machine"
    })
    .await;
    let offset = fs::metadata(f.temp(id)).unwrap().len();
    f.stop_worker().await;
    assert_eq!(fs::metadata(f.temp(id)).unwrap().len(), offset);
    assert!(!f.published(id).exists());
    f.start_worker().await;
    f.accepted(&input).await;
    assert_eq!(fs::read(f.published(id)).unwrap(), bytes);
    assert_eq!(f.sj_transmissions(input.command_id).len(), 1);
}

#[test]
fn execution_directory_parents_are_synced_before_ownership_and_readiness() {
    let root = tempfile::tempdir().unwrap();
    let trace = root.path().join("syscalls");
    let status = std::process::Command::new("strace")
        .args([
            "-f",
            "-qq",
            "-yy",
            "-e",
            "trace=fsync,fdatasync,mkdir,rename",
            "-o",
        ])
        .arg(&trace)
        .arg(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "transmission_waits_for_attachments_under_strace",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let calls = fs::read_to_string(trace).unwrap();
    let calls: Vec<_> = calls.lines().collect();
    let execution_mkdir = calls
        .iter()
        .position(|line| line.contains("mkdir(") && line.contains("/store/executions/"))
        .unwrap();
    let line = calls[execution_mkdir];
    let path = line.split('"').nth(1).unwrap();
    let execution = std::path::Path::new(path);
    let executions = execution.parent().unwrap();
    let data = executions.parent().unwrap();
    let synced = |path: &std::path::Path, after: usize| {
        calls
            .iter()
            .enumerate()
            .skip(after)
            .find(|(_, line)| {
                line.contains("fsync(") && line.contains(&format!("<{}>", path.display()))
            })
            .map(|(i, _)| i)
            .expect("execution ancestor must be synced")
    };
    let executions_mkdir = calls
        .iter()
        .position(|line| {
            line.contains("mkdir(") && line.contains(&format!("\"{}\"", executions.display()))
        })
        .unwrap();
    let data_sync = synced(data, executions_mkdir);
    let parent_sync = synced(executions, execution_mkdir);
    let ownership = calls
        .iter()
        .enumerate()
        .skip(execution_mkdir)
        .find(|(_, line)| {
            (line.contains("fsync(") || line.contains("fdatasync("))
                && line.contains("/store/ownership/")
        })
        .map(|(i, _)| i)
        .unwrap();
    let attachment_sync = synced(&execution.join("attachments"), execution_mkdir);
    assert!(executions_mkdir < data_sync && data_sync < execution_mkdir);
    assert!(
        execution_mkdir < parent_sync && parent_sync < ownership && ownership < attachment_sync
    );
}
