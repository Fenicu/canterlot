#![forbid(unsafe_code)]

use std::{fs, path::Path, process::Command};

use canterlot_worker::supervisor::git_facts::{self};
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
