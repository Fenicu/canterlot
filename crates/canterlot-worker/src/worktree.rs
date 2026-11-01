//! Изолированные рабочие копии git для делегированных сессий: подготовка перед запуском CLI и
//! уборка после приёма результата. Исходное дерево не меняется, в `.git` добавляются только
//! метаданные копии и одна ветка, наружу ничего не отправляется.

use std::{
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
    process::{Output, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use canterlot_proto::worker::v1::{RemoveWorktree, WorktreeSpec};
use tokio::{process::Command, time::timeout};

/// Общий срок одной операции: медленная распаковка не должна держать запуск бесконечно.
pub const DEADLINE: Duration = Duration::from_secs(120);
const ROLLBACK_DEADLINE: Duration = Duration::from_secs(30);
const BRANCH_PREFIX: &str = "canterlot/";

/// Итог уборки: `outcome` бывает `removed`, `dirty` или `failed`, `detail` короткий код без путей.
#[derive(Debug, PartialEq, Eq)]
pub struct Removed {
    pub outcome: &'static str,
    pub detail: &'static str,
    pub branch_kept: bool,
}

impl Removed {
    pub fn failed(detail: &'static str) -> Removed {
        Removed {
            outcome: "failed",
            detail,
            branch_kept: false,
        }
    }
}

/// Прямой потомок корня рабочих копий: без `..`, ссылок и вложенности. Каталога может ещё не быть.
pub fn resolve_target(root: &Path, path: &Path) -> Option<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return None;
    }
    let root = fs::canonicalize(root).ok()?;
    let name = path.file_name()?;
    if fs::canonicalize(path.parent()?).ok()? != root {
        return None;
    }
    let target = root.join(name);
    match fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_symlink() => None,
        _ => Some(target),
    }
}

async fn git(dir: &Path, args: &[&OsStr]) -> Option<Output> {
    Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_NAMESPACE")
        .env_remove("GIT_PREFIX")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()
}

macro_rules! args {
    ($($arg:expr),* $(,)?) => { &[$(AsRef::<OsStr>::as_ref(&$arg)),*] };
}

async fn git_ok(dir: &Path, args: &[&OsStr]) -> bool {
    git(dir, args)
        .await
        .is_some_and(|output| output.status.success())
}

async fn git_text(dir: &Path, args: &[&OsStr]) -> Option<String> {
    let output = git(dir, args)
        .await
        .filter(|output| output.status.success())?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Общий каталог git рабочего дерева; `None`, если каталог не рабочее дерево git.
async fn common_dir(dir: &Path) -> Option<PathBuf> {
    let text = git_text(
        dir,
        args![
            "rev-parse",
            "--is-inside-work-tree",
            "--path-format=absolute",
            "--git-common-dir"
        ],
    )
    .await?;
    let mut lines = text.lines();
    if lines.next()? != "true" {
        return None;
    }
    fs::canonicalize(lines.next()?).ok()
}

async fn toplevel(dir: &Path) -> Option<PathBuf> {
    fs::canonicalize(git_text(dir, args!["rev-parse", "--show-toplevel"]).await?).ok()
}

/// Каталог — рабочая копия того же репозитория, и сам является её корнем.
async fn is_worktree_of(dir: &Path, common: &Path) -> bool {
    common_dir(dir).await.as_deref() == Some(common)
        && toplevel(dir).await.as_deref() == fs::canonicalize(dir).ok().as_deref()
}

async fn valid_branch(branch: &str) -> bool {
    branch.starts_with(BRANCH_PREFIX)
        && git_ok(
            Path::new("/"),
            args!["check-ref-format", "--branch", branch],
        )
        .await
}

async fn branch_exists(source: &Path, branch: &str) -> bool {
    let name = format!("refs/heads/{branch}");
    git_ok(source, args!["show-ref", "--verify", "--quiet", name]).await
}

/// Готовит копию `target`; повтор над готовой копией того же репозитория ничего не меняет.
pub async fn prepare(
    target: &Path,
    source: &Path,
    spec: &WorktreeSpec,
) -> Result<(), &'static str> {
    let created = AtomicBool::new(false);
    let result = timeout(DEADLINE, prepare_inner(target, source, spec, &created)).await;
    match result {
        Ok(Ok(())) => Ok(()),
        failure => {
            if created.load(Ordering::Relaxed) {
                let _ = timeout(ROLLBACK_DEADLINE, rollback(target, source, &spec.branch)).await;
            }
            match failure {
                Ok(Err(reason)) => Err(reason),
                _ => Err("worktree_failed"),
            }
        }
    }
}

