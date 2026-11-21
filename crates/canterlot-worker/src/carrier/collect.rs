//! `CollectOutputs`: результаты задачи с раннера (или с самого носителя) в
//! `<data_dir>/job-outputs/<job_id>/`, и уборка старых каталогов результатов.
//!
//! Только обычные файлы и каталоги. Ссылки и особые файлы отбрасываются на каждом шаге: путь с
//! символьной ссылкой в любой части пропускается целиком, `find -type f` не заходит в ссылки,
//! а `rsync` запускается с `--no-links --no-specials --no-devices`. Общий предел `cap_bytes`
//! измеряется до копирования: файлы идут в порядке путей, пока влезают.

use std::{
    collections::BTreeMap,
    io::{self, Read},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime},
};

use super::stage::{
    Failure, Place, Stager, carrier_workdir_shape, first_line, kv, parse_job, script_failure,
};
use tokio::{io::AsyncWriteExt, process::Command};

/// Сколько путей в одной команде принимает носитель.
const MAX_PATHS: usize = 64;

/// `CollectOutputs`.
#[derive(Clone, Debug)]
pub struct CollectRequest {
    pub job_id: String,
    pub target_machine_id: String,
    pub target_root_dir: String,
    pub workdir: String,
    pub paths: Vec<String>,
    pub cap_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputFile {
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outputs {
    pub local_dir: PathBuf,
    pub files: Vec<OutputFile>,
    pub truncated: bool,
    /// Пути, которых в рабочей копии нет: не ошибка.
    pub missing: Vec<String>,
    /// Ссылки и особые файлы, не скопированные.
    pub skipped: Vec<String>,
}

/// Относительный путь из знаков, которым не нужны кавычки, без `..`.
fn valid_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && !path.starts_with('/')
        && !path.starts_with('-')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && path.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'+' | b'@')
        })
}

/// Файлы по порядку путей, пока сумма не выше предела; первый не поместившийся обрывает отбор.
fn select(mut entries: Vec<(String, u64)>, cap: u64) -> (Vec<(String, u64)>, bool) {
    entries.sort();
    entries.dedup_by(|a, b| a.0 == b.0);
    let mut total = 0u64;
    let mut chosen = Vec::new();
    for (path, bytes) in entries {
        match total.checked_add(bytes) {
            Some(next) if next <= cap => {
                total = next;
                chosen.push((path, bytes));
            }
            _ => return (chosen, true),
        }
    }
    (chosen, false)
}

fn printable(name: &str) -> bool {
    !name.contains(['\u{fffd}', '\n', '\r', '\t']) && !name.chars().any(char::is_control)
}

impl Stager {
    /// Копирует `paths` из `workdir` в каталог результатов задачи. Повтор перезаписывает.
    pub async fn collect(&self, request: &CollectRequest) -> Result<Outputs, Failure> {
        let job = parse_job(&request.job_id)?;
        if request.paths.is_empty() || request.paths.len() > MAX_PATHS {
            return Err(Failure::new(
                "bad_path",
                "between 1 and 64 paths are required",
            ));
        }
        if request.paths.iter().any(|path| !valid_relative(path)) {
            return Err(Failure::new(
                "bad_path",
                "paths must be relative, without .., and use only [A-Za-z0-9._/+@-]",
            ));
        }
        let place = self.place(&request.target_machine_id, &request.target_root_dir)?;
        let workdir = PathBuf::from(&request.workdir);
        let local_dir = self.settings.outputs_root.join(job.to_string());
        match place {
            Place::Runner { ssh, alias, root } => {
                let fits = ["personal", "work"]
                    .iter()
                    .any(|domain| workdir == root.join("jobs").join(domain).join(job.to_string()));
                if !fits {
                    return Err(Failure::new(
                        "bad_workdir",
                        "workdir is not <root>/jobs/<domain>/<job>",
                    ));
                }
                self.collect_runner(&ssh, &alias, request, &workdir, local_dir)
                    .await
            }
            Place::Carrier => {
                if !carrier_workdir_shape(&workdir, job) {
                    return Err(Failure::new(
                        "bad_workdir",
                        "workdir is not under .canterlot-jobs",
                    ));
                }
                let paths = request.paths.clone();
                let cap = request.cap_bytes;
                tokio::task::spawn_blocking(move || collect_local(&workdir, &paths, cap, local_dir))
                    .await
                    .map_err(|_| Failure::new("collect_failed", "collection task failed"))?
            }
        }
    }

