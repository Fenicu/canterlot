//! Собственный CODEX_HOME canterlot: ссылки на дом владельца без инструкций,
//! SQLite, блокировок и демонов, общий AGENTS.md. Только временные каталоги;
//! настоящий ~/.codex не трогается.
#![forbid(unsafe_code)]

use canterlot_worker::adapter::codex::home::{
    CodexHome, EXCLUDED_NAMES, EXCLUDED_SUFFIXES, HomeError, HomeKind, LinkReport, TempDirPolicy,
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
    std::fs::create_dir_all(canterlot.join("cache")).unwrap();
    std::fs::create_dir_all(owner.join("cache")).unwrap();
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
        report.conflicts.contains(&"cache".to_string()),
        "{report:?}"
    );
}

fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .unwrap()
        .file_type()
        .is_symlink()
}

#[test]
fn sessions_are_linked_even_when_the_owner_has_none_yet() {
    let (_td, owner) = owner_home();
    let canterlot = owner.parent().unwrap().join("canterlot");
    let report = resolve(&canterlot, &owner)
        .prepare(BLOCK, TempDirPolicy::Allow)
        .unwrap();
    assert!(report.conflicts.is_empty(), "{report:?}");
    assert!(is_link(&canterlot.join("sessions")));
    assert!(
        owner.join("sessions").is_dir(),
        "created before first launch"
    );
}

#[test]
fn sessions_are_linked_when_the_owner_home_does_not_exist() {
    let (td, _) = owner_home();
    let owner = td.path().join("never-used");
    let canterlot = td.path().join("canterlot");
    resolve(&canterlot, &owner)
        .prepare(BLOCK, TempDirPolicy::Allow)
        .unwrap();
    assert!(is_link(&canterlot.join("sessions")));
}

#[test]
fn real_sessions_directory_moves_to_the_owner_and_becomes_a_link() {
    // Codex успел создать настоящий каталог: дом владельца был пуст при первом запуске.
    let (_td, owner) = owner_home();
    let canterlot = owner.parent().unwrap().join("canterlot");
    let day = canterlot.join("sessions/2026/11/01");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::write(day.join("rollout-a.jsonl"), "created under canterlot").unwrap();
    std::fs::create_dir_all(owner.join("sessions/2026/11/01")).unwrap();
    std::fs::write(owner.join("sessions/2026/11/01/rollout-b.jsonl"), "owner").unwrap();

    let report = resolve(&canterlot, &owner)
        .prepare(BLOCK, TempDirPolicy::Allow)
        .unwrap();
    assert!(report.conflicts.is_empty(), "{report:?}");
    assert!(is_link(&canterlot.join("sessions")));
    for name in ["rollout-a.jsonl", "rollout-b.jsonl"] {
        assert!(
            canterlot.join("sessions/2026/11/01").join(name).is_file(),
            "{name} visible from the canterlot home"
        );
        assert!(
            owner.join("sessions/2026/11/01").join(name).is_file(),
            "{name} visible from the owner home"
        );
    }
}

#[test]
fn real_sessions_directory_with_a_name_clash_stays_a_conflict() {
    let (_td, owner) = owner_home();
    let canterlot = owner.parent().unwrap().join("canterlot");
    std::fs::create_dir_all(canterlot.join("sessions")).unwrap();
    std::fs::write(canterlot.join("sessions/rollout-a.jsonl"), "ours").unwrap();
    std::fs::create_dir_all(owner.join("sessions")).unwrap();
    std::fs::write(owner.join("sessions/rollout-a.jsonl"), "theirs").unwrap();

    let report = resolve(&canterlot, &owner)
        .prepare(BLOCK, TempDirPolicy::Allow)
        .unwrap();
    assert!(
        report.conflicts.contains(&"sessions".to_string()),
        "{report:?}"
    );
    assert_eq!(
        std::fs::read_to_string(canterlot.join("sessions/rollout-a.jsonl")).unwrap(),
        "ours"
    );
    assert_eq!(
        std::fs::read_to_string(owner.join("sessions/rollout-a.jsonl")).unwrap(),
        "theirs"
    );
}

