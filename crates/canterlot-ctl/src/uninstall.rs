//! `canterlot uninstall`. Воркер: юнит и `~/.local/opt/canterlot` уходят, тождество машины
//! и данные воркера остаются, пока их не снимет `--purge`. Хост хаба — ниже.

use std::{
    fmt,
    os::unix::fs::MetadataExt as _,
    path::{Component, Path, PathBuf},
};

use uuid::Uuid;

use crate::{
    cli::UninstallArgs,
    error::CtlError,
    fsx,
    install::Host,
    layout::Layout,
    ops::{self, OpKind, OpRecord, OpResult},
    runner::Runner,
    stack::{cmd, compose, psql, run_ok, stdout},
    state::InstallState,
    worker::{self, WorkerHost},
};

#[derive(Debug, Default)]
pub struct UninstallReport {
    pub removed: Vec<PathBuf>,
    pub kept: Vec<PathBuf>,
    pub machine_id: Option<Uuid>,
    pub notes: Vec<String>,
}

impl fmt::Display for UninstallReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "canterlot worker is removed.")?;
        for path in &self.removed {
            writeln!(f, "removed: {}", path.display())?;
        }
        for path in &self.kept {
            writeln!(f, "kept:    {}", path.display())?;
        }
        if let Some(id) = self.machine_id {
            writeln!(
                f,
                "Revoke the machine on the hub: PWA «Машины», or on the hub host `canterlot-hub machine revoke --machine-id {id}`."
            )?;
        }
        for note in &self.notes {
            writeln!(f, "Note: {note}")?;
        }
        Ok(())
    }
}

/// Итог `canterlot uninstall`: половина хаба или воркер этой учётной записи.
#[derive(Debug)]
pub enum Uninstalled {
    Hub(HubUninstallReport),
    Worker(UninstallReport),
}

impl fmt::Display for Uninstalled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hub(report) => report.fmt(f),
            Self::Worker(report) => report.fmt(f),
        }
    }
}

/// Хост хаба (`install.toml` есть, и это не вызов от имени воркера) — половина хаба: она
/// сама повышает права и затем удаляет воркер этого хоста; иначе — воркер этой учётной записи.
pub async fn uninstall(
    args: &UninstallArgs,
    layout: &Layout,
    runner: &dyn Runner,
    host: &WorkerHost,
) -> Result<Uninstalled, CtlError> {
    if crate::hub_update::hub_host(layout) {
        let hub_host = Host {
            sudo_user: None,
            hostname: host.hostname.clone(),
            is_root: host.is_root,
            interactive: host.interactive,
            self_exe: host.self_exe.clone(),
            retries: 90,
            retry_delay: std::time::Duration::from_secs(2),
        };
        let mut ask = |question: &str| host.ask(question, "the typed host name");
        return uninstall_hub(args, layout, runner, &hub_host, &mut ask)
            .await
            .map(Uninstalled::Hub);
    }
    uninstall_worker(args, layout, runner, host)
        .await
        .map(Uninstalled::Worker)
}

