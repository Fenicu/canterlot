// Ошибки busy и unfinished несут запись операции: этот состав зафиксирован контрактом.
#![allow(clippy::result_large_err)]

use canterlot_ctl::cli::{Cli, Command};
use canterlot_ctl::error::CtlError;
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
    let name = match cli.command() {
        Command::Install(_) => "install",
        Command::Join(_) => "join",
        Command::Adopt => "adopt",
        Command::Update(_) => "update",
        Command::Uninstall(_) => "uninstall",
        Command::Status => "status",
        Command::Tls(_) => "tls add-name",
        Command::HostAgent => "host-agent",
    };
    Err(CtlError::NotImplemented(name))
}
