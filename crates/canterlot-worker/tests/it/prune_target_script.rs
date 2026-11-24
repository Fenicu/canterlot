//! `scripts/prune-target.sh`: старые копии тестовых бинарников уходят по единице сборки
//! из отпечатков cargo, текущие единицы с общим стеблем имени остаются; в раскладе
//! cargo 1.100 уходит старый каталог `build/<пакет>/<хэш>`.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn age(path: &Path, age_hours: u64) {
    let at = SystemTime::now() - Duration::from_secs(age_hours * 3600);
    fs::File::open(path).unwrap().set_modified(at).unwrap();
}

fn binary(path: &Path, age_hours: u64) {
    fs::write(path, vec![0u8; (1 << 20) + 1]).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    age(path, age_hours);
}

fn unit(profile: &Path, package: &str, hash: &str, name: &str, features: &str) {
    let dir = profile.join(format!(".fingerprint/{package}-{hash}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), "0011223344556677").unwrap();
    fs::write(dir.join(format!("dep-{name}")), "").unwrap();
    fs::write(dir.join("invoked.timestamp"), "").unwrap();
    let json = serde_json::json!({
        "rustc": 1, "features": features, "declared_features": "[]",
        "target": name.len(), "profile": 7, "path": 3, "deps": [], "local": [],
        "rustflags": [], "config": 9, "compile_kind": 0,
    });
    fs::write(dir.join(format!("{name}.json")), json.to_string()).unwrap();
}

#[test]
fn prune_keeps_every_current_unit_and_drops_older_copies_of_the_same_unit() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("debug");
    let deps = profile.join("deps");
    fs::create_dir_all(&deps).unwrap();
    let units = [
        // Интеграционный тест `version` в двух пакетах.
        (
            "version-1111111111111111",
            5,
            "canterlot-hub",
            "test-integration-test-version",
            "[]",
        ),
        (
            "version-2222222222222222",
            1,
            "canterlot-worker",
            "test-integration-test-version",
            "[]",
        ),
        // Бинарник, тесты библиотеки и тесты бинарника одного пакета.
        (
            "canterlot_hub-3333333333333333",
            4,
            "canterlot-hub",
            "bin-canterlot-hub",
            "[]",
        ),
        (
            "canterlot_hub-4444444444444444",
            3,
            "canterlot-hub",
            "test-lib-canterlot_hub",
            "[]",
        ),
        (
            "canterlot_hub-5555555555555555",
            2,
            "canterlot-hub",
            "test-bin-canterlot-hub",
            "[]",
        ),
        // Другой набор возможностей.
        (
            "exec-6666666666666666",
            6,
            "canterlot-worker",
            "test-integration-test-exec",
            "[]",
        ),
        (
            "exec-7777777777777777",
            1,
            "canterlot-worker",
            "test-integration-test-exec",
            "[\"fault-injection\"]",
        ),
        // Старая и свежая копии одной единицы.
        (
            "lease-8888888888888888",
            9,
            "canterlot-worker",
            "test-integration-test-lease",
            "[]",
        ),
        (
            "lease-9999999999999999",
            1,
            "canterlot-worker",
            "test-integration-test-lease",
            "[]",
        ),
    ];
    for (file, age, package, name, features) in units {
        binary(&deps.join(file), age);
        let hash = file.rsplit_once('-').unwrap().1;
        unit(&profile, package, hash, name, features);
    }
    fs::write(deps.join("lease-8888888888888888.d"), "d").unwrap();
    // Без отпечатка — не трогается.
    binary(&deps.join("lease-aaaaaaaaaaaaaaaa"), 20);

    let output = Command::new("bash")
        .arg(repo().join("scripts/prune-target.sh"))
        .arg(root.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let mut left: Vec<_> = fs::read_dir(&deps)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    let mut want: Vec<_> = units
        .iter()
        .map(|u| u.0.to_owned())
        .filter(|f| f != "lease-8888888888888888")
        .chain(["lease-aaaaaaaaaaaaaaaa".to_owned()])
        .collect();
    want.sort();
    assert_eq!(left, want);
}

/// Единица нового расклада cargo 1.100: `build/<пакет>/<хэш>/{fingerprint,out}`.
fn new_unit(
    profile: &Path,
    package: &str,
    hash: &str,
    name: &str,
    features: &str,
    age_hours: u64,
) -> PathBuf {
    let dir = profile.join(format!("build/{package}/{hash}"));
    let fingerprint = dir.join("fingerprint");
    fs::create_dir_all(&fingerprint).unwrap();
    fs::create_dir_all(dir.join("out")).unwrap();
    fs::write(fingerprint.join(name), "0011223344556677").unwrap();
    fs::write(fingerprint.join(format!("dep-{name}")), "").unwrap();
    fs::write(fingerprint.join("invoked.timestamp"), "").unwrap();
    let json = serde_json::json!({
        "rustc": 1, "features": features, "declared_features": "[]",
        "target": name.len(), "profile": 7, "path": 3, "deps": [], "local": [],
        "rustflags": [], "config": 9, "compile_kind": 0,
    });
    fs::write(fingerprint.join(format!("{name}.json")), json.to_string()).unwrap();
    binary(&dir.join("out").join(format!("artifact-{hash}")), age_hours);
    age(&dir.join("out"), age_hours);
    dir
}

#[test]
fn prune_drops_older_new_layout_units_and_keeps_libraries_scripts_and_ambiguity() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("debug");
    // Две копии одной единицы: старая уходит целым каталогом, свежая остаётся.
    let old = new_unit(
        &profile,
        "canterlot-worker",
        "8888888888888888",
        "test-integration-test-lease",
        "[]",
        9,
    );
    let fresh = new_unit(
        &profile,
        "canterlot-worker",
        "9999999999999999",
        "test-integration-test-lease",
        "[]",
        1,
    );
    // Бинарник — тоже единица.
    let bin_old = new_unit(
        &profile,
        "canterlot-hub",
        "1111111111111111",
        "bin-canterlot-hub",
        "[]",
        8,
    );
    let bin_fresh = new_unit(
        &profile,
        "canterlot-hub",
        "2222222222222222",
        "bin-canterlot-hub",
        "[]",
        2,
    );
    // Библиотека и скрипт сборки с большими файлами в out не трогаются.
    let lib_old = new_unit(
        &profile,
        "canterlot-worker",
        "3333333333333333",
        "lib-canterlot_worker",
        "[]",
        30,
    );
    let lib_fresh = new_unit(
        &profile,
        "canterlot-worker",
        "4444444444444444",
        "lib-canterlot_worker",
        "[]",
        1,
    );
    let script_old = new_unit(
        &profile,
        "canterlot-worker",
        "5555555555555555",
        "build-script-build-script-build",
        "[]",
        30,
    );
    let script_fresh = new_unit(
        &profile,
        "canterlot-worker",
        "6666666666666666",
        "build-script-build-script-build",
        "[]",
        1,
    );
    // Каталог без ровно одного отпечатка не трогается.
    let ambiguous = new_unit(
        &profile,
        "canterlot-worker",
        "7777777777777777",
        "test-integration-test-lease",
        "[]",
        40,
    );
    fs::write(
        ambiguous.join("fingerprint/test-lib-canterlot_worker.json"),
        "{}",
    )
    .unwrap();
    // Под target/lint/<профиль> правило то же.
    let lint_profile = root.path().join("lint/debug");
    let lint_old = new_unit(
        &lint_profile,
        "canterlot-worker",
        "aaaaaaaaaaaaaaaa",
        "test-integration-test-exec",
        "[]",
        9,
    );
    let lint_fresh = new_unit(
        &lint_profile,
        "canterlot-worker",
        "bbbbbbbbbbbbbbbb",
        "test-integration-test-exec",
        "[]",
        1,
    );

    let output = Command::new("bash")
        .arg(repo().join("scripts/prune-target.sh"))
        .arg(root.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    assert!(!old.exists(), "the older copy goes as a whole directory");
    assert!(!bin_old.exists());
    assert!(!lint_old.exists());
    for kept in [
        &fresh,
        &bin_fresh,
        &lib_old,
        &lib_fresh,
        &script_old,
        &script_fresh,
        &ambiguous,
        &lint_fresh,
    ] {
        assert!(kept.exists(), "{} must stay", kept.display());
    }
}
