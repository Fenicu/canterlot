//! Исходники задачи на раннере или на самом носителе: `StageSource` и `CleanupSource`.
//!
//! Образец — `scripts/gate-remote.sh`: коммит уходит `git push` в bare-репозиторий раннера
//! под ссылкой `refs/canterlot/<job_id>`, там создаётся отсоединённая рабочая копия, после
//! задачи копия снимается `git worktree remove`, а ссылка удаляется `update-ref -d`. Никакого
//! `rm -rf`. На самом носителе (корень пуст) рабочая копия делается прямо из репозитория
//! владельца рядом с ним, в `.canterlot-jobs`, поэтому домен по пути тот же.
//!
//! Хаб повторяет эти команды после рестарта воркера, поэтому каждая безопасна при повторе.
//! Все значения проверяются до того, как строится хоть одна строка оболочки: скрипты идут в
//! `sh -s` через stdin ssh, а проверенные значения вставляются буквально.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use tokio::process::Command;
use uuid::Uuid;

pub use super::collect::{CollectRequest, OutputFile, Outputs, sweep_loop, sweep_outputs};
use super::{
    script,
    ssh::{Output, Ssh},
};

/// Короткая машинная причина и одна строка для человека.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub reason: String,
    pub message: String,
}

impl Failure {
    pub fn new(reason: &str, message: impl Into<String>) -> Self {
        Self {
            reason: reason.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.reason, self.message)
    }
}

/// Куда ведёт машина раннера: псевдоним ssh и корень, назначенный хабом. У облачного раннера
/// псевдонима нет: адрес, DNAT-порт, пользователь, ключ и закреплённый `known_hosts` приходят
/// из подключения, поэтому подсоединение несёт [`RunnerHost`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerTarget {
    pub alias: String,
    pub root: PathBuf,
    pub host: Option<RunnerHost>,
}

/// Подключение к облачному серверу окна: всё, что нужно `ssh`, git и `rsync`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerHost {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity: Option<PathBuf>,
    pub known_hosts: PathBuf,
}

/// Раннер по идентификатору машины; `None` — этот носитель такого раннера не ведёт.
pub type Targets = Arc<dyn Fn(Uuid) -> Option<RunnerTarget> + Send + Sync>;

pub struct StagerSettings {
    pub ssh_config: Option<PathBuf>,
    /// Корень каталогов результатов: `<outputs_root>/<job_id>`.
    pub outputs_root: PathBuf,
    pub targets: Targets,
    /// Предел одного короткого вызова по SSH.
    pub ssh_limit: Duration,
    /// Предел отправки коммита и копирования результатов.
    pub push_limit: Duration,
    /// `[execution].allowed_workdir_roots`: репозиторий-источник обязан лежать внутри одного из
    /// них, как рабочий каталог обычной задачи.
    pub allowed_roots: Vec<PathBuf>,
}

/// `StageSource`: поля как в сообщении хаба. Пустой `target_root_dir` — цель сам носитель.
#[derive(Clone, Debug)]
pub struct StageRequest {
    pub job_id: String,
    /// Попытка задачи; 0 от хаба до повторов — первая.
    pub attempt: u32,
    pub repo: PathBuf,
    pub rev: String,
    pub domain: String,
    pub target_machine_id: String,
    pub target_root_dir: String,
    pub workdir: String,
    pub repo_key: String,
}

/// `CleanupSource`.
#[derive(Clone, Debug)]
pub struct CleanupRequest {
    pub job_id: String,
    pub attempt: u32,
    pub target_machine_id: String,
    pub target_root_dir: String,
    pub workdir: String,
    pub repo_key: String,
    pub domain: String,
}

#[derive(Clone)]
pub struct Stager {
    pub(super) settings: Arc<StagerSettings>,
}

/// Куда направлена команда после проверок.
pub(super) enum Place {
    Runner { ssh: Ssh, root: PathBuf },
    Carrier,
}

