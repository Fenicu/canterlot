use std::{fs, io::Write, path::PathBuf, process::ExitCode};

use canterlot_hub::{
    config::HubConfig, db, dispatcher, grpc, ingest, installation, machines, outbox,
    session::Registry,
};
use clap::{Parser, Subcommand};
use uuid::Uuid;

#[derive(Parser)]
#[command(version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct ConfigArgs {
    #[arg(long)]
    config: PathBuf,
}

#[derive(Subcommand)]
enum Command {
    Init(ConfigArgs),
    Machine {
        #[command(subcommand)]
        command: MachineCommand,
    },
    Serve(ConfigArgs),
}

#[derive(Subcommand)]
enum MachineCommand {
    Add {
        #[command(flatten)]
        config: ConfigArgs,
        #[arg(long)]
        name: String,
    },
    Revoke {
        #[command(flatten)]
        config: ConfigArgs,
        #[arg(long)]
        machine_id: Uuid,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            eprintln!("{code}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    let config = match &args.command {
        Command::Init(config) | Command::Serve(config) => config,
        Command::Machine { command } => match command {
            MachineCommand::Add { config, .. } | MachineCommand::Revoke { config, .. } => config,
        },
    };
    let config = HubConfig::load(&config.config).map_err(|error| error.to_string())?;
    let database_url = fs::read_to_string(&config.database_url_file)
        .map_err(|_| "database_url_file_error".to_owned())?;
    let database_url = database_url.trim();
    if database_url.is_empty() {
        return Err("database_url_invalid".into());
    }
    let pool = db::connect(database_url)
        .await
        .map_err(|_| "db_error".to_owned())?;
    match args.command {
        Command::Init(_) => {
            db::MIGRATOR
                .run(&pool)
                .await
                .map_err(|_| "migration_error".to_owned())?;
            installation::init(&pool, &config.manifest)
                .await
                .map_err(|error| error.to_string())?;
        }
        Command::Serve(_) => {
            let installation = installation::check(&pool, &config.manifest)
                .await
                .map_err(|error| error.to_string())?;
            serve(pool.clone(), config, installation).await?;
        }
        Command::Machine { command } => {
            let installation = installation::check(&pool, &config.manifest)
                .await
                .map_err(|error| error.to_string())?;
            match command {
                MachineCommand::Add { name, .. } => {
                    let machine = machines::add(&pool, installation.owner_id, &name)
                        .await
                        .map_err(|_| "db_error".to_owned())?;
                    writeln!(
                        std::io::stdout().lock(),
                        "machine_id={}\ninstallation_id={}\nrestore_generation={}\ntoken={}",
                        machine.machine_id,
                        installation.installation_id,
                        installation.restore_generation,
                        machine.token
                    )
                    .map_err(|_| "output_error".to_owned())?;
                }
                MachineCommand::Revoke { machine_id, .. } => {
                    machines::revoke(&pool, machine_id)
                        .await
                        .map_err(|_| "db_error".to_owned())?;
                }
            }
        }
    }
    pool.close().await;
    Ok(())
}

async fn serve(
    pool: sqlx::PgPool,
    config: HubConfig,
    installation: installation::Installation,
) -> Result<(), String> {
    let cert =
        fs::read_to_string(&config.tls_cert).map_err(|_| "tls_cert_file_error".to_owned())?;
    let key = fs::read_to_string(&config.tls_key).map_err(|_| "tls_key_file_error".to_owned())?;
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| "signal_error".to_owned())?;
    let registry = Registry::new(config.timers.clone());
    let (dispatcher, rx) = dispatcher::Dispatcher::channel();
    let mut state = grpc::HubGrpc::new(
        pool.clone(),
        registry.clone(),
        installation,
        config.timers.clone(),
    );
    state.ingest =
        std::sync::Arc::new(ingest::Ingest::new(pool.clone()).with_dispatcher(dispatcher));
    let (shutdown, stopped) = tokio::sync::watch::channel(false);
    let outbox = tokio::spawn(outbox::run(
        pool.clone(),
        registry.clone(),
        config.timers.clone(),
        stopped.clone(),
    ));
    let dispatch = tokio::spawn(dispatcher::run(pool, registry, rx, config.timers, stopped));
    let signal = shutdown.clone();
    let result = grpc::serve(state, config.grpc_listen, &cert, &key, async move {
        tokio::select! {
            _ = sigterm.recv() => {},
            _ = tokio::signal::ctrl_c() => {},
        }
        let _ = signal.send(true);
    })
    .await;
    let _ = shutdown.send(true);
    let (outbox, dispatch) = tokio::join!(outbox, dispatch);
    outbox.map_err(|_| "outbox_task_error".to_owned())?;
    dispatch.map_err(|_| "dispatcher_task_error".to_owned())?;
    result.map_err(|_| "grpc_server_error".to_owned())
}
