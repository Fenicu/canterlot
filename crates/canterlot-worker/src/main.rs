#![forbid(unsafe_code)]

use std::{io::Write, path::PathBuf, process::ExitCode};

use canterlot_worker::{
    config,
    session::{Worker, WorkerError},
    store::WorkerStore,
};
use clap::{Parser, Subcommand, ValueEnum};
use tokio::sync::watch;

#[derive(Parser)]
#[command(version)]
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
    #[cfg(feature = "synthetic-adapter")]
    #[command(hide = true)]
    CodexReplay {
        #[arg(long)]
        evidence: PathBuf,
        #[arg(long)]
        fixtures_dir: PathBuf,
        /// Аргументы, с которыми адаптер запускает codex (`--version`, `app-server …`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        codex_args: Vec<String>,
    },
    #[command(hide = true)]
    McpRelay {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Оболочка одной задачи на месте; запускается только воркером.
    #[command(hide = true, name = "job-shim")]
    JobShim {
        /// Каталог попытки: `<data_dir>/jobs/<job_id>/<attempt>`.
        dir: PathBuf,
    },
    Supervise {
        #[arg(long)]
        exec_dir: PathBuf,
    },
    /// Напечатать возможности и харнессы этой сборки.
    ///
    /// Строки `capability <имя>` и `harness <имя>`, без конфига, файлов и сети: по ним
    /// инструменты выкладки сверяют развёрнутый воркер с выпуском.
    Capabilities,
    /// Аренда тяжёлых задач машины (очередь сборок и гейтов в воркере).
    Lease {
        #[command(subcommand)]
        command: LeaseCommand,
    },
    /// Разовый импорт памяти Claude: план и пакет. Только чтение, без конфига и сети.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    /// Задачи владельца на раннерах и на этой машине без разговора (только на несущей).
    Job {
        #[command(subcommand)]
        command: JobCommand,
    },
}

