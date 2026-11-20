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

fn entry_of<'a>(block: &'a DirBlock, name: &str) -> &'a canterlot_worker::memory_import::FileEntry {
    block
        .file
        .iter()
        .find(|file| file.path == format!("memory/{name}"))
        .unwrap_or_else(|| panic!("нет файла {name}"))
}

#[test]
fn skipped_manifest_entry_carries_no_content() {
    let (_dir, root) = scratch();
    // Без заголовка: первая строка стала бы темой, а в ней токен.
    fs::write(
        root.join("-solo/memory/leaky.md"),
        "ghp_abcdefghijklmnopqrstuvwx0123\nобычная строка\n",
    )
    .unwrap();

    let manifest = plan(&root, &options()).unwrap();

    let solo = block(&manifest, "-solo");
    let leaky = entry_of(solo, "leaky.md");
    assert_eq!(leaky.skip, "looks_like_secret");
    assert_eq!(leaky.subject, "");
    assert_eq!(leaky.kind, "");
    assert_eq!(leaky.sha256, "");
    let text = manifest.to_toml().unwrap();
    assert!(!text.contains("ghp_"), "{text}");
    assert!(!text.contains("abcdefghijklmnop"), "{text}");
    // Имя с учётными данными: содержимое не читается, тема пуста.
    let creds = entry_of(
        block(&manifest, "-h-work-thing"),
        "reference_credentials_notes.md",
    );
    assert_eq!(creds.skip, "credential_name");
    assert_eq!((creds.subject.as_str(), creds.kind.as_str()), ("", ""));
    assert!(!text.contains("Где лежат учётные данные"), "{text}");
}

#[test]
fn bundle_ignores_a_cleared_skip_on_forbidden_names() {
    let (_dir, root) = scratch();
    let mut manifest = plan(&root, &options()).unwrap();
    for block in &mut manifest.dir {
        for file in &mut block.file {
            if file.path.ends_with("MEMORY.md")
                || file.path.ends_with("reference_credentials_notes.md")
                || file.path.ends_with("reference_all.md")
            {
                file.skip.clear();
            }
        }
    }

    let bundle = bundle(&manifest, "b-9", "2026-03-06T10:00:00Z").unwrap();

    let json = serde_json::to_string(&bundle).unwrap();
    for name in ["MEMORY.md", "reference_credentials_notes", "reference_all"] {
        assert!(!json.contains(name), "{name} попал в пакет");
    }
    assert_eq!(bundle.entries.len(), 3);
}

#[test]
fn symlinked_root_and_symlinked_memory_dir_are_refused() {
    let (dir, root) = scratch();
    let linked_root = dir.path().join("projects-link");
    symlink(&root, &linked_root).unwrap();

    let error = plan(&linked_root, &options()).unwrap_err();
    assert_eq!(error.to_string(), "root is a symbolic link");

    let manifest = plan(&root, &options()).unwrap();
    let mut through_link = manifest.clone();
    through_link.root = linked_root.to_string_lossy().into_owned();
    let error = bundle(&through_link, "b-1", "2026-03-06T10:00:00Z").unwrap_err();
    assert!(matches!(error, BundleError::Symlink { .. }), "{error:?}");
    assert!(error.to_string().contains("symbolic link"), "{error}");

    // `memory` подменена ссылкой уже после плана.
    let moved = dir.path().join("moved-memory");
    fs::rename(root.join("-solo/memory"), &moved).unwrap();
    symlink(&moved, root.join("-solo/memory")).unwrap();
    let error = bundle(&manifest, "b-1", "2026-03-06T10:00:00Z").unwrap_err();
    assert!(matches!(error, BundleError::Symlink { .. }), "{error:?}");
    // План такой каталог не читает вовсе.
    let again = plan(&root, &options()).unwrap();
    assert!(again.dir.iter().all(|b| b.source != "-solo"));
}

fn framed(front_bytes: usize, body_bytes: usize) -> String {
    let mut text = String::from("---\ndescription: Длинный заголовок\n");
    let mut n = 0;
    while text.len() < front_bytes {
        text.push_str(&format!("note{n}: value value value\n"));
        n += 1;
    }
    text.push_str("---\n\n");
    text.push_str(&"word ".repeat(body_bytes / 5));
    text
}

#[test]
fn body_limit_applies_to_the_parsed_body_not_the_file() {
    let (_dir, root) = scratch();
    fs::write(root.join("-solo/memory/framed.md"), framed(2048, 7 * 1024)).unwrap();
    fs::write(root.join("-solo/memory/fat.md"), framed(64, 9 * 1024)).unwrap();

    let manifest = plan(&root, &options()).unwrap();

    let solo = block(&manifest, "-solo");
    let framed_entry = entry_of(solo, "framed.md");
    assert!(framed_entry.size > 8 * 1024, "файл целиком больше предела");
    assert_eq!(framed_entry.skip, "");
    assert_eq!(entry_of(solo, "fat.md").skip, "too_large");

    // Пакет собирается по манифесту: тело в пределе берётся, лишнее отказ.
    let mut wanted = manifest.clone();
    let fat = fs::read(root.join("-solo/memory/fat.md")).unwrap();
    for block in &mut wanted.dir {
        for file in &mut block.file {
            if file.path.ends_with("fat.md") {
                use sha2::{Digest, Sha256};
                file.skip.clear();
                file.sha256 = Sha256::digest(&fat)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
            }
        }
    }
    let error = bundle(&wanted, "b-1", "2026-03-06T10:00:00Z").unwrap_err();
    assert!(matches!(error, BundleError::TooLarge { .. }), "{error:?}");
    let bundle = bundle(&manifest, "b-1", "2026-03-06T10:00:00Z").unwrap();
    assert!(
        bundle
            .entries
            .iter()
            .any(|e| e.source_path.ends_with("framed.md") && e.body.len() <= 8 * 1024)
    );
}