/// Удаляет путь, не проходя по ссылкам: ссылка снимается сама, каталог — вместе с
/// содержимым (`remove_dir_all` ссылки внутри не разыменовывает).
fn remove_tree(path: &Path) -> Result<bool, CtlError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            std::fs::remove_dir_all(path)?;
            Ok(true)
        }
        Ok(_) => {
            std::fs::remove_file(path)?;
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// Сколько исполнений живо: по сокету работающего воркера, а у остановленного — по
/// каталогам исполнений (сопровождения переживают воркер).
async fn live_sessions(runner: &dyn Runner, layout: &Layout) -> Result<usize, CtlError> {
    if let Ok(status) = worker::worker_status(runner, layout).await {
        return Ok(status.executions.len());
    }
    match std::fs::read_dir(layout.worker_data_dir().join("executions")) {
        Ok(entries) => Ok(entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .count()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err.into()),
    }
}

fn machine_id(layout: &Layout) -> Option<Uuid> {
    let text = std::fs::read_to_string(layout.worker_config_dir().join("worker.toml")).ok()?;
    let table: toml::Table = toml::from_str(&text).ok()?;
    table.get("machine_id")?.as_str()?.parse().ok()
}

pub async fn uninstall_worker(
    args: &UninstallArgs,
    layout: &Layout,
    runner: &dyn Runner,
    host: &WorkerHost,
) -> Result<UninstallReport, CtlError> {
    worker::home_of(layout)?;
    let live = live_sessions(runner, layout).await?;
    if live > 0 && !args.force {
        return Err(CtlError::SessionsAlive(live));
    }
    // Подтверждение до первого изменения: отказ ничего не трогает.
    if args.purge {
        let typed = host.ask(
            &format!(
                "This deletes the worker identity and data. Type the host name ({}) to confirm",
                host.hostname
            ),
            "the typed host name",
        )?;
        if typed != host.hostname {
            return Err(CtlError::ConfirmationMismatch(host.hostname.clone()));
        }
    }
    let ops_dir = layout.worker_ops_dir();
    let _lock = ops::lock(&ops_dir)?;
    let mut rec = ops::begin(&ops_dir, OpRecord::new(OpKind::Uninstall, None))?;
    let mut report = UninstallReport {
        machine_id: machine_id(layout),
        ..UninstallReport::default()
    };

    ops::advance(&ops_dir, &mut rec, "unit")?;
    let stopped = runner
        .run(worker::systemctl_user(&[
            "disable",
            "--now",
            "canterlot-worker",
        ]))
        .await?;
    if stopped.status != 0 {
        report.notes.push(format!(
            "systemctl --user disable exited with {}",
            stopped.status
        ));
    }
    let unit = layout.worker_unit();
    if remove_tree(&unit)? {
        report.removed.push(unit);
    }
    let _ = runner.run(worker::systemctl_user(&["daemon-reload"])).await;

    ops::advance(&ops_dir, &mut rec, "files")?;
    let opt = layout.worker_opt();
    // Ссылка на утилиту снимается, только если ведёт в удаляемый каталог.
    let link = layout.worker_ctl_link();
    if std::fs::read_link(&link).is_ok_and(|target| target.starts_with(&opt)) {
        std::fs::remove_file(&link)?;
        report.removed.push(link);
    }
    if remove_tree(&opt)? {
        report.removed.push(opt);
    }
    for dir in [layout.worker_config_dir(), layout.worker_data_dir()] {
        if args.purge {
            if remove_tree(&dir)? {
                report.removed.push(dir);
            }
        } else if std::fs::symlink_metadata(&dir).is_ok() {
            report.kept.push(dir);
        }
    }
    ops::finish(&ops_dir, &mut rec, OpResult::Done)?;
    Ok(report)
}

// ---- Хост хаба ---------------------------------------------------------------------
//
// Стек compose, служба host-agent и `/opt/canterlot` уходят; `/etc/canterlot` и
// `/var/lib/canterlot` остаются (повторный `install` возьмёт их). `--purge` после ввода
// имени хоста удаляет только пути, записанные установщиком как созданные им, под
// ожидаемыми корнями, без перехода по ссылкам; принятые каталоги перечисляются.

/// uid пользователя postgres в официальном образе (как в install.rs).
const POSTGRES_UID: u32 = 999;
const HOST_AGENT_UNIT: &str = "canterlot-host-agent.service";

#[derive(Debug, Default)]
pub struct HubUninstallReport {
    pub removed: Vec<PathBuf>,
    pub kept: Vec<PathBuf>,
    /// Пути, которые утилита отказалась удалять: удалить вручную после проверки.
    pub manual: Vec<PathBuf>,
    /// Вывод удаления воркера этого хоста (от имени его учётной записи).
    pub worker: Option<String>,
}

impl fmt::Display for HubUninstallReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "The canterlot hub stack is removed from this host.")?;
        for path in &self.removed {
            writeln!(f, "Removed: {}", path.display())?;
        }
        for path in &self.kept {
            writeln!(f, "Kept: {}", path.display())?;
        }
        if !self.kept.is_empty() {
            writeln!(
                f,
                "`canterlot install` reuses the kept data; `canterlot uninstall --purge` deletes it."
            )?;
        }
        for path in &self.manual {
            writeln!(
                f,
                "Not removed (adopted, outside the installer's paths or behind a link): {}",
                path.display()
            )?;
        }
        if let Some(worker) = &self.worker {
            write!(f, "{worker}")?;
        }
        Ok(())
    }
}

/// Вопрос владельцу (в работе — через `/dev/tty`), ответ — введённая строка.
pub type Ask<'a> = &'a mut (dyn FnMut(&str) -> Result<String, CtlError> + Send);

async fn live_executions(runner: &dyn Runner, layout: &Layout) -> Result<u64, CtlError> {
    let out = run_ok(
        runner,
        "psql live sessions",
        psql(
            layout,
            "canterlot",
            "select count(*) from executions where state in ('created', 'starting', 'running', 'stopping');\n",
        ),
    )
    .await?;
    stdout(&out)
        .parse()
        .map_err(|_| CtlError::BadOutput("live session count".into()))
}

