//! Смертельный выход воркера оставляет `<data_dir>/fatal` с одним словом причины;
//! обычный запуск файл убирает. Носитель раннера читает слово, чтобы выбрать между
//! запросом токена и правкой конфига, а не перезапускать вслепую.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use canterlot_worker::{
    config::WorkerConfig,
    session::{SessionState, Worker},
};
use tokio::{process::Command, sync::watch, time::timeout};
use uuid::Uuid;

use crate::support::{Behavior, Observed, ProtocolHub};

const WORKER: &str = env!("CARGO_BIN_EXE_canterlot-worker");

fn toml_of(config: &WorkerConfig) -> String {
    format!(
        "hub_url = \"{}\"\nca_cert = \"{}\"\ntoken_file = \"{}\"\nmachine_id = \"{}\"\n\
         installation_id = \"{}\"\nrestore_generation = \"{}\"\ndata_dir = \"{}\"\n",
        config.hub_url,
        config.ca_cert.display(),
        config.token_file.display(),
        config.machine_id,
        config.installation_id,
        config.restore_generation,
        config.data_dir.display(),
    )
}

fn spawn_worker(config_path: &Path) -> tokio::process::Child {
    Command::new(WORKER)
        .args(["run", "--config"])
        .arg(config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

async fn exit_of(mut child: tokio::process::Child) -> std::process::ExitStatus {
    timeout(Duration::from_secs(15), child.wait())
        .await
        .expect("the worker must exit on a fatal error")
        .unwrap()
}

fn fatal_word(data_dir: &Path) -> String {
    fs::read_to_string(data_dir.join("fatal")).expect("fatal file")
}

#[tokio::test]
async fn unauthenticated_exit_leaves_the_fatal_word() {
    let (hub, store) = ProtocolHub::start(Behavior {
        status: Some((tonic::Code::Unauthenticated, "credential_revoked")),
        ..Default::default()
    })
    .await;
    drop(store);
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("worker.toml");
    fs::write(&config, toml_of(&hub.config)).unwrap();
    let status = exit_of(spawn_worker(&config)).await;
    assert!(!status.success());
    assert_eq!(fatal_word(&hub.config.data_dir), "unauthenticated");
}

#[tokio::test]
async fn changed_restore_generation_exit_leaves_its_word() {
    let (hub, store) = ProtocolHub::start(Behavior::default()).await;
    // Первый запуск записывает поколение в хранилище, хаб не нужен: оно инициализируется до связи.
    let mut first = hub.config.clone();
    first.hub_url = "https://localhost:1".into();
    let worker = Worker::new(first, store).with_runtime_dir(hub.config.data_dir.join("runtime"));
    let mut state = worker.state();
    let (stop, shutdown) = watch::channel(false);
    let join = tokio::spawn(worker.run(shutdown));
    timeout(Duration::from_secs(8), async {
        while !matches!(*state.borrow_and_update(), SessionState::Backoff { .. }) {
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    join.await.unwrap().unwrap();

    let mut config = hub.config.clone();
    config.restore_generation = Uuid::now_v7();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.toml");
    fs::write(&path, toml_of(&config)).unwrap();
    let status = exit_of(spawn_worker(&path)).await;
    assert!(!status.success());
    assert_eq!(fatal_word(&config.data_dir), "restore_generation");
}

#[tokio::test]
async fn invalid_config_exit_leaves_its_word_in_the_data_dir_it_names() {
    let (hub, store) = ProtocolHub::start(Behavior::default()).await;
    drop(store);
    let mut config = hub.config.clone();
    config.hub_url = "http://hub.example".into();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.toml");
    fs::write(&path, toml_of(&config)).unwrap();
    let status = exit_of(spawn_worker(&path)).await;
    assert!(!status.success());
    assert_eq!(fatal_word(&config.data_dir), "config");
}

#[tokio::test]
async fn a_normal_start_removes_the_fatal_file() {
    let (mut hub, store) = ProtocolHub::start(Behavior::default()).await;
    drop(store);
    let data_dir: PathBuf = hub.config.data_dir.clone();
    fs::write(data_dir.join("fatal"), "unauthenticated").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.toml");
    fs::write(&path, toml_of(&hub.config)).unwrap();
    let mut child = spawn_worker(&path);
    let Observed::Hello(..) = hub.event().await else {
        panic!("expected the hello of the restarted worker");
    };
    assert!(
        !data_dir.join("fatal").exists(),
        "a worker that started normally must clear the stale fatal file"
    );
    child.kill().await.unwrap();
}