pub(super) const WORKDIRS: &str = ".canterlot-jobs";

impl Stager {
    pub fn new(settings: StagerSettings) -> Self {
        Self {
            settings: Arc::new(settings),
        }
    }

    pub(super) fn place(&self, machine_id: &str, root: &str) -> Result<Place, Failure> {
        let machine = Uuid::parse_str(machine_id)
            .map_err(|_| Failure::new("bad_machine_id", "target machine id is not a UUID"))?;
        if root.is_empty() {
            return Ok(Place::Carrier);
        }
        let root_path = PathBuf::from(root);
        if !script::valid_root(&root_path) {
            return Err(Failure::new(
                "bad_root",
                "target root is not a safe absolute path",
            ));
        }
        let target = (self.settings.targets)(machine)
            .filter(valid_target)
            .ok_or_else(|| {
                Failure::new("unknown_target", "this carrier does not lead that runner")
            })?;
        if target.root != root_path {
            return Err(Failure::new(
                "bad_root",
                "target root differs from the runner root this carrier knows",
            ));
        }
        let ssh = match &target.host {
            Some(host) => Ssh::host(
                host.host.clone(),
                host.port,
                host.user.clone(),
                host.identity.clone(),
                host.known_hosts.clone(),
            ),
            None => Ssh::new(target.alias.clone(), self.settings.ssh_config.clone()),
        };
        Ok(Place::Runner {
            ssh,
            root: root_path,
        })
    }

    /// Готовит рабочую копию `rev` в `workdir`. Повтор на готовой копии — успех.
    pub async fn stage(&self, request: &StageRequest) -> Result<(), Failure> {
        let job = parse_job(&request.job_id)?;
        let rev = parse_rev(&request.rev)?;
        let domain = parse_domain(&request.domain)?;
        valid_key(&request.repo_key)?;
        if !request.repo.is_absolute() || has_dot_dot(&request.repo) {
            return Err(Failure::new("bad_repo", "repo must be an absolute path"));
        }
        self.check_repo_pinned(&request.repo).await?;
        let place = self.place(&request.target_machine_id, &request.target_root_dir)?;
        let workdir = PathBuf::from(&request.workdir);
        match &place {
            Place::Runner { root, .. } => {
                let want = root
                    .join("jobs")
                    .join(domain)
                    .join(job_dir(job, request.attempt));
                if workdir != want {
                    return Err(Failure::new(
                        "bad_workdir",
                        "workdir is not <root>/jobs/<domain>/<job>",
                    ));
                }
            }
            Place::Carrier => {
                let name = request
                    .repo
                    .file_name()
                    .ok_or_else(|| Failure::new("bad_repo", "repo path has no directory name"))?;
                let want = request
                    .repo
                    .parent()
                    .unwrap_or(Path::new("/"))
                    .join(WORKDIRS)
                    .join(name)
                    .join(job_dir(job, request.attempt));
                if workdir != want {
                    return Err(Failure::new(
                        "bad_workdir",
                        "workdir is not <repo parent>/.canterlot-jobs/<repo>/<job>",
                    ));
                }
            }
        }
        match place {
            Place::Runner { ssh, root } => {
                self.stage_runner(&ssh, &root, request, job, &rev, domain, &workdir)
                    .await
            }
            Place::Carrier => self.stage_local(request, &rev, &workdir).await,
        }
    }

    /// Репозиторий-источник задан каноническим путём (ни ссылок, ни `..`) и лежит внутри
    /// разрешённых корней: хаб относит задачу к домену по тексту пути, и ссылка из личного
    /// каталога на рабочий репозиторий перенесла бы его содержимое через границу доменов. Та же
    /// проверка, что у рабочего каталога обычной задачи (`jobs::workdir::pin`).
    async fn check_repo_pinned(&self, repo: &Path) -> Result<(), Failure> {
        let bad = || {
            Failure::new(
                "bad_repo",
                "repo is not a canonical path inside the allowed roots",
            )
        };
        let real = tokio::fs::canonicalize(repo).await.map_err(|_| bad())?;
        if real.as_os_str() != repo.as_os_str() {
            return Err(bad());
        }
        for root in &self.settings.allowed_roots {
            if tokio::fs::canonicalize(root)
                .await
                .is_ok_and(|root| real.starts_with(root))
            {
                return Ok(());
            }
        }
        Err(bad())
    }

