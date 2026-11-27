//! `canterlot update` на машине воркера.
//!
//! Снаружи утилита только ставит себя целевой версии и запускает её же в отдельном
//! временном юните (`systemd-run --user`): перезапуск `canterlot-worker` не должен убить
//! код, который за ним следит. Внутри юнита — переключение `current`, перезапуск и проверка:
//! новая сборка подключилась к хабу и подхватила каждое исполнение, жившее до перезапуска.
//! Иначе ссылка возвращается назад.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use crate::{
    cli::{UpdateArgs, Version},
    error::CtlError,
    fsx,
    layout::Layout,
    ops::{self, OpKind, OpRecord, OpResult},
    release::{self, ReleaseSource, Staged},
    runner::Runner,
    stack::{cmd, run_ok},
    worker::{self, IN_UNIT_ENV, WorkerHost, WorkerStatus, version_key},
};

/// Имя каталога релиза, на который указывает ссылка (любое: старые выкладки называли по sha).
fn link_name(link: &Path) -> Option<String> {
    std::fs::read_link(link)
        .ok()?
        .file_name()?
        .to_str()
        .map(str::to_owned)
}

/// Версия с `--from-dir` — из имени архива воркера, если `--version` не задан.
fn version_in_dir(dir: &Path) -> Result<Version, CtlError> {
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(rest) = name.strip_prefix("canterlot-worker-")
            && let Some(version) = rest.strip_suffix("-x86_64-linux-gnu.tar.gz")
        {
            return Version::parse(version);
        }
    }
    Err(CtlError::AssetMissing("canterlot-worker archive".into()))
}

async fn target_version(args: &UpdateArgs, src: &ReleaseSource) -> Result<Version, CtlError> {
    match (&args.version, &args.from_dir) {
        (Some(v), _) => Ok(v.clone()),
        (None, Some(dir)) => version_in_dir(dir),
        (None, None) => release::resolve(src, None).await,
    }
}

/// Архив и `SHA256SUMS`: из `--from-dir` или скачанные; второй флаг — удалить ли каталог.
async fn fetch_assets(
    args: &UpdateArgs,
    layout: &Layout,
    src: &ReleaseSource,
    v: &Version,
    asset: &str,
) -> Result<(Staged, bool), CtlError> {
    let names = [asset, "SHA256SUMS"];
    if let Some(dir) = &args.from_dir {
        // Версию каталога `from_dir` читает из имени архива воркера: он есть в любом выпуске.
        let worker = release::asset_names(v).worker;
        let mut wanted = vec![asset, "SHA256SUMS"];
        if asset != worker {
            wanted.push(&worker);
        }
        let staged = release::from_dir(dir, &wanted)?;
        if staged.version != *v {
            return Err(CtlError::BadVersion(format!(
                "{} holds {}, not {v}",
                dir.display(),
                staged.version
            )));
        }
        return Ok((staged, false));
    }
    let home = worker::home_of(layout)?;
    let staging = layout.worker_staging();
    fsx::ensure_dir(home, &staging, 0o700)?;
    Ok((release::fetch(src, v, &names, &staging).await?, true))
}

/// Снаружи юнита: утилита целевой версии и запуск её внутри `systemd-run --user`.
async fn launch_in_unit(
    args: &UpdateArgs,
    layout: &Layout,
    runner: &dyn Runner,
    src: &ReleaseSource,
    host: &WorkerHost,
    target: &Version,
) -> Result<OpResult, CtlError> {
    let own = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let ctl = if *target == own {
        match &host.self_exe {
            Some(exe) => worker::install_ctl(layout, exe, target)?,
            None => layout.worker_ctl(target),
        }
    } else {
        let assets = release::asset_names(target);
        let (staged, fetched) = fetch_assets(args, layout, src, target, &assets.ctl).await?;
        let unpacked = layout
            .worker_staging()
            .join(format!(".ctl-{}", uuid::Uuid::now_v7()));
        let result = release::extract_tar_gz(&staged.files[&assets.ctl], &unpacked)
            .and_then(|()| worker::install_ctl(layout, &unpacked.join("canterlot"), target));
        let _ = std::fs::remove_dir_all(&unpacked);
        if fetched {
            let _ = std::fs::remove_dir_all(&staged.dir);
        }
        result?
    };
    let unit = format!("canterlot-update-{}", uuid::Uuid::now_v7());
    let mut args_out = vec![
        "--user".to_owned(),
        "--unit".into(),
        unit,
        "--wait".into(),
        "--collect".into(),
        "--quiet".into(),
        "--pipe".into(),
        format!("--setenv={IN_UNIT_ENV}=1"),
        "--".into(),
        ctl.display().to_string(),
        "update".into(),
        "--version".into(),
        target.to_string(),
    ];
    if let Some(dir) = &args.from_dir {
        args_out.push("--from-dir".into());
        args_out.push(dir.display().to_string());
    }
    if args.force {
        args_out.push("--force".into());
    }
    if args.hub_chain {
        args_out.push("--hub-chain".into());
    }
    let mut run = cmd("systemd-run", &[]);
    run.args = args_out;
    let out = runner.run(run).await?;
    print!("{}", String::from_utf8_lossy(&out.stdout));
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    // Итог пишет запись операции внутри юнита; код выхода — лишь подтверждение.
    let latest = ops::latest(&layout.worker_ops_dir())?;
    match latest.and_then(|rec| rec.result) {
        Some(result) if out.status == 0 || !matches!(result, OpResult::Done) => Ok(result),
        _ => Err(CtlError::CommandFailed {
            program: "systemd-run canterlot update".into(),
            status: out.status,
        }),
    }
}

