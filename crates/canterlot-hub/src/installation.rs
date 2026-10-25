use std::{
    fs::{self, File},
    io::{self, Write},
    path::Path,
};

use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct Installation {
    pub installation_id: Uuid,
    pub restore_generation: Uuid,
    pub owner_id: Uuid,
}

#[derive(Debug, thiserror::Error)]
pub enum InstallationError {
    #[error("already_initialized")]
    AlreadyInitialized,
    #[error("manifest_missing")]
    ManifestMissing,
    #[error("manifest_mismatch")]
    ManifestMismatch,
    #[error("restore_in_progress")]
    RestoreInProgress,
    #[error("not_initialized")]
    NotInitialized,
    #[error("io_error")]
    Io(#[from] io::Error),
    #[error("db_error")]
    Db(#[from] sqlx::Error),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    installation_id: Uuid,
    restore_generation: Uuid,
    restore_in_progress: bool,
}

pub async fn init(pool: &PgPool, manifest: &Path) -> Result<Installation, InstallationError> {
    refuse_existing_manifest(manifest)?;
    let mut transaction = pool.begin().await?;
    sqlx::query("lock table installation in exclusive mode")
        .execute(&mut *transaction)
        .await?;
    let initialized: bool = sqlx::query_scalar("select exists(select 1 from installation)")
        .fetch_one(&mut *transaction)
        .await?;
    if initialized {
        return Err(InstallationError::AlreadyInitialized);
    }
    refuse_existing_manifest(manifest)?;
    let installation = Installation {
        installation_id: Uuid::now_v7(),
        restore_generation: Uuid::now_v7(),
        owner_id: Uuid::now_v7(),
    };
    sqlx::query("insert into owners (id) values ($1)")
        .bind(installation.owner_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "insert into installation (installation_id, restore_generation, owner_id) values ($1, $2, $3)",
    )
    .bind(installation.installation_id)
    .bind(installation.restore_generation)
    .bind(installation.owner_id)
    .execute(&mut *transaction)
    .await?;
    write_manifest(manifest, &installation)?;
    // При сбое commit манифест сохраняется: повторный init требует административной сверки.
    transaction.commit().await?;
    Ok(installation)
}

pub async fn check(pool: &PgPool, manifest: &Path) -> Result<Installation, InstallationError> {
    let text = fs::read_to_string(manifest).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            InstallationError::ManifestMissing
        } else {
            InstallationError::Io(error)
        }
    })?;
    let manifest: Manifest =
        toml::from_str(&text).map_err(|_| InstallationError::ManifestMismatch)?;
    if manifest.restore_in_progress {
        return Err(InstallationError::RestoreInProgress);
    }
    let installation: Installation =
        sqlx::query_as("select installation_id, restore_generation, owner_id from installation")
            .fetch_optional(pool)
            .await?
            .ok_or(InstallationError::NotInitialized)?;
    if installation.installation_id != manifest.installation_id
        || installation.restore_generation != manifest.restore_generation
    {
        return Err(InstallationError::ManifestMismatch);
    }
    Ok(installation)
}

fn refuse_existing_manifest(path: &Path) -> Result<(), InstallationError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(InstallationError::AlreadyInitialized),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn write_manifest(path: &Path, installation: &Installation) -> Result<(), InstallationError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = File::open(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    write!(
        temporary,
        "installation_id = \"{}\"\nrestore_generation = \"{}\"\nrestore_in_progress = false\n",
        installation.installation_id, installation.restore_generation
    )?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(path).map_err(|error| {
        if error.error.kind() == io::ErrorKind::AlreadyExists {
            InstallationError::AlreadyInitialized
        } else {
            InstallationError::Io(error.error)
        }
    })?;
    directory.sync_all()?;
    Ok(())
}