    #[allow(clippy::too_many_arguments)]
    async fn stage_runner(
        &self,
        ssh: &Ssh,
        root: &Path,
        request: &StageRequest,
        job: Uuid,
        rev: &str,
        domain: &str,
        workdir: &Path,
    ) -> Result<(), Failure> {
        let bare = format!("{}/repos/{domain}/{}.git", root.display(), request.repo_key);
        let wd = workdir.display();
        let probe = format!(
            "set -u\n\
             mkdir -p {root}/repos/{domain} {root}/jobs/{domain} || {{ echo fail=mkdir_failed; exit 0; }}\n\
             if [ -f {wd}/.git ]; then\n\
             h=$(git -C {wd} rev-parse HEAD 2>/dev/null)\n\
             if [ \"$h\" = {rev} ]; then echo staged=1; exit 0; fi\n\
             fi\n\
             command -v git >/dev/null 2>&1 || {{ echo fail=git_missing; exit 0; }}\n\
             if [ ! -d {bare} ]; then git init --bare -q {bare} >/dev/null 2>&1 || {{ echo fail=init_failed; exit 0; }}; fi\n\
             echo ready=1\n",
            root = root.display(),
        );
        let answer = self.remote(ssh, &probe).await?;
        if let Some(reason) = kv(&answer.stdout, "fail") {
            return Err(Failure::new(
                reason,
                "runner could not prepare its repository",
            ));
        }
        if kv(&answer.stdout, "staged").is_some() {
            return Ok(());
        }
        if kv(&answer.stdout, "ready").is_none() {
            return Err(script_failure(&answer));
        }
        self.check_local_rev(&request.repo, rev).await?;
        self.push(&request.repo, ssh, &bare, rev, job).await?;
        let add = format!(
            "set -u\n\
             git -C {bare} worktree prune >/dev/null 2>&1\n\
             if [ -e {wd} ]; then\n\
             if [ -f {wd}/.git ]; then\n\
             h=$(git -C {wd} rev-parse HEAD 2>/dev/null)\n\
             if [ \"$h\" = {rev} ]; then echo staged=1; exit 0; fi\n\
             git -C {bare} worktree remove --force {wd} >/dev/null 2>&1\n\
             fi\n\
             if [ -e {wd} ]; then rmdir {wd} 2>/dev/null; fi\n\
             if [ -e {wd} ]; then echo fail=workdir_busy; exit 0; fi\n\
             fi\n\
             git -C {bare} worktree add --detach -q {wd} refs/canterlot/{job} >/dev/null 2>&1 || {{ echo fail=worktree_failed; exit 0; }}\n\
             h=$(git -C {wd} rev-parse HEAD 2>/dev/null)\n\
             if [ \"$h\" = {rev} ]; then echo staged=1; else echo fail=wrong_rev; fi\n"
        );
        let answer = self.remote(ssh, &add).await?;
        if let Some(reason) = kv(&answer.stdout, "fail") {
            return Err(Failure::new(reason, "runner could not create the worktree"));
        }
        if kv(&answer.stdout, "staged").is_some() {
            Ok(())
        } else {
            Err(script_failure(&answer))
        }
    }

