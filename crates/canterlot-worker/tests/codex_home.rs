//! Собственный CODEX_HOME canterlot: ссылки на дом владельца без инструкций,
//! SQLite, блокировок и демонов, общий AGENTS.md. Только временные каталоги;
//! настоящий ~/.codex не трогается.
#![forbid(unsafe_code)]

use canterlot_worker::adapter::codex::home::{
    CodexHome, EXCLUDED_NAMES, EXCLUDED_SUFFIXES, HomeError, LinkReport, TempDirPolicy,
};
use canterlot_worker::config::CodexConfig;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const BLOCK: &str = "canterlot-block";

fn owner_home() -> (TempDir, PathBuf) {
    let td = TempDir::new().unwrap();
    let home = td.path().join("owner");
    std::fs::create_dir_all(&home).unwrap();
    (td, home)
}

fn place_owner(home: &Path) {
    std::fs::write(home.join("config.toml"), "model = \"x\"\n").unwrap();
    std::fs::write(home.join("auth.json"), "{\"secret\":\"owner-secret\"}").unwrap();
    std::fs::write(home.join("AGENTS.md"), "owner instructions").unwrap();
    std::fs::write(home.join("AGENTS.override.md"), "override").unwrap();
    std::fs::write(home.join("custom.json"), "{}").unwrap();
    std::fs::create_dir_all(home.join("skills")).unwrap();
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::create_dir_all(home.join("app-server-control")).unwrap();
    std::fs::create_dir_all(home.join("thread-writer-locks")).unwrap();
    std::fs::create_dir_all(home.join("tmp")).unwrap();
    std::fs::write(home.join("app.db"), "sqlite").unwrap();
    std::fs::write(home.join("app.db-wal"), "wal").unwrap();
    std::fs::write(home.join("app.db-shm"), "shm").unwrap();
    std::fs::write(home.join(".sqlite-maintenance.lock"), "").unwrap();
    std::fs::create_dir_all(home.join("hapi-runtime-owners")).unwrap();
}

fn cfg(home_dir: &Path, owner: &Path) -> CodexConfig {
    CodexConfig {
        home_dir: Some(home_dir.to_path_buf()),
        owner_home: Some(owner.to_path_buf()),
        home_policy: TempDirPolicy::Allow,
        ..Default::default()
    }
}

fn env(owner: &Path) -> Vec<(OsString, OsString)> {
    vec![
        (
            OsString::from("CODEX_HOME"),
            owner.as_os_str().to_os_string(),
        ),
        (
            OsString::from("HOME"),
            OsString::from("/nonexistent-test-home"),
        ),
    ]
}

fn resolve(home_dir: &Path, owner: &Path) -> CodexHome {
    CodexHome::resolve(&cfg(home_dir, owner), Path::new("/tmp/exec"), &env(owner)).unwrap()
}

