//! `canterlot join` против заглушки `WorkerEnroll` с УЦ из теста. Заглушка запоминает,
//! дошёл ли до неё `Join`: при чужом УЦ или чужом имени код не должен уйти.

mod support;

use std::os::unix::fs::PermissionsExt;

use canterlot_ctl::{
    cli::{JoinArgs, Version},
    error::CtlError,
    join::join,
    layout::Layout,
    release::ReleaseSource,
    render::render_worker_unit,
    runner::{FakeRunner, Output},
    tls::{new_ca, pin_of},
};
use support::{MACHINE, TOKEN, enroll_stub, home_of, host, release_dir, release_server};

const VERSION: &str = "1.2.3";

fn args(hub: String, pin: String) -> JoinArgs {
    JoinArgs {
        hub,
        pin,
        code: Some("good-code".into()),
        replace: false,
        version: Some(Version::parse(VERSION).unwrap()),
        from_dir: None,
    }
}

/// Адрес, с которого нельзя скачать ничего: `--from-dir` не должен трогать сеть.
fn offline_source() -> ReleaseSource {
    ReleaseSource {
        api_base: "http://127.0.0.1:9/".parse().unwrap(),
        download_base: "http://127.0.0.1:9/".parse().unwrap(),
        repo: "Fenicu/canterlot".into(),
    }
}

fn connected_runner() -> FakeRunner {
    let runner = FakeRunner::new();
    runner.respond(|cmd| {
        (cmd.args.first().map(String::as_str) == Some("status")).then(|| {
            Output::ok(format!(
                r#"{{"version":"{VERSION}","connected":true,"hub_version":"{VERSION}","executions":[]}}"#
            ))
        })
    });
    runner
}

#[tokio::test]
async fn join_refuses_wrong_pin_before_sending_code() {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path().into(), Some(home_of(dir.path())));
    let ca = new_ca().unwrap();
    let other = new_ca().unwrap();
    let stub = enroll_stub(&ca, &ca, &["127.0.0.1"], "").await;
    let runner = connected_runner();
    let src = release_server(&[VERSION]).await;
    let err = join(
        &args(stub.addr.to_string(), pin_of(&other.cert_der)),
        &layout,
        &runner,
        &src,
        &host(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CtlError::PinMismatch { .. }), "{err}");
    assert!(!stub.joined(), "the code reached the hub");
    assert!(!layout.worker_config_dir().join("token").exists());
}

#[tokio::test]
async fn join_refuses_san_mismatch_before_sending_code() {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path().into(), Some(home_of(dir.path())));
    let ca = new_ca().unwrap();
    // Сертификат называет только имя, а машину подключают по адресу.
    let stub = enroll_stub(&ca, &ca, &["hub.example.internal"], "").await;
    let runner = connected_runner();
    let src = release_server(&[VERSION]).await;
    let err = join(
        &args(stub.addr.to_string(), pin_of(&ca.cert_der)),
        &layout,
        &runner,
        &src,
        &host(),
    )
    .await
    .unwrap_err();
    match &err {
        CtlError::SanMismatch { host } => assert_eq!(host, "127.0.0.1"),
        other => panic!("expected san_mismatch, got {other}"),
    }
    assert!(
        err.to_string().contains("canterlot tls add-name 127.0.0.1"),
        "{err}"
    );
    assert!(!stub.joined(), "the code reached the hub");
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

#[tokio::test]
async fn join_refuses_chain_from_another_ca_before_sending_code() {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path().into(), Some(home_of(dir.path())));
    let pinned = new_ca().unwrap();
    let signer = new_ca().unwrap();
    // `GetCa` отдаёт закреплённый УЦ, но сертификат сервера подписан другим.
    let stub = enroll_stub(&pinned, &signer, &["127.0.0.1"], "").await;
    let runner = connected_runner();
    let src = release_server(&[VERSION]).await;
    let err = join(
        &args(stub.addr.to_string(), pin_of(&pinned.cert_der)),
        &layout,
        &runner,
        &src,
        &host(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CtlError::TlsUntrusted(_)), "{err}");
    assert!(!stub.joined(), "the code reached the hub");
}

