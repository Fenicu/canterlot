//! `hub_dial`: воркер ходит к хабу через Unix-сокет, а TLS по-прежнему проверяет имя из `hub_url`.

use std::time::Duration;

use canterlot_worker::{
    config::{self, ConfigErrorCategory},
    session::{SessionState, Worker, WorkerError},
};
use tokio::{sync::watch, task::JoinHandle, time::timeout};
use uuid::Uuid;

use crate::support::{Behavior, Listen, Observed, ProtocolHub};

fn spawn(
    hub: &ProtocolHub,
    store: canterlot_worker::store::WorkerStore,
) -> (
    watch::Sender<bool>,
    watch::Receiver<SessionState>,
    JoinHandle<Result<(), WorkerError>>,
) {
    let worker = Worker::new(hub.config.clone(), store)
        .with_runtime_dir(hub.config.data_dir.join("runtime"));
    let state = worker.state();
    let (stop, shutdown) = watch::channel(false);
    (stop, state, tokio::spawn(worker.run(shutdown)))
}

async fn wait_for(state: &mut watch::Receiver<SessionState>, want: impl Fn(&SessionState) -> bool) {
    timeout(Duration::from_secs(8), async {
        loop {
            if want(&state.borrow_and_update()) {
                return;
            }
            state.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn worker_reaches_a_tls_hub_through_the_unix_socket_named_by_hub_dial() {
    let (mut hub, store) = ProtocolHub::start_on(Behavior::default(), Listen::Unix).await;
    // Порт в `hub_url` никто не слушает: соединение возможно только через сокет.
    assert!(
        hub.config
            .hub_dial
            .as_deref()
            .unwrap()
            .starts_with("unix:/")
    );
    let (stop, mut state, join) = spawn(&hub, store);
    wait_for(&mut state, |s| *s == SessionState::Live).await;
    let Observed::Hello(control, _) = hub.event().await else {
        panic!("expected control hello");
    };
    assert_eq!(control.machine_id, hub.config.machine_id.to_string());
    stop.send(true).unwrap();
    timeout(Duration::from_secs(3), join)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_certificate_for_another_name_is_refused_even_through_the_socket() {
    let (mut hub, store) = ProtocolHub::start_on(Behavior::default(), Listen::Unix).await;
    // Сертификат хаба выписан на `localhost`; `hub_url` называет другое имя.
    hub.config.hub_url = "https://wrong.example:1".into();
    let (stop, mut state, join) = spawn(&hub, store);
    // Две неудачные попытки подряд: Backoff, новая попытка, снова Backoff.
    wait_for(&mut state, |s| matches!(s, SessionState::Backoff { .. })).await;
    wait_for(&mut state, |s| !matches!(s, SessionState::Backoff { .. })).await;
    wait_for(&mut state, |s| matches!(s, SessionState::Backoff { .. })).await;
    assert!(
        hub.events.try_recv().is_err(),
        "the handshake must fail before any hello reaches the hub"
    );
    assert!(!matches!(*state.borrow(), SessionState::Live));
    stop.send(true).unwrap();
    timeout(Duration::from_secs(3), join)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

fn worker_toml(extra: &str) -> String {
    let id = Uuid::now_v7();
    format!(
        "hub_url = \"https://hub.example:8443\"\nca_cert = \"/ca\"\ntoken_file = \"/token\"\n\
         machine_id = \"{id}\"\ninstallation_id = \"{id}\"\nrestore_generation = \"{id}\"\n\
         data_dir = \"/tmp/data\"\n{extra}\n"
    )
}

fn load(extra: &str) -> Result<config::WorkerConfig, config::ConfigError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.toml");
    std::fs::write(&path, worker_toml(extra)).unwrap();
    config::load(&path)
}

#[test]
fn hub_dial_is_optional_and_accepts_an_absolute_unix_path() {
    assert!(load("").unwrap().hub_dial.is_none());
    let config = load("hub_dial = \"unix:/srv/runner/hub.sock\"").unwrap();
    assert_eq!(
        config.hub_dial_path().unwrap().unwrap(),
        std::path::Path::new("/srv/runner/hub.sock")
    );
}

#[test]
fn hub_dial_rejects_anything_but_an_absolute_unix_path() {
    for bad in [
        "unix:relative/hub.sock",
        "unix:",
        "tcp:127.0.0.1:1",
        "/srv/runner/hub.sock",
        "unix:/srv/../hub.sock",
    ] {
        let error = load(&format!("hub_dial = \"{bad}\"")).unwrap_err();
        assert_eq!(error.category, ConfigErrorCategory::HubDial, "{bad}");
    }
}
