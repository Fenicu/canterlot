//! Факты git на конец хода. Тексты коммитов и пути относятся к данным уровня сообщения:
//! ни в логи, ни в диагностику они не попадают, здесь нет ни одного вызова `tracing`.

use std::{
    ffi::OsStr,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

use canterlot_proto::worker::v1::{BranchMerge, GitCommit, GitFacts, GitFile};
use prost::Message;
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

pub const DEADLINE: Duration = Duration::from_secs(2);
pub const MAX_COMMITS: usize = 50;
pub const MAX_FILES: usize = 200;
pub const MAX_SUBJECT_BYTES: usize = 256;
pub const MAX_PATH_BYTES: usize = 1024;
pub const MAX_FACTS_BYTES: usize = 32 * 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
/// Веток в пробе слияния за ход: столько хаб и присылает.
pub const MAX_MERGE_BRANCHES: usize = 10;
/// Ветки делегатов, которые хаб создаёт сам (`canterlot/<хвост id>`).
const MERGE_BRANCH_PREFIX: &str = "canterlot/";

/// `HEAD` рабочего каталога; `None` для не-репозитория, ошибки и истёкшего срока.
pub async fn head(workdir: &Path) -> Option<String> {
    head_with(OsStr::new("git"), workdir, DEADLINE).await
}

pub async fn head_with(git: &OsStr, workdir: &Path, deadline: Duration) -> Option<String> {
    timeout(deadline, rev_parse(git, workdir)).await.ok()?
}

pub async fn collect(
    workdir: &Path,
    head_before: Option<&str>,
    merge_branches: &[String],
) -> GitFacts {
    collect_with(
        OsStr::new("git"),
        workdir,
        head_before,
        merge_branches,
        DEADLINE,
    )
    .await
}

/// Единый срок на все команды: по истечении факты недоступны, а не частичны. Проба слияния
/// идёт после основных фактов в остатке того же срока и, не уложившись, пропадает одна.
pub async fn collect_with(
    git: &OsStr,
    workdir: &Path,
    head_before: Option<&str>,
    merge_branches: &[String],
    deadline: Duration,
) -> GitFacts {
    let started = Instant::now();
    let mut facts = timeout(deadline, gather(git, workdir, head_before))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    if facts.available && !merge_branches.is_empty() {
        let left = deadline.saturating_sub(started.elapsed());
        if let Ok(merges) = timeout(left, merges(git, workdir, merge_branches)).await {
            facts.merges = merges;
        }
    }
    facts
}

/// Только имена, которые хаб даёт веткам делегатов: префикс и безопасные символы, без
/// ведущего дефиса и точек. Остальное воркер не спрашивает вовсе.
fn merge_branch_allowed(branch: &str) -> bool {
    let Some(tail) = branch.strip_prefix(MERGE_BRANCH_PREFIX) else {
        return false;
    };
    (1..=64).contains(&tail.len())
        && !tail.starts_with('-')
        && tail
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `git cherry HEAD <branch>` по каждой ветке: строки `+` — коммиты без равного патча в HEAD.
/// Ветка, которую прочитать не вышло, в ответ не попадает: «неизвестно» не равно «слита».
async fn merges(git: &OsStr, workdir: &Path, branches: &[String]) -> Vec<BranchMerge> {
    let mut out = Vec::new();
    for branch in branches
        .iter()
        .filter(|branch| merge_branch_allowed(branch))
        .take(MAX_MERGE_BRANCHES)
    {
        let name = format!("refs/heads/{branch}");
        let revision = format!("{name}^{{commit}}");
        let tip = run(
            git,
            workdir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &revision,
            ],
        )
        .await
        .and_then(|out| String::from_utf8(out.bytes).ok())
        .map(|tip| tip.trim().to_owned())
        .filter(|tip| !tip.is_empty());
        let Some(tip) = tip else {
            out.push(BranchMerge {
                branch: branch.clone(),
                missing: true,
                ..Default::default()
            });
            continue;
        };
        let Some(cherry) = run(git, workdir, &["cherry", "HEAD", &name]).await else {
            continue;
        };
        if cherry.overflow {
            continue;
        }
        let unmerged = String::from_utf8_lossy(&cherry.bytes)
            .lines()
            .filter(|line| line.starts_with('+'))
            .count();
        out.push(BranchMerge {
            branch: branch.clone(),
            tip,
            unmerged: u32::try_from(unmerged).unwrap_or(u32::MAX),
            missing: false,
        });
    }
    out
}

async fn rev_parse(git: &OsStr, workdir: &Path) -> Option<String> {
    let out = run(git, workdir, &["rev-parse", "HEAD"]).await?;
    let sha = String::from_utf8(out.bytes).ok()?.trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

async fn gather(git: &OsStr, workdir: &Path, head_before: Option<&str>) -> Option<GitFacts> {
    let head_after = rev_parse(git, workdir).await?;
    let before = head_before.filter(|b| !b.is_empty() && *b != head_after);
    let range = before.map(|b| format!("{b}..{head_after}"));
    let (log, diff, status) = tokio::join!(
        async {
            match &range {
                Some(range) => {
                    run(
                        git,
                        workdir,
                        &["log", "--format=%H%x00%ct%x00%s", "--max-count=51", range],
                    )
                    .await
                }
                None => Some(Output::default()),
            }
        },
        async {
            match before {
                Some(b) => {
                    run(
                        git,
                        workdir,
                        &[
                            "diff",
                            "--name-status",
                            "-z",
                            "--no-renames",
                            b,
                            &head_after,
                        ],
                    )
                    .await
                }
                None => Some(Output::default()),
            }
        },
        run(git, workdir, &["status", "--porcelain=v1"]),
    );
    let status = status?;
    let mut facts = GitFacts {
        available: true,
        head_before: head_before.unwrap_or_default().to_string(),
        head_after,
        dirty: !status.bytes.is_empty(),
        ..Default::default()
    };
    // Диапазон не прочитан (объект исчез после перебазирования): списки неполны.
    let (Some(log), Some(diff)) = (log, diff) else {
        facts.truncated = true;
        return Some(facts);
    };
    facts.truncated = log.overflow || diff.overflow;
    parse_log(&log.bytes, &mut facts);
    parse_diff(&diff.bytes, &mut facts);
    fit(&mut facts);
    Some(facts)
}

fn parse_log(bytes: &[u8], facts: &mut GitFacts) {
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines().filter(|l| !l.is_empty()) {
        let mut parts = line.splitn(3, '\0');
        let sha = parts.next().unwrap_or_default();
        let committed_at_ms = parts
            .next()
            .and_then(|seconds| seconds.parse::<i64>().ok())
            .and_then(|seconds| seconds.checked_mul(1000))
            .unwrap_or_default();
        let subject = parts.next().unwrap_or_default();
        if facts.commits.len() == MAX_COMMITS {
            facts.truncated = true;
            break;
        }
        let (subject, clipped) = clip(subject, MAX_SUBJECT_BYTES);
        facts.truncated |= clipped;
        facts.commits.push(GitCommit {
            sha: sha.to_string(),
            subject,
            committed_at_ms,
        });
    }
}

fn parse_diff(bytes: &[u8], facts: &mut GitFacts) {
    let text = String::from_utf8_lossy(bytes);
    let mut parts = text.split('\0');
    while let (Some(status), Some(path)) = (parts.next(), parts.next()) {
        if status.is_empty() || path.is_empty() {
            continue;
        }
        if facts.files.len() == MAX_FILES {
            facts.truncated = true;
            break;
        }
        let (path, clipped) = clip(path, MAX_PATH_BYTES);
        facts.truncated |= clipped;
        facts.files.push(GitFile {
            path,
            status: status.chars().take(1).collect(),
        });
    }
}

/// Укладывает в общий предел: сначала отбрасываются файлы с конца, затем коммиты.
fn fit(facts: &mut GitFacts) {
    while facts.encoded_len() > MAX_FACTS_BYTES {
        facts.truncated = true;
        if facts.files.pop().is_none() && facts.commits.pop().is_none() {
            break;
        }
    }
}

fn clip(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
    overflow: bool,
}

/// Запуск git только на чтение: без подсказок, без необязательных блокировок индекса и без
/// хуков наблюдателя файловой системы. Окружение репозитория, унаследованное от CLI, снимается.
async fn run(git: &OsStr, workdir: &Path, args: &[&str]) -> Option<Output> {
    let mut child = Command::new(git)
        .args(["-c", "core.quotepath=false", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(workdir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut bytes = Vec::new();
    let limit = u64::try_from(MAX_OUTPUT_BYTES).unwrap_or(u64::MAX);
    (&mut stdout)
        .take(limit)
        .read_to_end(&mut bytes)
        .await
        .ok()?;
    let overflow = bytes.len() >= MAX_OUTPUT_BYTES;
    if overflow {
        child.kill().await.ok()?;
        return Some(Output { bytes, overflow });
    }
    let status = child.wait().await.ok()?;
    status.success().then_some(Output { bytes, overflow })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_delegate_branch_names_are_probed() {
        assert!(merge_branch_allowed("canterlot/0a1b2c3d"));
        assert!(merge_branch_allowed("canterlot/feature_x-2"));
        for name in [
            "main",
            "canterlot/",
            "canterlot/-p",
            "canterlot/../x",
            "canterlot/a/b",
            "canterlot/a.lock",
            "canterlot/a b",
            "--all",
        ] {
            assert!(!merge_branch_allowed(name), "{name}");
        }
        assert!(!merge_branch_allowed(&format!(
            "canterlot/{}",
            "a".repeat(65)
        )));
    }
}
