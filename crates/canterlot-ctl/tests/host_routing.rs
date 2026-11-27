//! Правило «обычный пользователь — свой воркер, sudo — весь хост» на временных каталогах.

use std::{fs, path::PathBuf};

use canterlot_ctl::{
    error::CtlError,
    hub_update::{HostRoute, route_host},
    layout::Layout,
    root::{Elevate, ensure_root_with},
};

struct HostLayout {
    _dir: tempfile::TempDir,
    hub: Layout,
    worker: Layout,
}

fn layout(with_hub: bool, with_worker: bool) -> HostLayout {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let home = dir.path().join("home/worker");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&home).unwrap();
    let hub = Layout::new(root.clone(), None);
    let worker = Layout::new(root, Some(home));
    if with_hub {
        fs::create_dir_all(hub.install_toml().parent().unwrap()).unwrap();
        fs::write(hub.install_toml(), "format = 1\n").unwrap();
    }
    if with_worker {
        fs::create_dir_all(worker.worker_config_dir()).unwrap();
        fs::write(worker.worker_config_dir().join("worker.toml"), "").unwrap();
    }
    HostLayout {
        _dir: dir,
        hub,
        worker,
    }
}

#[test]
fn root_on_hub_host_manages_the_whole_stack() {
    let l = layout(true, true);
    assert_eq!(
        route_host(&l.hub, &l.worker, true, false, false),
        HostRoute::Hub
    );
}

#[test]
fn plain_user_with_own_worker_manages_only_it() {
    let l = layout(true, true);
    assert_eq!(
        route_host(&l.hub, &l.worker, false, false, false),
        HostRoute::Worker
    );
}

#[test]
fn plain_user_without_worker_on_hub_host_needs_sudo() {
    let l = layout(true, false);
    assert_eq!(
        route_host(&l.hub, &l.worker, false, false, false),
        HostRoute::NeedsRoot
    );
}

#[test]
fn plain_user_off_hub_host_manages_own_worker() {
    let l = layout(false, true);
    assert_eq!(
        route_host(&l.hub, &l.worker, false, false, false),
        HostRoute::Worker
    );
}

#[test]
fn worker_only_markers_force_worker_route() {
    let l = layout(true, true);
    assert_eq!(
        route_host(&l.hub, &l.worker, true, true, false),
        HostRoute::Worker
    );
    assert_eq!(
        route_host(&l.hub, &l.worker, true, false, true),
        HostRoute::Worker
    );
}

struct NoTerminal;

impl Elevate for NoTerminal {
    fn is_root(&self) -> bool {
        false
    }
    fn interactive(&self) -> bool {
        false
    }
    fn self_exe(&self) -> Option<PathBuf> {
        None
    }
    fn exec_sudo(&self, _: &[String]) -> Result<(), CtlError> {
        Ok(())
    }
}

#[test]
fn root_required_error_names_the_command() {
    let err = ensure_root_with(&["update".into()], &NoTerminal).unwrap_err();
    assert!(err.to_string().contains("sudo canterlot update"), "{err}");
    let err = CtlError::RootRequired("uninstall".into());
    assert!(err.to_string().contains("sudo canterlot uninstall"), "{err}");
}
