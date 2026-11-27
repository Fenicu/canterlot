// Ошибки busy и unfinished несут запись операции: этот состав зафиксирован контрактом.
#![allow(clippy::result_large_err)]

use std::path::PathBuf;

use canterlot_ctl::{
    cli::{Cli, Command, TlsCommand},
    error::CtlError,
    hub_update::{HostRoute, host_route, update},
    install::{Host, install},
    join::join,
    layout::Layout,
    ops::OpResult,
    release::ReleaseSource,
    runner::SystemRunner,
    status::status,
    tls::add_name,
    uninstall::uninstall,
    worker::WorkerHost,
    worker_update::update_worker,
};
use clap::Parser;

/// Пути воркера — от домашнего каталога того, кто запустил утилиту.
fn user_layout() -> Layout {
    Layout::new(
        PathBuf::from("/"),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn main() {
    let cli = Cli::parse();
    match run(cli) {
        Ok(true) => {}
        // Операция завершилась, но не успехом (отказ, откат, ручные шаги): отчёт уже напечатан.
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn run(cli: Cli) -> Result<bool, CtlError> {
    // Корень — настоящий `/`; домашний каталог воркера установка узнаёт сама (getent).
    let layout = Layout::new(PathBuf::from("/"), None);
    let runner = SystemRunner;
    match cli.command() {
        Command::Install(args) => {
            let report = install(
                args,
                &layout,
                &runner,
                &ReleaseSource::default(),
                &Host::from_env(),
            )
            .await?;
            print!("{report}");
            Ok(true)
        }
        Command::Status(args) => {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            // Обычный пользователь видит свой воркер; root — то, что записано в install.toml.
            let layout = if rustix::process::geteuid().is_root() {
                layout
            } else {
                Layout::new(PathBuf::from("/"), home)
            };
            let report = status(&layout, &runner).await?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{report}");
            }
            Ok(true)
        }
        Command::Join(args) => {
            let report = join(
                &args,
                &user_layout(),
                &runner,
                &ReleaseSource::default(),
                &WorkerHost::from_env(),
            )
            .await?;
            print!("{report}");
            Ok(true)
        }
        Command::Adopt => Err(CtlError::NotImplemented("adopt")),
        Command::Update(args) => {
            // Обычный пользователь обновляет свой воркер, sudo — весь хост хаба.
            let worker_layout = user_layout();
            match host_route(
                &layout,
                &worker_layout,
                rustix::process::geteuid().is_root(),
            ) {
                HostRoute::Hub => {
                    // Сначала хаб, затем воркер этого хоста от его учётной записи.
                    let reports = update(
                        args,
                        &layout,
                        &runner,
                        &ReleaseSource::default(),
                        &Host::from_env(),
                    )
                    .await?;
                    for report in &reports {
                        print!("{report}");
                    }
                    Ok(reports.iter().all(|r| r.succeeded()))
                }
                HostRoute::Worker => {
                    let result = update_worker(
                        &args,
                        &worker_layout,
                        &runner,
                        &ReleaseSource::default(),
                        &WorkerHost::from_env(),
                    )
                    .await?;
                    match result {
                        OpResult::Done => {
                            println!("worker update finished");
                            Ok(true)
                        }
                        other => Err(CtlError::Other(format!("worker update: {other:?}"))),
                    }
                }
                HostRoute::NeedsRoot => Err(CtlError::RootRequired("update".into())),
            }
        }
        Command::Uninstall(args) => {
            let report = uninstall(&args, &user_layout(), &runner, &WorkerHost::from_env()).await?;
            print!("{report}");
            Ok(true)
        }
        Command::Tls(TlsCommand::AddName { name }) => {
            add_name(&name, &layout, &runner, &Host::from_env()).await?;
            println!("The hub certificate now also names {name}; the hub was restarted.");
            Ok(true)
        }
        Command::HostAgent => Err(CtlError::NotImplemented("host-agent")),
    }
}
