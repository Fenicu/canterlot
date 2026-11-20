#![forbid(unsafe_code)]

//! Claude Code «полностью игнорируется» воркером без таблицы `[claude_code]`: возможность
//! историй не объявляется, прямой запрос истории отклоняется, обзор инструкций владельца
//! не заглядывает в каталог Claude Code, опрос лимитов `claude` не заводится.
//! Все пути ниже — во временных каталогах, настоящий домашний каталог не трогается.

use canterlot_proto::worker::v1::ListClaudeHistories;
use canterlot_worker::{
    config::WorkerConfig,
    machine::{claude_history, owner_instructions},
    session::{advertised_capabilities, claude_histories_reply, owner_roots, polled_providers},
};
use std::{collections::HashSet, ffi::OsString, fs, path::Path};
use uuid::Uuid;

fn config(extra: &str) -> WorkerConfig {
    let id = Uuid::now_v7();
    toml::from_str(&format!(
        r#"
hub_url = "https://hub.test"
ca_cert = "/tmp/ca.pem"
token_file = "/tmp/token"
machine_id = "{id}"
installation_id = "{id}"
restore_generation = "{id}"
data_dir = "/tmp/data"
{extra}
"#
    ))
    .unwrap()
}

/// Каталог Claude Code с одной историей и глобальным CLAUDE.md в `home/.claude`.
fn claude_home(home: &Path, workdir: &str) {
    let dir = home
        .join(".claude/projects")
        .join(claude_history::project_slug(workdir));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(format!("{}.jsonl", Uuid::now_v7())),
        r#"{"type":"user","message":{"role":"user","content":"привет"}}"#,
    )
    .unwrap();
    let memory = home.join(".claude/projects/p1/memory");
    fs::create_dir_all(&memory).unwrap();
    fs::write(memory.join("MEMORY.md"), "# память").unwrap();
    fs::write(home.join(".claude/CLAUDE.md"), "# user").unwrap();
}

#[test]
fn without_the_table_the_histories_capability_is_not_advertised() {
    let ignored = advertised_capabilities(&config("[opencode]\n"));
    assert!(
        !ignored.iter().any(|cap| cap == "claude_histories_v1"),
        "{ignored:?}"
    );
    // Остальные возможности остаются как были.
    assert!(ignored.iter().any(|cap| cap == "owner_instructions_v1"));
    let enabled = advertised_capabilities(&config("[claude_code]\n"));
    assert!(enabled.iter().any(|cap| cap == "claude_histories_v1"));
}

#[test]
fn a_direct_history_request_without_the_table_lists_nothing() {
    let home = tempfile::tempdir().unwrap();
    let workdir = "/h/u/proj";
    claude_home(home.path(), workdir);
    let request = || ListClaudeHistories {
        request_id: "r1".into(),
        workdir: workdir.into(),
    };
    let claude_dir = home.path().join(".claude");
    let listed = claude_histories_reply(true, request(), HashSet::new(), claude_dir.clone());
    assert_eq!(listed.items.len(), 1, "the fixture must be listable");
    let refused = claude_histories_reply(false, request(), HashSet::new(), claude_dir);
    assert_eq!(refused.request_id, "r1");
    assert!(refused.items.is_empty());
}

#[test]
fn the_owner_overview_skips_the_claude_directory_without_the_table() {
    let home = tempfile::tempdir().unwrap();
    claude_home(home.path(), "/h/u/proj");
    let env: Vec<(OsString, OsString)> = Vec::new();

    let with = owner_roots(&config("[claude_code]\n"), home.path(), &env);
    let overview = owner_instructions::overview(&with);
    assert!(
        overview
            .files
            .iter()
            .any(|f| f.kind == "claude_user" && f.present)
    );
    assert_eq!(overview.projects.len(), 1, "the fixture memory is listed");

    let without = owner_roots(&config("[opencode]\n"), home.path(), &env);
    let overview = owner_instructions::overview(&without);
    assert!(
        overview.files.iter().all(|f| !f.kind.starts_with("claude")),
        "{:?}",
        overview.files
    );
    assert!(overview.projects.is_empty());
    // Прямое чтение файла Claude Code тоже отказывает.
    let file = owner_instructions::read_one("claude_user", "", "", &without, "r1");
    assert!(!file.present && file.content.is_empty());
    let memory = owner_instructions::read_one("memory_index", "p1", "", &without, "r2");
    assert!(!memory.present && memory.content.is_empty());
}

#[test]
fn claude_limits_are_not_polled_without_the_table() {
    assert!(!polled_providers(&config("[opencode]\n")).contains(&"claude"));
    assert!(polled_providers(&config("[claude_code]\n[opencode]\n")).contains(&"claude"));
    assert_eq!(polled_providers(&config("")), Vec::<&str>::new());
}
