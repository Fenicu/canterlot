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
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
    let (config, store) = tokio::task::spawn_blocking(move || {
        let config = config::load(&path)?;
        let _token = config::read_token(&config.token_file)?;
        let store = WorkerStore::open(&config.data_dir)?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((config, store))
    })
    .await?
    .map_err(|error| -> Box<dyn std::error::Error> { error })?;
    let (stop, shutdown) = watch::channel(false);
    let worker = Worker::new(config, store);
    let run = worker.run(shutdown);
    tokio::pin!(run);
    tokio::select! {
        result = &mut run => result?,
        result = shutdown_signal() => {
            result?;
            stop.send_replace(true);
            run.await?;
        },
    }
    Ok(())
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