    async fn stage_local(
        &self,
        request: &StageRequest,
        rev: &str,
        workdir: &Path,
    ) -> Result<(), Failure> {
        let repo = &request.repo;
        if worktree_head(workdir)
            .await
            .is_some_and(|head| head.eq_ignore_ascii_case(rev))
        {
            return Ok(());
        }
        self.check_local_rev(repo, rev).await?;
        let limit = self.settings.ssh_limit;
        git_local(repo, &["worktree", "prune"], limit).await.ok();
        if tokio::fs::symlink_metadata(workdir).await.is_ok() {
            if workdir.join(".git").is_file() {
                git_local(
                    repo,
                    &[
                        "worktree",
                        "remove",
                        "--force",
                        &workdir.display().to_string(),
                    ],
                    limit,
                )
                .await
                .ok();
            }
            // Остаток от прерванного создания: пустой каталог без записи.
            tokio::fs::remove_dir(workdir).await.ok();
            if tokio::fs::symlink_metadata(workdir).await.is_ok() {
                return Err(Failure::new(
                    "workdir_busy",
                    "workdir exists and is not empty",
                ));
            }
        }
        let parent = workdir.parent().unwrap_or(Path::new("/"));
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            Failure::new(
                "mkdir_failed",
                format!("cannot create {}: {}", parent.display(), error.kind()),
            )
        })?;
        let output = git_local(
            repo,
            &[
                "worktree",
                "add",
                "--detach",
                "-q",
                &workdir.display().to_string(),
                rev,
            ],
            limit,
        )
        .await?;
        if output.code != 0 {
            return Err(Failure::new("worktree_failed", first_line(&output.stderr)));
        }
        match worktree_head(workdir).await {
            Some(head) if head.eq_ignore_ascii_case(rev) => Ok(()),
            _ => Err(Failure::new(
                "wrong_rev",
                "worktree is not at the requested sha",
            )),
        }
    }

    /// Снимает рабочую копию и ссылку. Копию, которая пережила удаление, оставляет.
    pub async fn cleanup(&self, request: &CleanupRequest) -> Result<(), Failure> {
        let job = parse_job(&request.job_id)?;
        let domain = parse_domain(&request.domain)?;
        valid_key(&request.repo_key)?;
        let place = self.place(&request.target_machine_id, &request.target_root_dir)?;
        let workdir = PathBuf::from(&request.workdir);
        match place {
            Place::Runner { ssh, root } => {
                let want = root
                    .join("jobs")
                    .join(domain)
                    .join(job_dir(job, request.attempt));
                if workdir != want {
                    return Err(Failure::new(
                        "bad_workdir",
                        "workdir is not <root>/jobs/<domain>/<job>",
                    ));
                }
                let bare = format!("{}/repos/{domain}/{}.git", root.display(), request.repo_key);
                let wd = workdir.display();
                let script = format!(
                    "if [ -d {bare} ]; then\n\
                     if [ -e {wd} ]; then git -C {bare} worktree remove --force {wd} >/dev/null 2>&1; fi\n\
                     git -C {bare} worktree prune >/dev/null 2>&1\n\
                     git -C {bare} update-ref -d refs/canterlot/{job} >/dev/null 2>&1\n\
                     if [ -z \"$(git -C {bare} for-each-ref --count=1 refs/canterlot 2>/dev/null)\" ]; then\n\
                     git -C {bare} gc --prune=now --quiet >/dev/null 2>&1\n\
                     fi\n\
                     fi\n\
                     if [ -e {wd} ]; then rmdir {wd} 2>/dev/null; fi\n\
                     if [ -e {wd} ]; then echo fail=workdir_remains; else echo cleaned=1; fi\n"
                );
                let answer = self.remote(&ssh, &script).await?;
                if let Some(reason) = kv(&answer.stdout, "fail") {
                    return Err(Failure::new(
                        reason,
                        "the worktree survived removal and was left in place",
                    ));
                }
                if kv(&answer.stdout, "cleaned").is_some() {
                    Ok(())
                } else {
                    Err(script_failure(&answer))
                }
            }
            Place::Carrier => {
                if !carrier_workdir_shape(&workdir, job, request.attempt) {
                    return Err(Failure::new(
                        "bad_workdir",
                        "workdir is not under .canterlot-jobs",
                    ));
                }
                self.cleanup_local(&workdir).await
            }
        }
    }

    async fn cleanup_local(&self, workdir: &Path) -> Result<(), Failure> {
        let limit = self.settings.ssh_limit;
        if tokio::fs::symlink_metadata(workdir).await.is_err() {
            return Ok(());
        }
        let mut common = None;
        if workdir.join(".git").is_file() {
            let output = git_local(
                workdir,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                limit,
            )
            .await?;
            if output.code == 0 {
                common = Some(PathBuf::from(output.stdout.trim()));
            }
        }
        if let Some(common) = &common {
            let dir = common.display().to_string();
            let target = workdir.display().to_string();
            run_git(
                &["--git-dir", &dir, "worktree", "remove", "--force", &target],
                limit,
            )
            .await
            .ok();
            run_git(&["--git-dir", &dir, "worktree", "prune"], limit)
                .await
                .ok();
        }
        tokio::fs::remove_dir(workdir).await.ok();
        if tokio::fs::symlink_metadata(workdir).await.is_ok() {
            return Err(Failure::new(
                "workdir_remains",
                "the worktree survived removal and was left in place",
            ));
        }
        Ok(())
    }

    pub(super) async fn remote(&self, ssh: &Ssh, script: &str) -> Result<Output, Failure> {
        ssh.script(script, self.settings.ssh_limit)
            .await
            .map_err(|_| Failure::new("ssh_unreachable", "runner is not reachable over ssh"))
    }

    async fn check_local_rev(&self, repo: &Path, rev: &str) -> Result<(), Failure> {
        let limit = self.settings.ssh_limit;
        let git_dir = git_local(repo, &["rev-parse", "--git-dir"], limit).await?;
        if git_dir.code != 0 {
            return Err(Failure::new("not_a_repo", "repo is not a git repository"));
        }
        let object = git_local(
            repo,
            &["cat-file", "-e", &format!("{rev}^{{commit}}")],
            limit,
        )
        .await?;
        if object.code != 0 {
            return Err(Failure::new(
                "unknown_rev",
                "the commit is not in the repository",
            ));
        }
        Ok(())
    }

    async fn push(
        &self,
        repo: &Path,
        ssh: &Ssh,
        bare: &str,
        rev: &str,
        job: Uuid,
    ) -> Result<(), Failure> {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(repo)
            .args(["push", "--no-verify", "--quiet"])
            .arg(ssh.destination(bare))
            .arg(format!("{rev}:refs/canterlot/{job}"))
            .env("GIT_SSH_COMMAND", ssh.command_line())
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let output = tokio::time::timeout(self.settings.push_limit, command.output())
            .await
            .map_err(|_| Failure::new("push_timeout", "git push did not finish in time"))?
            .map_err(|_| Failure::new("push_failed", "git could not be started"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(Failure::new(
                "push_failed",
                first_line(&String::from_utf8_lossy(&output.stderr)),
            ))
        }
    }
}

