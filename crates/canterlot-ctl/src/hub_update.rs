//! `canterlot update` на хосте хаба. Фазы записываются в запись операции до начала работы:
//! `prepare` (ничего не остановлено), `fence` (API в обслуживании, gRPC не опубликован,
//! хаб остановлен), `dump`, `candidate` (новый хаб без gRPC), при неудаче кандидата —
//! `rollback_restore`, `rollback_swap`, `rollback_start`; при успехе `admit` (точка фиксации,
//! дальше только вперёд) и `post`. Продолжение после сбоя идёт с записанной фазы; состояние,
//! которое нельзя однозначно продолжить, — отказ с ручной проверкой, а не догадка.
//!
//! Живая база `canterlot` не удаляется ни на одном пути: откат восстанавливает дамп в
//! отдельную базу и меняет имена одной транзакцией, мигрированная база остаётся рядом.

use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use crate::{
    cli::{UpdateArgs, Version},
    db::{self, LIVE_DB, ScratchDb},
    error::CtlError,
    fsx,
    identity::{check_identity, db_identity, db_identity_of},
    install::Host,
    layout::Layout,
    ops::{self, DbIdentity, OpKind, OpRecord, OpResult, Traffic},
    release::{self, ReleaseSource, Staged},
    remove,
    render::{self, HUB_BIN, HUB_CONFIG, HUB_IMAGE},
    runner::{Cmd, Runner},
    stack::{cmd, compose, hub_exec, run_ok, stdout},
    state::InstallState,
    web,
};

const GIB: u64 = 1 << 30;
/// Сколько дампов успешных обновлений хранится.
const KEEP_DUMPS: usize = 5;
/// Сколько деревьев PWA хранится (новое плюс источники чанков).
const KEEP_WEB: usize = 4;

#[derive(Debug)]
pub enum UpdateReport {
    UpToDate(Version),
    Finished(Box<OpRecord>),
    /// Обновление воркера этого хоста от имени его учётной записи.
    Worker {
        user: String,
        status: i32,
        output: String,
        errors: String,
    },
}

impl UpdateReport {
    pub fn succeeded(&self) -> bool {
        match self {
            Self::UpToDate(_) => true,
            Self::Finished(rec) => rec.result == Some(OpResult::Done),
            Self::Worker { status, .. } => *status == 0,
        }
    }
}

fn opt_version(v: &Option<Version>) -> String {
    v.as_ref()
        .map(Version::to_string)
        .unwrap_or_else(|| "?".into())
}

impl fmt::Display for UpdateReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rec = match self {
            Self::UpToDate(v) => return writeln!(f, "The hub is already at {v}."),
            Self::Worker {
                user,
                status,
                output,
                errors,
            } => {
                // Причина отказа уходит в stderr, без неё отчёт бесполезен.
                write!(f, "{output}")?;
                write!(f, "{errors}")?;
                if *status != 0 {
                    writeln!(
                        f,
                        "The worker update as {user} exited with {status}; run `canterlot update` as {user} to retry."
                    )?;
                }
                return Ok(());
            }
            Self::Finished(rec) => rec,
        };
        let (from, to) = (opt_version(&rec.previous), opt_version(&rec.target));
        match &rec.result {
            Some(OpResult::Done) => writeln!(f, "The hub is updated from {from} to {to}.")?,
            Some(OpResult::FailedBeforeChange { reason }) => writeln!(
                f,
                "The update to {to} failed before any change: {reason}\nThe hub runs {from}."
            )?,
            Some(OpResult::RolledBack { reason }) => {
                writeln!(
                    f,
                    "The update to {to} failed and was rolled back: {reason}\nThe hub runs {from} on the database restored from the pre-update dump."
                )?;
                if let Some(failed) = &rec.details.failed_db {
                    writeln!(
                        f,
                        "The database the failed candidate migrated is kept as {failed} until the next successful update."
                    )?;
                }
            }
            Some(OpResult::NeedsOwner { steps }) => {
                writeln!(
                    f,
                    "The update to {to} stopped and needs manual steps (operation {}):",
                    rec.id
                )?;
                for (n, step) in steps.iter().enumerate() {
                    writeln!(f, "  {}. {step}", n + 1)?;
                }
            }
            None => writeln!(
                f,
                "The update to {to} is unfinished at phase {}.",
                rec.phase
            )?,
        }
        if let Some(dump) = &rec.dump {
            writeln!(f, "Pre-update dump: {}", dump.display())?;
        }
        for warning in &rec.details.warnings {
            writeln!(f, "Warning: {warning}")?;
        }
        Ok(())
    }
}

/// Версия — имя каталога, на который указывает ссылка `current`.
fn link_version(link: &Path) -> Result<Version, CtlError> {
    let target = fs::read_link(link).map_err(|err| {
        CtlError::Other(format!("hub_current_missing: {}: {err}", link.display()))
    })?;
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| CtlError::BadOutput(format!("{} target", link.display())))?;
    Version::parse(name)
}

/// Операции хоста хаба идут от root: без него утилита перезапускает себя через sudo с теми
/// же аргументами (как `install`). Путь воркера сюда не попадает: он работает от пользователя.
pub(crate) fn elevate(host: &Host) -> Result<(), CtlError> {
    if host.is_root {
        return Ok(());
    }
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    crate::root::ensure_root(&argv)
}

/// Куда идёт команда хоста: в стек хаба (только root) или в воркер учётной записи.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostRoute {
    Hub,
    Worker,
    /// Хост хаба, но вызывающий — обычный пользователь без своего воркера.
    NeedsRoot,
}

/// Правило маршрутизации без окружения, чтобы проверять его на временных каталогах.
/// Обычный вызов управляет своим воркером; `sudo` — всем хостом.
pub fn route_host(
    layout: &Layout,
    worker_layout: &Layout,
    is_root: bool,
    worker_only: bool,
    in_unit: bool,
) -> HostRoute {
    if worker_only || in_unit {
        return HostRoute::Worker;
    }
    let hub = layout.install_toml().is_file();
    if hub && is_root {
        return HostRoute::Hub;
    }
    if worker_layout
        .worker_config_dir()
        .join("worker.toml")
        .is_file()
    {
        return HostRoute::Worker;
    }
    if hub {
        return HostRoute::NeedsRoot;
    }
    HostRoute::Worker
}

