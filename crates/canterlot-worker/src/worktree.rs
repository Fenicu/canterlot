//! Изолированные рабочие копии git для делегированных сессий: подготовка перед запуском CLI и
//! уборка после приёма результата. Исходное дерево не меняется, в `.git` добавляются только
//! метаданные копии и одна ветка, наружу ничего не отправляется.

use std::{
    ffi::{OsStr, OsString},
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
    git_with(dir, &[], args).await
}

/// `overrides` идут перед командой как `-c`; дочерние git получают их через окружение.
async fn git_with(dir: &Path, overrides: &[OsString], args: &[&OsStr]) -> Option<Output> {
    git_command(dir, overrides, args).output().await.ok()
}

fn git_command(dir: &Path, overrides: &[OsString], args: &[&OsStr]) -> Command {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .args(overrides)
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
        .kill_on_drop(true);
    command
}

/// Запускает команду лидером своей группы процессов и ждёт её. Если ожидание брошено (вышел
/// срок), убита будет вся группа: `git worktree add` распаковывает файлы дочерним
/// `git reset --hard`, и `kill_on_drop` оставил бы его писать в каталог, пока откат его удаляет.
async fn run_group(mut command: Command) -> Option<std::process::ExitStatus> {
    let mut child = command.process_group(0).spawn().ok()?;
    let mut group = GroupKill(child.id());
    let status = child.wait().await.ok();
    group.0 = None;
    status
}

/// Пока лидер не дождан, его PID и номер группы не переиспользуются: сигнал уходит только своим.
struct GroupKill(Option<u32>);