#[tokio::test]
async fn join_writes_identity_and_starts_worker() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_of(dir.path());
    let layout = Layout::new(dir.path().into(), Some(home.clone()));
    let ca = new_ca().unwrap();
    let stub = enroll_stub(&ca, &ca, &["127.0.0.1"], "").await;
    let runner = connected_runner();
    let src = release_server(&[VERSION]).await;
    let report = join(
        &args(stub.addr.to_string(), pin_of(&ca.cert_der)),
        &layout,
        &runner,
        &src,
        &host(),
    )
    .await
    .unwrap();
    assert!(stub.joined());
    assert_eq!(report.machine_id.to_string(), MACHINE);

    let config = layout.worker_config_dir();
    let token = config.join("token");
    assert_eq!(std::fs::read_to_string(&token).unwrap(), TOKEN);
    assert_eq!(
        std::fs::metadata(&token).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        std::fs::read_to_string(config.join("ca.pem"))
            .unwrap()
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    let toml = std::fs::read_to_string(config.join("worker.toml")).unwrap();
    assert!(
        toml.contains(&format!("machine_id = \"{MACHINE}\"")),
        "{toml}"
    );
    assert!(
        toml.contains(&format!("hub_url = \"https://{}\"", stub.addr)),
        "{toml}"
    );
    assert_eq!(
        std::fs::read_to_string(layout.worker_unit()).unwrap(),
        render_worker_unit()
    );
    assert_eq!(
        std::fs::read_link(layout.worker_current()).unwrap(),
        std::path::Path::new("releases").join(VERSION)
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".local/opt/canterlot/current/bin/canterlot-worker"))
            .unwrap(),
        format!("worker {VERSION}")
    );

    let calls: Vec<String> = runner
        .calls()
        .iter()
        .map(|c| format!("{} {}", c.program, c.args.join(" ")))
        .collect();
    for want in [
        "sudo -n loginctl enable-linger worker",
        "systemctl --user daemon-reload",
        "systemctl --user enable --now canterlot-worker",
    ] {
        assert!(calls.iter().any(|c| c == want), "{want} not in {calls:?}");
    }
    assert!(
        calls
            .iter()
            .all(|c| !c.contains(TOKEN) && !c.contains("good-code")),
        "a secret reached a command line: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.contains("status --json --config")),
        "{calls:?}"
    );
}

#[tokio::test]
async fn join_from_dir_uses_no_network() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_of(dir.path());
    let layout = Layout::new(dir.path().into(), Some(home.clone()));
    let ca = new_ca().unwrap();
    let stub = enroll_stub(&ca, &ca, &["127.0.0.1"], "").await;
    let runner = connected_runner();
    let release = tempfile::tempdir().unwrap();
    release_dir(release.path(), VERSION);
    let mut args = args(stub.addr.to_string(), pin_of(&ca.cert_der));
    args.version = None;
    args.from_dir = Some(release.path().to_path_buf());
    let report = join(&args, &layout, &runner, &offline_source(), &host())
        .await
        .unwrap();
    assert!(stub.joined());
    assert_eq!(report.version.as_str(), VERSION);
    assert_eq!(
        std::fs::read_to_string(home.join(".local/opt/canterlot/current/bin/canterlot-worker"))
            .unwrap(),
        format!("worker {VERSION}")
    );
    assert!(release.path().join("SHA256SUMS").is_file());
}

#[tokio::test]
async fn join_from_dir_with_bad_checksum_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path().into(), Some(home_of(dir.path())));
    let ca = new_ca().unwrap();
    let stub = enroll_stub(&ca, &ca, &["127.0.0.1"], "").await;
    let runner = connected_runner();
    let release = tempfile::tempdir().unwrap();
    release_dir(release.path(), VERSION);
    let worker = release.path().join(format!(
        "canterlot-worker-{VERSION}-x86_64-linux-gnu.tar.gz"
    ));
    std::fs::write(&worker, b"tampered").unwrap();
    let mut args = args(stub.addr.to_string(), pin_of(&ca.cert_der));
    args.version = None;
    args.from_dir = Some(release.path().to_path_buf());
    match join(&args, &layout, &runner, &offline_source(), &host()).await {
        Err(CtlError::ChecksumMismatch(name)) => {
            assert_eq!(name, worker.file_name().unwrap().to_str().unwrap());
        }
        other => panic!("expected checksum_mismatch, got {other:?}"),
    }
    assert!(!layout.worker_current().exists());
}

#[tokio::test]
async fn join_refuses_second_identity_without_replace() {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path().into(), Some(home_of(dir.path())));
    let ca = new_ca().unwrap();
    let stub = enroll_stub(&ca, &ca, &["127.0.0.1"], "").await;
    let runner = connected_runner();
    let src = release_server(&[VERSION]).await;
    let join_args = args(stub.addr.to_string(), pin_of(&ca.cert_der));
    join(&join_args, &layout, &runner, &src, &host())
        .await
        .unwrap();
    stub.join_called
        .store(false, std::sync::atomic::Ordering::SeqCst);

    let err = join(&join_args, &layout, &runner, &src, &host())
        .await
        .unwrap_err();
    assert!(matches!(err, CtlError::AlreadyJoined), "{err}");
    assert!(!stub.joined(), "a second code exchange started");
}

#[tokio::test]
async fn join_reports_refused_code_without_echoing_it() {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path().into(), Some(home_of(dir.path())));
    let ca = new_ca().unwrap();
    let stub = enroll_stub(&ca, &ca, &["127.0.0.1"], "").await;
    let runner = connected_runner();
    let src = release_server(&[VERSION]).await;
    let mut join_args = args(stub.addr.to_string(), pin_of(&ca.cert_der));
    join_args.code = Some("used-code-value".into());
    let err = join(&join_args, &layout, &runner, &src, &host())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("join_code_invalid"), "{err}");
    assert!(!err.to_string().contains("used-code-value"));
    assert!(!layout.worker_config_dir().join("token").exists());
}