/// То же правило с отметками «вызов от имени воркера» и «внутри временного юнита».
pub fn host_route(layout: &Layout, worker_layout: &Layout, is_root: bool) -> HostRoute {
    route_host(
        layout,
        worker_layout,
        is_root,
        std::env::var_os(WORKER_ONLY_ENV).is_some(),
        std::env::var(crate::worker::IN_UNIT_ENV).as_deref() == Ok("1"),
    )
}

/// Отметка вызова от имени воркера на хосте хаба: такой вызов обслуживает только воркер.
pub const WORKER_ONLY_ENV: &str = "CANTERLOT_CTL_WORKER_ONLY";

/// Хост хаба: есть `install.toml`, и это не вызов утилиты от имени воркера (ни прямой,
/// ни изнутри транзакции воркера в `systemd-run --user`).
pub fn hub_host(layout: &Layout) -> bool {
    layout.install_toml().is_file()
        && std::env::var_os(WORKER_ONLY_ENV).is_none()
        && std::env::var(crate::worker::IN_UNIT_ENV).as_deref() != Ok("1")
}

/// `canterlot <args>` от имени учётной записи воркера этого хоста: с её домашним каталогом
/// и пользовательским менеджером systemd, с отметкой «только воркер».
pub(crate) async fn run_as_worker(
    runner: &dyn Runner,
    layout: &Layout,
    state: &InstallState,
    user: &str,
    args: &[&str],
) -> Result<crate::runner::Output, CtlError> {
    let id = run_ok(runner, "id -u", cmd("id", &["-u", user])).await?;
    let uid: u32 = stdout(&id)
        .parse()
        .map_err(|_| CtlError::UserNotFound(user.into()))?;
    let home = state
        .worker_home
        .clone()
        .ok_or_else(|| CtlError::UserNotFound(user.into()))?;
    let mut c = cmd(&layout.ctl_link().display().to_string(), args);
    c.as_user = Some(user.to_owned());
    c.env = vec![
        ("HOME".into(), home.display().to_string()),
        ("USER".into(), user.to_owned()),
        ("XDG_RUNTIME_DIR".into(), format!("/run/user/{uid}")),
        (
            "DBUS_SESSION_BUS_ADDRESS".into(),
            format!("unix:path=/run/user/{uid}/bus"),
        ),
        (WORKER_ONLY_ENV.into(), "1".into()),
    ];
    runner.run(c).await
}

/// Обновление стека хаба. Возвращает итог операции; `Err` — отказ до начала операции,
/// неоднозначная запись или прерванная фиксация, которую надо довести повтором.
pub async fn update_hub(
    args: &UpdateArgs,
    layout: &Layout,
    runner: &dyn Runner,
    src: &ReleaseSource,
    host: &Host,
) -> Result<UpdateReport, CtlError> {
    elevate(host)?;
    let ops_dir = layout.ops_dir();
    fsx::ensure_dir(&layout.root, &ops_dir, 0o700)?;
    let _lock = ops::lock(&ops_dir)?;
    let state = InstallState::load(layout)?
        .filter(|s| s.components.hub)
        .ok_or(CtlError::HubNotInstalled)?;
    let job = Job {
        layout,
        runner,
        host,
        src,
        args,
        state,
        ops_dir,
    };

    if let Some(mut rec) = ops::unfinished(&job.ops_dir)? {
        // Незавершённая операция продолжается, а не заменяется новой.
        if rec.kind != OpKind::Update {
            return Err(CtlError::Unfinished(rec));
        }
        let wanted = match &args.from_dir {
            Some(dir) => Some(release::dir_version(dir)?),
            None => args.version.clone(),
        };
        if wanted.is_some() && wanted != rec.target {
            return Err(CtlError::Unfinished(rec));
        }
        job.drive(&mut rec).await?;
        return Ok(UpdateReport::Finished(Box::new(rec)));
    }

    if !args.force
        && let Some(last) = ops::latest(&job.ops_dir)?
        && matches!(last.result, Some(OpResult::NeedsOwner { .. }))
    {
        return Err(CtlError::NeedsOwnerPending(last.id.0));
    }

    let current = link_version(&layout.hub_current())?;
    let target = match &args.from_dir {
        Some(dir) => {
            let found = release::dir_version(dir)?;
            if let Some(want) = &args.version
                && want != &found
            {
                return Err(CtlError::Other(format!(
                    "version_mismatch: --version {want} but {} holds {found}",
                    dir.display()
                )));
            }
            found
        }
        None => release::resolve(src, args.version.as_ref()).await?,
    };
    if target == current {
        return Ok(UpdateReport::UpToDate(current));
    }
    if target.key() < current.key() {
        return Err(CtlError::DowngradeRefused {
            target: target.to_string(),
            current: current.to_string(),
        });
    }
    let mut rec = OpRecord::new(OpKind::Update, Some(target));
    rec.previous = Some(current);
    let mut rec = ops::begin(&job.ops_dir, rec)?;
    job.drive(&mut rec).await?;
    Ok(UpdateReport::Finished(Box::new(rec)))
}

