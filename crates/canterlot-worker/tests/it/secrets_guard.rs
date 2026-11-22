//! Запрет боевого корня секретов в тестах воркера: каждый `WorkerConfig` в
//! тестах обязан указывать временную базу на tmpfs, иначе тестовый воркер
//! со свежим `data_dir` уберёт живые секреты боевого
//! `$XDG_RUNTIME_DIR/canterlot-secrets`.

use std::path::{Path, PathBuf};

fn rs_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_default_secret_base_in_worker_tests() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let needles = ["secrets: Default::default()", "SecretConfig::default()"];
    let mut offenders = Vec::new();
    for dir in ["tests/it", "src"] {
        let mut files = Vec::new();
        rs_files(&root.join(dir), &mut files);
        files.sort();
        for file in files {
            if file
                .file_name()
                .is_some_and(|name| name == "secrets_guard.rs")
            {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            for (index, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                if needles.iter().any(|needle| line.contains(needle)) {
                    offenders.push(format!(
                        "{}:{}: {}",
                        file.display(),
                        index + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "боевой корень секретов в тестах воркера:\n{}",
        offenders.join("\n")
    );
}
