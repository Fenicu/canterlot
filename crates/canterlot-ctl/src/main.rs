// Ошибки busy и unfinished несут запись операции: этот состав зафиксирован контрактом.
#![allow(clippy::result_large_err)]

use std::path::PathBuf;

use canterlot_ctl::{
    cli::{Cli, Command},
    error::CtlError,
    install::{Host, install},
    layout::Layout,
    release::ReleaseSource,
    runner::SystemRunner,
    status::status,
};
use clap::Parser;

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
        Command::Join(_) => Err(CtlError::NotImplemented("join")),
        Command::Adopt => Err(CtlError::NotImplemented("adopt")),
        Command::Update(_) => Err(CtlError::NotImplemented("update")),
        Command::Uninstall(_) => Err(CtlError::NotImplemented("uninstall")),
        Command::Tls(_) => Err(CtlError::NotImplemented("tls add-name")),
        Command::HostAgent => Err(CtlError::NotImplemented("host-agent")),
    }
}
