#![forbid(unsafe_code)]

use std::{fs, io::Write, path::PathBuf, process::ExitCode};

use canterlot_hub::{
    background::{Background, BackgroundDeps},
    config::HubConfig,
    db, dispatcher, enroll, grpc, http, ingest, installation, machines, presets,
    session::Registry,
};
use clap::{Parser, Subcommand};
use tracing_subscriber::{Layer, prelude::*};
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
    /// Вход владельца: ссылка восстановления, пароль, сеансы.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// База мест по IP (DB-IP City Lite).
    Geoip {
        #[command(subcommand)]
        command: GeoipCommand,
    },
    /// Печатает документ OpenAPI (содержимое docs/api/openapi.json) без конфига и базы.
    Openapi,
    /// Миграции, вшитые в этот бинарь (без конфига и базы).
    Migrations {
        #[command(subcommand)]
        command: MigrationsCommand,
    },
    /// Закрытая проверка готовности: один JSON-отчёт и код выхода.
    Probe(ConfigArgs),
    /// Общая память агентов.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
}

#[derive(Subcommand)]
enum MemoryCommand {
    /// Применить пакет импорта памяти Claude одной транзакцией.
    Import {
        #[command(flatten)]
        config: ConfigArgs,
        /// Пакет, собранный `canterlot-worker memory import-bundle`.
        #[arg(long)]
        bundle: PathBuf,
        /// Выполнить всё и откатить: отчёт тот же, записей нет.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum MigrationsCommand {
    /// Печатает JSON со всеми вшитыми миграциями: версия, описание, контрольная сумма.
    Manifest {
        /// Формат вывода один, флаг оставлен для явности вызова из выката.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum MachineCommand {
    Add {
        #[command(flatten)]
        config: ConfigArgs,
        #[arg(long)]
        name: String,
        /// `worker` (по умолчанию) или `runner`: раннер ведёт несущая машина по SSH.
        #[arg(long, value_parser = ["worker", "runner"])]
        role: Option<String>,
        /// Раннер: несущая машина (существующая неотозванная машина `worker`).
        #[arg(long)]
        parent: Option<Uuid>,
        /// Раннер: корневой каталог на раннере, абсолютный и нормализованный.
        #[arg(long)]
        root_dir: Option<String>,
        /// Раннер: ранг размещения, больше — предпочтительнее.
        #[arg(long, allow_hyphen_values = true)]
        rank: Option<i32>,
        /// Раннер: метки через запятую.
        #[arg(long, value_delimiter = ',')]
        labels: Vec<String>,
    },
    /// Пометить машину несущей раннеров или снять пометку.
    SetCarrier {
        #[command(flatten)]
        config: ConfigArgs,
        #[arg(long)]
        machine_id: Uuid,
        #[arg(long)]
        off: bool,
    },
    /// Изменить ранг, метки или корень машины: метки у любой (в том числе у самой VM, например
    /// `kernel-6.12`), корень только у раннера.
    Set {
        #[command(flatten)]
        config: ConfigArgs,
        #[arg(long)]
        machine_id: Uuid,
        /// Ранг размещения, больше — предпочтительнее.
        #[arg(long, allow_hyphen_values = true)]
        rank: Option<i32>,
        /// Метки через запятую; пустое значение снимает все.
        #[arg(long, value_delimiter = ',')]
        labels: Option<Vec<String>>,
        /// Раннер: корневой каталог, абсолютный и нормализованный, до 90 байт.
        #[arg(long)]
        root_dir: Option<String>,
    },
    RotateToken {
        #[command(flatten)]
        config: ConfigArgs,
        #[arg(long)]
        machine_id: Uuid,
        #[arg(long)]
        reset_local_state: bool,
        #[arg(long)]
        reactivate: bool,
    },
    Revoke {
        #[command(flatten)]
        config: ConfigArgs,
        #[arg(long)]
        machine_id: Uuid,
    },
    /// Завести машину «ожидает подключения» и выдать одноразовый код привязки на 15 минут.
    JoinCode {
        #[command(flatten)]
        config: ConfigArgs,
        #[arg(long)]
        name: String,
        /// Адрес gRPC хаба для новой машины (`host:port`); по умолчанию `[enroll] grpc_endpoint`.
        #[arg(long)]
        endpoint: Option<String>,
    },
}

#[derive(Subcommand)]
enum GeoipCommand {
    /// Скачать свежую базу в `[auth.geoip].path`; при ошибке прежний файл остаётся.
    Update {
        #[command(flatten)]
        config: ConfigArgs,
    },
    /// Напечатать действующий `[auth.geoip].path` (выкат проверяет возраст этого файла).
    Path {
        #[command(flatten)]
        config: ConfigArgs,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Одноразовая ссылка входа (по умолчанию на 10 минут, не больше 60).
    RecoveryLink {
        #[command(flatten)]
        config: ConfigArgs,
        #[arg(long)]
        minutes: Option<u32>,
        /// Отправить ссылку только в Gotify владельца и напечатать «sent».
        #[arg(long)]
        push: bool,
    },
    /// Задать пароль: дважды из TTY без эха или одной строкой из stdin.
    SetPassword {
        #[command(flatten)]
        config: ConfigArgs,
        #[arg(long)]
        stdin: bool,
        /// Также завершить все сеансы.
        #[arg(long)]
        revoke_sessions: bool,
    },
    /// Живые сеансы владельца.
    Sessions {
        #[command(flatten)]
        config: ConfigArgs,
    },
    /// Завершить все сеансы.
    RevokeAll {
        #[command(flatten)]
        config: ConfigArgs,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    // Проба не открывает базу: иначе она не смогла бы проверить готовность самой базы.
    if let Command::Probe(config) = &args.command {
        return ExitCode::from(canterlot_hub::probe::run(&config.config).await);
    }
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            eprintln!("{code}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), String> {
    if matches!(&args.command, Command::Serve(_)) {
        http::layers::install_panic_hook();
    }
    let config = match &args.command {
        Command::Openapi => {
            return write!(std::io::stdout().lock(), "{}", http::openapi::render())
                .map_err(|_| "output_error".to_owned());
        }
        Command::Migrations {
            command: MigrationsCommand::Manifest { .. },
        } => {
            return writeln!(
                std::io::stdout().lock(),
                "{}",
                db::migration_manifest_json()
            )
            .map_err(|_| "output_error".to_owned());
        }
        Command::Probe(_) => unreachable!("handled in main before run"),
        Command::Init(config) | Command::Serve(config) => config,
        Command::Machine { command } => match command {
            MachineCommand::Add { config, .. }
            | MachineCommand::SetCarrier { config, .. }
            | MachineCommand::Set { config, .. }
            | MachineCommand::Revoke { config, .. }
            | MachineCommand::RotateToken { config, .. }
            | MachineCommand::JoinCode { config, .. } => config,
        },
        Command::Auth { command } => match command {
            AuthCommand::RecoveryLink { config, .. }
            | AuthCommand::SetPassword { config, .. }
            | AuthCommand::Sessions { config }
            | AuthCommand::RevokeAll { config } => config,
        },
        Command::Geoip {
            command: GeoipCommand::Update { config } | GeoipCommand::Path { config },
        } => config,
        Command::Memory {
            command: MemoryCommand::Import { config, .. },
        } => config,
    };
    let config = HubConfig::load(&config.config).map_err(|error| error.to_string())?;
    if let Command::Geoip {
        command: GeoipCommand::Path { .. },
    } = &args.command
    {
        return writeln!(
            std::io::stdout().lock(),
            "{}",
            config.auth.geoip.path.display()
        )
        .map_err(|_| "output_error".to_owned());
    }
    if let Command::Geoip {
        command: GeoipCommand::Update { .. },
    } = &args.command
    {
        let name = canterlot_hub::owner_auth::geo::update(
            &config.auth.geoip.path,
            canterlot_hub::owner_auth::geo::DBIP_BASE_URL,
            chrono::Utc::now().date_naive(),
        )
        .await?;
        return writeln!(std::io::stdout().lock(), "updated {name}")
            .map_err(|_| "output_error".to_owned());
    }
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
                .map_err(|error| db::migration_error_code(&error))?;
            installation::init(&pool, &config.manifest)
                .await
                .map_err(|error| error.to_string())?;
        }
        Command::Serve(_) => {
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_target(false)
                        .with_ansi(false)
                        .with_writer(std::io::stderr)
                        .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                            metadata.target().starts_with("canterlot_hub")
                        })),
                )
                .init();
            config.log_startup_warnings();
            installation::check_manifest(&config.manifest).map_err(|error| error.to_string())?;
            db::MIGRATOR
                .run(&pool)
                .await
                .map_err(|error| db::migration_error_code(&error))?;
            let version: i64 = sqlx::query_scalar(
                "select coalesce(max(version), 0) from _sqlx_migrations where success",
            )
            .fetch_one(&pool)
            .await
            .map_err(|_| "migration_error".to_owned())?;
            tracing::info!(kind = "migrations_applied", version, "schema ready");
            let installation = installation::check(&pool, &config.manifest)
                .await
                .map_err(|error| error.to_string())?;
            presets::seed_all_missing(&pool)
                .await
                .map_err(|_| "presets_seed_error".to_owned())?;
            serve(pool.clone(), config, installation).await?;
        }
        Command::Probe(_) => unreachable!("handled in main before run"),
        Command::Openapi | Command::Migrations { .. } => {
            unreachable!("handled before the config is loaded")
        }
        Command::Geoip { .. } => unreachable!("handled before the database is opened"),
        Command::Memory {
            command: MemoryCommand::Import {
                bundle, dry_run, ..
            },
        } => {
            use canterlot_hub::memory::import;
            let text = fs::read_to_string(&bundle).map_err(|_| "bundle_read_error".to_owned())?;
            let bundle: import::Bundle =
                serde_json::from_str(&text).map_err(|_| "bundle_invalid".to_owned())?;
            let report = import::apply(&pool, &bundle, dry_run, &config.memory)
                .await
                .map_err(|error| error.to_string())?;
            let report = serde_json::to_string(&report).map_err(|_| "output_error".to_owned())?;
            writeln!(
                std::io::stdout().lock(),
                "{}{report}",
                if dry_run { "dry-run " } else { "" }
            )
            .map_err(|_| "output_error".to_owned())?;
        }
        Command::Auth { command } => {
            use canterlot_hub::owner_auth::cli;
            let installation = installation::check(&pool, &config.manifest)
                .await
                .map_err(|error| error.to_string())?;
            let owner = installation.owner_id;
            let output = match command {
                AuthCommand::RecoveryLink { minutes, push, .. } => {
                    cli::recovery_link(&pool, &config, owner, minutes, push).await?
                }
                AuthCommand::SetPassword {
                    stdin,
                    revoke_sessions,
                    ..
                } => {
                    let password = cli::read_password(stdin)?;
                    cli::set_password(&pool, owner, password, revoke_sessions).await?
                }
                AuthCommand::Sessions { .. } => cli::list_sessions(&pool, owner).await?,
                AuthCommand::RevokeAll { .. } => cli::revoke_all(&pool, owner).await?,
            };
            let output = output.trim_end_matches('\n');
            if !output.is_empty() {
                writeln!(std::io::stdout().lock(), "{output}")
                    .map_err(|_| "output_error".to_owned())?;
            }
        }
        Command::Machine { command } => {
            let installation = installation::check(&pool, &config.manifest)
                .await
                .map_err(|error| error.to_string())?;
            match command {
                MachineCommand::Add {
                    name,
                    role,
                    parent,
                    root_dir,
                    rank,
                    labels,
                    ..
                } => {
                    let machine = if role.as_deref() == Some(machines::ROLE_RUNNER) {
                        let spec = machines::RunnerSpec {
                            parent: parent.ok_or("parent_required")?,
                            root_dir: root_dir.ok_or("root_dir_required")?,
                            rank: rank.unwrap_or(0),
                            labels,
                        };
                        machines::add_runner(&pool, installation.owner_id, &name, &spec)
                            .await
                            .map_err(|error| error.to_string())?
                    } else {
                        // Флаги раннера без `--role runner` — ошибка, а не молчаливое «worker».
                        if parent.is_some()
                            || root_dir.is_some()
                            || rank.is_some()
                            || !labels.is_empty()
                        {
                            return Err("runner_flags_need_role_runner".into());
                        }
                        machines::add(&pool, installation.owner_id, &name)
                            .await
                            .map_err(|_| "db_error".to_owned())?
                    };
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
                MachineCommand::SetCarrier {
                    machine_id, off, ..
                } => {
                    machines::set_carrier(&pool, machine_id, !off)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                MachineCommand::Set {
                    machine_id,
                    rank,
                    labels,
                    root_dir,
                    ..
                } => {
                    let attrs = machines::MachineAttrs {
                        rank,
                        labels: labels.map(|labels| {
                            labels
                                .into_iter()
                                .filter(|label| !label.is_empty())
                                .collect()
                        }),
                        root_dir,
                    };
                    machines::set_attrs(&pool, machine_id, &attrs)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                MachineCommand::RotateToken {
                    machine_id,
                    reset_local_state,
                    reactivate,
                    ..
                } => {
                    let token =
                        machines::rotate_token(&pool, machine_id, reset_local_state, reactivate)
                            .await
                            .map_err(|error| error.to_string())?
                            .ok_or("machine_not_found")?;
                    writeln!(std::io::stdout().lock(), "token={token}")
                        .map_err(|_| "output_error".to_owned())?;
                }
                MachineCommand::Revoke { machine_id, .. } => {
                    if !machines::revoke(&pool, machine_id)
                        .await
                        .map_err(|_| "db_error".to_owned())?
                    {
                        return Err("machine_not_found".into());
                    }
                }
                MachineCommand::JoinCode { name, endpoint, .. } => {
                    let settings = enroll::EnrollSettings::from_config(&config)?;
                    let ca = settings.ca_der.as_deref().ok_or("ca_unavailable")?;
                    let endpoint = endpoint
                        .or(settings.grpc_endpoint.clone())
                        .ok_or("enroll_endpoint_missing")?;
                    let issued = enroll::issue(
                        &pool,
                        installation.owner_id,
                        &name,
                        &endpoint,
                        &settings.install_url,
                        ca,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                    writeln!(
                        std::io::stdout().lock(),
                        "machine_id={}\ncode={}\nexpires_at={}\ncommand={}",
                        issued.machine_id,
                        issued.code,
                        issued
                            .expires_at
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        issued.command
                    )
                    .map_err(|_| "output_error".to_owned())?;
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
    let enroll_settings = enroll::EnrollSettings::from_config(&config).map_err(str::to_owned)?;
    if enroll_settings.ca_der.is_none() {
        tracing::info!(
            kind = "enroll_ca_missing",
            "no tls_ca configured: join codes are unavailable"
        );
    }
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| "signal_error".to_owned())?;
    let HubConfig {
        grpc_listen,
        manifest,
        timers,
        http: http_config,
        limits,
        pricing,
        activity,
        hibernate,
        scheduler: scheduler_config,
        machines,
        attachments,
        session_tools,
        memory,
        jobs,
        sessions,
        orchestrator,
        notify,
        auth,
        provider_status,
        dictation,
        runners,
        ..
    } = config;
    if let Some(http) = &http_config {
        for key in http.deprecated_keys() {
            tracing::warn!(
                kind = "config_key_deprecated",
                key,
                "deprecated config key is ignored"
            );
        }
    }
    let push_settings = notify
        .gotify
        .as_ref()
        .map(canterlot_hub::notify::PushSettings::from);
    canterlot_hub::notify::configure(&pool, push_settings.as_ref())
        .await
        .map_err(str::to_owned)?;
    let presence = canterlot_hub::presence::Presence::default();
    let owner_push = notify
        .gotify
        .as_ref()
        .map(canterlot_hub::notify::Sender::new)
        .transpose()
        .map_err(str::to_owned)?
        .map(|sender| sender.with_presence(presence.clone()));
    match &owner_push {
        Some(_) => tracing::info!(kind = "owner_push_enabled", "gotify pushes enabled"),
        None => tracing::info!(kind = "owner_push_disabled", "no [notify.gotify] table"),
    }
    let blobs: std::sync::Arc<dyn canterlot_hub::attachments::BlobStore> = std::sync::Arc::new(
        canterlot_hub::attachments::LocalBlobStore::open(
            &attachments,
            std::sync::Arc::new(canterlot_hub::attachments::NoTrace),
        )
        .map_err(|error| format!("blob_dir_error: {error}"))?,
    );
    let http_parts = match http_config {
        Some(http_config) => {
            let secret = http_config
                .proxy_secret_file
                .as_deref()
                .map(http::load_proxy_secret)
                .transpose()
                .map_err(str::to_owned)?;
            let listener = tokio::net::TcpListener::bind(http_config.listen)
                .await
                .map_err(|_| "http_bind_error".to_owned())?;
            Some((http_config, secret, listener))
        }
        None => {
            tracing::info!(kind = "http_disabled", "no [http] table, serving gRPC only");
            None
        }
    };
    if !provider_status.enabled {
        // До приёма запросов: читатели не должны увидеть сбой, который никто больше не опрашивает.
        match canterlot_hub::provider_status::retire(&pool, chrono::Utc::now()).await {
            Ok(changed) => tracing::info!(
                kind = "provider_status_disabled",
                changed,
                "provider status polling disabled; stored status cleared"
            ),
            Err(error) => tracing::warn!(
                kind = "provider_status_database",
                error = %canterlot_hub::dberr::describe(&error),
                "stored provider status not cleared"
            ),
        }
    }
    let registry = Registry::new(timers.clone());
    let cloud = canterlot_hub::cloud::service::from_config(&pool, &registry, runners.cloud).await;
    let (dispatcher, dispatch_rx) = dispatcher::Dispatcher::channel();
    let scheduler = canterlot_hub::scheduler::Scheduler::new(
        pool.clone(),
        dispatcher.clone(),
        scheduler_config,
    )
    .with_limits(registry.clone(), limits.clone())
    .with_session_tools(session_tools.clone())
    .with_orchestrator(&orchestrator)
    .with_trash_retention(sessions.trash_retention)
    .with_settle(canterlot_hub::session_tools::settle::SettleConfig {
        nudge_enabled: orchestrator.nudge.enabled,
        ..(&activity).into()
    })
    .with_thresholds(canterlot_hub::activity::Thresholds::from(&activity))
    .with_blobs(blobs.clone())
    .with_cloud(cloud.clone());
    let mut state =
        grpc::HubGrpc::new(pool.clone(), registry.clone(), installation, timers.clone())
            .with_blob_store(blobs.clone())
            .with_media_quota(attachments.max_media_bytes_per_machine)
            .with_session_tools(session_tools.clone())
            .with_memory(memory.clone())
            .with_limits(limits.clone())
            .with_orchestrator(&orchestrator)
            .with_activity(&activity)
            .with_dispatcher(dispatcher.clone())
            .with_cloud(cloud.clone())
            .with_enroll(enroll_settings.clone());
    state.ingest = std::sync::Arc::new(
        ingest::Ingest::new(pool.clone())
            .with_dispatcher(dispatcher.clone())
            .with_registry(registry.clone())
            .with_limits(limits.clone())
            .with_prices(canterlot_hub::pricing::PriceBook::with_overrides(&pricing)),
    );
    let background = Background::start(BackgroundDeps {
        pool: pool.clone(),
        registry: registry.clone(),
        timers,
        dispatch_rx,
        scheduler: scheduler.clone(),
        activity: activity.clone(),
        hibernate,
        presence: presence.clone(),
        machines: machines.clone(),
        session_tools: session_tools.clone(),
        jobs,
        nudge_enabled: orchestrator.nudge.enabled,
        lease_warn_hold: orchestrator.lease_warn_hold(),
        multi_process: http_parts
            .as_ref()
            .is_some_and(|(config, _, _)| config.multi_process),
        attachments: Some((blobs.clone(), attachments.clone())),
        owner_push,
        provider_status: Some(provider_status),
        cloud: cloud.clone(),
        secrets_ready: None,
    });
    let root = tokio_util::sync::CancellationToken::new();
    let trigger = root.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = sigterm.recv() => {},
            _ = tokio::signal::ctrl_c() => {},
        }
        trigger.cancel();
    });
    // Любой из серверов, завершившись, останавливает второй.
    let grpc_root = root.clone();
    let grpc_serving = async {
        let result = grpc::serve(
            state,
            grpc_listen,
            &cert,
            &key,
            grpc_root.clone().cancelled_owned(),
        )
        .await;
        grpc_root.cancel();
        result.map_err(|_| "grpc_server_error".to_owned())
    };
    let http_root = root.clone();
    let http_serving = async move {
        let Some((http_config, proxy_secret, listener)) = http_parts else {
            return Ok(());
        };
        let app = http::AppState::new(http::AppDeps {
            pool,
            registry,
            installation,
            dispatcher,
            scheduler,
            cloud,
            http: http_config,
            proxy_secret,
            limits,
            activity,
            nudge_enabled: orchestrator.nudge.enabled,
            presence,
            routing_stale: orchestrator.routing.stale_limits,
            guards: orchestrator.guards.clone(),
            session_tools: session_tools.clone(),
            memory,
            spawn_queue: orchestrator.spawn_queue.clone(),
            routing_queue_horizon: orchestrator.routing.queue_horizon,
            machines,
            enroll: enroll_settings,
            attachments,
            blobs,
            manifest,
            auth,
            dictation,
            dictation_timers: Default::default(),
            passwords: Default::default(),
            shutdown: http_root.clone(),
        });
        let result = http::serve(app, listener, http_root.clone().cancelled_owned()).await;
        http_root.cancel();
        result.map_err(|_| "http_server_error".to_owned())
    };
    let (grpc_result, http_result) = tokio::join!(grpc_serving, http_serving);
    background
        .stop()
        .await
        .map_err(|failed| failed.0.to_owned())?;
    http_result?;
    grpc_result
}

#[cfg(test)]
mod tests {
    /// Всё, что хаб делает с базой в `serve` до начала обслуживания, должно быть либо
    /// миграциями из `MIGRATOR` (их видит манифест выката), либо перечислено здесь, а
    /// файлы этих вызовов — в `MIGRATION_CODE` у `scripts/deploy/fastpath-classify.py`.
    /// Новый вызов с записью в базу до `serve(` без правки списка и классификатора
    /// ломает этот тест: выкат без vzdump и без репетиции его бы не заметил.
    #[test]
    fn serve_startup_calls_are_pinned() {
        let source = include_str!("main.rs");
        let start = source
            .find("        Command::Serve(_) => {")
            .expect("the Serve arm");
        let rest = &source[start..];
        let end = rest[10..]
            .find("\n        Command::")
            .map_or(rest.len(), |at| at + 10);
        let arm = &rest[..end];
        let mut calls = Vec::new();
        for (at, _) in arm.match_indices('(') {
            let token: String = arm[..at]
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            if token.contains("::") {
                calls.push(token);
            }
        }
        let expected = [
            "Command::Serve",
            "tracing_subscriber::registry",
            "tracing_subscriber::fmt::layer",
            "tracing_subscriber::filter::filter_fn",
            "installation::check_manifest",
            "db::migration_error_code",
            "sqlx::query_scalar",
            "installation::check",
            "presets::seed_all_missing",
        ];
        assert_eq!(
            calls, expected,
            "a startup call changed in the Serve arm: if it writes to the database, add its source \
             files to MIGRATION_CODE in scripts/deploy/fastpath-classify.py, then update this list"
        );
    }
}
