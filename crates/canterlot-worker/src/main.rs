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
    #[cfg(feature = "synthetic-adapter")]
    #[command(hide = true)]
    SyntheticCli {
        #[arg(long)]
        profile: String,
        #[arg(long)]
        native_conversation_id: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        enable: Vec<String>,
    },
    Run {
        #[arg(long)]
        config: PathBuf,
    },
    Supervise {
        #[arg(long)]
        exec_dir: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    if let Some(code) = supervise().await {
        return code;
    }
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn supervise() -> Option<ExitCode> {
    let Command::Supervise { exec_dir } = Cli::try_parse().ok()?.command else {
        return None;
    };
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
    Some(match canterlot_worker::supervisor::run(exec_dir).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    })
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = match Cli::parse().command {
        Command::Run { config } => config,
        Command::Supervise { .. } => return Ok(()),
        #[cfg(feature = "synthetic-adapter")]
        Command::SyntheticCli {
            profile,
            native_conversation_id,
            model,
            enable,
        } => {
            canterlot_worker::adapter::synthetic_cli::run(
                &profile,
                native_conversation_id,
                model,
                &enable,
            )
            .await?;
            return Ok(());
        }
    };
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
        result = shutdown_signals(stop) => result?,
    }
    Ok(())
}

async fn shutdown_signals(stop: watch::Sender<bool>) -> std::io::Result<()> {
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(unix)]
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    for _ in 0..2 {
        #[cfg(unix)]
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = terminate.recv() => {},
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await?;
        if stop.send_replace(true) {
            std::process::exit(130);
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_child() {
        let Ok(directory) = std::env::var("CANTERLOT_SIGNAL_TEST_DIR") else {
            return;
        };
        let directory = PathBuf::from(directory);
        let (stop, mut shutdown) = watch::channel(false);
        let signals = shutdown_signals(stop);
        tokio::pin!(signals);
        std::future::poll_fn(|cx| {
            assert!(signals.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        std::fs::write(directory.join("ready"), []).unwrap();
        tokio::select! {
            result = &mut signals => panic!("signal listener ended: {result:?}"),
            result = shutdown.changed() => result.unwrap(),
        }
        std::fs::write(directory.join("stopping"), []).unwrap();
        signals.await.unwrap();
        panic!("second signal did not exit");
    }

    #[tokio::test]
    async fn second_termination_signal_exits_immediately_with_130() {
        for (first, second) in [
            ("-TERM", "-TERM"),
            ("-INT", "-INT"),
            ("-TERM", "-INT"),
            ("-INT", "-TERM"),
        ] {
            let directory = tempfile::TempDir::new().unwrap();
            let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::shutdown_child"])
                .env("CANTERLOT_SIGNAL_TEST_DIR", directory.path())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            for (marker, signal) in [("ready", first), ("stopping", second)] {
                tokio::time::timeout(Duration::from_secs(3), async {
                    while !directory.path().join(marker).exists() {
                        assert!(child.try_wait().unwrap().is_none());
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .unwrap();
                assert!(
                    std::process::Command::new("kill")
                        .args([signal, &child.id().unwrap().to_string()])
                        .status()
                        .unwrap()
                        .success()
                );
            }
            let status = tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(status.code(), Some(130));
        }
    }
}