#[test]
fn lock_and_journal_entries_are_never_linked() {
    let (_td, owner) = owner_home();
    std::fs::create_dir_all(owner.join("mcp-oauth-locks")).unwrap();
    std::fs::write(owner.join("state_5.sqlite-journal"), "journal").unwrap();
    std::fs::write(owner.join("history.lock"), "").unwrap();
    std::fs::write(owner.join("config.toml"), "").unwrap();
    let td = TempDir::new().unwrap();
    let home = resolve(&td.path().join("home"), &owner);
    home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    assert!(home.path.join("config.toml").exists());
    for name in ["mcp-oauth-locks", "state_5.sqlite-journal", "history.lock"] {
        assert!(
            std::fs::symlink_metadata(home.path.join(name)).is_err(),
            "{name}"
        );
    }
}

#[test]
fn links_of_entries_excluded_later_are_removed() {
    let (_td, owner) = owner_home();
    std::fs::create_dir_all(owner.join("mcp-oauth-locks")).unwrap();
    let td = TempDir::new().unwrap();
    let home = resolve(&td.path().join("home"), &owner);
    std::fs::create_dir_all(&home.path).unwrap();
    let link = home.path.join("mcp-oauth-locks");
    std::os::unix::fs::symlink(owner.join("mcp-oauth-locks"), &link).unwrap();
    let report = home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    assert!(std::fs::symlink_metadata(&link).is_err());
    assert_eq!(report.removed, 1);
    assert!(owner.join("mcp-oauth-locks").is_dir(), "owner entry stays");
}

#[test]
fn link_with_an_unreadable_target_is_not_treated_as_vanished() {
    let (_td, owner) = owner_home();
    let closed = owner.join("closed");
    std::fs::create_dir_all(&closed).unwrap();
    std::fs::write(closed.join("inner"), "").unwrap();
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
    let td = TempDir::new().unwrap();
    let home = resolve(&td.path().join("home"), &owner);
    std::fs::create_dir_all(&home.path).unwrap();
    let link = home.path.join("kept");
    std::os::unix::fs::symlink(closed.join("inner"), &link).unwrap();
    let denied = std::fs::metadata(&link).is_err();
    let report = home.prepare(BLOCK, TempDirPolicy::Allow);
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o700)).unwrap();
    report.unwrap();
    // Под root отказа в доступе нет, и проверять нечего.
    if denied {
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "EACCES is not gone"
        );
    }
}

#[test]
fn relink_never_leaves_the_name_missing() {
    let (_td, first) = owner_home();
    let (_td2, second) = owner_home();
    std::fs::write(first.join("config.toml"), "").unwrap();
    std::fs::write(second.join("config.toml"), "").unwrap();
    let td = TempDir::new().unwrap();
    let path = td.path().join("home");
    let homes = [resolve(&path, &first), resolve(&path, &second)];
    homes[0].prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    let link = path.join("config.toml");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (stop, link) = (stop.clone(), link.clone());
        std::thread::spawn(move || {
            let mut missing = 0u32;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if std::fs::symlink_metadata(&link).is_err() {
                    missing += 1;
                }
            }
            missing
        })
    };
    for round in 0..400 {
        homes[round % 2]
            .prepare(BLOCK, TempDirPolicy::Allow)
            .unwrap();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(reader.join().unwrap(), 0, "running servers saw no config");
    assert!(
        std::fs::read_dir(&path).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("link-")),
        "no temporary link stays behind"
    );
}

