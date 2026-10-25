#![forbid(unsafe_code)]

use std::{path::PathBuf, process::ExitCode};

use canterlot_worker::{config, session::Worker, store::WorkerStore};
use clap::{Parser, Subcommand};
use tokio::sync::watch;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let Command::Run { config: path } = Cli::parse().command;
    let config = config::load(&path)?;
    let _token = config::read_token(&config.token_file)?;
    let store = WorkerStore::open(&config.data_dir)?;
    let (stop, shutdown) = watch::channel(false);
    let worker = Worker::new(config, store);
    tokio::select! {
        result = worker.run(shutdown) => result?,
        result = tokio::signal::ctrl_c() => { result?; stop.send_replace(true); },
    }
    Ok(())
}
