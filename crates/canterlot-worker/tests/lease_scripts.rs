//! Самообёртка `scripts/check.sh` и `scripts/check-web.sh` в аренду: какой вид аренды
//! выбирается и что делается без воркера. Сами проверки не запускаются: решение печатает
//! `CANTERLOT_LEASE_DRY=1`, перезапуск ловит поддельный бинарник воркера.

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Бинарник, который знает `lease --help` и записывает аргументы `lease run`.
fn fake_worker(dir: &Path, knows_lease: bool) -> PathBuf {
    let path = dir.join(if knows_lease {
        "new-worker"
    } else {
        "old-worker"
    });
    let log = dir.join("argv");
    let body = if knows_lease {
        format!(
            "#!/bin/sh\nif [ \"$1 $2\" = \"lease --help\" ]; then exit 0; fi\nprintf '%s\\n' \"$@\" > {log:?}\n"
        )
    } else {
        "#!/bin/sh\necho \"error: unrecognized subcommand '$1'\" >&2\nexit 2\n".to_owned()
    };
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn script(name: &str, args: &[&str], bin: Option<&Path>, env: &[(&str, &str)]) -> Command {
    let mut command = Command::new("bash");
    command
        .arg(repo().join("scripts").join(name))
        .args(args)
        .env_remove("CANTERLOT_LEASE_HELD")
        .env_remove("CANTERLOT_NO_LEASE")
        .env_remove("CANTERLOT_WORKER_BIN")
        // Без настоящего canterlot-worker на PATH.
        .env("PATH", "/usr/local/bin:/usr/bin:/bin");
    if let Some(bin) = bin {
        command.env("CANTERLOT_WORKER_BIN", bin);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    command
}

fn dry(name: &str, args: &[&str], bin: Option<&Path>, env: &[(&str, &str)]) -> String {
    let out = script(name, args, bin, env)
        .env("CANTERLOT_LEASE_DRY", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{name} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("canterlot lease dry: "))
        .unwrap_or_else(|| panic!("{name} {args:?}: no decision in {stdout}"))
        .to_owned()
}

#[test]
fn check_scripts_pick_gate_only_for_all_and_stack() {
    let dir = tempfile::tempdir().unwrap();
    let bin = fake_worker(dir.path(), true);
    let cases: &[(&str, &[&str], &str)] = &[
        ("check.sh", &["--all"], "gate"),
        ("check.sh", &[], "build"),
        ("check.sh", &["--fast"], "build"),
        ("check.sh", &["--fast", "-E", "binary(ws)"], "build"),
        ("check.sh", &["--base", "main"], "build"),
        ("check.sh", &["--fast", "--all"], "gate"),
        ("check-web.sh", &["--stack"], "gate"),
        ("check-web.sh", &[], "build"),
        ("check-web.sh", &["--fast"], "build"),
        ("check-web.sh", &["--fast", "--base", "main"], "build"),
    ];
    for (name, args, kind) in cases {
        assert_eq!(
            dry(name, args, Some(&bin), &[]),
            format!("kind={kind} wrap=yes"),
            "{name} {args:?}"
        );
    }
}

#[test]
fn check_scripts_skip_or_fall_back_without_a_usable_worker() {
    let dir = tempfile::tempdir().unwrap();
    let new = fake_worker(dir.path(), true);
    let old = fake_worker(dir.path(), false);
    for (bin, env, want) in [
        (
            Some(&new),
            &[("CANTERLOT_LEASE_HELD", "outer")][..],
            "wrap=held",
        ),
        (
            Some(&new),
            &[("CANTERLOT_NO_LEASE", "1")][..],
            "wrap=disabled",
        ),
        (Some(&old), &[][..], "wrap=old-binary"),
        (None, &[][..], "wrap=no-binary"),
    ] {
        for name in ["check.sh", "check-web.sh"] {
            let decision = dry(name, &[], bin.map(PathBuf::as_path), env);
            assert!(decision.ends_with(want), "{name} {env:?}: {decision}");
        }
    }
}

#[test]
fn check_sh_reexecs_itself_under_lease_run() {
    let dir = tempfile::tempdir().unwrap();
    let bin = fake_worker(dir.path(), true);
    let out = script("check.sh", &["--all"], Some(&bin), &[])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let argv = std::fs::read_to_string(dir.path().join("argv")).unwrap();
    let check = repo().join("scripts/check.sh");
    assert_eq!(
        argv.lines().collect::<Vec<_>>(),
        [
            "lease",
            "run",
            "--kind",
            "gate",
            "--label",
            "check.sh --all",
            "--",
            "bash",
            check.to_str().unwrap(),
            "--all"
        ]
    );
    std::fs::remove_file(dir.path().join("argv")).unwrap();
    let out = script("check-web.sh", &["--stack"], Some(&bin), &[])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let argv = std::fs::read_to_string(dir.path().join("argv")).unwrap();
    let web = repo().join("scripts/check-web.sh");
    assert_eq!(
        argv.lines().collect::<Vec<_>>(),
        [
            "lease",
            "run",
            "--kind",
            "gate",
            "--label",
            "check-web.sh --stack",
            "--",
            "bash",
            web.to_str().unwrap(),
            "--stack"
        ]
    );
}

/// Без воркера скрипт идёт дальше с одной строкой и метит вложенные вызовы.
#[test]
fn fallback_prints_one_line_and_marks_nested_calls() {
    let dir = tempfile::tempdir().unwrap();
    let old = fake_worker(dir.path(), false);
    let lib = repo().join("scripts/lease.sh");
    for (bin, reason) in [(Some(&old), "has no lease subcommand"), (None, "not found")] {
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(format!(
                "source {lib:?}; lease_self_wrap build label true; echo \"after=$CANTERLOT_LEASE_HELD\""
            ))
            .env_remove("CANTERLOT_LEASE_HELD")
            .env_remove("CANTERLOT_NO_LEASE")
            .env_remove("CANTERLOT_WORKER_BIN")
            .env("PATH", "/usr/local/bin:/usr/bin:/bin");
        if let Some(bin) = bin {
            command.env("CANTERLOT_WORKER_BIN", bin);
        }
        let out = command.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "after=unleased\n");
        assert_eq!(stderr.lines().count(), 1, "{stderr}");
        assert!(stderr.contains(reason), "{stderr}");
        assert!(stderr.contains("running without lease"), "{stderr}");
    }
}
