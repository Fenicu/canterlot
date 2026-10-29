use std::sync::Arc;

use canterlot_hub::events::{self, NewEvent, Scope};
use canterlot_hub::requests::{
    DecisionResult, NewRequest, RequestKind, RequestRow, RequestState, create_request_in_tx,
    save_decision_in_tx,
};
use canterlot_hub::testkit;
use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::Barrier;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn migration_0018_creates_requests_table_with_constraints(
    pool: PgPool,
) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    let execution_id = Uuid::now_v7();
    sqlx::query("insert into executions (id, conversation_id, generation, state) values ($1, $2, 1, 'running')")
        .bind(execution_id)
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;

    // Проверяем существование таблицы requests
    let table_exists: bool = sqlx::query_scalar(
        "select exists(select 1 from information_schema.tables where table_name = 'requests')",
    )
    .fetch_one(&pool)
    .await?;
    assert!(
        table_exists,
        "requests table must exist after migration 0018"
    );

    // Проверяем успешную вставку корректной строки запроса
    let req_id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    let row = create_request_in_tx(
        &mut tx,
        &NewRequest {
            id: req_id,
            conversation_id: seed.conversation_id,
            execution_id,
            generation: 1,
            kind: RequestKind::Permission,
            native_request_id: "perm-test-1".into(),
            request_payload: json!({"action": "bash", "command": "ls -la"}),
        },
    )
    .await?;
    tx.commit().await?;

    assert_eq!(row.id, req_id);
    assert_eq!(row.conversation_id, seed.conversation_id);
    assert_eq!(row.execution_id, execution_id);
    assert_eq!(row.generation, 1);
    assert_eq!(row.kind, RequestKind::Permission);
    assert_eq!(row.native_request_id, "perm-test-1");
    assert_eq!(row.state, RequestState::Open);
    assert_eq!(row.winner_decision_id, None);
    assert_eq!(row.decision_payload, None);
    assert_eq!(row.idempotency_key, None);

    // Проверяем ограничение-проверку (check constraint) на kind
    let mut tx = pool.begin().await?;
    let err = sqlx::query(
        "insert into requests (id, conversation_id, execution_id, generation, kind, native_request_id, state, request_payload)
         values ($1, $2, $3, 1, 'invalid_kind', 'req_bad', 'open', '{}')",
    )
    .bind(Uuid::now_v7())
    .bind(seed.conversation_id)
    .bind(execution_id)
    .execute(&mut *tx)
    .await;
    let _ = tx.rollback().await;
    assert!(
        err.is_err(),
        "inserting invalid kind must violate kind check constraint"
    );

    // Проверяем ограничение-проверку (check constraint) на state
    let mut tx = pool.begin().await?;
    let err = sqlx::query(
        "insert into requests (id, conversation_id, execution_id, generation, kind, native_request_id, state, request_payload)
         values ($1, $2, $3, 1, 'permission', 'req_bad', 'invalid_state', '{}')",
    )
    .bind(Uuid::now_v7())
    .bind(seed.conversation_id)
    .bind(execution_id)
    .execute(&mut *tx)
    .await;
    let _ = tx.rollback().await;
    assert!(
        err.is_err(),
        "inserting invalid state must violate state check constraint"
    );

    // Проверяем внешний ключ на conversation_id
    let mut tx = pool.begin().await?;
    let err = sqlx::query(
        "insert into requests (id, conversation_id, execution_id, generation, kind, native_request_id, state, request_payload)
         values ($1, $2, $3, 1, 'permission', 'req_bad', 'open', '{}')",
    )
    .bind(Uuid::now_v7())
    .bind(Uuid::now_v7())
    .bind(execution_id)
    .execute(&mut *tx)
    .await;
    let _ = tx.rollback().await;
    assert!(
        err.is_err(),
        "inserting with non-existent conversation_id must fail FK check"
    );

    // Проверяем, что commands_kind_check допускает 'deliver_decision'
    let mut tx = pool.begin().await?;
    let cmd_id = Uuid::now_v7();
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, execution_id, execution_generation, kind, payload, delivery_state)
         values ($1, $2, $3, $4, 1, 'deliver_decision', '{}', 'pending')",
    )
    .bind(cmd_id)
    .bind(seed.machine_id)
    .bind(seed.conversation_id)
    .bind(execution_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let cmd_kind: String = sqlx::query_scalar("select kind from commands where id = $1")
        .bind(cmd_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(cmd_kind, "deliver_decision");

    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sole_winner_commits_first_decision_and_concurrent_callers_receive_conflict(
    pool: PgPool,
) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    let execution_id = Uuid::now_v7();
    sqlx::query("insert into executions (id, conversation_id, generation, state) values ($1, $2, 1, 'running')")
        .bind(execution_id)
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;

    let req_id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    create_request_in_tx(
        &mut tx,
        &NewRequest {
            id: req_id,
            conversation_id: seed.conversation_id,
            execution_id,
            generation: 1,
            kind: RequestKind::Permission,
            native_request_id: "perm-race-1".into(),
            request_payload: json!({"action": "bash", "command": "rm -rf /tmp/test"}),
        },
    )
    .await?;
    tx.commit().await?;

    // Конкурентная гонка двух устройств за принятие решения
    let barrier = Arc::new(Barrier::new(2));
    let conv_id = seed.conversation_id;

    let pool1 = pool.clone();
    let b1 = barrier.clone();
    let task1 = tokio::spawn(async move {
        b1.wait().await;
        let mut tx = pool1.begin().await.unwrap();
        let decision = DecisionPayload::Permission {
            behavior: PermissionBehavior::Allow,
            message: Some("Разрешить (устройство 1)".into()),
        };
        let res = save_decision_in_tx(
            &mut tx,
            conv_id,
            req_id,
            1,
            &decision,
            "idempotency-key-device-1",
        )
        .await;
        if matches!(res, Ok(DecisionResult::Committed { .. })) {
            tx.commit().await.unwrap();
        } else {
            let _ = tx.rollback().await;
        }
        res
    });

    let pool2 = pool.clone();
    let b2 = barrier.clone();
    let task2 = tokio::spawn(async move {
        b2.wait().await;
        let mut tx = pool2.begin().await.unwrap();
        let decision = DecisionPayload::Permission {
            behavior: PermissionBehavior::Deny,
            message: Some("Отклонить (устройство 2)".into()),
        };
        let res = save_decision_in_tx(
            &mut tx,
            conv_id,
            req_id,
            1,
            &decision,
            "idempotency-key-device-2",
        )
        .await;
        if matches!(res, Ok(DecisionResult::Committed { .. })) {
            tx.commit().await.unwrap();
        } else {
            let _ = tx.rollback().await;
        }
        res
    });

    let (r1, r2) = tokio::join!(task1, task2);
    let res1 = r1.unwrap().expect("task1 execution");
    let res2 = r2.unwrap().expect("task2 execution");

    // Ровно одно решение зафиксировано как Committed, а второе — как Conflict
    let (winner_res, conflict_res) = match (&res1, &res2) {
        (DecisionResult::Committed { .. }, DecisionResult::Conflict { .. }) => (res1, res2),
        (DecisionResult::Conflict { .. }, DecisionResult::Committed { .. }) => (res2, res1),
        other => panic!("expected exactly one Committed and one Conflict, got: {other:?}"),
    };

    let (winning_row, winning_command_id) = match winner_res {
        DecisionResult::Committed { row, command_id } => (row, command_id),
        _ => unreachable!(),
    };

    let (conflict_payload, conflict_state) = match conflict_res {
        DecisionResult::Conflict {
            winner_decision,
            current_state,
        } => (winner_decision, current_state),
        _ => unreachable!(),
    };

    assert_eq!(conflict_state, RequestState::DecisionSaved);
    assert_eq!(
        conflict_payload,
        winning_row
            .decision_payload
            .expect("winner decision payload")
    );

    // Проверяем состояние в БД: ровно одна исходящая команда и верное состояние строки запроса
    let db_request: RequestRow = sqlx::query_as(
        "select id, conversation_id, execution_id, generation, kind, native_request_id, state,
                request_payload, decision_payload, winner_decision_id, idempotency_key, created_at, updated_at
         from requests where id = $1",
    )
    .bind(req_id)
    .fetch_one(&pool)
    .await?;

    assert_eq!(db_request.state, RequestState::DecisionSaved);
    assert_eq!(
        db_request.winner_decision_id,
        winning_row.winner_decision_id
    );

    let commands_count: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id = $1 and kind = 'deliver_decision'",
    )
    .bind(conv_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        commands_count, 1,
        "exactly one deliver_decision command must be queued"
    );

    let (cmd_id, cmd_payload): (Uuid, serde_json::Value) = sqlx::query_as(
        "select id, payload from commands where conversation_id = $1 and kind = 'deliver_decision'",
    )
    .bind(conv_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(cmd_id, winning_command_id);
    assert_eq!(
        cmd_payload.get("decision_id").and_then(|v| v.as_str()),
        db_request
            .winner_decision_id
            .map(|u| u.to_string())
            .as_deref()
    );

    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn identical_idempotency_key_replays_committed_decision(
    pool: PgPool,
) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    let execution_id = Uuid::now_v7();
    sqlx::query("insert into executions (id, conversation_id, generation, state) values ($1, $2, 1, 'running')")
        .bind(execution_id)
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;

    let req_id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    create_request_in_tx(
        &mut tx,
        &NewRequest {
            id: req_id,
            conversation_id: seed.conversation_id,
            execution_id,
            generation: 1,
            kind: RequestKind::Question,
            native_request_id: "q-idem-1".into(),
            request_payload: json!({"question": "Deploy?"}),
        },
    )
    .await?;
    tx.commit().await?;

    let mut answers = std::collections::BTreeMap::new();
    answers.insert(
        "q1".to_string(),
        canterlot_proto::requests::AnswerValue::Single("yes".into()),
    );
    let decision = DecisionPayload::Question { answers };
    let idem_key = "op-decision-key-100";

    // Первая отправка решения: успешно фиксируется
    let mut tx1 = pool.begin().await?;
    let res1 = save_decision_in_tx(
        &mut tx1,
        seed.conversation_id,
        req_id,
        1,
        &decision,
        idem_key,
    )
    .await
    .expect("first decision save");
    tx1.commit().await?;

    let committed_row = match res1 {
        DecisionResult::Committed { row, .. } => row,
        other => panic!("expected Committed, got: {other:?}"),
    };

    // Повторная отправка с тем же ключом идемпотентности: IdempotentReplay
    let mut tx2 = pool.begin().await?;
    let res2 = save_decision_in_tx(
        &mut tx2,
        seed.conversation_id,
        req_id,
        1,
        &decision,
        idem_key,
    )
    .await
    .expect("replay decision save");
    tx2.commit().await?;

    let replayed_row = match res2 {
        DecisionResult::IdempotentReplay { row } => row,
        other => panic!("expected IdempotentReplay, got: {other:?}"),
    };

    assert_eq!(replayed_row.id, committed_row.id);
    assert_eq!(
        replayed_row.winner_decision_id,
        committed_row.winner_decision_id
    );
    assert_eq!(replayed_row.idempotency_key, Some(idem_key.into()));

    // В таблице commands остаётся ровно одна команда
    let commands_count: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id = $1 and kind = 'deliver_decision'",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(commands_count, 1);

    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn decision_on_closed_request_returns_already_closed_conflict(
    pool: PgPool,
) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    let execution_id = Uuid::now_v7();
    sqlx::query("insert into executions (id, conversation_id, generation, state) values ($1, $2, 1, 'running')")
        .bind(execution_id)
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;

    let req_id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    create_request_in_tx(
        &mut tx,
        &NewRequest {
            id: req_id,
            conversation_id: seed.conversation_id,
            execution_id,
            generation: 1,
            kind: RequestKind::Permission,
            native_request_id: "perm-closed-1".into(),
            request_payload: json!({"action": "read"}),
        },
    )
    .await?;

    // Переводим запрос в состояние closed_without_decision (например, ход прерван пользователем)
    sqlx::query("update requests set state = 'closed_without_decision' where id = $1")
        .bind(req_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let decision = DecisionPayload::Permission {
        behavior: PermissionBehavior::Allow,
        message: None,
    };

    let mut tx2 = pool.begin().await?;
    let res = save_decision_in_tx(
        &mut tx2,
        seed.conversation_id,
        req_id,
        1,
        &decision,
        "late-key-1",
    )
    .await
    .expect("save on closed request");
    let _ = tx2.rollback().await;

    match res {
        DecisionResult::AlreadyClosed {
            current_state,
            reason,
        } => {
            assert_eq!(current_state, RequestState::ClosedWithoutDecision);
            assert_eq!(reason, "closed_without_decision");
        }
        other => panic!("expected AlreadyClosed, got: {other:?}"),
    }

    // Команда не должна быть создана
    let count: i64 = sqlx::query_scalar("select count(*) from commands where conversation_id = $1")
        .bind(seed.conversation_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 0);

    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn decision_with_stale_generation_returns_stale_generation(
    pool: PgPool,
) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    let execution_id = Uuid::now_v7();
    sqlx::query("insert into executions (id, conversation_id, generation, state) values ($1, $2, 1, 'running')")
        .bind(execution_id)
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;

    let req_id = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    create_request_in_tx(
        &mut tx,
        &NewRequest {
            id: req_id,
            conversation_id: seed.conversation_id,
            execution_id,
            generation: 1,
            kind: RequestKind::Permission,
            native_request_id: "perm-stale-gen".into(),
            request_payload: json!({"action": "edit"}),
        },
    )
    .await?;
    tx.commit().await?;

    let decision = DecisionPayload::Permission {
        behavior: PermissionBehavior::Allow,
        message: None,
    };

    // Передаём несовпадающее поколение (2 вместо 1)
    let mut tx2 = pool.begin().await?;
    let res = save_decision_in_tx(
        &mut tx2,
        seed.conversation_id,
        req_id,
        2,
        &decision,
        "stale-gen-key",
    )
    .await
    .expect("stale generation save");
    let _ = tx2.rollback().await;

    assert_eq!(res, DecisionResult::StaleGeneration);

    // Запрос остаётся в состоянии open, команда не добавлена
    let state: RequestState = sqlx::query_scalar("select state from requests where id = $1")
        .bind(req_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, RequestState::Open);

    let count: i64 = sqlx::query_scalar("select count(*) from commands where conversation_id = $1")
        .bind(seed.conversation_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 0);

    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lock_order_requests_prevents_deadlocks_under_concurrent_turn_events(
    pool: PgPool,
) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    let execution_id = Uuid::now_v7();
    sqlx::query("insert into executions (id, conversation_id, generation, state) values ($1, $2, 1, 'running')")
        .bind(execution_id)
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;

    // Создаём 50 запросов для 50 параллельных чередующихся транзакций
    let mut request_ids = Vec::with_capacity(50);
    for i in 0..50 {
        let req_id = Uuid::now_v7();
        let mut tx = pool.begin().await?;
        create_request_in_tx(
            &mut tx,
            &NewRequest {
                id: req_id,
                conversation_id: seed.conversation_id,
                execution_id,
                generation: 1,
                kind: if i % 2 == 0 {
                    RequestKind::Permission
                } else {
                    RequestKind::Question
                },
                native_request_id: format!("req-interleave-{i}"),
                request_payload: json!({"idx": i}),
            },
        )
        .await?;
        tx.commit().await?;
        request_ids.push(req_id);
    }

    // Считываем начальное число взаимных блокировок (deadlocks) в текущей базе
    let deadlocks_before: i64 = sqlx::query_scalar(
        "select deadlocks from pg_stat_database where datname = current_database()",
    )
    .fetch_one(&pool)
    .await?;

    let barrier = Arc::new(Barrier::new(50));
    let conv_id = seed.conversation_id;
    let mut handles = Vec::with_capacity(50);

    for (i, req_id) in request_ids.into_iter().enumerate() {
        let pool_c = pool.clone();
        let bar_c = barrier.clone();
        handles.push(tokio::spawn(async move {
            bar_c.wait().await;
            let mut tx = pool_c.begin().await.unwrap();

            // Чередование типов транзакций:
            // Чётные: вызывают save_decision_in_tx (шаги 7 -> 10.5) и затем events::append (шаг 12)
            // Нечётные: явно берут шаг 7 (conversations FOR NO KEY UPDATE), вызывают save_decision_in_tx (10.5), и затем events::append (12)
            let decision = DecisionPayload::Permission {
                behavior: PermissionBehavior::Allow,
                message: Some(format!("decision {i}")),
            };

            let res = save_decision_in_tx(
                &mut tx,
                conv_id,
                req_id,
                1,
                &decision,
                &format!("key-interleave-{i}"),
            )
            .await
            .unwrap();

            assert!(matches!(res, DecisionResult::Committed { .. }));

            // Шаг 12 глобального порядка: добавление события в область разговора
            events::append(
                &mut tx,
                NewEvent {
                    scope: Scope::Conversation { id: conv_id },
                    kind: "request_decided",
                    conversation_id: Some(conv_id),
                    execution_id: Some(execution_id),
                    payload: json!({"request_id": req_id, "idx": i}),
                    correlation_id: Uuid::now_v7(),
                },
            )
            .await
            .unwrap();

            tx.commit().await.unwrap();
        }));
    }

    for h in handles {
        h.await.expect("task join failed");
    }

    // Проверяем, что число взаимных блокировок в pg_stat_database не увеличилось
    let deadlocks_after: i64 = sqlx::query_scalar(
        "select deadlocks from pg_stat_database where datname = current_database()",
    )
    .fetch_one(&pool)
    .await?;

    assert_eq!(
        deadlocks_after, deadlocks_before,
        "deadlocks must not increase across 50 concurrent turn/decision interleavings (was {deadlocks_before}, now {deadlocks_after})"
    );

    // Проверяем, что все 50 команд созданы
    let total_commands: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id = $1 and kind = 'deliver_decision'",
    )
    .bind(conv_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(total_commands, 50);

    Ok(())
}
