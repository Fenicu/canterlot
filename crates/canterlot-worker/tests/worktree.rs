#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use canterlot_proto::worker::v1::{RemoveWorktree, WorktreeSpec};
use canterlot_worker::worktree::{prepare, remove};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.test"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap()
        .status
        .success()
}

struct Fixture {
    _dir: TempDir,
    source: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        let root = dir.path().join("worktrees");
        fs::create_dir_all(source.join("src")).unwrap();
        fs::create_dir_all(&root).unwrap();
        let (source, root) = (
            fs::canonicalize(source).unwrap(),
            fs::canonicalize(root).unwrap(),
        );
        git(&source, &["init", "-q", "-b", "main"]);
        fs::write(source.join("src/a.txt"), "alpha\n").unwrap();
        fs::write(source.join("b.txt"), "beta\n").unwrap();
        git(&source, &["add", "-A"]);
        git(&source, &["commit", "-q", "-m", "first"]);
        Fixture {
            _dir: dir,
            source,
            root,
        }
    }

    fn target(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn spec(&self, branch: &str) -> WorktreeSpec {
        WorktreeSpec {
            source_workdir: self.source.to_string_lossy().into_owned(),
            branch: branch.into(),
            base: "HEAD".into(),
        }
    }

    fn removal(&self, target: &Path, branch: &str, force: bool) -> RemoveWorktree {
        RemoveWorktree {
            source_workdir: self.source.to_string_lossy().into_owned(),
            path: target.to_string_lossy().into_owned(),
            branch: branch.into(),
            force,
        }
    }
}

/// Содержимое каталога с байтами файлов; `skip` пропускает первый компонент относительного пути.
fn snapshot(root: &Path, skip: &str) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, skip: &str, into: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if relative == skip {
                continue;
            }
            if path.is_dir() {
                into.insert(format!("{relative}/"), Vec::new());
                walk(root, &path, skip, into);
            } else {
                into.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    let mut into = BTreeMap::new();
    walk(root, root, skip, &mut into);
    into
}

#[tokio::test]
async fn prepare_creates_worktree_outside_repo() {
    let fixture = Fixture::new();
    let tree_before = snapshot(&fixture.source, ".git");
    let git_before = snapshot(&fixture.source.join(".git"), "");
    let target = fixture.target("c1");
    prepare(
        &target,
        &fixture.source,
        &fixture.spec("canterlot/0190abcd"),
    )
    .await
    .unwrap();
    assert!(!target.starts_with(&fixture.source));
    assert_eq!(
        fs::read_to_string(target.join("src/a.txt")).unwrap(),
        "alpha\n"
    );
    assert_eq!(
        git(&target, &["symbolic-ref", "--short", "HEAD"]),
        "canterlot/0190abcd"
    );
    assert_eq!(
        git(&target, &["rev-parse", "HEAD"]),
        git(&fixture.source, &["rev-parse", "HEAD"])
    );
    assert_eq!(snapshot(&fixture.source, ".git"), tree_before);
    let git_after = snapshot(&fixture.source.join(".git"), "");
    for (path, bytes) in &git_before {
        if path == "HEAD" || path == "index" || path == "config" || path == "packed-refs" {
            assert_eq!(git_after.get(path), Some(bytes), "{path} changed");
        }
    }
    let added: Vec<_> = git_after
        .keys()
        .filter(|path| !git_before.contains_key(*path))
        .collect();
    assert!(
        added.iter().all(|path| path.starts_with("worktrees/")
            || path.starts_with("refs/heads/canterlot")
            || path.starts_with("logs/refs/heads/canterlot")
            || *path == "logs/"
            || *path == "logs/refs/"
            || *path == "logs/refs/heads/"
            || *path == "refs/heads/canterlot/"),
        "unexpected additions: {added:?}"
    );
    assert!(added.iter().any(|path| path.starts_with("worktrees/")));
}

#[tokio::test]
async fn prepare_is_idempotent() {
    let fixture = Fixture::new();
    let target = fixture.target("c2");
    let spec = fixture.spec("canterlot/0190abce");
    prepare(&target, &fixture.source, &spec).await.unwrap();
    fs::write(target.join("work.txt"), "in progress\n").unwrap();
    prepare(&target, &fixture.source, &spec).await.unwrap();
    assert_eq!(
        fs::read_to_string(target.join("work.txt")).unwrap(),
        "in progress\n"
    );
    let list = git(&fixture.source, &["worktree", "list", "--porcelain"]);
    assert_eq!(list.matches("worktree ").count(), 2);
}

