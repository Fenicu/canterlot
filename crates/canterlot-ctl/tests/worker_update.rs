//! Обновление воркера на FakeRunner. Ответ `canterlot-worker status --json` строится из
//! того, на какой релиз сейчас указывает `current`: так «перезапуск» меняет версию.

mod support;

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use canterlot_ctl::{
    cli::{UpdateArgs, Version},
    error::CtlError,
    layout::Layout,
    ops::{self, OpKind, OpRecord, OpResult},
    runner::{Cmd, FakeRunner, Output},
    worker_update::update_worker,
};
use serde_json::json;
use support::{home_of, host, release_server};

const OLD: &str = "1.2.3";
const NEW: &str = "1.3.0";

struct Machine {
    _dir: tempfile::TempDir,
    layout: Layout,
    home: PathBuf,
}

fn machine(releases: &[&str]) -> Machine {
    let dir = tempfile::tempdir().unwrap();
    let home = home_of(dir.path());
    let layout = Layout::new(dir.path().into(), Some(home.clone()));
    for name in releases {
        let bin = layout.worker_releases().join(name).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("canterlot-worker"), format!("worker {name}")).unwrap();
    }
    std::os::unix::fs::symlink(Path::new("releases").join(OLD), layout.worker_current()).unwrap();
    Machine {
        _dir: dir,
        layout,
        home,
    }
}

fn is_status(cmd: &Cmd) -> bool {
    cmd.args.first().map(String::as_str) == Some("status")
}

fn is_restart(cmd: &Cmd) -> bool {
    cmd.program == "systemctl" && cmd.args.iter().any(|a| a == "restart")
}

/// Статус воркера: версия — имя релиза `current`; `attached(version)` решает, подхватила ли
/// эта сборка исполнение `e1`, а `exe` — путь его сопровождения.
fn fake_worker(
    layout: &Layout,
    hub: &str,
    attached: fn(&str) -> bool,
    exe: Option<PathBuf>,
) -> (FakeRunner, Arc<AtomicUsize>) {
    let runner = FakeRunner::new();
    let restarts = Arc::new(AtomicUsize::new(0));
    let current = layout.worker_current();
    let hub = hub.to_owned();
    let counter = restarts.clone();
    runner.respond(move |cmd| {
        if is_restart(cmd) {
            counter.fetch_add(1, Ordering::SeqCst);
            return Some(Output::ok(""));
        }
        if !is_status(cmd) {
            return None;
        }
        let version = std::fs::read_link(&current)
            .ok()?
            .file_name()?
            .to_string_lossy()
            .into_owned();
        Some(Output::ok(
            json!({
                "version": version,
                "connected": true,
                "connected_since": "2026-11-27T00:00:00Z",
                "hub_version": hub,
                "executions": [{
                    "id": "e1",
                    "attached": attached(&version),
                    "supervisor_exe": exe,
                }],
            })
            .to_string(),
        ))
    });
    (runner, restarts)
}

fn update_to(v: &str) -> UpdateArgs {
    UpdateArgs {
        version: Some(Version::parse(v).unwrap()),
        ..UpdateArgs::default()
    }
}

#[tokio::test]
async fn worker_update_refuses_past_hub_version() {
    let m = machine(&[OLD]);
    let (runner, restarts) = fake_worker(&m.layout, OLD, |_| true, None);
    let src = release_server(&[NEW]).await;
    let err = update_worker(&update_to(NEW), &m.layout, &runner, &src, &host())
        .await
        .unwrap_err();
    assert!(matches!(err, CtlError::HubOlderThanTarget { .. }), "{err}");
    assert_eq!(restarts.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read_link(m.layout.worker_current()).unwrap(),
        Path::new("releases").join(OLD)
    );
    assert!(!m.layout.worker_releases().join(NEW).exists());
}

