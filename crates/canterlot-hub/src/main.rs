use std::{fs, io::Write, path::PathBuf, process::ExitCode};

use canterlot_hub::{config::HubConfig, db, installation, machines};
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
            installation::check(&pool, &config.manifest)
                .await
                .map_err(|error| error.to_string())?;
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
                        "machine_id={}\ntoken={}",
                        machine.machine_id,
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