/// `canterlot update` на хосте: сначала хаб, затем воркер этого хоста. Сбой хаба
/// пропускает воркер (правило «сначала хаб»).
pub async fn update(
    args: UpdateArgs,
    layout: &Layout,
    runner: &dyn Runner,
    src: &ReleaseSource,
    host: &Host,
) -> Result<Vec<UpdateReport>, CtlError> {
    // install.toml читает только root: права повышаются до его чтения.
    elevate(host)?;
    let state = InstallState::load(layout)?
        .filter(|s| s.components.hub)
        .ok_or(CtlError::HubNotInstalled)?;
    let report = update_hub(&args, layout, runner, src, host).await?;
    let target = match &report {
        UpdateReport::UpToDate(v) => Some(v.clone()),
        UpdateReport::Finished(rec) if report.succeeded() => rec.target.clone(),
        _ => None,
    };
    let mut reports = vec![report];
    // Воркер этого хоста — после хаба и только при его успехе, той же версии, от имени
    // своей учётной записи (его транзакция идёт в её `systemd-run --user`).
    if let (Some(target), true, Some(user)) =
        (target, state.components.worker, state.worker_user.clone())
    {
        let from = args.from_dir.as_ref().map(|d| d.display().to_string());
        let mut child = vec!["update", "--version", target.as_str()];
        if let Some(dir) = &from {
            child.extend(["--from-dir", dir.as_str()]);
        }
        if args.force {
            child.push("--force");
        }
        // Воркер на этом хосте обновляется сразу после перезапуска хаба: он должен
        // дождаться, пока увидит новую версию, иначе проверка «хаб первым» сорвётся.
        child.push("--hub-chain");
        let out = run_as_worker(runner, layout, &state, &user, &child).await?;
        reports.push(UpdateReport::Worker {
            user,
            status: out.status,
            output: String::from_utf8_lossy(&out.stdout).into_owned(),
            errors: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(reports)
}

/// Состояние контейнера из `docker compose ps --all --format {{.State}} hub`: Compose печатает
/// одно состояние в строке. Пустой вывод означает, что контейнера ещё нет.
/// `--all` обязателен — без него stopped-контейнер не попадает в вывод.
fn container_state(text: &str) -> Option<String> {
    text.lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
}

/// Последние строки журнала кандидата: упавшая миграция должна называть себя в причине отказа.
async fn candidate_log_tail(runner: &dyn Runner, layout: &Layout) -> String {
    let out = match runner
        .run(compose(
            layout,
            &["logs", "--no-color", "--tail", "20", "hub"],
        ))
        .await
    {
        Ok(out) => out,
        Err(err) => return format!("logs unavailable: {err}"),
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let errors = String::from_utf8_lossy(&out.stderr);
    if !errors.trim().is_empty() {
        if !text.trim().is_empty() {
            text.push('\n');
        }
        text.push_str(&errors);
    }
    let text = text.trim().to_owned();
    if text.is_empty() {
        "no logs from the candidate".into()
    } else {
        format!("Candidate logs (last 20 lines):\n{text}")
    }
}

/// Готовность хаба по `canterlot-hub probe` внутри контейнера: код 0, ready и нужная версия.
/// Число попыток и пауза — из `Host`, в тестах пауза нулевая. Каждая попытка сначала
/// смотрит состояние контейнера: окончательно упавший кандидат не ждёт остальных попыток,
/// а временный `restarting` (у обычного стека с `unless-stopped`) продолжает нормальный опрос.
pub async fn probe(
    runner: &dyn Runner,
    layout: &Layout,
    host: &Host,
    version: &Version,
) -> Result<(), CtlError> {
    #[derive(serde::Deserialize)]
    struct Probe {
        ready: bool,
        version: String,
    }
    let mut last = String::from("no attempt");
    for attempt in 0..host.retries.max(1) {
        if attempt > 0 {
            tokio::time::sleep(host.retry_delay).await;
        }
        if let Ok(out) = runner
            .run(compose(
                layout,
                &["ps", "--all", "--format", "{{.State}}", "hub"],
            ))
            .await
            && let Some(state) = container_state(&stdout(&out))
            && matches!(state.as_str(), "exited" | "dead")
        {
            return Err(CtlError::NotReady(format!(
                "candidate hub container is {state}; {}",
                candidate_log_tail(runner, layout).await
            )));
        }
        let out = runner.run(hub_exec(layout, &["probe"])).await?;
        match serde_json::from_str::<Probe>(&stdout(&out)) {
            Ok(p) if out.status == 0 && p.ready && p.version == version.as_str() => return Ok(()),
            Ok(p) => {
                last = format!(
                    "hub probe exited with {}: ready={}, version={} (want {version})",
                    out.status, p.ready, p.version
                )
            }
            Err(_) => last = format!("hub probe exited with {} without a verdict", out.status),
        }
    }
    Err(CtlError::NotReady(format!(
        "{last}\n{}",
        candidate_log_tail(runner, layout).await
    )))
}

struct Job<'a> {
    layout: &'a Layout,
    runner: &'a dyn Runner,
    host: &'a Host,
    src: &'a ReleaseSource,
    args: &'a UpdateArgs,
    state: InstallState,
    ops_dir: PathBuf,
}

impl Job<'_> {
    fn target(&self, rec: &OpRecord) -> Result<Version, CtlError> {
        rec.target
            .clone()
            .ok_or_else(|| self.ambiguous_err(rec, "the record has no target version"))
    }

    fn previous(&self, rec: &OpRecord) -> Result<Version, CtlError> {
        rec.previous
            .clone()
            .ok_or_else(|| self.ambiguous_err(rec, "the record has no previous version"))
    }

    fn dc(&self) -> String {
        format!(
            "docker compose -f {}",
            self.layout.compose_dir().join("compose.yaml").display()
        )
    }

    fn ambiguous_err(&self, rec: &OpRecord, why: &str) -> CtlError {
        let mut rec = rec.clone();
        rec.details.manual = vec![why.to_owned()];
        CtlError::Ambiguous(rec)
    }

    /// Записывает ручную проверку в запись (она остаётся незавершённой) и отказывает.
    fn ambiguous(&self, rec: &mut OpRecord, manual: Vec<String>) -> CtlError {
        rec.details.manual = manual;
        if let Err(err) = ops::checkpoint(&self.ops_dir, rec) {
            return err;
        }
        CtlError::Ambiguous(rec.clone())
    }

    fn advance(&self, rec: &mut OpRecord, phase: &str) -> Result<(), CtlError> {
        ops::advance(&self.ops_dir, rec, phase)
    }

    fn finish(&self, rec: &mut OpRecord, result: OpResult) -> Result<(), CtlError> {
        ops::finish(&self.ops_dir, rec, result)
    }

    async fn drive(&self, rec: &mut OpRecord) -> Result<(), CtlError> {
        loop {
            match rec.phase.as_str() {
                "begin" | "prepare" => self.prepare(rec).await?,
                "fence" => self.fence(rec).await?,
                "dump" => self.dump(rec).await?,
                "unfence" => self.unfence(rec).await?,
                "candidate" => self.candidate(rec).await?,
                "rollback_restore" => self.rollback_restore(rec).await?,
                "rollback_swap" => self.rollback_swap(rec).await?,
                "rollback_start" => self.rollback_start(rec).await?,
                "admit" => self.admit(rec).await?,
                "post" => self.post(rec).await?,
                "done" => return Ok(()),
                other => {
                    let manual = vec![format!(
                        "unknown phase {other}; inspect {} and the stack ({} ps) and finish by hand",
                        self.ops_dir.join(format!("{}.json", rec.id)).display(),
                        self.dc()
                    )];
                    return Err(self.ambiguous(rec, manual));
                }
            }
        }
    }

    fn write_compose(&self, v: &Version) -> Result<(), CtlError> {
        let dir = self.layout.compose_dir();
        fsx::write_atomic_mode(
            &dir.join("compose.yaml"),
            render::render_compose(&self.state, v).as_bytes(),
            0o600,
        )?;
        fsx::write_atomic_mode(
            &dir.join("compose.fenced.yaml"),
            render::render_compose_fenced().as_bytes(),
            0o600,
        )
    }

    /// `docker compose` с добавкой, снимающей публикацию gRPC.
    fn compose_fenced(&self, args: &[&str]) -> Cmd {
        let dir = self.layout.compose_dir();
        let mut all = vec![
            "compose".to_owned(),
            "-f".into(),
            dir.join("compose.yaml").display().to_string(),
            "-f".into(),
            dir.join("compose.fenced.yaml").display().to_string(),
        ];
        all.extend(args.iter().map(|a| (*a).to_owned()));
        let mut c = cmd("docker", &[]);
        c.args = all;
        c
    }

    async fn stop_hub(&self) -> Result<(), CtlError> {
        run_ok(
            self.runner,
            "compose stop hub",
            compose(self.layout, &["stop", "hub"]),
        )
        .await?;
        Ok(())
    }

    // ---- prepare -------------------------------------------------------------------

    async fn prepare(&self, rec: &mut OpRecord) -> Result<(), CtlError> {
        if rec.phase != "prepare" {
            self.advance(rec, "prepare")?;
        }
        match self.prepare_inner(rec).await {
            Ok(()) => {
                // Трафик отмечается закрытым до первого действия фазы fence.
                rec.traffic = Traffic::Fenced;
                self.advance(rec, "fence")
            }
            Err(err) => {
                self.finish(
                    rec,
                    OpResult::FailedBeforeChange {
                        reason: err.to_string(),
                    },
                )?;
                Err(err)
            }
        }
    }

    async fn prepare_inner(&self, rec: &mut OpRecord) -> Result<(), CtlError> {
        let target = self.target(rec)?;
        let assets = release::asset_names(&target);
        let names = [
            assets.hub.as_str(),
            assets.web.as_str(),
            assets.csp.as_str(),
            assets.ctl.as_str(),
            assets.sums.as_str(),
        ];
        let (staged, downloaded) = match &self.args.from_dir {
            Some(dir) => (release::from_dir(dir, &names)?, false),
            None => {
                fsx::ensure_dir(&self.layout.root, &self.layout.staging(), 0o700)?;
                let staged =
                    release::fetch(self.src, &target, &names, &self.layout.staging()).await?;
                (staged, true)
            }
        };
        let result = async {
            if staged.version != target {
                return Err(CtlError::Other(format!(
                    "version_mismatch: the release holds {}, the operation targets {target}",
                    staged.version
                )));
            }
            self.self_update(rec, &staged, &target)?;
            self.stage(&staged, &target)?;
            self.checks(rec, &staged, &target).await
        }
        .await;
        if downloaded {
            // Каталог загрузки создан этим запуском; удаляется по точному пути.
            let _ = fs::remove_dir_all(&staged.dir);
        }
        result
    }

    /// Новая утилита ставится до любых остановок и перезапускает операцию собой: дальше
    /// обновление ведёт код целевого выпуска. Повторный запуск видит ту же запись.
    fn self_update(
        &self,
        rec: &OpRecord,
        staged: &Staged,
        target: &Version,
    ) -> Result<(), CtlError> {
        if self.host.self_exe.is_none() || env!("CARGO_PKG_VERSION") == target.as_str() {
            return Ok(());
        }
        // Уже перезапущены для этой операции: не зацикливаемся, если версия бинаря иная.
        if std::env::var("CANTERLOT_REEXEC").ok().as_deref() == Some(rec.id.0.as_str()) {
            return Ok(());
        }
        let assets = release::asset_names(target);
        let dir = self.layout.ctl_release(target);
        fsx::ensure_dir(
            &self.layout.root,
            dir.parent().unwrap_or(&self.layout.root),
            0o755,
        )?;
        web::extract_once(staged_file(staged, &assets.ctl)?, &dir)?;
        let exe = dir.join("canterlot");
        if !exe.is_file() {
            return Err(CtlError::AssetMissing(format!(
                "{} in {}",
                "canterlot", assets.ctl
            )));
        }
        let link = self.layout.ctl_link();
        fsx::ensure_dir(
            &self.layout.root,
            link.parent().unwrap_or(&self.layout.root),
            0o755,
        )?;
        fsx::replace_symlink(&exe, &link)?;
        use std::os::unix::process::CommandExt as _;
        let err = std::process::Command::new(&exe)
            .args(std::env::args_os().skip(1))
            .env("CANTERLOT_REEXEC", &rec.id.0)
            .exec();
        Err(CtlError::Other(format!("reexec_failed: {err}")))
    }

    fn stage(&self, staged: &Staged, target: &Version) -> Result<(), CtlError> {
        let assets = release::asset_names(target);
        let hub = self.layout.hub_release(target);
        fsx::ensure_dir(
            &self.layout.root,
            hub.parent().unwrap_or(&self.layout.root),
            0o755,
        )?;
        web::extract_once(staged_file(staged, &assets.hub)?, &hub.join("bin"))?;
        // CSP выпуска лежит рядом с бинарями хаба: в раздаваемое дерево PWA он не попадает.
        let csp = fs::read_to_string(staged_file(staged, &assets.csp)?)?;
        render::csp_for_origin(&csp, self.state.origin.as_deref().unwrap_or_default())?;
        fsx::write_atomic_mode(&hub.join("csp.txt"), csp.as_bytes(), 0o644)?;
        let web_dir = self.layout.web_release(target);
        fsx::ensure_dir(
            &self.layout.root,
            web_dir.parent().unwrap_or(&self.layout.root),
            0o755,
        )?;
        web::extract_once(staged_file(staged, &assets.web)?, &web_dir)?;
        Ok(())
    }

    async fn checks(
        &self,
        rec: &mut OpRecord,
        staged: &Staged,
        target: &Version,
    ) -> Result<(), CtlError> {
        // Миграции: применённые в базе — начало списка целевого бинаря.
        let release_dir = self.layout.hub_release(target);
        let mount = format!("{}:/opt/canterlot/hub/current:ro", release_dir.display());
        let user = format!("{}:{}", self.state.hub_uid, self.state.hub_gid);
        let out = run_ok(
            self.runner,
            "canterlot-hub migrations manifest",
            cmd(
                "docker",
                &[
                    "run",
                    "--rm",
                    "--network",
                    "none",
                    "--user",
                    &user,
                    "-v",
                    &mount,
                    HUB_IMAGE,
                    HUB_BIN,
                    "migrations",
                    "manifest",
                    "--json",
                ],
            ),
        )
        .await?;
        let applied = db::applied_migrations(self.runner, self.layout, LIVE_DB).await?;
        db::compare_migrations(&String::from_utf8_lossy(&out.stdout), &applied)
            .map_err(CtlError::MigrationsIncompatible)?;
        rec.details.migrations = applied;
        rec.details.tables = Some(db::table_count(self.runner, self.layout, LIVE_DB).await?);

        // Тождество: та же база, что после прошлой операции.
        let live = db_identity(self.runner, self.layout).await?;
        if let Some(expected) = ops::last_identity(&self.ops_dir)? {
            if self.args.force {
                // После ручного восстановления oid другой; UUID установки должны совпасть.
                check_identity(
                    &DbIdentity {
                        database_oid: live.database_oid,
                        ..expected
                    },
                    &live,
                )?;
            } else {
                check_identity(&expected, &live)?;
            }
        }
        rec.db_identity = Some(live);
        ops::checkpoint(&self.ops_dir, rec)?;

        // Место: дамп плюс вторая копия базы при восстановлении, выпуск и запас.
        let size = db::database_size(self.runner, self.layout).await?;
        let release_size: u64 = staged
            .files
            .values()
            .filter_map(|p| fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        let need = 2 * size + release_size + GIB;
        fsx::ensure_dir(&self.layout.root, &self.layout.backups(), 0o700)?;
        for path in [self.layout.backups(), self.layout.pg_data()] {
            match db::free_bytes(self.runner, &path).await? {
                Some(free) if free >= need => {}
                Some(free) => {
                    return Err(CtlError::InsufficientSpace(format!(
                        "{}: {free} bytes free, {need} needed (2 x database + release + 1 GiB)",
                        path.display()
                    )));
                }
                None => {
                    return Err(CtlError::InsufficientSpace(format!(
                        "could not read free space of {}",
                        path.display()
                    )));
                }
            }
        }

        // Compose цели проверяется самим docker compose до первой остановки.
        let dir = self.layout.compose_dir();
        let next = dir.join("compose.next.yaml");
        fsx::write_atomic_mode(
            &next,
            render::render_compose(&self.state, target).as_bytes(),
            0o600,
        )?;
        fsx::write_atomic_mode(
            &dir.join("compose.fenced.yaml"),
            render::render_compose_fenced().as_bytes(),
            0o600,
        )?;
        let mut check = cmd("docker", &[]);
        check.args = vec![
            "compose".into(),
            "-f".into(),
            next.display().to_string(),
            "-f".into(),
            dir.join("compose.fenced.yaml").display().to_string(),
            "config".into(),
            "--quiet".into(),
        ];
        let checked = run_ok(self.runner, "compose config", check).await;
        let _ = fs::remove_file(&next);
        checked?;
        Ok(())
    }

    // ---- fence, dump, unfence --------------------------------------------------------

    async fn fence(&self, rec: &mut OpRecord) -> Result<(), CtlError> {
        let result = async {
            web::set_maintenance(self.layout)?;
            self.stop_hub().await
        }
        .await;
        match result {
            Ok(()) => self.advance(rec, "dump"),
            Err(err) => self.to_unfence(rec, format!("fence failed: {err}")),
        }
    }

    fn to_unfence(&self, rec: &mut OpRecord, reason: String) -> Result<(), CtlError> {
        rec.details.reason = Some(reason);
        self.advance(rec, "unfence")
    }

    async fn dump(&self, rec: &mut OpRecord) -> Result<(), CtlError> {
        let target = self.target(rec)?;
        if rec.dump.is_none() {
            let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
            rec.dump = Some(
                self.layout
                    .backups()
                    .join(format!("canterlot-pre-{target}-{stamp}.dump")),
            );
            ops::checkpoint(&self.ops_dir, rec)?;
        }
        let path = rec.dump.clone().unwrap_or_default();
        let Some(expected) = rec.db_identity.clone() else {
            return Err(self.ambiguous_err(rec, "no database identity recorded before the dump"));
        };
        let result = async {
            // Остановка повторяется: после перезагрузки хаб мог подняться политикой restart.
            self.stop_hub().await?;
            check_identity(&expected, &db_identity(self.runner, self.layout).await?)?;
            if path.is_file() {
                // Дамп уже дописан и переименован в прошлом запуске: только проверка.
                db::verify_dump(self.runner, self.layout, &path).await
            } else {
                db::dump(self.runner, self.layout, &path).await
            }
        }
        .await;
        match result {
            Ok(()) => self.advance(rec, "candidate"),
            Err(err) => {
                if !path.is_file() {
                    rec.dump = None;
                }
                self.to_unfence(rec, format!("dump failed: {err}"))
            }
        }
    }

    /// Сбой до кандидата: база не тронута, поднимаем прежний хаб и снимаем обслуживание.
    async fn unfence(&self, rec: &mut OpRecord) -> Result<(), CtlError> {
        let previous = self.previous(rec)?;
        self.write_compose(&previous)?;
        run_ok(
            self.runner,
            "compose up hub",
            compose(self.layout, &["up", "-d", "--no-deps", "hub"]),
        )
        .await?;
        probe(self.runner, self.layout, self.host, &previous).await?;
        web::clear_maintenance(self.layout)?;
        rec.traffic = Traffic::Admitted;
        let reason = rec
            .details
            .reason
            .clone()
            .unwrap_or_else(|| "failed before the candidate".into());
        self.finish(rec, OpResult::FailedBeforeChange { reason })
    }

    // ---- candidate -----------------------------------------------------------------

    async fn candidate(&self, rec: &mut OpRecord) -> Result<(), CtlError> {
        let target = self.target(rec)?;
        match &rec.dump {
            Some(dump) if dump.is_file() => {}
            _ => {
                let manual = vec![format!(
                    "the record is at candidate but has no verified dump; check the database with `{} exec -T postgres psql -U postgres -d canterlot -c 'select version, success from _sqlx_migrations order by version desc limit 5'` before deciding",
                    self.dc()
                )];
                return Err(self.ambiguous(rec, manual));
            }
        }
        let result = async {
            self.write_compose(&target)?;
            run_ok(
                self.runner,
                "compose up candidate",
                self.compose_fenced(&["up", "-d", "--force-recreate", "--no-deps", "hub"]),
            )
            .await?;
            probe(self.runner, self.layout, self.host, &target).await
        }
        .await;
        match result {
            Ok(()) => self.advance(rec, "admit"),
            Err(err) => {
                rec.details.reason = Some(format!("candidate {target} failed: {err}"));
                rec.details.restore_db = Some(ScratchDb::restore(&rec.id)?.as_str().to_owned());
                rec.details.failed_db = Some(ScratchDb::failed(&rec.id)?.as_str().to_owned());
                self.advance(rec, "rollback_restore")
            }
        }
    }

    // ---- rollback ------------------------------------------------------------------

    fn scratch(&self, rec: &OpRecord) -> Result<(ScratchDb, ScratchDb), CtlError> {
        let restore = rec.details.restore_db.as_deref().unwrap_or_default();
        let failed = rec.details.failed_db.as_deref().unwrap_or_default();
        match (ScratchDb::parse(restore), ScratchDb::parse(failed)) {
            (Ok(r), Ok(f)) => Ok((r, f)),
            _ => Err(self.ambiguous_err(rec, "the record names no valid rollback databases")),
        }
    }

    fn oid_check(&self, rec: &OpRecord) -> String {
        format!(
            "{} exec -T postgres psql -U postgres -d postgres -c \"select datname, oid from pg_database where datname like 'canterlot%'\"; the operation record is {}",
            self.dc(),
            self.ops_dir.join(format!("{}.json", rec.id)).display()
        )
    }

    /// Остановка для владельца: живая база не трогается, обслуживание и остановленный хаб
    /// остаются. compose.yaml указывает на прежний выпуск, но хаб не запускается.
    fn needs_owner(
        &self,
        rec: &mut OpRecord,
        problem: String,
        extra: Vec<String>,
    ) -> Result<(), CtlError> {
        let previous = opt_version(&rec.previous);
        if let Some(v) = &rec.previous {
            self.write_compose(v)?;
        }
        let dc = self.dc();
        let dump = rec
            .dump
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        let mut steps = vec![
            problem,
            format!(
                "The hub is stopped and the API answers 503 (maintenance flag {}); nothing else was changed after this point.",
                web::maintenance_flag(self.layout).display()
            ),
        ];
        steps.extend(extra);
        steps.extend([
            format!(
                "Check free space: df -h {} {}",
                self.layout.backups().display(),
                self.layout.pg_data().display()
            ),
            format!(
                "Restore the pre-update dump into a new database: {dc} exec -T postgres createdb -U postgres -O canterlot -T template0 -E UTF8 canterlot_manual && {dc} exec -T postgres pg_restore -U postgres --exit-on-error -d canterlot_manual < {dump}"
            ),
            format!(
                "Swap the names in one transaction: printf 'begin; alter database canterlot rename to canterlot_failed_manual; alter database canterlot_manual rename to canterlot; commit;\\n' | {dc} exec -T postgres psql -U postgres -d postgres -v ON_ERROR_STOP=1"
            ),
            format!(
                "Start the previous hub {previous} and check it: {dc} up -d --no-deps hub && {dc} exec -T hub {HUB_BIN} probe --config {HUB_CONFIG}"
            ),
            format!(
                "Leave maintenance: rm {}",
                web::maintenance_flag(self.layout).display()
            ),
            "Then `canterlot update --force` updates again; keep canterlot_failed_manual until the hub works.".into(),
        ]);
        self.finish(rec, OpResult::NeedsOwner { steps })
    }

    async fn rollback_restore(&self, rec: &mut OpRecord) -> Result<(), CtlError> {
        let (restore, failed) = self.scratch(rec)?;
        let Some(expected) = rec.db_identity.clone() else {
            return Err(self.ambiguous_err(rec, "no database identity recorded"));
        };
        let Some(dump) = rec.dump.clone().filter(|d| d.is_file()) else {
            let problem =
                "The pre-update dump is missing, so the database cannot be restored automatically."
                    .to_owned();
            return self.needs_owner(rec, problem, Vec::new());
        };
        // Кандидат — клиент живой базы; до любых действий с базами он остановлен.
        self.stop_hub().await?;
        let oids = db::oids(
            self.runner,
            self.layout,
            &[LIVE_DB, restore.as_str(), failed.as_str()],
        )
        .await?;
        if oids.get(LIVE_DB) != Some(&expected.database_oid) || oids.contains_key(failed.as_str()) {
            let manual = vec![
                format!(
                    "expected database canterlot with oid {} and no {}; check: {}",
                    expected.database_oid,
                    failed.as_str(),
                    self.oid_check(rec)
                ),
                "when canterlot is the database from before the update, rerun `canterlot update` after removing the extra database; otherwise restore by hand (docs/deploy/pilot.md §8.2)".into(),
            ];
            return Err(self.ambiguous(rec, manual));
        }
        if oids.contains_key(restore.as_str()) {
            // Остаток прерванной попытки: имя служебное, живая база проверена выше.
            db::drop_scratch(self.runner, self.layout, &restore).await?;
        }
        let problem = |what: &str, err: &dyn fmt::Display| {
            format!(
                "Automatic rollback could not {what}: {err}. The live database canterlot is the one the failed candidate migrated."
            )
        };
        if let Err(err) = db::create_scratch(self.runner, self.layout, &restore).await {
            return self.needs_owner(
                rec,
                problem("create the restore database", &err),
                Vec::new(),
            );
        }
        let created = db::oids(self.runner, self.layout, &[restore.as_str()]).await?;
        rec.details.restore_oid = created.get(restore.as_str()).copied();
        ops::checkpoint(&self.ops_dir, rec)?;
        let drop_note = vec![format!(
            "The partial restore database can be dropped: {} exec -T postgres dropdb -U postgres {}",
            self.dc(),
            restore.as_str()
        )];
        if let Err(err) = db::restore_into(self.runner, self.layout, &dump, &restore).await {
            return self.needs_owner(rec, problem("restore the dump", &err), drop_note);
        }
        if let Err(err) = self.verify_restored(rec, &restore, &expected).await {
            return self.needs_owner(rec, problem("verify the restored copy", &err), drop_note);
        }
        self.advance(rec, "rollback_swap")
    }

    async fn verify_restored(
        &self,
        rec: &OpRecord,
        restore: &ScratchDb,
        expected: &DbIdentity,
    ) -> Result<(), CtlError> {
        let got = db_identity_of(self.runner, self.layout, restore.as_str()).await?;
        // oid у новой базы свой; кластер и UUID установки обязаны совпасть.
        check_identity(
            &DbIdentity {
                database_oid: got.database_oid,
                ..expected.clone()
            },
            &got,
        )?;
        let migrations = db::applied_migrations(self.runner, self.layout, restore.as_str()).await?;
        if migrations != rec.details.migrations {
            return Err(CtlError::BadOutput(
                "restored _sqlx_migrations differ from the pre-update set".into(),
            ));
        }
        if let Some(tables) = rec.details.tables {
            let got = db::table_count(self.runner, self.layout, restore.as_str()).await?;
            if got != tables {
                return Err(CtlError::BadOutput(format!(
                    "restored copy has {got} tables, {tables} before the update"
                )));
            }
        }
        Ok(())
    }

    async fn rollback_swap(&self, rec: &mut OpRecord) -> Result<(), CtlError> {
        let (restore, failed) = self.scratch(rec)?;
        let (Some(expected), Some(restore_oid)) =
            (rec.db_identity.clone(), rec.details.restore_oid)
        else {
            return Err(self.ambiguous_err(rec, "no oids recorded for the swap"));
        };
        let orig = expected.database_oid;
        let names = [LIVE_DB, restore.as_str(), failed.as_str()];
        let before = |o: &std::collections::BTreeMap<String, u32>| {
            o.get(LIVE_DB) == Some(&orig)
                && o.get(restore.as_str()) == Some(&restore_oid)
                && !o.contains_key(failed.as_str())
        };
        let after = |o: &std::collections::BTreeMap<String, u32>| {
            o.get(LIVE_DB) == Some(&restore_oid)
                && o.get(failed.as_str()) == Some(&orig)
                && !o.contains_key(restore.as_str())
        };
        let manual = |job: &Self, rec: &OpRecord| {
            vec![
                format!(
                    "before the swap: canterlot={orig}, {}={restore_oid}; after it: canterlot={restore_oid}, {}={orig}",
                    restore.as_str(),
                    failed.as_str()
                ),
                format!("check: {}", job.oid_check(rec)),
                "rename the databases by hand into one of these two states, then rerun `canterlot update`".into(),
            ]
        };
        let oids = db::oids(self.runner, self.layout, &names).await?;
        if before(&oids) {
            if let Err(err) = db::swap(self.runner, self.layout, &restore, &failed).await {
                let now = db::oids(self.runner, self.layout, &names).await?;
                if before(&now) {
                    let problem = format!(
                        "Swapping the restored database {} in failed: {err}. canterlot is still the migrated database; the restored copy is complete.",
                        restore.as_str()
                    );
                    return self.needs_owner(rec, problem, Vec::new());
                }
                if !after(&now) {
                    let m = manual(self, rec);
                    return Err(self.ambiguous(rec, m));
                }
            }
        } else if !after(&oids) {
            let m = manual(self, rec);
            return Err(self.ambiguous(rec, m));
        }
        // Проверка после обмена: живая база — восстановленная копия.
        let oids = db::oids(self.runner, self.layout, &names).await?;
        if !after(&oids) {
            let m = manual(self, rec);
            return Err(self.ambiguous(rec, m));
        }
        self.advance(rec, "rollback_start")
    }

    async fn rollback_start(&self, rec: &mut OpRecord) -> Result<(), CtlError> {
        let previous = self.previous(rec)?;
        let (Some(expected), Some(restore_oid)) =
            (rec.db_identity.clone(), rec.details.restore_oid)
        else {
            return Err(self.ambiguous_err(rec, "no identity recorded for the restored database"));
        };
        let restored = DbIdentity {
            database_oid: restore_oid,
            ..expected
        };
        // Тождество проверяется до запуска любого хаба на восстановленной базе.
        match db_identity(self.runner, self.layout).await {
            Ok(got) => {
                if let Err(err) = check_identity(&restored, &got) {
                    let problem = format!(
                        "The restored database does not match the recorded identity: {err}."
                    );
                    return self.needs_owner(rec, problem, Vec::new());
                }
            }
            Err(err) => {
                let problem = format!("Reading the restored database identity failed: {err}.");
                return self.needs_owner(rec, problem, Vec::new());
            }
        }
        self.write_compose(&previous)?;
        let started = async {
            run_ok(
                self.runner,
                "compose up previous hub",
                self.compose_fenced(&["up", "-d", "--force-recreate", "--no-deps", "hub"]),
            )
            .await?;
            probe(self.runner, self.layout, self.host, &previous).await
        }
        .await;
        if let Err(err) = started {
            let _ = self.stop_hub().await;
            let problem = format!(
                "The previous hub {previous} did not become ready on the restored database: {err}. Logs: {} logs hub",
                self.dc()
            );
            return self.needs_owner(rec, problem, Vec::new());
        }
        // Снятие ограждения: gRPC публикуется, затем API выходит из обслуживания.
        run_ok(
            self.runner,
            "compose up hub",
            compose(self.layout, &["up", "-d", "--no-deps", "hub"]),
        )
        .await?;
        probe(self.runner, self.layout, self.host, &previous).await?;
        web::clear_maintenance(self.layout)?;
        rec.traffic = Traffic::Admitted;
        rec.db_identity = Some(restored);
        let reason = rec
            .details
            .reason
            .clone()
            .unwrap_or_else(|| "candidate failed".into());
        self.finish(rec, OpResult::RolledBack { reason })
    }

    // ---- admit, post ---------------------------------------------------------------

    /// Точка фиксации. Каждый шаг повторяем; сбой оставляет запись на admit, и повторный
    /// `canterlot update` доводит допуск трафика. Отката отсюда нет.
    async fn admit(&self, rec: &mut OpRecord) -> Result<(), CtlError> {
        let target = self.target(rec)?;
        let previous = self.previous(rec)?;
        let result = async {
            let hub_parent = self.layout.root.join("opt/canterlot/hub");
            fsx::replace_symlink(
                &Path::new("releases").join(target.as_str()),
                &self.layout.hub_current(),
            )?;
            fsx::replace_symlink(
                &Path::new("releases").join(previous.as_str()),
                &hub_parent.join("previous"),
            )?;

            let sources: Vec<PathBuf> = web::web_versions(self.layout)?
                .into_iter()
                .filter(|v| v != &target)
                .rev()
                .take(web::MERGE_FROM)
                .map(|v| self.layout.web_release(&v))
                .collect();
            web::merge_chunks(&self.layout.web_release(&target), &sources)?;

            let csp = fs::read_to_string(self.layout.hub_release(&target).join("csp.txt"))?;
            let csp =
                render::csp_for_origin(&csp, self.state.origin.as_deref().unwrap_or_default())?;
            fsx::write_atomic_mode(
                &self.layout.caddy_dir().join("Caddyfile"),
                render::render_caddyfile(&self.state, &csp).as_bytes(),
                0o644,
            )?;
            web::flip_build(self.layout, &target)?;
            run_ok(
                self.runner,
                "caddy reload",
                compose(
                    self.layout,
                    &[
                        "exec",
                        "-T",
                        "caddy",
                        "caddy",
                        "reload",
                        "--config",
                        "/etc/caddy/Caddyfile",
                    ],
                ),
            )
            .await?;
            run_ok(
                self.runner,
                "compose up hub",
                compose(self.layout, &["up", "-d", "--no-deps", "hub"]),
            )
            .await?;
            probe(self.runner, self.layout, self.host, &target).await?;
            web::clear_maintenance(self.layout)
        }
        .await;
        match result {
            Ok(()) => {
                rec.traffic = Traffic::Admitted;
                self.advance(rec, "post")
            }
            Err(err) => {
                let text = format!(
                    "{err}; the update to {target} is committed (forward-only), the database is kept; run `canterlot update` again to finish admitting traffic"
                );
                rec.details.warnings.push(format!("admit: {err}"));
                ops::checkpoint(&self.ops_dir, rec)?;
                Err(CtlError::AdmitFailed(text))
            }
        }
    }

    /// После фиксации только мягкие шаги: сбой — предупреждение в итоге, не откат.
    async fn post(&self, rec: &mut OpRecord) -> Result<(), CtlError> {
        let mut warnings = Vec::new();
        if let Err(err) = self.retention(rec, &mut warnings).await {
            warnings.push(format!("retention: {err}"));
        }
        // Служба распознавания речи в стек этапа 1 не входит: перезапускать нечего.
        rec.details.warnings.extend(warnings);
        self.finish(rec, OpResult::Done)
    }

    async fn retention(&self, rec: &OpRecord, warnings: &mut Vec<String>) -> Result<(), CtlError> {
        let target = self.target(rec)?;
        let previous = self.previous(rec)?;
        let records: Vec<OpRecord> = ops::all(&self.ops_dir)?
            .into_iter()
            .filter(|r| r.kind == OpKind::Update && r.id != rec.id)
            .collect();

        // Мигрированные базы прежних откатов удаляются после успешного обновления; только
        // имена из записей и только если это не живая база.
        for old in records
            .iter()
            .filter(|r| matches!(r.result, Some(OpResult::RolledBack { .. })))
        {
            let Some(name) = old.details.failed_db.as_deref() else {
                continue;
            };
            let Ok(db) = ScratchDb::parse(name) else {
                warnings.push(format!(
                    "record {} names a bad failed database {name}",
                    old.id
                ));
                continue;
            };
            let oids = db::oids(self.runner, self.layout, &[LIVE_DB, db.as_str()]).await?;
            match (oids.get(db.as_str()), oids.get(LIVE_DB)) {
                (None, _) => {}
                (Some(a), Some(b)) if a == b => {}
                (Some(_), _) => {
                    if let Err(err) = db::drop_scratch(self.runner, self.layout, &db).await {
                        warnings.push(format!(
                            "could not drop the old failed database {name}: {err}; drop it by hand"
                        ));
                    }
                }
            }
        }

        // Дампы: последние KEEP_DUMPS успешных обновлений, включая это; дампы записей,
        // которые ждут владельца, не трогаются.
        let mut keep: Vec<PathBuf> = rec.dump.iter().cloned().collect();
        keep.extend(
            records
                .iter()
                .rev()
                .filter(|r| r.result == Some(OpResult::Done))
                .filter_map(|r| r.dump.clone())
                .take(KEEP_DUMPS.saturating_sub(1)),
        );
        let backups = self.layout.backups();
        for old in &records {
            let Some(dump) = &old.dump else { continue };
            if keep.contains(dump)
                || old.result.is_none()
                || matches!(old.result, Some(OpResult::NeedsOwner { .. }))
                || dump.parent() != Some(backups.as_path())
            {
                continue;
            }
            match fs::symlink_metadata(dump) {
                Ok(meta) if meta.is_file() => {
                    if let Err(err) = fs::remove_file(dump) {
                        warnings.push(format!("could not remove {}: {err}", dump.display()));
                    }
                }
                _ => {}
            }
        }

        // Выпуски хаба: current и previous; деревья PWA: последние KEEP_WEB.
        let hub_releases = self.layout.root.join("opt/canterlot/hub/releases");
        for v in web::versions_in(&hub_releases)? {
            if v != target && v != previous {
                self.remove_release(&hub_releases.join(v.as_str()), warnings);
            }
        }
        let web_list = web::web_versions(self.layout)?;
        let drop_count = web_list.len().saturating_sub(KEEP_WEB);
        for v in web_list.into_iter().take(drop_count) {
            if v != target {
                self.remove_release(&self.layout.web_release(&v), warnings);
            }
        }
        Ok(())
    }

    fn remove_release(&self, dir: &Path, warnings: &mut Vec<String>) {
        if let Err(err) = remove::remove_tree(&self.layout.root, dir) {
            warnings.push(format!("could not remove {}: {err}", dir.display()));
        }
    }
}

fn staged_file<'a>(staged: &'a Staged, name: &str) -> Result<&'a Path, CtlError> {
    staged
        .files
        .get(name)
        .map(PathBuf::as_path)
        .ok_or_else(|| CtlError::AssetMissing(name.into()))
}