#[tokio::test]
async fn worker_update_rolls_back_when_execution_not_reattached() {
    let m = machine(&[OLD]);
    // Новая сборка на связи, но исполнение не подхватила до конца срока.
    let (runner, restarts) = fake_worker(&m.layout, NEW, |v| v == OLD, None);
    let src = release_server(&[NEW]).await;
    let result = update_worker(&update_to(NEW), &m.layout, &runner, &src, &host())
        .await
        .unwrap();
    match &result {
        OpResult::RolledBack { reason } => assert!(reason.contains("e1"), "{reason}"),
        other => panic!("expected rolled back, got {other:?}"),
    }
    assert_eq!(restarts.load(Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read_link(m.layout.worker_current()).unwrap(),
        Path::new("releases").join(OLD)
    );
    let rec = ops::latest(&m.layout.worker_ops_dir()).unwrap().unwrap();
    assert_eq!(rec.result, Some(result));
    assert!(rec.history.iter().any(|(_, phase)| phase == "rollback"));
}

#[tokio::test]
async fn worker_update_keeps_release_dirs_in_use() {
    // 1.0.0 держит живое сопровождение, 1.1.0 никому не нужен, sha-каталог прежних выкладок
    // утилите чужой.
    let m = machine(&["1.0.0", "1.1.0", OLD, "0123abcd"]);
    let supervisor = m
        .layout
        .worker_releases()
        .join("1.0.0/bin/canterlot-worker");
    let (runner, restarts) = fake_worker(&m.layout, NEW, |_| true, Some(supervisor));
    let src = release_server(&[NEW]).await;
    let result = update_worker(&update_to(NEW), &m.layout, &runner, &src, &host())
        .await
        .unwrap();
    assert_eq!(result, OpResult::Done);
    assert_eq!(restarts.load(Ordering::SeqCst), 1);
    let releases = m.layout.worker_releases();
    assert_eq!(
        std::fs::read_link(m.layout.worker_current()).unwrap(),
        Path::new("releases").join(NEW)
    );
    assert_eq!(
        std::fs::read_link(m.layout.worker_previous()).unwrap(),
        Path::new("releases").join(OLD)
    );
    assert_eq!(
        std::fs::read_to_string(releases.join(NEW).join("bin/canterlot-worker")).unwrap(),
        format!("worker {NEW}")
    );
    for kept in ["1.0.0", OLD, "0123abcd", NEW] {
        assert!(releases.join(kept).is_dir(), "{kept} was removed");
    }
    assert!(!releases.join("1.1.0").exists(), "unused release kept");
}

#[tokio::test]
async fn worker_update_refuses_missing_supervisor_binary() {
    let m = machine(&[OLD]);
    let gone = m.home.join("gone/canterlot-worker (deleted)");
    let (runner, restarts) = fake_worker(&m.layout, NEW, |_| true, Some(gone));
    let src = release_server(&[NEW]).await;
    let err = update_worker(&update_to(NEW), &m.layout, &runner, &src, &host())
        .await
        .unwrap_err();
    assert!(matches!(err, CtlError::SupervisorBinaryMissing(_)), "{err}");
    assert_eq!(restarts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn worker_update_runs_inside_transient_unit() {
    let m = machine(&[OLD]);
    let (runner, restarts) = fake_worker(&m.layout, NEW, |_| true, None);
    let ops_dir = m.layout.worker_ops_dir();
    // Юнит — это та же утилита: здесь он только записывает итог, как сделал бы внутренний запуск.
    runner.respond(move |cmd| {
        if cmd.program != "systemd-run" {
            return None;
        }
        let mut rec = ops::begin(&ops_dir, OpRecord::new(OpKind::Update, None)).unwrap();
        ops::finish(&ops_dir, &mut rec, OpResult::Done).unwrap();
        Some(Output::ok(""))
    });
    let src = release_server(&[NEW]).await;
    let mut outside = host();
    outside.in_unit = false;
    let result = update_worker(&update_to(NEW), &m.layout, &runner, &src, &outside)
        .await
        .unwrap();
    assert_eq!(result, OpResult::Done);
    assert_eq!(
        restarts.load(Ordering::SeqCst),
        0,
        "restart outside the unit"
    );
    let ctl = m.layout.worker_ctl(&Version::parse(NEW).unwrap());
    assert_eq!(std::fs::read_to_string(&ctl).unwrap(), format!("ctl {NEW}"));
    assert_eq!(std::fs::read_link(m.layout.worker_ctl_link()).unwrap(), ctl);
    let call = runner
        .calls()
        .into_iter()
        .find(|c| c.program == "systemd-run")
        .expect("systemd-run");
    let args = call.args.join(" ");
    assert!(
        args.starts_with("--user --unit canterlot-update-"),
        "{args}"
    );
    assert!(args.contains("--wait --collect"), "{args}");
    assert!(args.contains("--setenv=CANTERLOT_CTL_IN_UNIT=1"), "{args}");
    assert!(
        args.ends_with(&format!("{} update --version {NEW}", ctl.display())),
        "{args}"
    );
}