pub async fn update_worker(
    args: &UpdateArgs,
    layout: &Layout,
    runner: &dyn Runner,
    src: &ReleaseSource,
    host: &WorkerHost,
) -> Result<OpResult, CtlError> {
    let target = target_version(args, src).await?;
    if !host.in_unit {
        return launch_in_unit(args, layout, runner, src, host, &target).await;
    }
    let ops_dir = layout.worker_ops_dir();
    let _lock = ops::lock(&ops_dir)?;
    let mut rec = ops::begin(
        &ops_dir,
        OpRecord::new(OpKind::Update, Some(target.clone())),
    )?;
    let result = transaction(args, layout, runner, src, host, &target, &ops_dir, &mut rec).await;
    match result {
        Ok(result) => {
            ops::finish(&ops_dir, &mut rec, result.clone())?;
            Ok(result)
        }
        Err(err) => {
            let _ = ops::finish(
                &ops_dir,
                &mut rec,
                OpResult::FailedBeforeChange {
                    reason: err.to_string(),
                },
            );
            Err(err)
        }
    }
}

/// Хаб первым: воркер не обгоняет версию хаба без `--force`.
/// На хосте хаба подсказка называет команду с sudo: хаб обновляет только root.
fn check_hub_first(
    status: &WorkerStatus,
    target: &Version,
    force: bool,
    on_hub_host: bool,
) -> Result<(), CtlError> {
    if force {
        return Ok(());
    }
    let hub = status
        .hub_version
        .as_deref()
        .filter(|_| status.connected)
        .ok_or(CtlError::HubVersionUnknown)?;
    let hub_key = version_key(hub).ok_or(CtlError::HubVersionUnknown)?;
    if version_key(target.as_str()) > Some(hub_key) {
        return Err(if on_hub_host {
            CtlError::HubOlderThanTargetOnHubHost {
                hub: hub.to_owned(),
                target: target.to_string(),
            }
        } else {
            CtlError::HubOlderThanTarget {
                hub: hub.to_owned(),
                target: target.to_string(),
            }
        });
    }
    Ok(())
}

/// В цепочке на хосте хаба воркер обновляется сразу после перезапуска хаба и может ещё
/// не видеть его новую версию (переподключение идёт с задержкой). Ждём ограниченное
/// время и возвращаем последнюю причину; отдельный вызов обновления падает сразу.
async fn wait_for_hub(
    runner: &dyn Runner,
    layout: &Layout,
    host: &WorkerHost,
    target: &Version,
    force: bool,
    on_hub_host: bool,
) -> Result<WorkerStatus, CtlError> {
    let mut last = CtlError::HubVersionUnknown;
    for n in 0..host.hub_wait_polls.max(1) {
        if n > 0 {
            tokio::time::sleep(host.hub_wait_delay).await;
        }
        match worker::worker_status(runner, layout).await {
            Ok(status) => match check_hub_first(&status, target, force, on_hub_host) {
                Ok(()) => return Ok(status),
                Err(err) => last = err,
            },
            Err(err) => last = err,
        }
    }
    Err(last)
}

/// Каталоги релизов, из которых запущены живые сопровождения. Бинарь, которого уже нет
/// на диске, — отказ: такое сопровождение нечем перезапустить и нечего сохранять.
fn releases_in_use(layout: &Layout, status: &WorkerStatus) -> Result<BTreeSet<String>, CtlError> {
    let releases = layout.worker_releases();
    let canonical = std::fs::canonicalize(&releases).unwrap_or(releases.clone());
    let mut used = BTreeSet::new();
    for exec in &status.executions {
        let Some(exe) = &exec.supervisor_exe else {
            continue;
        };
        if !exe.is_file() {
            return Err(CtlError::SupervisorBinaryMissing(format!(
                "execution {}: {}",
                exec.id,
                exe.display()
            )));
        }
        for base in [&canonical, &releases] {
            if let Ok(rest) = exe.strip_prefix(base)
                && let Some(first) = rest.components().next()
            {
                used.insert(first.as_os_str().to_string_lossy().into_owned());
            }
        }
    }
    Ok(used)
}