impl Drop for GroupKill {
    fn drop(&mut self) {
        let leader = self
            .0
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(rustix::process::Pid::from_raw);
        if let Some(leader) = leader {
            let _ = rustix::process::kill_process_group(leader, rustix::process::Signal::KILL);
        }
    }
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

/// Пустые команды для всех фильтров из конфигурации, которую видит `dir` (система, владелец,
/// репозиторий, копия): `git status` и `git worktree remove` в копии делегата не запускают
/// `filter.<имя>.clean`, `smudge` и `process` с правами воркера. Чтение конфига ничего не
/// запускает. Имя, которое нельзя передать через `-c`, даёт `None`: удаление откажет.
async fn filter_overrides(dir: &Path) -> Option<Vec<OsString>> {
    let output = git(dir, args!["config", "--null", "--get-regexp", r"^filter\."]).await?;
    // Код 1: ни одного фильтра.
    match output.status.code() {
        Some(0) => {}
        Some(1) if output.stdout.is_empty() => return Some(Vec::new()),
        _ => return None,
    }
    let mut names = std::collections::BTreeSet::new();
    for entry in output.stdout.split(|byte| *byte == 0) {
        let Some(key) = entry.split(|byte| *byte == b'\n').next() else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        let key = std::str::from_utf8(key).ok()?;
        let name = key.strip_prefix("filter.")?.rsplit_once('.')?.0;
        if name.is_empty() || name.contains('=') {
            return None;
        }
        names.insert(name.to_owned());
    }
    let mut overrides = Vec::new();
    for name in names {
        for setting in ["clean=", "smudge=", "process=", "required=false"] {
            overrides.push(OsString::from("-c"));
            overrides.push(format!("filter.{name}.{setting}").into());
        }
    }
    Some(overrides)
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
        if !is_worktree_of(target, &common).await {
            return Err("worktree_failed");
        }
        if !add_interrupted(target).await {
            return Ok(());
        }
        // Прошлый `git worktree add` не дошёл до конца (перезагрузка, убитый git): в копии
        // часть файлов. Она наша и пустая по смыслу, поэтому убирается и создаётся заново.
        if !valid_branch(&spec.branch).await {
            return Err("worktree_failed");
        }
        tracing::warn!(
            kind = "worktree_add_interrupted",
            "unfinished worktree is rebuilt"
        );
        rollback(target, source, &spec.branch).await;
        if fs::symlink_metadata(target).is_ok() {
            return Err("worktree_failed");
        }
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
    let mut add = git_command(
        source,
        &[],
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
    );
    add.stdout(Stdio::null());
    if run_group(add).await.is_some_and(|status| status.success()) {
        Ok(())
    } else {
        Err("worktree_failed")
    }
}

/// `git worktree add` держит в метаданных копии замок `initializing` и снимает его последним
/// шагом. Замок с этой причиной значит, что распаковка не закончилась. Замок владельца
/// (`git worktree lock`) несёт другую причину и готовой копии не мешает.
async fn add_interrupted(target: &Path) -> bool {
    let Some(git_dir) = git_text(
        target,
        args!["rev-parse", "--path-format=absolute", "--git-dir"],
    )
    .await
    else {
        return false;
    };
    fs::read_to_string(Path::new(&git_dir).join("locked"))
        .is_ok_and(|reason| reason.trim() == "initializing")
}

/// Откат недоделанной копии: каталога и ветки до начала не было, они наши. Двойной `--force`
/// снимает и запертую копию: прерванный `add` оставляет замок `initializing`.
async fn rollback(target: &Path, source: &Path, branch: &str) {
    if !git_ok(
        source,
        args!["worktree", "remove", "--force", "--force", "--", target],
    )
    .await
    {
        let _ = fs::remove_dir_all(target);
    }
    forget_missing(source, target).await;
    let _ = git_ok(source, args!["branch", "-D", "--", branch]).await;
}

/// Метаданные пропавшей копии убирает только её собственная запись: `prune` чистил бы
/// и чужие записи репозитория, например рабочие копии владельца с удалёнными каталогами.
/// Запертую запись `prune` не тронул бы вовсе, поэтому здесь двойной `--force`.
async fn forget_missing(source: &Path, target: &Path) {
    let _ = git_ok(
        source,
        args!["worktree", "remove", "--force", "--force", "--", target],
    )
    .await;
}

/// Убирает копию, а ветку только если в ней нет коммитов, которых нет больше нигде.
pub async fn remove(root: &Path, source: &Path, request: &RemoveWorktree) -> Removed {
    match timeout(DEADLINE, remove_inner(root, source, request)).await {
        Ok(done) => done,
        Err(_) => Removed::failed("timeout"),
    }
}

/// Каталог или его предок содержит `.git`: у пропавшей копии могли остаться метаданные и
/// ветка. В не-git источнике git не нужен вовсе.
fn inside_git_worktree(dir: &Path) -> bool {
    dir.ancestors()
        .any(|ancestor| fs::symlink_metadata(ancestor.join(".git")).is_ok())
}

async fn remove_inner(root: &Path, source: &Path, request: &RemoveWorktree) -> Removed {
    let Some(target) = resolve_target(root, Path::new(&request.path)) else {
        return Removed::failed("path_outside_root");
    };
    let present = fs::symlink_metadata(&target).is_ok();
    // Копии нет, а исходный каталог не git: убирать нечего и git звать не за чем.
    if !present && !inside_git_worktree(source) {
        return Removed {
            outcome: "removed",
            detail: "",
            branch_kept: false,
        };
    }
    if !valid_branch(&request.branch).await {
        return Removed::failed("branch_not_allowed");
    }
    let Some(common) = common_dir(source).await else {
        // Копии нет: даже если `.git` нашёлся выше, убирать всё равно нечего.
        return if present {
            Removed::failed("source_not_git")
        } else {
            Removed {
                outcome: "removed",
                detail: "",
                branch_kept: false,
            }
        };
    };
    if !present {
        forget_missing(source, &target).await;
    } else {
        if !is_worktree_of(&target, &common).await {
            return Removed::failed("not_a_worktree");
        }
        let Some(overrides) = filter_overrides(&target).await else {
            return Removed::failed("git_failed");
        };
        if !request.force {
            match holds_unsaved_work(&target, &overrides).await {
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
            git_with(
                source,
                &overrides,
                args!["worktree", "remove", "--force", "--", target],
            )
            .await
        } else {
            git_with(
                source,
                &overrides,
                args!["worktree", "remove", "--", target],
            )
            .await
        }
        .is_some_and(|output| output.status.success());
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
/// Изменения внутри подмодулей не смотрятся: их `git status` читал бы конфиг подмодуля
/// с фильтрами, которых `overrides` не знает. Сдвиг коммита подмодуля виден по-прежнему.
async fn holds_unsaved_work(target: &Path, overrides: &[OsString]) -> Option<bool> {
    let status = git_with(
        target,
        overrides,
        args![
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignore-submodules=dirty"
        ],
    )
    .await
    .filter(|output| output.status.success())?;
    let status = String::from_utf8_lossy(&status.stdout).trim().to_owned();
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

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read};

    use super::*;

    /// Срок вышел посреди `git worktree add`: вместе с верхним процессом уходит и его
    /// потомок, который распаковывает файлы. Канал закрывается, только когда мертвы оба.
    #[tokio::test]
    async fn a_dropped_run_kills_the_whole_process_group() {
        let (reader, writer) = std::io::pipe().unwrap();
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 60 & echo ready; wait"])
            .stdin(Stdio::null())
            .stdout(writer)
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut run = Box::pin(run_group(command));
        let mut ready = tokio::task::spawn_blocking(move || {
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            (reader, line)
        });
        let (mut reader, line) = tokio::select! {
            status = &mut run => panic!("the command ended by itself: {status:?}"),
            ready = &mut ready => ready.unwrap(),
        };
        assert_eq!(line, "ready\n");
        drop(run);
        let rest = tokio::task::spawn_blocking(move || {
            let mut rest = Vec::new();
            reader.read_to_end(&mut rest).map(|_| rest)
        });
        let rest = timeout(Duration::from_secs(30), rest)
            .await
            .expect("a descendant outlived the dropped run and still holds the pipe")
            .unwrap()
            .unwrap();
        assert!(rest.is_empty());
    }
}
