//! Правка ожидающего ввода: сравнение-и-замена по ревизии, гонка с выдачей,
//! граница допустимости та же, что у «Отправить сейчас».

use std::sync::Arc;

use canterlot_hub::{
    delivery::{
        control::{ControlError, EditInputOutcome, request_edit_input},
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
        receipts::{InputReceipt, apply_input_receipt},
        submit::{ContentBlock, SubmitInput, submit_input},
    },
    executions::{self},
    model::{ExecutionState, InputState, RunSlot},
    testkit::{self, Seed},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::Barrier;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn input(pool: &PgPool, seed: &Seed, text: &str) -> (Uuid, Uuid) {
    let submitted = submit_input(
        pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text { text: text.into() }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    (submitted.input_id, submitted.command_id)
}

async fn running(pool: &PgPool, seed: &Seed) {
    let execution = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
}

async fn edit(
    pool: &PgPool,
    seed: &Seed,
    input_id: Uuid,
    text: &str,
    revision: i64,
    key: &str,
) -> Result<EditInputOutcome, ControlError> {
    request_edit_input(
        pool,
        seed.owner_id,
        seed.conversation_id,
        input_id,
        text.into(),
        Vec::new(),
        revision,
        key,
        Uuid::now_v7(),
    )
    .await
}

async fn state(pool: &PgPool, input_id: Uuid) -> InputState {
    sqlx::query_scalar::<_, String>("select state from inputs where id = $1")
        .bind(input_id)
        .fetch_one(pool)
        .await
        .unwrap()
        .parse()
        .unwrap()
}

async fn revision(pool: &PgPool, input_id: Uuid) -> i64 {
    sqlx::query_scalar("select revision from inputs where id = $1")
        .bind(input_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn deliver_text(pool: &PgPool, command_id: Uuid) -> String {
    sqlx::query_scalar::<_, Value>("select payload->'content' from commands where id = $1")
        .bind(command_id)
        .fetch_one(pool)
        .await
        .and_then(|content| {
            content
                .as_array()
                .and_then(|blocks| blocks.first().cloned())
                .and_then(|block| block.get("text").cloned())
                .and_then(|text| text.as_str().map(str::to_owned))
                .ok_or(sqlx::Error::RowNotFound)
        })
        .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn edit_queued_input_replaces_text_and_bumps_revision(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let (input_id, command_id) = input(&pool, &seed, "старый текст").await;

    let outcome = edit(&pool, &seed, input_id, "новый текст", 1, "edit-1").await?;
    assert_eq!(
        outcome,
        EditInputOutcome {
            input_id,
            revision: 2
        }
    );
    assert_eq!(revision(&pool, input_id).await, 2);
    assert_eq!(deliver_text(&pool, command_id).await, "новый текст");
    // Повтор тем же ключом возвращает записанный итог, ревизия не растёт.
    let replay = edit(&pool, &seed, input_id, "новый текст", 1, "edit-1").await?;
    assert_eq!(replay, outcome);
    assert_eq!(revision(&pool, input_id).await, 2);
    // Событие для ленты несёт новое содержимое и ревизию.
    let (kind, payload): (String, Value) = sqlx::query_as(
        "select kind, payload from events where conversation_id = $1 and kind = 'input_state_changed' order by scope_seq desc limit 1",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(kind, "input_state_changed");
    assert_eq!(payload["input_id"], json!(input_id));
    assert_eq!(payload["cause"], "edited");
    assert_eq!(payload["revision"], 2);
    assert_eq!(payload["content"][0]["text"], "новый текст");
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn stale_revision_does_not_overwrite(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let (input_id, command_id) = input(&pool, &seed, "v1").await;
    edit(&pool, &seed, input_id, "v2", 1, "edit-1").await?;
    assert!(matches!(
        edit(&pool, &seed, input_id, "протухшая", 1, "edit-2").await,
        Err(ControlError::InvalidRequest {
            reason: "revision_conflict"
        })
    ));
    assert_eq!(deliver_text(&pool, command_id).await, "v2");
    assert_eq!(revision(&pool, input_id).await, 2);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn edit_refused_once_not_queued(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, _) = input(&pool, &seed, "second").await;
    // Выданный ввод правится только отзывом.
    dispatch_next(
        &pool,
        seed.conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(matches!(
        edit(&pool, &seed, first_id, "поздно", 1, "e1").await,
        Err(ControlError::InvalidRequest {
            reason: "input_not_queued"
        })
    ));
    for receipt in [
        InputReceipt::Stored,
        InputReceipt::TransmissionStarted,
        InputReceipt::Accepted,
    ] {
        apply_input_receipt(&pool, first_cmd, 1, receipt, Uuid::now_v7())
            .await
            .unwrap();
    }
    assert!(matches!(
        edit(&pool, &seed, first_id, "поздно", 1, "e2").await,
        Err(ControlError::InvalidRequest {
            reason: "input_not_queued"
        })
    ));
    // Отменённый — тоже.
    canterlot_hub::delivery::control::cancel_input(
        &pool,
        seed.owner_id,
        second_id,
        "cancel",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(matches!(
        edit(&pool, &seed, second_id, "поздно", 1, "e3").await,
        Err(ControlError::InvalidRequest {
            reason: "input_not_queued"
        })
    ));
    assert_eq!(state(&pool, second_id).await, InputState::Cancelled);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn edit_races_dispatch_without_double_delivery(pool: PgPool) -> TestResult {
    for _ in 0..25 {
        let seed = testkit::seed(&pool).await;
        running(&pool, &seed).await;
        let (input_id, command_id) = input(&pool, &seed, "исходный").await;
        let barrier = Arc::new(Barrier::new(3));
        let edit = {
            let pool = pool.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                request_edit_input(
                    &pool,
                    seed.owner_id,
                    seed.conversation_id,
                    input_id,
                    "исправленный".into(),
                    Vec::new(),
                    1,
                    &Uuid::now_v7().to_string(),
                    Uuid::now_v7(),
                )
                .await
            })
        };
        let dispatch = {
            let pool = pool.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                dispatch_next(
                    &pool,
                    seed.conversation_id,
                    GateInputs {
                        machine_available: true,
                        machine_reconciled: true,
                        session_capabilities: vec!["pilot_v1".into()],
                    },
                    Uuid::now_v7(),
                )
                .await
            })
        };
        barrier.wait().await;
        let (edited, dispatched) = tokio::join!(edit, dispatch);
        let edited = edited.unwrap();
        let dispatched = dispatched.unwrap().unwrap();
        let delivered = deliver_text(&pool, command_id).await;
        match (edited, dispatched) {
            // Правка первая: выдача несёт новый текст ровно один раз.
            (Ok(_), DispatchOutcome::Dispatched { .. }) => {
                assert_eq!(delivered, "исправленный");
                assert_eq!(revision(&pool, input_id).await, 2);
            }
            // Выдача первая: правка отклонена, выдан старый текст ровно один раз.
            (
                Err(ControlError::InvalidRequest {
                    reason: "input_not_queued",
                }),
                DispatchOutcome::Dispatched { .. },
            ) => {
                assert_eq!(delivered, "исходный");
                assert_eq!(revision(&pool, input_id).await, 1);
            }
            (edited, dispatched) => {
                panic!("несогласованный исход: {edited:?} {dispatched:?}")
            }
        }
        // Не больше одной выдачи на ввод.
        let attempts: i32 =
            sqlx::query_scalar("select dispatch_attempt from commands where id = $1")
                .bind(command_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(attempts, 1);
    }
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn edit_updates_pending_steer_and_refuses_in_flight(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    sqlx::query("update conversations set harness = 'codex' where id = $1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;
    running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, second_cmd) = input(&pool, &seed, "second").await;
    dispatch_next(
        &pool,
        seed.conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into(), "send_now_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    for receipt in [
        InputReceipt::Stored,
        InputReceipt::TransmissionStarted,
        InputReceipt::Accepted,
    ] {
        apply_input_receipt(&pool, first_cmd, 1, receipt, Uuid::now_v7())
            .await
            .unwrap();
    }
    let steer_id = Uuid::now_v7();
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, execution_id, execution_generation,
                               kind, payload, target_command_id, delivery_state, dispatch_attempt)
         values ($1, $2, $3, (select id from executions where conversation_id = $3 and state = 'running'), 1,
                 'steer_input', $4, $5, 'pending', 0)",
    )
    .bind(steer_id)
    .bind(seed.machine_id)
    .bind(seed.conversation_id)
    .bind(json!({"input_id": second_id, "conversation_id": seed.conversation_id, "content": [{"type":"text","text":"second"}], "attachment_ids": [], "expected_running_command_id": first_cmd}))
    .bind(second_cmd)
    .execute(&pool)
    .await?;
    edit(&pool, &seed, second_id, "second, исправленный", 1, "edit-steer").await?;
    // Ожидающий steer тянет новое содержимое.
    let steer_text: String = sqlx::query_scalar(
        "select payload->'content'->0->>'text' from commands where id = $1",
    )
    .bind(steer_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(steer_text, "second, исправленный");
    // Ушедший на машину steer правится только отзывом.
    sqlx::query("update commands set delivery_state = 'stored_on_machine' where id = $1")
        .bind(steer_id)
        .execute(&pool)
        .await?;
    assert!(matches!(
        edit(&pool, &seed, second_id, "ещё правка", 2, "edit-steer-2").await,
        Err(ControlError::InvalidRequest {
            reason: "steer_in_flight"
        })
    ));
    assert_eq!(deliver_text(&pool, second_cmd).await, "second, исправленный");
    let _ = first_id;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn edit_replaces_attachment_list(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    sqlx::query("update machines set capabilities = '{pilot_v1}', limits_ok = true where id = $1")
        .bind(seed.machine_id)
        .execute(&pool)
        .await?;
    let (input_id, command_id) = input(&pool, &seed, "с файлом").await;
    let kept = Uuid::now_v7();
    let dropped = Uuid::now_v7();
    for (id, name) in [(kept, "keep.txt"), (dropped, "drop.txt")] {
        sqlx::query(
            "insert into attachments(id,owner_id,conversation_id,name,mime,size_bytes,sha256,state)
             values($1,$2,$3,$4,'text/plain',8,'2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881','ready')",
        )
        .bind(id)
        .bind(seed.owner_id)
        .bind(seed.conversation_id)
        .bind(name)
        .execute(&pool)
        .await?;
    }
    sqlx::query("insert into input_attachments(input_id,attachment_id,position) values($1,$2,0),($1,$3,1)")
        .bind(input_id).bind(kept).bind(dropped).execute(&pool).await?;
    let outcome = request_edit_input(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        input_id,
        "без лишнего".into(),
        vec![kept],
        1,
        "edit-attachments",
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(outcome.revision, 2);
    let linked: Vec<Uuid> = sqlx::query_scalar(
        "select attachment_id from input_attachments where input_id = $1 order by position",
    )
    .bind(input_id)
    .fetch_all(&pool)
    .await?;
    assert_eq!(linked, vec![kept]);
    let content: Value =
        sqlx::query_scalar("select content from inputs where id = $1")
            .bind(input_id)
            .fetch_one(&pool)
            .await?;
    let texts: Vec<&str> = content
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block.get("text")?.as_str())
        .collect();
    assert_eq!(texts, vec!["без лишнего"]);
    assert_eq!(deliver_text(&pool, command_id).await, "без лишнего");
    Ok(())
}