#[tokio::test]
async fn prepare_refuses_a_foreign_existing_directory() {
    let fixture = Fixture::new();
    let target = fixture.target("c3");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("x"), "x").unwrap();
    let reason = prepare(
        &target,
        &fixture.source,
        &fixture.spec("canterlot/0190abcf"),
    )
    .await
    .unwrap_err();
    assert_eq!(reason, "worktree_failed");
    assert_eq!(fs::read_to_string(target.join("x")).unwrap(), "x");
    let other = Fixture::new();
    let foreign = fixture.target("c4");
    prepare(&foreign, &other.source, &other.spec("canterlot/0190abd0"))
        .await
        .unwrap();
    assert_eq!(
        prepare(
            &foreign,
            &fixture.source,
            &fixture.spec("canterlot/0190abd0")
        )
        .await
        .unwrap_err(),
        "worktree_failed"
    );
}

#[tokio::test]
async fn prepare_rejects_non_git_source() {
    let fixture = Fixture::new();
    let plain = TempDir::new().unwrap();
    let plain = fs::canonicalize(plain.path()).unwrap();
    let spec = WorktreeSpec {
        source_workdir: plain.to_string_lossy().into_owned(),
        branch: "canterlot/0190abd1".into(),
        base: "HEAD".into(),
    };
    let target = fixture.target("c5");
    assert_eq!(
        prepare(&target, &plain, &spec).await.unwrap_err(),
        "worktree_not_git"
    );
    assert!(!target.exists());
}

#[tokio::test]
async fn prepare_reports_unknown_base() {
    let fixture = Fixture::new();
    let mut spec = fixture.spec("canterlot/0190abd2");
    spec.base = "no-such-ref".into();
    let target = fixture.target("c6");
    assert_eq!(
        prepare(&target, &fixture.source, &spec).await.unwrap_err(),
        "worktree_base_unknown"
    );
    spec.base = "--upload-pack=x".into();
    assert_eq!(
        prepare(&target, &fixture.source, &spec).await.unwrap_err(),
        "worktree_base_unknown"
    );
    assert!(!target.exists());
    assert!(!git_ok(
        &fixture.source,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            "refs/heads/canterlot/0190abd2"
        ]
    ));
    let empty = TempDir::new().unwrap();
    let empty = fs::canonicalize(empty.path()).unwrap();
    git(&empty, &["init", "-q", "-b", "main"]);
    let spec = WorktreeSpec {
        source_workdir: empty.to_string_lossy().into_owned(),
        branch: "canterlot/0190abd3".into(),
        base: "HEAD".into(),
    };
    assert_eq!(
        prepare(&fixture.target("c7"), &empty, &spec)
            .await
            .unwrap_err(),
        "worktree_base_unknown"
    );
}

#[tokio::test]
async fn prepare_uses_the_requested_base_and_refuses_foreign_branch_names() {
    let fixture = Fixture::new();
    let first = git(&fixture.source, &["rev-parse", "HEAD"]);
    fs::write(fixture.source.join("b.txt"), "second\n").unwrap();
    git(&fixture.source, &["commit", "-q", "-am", "second"]);
    let mut spec = fixture.spec("canterlot/0190abd4");
    spec.base = first.clone();
    let target = fixture.target("c8");
    prepare(&target, &fixture.source, &spec).await.unwrap();
    assert_eq!(git(&target, &["rev-parse", "HEAD"]), first);
    let mut main = fixture.spec("main");
    main.base = "HEAD".into();
    assert_eq!(
        prepare(&fixture.target("c9"), &fixture.source, &main)
            .await
            .unwrap_err(),
        "worktree_failed"
    );
    spec.branch = "canterlot/0190abd4".into();
    assert_eq!(
        prepare(&fixture.target("c10"), &fixture.source, &spec)
            .await
            .unwrap_err(),
        "worktree_failed",
        "an existing branch is never taken over"
    );
}

#[tokio::test]
async fn remove_refuses_dirty_without_force() {
    let fixture = Fixture::new();
    let target = fixture.target("r1");
    let spec = fixture.spec("canterlot/0190abe0");
    prepare(&target, &fixture.source, &spec).await.unwrap();
    fs::write(target.join("new.txt"), "untracked\n").unwrap();
    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&target, &spec.branch, false),
    )
    .await;
    assert_eq!((done.outcome, done.detail), ("dirty", "dirty"));
    assert!(target.join("new.txt").exists());
    fs::remove_file(target.join("new.txt")).unwrap();
    fs::write(target.join("b.txt"), "edited\n").unwrap();
    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&target, &spec.branch, false),
    )
    .await;
    assert_eq!(done.outcome, "dirty");
    assert!(target.exists());
}

