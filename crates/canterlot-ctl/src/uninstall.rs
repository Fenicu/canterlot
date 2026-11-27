//! `canterlot uninstall`. Здесь — половина воркера: юнит и `~/.local/opt/canterlot` уходят,
//! тождество машины и данные воркера остаются, пока их не снимет `--purge`.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{
    cli::UninstallArgs,
    error::CtlError,
    layout::Layout,
    ops::{self, OpKind, OpRecord, OpResult},
    runner::Runner,
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

/// Хост хаба (root и `install.toml`) — половина хаба; иначе — воркер этой учётной записи.
pub async fn uninstall(
    args: &UninstallArgs,
    layout: &Layout,
    runner: &dyn Runner,
    host: &WorkerHost,
) -> Result<UninstallReport, CtlError> {
    if host.is_root && layout.install_toml().is_file() {
        return Err(CtlError::NotImplemented("uninstall on the hub host"));
    }
    uninstall_worker(args, layout, runner, host).await
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