    async fn collect_runner(
        &self,
        ssh: &super::ssh::Ssh,
        alias: &str,
        request: &CollectRequest,
        workdir: &Path,
        local_dir: PathBuf,
    ) -> Result<Outputs, Failure> {
        let wd = workdir.display();
        let list = format!(
            "cd {wd} 2>/dev/null || {{ echo fail=no_workdir; exit 0; }}\n\
             for p in {paths}; do\n\
             bad=0; d=\"\"; oldifs=$IFS; IFS=/\n\
             set -- $p\n\
             IFS=$oldifs\n\
             for c in \"$@\"; do d=\"$d$c\"; if [ -L \"$d\" ]; then bad=1; fi; d=\"$d/\"; done\n\
             if [ $bad = 1 ]; then printf 'skip\\t%s\\n' \"$p\"; continue; fi\n\
             if [ ! -e \"$p\" ]; then printf 'missing\\t%s\\n' \"$p\"; continue; fi\n\
             if [ ! -f \"$p\" ] && [ ! -d \"$p\" ]; then printf 'skip\\t%s\\n' \"$p\"; continue; fi\n\
             find \"$p\" -type f -printf 'f\\t%s\\t%p\\n'\n\
             done\n\
             echo done=1\n",
            paths = request.paths.join(" "),
        );
        let answer = self.remote(ssh, &list).await?;
        if let Some(reason) = kv(&answer.stdout, "fail") {
            return Err(Failure::new(
                reason,
                "the runner has no such working directory",
            ));
        }
        if kv(&answer.stdout, "done").is_none() {
            return Err(script_failure(&answer));
        }
        let mut entries = Vec::new();
        let mut missing = Vec::new();
        let mut skipped = Vec::new();
        for line in answer.stdout.lines() {
            let mut fields = line.splitn(3, '\t');
            match (fields.next(), fields.next(), fields.next()) {
                (Some("f"), Some(size), Some(path)) => match size.parse::<u64>() {
                    Ok(bytes) if printable(path) && valid_listed(path) => {
                        entries.push((path.to_owned(), bytes));
                    }
                    _ => skipped.push(path.to_owned()),
                },
                (Some("missing"), Some(path), None) => missing.push(path.to_owned()),
                (Some("skip"), Some(path), None) => skipped.push(path.to_owned()),
                _ => {}
            }
        }
        let (chosen, truncated) = select(entries, request.cap_bytes);
        make_local_dir(&local_dir)?;
        if !chosen.is_empty() {
            self.rsync(alias, workdir, &local_dir, &chosen, request.cap_bytes)
                .await?;
        }
        let files = chosen
            .into_iter()
            .filter_map(|(path, _)| {
                let meta = std::fs::symlink_metadata(local_dir.join(&path)).ok()?;
                meta.file_type().is_file().then_some(OutputFile {
                    path,
                    bytes: meta.len(),
                })
            })
            .collect();
        Ok(Outputs {
            local_dir,
            files,
            truncated,
            missing,
            skipped,
        })
    }

