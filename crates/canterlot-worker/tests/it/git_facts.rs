#![forbid(unsafe_code)]

use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use canterlot_worker::supervisor::git_facts::{self, MAX_COMMITS, MAX_FACTS_BYTES, MAX_FILES};
use prost::Message;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

fn repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    fs::write(dir.path().join("base.txt"), "base").expect("write");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "base"]);
    dir
}

#[tokio::test]
async fn git_facts_list_commits_and_files() {
    let dir = repo();
    let before = git(dir.path(), &["rev-parse", "HEAD"]);
    fs::write(dir.path().join("a.txt"), "a").expect("write");
    fs::write(dir.path().join("base.txt"), "changed").expect("write");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "first change"]);
    fs::remove_file(dir.path().join("base.txt")).expect("rm");
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "second change"]);
    let after = git(dir.path(), &["rev-parse", "HEAD"]);

    let facts = git_facts::collect(dir.path(), Some(&before)).await;

    assert!(facts.available);
    assert!(!facts.truncated);
    assert!(!facts.dirty);
    assert_eq!(facts.head_before, before);
    assert_eq!(facts.head_after, after);
    let subjects: Vec<_> = facts.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, ["second change", "first change"]);
    assert_eq!(facts.commits[0].sha, after);
    assert!(
        facts
            .commits
            .iter()
            .all(|commit| commit.committed_at_ms > 0),
        "commit times are read for the stale marker"
    );
    assert!(facts.commits[0].committed_at_ms >= facts.commits[1].committed_at_ms);
    let mut files: Vec<_> = facts
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.status.as_str()))
        .collect();
    files.sort();
    assert_eq!(files, [("a.txt", "A"), ("base.txt", "D")]);
}

#[tokio::test]
async fn git_facts_without_previous_head_lists_no_commits() {
    let dir = repo();
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    let facts = git_facts::collect(dir.path(), None).await;
    assert!(facts.available);
    assert_eq!(facts.head_before, "");
    assert_eq!(facts.head_after, head);
    assert!(facts.commits.is_empty());
    assert!(facts.files.is_empty());
}

#[tokio::test]
async fn git_facts_mark_dirty() {
    let dir = repo();
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    let clean = git_facts::collect(dir.path(), Some(&head)).await;
    assert!(clean.available && !clean.dirty);
    assert!(clean.commits.is_empty() && clean.files.is_empty());

    fs::write(dir.path().join("untracked.txt"), "x").expect("write");
    let dirty = git_facts::collect(dir.path(), Some(&head)).await;
    assert!(dirty.available && dirty.dirty);
    assert_eq!(dirty.head_after, head);
}

#[tokio::test]
async fn git_facts_truncate_at_limits() {
    let dir = repo();
    let before = git(dir.path(), &["rev-parse", "HEAD"]);
    let long_subject = "s".repeat(600);
    for i in 0..(MAX_COMMITS + 5) {
        fs::write(dir.path().join(format!("f{i}.txt")), "x").expect("write");
        git(dir.path(), &["add", "."]);
        git(
            dir.path(),
            &["commit", "-q", "-m", &format!("{i}{long_subject}")],
        );
    }

    let facts = git_facts::collect(dir.path(), Some(&before)).await;

    assert!(facts.available);
    assert!(facts.truncated);
    assert!(facts.commits.len() <= MAX_COMMITS);
    assert!(facts.files.len() <= MAX_FILES);
    assert!(facts.commits.iter().all(|c| c.subject.len() <= 256));
    assert!(facts.encoded_len() <= MAX_FACTS_BYTES);
}

#[tokio::test]
async fn git_facts_clip_long_paths_and_subjects() {
    let dir = repo();
    let before = git(dir.path(), &["rev-parse", "HEAD"]);
    let name = "n".repeat(200);
    let nested = Path::new("d1")
        .join(&name)
        .join(&name)
        .join(&name)
        .join(&name)
        .join(&name)
        .join(&name);
    fs::create_dir_all(dir.path().join(&nested)).expect("mkdir");
    fs::write(dir.path().join(&nested).join("кириллица.txt"), "x").expect("write");
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", &"я".repeat(300)]);

    let facts = git_facts::collect(dir.path(), Some(&before)).await;

    assert!(facts.available);
    assert!(facts.truncated);
    assert_eq!(facts.files.len(), 1);
    assert!(facts.files[0].path.len() <= 1024);
    assert!(facts.commits[0].subject.len() <= 256);
}

#[tokio::test]
async fn git_facts_non_repo_is_unavailable() {
    let dir = TempDir::new().expect("tempdir");
    let facts = git_facts::collect(dir.path(), None).await;
    assert!(!facts.available);
    assert!(facts.commits.is_empty() && facts.files.is_empty());
    assert!(git_facts::head(dir.path()).await.is_none());
}

#[tokio::test]
async fn git_facts_missing_workdir_is_unavailable() {
    let facts = git_facts::collect(Path::new("/nonexistent/canterlot-git-facts"), None).await;
    assert!(!facts.available);
}

#[tokio::test]
async fn git_facts_head_reads_commit() {
    let dir = repo();
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        git_facts::head(dir.path()).await.as_deref(),
        Some(head.as_str())
    );
}

#[tokio::test]
async fn git_facts_timeout_gives_unavailable() {
    let dir = repo();
    let bin = TempDir::new().expect("tempdir");
    let fake = bin.path().join("git");
    fs::write(&fake, "#!/bin/sh\nsleep 30\n").expect("write");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");

    let started = Instant::now();
    let facts = git_facts::collect_with(
        fake.as_os_str(),
        dir.path(),
        Some("deadbeef"),
        Duration::from_millis(300),
    )
    .await;

    assert!(!facts.available);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn git_facts_use_isolated_environment() {
    let dir = repo();
    let bin = TempDir::new().expect("tempdir");
    let out = bin.path().join("env.txt");
    let fake = bin.path().join("git");
    fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho \"$GIT_TERMINAL_PROMPT $GIT_OPTIONAL_LOCKS\" >> {}\nexit 1\n",
            out.display()
        ),
    )
    .expect("write");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("chmod");

    let facts = git_facts::collect_with(
        OsStr::new(fake.as_os_str()),
        dir.path(),
        None,
        Duration::from_secs(2),
    )
    .await;

    assert!(!facts.available);
    let seen = fs::read_to_string(&out).expect("fake git was run");
    assert!(seen.lines().all(|line| line == "0 0"));
}
