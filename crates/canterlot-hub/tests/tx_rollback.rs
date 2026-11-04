use canterlot_hub::{
    conversations, delivery::ContentBlock, executions, idempotency, machines,
    model::ExecutionState, testkit,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const ITERATIONS: usize = 50;

#[derive(Debug)]
struct OpError(#[allow(dead_code)] sqlx::Error);
impl From<sqlx::Error> for OpError {
    fn from(error: sqlx::Error) -> Self {
        Self(error)
    }
}
impl From<idempotency::IdempotencyConflict> for OpError {
    fn from(_: idempotency::IdempotencyConflict) -> Self {
        Self(sqlx::Error::RowNotFound)
    }
}
impl canterlot_hub::db::TransientError for OpError {
    fn is_transient(&self) -> bool {
        canterlot_hub::db::is_transient(&self.0)
    }
}

/// Значение, которое нельзя закодировать в JSON: ключи отображения не строки.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Unencodable {
    map: HashMap<Vec<u8>, String>,
}

async fn lock_queue(conn: &mut PgConnection, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
        .bind(id)
        .fetch_optional(conn)
        .await?;
    Ok(())
}

/// Зонд по заранее взятому соединению: None означает пропуск SKIP LOCKED.
async fn probe_queue(conn: &mut PgConnection, id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar(
        "select conversation_id from queue_state where conversation_id=$1 for update skip locked",
    )
    .bind(id)
    .fetch_optional(conn)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn in_tx_rolls_back_synchronously_on_op_error(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let mut probe = pool.acquire().await?;
    for _ in 0..ITERATIONS {
        let result: Result<(), OpError> = canterlot_hub::db::in_tx(&pool, |conn| {
            Box::pin(async move {
                lock_queue(conn, seed.conversation_id).await?;
                Err(OpError(sqlx::Error::RowNotFound))
            })
        })
        .await;
        assert!(result.is_err());
        assert!(
            probe_queue(&mut probe, seed.conversation_id)
                .await
                .is_some(),
            "queue row skipped: in_tx did not roll back synchronously"
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_in_tx_op_error_rolls_back_synchronously(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let mut probe = pool.acquire().await?;
    for i in 0..ITERATIONS {
        let result: Result<idempotency::Outcome<()>, OpError> = idempotency::run_in_tx(
            &pool,
            seed.owner_id,
            &format!("tx-op-error-{i}"),
            "probe",
            &serde_json::json!({}),
            Some(seed.conversation_id),
            |conn| {
                Box::pin(async move {
                    lock_queue(conn, seed.conversation_id).await?;
                    Err(OpError(sqlx::Error::RowNotFound))
                })
            },
        )
        .await;
        assert!(result.is_err());
        assert!(
            probe_queue(&mut probe, seed.conversation_id)
                .await
                .is_some(),
            "queue row skipped: run_in_tx op error did not roll back synchronously"
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_in_tx_serde_failure_rolls_back_synchronously(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let value = Unencodable {
        map: HashMap::from([(vec![1u8], "x".to_string())]),
    };
    assert!(serde_json::to_value(&value).is_err());
    let mut probe = pool.acquire().await?;
    for i in 0..ITERATIONS {
        let result: Result<idempotency::Outcome<Unencodable>, OpError> = idempotency::run_in_tx(
            &pool,
            seed.owner_id,
            &format!("tx-serde-fail-{i}"),
            "probe",
            &serde_json::json!({}),
            Some(seed.conversation_id),
            |conn| {
                let value = value.clone();
                Box::pin(async move {
                    lock_queue(conn, seed.conversation_id).await?;
                    Ok(value)
                })
            },
        )
        .await;
        assert!(result.is_err());
        assert!(
            probe_queue(&mut probe, seed.conversation_id)
                .await
                .is_some(),
            "queue row skipped: run_in_tx serde failure did not roll back synchronously"
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_in_tx_record_failure_rolls_back_synchronously(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let mut probe = pool.acquire().await?;
    for i in 0..ITERATIONS {
        let result: Result<idempotency::Outcome<bool>, OpError> = idempotency::run_in_tx(
            &pool,
            seed.owner_id,
            &format!("tx-record-fail-{i}"),
            "probe",
            &serde_json::json!({}),
            Some(Uuid::now_v7()),
            |conn| {
                Box::pin(async move {
                    lock_queue(conn, seed.conversation_id).await?;
                    Ok(true)
                })
            },
        )
        .await;
        assert!(result.is_err());
        assert!(
            probe_queue(&mut probe, seed.conversation_id)
                .await
                .is_some(),
            "queue row skipped: run_in_tx record failure did not roll back synchronously"
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn set_state_invalid_transition_rolls_back_queue_lock(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let started =
        executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await?;
    let mut probe = pool.acquire().await?;
    for _ in 0..ITERATIONS {
        let result = executions::set_state(
            &pool,
            started.execution_id,
            ExecutionState::Running,
            None,
            Uuid::now_v7(),
        )
        .await;
        assert!(matches!(
            result,
            Err(executions::ExecutionError::InvalidTransition { .. })
        ));
        assert!(
            probe_queue(&mut probe, seed.conversation_id)
                .await
                .is_some(),
            "queue row skipped: set_state did not roll back synchronously"
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn rotate_revoked_machine_rolls_back_machine_lock(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let machine = machines::add(&pool, seed.owner_id, "revoked").await?;
    assert!(machines::revoke(&pool, machine.machine_id).await?);
    let mut probe = pool.acquire().await?;
    for _ in 0..ITERATIONS {
        let result = machines::rotate_token(&pool, machine.machine_id, false, false).await;
        assert!(matches!(
            result,
            Err(machines::RotateTokenError::MachineRevoked)
        ));
        let row: Option<Uuid> =
            sqlx::query_scalar("select id from machines where id=$1 for update skip locked")
                .bind(machine.machine_id)
                .fetch_optional(&mut *probe)
                .await?;
        assert!(
            row.is_some(),
            "machine row skipped: rotate_token did not roll back synchronously"
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn submit_unready_attachment_rolls_back_share_lock(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let attachment = Uuid::now_v7();
    sqlx::query(
        "insert into attachments (id, owner_id, conversation_id, name, size_bytes, sha256, state)
         values ($1, $2, $3, 'unready.bin', 10, $4, 'uploading')",
    )
    .bind(attachment)
    .bind(seed.owner_id)
    .bind(seed.conversation_id)
    .bind("a".repeat(64))
    .execute(&pool)
    .await?;
    let mut probe = pool.acquire().await?;
    for i in 0..ITERATIONS {
        let result = conversations::submit_message(
            &pool,
            conversations::SubmitMessage {
                owner_id: seed.owner_id,
                conversation_id: seed.conversation_id,
                key: format!("tx-probe-key-{i}"),
                content: vec![ContentBlock::Text {
                    text: "test".into(),
                }],
                attachment_ids: vec![attachment],
                retry_of: None,
                received_at: chrono::Utc::now(),
            },
            Uuid::now_v7(),
        )
        .await;
        assert!(result.is_err());
        let row: Option<Uuid> =
            sqlx::query_scalar("select id from attachments where id=$1 for update skip locked")
                .bind(attachment)
                .fetch_optional(&mut *probe)
                .await?;
        assert!(
            row.is_some(),
            "attachment row skipped: submit_message did not roll back synchronously"
        );
    }
    Ok(())
}

/// Взаимная блокировка двух транзакций: helper откатывает жертву и повторяет её,
/// поэтому оба изменения в итоге фиксируются.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_in_tx_retries_a_deadlock_until_both_transactions_commit(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let second = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    let first_id = seed.conversation_id;
    let owner = seed.owner_id;
    let request = serde_json::json!({});

    let left = idempotency::run_in_tx::<(), OpError, _>(
        &pool,
        owner,
        "deadlock-left",
        "probe",
        &request,
        None,
        move |conn| {
            Box::pin(async move {
                lock_queue(conn, first_id).await?;
                tokio::time::sleep(Duration::from_millis(100)).await;
                lock_queue(conn, second).await?;
                Ok(())
            })
        },
    );
    let right = idempotency::run_in_tx::<(), OpError, _>(
        &pool,
        owner,
        "deadlock-right",
        "probe",
        &request,
        None,
        move |conn| {
            Box::pin(async move {
                lock_queue(conn, second).await?;
                tokio::time::sleep(Duration::from_millis(100)).await;
                lock_queue(conn, first_id).await?;
                Ok(())
            })
        },
    );
    let (left, right) =
        tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(left, right) })
            .await
            .expect("the helper must resolve the deadlock, not wait forever");
    left.unwrap();
    right.unwrap();
    Ok(())
}