/// Цель годится, только если она даёт `ssh` безопасный адресат: псевдоним из конфигурации
/// или проверенное облачное подключение. Всё это уже отсекалось при записи, здесь — граница.
fn valid_target(target: &RunnerTarget) -> bool {
    match &target.host {
        Some(host) => valid_host(host),
        None => valid_alias(&target.alias),
    }
}

fn valid_host(host: &RunnerHost) -> bool {
    !host.host.is_empty()
        && !host.host.starts_with('-')
        && host
            .host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
        && !host.user.is_empty()
        && !host.user.starts_with('-')
        && host
            .user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !host.known_hosts.as_os_str().is_empty()
}

fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && !alias.starts_with('-')
        && alias
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@'))
}

pub(super) fn parse_job(text: &str) -> Result<Uuid, Failure> {
    // Канонический вид с дефисами: `Uuid::parse_str` принимает и другие записи.
    match Uuid::parse_str(text) {
        Ok(id) if id.hyphenated().to_string() == text => Ok(id),
        _ => Err(Failure::new("bad_job_id", "job id is not a UUID")),
    }
}

fn parse_rev(text: &str) -> Result<String, Failure> {
    if text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(text.to_ascii_lowercase())
    } else {
        Err(Failure::new("bad_rev", "rev must be a full 40-hex sha"))
    }
}

