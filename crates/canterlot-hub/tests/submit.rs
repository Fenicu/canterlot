use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    delivery::submit::{
        ContentBlock, LimitKind, QUEUE_LIMIT, SubmitError, SubmitInput, SubmittedInput,
        submit_input, submit_input_in_tx,
    },
    idempotency::{self, Begin},
    model::InputState,
    testkit::{self, Seed},
};
use canterlot_proto::limits::{MAX_INPUT_META_BYTES, MAX_INPUT_TEXT_BYTES};
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::{sync::Barrier, task::JoinSet, time::timeout};
use uuid::Uuid;

fn request(seed: &Seed, key: &str, text: &str) -> SubmitInput {
    SubmitInput {
        owner_id: seed.owner_id,
        conversation_id: seed.conversation_id,
        idempotency_key: key.into(),
        content: vec![ContentBlock::Text { text: text.into() }],
        received_at: Utc::now(),
        correlation_id: Uuid::now_v7(),
    }
}

async fn assert_counts(pool: &PgPool, expected: (i64, i64, i64, i64)) {
    let counts = sqlx::query_as::<_, (i64, i64, i64, i64)>(
        "select (select count(*) from inputs), (select count(*) from commands),
                (select count(*) from events where kind = 'input_queued'),
                (select count(*) from idempotency_keys)",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(counts, expected);
}

async fn assert_sequence(pool: &PgPool, conversation_id: Uuid, next_seq: i64, event_seq: i64) {
    let sequences: (i64, i64) = sqlx::query_as(
        "select next_input_seq,
                coalesce((select last_seq from scope_counters where scope = $2), 0)
         from queue_state where conversation_id = $1",
    )
    .bind(conversation_id)
    .bind(format!("conversation:{conversation_id}"))
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(sequences, (next_seq, event_seq));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn same_key_same_content_returns_same_input(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let req = request(&seed, "same", "synthetic input");
    let received_at = req.received_at;
    let correlation_id = req.correlation_id;
    let first = submit_input(&pool, req).await.unwrap();
    let second = submit_input(&pool, request(&seed, "same", "synthetic input"))
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(first.state, InputState::Queued);
    assert_eq!(first.input_seq, 1);
    assert_eq!(first.input_id.get_version_num(), 7);
    assert_eq!(first.command_id.get_version_num(), 7);
    assert_eq!(
        serde_json::from_value::<SubmittedInput>(serde_json::to_value(&first).unwrap()).unwrap(),
        first
    );
    assert_counts(&pool, (1, 1, 1, 1)).await;

    let input: (Uuid, Uuid, String, Value, chrono::DateTime<Utc>) = sqlx::query_as(
        "select owner_id, command_id, state, content, received_at from inputs where id = $1",
    )
    .bind(first.input_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (input.0, input.1, input.2.as_str()),
        (seed.owner_id, first.command_id, "queued")
    );
    let content = json!([{"type": "text", "text": "synthetic input"}]);
    assert!(input.3 == content);
    assert_eq!(input.4.timestamp_micros(), received_at.timestamp_micros());
    let command: (Uuid, Uuid, String, String, i32, Value, Option<Uuid>) = sqlx::query_as(
        "select machine_id, conversation_id, kind, delivery_state, dispatch_attempt, payload,
                execution_id from commands where id = $1",
    )
    .bind(first.command_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (command.0, command.1),
        (seed.machine_id, seed.conversation_id)
    );
    assert_eq!(
        (command.2.as_str(), command.3.as_str(), command.4, command.6),
        ("deliver_input", "held", 0, None)
    );
    assert!(
        command.5
            == json!({"input_id": first.input_id, "conversation_id": seed.conversation_id, "content": content})
    );
    let event: (String, i64, Uuid, Value) = sqlx::query_as(
        "select scope, scope_seq, correlation_id, payload from events where kind = 'input_queued'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (event.0, event.1, event.2),
        (
            format!("conversation:{}", seed.conversation_id),
            1,
            correlation_id
        )
    );
    assert!(
        event.3
            == json!({"input_id": first.input_id, "input_seq": 1, "idempotency_key": "same", "state": "queued", "content": content})
    );
    assert_sequence(&pool, seed.conversation_id, 2, 1).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn same_key_different_content_conflicts(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let first = submit_input(&pool, request(&seed, "same", "original"))
        .await
        .unwrap();
    assert!(matches!(
        submit_input(&pool, request(&seed, "same", "changed")).await,
        Err(SubmitError::IdempotencyConflict)
    ));
    let stored: Value = sqlx::query_scalar("select content from inputs where id = $1")
        .bind(first.input_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(stored == json!([{"type": "text", "text": "original"}]));
    assert_counts(&pool, (1, 1, 1, 1)).await;
    assert_sequence(&pool, seed.conversation_id, 2, 1).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_same_key_submits_create_one_input(pool: PgPool) {
    timeout(Duration::from_secs(30), async {
        let seed = testkit::seed(&pool).await;
        let start = Arc::new(Barrier::new(9));
        let mut tasks = JoinSet::new();
        let req = request(&seed, "concurrent", "synthetic input");
        for _ in 0..8 {
            let pool = pool.clone();
            let start = start.clone();
            let req = req.clone();
            tasks.spawn(async move {
                start.wait().await;
                submit_input(&pool, req).await
            });
        }
        start.wait().await;
        let mut results = Vec::new();
        while let Some(result) = tasks.join_next().await {
            results.push(result.unwrap().unwrap());
        }
        assert_eq!(results.len(), 8);
        assert!(results.iter().all(|result| result == &results[0]));
        assert_counts(&pool, (1, 1, 1, 1)).await;
        assert_sequence(&pool, seed.conversation_id, 2, 1).await;
    })
    .await
    .expect("same-key submitters timed out");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn uncommitted_submit_leaves_nothing(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let req = request(&seed, "uncommitted", "synthetic input");
    let rolled_back = {
        let mut tx = pool.begin().await.unwrap();
        let result = submit_input_in_tx(&mut tx, &req).await.unwrap();
        let count: i64 = sqlx::query_scalar("select count(*) from inputs")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(count, 1);
        assert_counts(&pool, (0, 0, 0, 0)).await;
        result
    };
    assert_counts(&pool, (0, 0, 0, 0)).await;
    assert_sequence(&pool, seed.conversation_id, 1, 0).await;
    let committed = submit_input(&pool, req).await.unwrap();
    assert_ne!(rolled_back.input_id, committed.input_id);
    assert_eq!(committed.input_seq, 1);
    assert_counts(&pool, (1, 1, 1, 1)).await;
    assert_sequence(&pool, seed.conversation_id, 2, 1).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn in_tx_requires_an_outer_transaction(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    assert!(matches!(
        submit_input_in_tx(
            &mut conn,
            &request(&seed, "no-transaction", "synthetic input")
        )
        .await,
        Err(SubmitError::Db(sqlx::Error::InvalidArgument(_)))
    ));
    assert_counts(&pool, (0, 0, 0, 0)).await;
    assert_sequence(&pool, seed.conversation_id, 1, 0).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn text_limit_counts_utf8_bytes(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let mut text = "я".repeat(MAX_INPUT_TEXT_BYTES / 2);
    if text.len() < MAX_INPUT_TEXT_BYTES {
        text.push('x');
    }
    assert_eq!(text.len(), 262_144);
    submit_input(&pool, request(&seed, "at-limit", &text))
        .await
        .unwrap();
    let mut oversized = request(&seed, "over-limit", &text);
    oversized
        .content
        .push(ContentBlock::Text { text: "x".into() });
    assert!(matches!(
        submit_input(&pool, oversized).await,
        Err(SubmitError::ResourceLimit {
            kind: LimitKind::InputText,
            limit: 262_144,
            actual: 262_145
        })
    ));
    assert_counts(&pool, (1, 1, 1, 1)).await;
    assert_sequence(&pool, seed.conversation_id, 2, 1).await;
    submit_input(&pool, request(&seed, "over-limit", "retry"))
        .await
        .unwrap();
    assert_counts(&pool, (2, 2, 2, 2)).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn queue_limit_refuses_101st_without_binding_key(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    assert_eq!(QUEUE_LIMIT, 100);
    for index in 0..100 {
        submit_input(
            &pool,
            request(&seed, &format!("input-{index}"), "synthetic input"),
        )
        .await
        .unwrap();
    }
    sqlx::query("update inputs set state = case input_seq when 1 then 'dispatched' else 'on_machine' end where input_seq <= 2")
        .execute(&pool).await.unwrap();
    assert!(matches!(
        submit_input(&pool, request(&seed, "retry-key", "synthetic input")).await,
        Err(SubmitError::QueueFull { limit: 100 })
    ));
    assert_counts(&pool, (100, 100, 100, 100)).await;
    assert_sequence(&pool, seed.conversation_id, 101, 100).await;
    sqlx::query("update inputs set state = 'accepted' where input_seq = 1")
        .execute(&pool)
        .await
        .unwrap();
    let accepted = submit_input(&pool, request(&seed, "retry-key", "synthetic input"))
        .await
        .unwrap();
    assert_eq!(accepted.input_seq, 101);
    assert_counts(&pool, (101, 101, 101, 101)).await;
    assert_sequence(&pool, seed.conversation_id, 102, 101).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn fifo_order_is_server_sequence(pool: PgPool) {
    timeout(Duration::from_secs(30), async {
        let seed = testkit::seed(&pool).await;
        let start = Arc::new(Barrier::new(3));
        let mut tasks = JoinSet::new();
        for device in 0..2 {
            let pool = pool.clone();
            let start = start.clone();
            let seed = Seed { owner_id: seed.owner_id, machine_id: seed.machine_id, conversation_id: seed.conversation_id };
            tasks.spawn(async move {
                start.wait().await;
                let mut sequences = Vec::new();
                for index in 0..50 {
                    let mut req = request(&seed, &format!("device-{device}-{index}"), "synthetic input");
                    req.received_at = Utc::now() - chrono::Duration::days(device * 100 + index);
                    sequences.push(submit_input(&pool, req).await.unwrap().input_seq);
                }
                sequences
            });
        }
        start.wait().await;
        let mut all_sequences = Vec::new();
        let mut devices = 0;
        while let Some(result) = tasks.join_next().await {
            let sequences = result.unwrap();
            assert_eq!(sequences.len(), 50);
            assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
            all_sequences.extend(sequences);
            devices += 1;
        }
        assert_eq!(devices, 2);
        all_sequences.sort_unstable();
        assert_eq!(all_sequences, (1..=100).collect::<Vec<_>>());
        let stored: Vec<i64> = sqlx::query_scalar("select input_seq from inputs order by input_seq")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(stored, all_sequences);
        let events: Vec<(i64, i64)> = sqlx::query_as(
            "select scope_seq, (payload->>'input_seq')::bigint from events where scope = $1 order by scope_seq",
        )
        .bind(format!("conversation:{}", seed.conversation_id))
        .fetch_all(&pool).await.unwrap();
        assert_eq!(events, (1..=100).map(|seq| (seq, seq)).collect::<Vec<_>>());
        assert_counts(&pool, (100, 100, 100, 100)).await;
        assert_sequence(&pool, seed.conversation_id, 101, 100).await;
    })
    .await
    .expect("FIFO submitters timed out");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unknown_or_foreign_conversation_not_found(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let foreign = testkit::seed(&pool).await;
    for conversation_id in [Uuid::now_v7(), foreign.conversation_id] {
        let mut req = request(&seed, "not-found", "synthetic input");
        req.conversation_id = conversation_id;
        assert!(matches!(
            submit_input(&pool, req).await,
            Err(SubmitError::ConversationNotFound)
        ));
    }
    assert_counts(&pool, (0, 0, 0, 0)).await;
    assert_sequence(&pool, seed.conversation_id, 1, 0).await;
    submit_input(&pool, request(&seed, "not-found", "synthetic input"))
        .await
        .unwrap();
    assert_counts(&pool, (1, 1, 1, 1)).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn closed_conversation_refuses(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    sqlx::query("update queue_state set closed = true where conversation_id = $1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        submit_input(&pool, request(&seed, "closed", "synthetic input")).await,
        Err(SubmitError::ConversationClosed)
    ));
    assert_counts(&pool, (0, 0, 0, 0)).await;
    assert_sequence(&pool, seed.conversation_id, 1, 0).await;
    sqlx::query("update queue_state set closed = false where conversation_id = $1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let first = submit_input(&pool, request(&seed, "closed", "synthetic input"))
        .await
        .unwrap();
    sqlx::query("update queue_state set closed = true where conversation_id = $1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        submit_input(&pool, request(&seed, "closed", "synthetic input"))
            .await
            .unwrap(),
        first
    );
    assert_counts(&pool, (1, 1, 1, 1)).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn metadata_limit_refuses_without_binding_key(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let mut req = request(&seed, "metadata", "");
    req.content = (0..3_000)
        .map(|_| ContentBlock::Text {
            text: String::new(),
        })
        .collect();
    let actual = serde_json::to_vec(&req.content).unwrap().len();
    assert!(actual > MAX_INPUT_META_BYTES);
    assert!(matches!(
        submit_input(&pool, req).await,
        Err(SubmitError::ResourceLimit { kind: LimitKind::InputMeta, limit: MAX_INPUT_META_BYTES, actual: bytes }) if bytes == actual
    ));
    assert_counts(&pool, (0, 0, 0, 0)).await;
    assert_sequence(&pool, seed.conversation_id, 1, 0).await;
    submit_input(&pool, request(&seed, "metadata", "retry"))
        .await
        .unwrap();
    assert_counts(&pool, (1, 1, 1, 1)).await;
}

async fn wait_for_submit_lock(pool: &PgPool, query: &str) {
    timeout(Duration::from_secs(10), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "select exists(select 1 from pg_stat_activity where datname = current_database()
                 and wait_event_type = 'Lock' and query like $1)",
            )
            .bind(query)
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("losing submitter never reached the expected lock");
}

async fn wait_for_queue_lock(pool: &PgPool) {
    wait_for_submit_lock(pool, "%for update of q%").await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn queue_lock_replay_after_winner_fills_queue(pool: PgPool) {
    timeout(Duration::from_secs(30), async {
        let seed = testkit::seed(&pool).await;
        for index in 0..99 {
            submit_input(&pool, request(&seed, &format!("filled-{index}"), "input"))
                .await
                .unwrap();
        }
        let req = request(&seed, "last-slot", "original");
        let mut winner_tx = pool.begin().await.unwrap();
        let winner = submit_input_in_tx(&mut winner_tx, &req).await.unwrap();
        assert_eq!(winner.input_seq, 100);
        let loser_pool = pool.clone();
        let loser = tokio::spawn(async move { submit_input(&loser_pool, req).await });
        wait_for_queue_lock(&pool).await;
        winner_tx.commit().await.unwrap();
        assert_eq!(loser.await.unwrap().unwrap(), winner);
        assert_counts(&pool, (100, 100, 100, 100)).await;
        assert_sequence(&pool, seed.conversation_id, 101, 100).await;
    })
    .await
    .expect("full-queue replay timed out");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn queue_lock_replay_after_winner_closes_conversation(pool: PgPool) {
    timeout(Duration::from_secs(30), async {
        let seed = testkit::seed(&pool).await;
        let req = request(&seed, "close-race", "original");
        let mut winner_tx = pool.begin().await.unwrap();
        let winner = submit_input_in_tx(&mut winner_tx, &req).await.unwrap();
        sqlx::query("update queue_state set closed = true where conversation_id = $1")
            .bind(seed.conversation_id)
            .execute(&mut *winner_tx)
            .await
            .unwrap();
        let loser_pool = pool.clone();
        let loser = tokio::spawn(async move { submit_input(&loser_pool, req).await });
        wait_for_queue_lock(&pool).await;
        winner_tx.commit().await.unwrap();
        assert_eq!(loser.await.unwrap().unwrap(), winner);
        assert_counts(&pool, (1, 1, 1, 1)).await;
        assert_sequence(&pool, seed.conversation_id, 2, 1).await;
        let closed: bool =
            sqlx::query_scalar("select closed from queue_state where conversation_id = $1")
                .bind(seed.conversation_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(closed);
    })
    .await
    .expect("closed-conversation replay timed out");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lost_race_rolls_back_and_returns_winner_or_conflict(pool: PgPool) {
    timeout(Duration::from_secs(30), async {
        let seed = testkit::seed(&pool).await;
        for (key, loser_text) in [("replay-race", "original"), ("conflict-race", "changed")] {
            let mut winner_tx = pool.begin().await.unwrap();
            let winner = submit_input_in_tx(&mut winner_tx, &request(&seed, key, "original"))
                .await
                .unwrap();
            let req = request(&seed, key, loser_text);
            let loser_pool = pool.clone();
            let loser = tokio::spawn(async move { submit_input(&loser_pool, req).await });
            wait_for_queue_lock(&pool).await;
            winner_tx.commit().await.unwrap();
            let result = loser.await.unwrap();
            if loser_text == "original" {
                assert_eq!(result.unwrap(), winner);
            } else {
                assert!(matches!(result, Err(SubmitError::IdempotencyConflict)));
            }
        }
        assert_counts(&pool, (2, 2, 2, 2)).await;
        assert_sequence(&pool, seed.conversation_id, 3, 2).await;
    })
    .await
    .expect("lost-race test timed out");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn in_tx_queue_lock_replay_can_commit_without_duplicate_rows(pool: PgPool) {
    timeout(Duration::from_secs(30), async {
        let seed = testkit::seed(&pool).await;
        let mut winner_tx = pool.begin().await.unwrap();
        let winner = submit_input_in_tx(&mut winner_tx, &request(&seed, "race", "original"))
            .await
            .unwrap();
        let req = request(&seed, "race", "original");
        let loser_pool = pool.clone();
        let loser = tokio::spawn(async move {
            let mut tx = loser_pool.begin().await.unwrap();
            let response = submit_input_in_tx(&mut tx, &req).await.unwrap();
            tx.commit().await.unwrap();
            response
        });
        wait_for_queue_lock(&pool).await;
        winner_tx.commit().await.unwrap();
        assert_eq!(loser.await.unwrap(), winner);
        assert_counts(&pool, (1, 1, 1, 1)).await;
        assert_sequence(&pool, seed.conversation_id, 2, 1).await;
    })
    .await
    .expect("in-tx queue replay timed out");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn in_tx_record_race_requires_caller_rollback(pool: PgPool) {
    timeout(Duration::from_secs(30), async {
        let seed = testkit::seed(&pool).await;
        let loser_conversation =
            testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
        let mut winner_tx = pool.begin().await.unwrap();
        submit_input_in_tx(&mut winner_tx, &request(&seed, "race", "original"))
            .await
            .unwrap();
        let mut req = request(&seed, "race", "original");
        req.conversation_id = loser_conversation;
        let loser_pool = pool.clone();
        let loser = tokio::spawn(async move {
            let mut tx = loser_pool.begin().await.unwrap();
            let result = submit_input_in_tx(&mut tx, &req).await;
            assert!(
                matches!(result, Err(SubmitError::KeyRaceLost)),
                "{result:?}"
            );
            let staged_counts: (i64, i64, i64, i64) = sqlx::query_as(
                "select (select count(*) from inputs), (select count(*) from commands),
                        (select count(*) from events where kind = 'input_queued'),
                        (select count(*) from idempotency_keys)",
            )
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            assert_eq!(staged_counts, (2, 2, 2, 1));
            let staged_sequences: (i64, i64) = sqlx::query_as(
                "select next_input_seq, (select last_seq from scope_counters where scope = $2)
                 from queue_state where conversation_id = $1",
            )
            .bind(loser_conversation)
            .bind(format!("conversation:{loser_conversation}"))
            .fetch_one(&mut *tx)
            .await
            .unwrap();
            assert_eq!(staged_sequences, (2, 1));
            tx.rollback().await.unwrap();
        });
        wait_for_submit_lock(&pool, "%insert into idempotency_keys%").await;
        winner_tx.commit().await.unwrap();
        loser.await.unwrap();
        assert_counts(&pool, (1, 1, 1, 1)).await;
        assert_sequence(&pool, seed.conversation_id, 2, 1).await;
        assert_sequence(&pool, loser_conversation, 1, 0).await;
    })
    .await
    .expect("in-tx lost-race test timed out");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn record_race_rolls_back_and_returns_winner_or_conflict(pool: PgPool) {
    timeout(Duration::from_secs(30), async {
        let seed = testkit::seed(&pool).await;
        for (key, loser_text) in [
            ("replay-record-race", "original"),
            ("conflict-record-race", "changed"),
        ] {
            let req = request(&seed, key, "original");
            let winner = submit_input(&pool, req.clone()).await.unwrap();
            sqlx::query("delete from idempotency_keys where owner_id = $1 and key = $2")
                .bind(seed.owner_id)
                .bind(key)
                .execute(&pool)
                .await
                .unwrap();
            let mut winner_tx = pool.begin().await.unwrap();
            let hash = idempotency::request_hash(
                "submit_input",
                &json!({
                    "conversation_id": req.conversation_id, "content": req.content,
                }),
            );
            assert!(
                idempotency::record(
                    &mut winner_tx,
                    seed.owner_id,
                    key,
                    "submit_input",
                    &hash,
                    &serde_json::to_value(&winner).unwrap(),
                    Some(seed.conversation_id),
                )
                .await
                .unwrap()
            );
            let req = request(&seed, key, loser_text);
            let loser_pool = pool.clone();
            let loser = tokio::spawn(async move { submit_input(&loser_pool, req).await });
            wait_for_submit_lock(&pool, "%insert into idempotency_keys%").await;
            winner_tx.commit().await.unwrap();
            let result = loser.await.unwrap();
            if loser_text == "original" {
                assert_eq!(result.unwrap(), winner);
            } else {
                assert!(matches!(result, Err(SubmitError::IdempotencyConflict)));
            }
        }
        assert_counts(&pool, (2, 2, 2, 2)).await;
        assert_sequence(&pool, seed.conversation_id, 3, 2).await;
    })
    .await
    .expect("record-race test timed out");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nul_content_is_rejected_before_sql_without_binding_key(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let req = request(&seed, "nul-content", "private-content-marker\0");
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("select 1 / 0")
        .execute(&mut *tx)
        .await
        .unwrap_err();
    assert!(matches!(
        submit_input_in_tx(&mut tx, &req).await,
        Err(SubmitError::InvalidContent {
            reason: "nul_character"
        })
    ));
    tx.rollback().await.unwrap();
    assert!(matches!(
        submit_input(&pool, req.clone()).await,
        Err(SubmitError::InvalidContent {
            reason: "nul_character"
        })
    ));
    assert_counts(&pool, (0, 0, 0, 0)).await;
    assert_sequence(&pool, seed.conversation_id, 1, 0).await;
    submit_input(&pool, request(&seed, "nul-content", "valid retry"))
        .await
        .unwrap();
    assert_counts(&pool, (1, 1, 1, 1)).await;
    pool.close().await;
    assert!(matches!(
        submit_input(&pool, req).await,
        Err(SubmitError::InvalidContent {
            reason: "nul_character"
        })
    ));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn db_error_formatting_redacts_message_detail_and_context(pool: PgPool) {
    let error = sqlx::query(
        "do $$ begin raise exception using errcode = 'P0001',
         message = 'private-message-marker', detail = 'private-detail-marker'; end $$",
    )
    .execute(&pool)
    .await
    .unwrap_err();
    let db = error.as_database_error().unwrap();
    assert_eq!(db.code().as_deref(), Some("P0001"));
    assert!(db.message().contains("private-message-marker"));
    let db = db.downcast_ref::<sqlx::postgres::PgDatabaseError>();
    assert!(db.detail().unwrap().contains("private-detail-marker"));
    assert!(db.r#where().unwrap().contains("RAISE"));
    let error = SubmitError::from(error);
    for rendered in [format!("{error:?}"), error.to_string()] {
        assert!(rendered.contains("P0001"));
        for private in [
            "private-message-marker",
            "private-detail-marker",
            "RAISE",
            "PL/pgSQL",
        ] {
            assert!(!rendered.contains(private), "{rendered}");
        }
    }
}

#[test]
fn db_error_formatting_without_sqlstate_shows_only_kind() {
    for (error, kind) in [
        (
            sqlx::Error::Protocol("private-protocol-marker".into()),
            "Protocol",
        ),
        (
            sqlx::Error::Decode("private-decode-marker".into()),
            "Decode",
        ),
        (
            sqlx::Error::InvalidArgument("private-argument-marker".into()),
            "InvalidArgument",
        ),
    ] {
        let error = SubmitError::from(error);
        assert_eq!(format!("{error:?}"), format!("Db({kind})"));
        assert_eq!(error.to_string(), format!("database error: {kind}"));
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn same_key_in_another_conversation_conflicts(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    submit_input(&pool, request(&seed, "same", "synthetic input"))
        .await
        .unwrap();
    let conversation_id = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    let mut req = request(&seed, "same", "synthetic input");
    req.conversation_id = conversation_id;
    assert!(matches!(
        submit_input(&pool, req).await,
        Err(SubmitError::IdempotencyConflict)
    ));
    assert_counts(&pool, (1, 1, 1, 1)).await;
    assert_sequence(&pool, conversation_id, 1, 0).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idempotency_lookup_record_are_owner_scoped_and_atomic(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let other = testkit::seed(&pool).await;
    let hash = idempotency::request_hash("operation", &json!({"a": 1}));
    let response = json!({"id": Uuid::now_v7()});
    let mut tx = pool.begin().await.unwrap();
    assert!(matches!(
        idempotency::lookup(&mut tx, seed.owner_id, "key", "operation", &hash)
            .await
            .unwrap(),
        Begin::New
    ));
    assert!(
        idempotency::record(
            &mut tx,
            seed.owner_id,
            "key",
            "operation",
            &hash,
            &response,
            Some(seed.conversation_id)
        )
        .await
        .unwrap()
    );
    assert!(
        !idempotency::record(
            &mut tx,
            seed.owner_id,
            "key",
            "other",
            &hash,
            &json!({}),
            None
        )
        .await
        .unwrap()
    );
    assert!(
        matches!(idempotency::lookup(&mut tx, seed.owner_id, "key", "operation", &hash).await.unwrap(), Begin::Replay(value) if value == response)
    );
    assert!(matches!(
        idempotency::lookup(&mut tx, seed.owner_id, "key", "other", &hash)
            .await
            .unwrap(),
        Begin::Conflict
    ));
    assert!(matches!(
        idempotency::lookup(&mut tx, seed.owner_id, "key", "operation", &[0; 32])
            .await
            .unwrap(),
        Begin::Conflict
    ));
    assert!(matches!(
        idempotency::lookup(&mut tx, other.owner_id, "key", "operation", &hash)
            .await
            .unwrap(),
        Begin::New
    ));
    assert!(
        idempotency::record(
            &mut tx,
            other.owner_id,
            "key",
            "operation",
            &hash,
            &response,
            None
        )
        .await
        .unwrap()
    );
    tx.rollback().await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    assert!(matches!(
        idempotency::lookup(&mut conn, seed.owner_id, "key", "operation", &hash)
            .await
            .unwrap(),
        Begin::New
    ));
    assert_counts(&pool, (0, 0, 0, 0)).await;
}

#[test]
fn request_hash_canonicalizes_objects_and_includes_operation() {
    #[derive(Serialize)]
    struct Unsorted {
        z: Value,
        a: Value,
    }
    let body = Unsorted {
        z: json!([{"z": 4, "a": 3}]),
        a: json!({"z": 2, "a": 1}),
    };
    let canonical = json!({"a": {"a": 1, "z": 2}, "z": [{"a": 3, "z": 4}]});
    let hash = idempotency::request_hash("operation", &body);
    assert_eq!(hash.len(), 32);
    assert_eq!(hash, idempotency::request_hash("operation", &canonical));
    assert_ne!(hash, idempotency::request_hash("other", &canonical));
    assert_ne!(
        hash,
        idempotency::request_hash("operation", &json!({"a": 1}))
    );
    assert_ne!(
        idempotency::request_hash("operation", &[1, 2]),
        idempotency::request_hash("operation", &[2, 1])
    );
}