    /// Команда копирования. `--max-size=<cap>` не даёт файлу, который успел вырасти между
    /// измерением и копированием (задача с таймаутом, чей контейнер пережил её), перетянуть
    /// предел: больший файл `rsync` просто пропускает.
    fn rsync_command(
        &self,
        alias: &str,
        workdir: &Path,
        local_dir: &Path,
        cap_bytes: u64,
    ) -> Command {
        let mut command = Command::new("rsync");
        command
            .args([
                "-rt",
                "--no-links",
                "--no-specials",
                "--no-devices",
                "--files-from=-",
            ])
            .arg(format!("--max-size={cap_bytes}"))
            .arg("-e")
            .arg(self.ssh_command())
            .arg(format!("{alias}:{}/", workdir.display()))
            .arg(format!("{}/", local_dir.display()))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    async fn rsync(
        &self,
        alias: &str,
        workdir: &Path,
        local_dir: &Path,
        files: &[(String, u64)],
        cap_bytes: u64,
    ) -> Result<(), Failure> {
        let mut command = self.rsync_command(alias, workdir, local_dir, cap_bytes);
        let list: String = files.iter().map(|(path, _)| format!("{path}\n")).collect();
        let work = async {
            let mut child = command.spawn()?;
            let mut stdin = child.stdin.take().expect("piped stdin");
            let _ = stdin.write_all(list.as_bytes()).await;
            drop(stdin);
            child.wait_with_output().await
        };
        let output = tokio::time::timeout(self.settings.push_limit, work)
            .await
            .map_err(|_| Failure::new("copy_timeout", "rsync did not finish in time"))?
            .map_err(|_| Failure::new("rsync_missing", "rsync could not be started"))?;
        // 24 — файл исчез между измерением и копированием: он просто не попадёт в список.
        match output.status.code() {
            Some(0 | 24) => Ok(()),
            Some(127) => Err(Failure::new(
                "rsync_missing",
                "rsync is not installed on one side",
            )),
            _ => Err(Failure::new(
                "rsync_failed",
                first_line(&String::from_utf8_lossy(&output.stderr)),
            )),
        }
    }
}

/// Имя файла из ответа раннера: относительное, без `..`, безопасное для `--files-from`.
fn valid_listed(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn make_local_dir(dir: &Path) -> Result<(), Failure> {
    let fail = |error: io::Error| {
        Failure::new(
            "outputs_dir",
            format!("cannot create {}: {}", dir.display(), error.kind()),
        )
    };
    if let Some(parent) = dir.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(fail)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(fail(error)),
    }
}

/// Отбор и копирование на самом носителе: без ссылок на каждом шаге пути.
fn collect_local(
    workdir: &Path,
    paths: &[String],
    cap: u64,
    local_dir: PathBuf,
) -> Result<Outputs, Failure> {
    if !std::fs::symlink_metadata(workdir).is_ok_and(|meta| meta.is_dir()) {
        return Err(Failure::new(
            "no_workdir",
            "the working directory does not exist",
        ));
    }
    let mut entries = Vec::new();
    let mut missing = Vec::new();
    let mut skipped = Vec::new();
    'paths: for path in paths {
        let mut current = workdir.to_path_buf();
        let mut exists = true;
        for part in path.split('/') {
            current.push(part);
            match std::fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    skipped.push(path.clone());
                    continue 'paths;
                }
                Ok(_) => {}
                Err(_) => {
                    exists = false;
                    break;
                }
            }
        }
        if !exists {
            missing.push(path.clone());
            continue;
        }
        let before = (entries.len(), skipped.len());
        walk(workdir, &current, &mut entries, &mut skipped);
        let meta = std::fs::symlink_metadata(&current).map_err(|_| {
            Failure::new("collect_failed", "a path disappeared while it was listed")
        })?;
        if !meta.is_file() && !meta.is_dir() && (entries.len(), skipped.len()) == before {
            skipped.push(path.clone());
        }
    }
    let (chosen, truncated) = select(entries, cap);
    make_local_dir(&local_dir)?;
    let mut files = Vec::new();
    for (path, _) in chosen {
        if let Some(bytes) = copy_file(&workdir.join(&path), &local_dir.join(&path)) {
            files.push(OutputFile { path, bytes });
        }
    }
    Ok(Outputs {
        local_dir,
        files,
        truncated,
        missing,
        skipped,
    })
}