async fn prepare_inner(
    target: &Path,
    source: &Path,
    spec: &WorktreeSpec,
    created: &AtomicBool,
) -> Result<(), &'static str> {
    let common = common_dir(source).await.ok_or("worktree_not_git")?;
    if fs::symlink_metadata(target).is_ok() {
        return if is_worktree_of(target, &common).await {
            Ok(())
        } else {
            Err("worktree_failed")
        };
    }
    if !valid_branch(&spec.branch).await {
        return Err("worktree_failed");
    }
    let base = if spec.base.is_empty() {
        "HEAD"
    } else {
        &spec.base
    };
    let revision = format!("{base}^{{commit}}");
    let commit = if base.starts_with('-') {
        None
    } else {
        git_text(
            source,
            args![
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                revision
            ],
        )
        .await
    }
    .filter(|commit| !commit.is_empty())
    .ok_or("worktree_base_unknown")?;
    if branch_exists(source, &spec.branch).await {
        return Err("worktree_failed");
    }
    created.store(true, Ordering::Relaxed);
    let added = git_ok(
        source,
        args![
            "worktree",
            "add",
            "--quiet",
            "-b",
            spec.branch,
            "--",
            target,
            commit
        ],
    )
    .await;
    if added {
        Ok(())
    } else {
        Err("worktree_failed")
    }
}

/// Откат недоделанной копии: каталога и ветки до начала не было, они наши.
async fn rollback(target: &Path, source: &Path, branch: &str) {
    if !git_ok(source, args!["worktree", "remove", "--force", "--", target]).await {
        let _ = fs::remove_dir_all(target);
    }
    forget_missing(source, target).await;
    let _ = git_ok(source, args!["branch", "-D", "--", branch]).await;
}

/// Метаданные пропавшей копии убирает только её собственная запись: `prune` чистил бы
/// и чужие записи репозитория, например рабочие копии владельца с удалёнными каталогами.
async fn forget_missing(source: &Path, target: &Path) {
    let _ = git_ok(source, args!["worktree", "remove", "--force", "--", target]).await;
}

/// Убирает копию, а ветку только если в ней нет коммитов, которых нет больше нигде.
pub async fn remove(root: &Path, source: &Path, request: &RemoveWorktree) -> Removed {
    match timeout(DEADLINE, remove_inner(root, source, request)).await {
        Ok(done) => done,
        Err(_) => Removed::failed("timeout"),
    }
}

async fn remove_inner(root: &Path, source: &Path, request: &RemoveWorktree) -> Removed {
    if !valid_branch(&request.branch).await {
        return Removed::failed("branch_not_allowed");
    }
    let Some(target) = resolve_target(root, Path::new(&request.path)) else {
        return Removed::failed("path_outside_root");
    };
    let Some(common) = common_dir(source).await else {
        return Removed::failed("source_not_git");
    };
    if fs::symlink_metadata(&target).is_err() {
        forget_missing(source, &target).await;
    } else {
        if !is_worktree_of(&target, &common).await {
            return Removed::failed("not_a_worktree");
        }
        if !request.force {
            match holds_unsaved_work(&target).await {
                None => return Removed::failed("git_failed"),
                Some(true) => {
                    return Removed {
                        outcome: "dirty",
                        detail: "dirty",
                        branch_kept: false,
                    };
                }
                Some(false) => {}
            }
        }
        let removed = if request.force {
            git_ok(source, args!["worktree", "remove", "--force", "--", target]).await
        } else {
            git_ok(source, args!["worktree", "remove", "--", target]).await
        };
        if !removed {
            return Removed::failed("git_failed");
        }
    }
    Removed {
        outcome: "removed",
        detail: "",
        branch_kept: settle_branch(source, &request.branch).await,
    }
}

/// С каталогом копии пропало бы то, что есть только в нём: незакоммиченные правки и коммиты, до
/// которых не дойти ни от одной ссылки, например на отсоединённом HEAD. `--all` здесь не годится:
/// он включает HEAD самой копии и потому всегда даёт ноль.
async fn holds_unsaved_work(target: &Path) -> Option<bool> {
    let status = git_text(
        target,
        args!["status", "--porcelain", "--untracked-files=all"],
    )
    .await?;
    if !status.is_empty() {
        return Some(true);
    }
    let unreachable = git_text(
        target,
        args!["rev-list", "--count", "HEAD", "--not", "--glob=*"],
    )
    .await?;
    Some(unreachable != "0")
}

/// `true`, если ветка осталась: в ней есть коммиты без другой ссылки, либо удалить не вышло.
async fn settle_branch(source: &Path, branch: &str) -> bool {
    if !branch_exists(source, branch).await {
        return false;
    }
    let name = format!("refs/heads/{branch}");
    let exclude = format!("--exclude={name}");
    let unique = git_text(
        source,
        args!["rev-list", "--count", name, "--not", exclude, "--all"],
    )
    .await;
    if unique.as_deref() != Some("0") {
        return true;
    }
    !git_ok(source, args!["branch", "-D", "--", branch]).await
}
