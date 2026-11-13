//! Факты git на конец хода. Тексты коммитов и пути относятся к данным уровня сообщения:
//! ни в логи, ни в диагностику они не попадают, здесь нет ни одного вызова `tracing`.

use std::{ffi::OsStr, path::Path, process::Stdio, time::Duration};

use canterlot_proto::worker::v1::{GitCommit, GitFacts, GitFile};
use prost::Message;
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

pub const DEADLINE: Duration = Duration::from_secs(2);
pub const MAX_COMMITS: usize = 50;
pub const MAX_FILES: usize = 200;
pub const MAX_SUBJECT_BYTES: usize = 256;
pub const MAX_PATH_BYTES: usize = 1024;
pub const MAX_FACTS_BYTES: usize = 32 * 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// `HEAD` рабочего каталога; `None` для не-репозитория, ошибки и истёкшего срока.
pub async fn head(workdir: &Path) -> Option<String> {
    head_with(OsStr::new("git"), workdir, DEADLINE).await
}

pub async fn head_with(git: &OsStr, workdir: &Path, deadline: Duration) -> Option<String> {
    timeout(deadline, rev_parse(git, workdir)).await.ok()?
}

pub async fn collect(workdir: &Path, head_before: Option<&str>) -> GitFacts {
    collect_with(OsStr::new("git"), workdir, head_before, DEADLINE).await
}

/// Единый срок на все команды: по истечении факты недоступны, а не частичны.
pub async fn collect_with(
    git: &OsStr,
    workdir: &Path,
    head_before: Option<&str>,
    deadline: Duration,
) -> GitFacts {
    timeout(deadline, gather(git, workdir, head_before))
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
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
