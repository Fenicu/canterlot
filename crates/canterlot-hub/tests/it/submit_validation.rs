//! Проверки приёма ввода до записи: пределы размера, пустое содержимое, ключи
//! идемпотентности и их границы для ввода и управляющих действий.

use canterlot_hub::{
    delivery::{
        control::{CancelOutcome, cancel_input, continue_queue, request_stop},
        submit::{ContentBlock, SubmitError, SubmitInput, submit_input, submit_input_in_tx},
    },
    testkit::{self, Seed},
};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn request(seed: &Seed, text: &str) -> SubmitInput {
    SubmitInput {
        owner_id: seed.owner_id,
        conversation_id: seed.conversation_id,
        idempotency_key: Uuid::now_v7().to_string(),
        content: vec![ContentBlock::Text { text: text.into() }],
        received_at: Utc::now(),
        correlation_id: Uuid::now_v7(),
        origin: None,
    }
}

#[tokio::test]
async fn quote_heavy_text_uses_protobuf_limits() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let text = "\"x".repeat(70_000);
    assert_eq!(text.len(), 140_000);
    assert!(submit_input(&pool, request(&seed, &text)).await.is_ok());
    Ok(())
}
#[tokio::test]
async fn empty_content_and_invalid_keys_are_rejected_before_sql() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let mut conn = pool.acquire().await?;
    let mut tx = sqlx::Connection::begin(&mut *conn).await?;
    let closed = PgPool::connect_lazy_with((*pool.connect_options()).clone());
    closed.close().await;
    for content in [
        vec![],
        vec![
            ContentBlock::Text {
                text: String::new()
            };
            2
        ],
    ] {
        let mut req = request(&seed, "text");
        req.content = content;
        assert!(matches!(
            submit_input(&closed, req.clone()).await,
            Err(SubmitError::InvalidContent { reason: "empty" })
        ));
        assert!(matches!(
            submit_input_in_tx(&mut tx, &req).await,
            Err(SubmitError::InvalidContent { reason: "empty" })
        ));
    }
    tx.rollback().await?;
    Ok(())
}
#[tokio::test]
async fn invalid_idempotency_keys_fail_before_pool_acquisition() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    pool.close().await;
    for key in [String::new(), "k".repeat(257), "é".repeat(129)] {
        let mut req = request(&seed, "text");
        req.idempotency_key = key.clone();
        let error = submit_input(&pool, req).await.unwrap_err();
        assert!(matches!(
            error,
            SubmitError::InvalidRequest {
                reason: "invalid_idempotency_key"
            }
        ));
        for error in [
            cancel_input(&pool, seed.owner_id, Uuid::now_v7(), &key, Uuid::now_v7())
                .await
                .unwrap_err(),
            request_stop(
                &pool,
                seed.owner_id,
                seed.conversation_id,
                &key,
                Uuid::now_v7(),
            )
            .await
            .unwrap_err(),
            continue_queue(
                &pool,
                seed.owner_id,
                seed.conversation_id,
                &key,
                Uuid::now_v7(),
            )
            .await
            .unwrap_err(),
        ] {
            assert!(matches!(
                error,
                canterlot_hub::delivery::control::ControlError::InvalidRequest {
                    reason: "invalid_idempotency_key"
                }
            ));
        }
    }
    Ok(())
}
#[tokio::test]
async fn message_limit_maps_to_submit_resource_limit_without_binding_key() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    use canterlot_hub::delivery::LimitKind;
    use canterlot_proto::limits::MAX_MESSAGE_BYTES;
    let seed = testkit::seed(&pool).await;
    let mut req = request(&seed, "x");
    req.content.resize(
        270_000,
        ContentBlock::Text {
            text: String::new(),
        },
    );
    let key = req.idempotency_key.clone();
    assert!(
        matches!(submit_input(&pool, req).await, Err(SubmitError::ResourceLimit { kind: LimitKind::Message, limit: MAX_MESSAGE_BYTES, actual }) if actual > MAX_MESSAGE_BYTES)
    );
    let count: i64 = sqlx::query_scalar("select count(*) from idempotency_keys where key = $1")
        .bind(&key)
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 0);
    let mut retry = request(&seed, "retry");
    retry.idempotency_key = key;
    submit_input(&pool, retry).await?;
    Ok(())
}
#[tokio::test]
async fn idempotency_key_boundary_and_in_tx_validation() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    use canterlot_hub::delivery::control::ControlError;
    let seed = testkit::seed(&pool).await;
    let mut req = request(&seed, "x");
    req.idempotency_key = "é".repeat(128);
    let input = submit_input(&pool, req.clone()).await?;
    assert_eq!(
        cancel_input(
            &pool,
            seed.owner_id,
            input.input_id,
            &"c".repeat(256),
            Uuid::now_v7()
        )
        .await?,
        CancelOutcome::Cancelled
    );
    request_stop(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        &"s".repeat(256),
        Uuid::now_v7(),
    )
    .await?;
    continue_queue(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        &"r".repeat(256),
        Uuid::now_v7(),
    )
    .await?;
    let mut tx = pool.begin().await?;
    for key in [String::new(), "k".repeat(257), "é".repeat(129)] {
        req.idempotency_key = key.clone();
        assert!(matches!(
            submit_input_in_tx(&mut tx, &req).await,
            Err(SubmitError::InvalidRequest {
                reason: "invalid_idempotency_key"
            })
        ));
        assert!(matches!(
            cancel_input(&pool, seed.owner_id, input.input_id, &key, Uuid::now_v7()).await,
            Err(ControlError::InvalidRequest {
                reason: "invalid_idempotency_key"
            })
        ));
    }
    tx.rollback().await?;
    Ok(())
}