/// Удаляет старые релизы с именем-версией, кроме `current`, `previous` и занятых живыми
/// сопровождениями. Чужие имена (sha прежних выкладок) и ссылки не трогаются.
fn retain(layout: &Layout, keep: &BTreeSet<String>) -> Result<(), CtlError> {
    let entries = match std::fs::read_dir(layout.worker_releases()) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if keep.contains(&name) || Version::parse(&name).is_err() {
            continue;
        }
        let meta = std::fs::symlink_metadata(entry.path())?;
        if meta.is_dir() {
            // remove_dir_all не идёт по ссылкам внутри каталога.
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

async fn restart(runner: &dyn Runner) -> Result<(), CtlError> {
    run_ok(
        runner,
        "systemctl --user restart",
        worker::systemctl_user(&["restart", "canterlot-worker"]),
    )
    .await
    .map(|_| ())
}

/// Ждёт сборку `version` на связи с хабом, и чтобы каждое из `alive` снова было подхвачено.
async fn wait_reattached(
    runner: &dyn Runner,
    layout: &Layout,
    host: &WorkerHost,
    version: Option<&str>,
    alive: &[String],
) -> Result<(), String> {
    worker::poll(host, || async {
        let status = worker::worker_status(runner, layout)
            .await
            .map_err(|err| err.to_string())?;
        if let Some(version) = version
            && status.version != version
        {
            return Err(format!("worker runs {} (want {version})", status.version));
        }
        if !status.connected {
            return Err(format!("worker {} is not connected", status.version));
        }
        let missing: Vec<&str> = alive
            .iter()
            .filter(|id| {
                !status
                    .executions
                    .iter()
                    .any(|exec| exec.id == **id && exec.attached)
            })
            .map(String::as_str)
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!("executions not reattached: {}", missing.join(", ")))
        }
    })
    .await
}

#[allow(clippy::too_many_arguments)]
async fn transaction(
    args: &UpdateArgs,
    layout: &Layout,
    runner: &dyn Runner,
    src: &ReleaseSource,
    host: &WorkerHost,
    target: &Version,
    ops_dir: &Path,
    rec: &mut OpRecord,
) -> Result<OpResult, CtlError> {
    let current_link = layout.worker_current();
    let previous_name = link_name(&current_link);
    if let Some(prev) = previous_name.as_deref() {
        rec.previous = Version::parse(prev).ok();
        if let (Some(have), Some(want)) = (version_key(prev), version_key(target.as_str())) {
            if want < have {
                return Err(CtlError::DowngradeRefused {
                    current: prev.to_owned(),
                    target: target.to_string(),
                });
            }
            if want == have {
                return Ok(OpResult::Done);
            }
        }
    }

    ops::advance(ops_dir, rec, "preflight")?;
    let on_hub_host = layout.install_toml().is_file();
    let before = if args.hub_chain {
        wait_for_hub(runner, layout, host, target, args.force, on_hub_host).await?
    } else {
        let before = match worker::worker_status(runner, layout).await {
            Ok(status) => status,
            Err(_) if args.force => WorkerStatus::default(),
            Err(_) => return Err(CtlError::HubVersionUnknown),
        };
        check_hub_first(&before, target, args.force, on_hub_host)?;
        before
    };
    let in_use = releases_in_use(layout, &before)?;
    let alive: Vec<String> = before.executions.iter().map(|e| e.id.clone()).collect();

    ops::advance(ops_dir, rec, "stage")?;
    let assets = release::asset_names(target);
    let (staged, fetched) = fetch_assets(args, layout, src, target, &assets.worker).await?;
    let installed = worker::install_release(layout, &staged.files[&assets.worker], target);
    if fetched {
        let _ = std::fs::remove_dir_all(&staged.dir);
    }
    installed?;

    ops::advance(ops_dir, rec, "switch")?;
    let previous_rel = std::fs::read_link(&current_link).ok();
    if let Some(prev) = &previous_rel {
        fsx::replace_symlink(prev, &layout.worker_previous())?;
    }
    fsx::replace_symlink(&Path::new("releases").join(target.as_str()), &current_link)?;
    restart(runner).await?;

    ops::advance(ops_dir, rec, "verify")?;
    let failure = wait_reattached(runner, layout, host, Some(target.as_str()), &alive)
        .await
        .err();
    let Some(reason) = failure else {
        let mut keep = in_use;
        keep.insert(target.to_string());
        keep.extend(previous_name);
        retain(layout, &keep)?;
        return Ok(OpResult::Done);
    };

    ops::advance(ops_dir, rec, "rollback")?;
    let Some(prev) = previous_rel else {
        return Ok(OpResult::NeedsOwner {
            steps: vec![format!(
                "the new worker did not come up ({reason}) and there is no previous release to return to"
            )],
        });
    };
    fsx::replace_symlink(&prev, &current_link)?;
    restart(runner).await?;
    match wait_reattached(runner, layout, host, None, &alive).await {
        Ok(()) => Ok(OpResult::RolledBack { reason }),
        Err(again) => Ok(OpResult::NeedsOwner {
            steps: vec![
                format!("update to {target} failed: {reason}"),
                format!("after returning to {}: {again}", PathBuf::from(&prev).display()),
                "check `journalctl --user -u canterlot-worker` and `canterlot-worker status --json --config ~/.config/canterlot/worker.toml`".into(),
            ],
        }),
    }
}
