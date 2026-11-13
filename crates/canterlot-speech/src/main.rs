use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::json;

use canterlot_speech::config::{Config, DEFAULT_SOCKET, verify_models};
use canterlot_speech::engine::{ChildSpec, Loader, serve_child, spawn_inprocess};
use canterlot_speech::fake::{FakeLoader, FakeSpec};
use canterlot_speech::native::{NativeLoader, init_native_logging};
use canterlot_speech::probe::{self, ProbeOptions};
use canterlot_speech::server::{Server, query_health};

#[derive(Parser)]
#[command(
    name = "canterlot-speech",
    version,
    about = "Служба распознавания речи Canterlot"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Служба на unix-сокете.
    Serve {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, hide = true)]
        fake_engine: Option<String>,
    },
    /// Состояние службы: JSON в stdout, код 0 при ok.
    Health {
        #[arg(long, default_value = DEFAULT_SOCKET)]
        socket: PathBuf,
    },
    /// Живой алгоритм на WAV-файле с замерами.
    Probe {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        wav: PathBuf,
        /// Подавать звук в темпе реального времени.
        #[arg(long)]
        realtime: bool,
        /// Строки JSON вместо текста.
        #[arg(long)]
        json: bool,
        /// Напечатать распознанный текст.
        #[arg(long)]
        text: bool,
    },
    /// Дочерний процесс движка (запускает serve).
    #[command(hide = true)]
    Engine {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        fake_engine: Option<String>,
    },
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
        .from_env_lossy();
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("рантайм tokio")
}

fn loader(config: &Config, fake: Option<&str>) -> Result<Box<dyn Loader>, String> {
    Ok(match fake {
        Some(spec) => Box::new(FakeLoader(FakeSpec::parse(spec)?)),
        None => Box::new(NativeLoader {
            config: config.clone(),
        }),
    })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            config,
            fake_engine,
        } => serve(config, fake_engine),
        Command::Health { socket } => health(socket),
        Command::Probe {
            config,
            wav,
            realtime,
            json,
            text,
        } => {
            if std::env::var_os("TRANSCRIBE_DUMP_DIR").is_some() {
                eprintln!("TRANSCRIBE_DUMP_DIR задан: transcribe.cpp записал бы тензоры на диск");
                return ExitCode::FAILURE;
            }
            let cfg = match Config::load(&config) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            init_native_logging();
            let l = Box::new(NativeLoader {
                config: cfg.clone(),
            });
            let opts = ProbeOptions {
                realtime,
                json,
                text,
            };
            match runtime().block_on(probe::run(cfg, l, &wav, opts)) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::Engine {
            config,
            fake_engine,
        } => engine(config, fake_engine),
    }
}

fn serve(path: PathBuf, fake: Option<String>) -> ExitCode {
    init_tracing();
    let config = match Config::load(&path) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(kind = "config_invalid", error = %e, "speech config rejected");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = verify_models(&config) {
        tracing::error!(kind = "model_mismatch", error = %e, "model files rejected");
        return ExitCode::FAILURE;
    }
    let program = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(kind = "self_exe_unknown", error = %e, "cannot locate own binary");
            return ExitCode::FAILURE;
        }
    };
    let mut args = vec![
        "engine".to_owned(),
        "--config".to_owned(),
        path.to_string_lossy().into_owned(),
    ];
    if let Some(f) = &fake {
        args.push("--fake-engine".into());
        args.push(f.clone());
    }
    let spec = ChildSpec { program, args };
    runtime().block_on(async move {
        let listener = match Server::bind(&config.socket).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(kind = "socket_bind_failed", socket = %config.socket.display(), error = %e, "cannot listen");
                return ExitCode::FAILURE;
            }
        };
        tracing::info!(
            kind = "speech_started",
            socket = %config.socket.display(),
            device = ?config.device,
            model = %config.model_name(),
            idle_unload_s = config.idle_unload.as_secs(),
            version = env!("CARGO_PKG_VERSION"),
            "speech service listening"
        );
        let server = Server::new(config, spec);
        let run = tokio::spawn(server.clone().run(listener));
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async { match &mut term { Some(t) => { t.recv().await; } None => std::future::pending().await } } => {}
        }
        tracing::info!(kind = "speech_stopping", "speech service stopping");
        server.shutdown();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), run).await;
        ExitCode::SUCCESS
    })
}

fn health(socket: PathBuf) -> ExitCode {
    let rt = runtime();
    match rt.block_on(query_health(&socket)) {
        Ok(v) => {
            let ok = v["ok"].as_bool().unwrap_or(false);
            let out =
                json!({"ok": ok, "engine": v["engine"], "mode": v["mode"], "model": v["model"]});
            println!("{out}");
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            println!(
                "{}",
                json!({"ok": false, "engine": null, "mode": null, "model": null})
            );
            eprintln!(
                "INFO kind=\"speech_unavailable\" socket=\"{}\" error=\"{}\"",
                socket.display(),
                e.kind()
            );
            ExitCode::FAILURE
        }
    }
}

fn engine(path: PathBuf, fake: Option<String>) -> ExitCode {
    // Звук и текст живут в памяти этого процесса: без дампа памяти и ptrace от того же uid.
    let _ = rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable);
    let config = match Config::load(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("transcribe: config {e}");
            return ExitCode::FAILURE;
        }
    };
    if fake.is_none() {
        init_native_logging();
    }
    let loader = match loader(&config, fake.as_deref()) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("transcribe: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime().block_on(async move {
        let link = spawn_inprocess(loader);
        serve_child(tokio::io::stdin(), tokio::io::stdout(), link).await;
    });
    // Рабочие потоки могут стоять в нативном прогоне: выход без ожидания их.
    std::process::exit(0)
}
