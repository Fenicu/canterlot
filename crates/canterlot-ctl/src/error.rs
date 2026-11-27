use std::io;

use thiserror::Error;

use crate::ops::OpRecord;

/// Ошибки утилиты. Первое слово в тексте — машинный код в `snake_case`.
#[derive(Debug, Error)]
pub enum CtlError {
    #[error("io_error: {0}")]
    Io(#[from] io::Error),
    #[error("http_error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("url_error: {0}")]
    Url(#[from] url::ParseError),
    #[error("json_error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("toml_error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("toml_write_error: {0}")]
    TomlWrite(#[from] toml::ser::Error),
    #[error("bad_version: {0}")]
    BadVersion(String),
    #[error("checksum_missing: {0}")]
    ChecksumMissing(String),
    #[error("checksum_mismatch: {0}")]
    ChecksumMismatch(String),
    #[error("unsafe_archive_entry: {0}")]
    UnsafeArchive(String),
    #[error("asset_missing: {0}")]
    AssetMissing(String),
    #[error("asset_not_found: {0}")]
    AssetNotFound(String),
    #[error("not_implemented: {0}")]
    NotImplemented(&'static str),
    #[error("busy: {0:?}")]
    Busy(Option<OpRecord>),
    #[error("unfinished: {0:?}")]
    Unfinished(OpRecord),
    #[error("ambiguous: {}", ambiguous_text(.0))]
    Ambiguous(OpRecord),
    #[error("op_not_found: {0}")]
    OpNotFound(String),
    // stderr не включается: в нём может оказаться секрет из конфигурации.
    #[error("command_failed: {program} exited with {status}")]
    CommandFailed { program: String, status: i32 },
    #[error("worker_user_required: run through sudo or pass --worker-user <name> (or --hub-only)")]
    WorkerUserRequired,
    #[error(
        "native_install_found: a native canterlot hub is installed here; take it over with `canterlot adopt`"
    )]
    NativeInstallFound,
    #[error("identity_mismatch: {0}")]
    IdentityMismatch(String),
    #[error("not_ready: {0}")]
    NotReady(String),
    #[error(
        "identity_unknown: data of an earlier installation is present but no operation record holds its database identity; restore the record or move the data away"
    )]
    IdentityUnknown,
    #[error(
        "restore_in_progress: the installation manifest marks an unfinished restore (docs/deploy/pilot.md §8.2)"
    )]
    RestoreInProgress,
    #[error("database_not_empty: the canterlot database already has tables; refusing to run init")]
    DatabaseNotEmpty,
    #[error("manifest_exists: an installation manifest exists; refusing to run init")]
    ManifestExists,
    #[error("missing_answer: {0} is required with --yes or without a terminal")]
    MissingAnswer(&'static str),
    #[error("bad_answer: {0}")]
    BadAnswer(String),
    #[error(
        "docker_missing: install Docker Engine with the compose plugin (https://docs.docker.com/engine/install/) and run again"
    )]
    DockerMissing,
    #[error("disk_low: {0} bytes free, at least 5 GB needed")]
    DiskLow(u64),
    #[error(
        "glibc_too_old: {0}; the native worker needs glibc 2.41 or newer (build from source, docs/development.md)"
    )]
    GlibcTooOld(String),
    #[error("root_required: run `sudo canterlot {0}` to manage it")]
    RootRequired(String),
    #[error("user_not_found: {0}")]
    UserNotFound(String),
    #[error("bad_output: {0}")]
    BadOutput(String),
    #[error("migrations_incompatible: {0}")]
    MigrationsIncompatible(String),
    #[error("insufficient_space: {0}")]
    InsufficientSpace(String),
    #[error("hub_not_installed: no hub stack is recorded in /etc/canterlot/install.toml")]
    HubNotInstalled,
    #[error(
        "needs_owner: the last operation {0} stopped for manual steps (see `canterlot status`); after fixing, run the command again with --force"
    )]
    NeedsOwnerPending(String),
    #[error("admit_failed: {0}")]
    AdmitFailed(String),
    #[error("live_sessions: {0} live sessions; stop them first or pass --force")]
    LiveSessions(u64),
    #[error("confirmation_required: {0}")]
    ConfirmationRequired(String),
    #[error("bad_name: {0}")]
    BadName(String),
    #[error("cancelled")]
    Cancelled,
    #[error(
        "already_joined: this account already runs a canterlot worker; pass --replace to join again with a new code"
    )]
    AlreadyJoined,
    #[error("bad_hub_address: {0} (expected host:port)")]
    BadHubAddress(String),
    #[error("hub_unreachable: {0}")]
    HubUnreachable(String),
    #[error(
        "pin_mismatch: the hub presented CA {got}, the command expects {want}; the code was not sent"
    )]
    PinMismatch { want: String, got: String },
    #[error(
        "san_mismatch: the hub certificate does not name {host}; on the hub host run `canterlot tls add-name {host}` or join with an address the certificate names; the code was not sent"
    )]
    SanMismatch { host: String },
    #[error("tls_untrusted: {0}; the code was not sent")]
    TlsUntrusted(String),
    #[error("join_refused: {0}")]
    JoinRefused(String),
    #[error(
        "hub_older_than_target: the hub runs {hub}, the worker update targets {target}; update the hub first or pass --force"
    )]
    HubOlderThanTarget { hub: String, target: String },
    #[error(
        "hub_older_than_target: the hub runs {hub}, the worker update targets {target}; run `sudo canterlot update` on this hub host first or pass --force"
    )]
    HubOlderThanTargetOnHubHost { hub: String, target: String },
    #[error(
        "hub_version_unknown: the worker is not connected to a hub that reports its version; pass --force to update anyway"
    )]
    HubVersionUnknown,
    #[error("downgrade_refused: installed {current}, requested {target}")]
    DowngradeRefused { current: String, target: String },
    #[error("supervisor_binary_missing: {0}")]
    SupervisorBinaryMissing(String),
    #[error(
        "sessions_alive: {0} live session(s) run on this worker; stop them first or pass --force"
    )]
    SessionsAlive(usize),
    #[error("confirmation_mismatch: the typed name does not match this host ({0})")]
    ConfirmationMismatch(String),
    #[error("{0}")]
    Other(String),
}

/// Неоднозначная запись: какая операция, на какой фазе и что проверить руками.
fn ambiguous_text(rec: &OpRecord) -> String {
    let mut text = format!(
        "operation {} ({:?}) stopped at phase {} in a state that cannot be continued automatically",
        rec.id, rec.kind, rec.phase
    );
    for step in &rec.details.manual {
        text.push_str("; ");
        text.push_str(step);
    }
    text
}
