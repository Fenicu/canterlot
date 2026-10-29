use futures_util::future::BoxFuture;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

#[derive(PartialEq)]
pub enum Begin {
    New,
    Replay(Value),
    Conflict,
}

impl std::fmt::Debug for Begin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::New => "New",
            Self::Replay(_) => "Replay(<redacted>)",
            Self::Conflict => "Conflict",
        })
    }
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

/// Тот же ключ уже занят другой операцией или другим запросом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdempotencyConflict;

#[derive(Debug, PartialEq)]
pub struct Outcome<T> {
    pub value: T,
    pub replayed: bool,
}

/// Выполняет изменение без собственной обработки ключа в одной транзакции с записью ключа.
/// Повтор возвращает сохранённый ответ без запуска `op`; потерянная гонка за ключ откатывает
/// все эффекты `op` и повторяет ответ победителя.
pub async fn run_in_tx<T, E, F>(
    pool: &PgPool,
    owner_id: Uuid,
    key: &str,
    operation: &str,
    request: &impl Serialize,
    conversation_id: Option<Uuid>,
    op: F,
) -> Result<Outcome<T>, E>
where
    T: Serialize + DeserializeOwned,
    E: From<sqlx::Error> + From<IdempotencyConflict>,
    F: for<'c> FnOnce(&'c mut PgConnection) -> BoxFuture<'c, Result<T, E>>,
{
    let hash = request_hash(operation, request);
    let replay = |stored: Value| -> Result<Outcome<T>, E> {
        serde_json::from_value(stored)
            .map(|value| Outcome {
                value,
                replayed: true,
            })
            .map_err(|error| E::from(sqlx::Error::Decode(Box::new(error))))
    };
    let mut tx = pool.begin().await?;
    let mut lock_hash = Sha256::new();
    lock_hash.update(b"canterlot:idempotency:");
    lock_hash.update(owner_id.as_bytes());
    lock_hash.update(key.as_bytes());
    let lock_id = i64::from_be_bytes(lock_hash.finalize()[..8].try_into().expect("eight bytes"));
    // Первая блокировка транзакции: ключ идемпотентности, затем общий порядок доменных строк.
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(lock_id)
        .execute(&mut *tx)
        .await?;
    match lookup(&mut tx, owner_id, key, operation, &hash).await {
        Ok(Begin::Replay(stored)) => {
            tx.rollback().await?;
            return replay(stored);
        }
        Ok(Begin::Conflict) => {
            tx.rollback().await?;
            return Err(IdempotencyConflict.into());
        }
        Ok(Begin::New) => {}
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error.into());
        }
    }
    let value = match op(&mut tx).await {
        Ok(value) => value,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    let response = match serde_json::to_value(&value) {
        Ok(response) => response,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(E::from(sqlx::Error::Encode(Box::new(error))));
        }
    };
    let inserted = match record(
        &mut tx,
        owner_id,
        key,
        operation,
        &hash,
        &response,
        conversation_id,
    )
    .await
    {
        Ok(inserted) => inserted,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error.into());
        }
    };
    if inserted {
        tx.commit().await?;
        return Ok(Outcome {
            value,
            replayed: false,
        });
    }
    tx.rollback().await?;
    let mut conn = pool.acquire().await?;
    match lookup(&mut conn, owner_id, key, operation, &hash).await? {
        Begin::Replay(stored) => replay(stored),
        Begin::New | Begin::Conflict => Err(IdempotencyConflict.into()),
    }
}