#[test]
fn missing_path_behind_a_symlink_into_temp_is_refused() {
    let (td, owner) = owner_home();
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "codex-home-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let alias = base.join("alias");
    let _ = std::fs::remove_file(&alias);
    std::os::unix::fs::symlink(td.path(), &alias).unwrap();
    let home = CodexHome {
        path: alias.join("missing").join("home"),
        owner_home: owner,
        kind: HomeKind::Shared,
    };
    let refused = matches!(
        home.prepare(BLOCK, TempDirPolicy::Refuse),
        Err(HomeError::Unusable)
    );
    std::fs::remove_dir_all(&base).unwrap();
    assert!(refused, "the path lies under the temp dir through the link");
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

#[test]
fn canterlot_home_inside_or_around_owner_home_is_refused() {
    use std::os::unix::fs::symlink;
    let td = TempDir::new().unwrap();
    let owner = td.path().join("owner");
    std::fs::create_dir_all(&owner).unwrap();
    let home_env = |codex: Option<&std::ffi::OsStr>| {
        let mut env = vec![(
            OsString::from("HOME"),
            OsString::from("/nonexistent-test-home"),
        )];
        if let Some(home) = codex {
            env.push((OsString::from("CODEX_HOME"), home.to_os_string()));
        }
        env
    };
    let attempt = |home_dir: &Path, env: &[(OsString, OsString)]| {
        let cfg = CodexConfig {
            home_dir: Some(home_dir.to_path_buf()),
            owner_home: None,
            home_policy: TempDirPolicy::Allow,
            ..Default::default()
        };
        CodexHome::resolve(&cfg, Path::new("/tmp/exec"), env)
    };
    // Вложен: дом canterlot внутри дома владельца.
    let nested = owner.join("nested-home");
    assert!(
        attempt(&nested, &home_env(Some(owner.as_os_str()))).is_err(),
        "canterlot home inside owner home must be refused"
    );
    // Родитель: дом canterlot содержит дом владельца.
    assert!(
        attempt(td.path(), &home_env(Some(owner.as_os_str()))).is_err(),
        "canterlot home around owner home must be refused"
    );
    // Симлинк-алиас: canterlot ведёт в дом владельца.
    let alias = td.path().join("alias-home");
    symlink(&owner, &alias).unwrap();
    assert!(
        attempt(&alias, &home_env(Some(owner.as_os_str()))).is_err(),
        "symlinked alias of owner home must be refused"
    );
    // Алиас внутрь дома владельца — тоже отказ.
    let nested_dir = owner.join("nested");
    std::fs::create_dir_all(&nested_dir).unwrap();
    let alias_inside = td.path().join("alias-inside");
    symlink(&nested_dir, &alias_inside).unwrap();
    assert!(
        attempt(&alias_inside, &home_env(Some(owner.as_os_str()))).is_err(),
        "symlink into owner home must be refused"
    );
    // Тот же дом через алиас наоборот: canterlot настоящий, владелец — ссылка
    // на посторонний каталог. Пересечений нет — разрешается.
    let owner_alias = td.path().join("owner-alias");
    symlink(&owner, &owner_alias).unwrap();
    let real_canterlot = td.path().join("real-home");
    std::fs::create_dir_all(&real_canterlot).unwrap();
    assert!(
        attempt(&real_canterlot, &home_env(Some(owner_alias.as_os_str()))).is_ok(),
        "unrelated homes must resolve even when the owner is behind a symlink"
    );
    // Относительный дом владельца.
    assert!(
        attempt(
            &td.path().join("elsewhere"),
            &home_env(Some(std::ffi::OsStr::new("relative/codex")))
        )
        .is_err(),
        "relative owner home must be refused"
    );
    // Пустой HOME: запасной `.codex` относителен — тоже отказ.
    assert!(
        attempt(
            &td.path().join("elsewhere"),
            &[(OsString::from("HOME"), OsString::from(""))]
        )
        .is_err(),
        "empty HOME fallback must be refused"
    );
    // Разные существующие каталоги рядом — можно.
    let sibling = td.path().join("sibling-home");
    std::fs::create_dir_all(&sibling).unwrap();
    assert!(
        attempt(&sibling, &home_env(Some(owner.as_os_str()))).is_ok(),
        "unrelated absolute homes must resolve"
    );
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

fn fake_owner_with_nanokvm(owner: &Path) {
    place_owner(owner);
    std::fs::write(
        owner.join("config.toml"),
        "model = \"x\"\napproval_policy = \"never\"\n\n[mcp_servers.nanokvm]\ncommand = \"/bin/true\"\n\n[mcp_servers.other]\nurl = \"https://example.invalid\"\n\n[projects.\"/w\"]\ntrust_level = \"trusted\"\n",
    )
    .unwrap();
}

#[test]
fn isolated_home_copies_the_owner_config_without_mcp_servers() {
    let (_td, owner) = owner_home();
    fake_owner_with_nanokvm(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    let shared = resolve(&canterlot, &owner);
    shared.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    assert!(
        canterlot.join("config.toml").is_symlink(),
        "the shared home of an owner-started session follows the owner"
    );

    let home = shared.clone().isolated(false);
    assert_eq!(
        home.path,
        owner.parent().unwrap().join("canterlot-isolated")
    );
    assert_eq!(home.kind, HomeKind::Isolated);
    home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    let config = home.path.join("config.toml");
    assert!(!config.is_symlink(), "a real file, not a link to the owner");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(
        !text.contains("nanokvm") && !text.contains("mcp_servers"),
        "{text}"
    );
    let parsed: toml::Table = text.parse().unwrap();
    assert_eq!(parsed["model"].as_str(), Some("x"));
    assert_eq!(parsed["approval_policy"].as_str(), Some("never"));
    assert_eq!(
        parsed["projects"]["/w"]["trust_level"].as_str(),
        Some("trusted")
    );
    assert_eq!(
        std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Остальное по-прежнему ссылки на владельца, а своё AGENTS.md - блок canterlot.
    assert!(home.path.join("auth.json").is_symlink());
    assert_eq!(
        std::fs::read_to_string(home.path.join("AGENTS.md")).unwrap(),
        BLOCK
    );
    // Файл владельца не тронут.
    assert!(
        std::fs::read_to_string(owner.join("config.toml"))
            .unwrap()
            .contains("nanokvm")
    );
}

#[test]
fn isolated_home_follows_the_owner_config_on_every_prepare() {
    let (_td, owner) = owner_home();
    fake_owner_with_nanokvm(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    let home = resolve(&canterlot, &owner).isolated(false);
    home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    std::fs::write(
        owner.join("config.toml"),
        "model = \"y\"\n[mcp_servers.late]\ncommand = \"x\"\n",
    )
    .unwrap();
    home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    let text = std::fs::read_to_string(home.path.join("config.toml")).unwrap();
    assert!(
        text.contains("model = \"y\"") && !text.contains("late"),
        "{text}"
    );
}

#[test]
fn isolated_home_refuses_an_unreadable_owner_config() {
    let (_td, owner) = owner_home();
    place_owner(&owner);
    std::fs::write(owner.join("config.toml"), "= broken [").unwrap();
    let canterlot = owner.parent().unwrap().join("canterlot");
    let home = resolve(&canterlot, &owner).isolated(false);
    assert!(matches!(
        home.prepare(BLOCK, TempDirPolicy::Allow),
        Err(HomeError::Unusable)
    ));
    assert!(!home.path.join("config.toml").exists());
}

#[test]
fn isolated_owner_home_links_the_owner_instructions_and_has_no_mcp_config() {
    let (_td, owner) = owner_home();
    fake_owner_with_nanokvm(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    let home = resolve(&canterlot, &owner).isolated(true);
    assert_eq!(
        home.path,
        owner.parent().unwrap().join("canterlot-isolated-owner")
    );
    home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    assert!(home.path.join("AGENTS.md").is_symlink());
    assert!(home.path.join("AGENTS.override.md").is_symlink());
    assert!(
        !std::fs::read_to_string(home.path.join("config.toml"))
            .unwrap()
            .contains("nanokvm")
    );
    // SQLite и блокировки по-прежнему не ссылаются.
    assert!(!home.path.join("state_5.sqlite").exists());
}

const FAKE_KEY: &str = "ctx7-FAKE-not-a-real-key";

fn fake_owner_with_context7(owner: &Path) {
    place_owner(owner);
    std::fs::write(
        owner.join("config.toml"),
        format!(
            "model = \"x\"\n\n[mcp_servers.nanokvm]\ncommand = \"/bin/true\"\n\n\
             [mcp_servers.context7]\nurl = \"https://mcp.context7.example/mcp\"\n\
             startup_timeout_sec = 20\n\n[mcp_servers.context7.http_headers]\n\
             Authorization = \"Bearer {FAKE_KEY}\"\n\n[projects.\"/w\"]\ntrust_level = \"trusted\"\n"
        ),
    )
    .unwrap();
}

fn delegates(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn isolated_home_keeps_only_the_allowed_server_with_its_sub_tables() {
    let (_td, owner) = owner_home();
    fake_owner_with_context7(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    for owner_instructions in [false, true] {
        let home = resolve(&canterlot, &owner).isolated(owner_instructions);
        let report = home
            .prepare_with(BLOCK, TempDirPolicy::Allow, &delegates(&["context7"]))
            .unwrap();
        assert_eq!(report.mcp_kept, ["context7"]);
        assert!(report.mcp_missing.is_empty());
        let config = home.path.join("config.toml");
        let parsed: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
        let servers = parsed["mcp_servers"].as_table().unwrap();
        assert_eq!(servers.keys().collect::<Vec<_>>(), ["context7"]);
        assert_eq!(
            servers["context7"]["http_headers"]["Authorization"].as_str(),
            Some(format!("Bearer {FAKE_KEY}").as_str())
        );
        assert_eq!(
            servers["context7"]["startup_timeout_sec"].as_integer(),
            Some(20)
        );
        assert_eq!(parsed["model"].as_str(), Some("x"));
        assert_eq!(
            std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&home.path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // Отчёт для диагностики без значений из файла.
        let debug = format!("{report:?}");
        assert!(!debug.contains(FAKE_KEY), "{debug}");
    }
}

#[test]
fn isolated_home_reports_a_missing_allowed_server_by_name() {
    let (_td, owner) = owner_home();
    fake_owner_with_nanokvm(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    let home = resolve(&canterlot, &owner).isolated(false);
    let report = home
        .prepare_with(
            BLOCK,
            TempDirPolicy::Allow,
            &delegates(&["context7", "canterlot"]),
        )
        .unwrap();
    assert!(report.mcp_kept.is_empty());
    assert_eq!(report.mcp_missing, ["context7"], "canterlot is never taken");
    let text = std::fs::read_to_string(home.path.join("config.toml")).unwrap();
    assert!(!text.contains("mcp_servers"), "{text}");
}

#[test]
fn isolated_home_with_no_allowed_server_is_exactly_as_before() {
    let (_td, owner) = owner_home();
    fake_owner_with_context7(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    let home = resolve(&canterlot, &owner).isolated(false);
    home.prepare(BLOCK, TempDirPolicy::Allow).unwrap();
    let plain = std::fs::read_to_string(home.path.join("config.toml")).unwrap();
    assert!(!plain.contains("mcp_servers") && !plain.contains(FAKE_KEY));
    let report = home.prepare_with(BLOCK, TempDirPolicy::Allow, &[]).unwrap();
    assert!(report.mcp_kept.is_empty() && report.mcp_missing.is_empty());
    assert_eq!(
        std::fs::read_to_string(home.path.join("config.toml")).unwrap(),
        plain
    );
}

#[test]
fn shared_home_ignores_the_allow_list() {
    let (_td, owner) = owner_home();
    fake_owner_with_context7(&owner);
    let canterlot = owner.parent().unwrap().join("canterlot");
    let home = resolve(&canterlot, &owner);
    let report = home
        .prepare_with(BLOCK, TempDirPolicy::Allow, &delegates(&["context7"]))
        .unwrap();
    assert!(canterlot.join("config.toml").is_symlink());
    assert!(report.mcp_kept.is_empty() && report.mcp_missing.is_empty());
}
