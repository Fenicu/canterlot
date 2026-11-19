//! Разовый импорт памяти Claude: план (манифест) и пакет. Читается только очищенное дерево
//! `tests/fixtures/claude_memory/` или его копия во временном каталоге.

use std::{
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

use canterlot_worker::memory_import::{BundleError, DirBlock, Manifest, PlanOptions, bundle, plan};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude_memory")
}

fn options() -> PlanOptions {
    PlanOptions {
        work_roots: vec![PathBuf::from("/h/work")],
    }
}

/// Копия дерева: тесты дописывают в неё ссылки и большие файлы, не трогая образец.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn scratch() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("projects");
    copy_tree(&fixture(), &root);
    (dir, root)
}

fn block<'a>(manifest: &'a Manifest, source: &str) -> &'a DirBlock {
    manifest
        .dir
        .iter()
        .find(|block| block.source == source)
        .unwrap_or_else(|| panic!("нет блока {source}"))
}

fn skip_of(block: &DirBlock, name: &str) -> String {
    block
        .file
        .iter()
        .find(|file| file.path == format!("memory/{name}"))
        .unwrap_or_else(|| panic!("нет файла {name}"))
        .skip
        .clone()
}

#[test]
fn plan_decodes_dir_names_and_marks_ambiguous() {
    let manifest = plan(&fixture(), &options()).unwrap();

    let work = block(&manifest, "-h-work-thing");
    assert!(work.ambiguous);
    assert!(work.candidates.contains(&"/h/work/thing".to_owned()));
    assert!(work.candidates.contains(&"/h/work-thing".to_owned()));
    assert_eq!(work.decoded_path, "/h/work/thing");
    assert_eq!(work.domain, "work");
    assert_eq!(work.project, "/h/work/thing");

    let personal = block(&manifest, "-h-myprojects-app");
    assert!(personal.ambiguous);
    assert_eq!(personal.domain, "personal");
    assert_eq!(personal.decoded_path, "/h/myprojects/app");

    let single = block(&manifest, "-solo");
    assert!(!single.ambiguous);
    assert_eq!(single.decoded_path, "/solo");
    assert_eq!(single.domain, "personal");

    // Манифест переживает запись в TOML: владелец правит именно его.
    let again = Manifest::from_toml(&manifest.to_toml().unwrap()).unwrap();
    assert_eq!(again, manifest);
}

#[test]
fn plan_skips_credential_names_secret_bodies_large_and_memory_md() {
    let (_dir, root) = scratch();
    fs::write(
        root.join("-h-work-thing/memory/huge_log.md"),
        "строка заметки\n".repeat(900),
    )
    .unwrap();

    let manifest = plan(&root, &options()).unwrap();
    let work = block(&manifest, "-h-work-thing");

    assert_eq!(skip_of(work, "MEMORY.md"), "index_file");
    assert_eq!(skip_of(work, "reference_all.md"), "credential_name");
    assert_eq!(
        skip_of(work, "reference_credentials_notes.md"),
        "credential_name"
    );
    assert_eq!(skip_of(work, "huge_log.md"), "too_large");
    // Невинное имя, токен в теле: пропуск виден в манифесте, класс без значения.
    assert_eq!(skip_of(work, "project_notes.md"), "looks_like_secret");
    assert_eq!(skip_of(work, "feedback_testing.md"), "");
    let text = manifest.to_toml().unwrap();
    assert!(
        !text.contains("ghp_"),
        "значение секрета не попадает в манифест"
    );
}

