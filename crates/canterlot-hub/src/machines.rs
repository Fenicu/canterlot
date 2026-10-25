use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;
use uuid::Uuid;

pub struct NewMachine {
    pub machine_id: Uuid,
    pub token: String,
}

pub async fn add(pool: &PgPool, owner_id: Uuid, name: &str) -> Result<NewMachine, sqlx::Error> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| sqlx::Error::Io(std::io::Error::other("random_source_failed")))?;
    let token = URL_SAFE_NO_PAD.encode(bytes);
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

pub async fn revoke(pool: &PgPool, machine_id: Uuid) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "update machine_credentials set revoked_at = clock_timestamp()
         where machine_id = $1 and revoked_at is null",
    )
    .bind(machine_id)
    .execute(pool)
    .await?
    .rows_affected()
        != 0)
}