#[derive(Subcommand)]
enum JobCommand {
    /// Отправить команду хабу как задачу владельца.
    ///
    /// Без `--wait` печатает идентификатор задачи и выходит с нулём. С `--wait` ждёт итог,
    /// печатает хвост журнала, каталог результатов и выходит с кодом задачи: 124 по таймауту,
    /// 130 при отмене, 1 для `failed` и `lost` без кода. Отказ принять задачу — код 2.
    Run {
        #[arg(long)]
        title: String,
        /// Имя машины или `auto`; с `--source` по умолчанию `auto`.
        #[arg(long)]
        machine: Option<String>,
        /// Метки раннера через запятую.
        #[arg(long, value_delimiter = ',')]
        labels: Vec<String>,
        #[arg(long)]
        expected_minutes: Option<u32>,
        #[arg(long, value_enum)]
        lease: Option<JobLease>,
        #[arg(long)]
        timeout_minutes: Option<u32>,
        /// `<репозиторий>@<полный sha>`; `@HEAD` — текущий коммит чистого дерева. Обязателен:
        /// задача владельца идёт только на подготовленные исходники.
        #[arg(long)]
        source: Option<String>,
        /// Путь относительно рабочей копии, который надо собрать; можно повторять.
        #[arg(long)]
        output: Vec<String>,
        #[arg(long)]
        wait: bool,
        #[arg(last = true, required = true, num_args = 1..)]
        command: Vec<std::ffi::OsString>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum JobLease {
    None,
    Build,
    Gate,
}

#[derive(Subcommand)]
enum MemoryCommand {
    /// Составить манифест TOML по каталогу `~/.claude/projects` для просмотра владельцем.
    ImportPlan {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Корень рабочих проектов (домен work); можно повторять. По умолчанию `~/work`.
        #[arg(long)]
        work_root: Vec<PathBuf>,
    },
    /// Собрать JSON-пакет по просмотренному манифесту; расхождение хэша отказывает.
    ImportBundle {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Subcommand)]
enum LeaseCommand {
    /// Дождаться слота и запустить команду с ограничениями ресурсов.
    ///
    /// Без воркера, при отказе сокета или ошибке протокола команда запускается без аренды
    /// с одной строкой в stderr. Код выхода — код команды; 127 — команда не найдена.
    Run {
        #[arg(long, value_enum)]
        kind: LeaseKind,
        #[arg(long)]
        label: Option<String>,
        /// Строгий режим: без запуска вне аренды. Нет воркера или сокета — ждать, отказ
        /// протокола — код 75. Так ждут своей очереди задачи на месте.
        #[arg(long)]
        strict: bool,
        #[arg(last = true, required = true, num_args = 1..)]
        command: Vec<std::ffi::OsString>,
    },
    /// Напечатать держателей и очередь.
    Status {
        /// Одна строка JSON вместо таблицы.
        #[arg(long)]
        json: bool,
    },
    #[command(hide = true)]
    Raise {
        #[arg(long)]
        signal: i32,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum LeaseKind {
    Gate,
    Build,
}

#[tokio::main]
async fn main() -> ExitCode {
    if let Some(code) = capabilities() {
        return code;
    }
    if let Some(code) = lease().await {
        return code;
    }
    if let Some(code) = owner_job().await {
        return code;
    }
    if let Some(code) = memory() {
        return code;
    }
    if let Some(code) = codex_replay().await {
        return code;
    }
    if let Some(code) = job_shim().await {
        return code;
    }
    if let Some(code) = mcp_relay().await {
        return code;
    }
    if let Some(code) = supervise().await {
        return code;
    }
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if let Some(code) = error
                .downcast_ref::<WorkerError>()
                .and_then(WorkerError::exit_code)
            {
                return ExitCode::from(code);
            }
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn capabilities() -> Option<ExitCode> {
    let Command::Capabilities = Cli::try_parse().ok()?.command else {
        return None;
    };
    let lines = canterlot_worker::session::OPTIONAL_CAPABILITIES
        .iter()
        .map(|name| format!("capability {name}\n"))
        .chain(
            canterlot_worker::adapter::HARNESSES
                .iter()
                .map(|name| format!("harness {name}\n")),
        )
        .collect::<String>();
    // Без `print!`: закрытый канал (`| head`) даёт код ошибки, а не панику.
    let mut stdout = std::io::stdout().lock();
    let written = stdout.write_all(lines.as_bytes());
    Some(match written.and_then(|()| stdout.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("capabilities: {error}");
            ExitCode::FAILURE
        }
    })
}

fn memory() -> Option<ExitCode> {
    use canterlot_worker::memory_import::{Manifest, PlanOptions, bundle, plan};
    let Command::Memory { command } = Cli::try_parse().ok()?.command else {
        return None;
    };
    let result = (|| -> Result<String, String> {
        match command {
            MemoryCommand::ImportPlan {
                root,
                out,
                work_root,
            } => {
                let work_roots = if work_root.is_empty() {
                    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
                    vec![PathBuf::from(home).join("work")]
                } else {
                    work_root
                };
                let manifest =
                    plan(&root, &PlanOptions { work_roots }).map_err(|e| e.to_string())?;
                let text = manifest.to_toml().map_err(|e| e.to_string())?;
                std::fs::write(&out, text).map_err(|e| format!("write {}: {e}", out.display()))?;
                let files: usize = manifest.dir.iter().map(|d| d.file.len()).sum();
                let skipped: usize = manifest
                    .dir
                    .iter()
                    .flat_map(|d| &d.file)
                    .filter(|f| !f.skip.is_empty())
                    .count();
                let ambiguous = manifest.dir.iter().filter(|d| d.ambiguous).count();
                Ok(format!(
                    "manifest: {} dirs, {files} files, {skipped} skipped, {ambiguous} ambiguous",
                    manifest.dir.len()
                ))
            }
            MemoryCommand::ImportBundle { manifest, out } => {
                let text = std::fs::read_to_string(&manifest)
                    .map_err(|e| format!("read {}: {e}", manifest.display()))?;
                let manifest = Manifest::from_toml(&text).map_err(|e| e.to_string())?;
                let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                let id = uuid::Uuid::now_v7().to_string();
                let bundle = bundle(&manifest, &id, &now).map_err(|e| e.to_string())?;
                let json = serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?;
                std::fs::write(&out, json).map_err(|e| format!("write {}: {e}", out.display()))?;
                Ok(format!("bundle {id}: {} entries", bundle.entries.len()))
            }
        }
    })();
    Some(match result {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("memory: {error}");
            ExitCode::FAILURE
        }
    })
}

async fn owner_job() -> Option<ExitCode> {
    use canterlot_worker::owner_jobs::client;
    let Command::Job { command } = Cli::try_parse().ok()?.command else {
        return None;
    };
    let JobCommand::Run {
        title,
        machine,
        labels,
        expected_minutes,
        lease,
        timeout_minutes,
        source,
        output,
        wait,
        command,
    } = command;
    Some(
        client::run(client::RunArgs {
            title,
            machine,
            labels,
            expected_minutes,
            lease: lease.map(|lease| {
                match lease {
                    JobLease::None => "none",
                    JobLease::Build => "build",
                    JobLease::Gate => "gate",
                }
                .to_owned()
            }),
            timeout_minutes,
            source,
            outputs: output,
            wait,
            command,
        })
        .await,
    )
}

async fn job_shim() -> Option<ExitCode> {
    let Command::JobShim { dir } = Cli::try_parse().ok()?.command else {
        return None;
    };
    Some(canterlot_worker::jobs::shim::run(dir).await)
}

async fn lease() -> Option<ExitCode> {
    use canterlot_worker::lease::{Kind, client};
    let Command::Lease { command } = Cli::try_parse().ok()?.command else {
        return None;
    };
    Some(match command {
        LeaseCommand::Run {
            kind,
            label,
            strict,
            command,
        } => {
            let kind = match kind {
                LeaseKind::Gate => Kind::Gate,
                LeaseKind::Build => Kind::Build,
            };
            client::run(client::RunArgs {
                kind,
                label,
                strict,
                command,
            })
            .await
        }
        LeaseCommand::Status { json } => client::status(json).await,
        LeaseCommand::Raise { signal } => client::raise(signal),
    })
}

#[cfg(feature = "synthetic-adapter")]
async fn codex_replay() -> Option<ExitCode> {
    let Command::CodexReplay {
        evidence,
        fixtures_dir,
        codex_args,
    } = Cli::try_parse().ok()?.command
    else {
        return None;
    };
    // Проверка версии адаптера видит версию записи; своя версия воркера — у `--version` верхнего уровня.
    if codex_args.iter().any(|arg| arg == "--version") {
        println!("codex-cli 0.164.0");
        return Some(ExitCode::SUCCESS);
    }
    Some(
        match canterlot_worker::adapter::codex::replay::run(&evidence, &fixtures_dir).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("codex-replay: {error}");
                ExitCode::FAILURE
            }
        },
    )
}

#[cfg(not(feature = "synthetic-adapter"))]
async fn codex_replay() -> Option<ExitCode> {
    None
}

async fn mcp_relay() -> Option<ExitCode> {
    let Command::McpRelay { socket } = Cli::try_parse().ok()?.command else {
        return None;
    };
    // Без tracing: в stdout идёт только JSON-RPC.
    match canterlot_worker::mcp_relay::run(&socket).await {
        Ok(()) => Some(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("mcp-relay: {error}");
            Some(ExitCode::FAILURE)
        }
    }
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

async fn supervise() -> Option<ExitCode> {
    let Command::Supervise { exec_dir } = Cli::try_parse().ok()?.command else {
        return None;
    };
    init_tracing();
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
        Command::McpRelay { .. }
        | Command::JobShim { .. }
        | Command::Supervise { .. }
        | Command::Capabilities
        | Command::Memory { .. }
        | Command::Job { .. }
        | Command::Lease { .. } => {
            return Ok(());
        }
        #[cfg(feature = "synthetic-adapter")]
        Command::CodexReplay { .. } => return Ok(()),
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
    init_tracing();
    // Каталог данных нужен для слова `fatal` и тогда, когда сам конфиг не разобрался.
    let data_dir_hint = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| canterlot_worker::fatal::data_dir_hint(&text));
    let loaded = tokio::task::spawn_blocking(move || {
        let config = config::load(&path)?;
        let _token = config::read_token(&config.token_file)?;
        let store = WorkerStore::open(&config.data_dir)?;
        // Обычный запуск: слово прошлого смертельного выхода отработано.
        canterlot_worker::fatal::clear(&config.data_dir);
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((config, store))
    })
    .await?;
    let (config, store) = match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            let reason = error
                .downcast_ref::<config::ConfigError>()
                .and_then(config::ConfigError::fatal_reason)
                .or_else(|| {
                    error
                        .downcast_ref::<canterlot_worker::store::StoreError>()
                        .and_then(canterlot_worker::store::StoreError::fatal_reason)
                });
            if let (Some(dir), Some(reason)) = (&data_dir_hint, reason) {
                canterlot_worker::fatal::write(dir, reason);
            }
            return Err(error);
        }
    };
    let data_dir = config.data_dir.clone();
    let (stop, shutdown) = watch::channel(false);
    let worker = Worker::new(config, store);
    let run = worker.run(shutdown);
    tokio::pin!(run);
    tokio::select! {
        result = &mut run => {
            if let Err(error) = &result
                && let Some(reason) = error.fatal_reason()
            {
                canterlot_worker::fatal::write(&data_dir, reason);
            }
            result?
        }
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