/// Обычные файлы под `current` (сам `current` — файл или каталог). `lstat` на каждом шаге.
fn walk(
    workdir: &Path,
    current: &Path,
    entries: &mut Vec<(String, u64)>,
    skipped: &mut Vec<String>,
) {
    let Ok(meta) = std::fs::symlink_metadata(current) else {
        return;
    };
    let relative = current
        .strip_prefix(workdir)
        .ok()
        .and_then(Path::to_str)
        .unwrap_or_default()
        .to_owned();
    if meta.is_file() {
        if printable(&relative) {
            entries.push((relative, meta.len()));
        } else {
            skipped.push(relative);
        }
    } else if meta.is_dir() {
        let Ok(read) = std::fs::read_dir(current) else {
            return;
        };
        let mut children: Vec<PathBuf> = read.flatten().map(|entry| entry.path()).collect();
        children.sort();
        for child in children {
            walk(workdir, &child, entries, skipped);
        }
    }
    // Ссылки и особые файлы внутри каталога молча не попадают в список.
}

/// Копия обычного файла; источник открывается без перехода по ссылке.
fn copy_file(source: &Path, destination: &Path) -> Option<u64> {
    let mut input = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(source)
        .ok()?;
    if !input.metadata().ok()?.is_file() {
        return None;
    }
    std::fs::create_dir_all(destination.parent()?).ok()?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(destination)
        .ok()?;
    let mut buffer = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let read = input.read(&mut buffer).ok()?;
        if read == 0 {
            return Some(total);
        }
        io::Write::write_all(&mut output, &buffer[..read]).ok()?;
        total += read as u64;
    }
}

/// Удаляет каталоги результатов старше `retention`. Только прямые потомки `root` с именем
/// UUID, по буквальным путям из списка одного каталога; ссылки не разыменовываются.
pub fn sweep_outputs(root: &Path, retention: Duration, now: SystemTime) -> usize {
    let Ok(read) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    let mut by_name = BTreeMap::new();
    for entry in read.flatten() {
        by_name.insert(entry.file_name(), entry);
    }
    for (name, entry) in by_name {
        let Some(text) = name.to_str() else { continue };
        if parse_job(text).is_err() {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
            continue;
        };
        if now
            .duration_since(modified)
            .is_ok_and(|age| age >= retention)
            && std::fs::remove_dir_all(root.join(text)).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// Раз в час и при запуске чистит каталог результатов, пока не придёт `shutdown`.
pub async fn sweep_loop(
    root: PathBuf,
    retention: Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        let sweep_root = root.clone();
        let removed = tokio::task::spawn_blocking(move || {
            sweep_outputs(&sweep_root, retention, SystemTime::now())
        })
        .await
        .unwrap_or(0);
        if removed > 0 {
            tracing::info!(removed, "old job output directories removed");
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(3600)) => {}
            _ = shutdown.wait_for(|stop| *stop) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::carrier::stage::StagerSettings;

    /// Правила носителя для путей результатов. Хаб проверяет те же при приёме задачи
    /// (`inplace_jobs::spec::valid_output_path`): расхождение значило бы задачу, которая
    /// отработала и упала на имени. При правке меняйте обе стороны.
    #[test]
    fn output_path_rules_match_what_the_hub_checks_at_submit() {
        for good in [
            "out",
            "a/b/c.txt",
            ".gate-out",
            "dir.v2/x+y@z_1-2",
            &"a".repeat(512),
        ] {
            assert!(valid_relative(good), "{good:?}");
        }
        for bad in [
            "",
            "/abs",
            "-flag",
            "a/../b",
            "..",
            "a//b",
            "./a",
            "a/./b",
            "test results",
            "tab\there",
            "ünï",
            "a;b",
            "a$b",
            &"a".repeat(513),
        ] {
            assert!(!valid_relative(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_copy_is_capped_per_file_so_a_growing_file_cannot_blow_the_total_cap() {
        let stager = Stager::new(StagerSettings {
            ssh_config: None,
            outputs_root: PathBuf::from("/tmp/outputs"),
            targets: Arc::new(|_| None),
            ssh_limit: Duration::from_secs(1),
            push_limit: Duration::from_secs(1),
            allowed_roots: Vec::new(),
        });
        let command = stager.rsync_command(
            "runner-a",
            Path::new("/srv/runner/jobs/work/j"),
            Path::new("/tmp/outputs/j"),
            4096,
        );
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.iter().any(|arg| arg == "--max-size=4096"),
            "rsync must be told the cap: {args:?}"
        );
    }
}