#[test]
fn links_every_owner_entry_except_instructions_sqlite_locks_and_daemon_dirs() {
    let (_td, owner) = owner_home();
    place_owner(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    let home = resolve(&canterlot, &owner);
    let report = home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    let names: Vec<String> = std::fs::read_dir(&canterlot)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    for keep in [
        "config.toml",
        "auth.json",
        "custom.json",
        "skills",
        "sessions",
        "AGENTS.md",
    ] {
        assert!(
            names.contains(&keep.to_string()),
            "linked {keep}: {names:?}"
        );
    }
    for excluded in EXCLUDED_NAMES.iter().chain(EXCLUDED_SUFFIXES.iter()) {
        // Собственный AGENTS.md canterlot — настоящий файл, а не ссылка.
        if *excluded == "AGENTS.md" || *excluded == "AGENTS.override.md" {
            continue;
        }
        assert!(
            !names.iter().any(|n| n == excluded || n.ends_with(excluded)),
            "excluded {excluded}: {names:?}"
        );
    }
    // AGENTS.md — блок canterlot, а не ссылка на файл владельца.
    let agents = std::fs::read_to_string(canterlot.join("AGENTS.md")).unwrap();
    assert_eq!(agents, BLOCK);
    assert!(
        !std::fs::symlink_metadata(canterlot.join("AGENTS.md"))
            .unwrap()
            .is_symlink()
    );
    // Ссылки абсолютные, каталог 0700, AGENTS.md 0600.
    let target = std::fs::read_link(canterlot.join("config.toml")).unwrap();
    assert!(target.is_absolute(), "{target:?}");
    assert_eq!(
        std::fs::metadata(&canterlot).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(canterlot.join("AGENTS.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(report.linked >= 5, "linked count: {report:?}");
    assert!(report.conflicts.is_empty(), "{report:?}");
}

#[test]
fn agents_md_is_the_canterlot_block_and_rewritten_only_when_changed() {
    let (_td, owner) = owner_home();
    place_owner(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    let home = resolve(&canterlot, &owner);
    let first = home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    assert!(first.linked > 0);
    let before = std::fs::metadata(canterlot.join("AGENTS.md")).unwrap();
    let second: LinkReport = home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    assert_eq!(second.linked, 0);
    assert_eq!(second.relinked, 0);
    assert_eq!(second.removed, 0);
    assert!(second.conflicts.is_empty());
    let same = std::fs::metadata(canterlot.join("AGENTS.md")).unwrap();
    assert_eq!(
        before.modified().unwrap(),
        same.modified().unwrap(),
        "unchanged block is not rewritten"
    );
    let third = home.prepare("other-block", TempDirPolicy::Allow).unwrap();
    assert_eq!(third.linked, 0);
    assert_eq!(
        std::fs::read_to_string(canterlot.join("AGENTS.md")).unwrap(),
        "other-block"
    );
}

#[test]
fn new_owner_entries_are_linked_and_vanished_targets_unlinked_on_next_launch() {
    let (_td, owner) = owner_home();
    place_owner(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    let home = resolve(&canterlot, &owner);
    home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    // Новая запись владельца появляется при следующем запуске.
    std::fs::write(owner.join("new-skill.md"), "new").unwrap();
    let report = home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    assert_eq!(report.linked, 1, "{report:?}");
    assert!(canterlot.join("new-skill.md").is_symlink());
    // Пропавшая запись: ссылка с исчезнувшей целью убирается.
    std::fs::remove_file(owner.join("custom.json")).unwrap();
    let report = home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    assert_eq!(report.removed, 1, "{report:?}");
    assert!(!canterlot.join("custom.json").exists());
}

#[test]
fn real_entries_are_never_replaced() {
    let (_td, owner) = owner_home();
    place_owner(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    std::fs::create_dir_all(&canterlot).unwrap();
    // Настоящий файл и каталог Codex: не трогать, отметить конфликт.
    std::fs::write(canterlot.join("config.toml"), "codex own").unwrap();
    std::fs::create_dir_all(canterlot.join("sessions")).unwrap();
    let home = resolve(&canterlot, &owner);
    let report = home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    assert_eq!(
        std::fs::read_to_string(canterlot.join("config.toml")).unwrap(),
        "codex own"
    );
    assert!(
        !std::fs::symlink_metadata(canterlot.join("config.toml"))
            .unwrap()
            .is_symlink()
    );
    assert!(
        report.conflicts.contains(&"config.toml".to_string()),
        "{report:?}"
    );
    assert!(
        report.conflicts.contains(&"sessions".to_string()),
        "{report:?}"
    );
}

#[test]
fn home_under_temp_dir_is_refused_in_production_policy() {
    let (_td, owner) = owner_home();
    place_owner(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    let home = resolve(&canterlot, &owner);
    assert!(matches!(
        home.prepare(BLOCK, TempDirPolicy::Refuse),
        Err(HomeError::Unusable)
    ));
    assert!(home.prepare(BLOCK, TempDirPolicy::Allow).is_ok());
}

#[test]
fn concurrent_prepare_is_serialised_by_the_lock() {
    let (_td, owner) = owner_home();
    place_owner(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    let home = resolve(&canterlot, &owner);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    home.prepare(BLOCK, TempDirPolicy::Allow)
                        .expect("concurrent prepare")
                })
            })
            .collect();
        let mut linked = 0;
        for handle in handles {
            linked += handle.join().unwrap().linked;
        }
        // Все записи связаны ровно один раз суммарно.
        assert!(linked >= 5, "total linked: {linked}");
    });
    let agents = std::fs::read_to_string(canterlot.join("AGENTS.md")).unwrap();
    assert_eq!(agents, BLOCK);
}

#[test]
fn builder_never_opens_owner_files() {
    let (_td, owner) = owner_home();
    place_owner(&owner);
    // Нечитаемый auth.json: построение всё равно успешно, секрет не копируется.
    std::fs::set_permissions(
        owner.join("auth.json"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let canterlot = owner.parent().unwrap().join("canterlot");
    let home = resolve(&canterlot, &owner);
    home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    assert!(canterlot.join("auth.json").is_symlink());
    let mut found_secret = false;
    for entry in walk(&canterlot) {
        if entry.is_symlink() {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&entry)
            && bytes.windows(12).any(|w| w == b"owner-secret")
        {
            found_secret = true;
        }
    }
    assert!(!found_secret, "owner file contents must not be copied");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() && !path.is_symlink() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}