pub(super) fn parse_domain(text: &str) -> Result<&'static str, Failure> {
    match text {
        "personal" => Ok("personal"),
        "work" => Ok("work"),
        _ => Err(Failure::new(
            "bad_domain",
            "domain must be personal or work",
        )),
    }
}

fn valid_key(key: &str) -> Result<(), Failure> {
    if !key.is_empty()
        && key != "."
        && key != ".."
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(Failure::new(
            "bad_repo_key",
            "repo key must match [A-Za-z0-9._-]+",
        ))
    }
}

fn has_dot_dot(path: &Path) -> bool {
    path.components().any(|part| {
        matches!(
            part,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    })
}

/// Имя каталога попытки, как его строит хаб: `<job>` у первой попытки (и у хаба до повторов,
/// который шлёт 0) и `<job>-<попытка>` у повтора. Повтор не делит каталог с прежней попыткой.
pub fn job_dir(job: Uuid, attempt: u32) -> String {
    if attempt <= 1 {
        job.to_string()
    } else {
        format!("{job}-{attempt}")
    }
}

/// `<что-то>/.canterlot-jobs/<репозиторий>/<каталог попытки>`, без `.` и `..`.
pub(super) fn carrier_workdir_shape(workdir: &Path, job: Uuid, attempt: u32) -> bool {
    if !workdir.is_absolute() || has_dot_dot(workdir) || workdir.to_str().is_none() {
        return false;
    }
    let mut parts = workdir.components().rev();
    let last = parts
        .next()
        .and_then(|part| part.as_os_str().to_str().map(str::to_owned));
    let name = parts.next();
    let marker = parts.next();
    last.as_deref() == Some(job_dir(job, attempt).as_str())
        && name.is_some()
        && marker.is_some_and(|part| part.as_os_str() == WORKDIRS)
}

pub(super) fn kv<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    output
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

pub(super) fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    line.chars().take(200).collect()
}

pub(super) fn script_failure(answer: &Output) -> Failure {
    Failure::new(
        "script_failed",
        format!("exit {}: {}", answer.code, first_line(&answer.stderr)),
    )
}

pub(super) fn sh_quote(text: &str) -> String {
    if !text.is_empty()
        && text.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-' | b'+' | b'@' | b'=')
        })
    {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

pub(super) struct Local {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub(super) async fn run_git(args: &[&str], limit: Duration) -> Result<Local, Failure> {
    let mut command = Command::new("git");
    command
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(limit, command.output())
        .await
        .map_err(|_| Failure::new("git_timeout", "git did not finish in time"))?
        .map_err(|_| Failure::new("git_missing", "git could not be started"))?;
    Ok(Local {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

pub(super) async fn git_local(
    dir: &Path,
    args: &[&str],
    limit: Duration,
) -> Result<Local, Failure> {
    let dir = dir.display().to_string();
    let mut all = vec!["-C", dir.as_str()];
    all.extend_from_slice(args);
    run_git(&all, limit).await
}

/// `HEAD` рабочей копии, только если каталог сам является рабочей копией (есть `.git`-файл).
async fn worktree_head(workdir: &Path) -> Option<String> {
    if !workdir.join(".git").is_file() {
        return None;
    }
    let output = git_local(workdir, &["rev-parse", "HEAD"], Duration::from_secs(30))
        .await
        .ok()?;
    (output.code == 0).then(|| output.stdout.trim().to_owned())
}
