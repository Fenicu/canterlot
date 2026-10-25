use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use uuid::Uuid;

#[derive(Debug, PartialEq)]
pub enum Begin {
    New,
    Replay(Value),
    Conflict,
}

pub async fn lookup(
    conn: &mut PgConnection,
    owner_id: Uuid,
    key: &str,
    operation: &str,
    request_hash: &[u8],
) -> Result<Begin, sqlx::Error> {
    let stored: Option<(String, Vec<u8>, Value)> = sqlx::query_as(
        "select operation, request_hash, response from idempotency_keys
         where owner_id = $1 and key = $2",
    )
    .bind(owner_id)
    .bind(key)
    .fetch_optional(conn)
    .await?;
    Ok(match stored {
        None => Begin::New,
        Some((stored_operation, stored_hash, response))
            if stored_operation == operation && stored_hash == request_hash =>
        {
            Begin::Replay(response)
        }
        Some(_) => Begin::Conflict,
    })
}

pub async fn record(
    conn: &mut PgConnection,
    owner_id: Uuid,
    key: &str,
    operation: &str,
    request_hash: &[u8],
    response: &Value,
    conversation_id: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "insert into idempotency_keys
             (owner_id, key, operation, request_hash, response, conversation_id)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (owner_id, key) do nothing",
    )
    .bind(owner_id)
    .bind(key)
    .bind(operation)
    .bind(request_hash)
    .bind(response)
    .bind(conversation_id)
    .execute(conn)
    .await?
    .rows_affected()
        == 1)
}

pub fn request_hash<T: Serialize>(operation: &str, body: &T) -> Vec<u8> {
    let mut canonical = serde_json::to_value(body).expect("request must be JSON serializable");
    canonical.sort_all_objects();
    let encoded = serde_json::to_vec(&canonical).expect("JSON value must be serializable");
    let mut hash = Sha256::new();
    hash.update(operation.as_bytes());
    hash.update(encoded);
    hash.finalize().to_vec()
}

pub(crate) fn validate_key(key: &str) -> Result<(), &'static str> {
    if key.is_empty() || key.len() > 256 {
        Err("invalid_idempotency_key")
    } else {
        Ok(())
    }
}
