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
    #[error("command_failed: {program} exited with {status}")]
    CommandFailed { program: String, status: i32 },
    #[error("{0}")]
    Other(String),
}
