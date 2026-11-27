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
    #[error("ambiguous: {0:?}")]
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
    #[error("root_required: run install as root (sudo)")]
    RootRequired,
    #[error("user_not_found: {0}")]
    UserNotFound(String),
    #[error("bad_output: {0}")]
    BadOutput(String),
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}