pub async fn uninstall_hub(
    args: &UninstallArgs,
    layout: &Layout,
    runner: &dyn Runner,
    host: &Host,
    ask: Ask<'_>,
) -> Result<HubUninstallReport, CtlError> {
    crate::hub_update::elevate(host)?;
    let mut state = InstallState::load(layout)?.ok_or(CtlError::HubNotInstalled)?;
    if args.purge {
        let typed = ask(&format!(
            "This deletes the database, secrets and the CA of canterlot. Type the host name ({}) to confirm",
            host.hostname
        ))?;
        if typed.trim() != host.hostname {
            return Err(CtlError::Cancelled);
        }
    }
    let ops_dir = layout.ops_dir();
    fsx::ensure_dir(&layout.root, &ops_dir, 0o700)?;
    let _lock = ops::lock(&ops_dir)?;
    let compose_file = layout.compose_dir().join("compose.yaml");
    let stack = state.components.hub && compose_file.is_file();
    if stack && !args.force {
        // Остановка хаба обрывает связь всех воркеров с их живыми сессиями.
        let live = live_executions(runner, layout).await?;
        if live > 0 {
            return Err(CtlError::LiveSessions(live));
        }
    }
    let mut rec = ops::begin(&ops_dir, OpRecord::new(OpKind::Uninstall, None))?;
    ops::advance(&ops_dir, &mut rec, "stop")?;
    if stack {
        run_ok(runner, "compose down", compose(layout, &["down"])).await?;
    }
    if state.components.host_agent {
        run_ok(
            runner,
            "systemctl disable host-agent",
            cmd("systemctl", &["disable", "--now", HOST_AGENT_UNIT]),
        )
        .await?;
        let unit = layout.root.join("etc/systemd/system").join(HOST_AGENT_UNIT);
        crate::remove::remove_tree(&layout.root, &unit)?;
    }

    ops::advance(&ops_dir, &mut rec, "remove")?;
    let mut report = HubUninstallReport::default();
    // Стабильная ссылка на утилиту ведёт в /opt/canterlot/ctl: без него она мертва.
    let link = layout.ctl_link();
    if let Ok(target) = std::fs::read_link(&link)
        && target.starts_with(layout.root.join("opt/canterlot"))
        && crate::remove::remove_tree(&layout.root, &link)?
    {
        report.removed.push(link);
    }
    let opt = layout.root.join("opt/canterlot");
    if state.adopted_paths.iter().any(|a| related(a, &opt)) {
        report.manual.push(opt);
    } else if crate::remove::remove_tree(&layout.root, &opt)? {
        report.removed.push(opt);
    }
    state.components.postgres = false;
    state.components.hub = false;
    state.components.web = false;
    state.components.caddy = false;
    state.components.host_agent = false;
    state.save(layout)?;

    if args.purge {
        ops::advance(&ops_dir, &mut rec, "purge")?;
        let (removed, manual) = purge(layout, &state);
        report.removed.extend(removed);
        report.manual.extend(manual);
    }
    if state.components.worker
        && let Some(user) = state.worker_user.clone()
    {
        ops::advance(&ops_dir, &mut rec, "worker")?;
        // Живые сессии уже проверены по базе хаба (она видит и этот воркер), отсюда --force.
        let mut child = vec!["uninstall", "--force"];
        if args.purge {
            child.push("--purge");
        }
        match crate::hub_update::run_as_worker(runner, layout, &state, &user, &child).await {
            Ok(out) if out.status == 0 => {
                report.worker = Some(String::from_utf8_lossy(&out.stdout).into_owned());
                state.components.worker = false;
                state.save(layout)?;
            }
            Ok(out) => {
                report.worker = Some(format!(
                    "Worker uninstall as {user} exited with {}; run `canterlot uninstall` as {user}.\n",
                    out.status
                ))
            }
            Err(err) => {
                report.worker = Some(format!(
                    "Worker uninstall as {user} failed: {err}; run `canterlot uninstall` as {user}.\n"
                ))
            }
        }
    }
    for kept in [
        layout.root.join("etc/canterlot"),
        layout.root.join("var/lib/canterlot"),
    ] {
        if kept.exists() {
            report.kept.push(kept);
        }
    }
    ops::finish(&ops_dir, &mut rec, OpResult::Done)?;
    Ok(report)
}

/// Один путь содержит другой или равен ему.
fn related(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn normal(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

/// Компоненты пути ниже `base` (кроме последнего) — настоящие каталоги, не ссылки.
fn parents_are_dirs(base: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(base) else {
        return false;
    };
    let mut current = base.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() => {}
            // Родителя нет — нечего и удалять; ссылка или файл — отказ.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return true,
            _ => return false,
        }
    }
    true
}

/// Удаляет записанные как созданные пути. Сначала проверяются все, затем удаляются
/// прошедшие: удаление одного пути не меняет решения о другом.
pub fn purge(layout: &Layout, state: &InstallState) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let roots = [
        layout.root.join("etc/canterlot"),
        layout.root.join("opt/canterlot"),
        layout.root.join("var/lib/canterlot"),
        layout.root.join("run/canterlot-host"),
        layout.ctl_link(),
    ];
    let owners = [
        0,
        state.hub_uid,
        POSTGRES_UID,
        rustix::process::geteuid().as_raw(),
    ];
    let mut accepted = Vec::new();
    let mut manual = Vec::new();
    for path in &state.created_paths {
        let under_root = roots.iter().any(|r| path.starts_with(r));
        let adopted = state.adopted_paths.iter().any(|a| related(a, path));
        if !normal(path) || !under_root || adopted || !parents_are_dirs(&layout.root, path) {
            manual.push(path.clone());
            continue;
        }
        match std::fs::symlink_metadata(path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Ok(meta) if owners.contains(&meta.uid()) => accepted.push(path.clone()),
            _ => manual.push(path.clone()),
        }
    }
    let mut removed = Vec::new();
    for path in accepted {
        match crate::remove::remove_tree(&layout.root, &path) {
            Ok(true) => removed.push(path),
            Ok(false) => {}
            Err(_) => manual.push(path),
        }
    }
    (removed, manual)
}