#[tokio::test]
async fn remove_refuses_commits_that_only_a_detached_head_reaches() {
    let fixture = Fixture::new();
    let target = fixture.target("r-detached");
    let spec = fixture.spec("canterlot/0190abe5");
    prepare(&target, &fixture.source, &spec).await.unwrap();
    git(&target, &["checkout", "-q", "--detach"]);
    fs::write(target.join("work.txt"), "work\n").unwrap();
    git(&target, &["add", "-A"]);
    git(&target, &["commit", "-q", "-m", "on no branch"]);
    let commit = git(&target, &["rev-parse", "HEAD"]);

    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&target, &spec.branch, false),
    )
    .await;
    assert_eq!((done.outcome, done.detail), ("dirty", "dirty"));
    assert!(target.exists(), "the only way to reach the commit stays");
    assert!(git_ok(&fixture.source, &["cat-file", "-e", &commit]));

    let forced = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&target, &spec.branch, true),
    )
    .await;
    assert_eq!(forced.outcome, "removed", "force is the explicit discard");
    assert!(!target.exists());
}

#[tokio::test]
async fn remove_takes_a_detached_head_with_nothing_of_its_own() {
    let fixture = Fixture::new();
    let target = fixture.target("r-detached-clean");
    let spec = fixture.spec("canterlot/0190abe6");
    prepare(&target, &fixture.source, &spec).await.unwrap();
    git(&target, &["checkout", "-q", "--detach"]);
    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&target, &spec.branch, false),
    )
    .await;
    assert_eq!(done.outcome, "removed");
    assert!(!target.exists());
}

#[tokio::test]
async fn remove_leaves_commits_on_the_childs_own_branch_alone() {
    let fixture = Fixture::new();
    let target = fixture.target("r-own-branch");
    let spec = fixture.spec("canterlot/0190abe7");
    prepare(&target, &fixture.source, &spec).await.unwrap();
    git(&target, &["checkout", "-q", "-b", "feature"]);
    fs::write(target.join("work.txt"), "work\n").unwrap();
    git(&target, &["add", "-A"]);
    git(&target, &["commit", "-q", "-m", "on its own branch"]);

    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&target, &spec.branch, false),
    )
    .await;
    assert_eq!(done.outcome, "removed");
    assert!(!done.branch_kept);
    assert_eq!(
        git(&fixture.source, &["log", "-1", "--format=%s", "feature"]),
        "on its own branch"
    );
}

#[tokio::test]
async fn remove_force_keeps_branch_with_commits() {
    let fixture = Fixture::new();
    let target = fixture.target("r2");
    let spec = fixture.spec("canterlot/0190abe1");
    prepare(&target, &fixture.source, &spec).await.unwrap();
    fs::write(target.join("done.txt"), "done\n").unwrap();
    git(&target, &["add", "-A"]);
    git(&target, &["commit", "-q", "-m", "work"]);
    fs::write(target.join("dirty.txt"), "dirty\n").unwrap();
    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&target, &spec.branch, true),
    )
    .await;
    assert_eq!(done.outcome, "removed");
    assert!(done.branch_kept);
    assert!(!target.exists());
    assert!(git_ok(
        &fixture.source,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            "refs/heads/canterlot/0190abe1"
        ]
    ));
    assert_eq!(
        git(
            &fixture.source,
            &["log", "-1", "--format=%s", "canterlot/0190abe1"]
        ),
        "work"
    );
}

#[tokio::test]
async fn remove_deletes_branch_without_unique_commits() {
    let fixture = Fixture::new();
    let tree_before = snapshot(&fixture.source, ".git");
    let target = fixture.target("r3");
    let spec = fixture.spec("canterlot/0190abe2");
    prepare(&target, &fixture.source, &spec).await.unwrap();
    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&target, &spec.branch, false),
    )
    .await;
    assert_eq!((done.outcome, done.detail), ("removed", ""));
    assert!(!done.branch_kept);
    assert!(!target.exists());
    assert!(!git_ok(
        &fixture.source,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            "refs/heads/canterlot/0190abe2"
        ]
    ));
    assert_eq!(snapshot(&fixture.source, ".git"), tree_before);
    assert_eq!(
        git(&fixture.source, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
}

#[tokio::test]
async fn remove_deletes_a_branch_already_merged_elsewhere() {
    let fixture = Fixture::new();
    let target = fixture.target("r4");
    let spec = fixture.spec("canterlot/0190abe3");
    prepare(&target, &fixture.source, &spec).await.unwrap();
    fs::write(target.join("c.txt"), "c\n").unwrap();
    git(&target, &["add", "-A"]);
    git(&target, &["commit", "-q", "-m", "unique"]);
    git(
        &fixture.source,
        &["merge", "-q", "--ff-only", "canterlot/0190abe3"],
    );
    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&target, &spec.branch, false),
    )
    .await;
    assert_eq!(done.outcome, "removed");
    assert!(
        !done.branch_kept,
        "commits reachable from main are not unique"
    );
}

