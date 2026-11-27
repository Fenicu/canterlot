//! Удаление воркера: живые сессии останавливают его без `--force`, тождество и данные
//! остаются без `--purge`, а `--purge` не ходит по подложенным ссылкам.

mod support;

use std::path::Path;

use canterlot_ctl::{
    cli::UninstallArgs,
    error::CtlError,
    layout::Layout,
    runner::{FakeRunner, Output},
    uninstall::uninstall_worker,
};
use support::{MACHINE, home_of, host};

struct Installed {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    layout: Layout,
}

fn installed() -> Installed {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let home = home_of(&root);
    let layout = Layout::new(root.clone(), Some(home.clone()));
    let config = layout.worker_config_dir();
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("worker.toml"),
        format!("machine_id = \"{MACHINE}\"\n"),
    )
    .unwrap();
    std::fs::write(config.join("token"), "token").unwrap();
    std::fs::create_dir_all(layout.worker_unit().parent().unwrap()).unwrap();
    std::fs::write(layout.worker_unit(), "[Unit]\n").unwrap();
    let bin = layout.worker_releases().join("1.2.3/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("canterlot-worker"), "worker").unwrap();
    std::os::unix::fs::symlink(Path::new("releases/1.2.3"), layout.worker_current()).unwrap();
    let ctl = layout.worker_opt().join("ctl/1.2.3/canterlot");
    std::fs::create_dir_all(ctl.parent().unwrap()).unwrap();
    std::fs::write(&ctl, "ctl").unwrap();
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    std::os::unix::fs::symlink(&ctl, layout.worker_ctl_link()).unwrap();
    std::fs::create_dir_all(layout.worker_data_dir().join("store")).unwrap();
    Installed {
        _dir: dir,
        root,
        layout,
    }
}

fn worker_with(executions: &str) -> FakeRunner {
    let runner = FakeRunner::new();
    let body = format!(
        r#"{{"version":"1.2.3","connected":true,"hub_version":"1.2.3","executions":{executions}}}"#
    );
    runner.respond(move |cmd| {
        (cmd.args.first().map(String::as_str) == Some("status")).then(|| Output::ok(body.clone()))
    });
    runner
}

#[tokio::test]
async fn uninstall_worker_refuses_live_sessions() {
    let m = installed();
    let runner = worker_with(r#"[{"id":"e1","attached":true}]"#);
    let err = uninstall_worker(&UninstallArgs::default(), &m.layout, &runner, &host())
        .await
        .unwrap_err();
    assert!(matches!(err, CtlError::SessionsAlive(1)), "{err}");
    assert!(m.layout.worker_unit().is_file());
    assert!(m.layout.worker_opt().is_dir());
    assert!(!runner.called("systemctl"));
}

#[tokio::test]
async fn uninstall_worker_refuses_supervisors_of_a_stopped_worker() {
    let m = installed();
    // Воркер не отвечает, но каталог исполнения на месте: сопровождение может жить.
    let runner = FakeRunner::new();
    runner.set_default(Output::failed(1, ""));
    std::fs::create_dir_all(m.layout.worker_data_dir().join("executions/e1")).unwrap();
    let err = uninstall_worker(&UninstallArgs::default(), &m.layout, &runner, &host())
        .await
        .unwrap_err();
    assert!(matches!(err, CtlError::SessionsAlive(1)), "{err}");
}

#[tokio::test]
async fn uninstall_worker_keeps_identity() {
    let m = installed();
    let runner = worker_with("[]");
    let report = uninstall_worker(&UninstallArgs::default(), &m.layout, &runner, &host())
        .await
        .unwrap();
    assert!(!m.layout.worker_unit().exists());
    assert!(!m.layout.worker_opt().exists());
    assert!(std::fs::symlink_metadata(m.layout.worker_ctl_link()).is_err());
    assert!(m.layout.worker_config_dir().join("token").is_file());
    assert!(m.layout.worker_data_dir().join("store").is_dir());
    let calls: Vec<String> = runner
        .calls()
        .iter()
        .map(|c| format!("{} {}", c.program, c.args.join(" ")))
        .collect();
    assert!(
        calls.contains(&"systemctl --user disable --now canterlot-worker".to_owned()),
        "{calls:?}"
    );
    let text = report.to_string();
    assert!(text.contains(&format!("--machine-id {MACHINE}")), "{text}");
    assert!(
        text.contains(&m.layout.worker_config_dir().display().to_string()),
        "{text}"
    );
}

#[tokio::test]
async fn uninstall_worker_purge_does_not_follow_symlinks() {
    let m = installed();
    let outside = m.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep"), "owner data").unwrap();
    std::os::unix::fs::symlink(&outside, m.layout.worker_data_dir().join("planted")).unwrap();
    let runner = worker_with("[]");
    let args = UninstallArgs {
        purge: true,
        force: false,
    };

    let mut wrong = host();
    wrong.answer = Some("another-host".into());
    let err = uninstall_worker(&args, &m.layout, &runner, &wrong)
        .await
        .unwrap_err();
    assert!(matches!(err, CtlError::ConfirmationMismatch(_)), "{err}");
    assert!(
        m.layout.worker_unit().is_file(),
        "changed before confirmation"
    );

    let mut confirmed = host();
    confirmed.answer = Some(confirmed.hostname.clone());
    uninstall_worker(&args, &m.layout, &runner, &confirmed)
        .await
        .unwrap();
    assert!(!m.layout.worker_config_dir().exists());
    assert!(!m.layout.worker_data_dir().exists());
    assert_eq!(
        std::fs::read_to_string(outside.join("keep")).unwrap(),
        "owner data"
    );
}
