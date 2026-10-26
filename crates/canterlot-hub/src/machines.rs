use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub struct NewMachine {
    pub machine_id: Uuid,
    pub token: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RotateTokenError {
    #[error("machine_revoked")]
    MachineRevoked,
    #[error("db_error")]
    Database(#[from] sqlx::Error),
}

pub async fn add(pool: &PgPool, owner_id: Uuid, name: &str) -> Result<NewMachine, sqlx::Error> {
    let token = new_token()?;
    let digest = Sha256::digest(token.as_bytes());
    let machine_id = Uuid::now_v7();
    let mut transaction = pool.begin().await?;
    sqlx::query("insert into machines (id, owner_id, name) values ($1, $2, $3)")
        .bind(machine_id)
        .bind(owner_id)
        .bind(name)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("insert into machine_credentials (machine_id, token_sha256) values ($1, $2)")
        .bind(machine_id)
        .bind(digest.as_slice())
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(NewMachine { machine_id, token })
}

pub async fn verify(pool: &PgPool, token: &str) -> Result<Option<Uuid>, sqlx::Error> {
    let digest = Sha256::digest(token.as_bytes());
    let credentials: Vec<(Uuid, Vec<u8>)> = sqlx::query_as(
        "select machine_id, token_sha256 from machine_credentials where revoked_at is null",
    )
    .fetch_all(pool)
    .await?;
    let mut found = None;
    for (machine_id, stored) in credentials {
        if bool::from(stored.as_slice().ct_eq(digest.as_slice())) {
            found = Some(machine_id);
        }
    }
    Ok(found)
}

fn new_token() -> Result<String, sqlx::Error> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| sqlx::Error::Io(std::io::Error::other("random_source_failed")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub async fn revoke(pool: &PgPool, machine_id: Uuid) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // NO KEY UPDATE сериализует отзыв и ротацию, не блокируя FK нового курсора журнала.
    sqlx::query("select id from machines where id = $1 for no key update")
        .bind(machine_id)
        .fetch_optional(&mut *tx)
        .await?;
    let revoked = sqlx::query(
        "update machine_credentials set revoked_at = clock_timestamp()
         where machine_id = $1 and revoked_at is null",
    )
    .bind(machine_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        != 0;
    if revoked {
        notify_revoke(&mut tx, machine_id).await?;
    }
    tx.commit().await?;
    Ok(revoked)
}

pub async fn rotate_token(
    pool: &PgPool,
    machine_id: Uuid,
    reset_local_state: bool,
    reactivate: bool,
) -> Result<Option<String>, RotateTokenError> {
    let token = new_token()?;
    let digest = Sha256::digest(token.as_bytes());
    let mut tx = pool.begin().await?;
    if sqlx::query("select id from machines where id = $1 for no key update")
        .bind(machine_id)
        .fetch_optional(&mut *tx)
        .await?
        .is_none()
    {
        return Ok(None);
    }
    let revoked = sqlx::query("update machine_credentials set revoked_at = clock_timestamp() where machine_id = $1 and revoked_at is null")
        .bind(machine_id).execute(&mut *tx).await?.rows_affected();
    if revoked == 0 && !reactivate {
        return Err(RotateTokenError::MachineRevoked);
    }
    sqlx::query("insert into machine_credentials (machine_id, token_sha256) values ($1, $2)")
        .bind(machine_id)
        .bind(digest.as_slice())
        .execute(&mut *tx)
        .await?;
    if reset_local_state {
        sqlx::query("update machines set cj_journal_id = null where id = $1")
            .bind(machine_id)
            .execute(&mut *tx)
            .await?;
    }
    notify_revoke(&mut tx, machine_id).await?;
    tx.commit().await?;
    Ok(Some(token))
}

async fn notify_revoke(conn: &mut sqlx::PgConnection, machine_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_notify('canterlot_revoke', $1)")
        .bind(machine_id.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

pub(crate) async fn credential_active(
    conn: &mut sqlx::PgConnection,
    machine_id: Uuid,
    digest: &[u8],
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("select exists(select 1 from machine_credentials where machine_id = $1 and token_sha256 = $2 and revoked_at is null)")
        .bind(machine_id).bind(digest).fetch_one(conn).await
}