#[tokio::test]
async fn remove_refuses_path_outside_root() {
    let fixture = Fixture::new();
    let elsewhere = TempDir::new().unwrap();
    let outside = fs::canonicalize(elsewhere.path()).unwrap().join("wt");
    let spec = fixture.spec("canterlot/0190abe4");
    prepare(&outside, &fixture.source, &spec).await.unwrap();
    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&outside, &spec.branch, true),
    )
    .await;
    assert_eq!((done.outcome, done.detail), ("failed", "path_outside_root"));
    assert!(outside.join("b.txt").exists());
    let inside = fixture.target("r5");
    prepare(
        &inside,
        &fixture.source,
        &fixture.spec("canterlot/0190abe5"),
    )
    .await
    .unwrap();
    let dotdot = fixture.root.join("r5/../r5");
    let nested = fixture.root.join("r5/src");
    let link = fixture.target("link");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    for path in [
        dotdot,
        nested,
        link,
        fixture.root.clone(),
        PathBuf::from("/"),
        PathBuf::from("relative"),
    ] {
        let done = remove(
            &fixture.root,
            &fixture.source,
            &fixture.removal(&path, "canterlot/0190abe5", true),
        )
        .await;
        assert_eq!(done.outcome, "failed", "{path:?}");
    }
    assert!(inside.join("b.txt").exists());
    assert!(outside.join("b.txt").exists());
}

#[tokio::test]
async fn remove_refuses_foreign_branches_and_plain_directories() {
    let fixture = Fixture::new();
    let target = fixture.target("r6");
    prepare(
        &target,
        &fixture.source,
        &fixture.spec("canterlot/0190abe6"),
    )
    .await
    .unwrap();
    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&target, "main", true),
    )
    .await;
    assert_eq!(
        (done.outcome, done.detail),
        ("failed", "branch_not_allowed")
    );
    assert!(target.exists());
    let plain = fixture.target("r7");
    fs::create_dir_all(&plain).unwrap();
    fs::write(plain.join("keep"), "keep").unwrap();
    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&plain, "canterlot/0190abe7", true),
    )
    .await;
    assert_eq!((done.outcome, done.detail), ("failed", "not_a_worktree"));
    assert!(plain.join("keep").exists());
}

#[tokio::test]
async fn remove_is_idempotent() {
    let fixture = Fixture::new();
    let target = fixture.target("r8");
    let spec = fixture.spec("canterlot/0190abe8");
    prepare(&target, &fixture.source, &spec).await.unwrap();
    let request = fixture.removal(&target, &spec.branch, false);
    let first = remove(&fixture.root, &fixture.source, &request).await;
    let second = remove(&fixture.root, &fixture.source, &request).await;
    assert_eq!(first.outcome, "removed");
    assert_eq!(second.outcome, "removed");
    assert!(!second.branch_kept);
    git(&fixture.source, &["worktree", "prune"]);
    // Каталог исчез сам: метаданные подчищаются, а ветка без своих коммитов уходит.
    let lost = fixture.target("r9");
    let spec = fixture.spec("canterlot/0190abe9");
    prepare(&lost, &fixture.source, &spec).await.unwrap();
    fs::remove_dir_all(&lost).unwrap();
    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&lost, &spec.branch, false),
    )
    .await;
    assert_eq!(done.outcome, "removed");
    assert!(!done.branch_kept);
    assert_eq!(
        git(&fixture.source, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
}

#[tokio::test]
async fn removing_a_lost_copy_leaves_foreign_stale_entries_alone() {
    let fixture = Fixture::new();
    let foreign = fixture.target("owner-own");
    git(
        &fixture.source,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "owner-branch",
            foreign.to_str().unwrap(),
        ],
    );
    fs::remove_dir_all(&foreign).unwrap();
    let lost = fixture.target("r10");
    let spec = fixture.spec("canterlot/0190abea");
    prepare(&lost, &fixture.source, &spec).await.unwrap();
    fs::remove_dir_all(&lost).unwrap();
    let done = remove(
        &fixture.root,
        &fixture.source,
        &fixture.removal(&lost, &spec.branch, false),
    )
    .await;
    assert_eq!(done.outcome, "removed");
    let listed = git(&fixture.source, &["worktree", "list", "--porcelain"]);
    assert!(listed.contains("owner-own"), "{listed}");
    assert!(!listed.contains("r10"), "{listed}");
}
