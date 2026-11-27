// Ошибки busy и unfinished несут запись операции: этот состав зафиксирован контрактом.
#![allow(clippy::result_large_err)]

use std::path::PathBuf;

use canterlot_ctl::{
    cli::{Cli, Command},
    error::CtlError,
    install::{Host, install},
    join::join,
    layout::Layout,
    ops::OpResult,
    release::ReleaseSource,
    runner::SystemRunner,
    status::status,
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
    if let Err(error) = run(cli) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

#[tokio::main]
async fn run(cli: Cli) -> Result<(), CtlError> {
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
            Ok(())
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
            Ok(())
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
            Ok(())
        }
        Command::Adopt => Err(CtlError::NotImplemented("adopt")),
        Command::Update(args) => {
            if rustix::process::geteuid().is_root() {
                return Err(CtlError::NotImplemented("update on the hub host"));
            }
            let result = update_worker(
                &args,
                &user_layout(),
                &runner,
                &ReleaseSource::default(),
                &WorkerHost::from_env(),
            )
            .await?;
            match result {
                OpResult::Done => {
                    println!("worker update finished");
                    Ok(())
                }
                other => Err(CtlError::Other(format!("worker update: {other:?}"))),
            }
        }
        Command::Uninstall(args) => {
            let report = uninstall(&args, &user_layout(), &runner, &WorkerHost::from_env()).await?;
            print!("{report}");
            Ok(())
        }
        Command::Tls(_) => Err(CtlError::NotImplemented("tls add-name")),
        Command::HostAgent => Err(CtlError::NotImplemented("host-agent")),
    }
}