#[test]
fn plan_does_not_follow_symlinks() {
    let (dir, root) = scratch();
    let outside = dir.path().join("outside");
    fs::create_dir_all(outside.join("memory")).unwrap();
    fs::write(outside.join("memory/stolen.md"), "чужое содержимое").unwrap();
    fs::write(dir.path().join("target.md"), "чужой файл").unwrap();
    // Файл-ссылка внутри каталога памяти, каталог-ссылка среди проектов и сама `memory`.
    symlink(
        dir.path().join("target.md"),
        root.join("-h-work-thing/memory/linked.md"),
    )
    .unwrap();
    symlink(&outside, root.join("-h-linked")).unwrap();
    fs::create_dir_all(root.join("-h-memlink")).unwrap();
    symlink(outside.join("memory"), root.join("-h-memlink/memory")).unwrap();

    let manifest = plan(&root, &options()).unwrap();

    let sources: Vec<&str> = manifest.dir.iter().map(|b| b.source.as_str()).collect();
    assert!(!sources.contains(&"-h-linked"), "{sources:?}");
    assert!(
        manifest
            .dir
            .iter()
            .filter(|b| b.source == "-h-memlink")
            .all(|b| b.file.is_empty()),
        "{sources:?}"
    );
    let work = block(&manifest, "-h-work-thing");
    assert!(work.file.iter().all(|file| !file.path.contains("linked")));
    let everything = manifest.to_toml().unwrap();
    assert!(!everything.contains("stolen"));
}

#[test]
fn bundle_refuses_hash_mismatch() {
    let (_dir, root) = scratch();
    let manifest = plan(&root, &options()).unwrap();
    fs::write(
        root.join("-h-work-thing/memory/feedback_testing.md"),
        "---\ndescription: подменено\n---\n\nдругое тело",
    )
    .unwrap();

    let error = bundle(&manifest, "b-1", "2026-03-06T10:00:00Z").unwrap_err();

    assert!(
        matches!(&error, BundleError::HashMismatch { path } if path.ends_with("feedback_testing.md")),
        "{error:?}"
    );
}

#[test]
fn bundle_contains_only_listed_unskipped_files() {
    let (_dir, root) = scratch();
    let manifest = plan(&root, &options()).unwrap();
    // Файл, появившийся после плана, в манифесте не значится и в пакет не идёт.
    fs::write(root.join("-h-work-thing/memory/late.md"), "поздний файл").unwrap();

    let bundle = bundle(&manifest, "b-7", "2026-03-06T10:00:00Z").unwrap();

    let mut sources: Vec<&str> = bundle
        .entries
        .iter()
        .map(|e| e.source_path.as_str())
        .collect();
    sources.sort_unstable();
    assert_eq!(
        sources,
        [
            "-h-myprojects-app/memory/user_prefs.md",
            "-h-work-thing/memory/feedback_testing.md",
            "-solo/memory/plain.md",
        ]
    );
    let json = serde_json::to_string(&bundle).unwrap();
    assert!(!json.contains("ghp_"), "тело с токеном в пакет не попадает");
    assert!(!json.contains("late.md"));
    assert_eq!(bundle.bundle_id, "b-7");
    let entry = bundle
        .entries
        .iter()
        .find(|e| e.source_path.ends_with("feedback_testing.md"))
        .unwrap();
    assert_eq!(entry.domain, "work");
    assert_eq!(entry.project.as_deref(), Some("/h/work/thing"));
    assert_eq!(entry.kind, "feedback");
    assert_eq!(
        entry.subject,
        "Интеграционные тесты идут в настоящую базу, без моков"
    );
    assert!(entry.body.starts_with("Интеграционные тесты проекта"));
    assert!(!entry.body.contains("---"), "заголовок не остаётся в теле");
    assert_eq!(entry.sha256.len(), 64);
    assert!(entry.updated_at.starts_with("20"), "{}", entry.updated_at);
}

#[test]
fn frontmatter_maps_description_type_and_falls_back() {
    let manifest = plan(&fixture(), &options()).unwrap();

    let prefs = block(&manifest, "-h-myprojects-app")
        .file
        .iter()
        .find(|f| f.path.ends_with("user_prefs.md"))
        .unwrap();
    assert_eq!(prefs.kind, "user", "тип из metadata.type");
    assert_eq!(prefs.subject, "Ответы владельцу по-русски и коротко");

    let plain = block(&manifest, "-solo").file.first().unwrap();
    assert_eq!(plain.kind, "reference", "без типа: reference");
    assert!(
        plain.subject.starts_with("Файл без заголовка"),
        "{}",
        plain.subject
    );
}
