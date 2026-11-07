//! «Отправить сейчас»: влить ожидающий ввод в идущий ход.
//!
//! Проверяются команда и её гонки: ввод остаётся в очереди, слот хода не
//! трогается, конец хода между кликом и доставкой даёт обычный следующий ход
//! без дубля, отказ оставляет ввод в очереди.

use canterlot_hub::{
    delivery::{
        control::{ControlError, SendNowOutcome, request_send_now},
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
        receipts::{
            ApplyOutcome, Ignored, InputReceipt, apply_input_receipt, apply_run_finished,
            expire_steers,
        },
        submit::{ContentBlock, SubmitInput, submit_input},
    },
    executions::{self},
    model::{ExecutionState, InputState, RunSlot},
    testkit::{self, Seed},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const CAPS: [&str; 1] = ["send_now_v1"];

fn caps() -> Vec<String> {
    CAPS.iter().map(|c| c.to_string()).collect()
}

async fn harness(pool: &PgPool, seed: &Seed, harness: &str) {
    sqlx::query("update conversations set harness = $1 where id = $2")
        .bind(harness)
        .bind(seed.conversation_id)
        .execute(pool)
        .await
        .unwrap();
}

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

async fn running(pool: &PgPool, seed: &Seed) -> Uuid {
    let execution = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    execution.execution_id
}

async fn gate_dispatch(pool: &PgPool, conversation_id: Uuid) -> DispatchOutcome {
    dispatch_next(
        pool,
        conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into(), "send_now_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap()
}

async fn accept_turn(pool: &PgPool, command_id: Uuid) {
    for receipt in [
        InputReceipt::Stored,
        InputReceipt::TransmissionStarted,
        InputReceipt::Accepted,
    ] {
        apply_input_receipt(pool, command_id, 1, receipt, Uuid::now_v7())
            .await
            .unwrap();
    }
}

async fn apply_steer(pool: &PgPool, steer_command_id: Uuid, receipt: InputReceipt) -> ApplyOutcome {
    apply_input_receipt(pool, steer_command_id, 0, receipt, Uuid::now_v7())
        .await
        .unwrap()
}

async fn send_now(
    pool: &PgPool,
    seed: &Seed,
    input_id: Uuid,
    key: &str,
    capabilities: &[String],
) -> Result<SendNowOutcome, ControlError> {
    request_send_now(
        pool,
        seed.owner_id,
        seed.conversation_id,
        input_id,
        key,
        Uuid::now_v7(),
        capabilities,
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

async fn slot(pool: &PgPool, conversation_id: Uuid) -> RunSlot {
    let (name, id, attempt): (String, Option<Uuid>, Option<i32>) = sqlx::query_as(
        "select run_slot, run_slot_input_id, run_slot_attempt from queue_state where conversation_id = $1",
    )
    .bind(conversation_id)
    .fetch_one(pool)
    .await
    .unwrap();
    RunSlot::from_columns(&name, id, attempt).unwrap()
}

async fn steer_row(pool: &PgPool, input_command_id: Uuid) -> (Uuid, String, Value) {
    sqlx::query_as(
        "select id, delivery_state, payload from commands where kind = 'steer_input' and target_command_id = $1",
    )
    .bind(input_command_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn send_now_into_running_turn_keeps_queue_and_slot(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, second_cmd) = input(&pool, &seed, "second").await;
    assert!(matches!(
        gate_dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Dispatched { .. }
    ));
    accept_turn(&pool, first_cmd).await;
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running { input_id: first_id }
    );

    let outcome = send_now(&pool, &seed, second_id, "send-now-1", &caps()).await?;
    let SendNowOutcome::Requested { steer_command_id } = outcome else {
        panic!("expected Requested, got {outcome:?}");
    };
    // Ввод остался в очереди, слот занят прежним ходом, второго хода нет.
    assert_eq!(state(&pool, second_id).await, InputState::Queued);
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running { input_id: first_id }
    );
    let (row_id, delivery, payload) = steer_row(&pool, second_cmd).await;
    assert_eq!(row_id, steer_command_id);
    assert_eq!(delivery, "pending");
    assert_eq!(
        payload["expected_running_command_id"],
        json!(first_cmd.to_string())
    );
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn send_now_is_idempotent(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, _) = input(&pool, &seed, "second").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;
    let _ = first_id;

    let first = send_now(&pool, &seed, second_id, "key-1", &caps()).await?;
    let replay = send_now(&pool, &seed, second_id, "key-1", &caps()).await?;
    assert_eq!(first, replay);
    let SendNowOutcome::Requested { steer_command_id } = first else {
        panic!("{first:?}");
    };
    // Другой ключ на тот же ввод: та же команда, нового хода нет.
    let other = send_now(&pool, &seed, second_id, "key-2", &caps()).await?;
    assert_eq!(other, SendNowOutcome::AlreadyRequested { steer_command_id });
    let count: i64 = sqlx::query_scalar(
        "select count(*) from commands where kind = 'steer_input' and conversation_id = $1",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(count, 1);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn send_now_valid_only_while_queued_and_running(pool: PgPool) -> TestResult {
    // Отдельный разговор: без живого исполнения хода нет.
    let lonely_seed = testkit::seed(&pool).await;
    harness(&pool, &lonely_seed, "codex").await;
    let (lonely_id, _) = input(&pool, &lonely_seed, "lonely").await;
    assert!(matches!(
        send_now(&pool, &lonely_seed, lonely_id, "k", &caps()).await,
        Err(ControlError::InvalidRequest {
            reason: "no_running_turn"
        })
    ));

    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "claude_code").await;
    running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, _) = input(&pool, &seed, "second").await;
    // Ход ещё не идёт (слот свободен): вливать некуда.
    assert!(matches!(
        send_now(&pool, &seed, second_id, "k", &caps()).await,
        Err(ControlError::InvalidRequest {
            reason: "no_running_turn"
        })
    ));
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;
    // Выданный ввод уже не в очереди.
    assert!(matches!(
        send_now(&pool, &seed, first_id, "k", &caps()).await,
        Err(ControlError::InvalidRequest {
            reason: "input_not_queued"
        })
    ));
    // Нет возможности — чистый отказ, команда не создаётся.
    assert!(matches!(
        send_now(&pool, &seed, second_id, "k", &[]).await,
        Err(ControlError::InvalidRequest {
            reason: "capability_missing"
        })
    ));
    // Чужой профиль — тоже отказ.
    harness(&pool, &seed, "synthetic").await;
    assert!(matches!(
        send_now(&pool, &seed, second_id, "k", &caps()).await,
        Err(ControlError::InvalidRequest {
            reason: "unsupported_harness"
        })
    ));
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn send_now_refused_for_request_and_compaction(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    let execution_id = running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, _) = input(&pool, &seed, "second").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;
    let _ = first_id;

    sqlx::query(
        "insert into requests (id, conversation_id, execution_id, generation, kind, native_request_id, request_payload, state)
         values ($1, $2, $3, 1, 'permission', 'tool-1', '{}', 'open')",
    )
    .bind(Uuid::now_v7())
    .bind(seed.conversation_id)
    .bind(execution_id)
    .execute(&pool)
    .await?;
    assert!(matches!(
        send_now(&pool, &seed, second_id, "k1", &caps()).await,
        Err(ControlError::InvalidRequest {
            reason: "request_open"
        })
    ));
    sqlx::query("update requests set state = 'decision_applied' where conversation_id = $1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;

    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, execution_id, execution_generation,
                               kind, payload, delivery_state, dispatch_attempt)
         values ($1, $2, $3, $4, 1, 'compact_session', '{}', 'pending', 0)",
    )
    .bind(Uuid::now_v7())
    .bind(seed.machine_id)
    .bind(seed.conversation_id)
    .bind(execution_id)
    .execute(&pool)
    .await?;
    assert!(matches!(
        send_now(&pool, &seed, second_id, "k2", &caps()).await,
        Err(ControlError::InvalidRequest {
            reason: "compacting"
        })
    ));
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn steer_accepted_belongs_to_running_turn(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    let execution_id = running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, second_cmd) = input(&pool, &seed, "second").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;

    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, second_id, "k", &caps()).await?
    else {
        panic!("not requested");
    };
    let outcome = apply_steer(&pool, steer_command_id, InputReceipt::Accepted).await;
    assert!(matches!(
        outcome,
        ApplyOutcome::Applied {
            input_state: InputState::Accepted
        }
    ));
    // Ответ принадлежит идущему ходу: слот не тронут, второго хода нет.
    assert_eq!(state(&pool, second_id).await, InputState::Accepted);
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running { input_id: first_id }
    );
    let (_, delivery, _): (Uuid, String, Value) = steer_row(&pool, second_cmd).await;
    assert_eq!(delivery, "completed");
    // Удержанная выдача этого ввода больше не висит: текст ушёл вливание
    // в чужой ход, и вторая команда не должна выглядеть ждущей доставки.
    let (own_delivery, own_completed): (String, bool) = sqlx::query_as(
        "select delivery_state, completed_at is not null from commands where id = $1",
    )
    .bind(second_cmd)
    .fetch_one(&pool)
    .await?;
    assert_eq!(own_delivery, "completed");
    assert!(own_completed);
    // Ход кончился как обычно.
    apply_run_finished(&pool, execution_id, first_cmd, Uuid::now_v7()).await?;
    assert_eq!(slot(&pool, seed.conversation_id).await, RunSlot::Free);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn turn_end_between_click_and_delivery_becomes_next_turn(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "claude_code").await;
    let execution_id = running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, second_cmd) = input(&pool, &seed, "second").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;

    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, second_id, "k", &caps()).await?
    else {
        panic!("not requested");
    };
    // Ход кончился раньше доставки: слот свободен, но выдача ждёт исхода steer.
    apply_run_finished(&pool, execution_id, first_cmd, Uuid::now_v7()).await?;
    assert!(
        matches!(
            gate_dispatch(&pool, seed.conversation_id).await,
            DispatchOutcome::Empty | DispatchOutcome::Blocked(_)
        ),
        "steer still pending: no second delivery yet"
    );
    // Воркер ответил «хода уже нет»: ввод снова в голове очереди.
    let outcome = apply_steer(
        &pool,
        steer_command_id,
        InputReceipt::DispatchDeferred {
            reason: "steer_no_turn".into(),
        },
    )
    .await;
    assert!(matches!(
        outcome,
        ApplyOutcome::Applied {
            input_state: InputState::Queued
        }
    ));
    assert_eq!(state(&pool, second_id).await, InputState::Queued);
    // Обычная выдача следующим ходом — ровно один раз, без дубля.
    assert!(matches!(
        gate_dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Dispatched { .. }
    ));
    let attempt: i32 = sqlx::query_scalar("select dispatch_attempt from commands where id = $1")
        .bind(second_cmd)
        .fetch_one(&pool)
        .await?;
    assert_eq!(attempt, 1);
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from commands where kind = 'steer_input' and delivery_state <> 'completed'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(pending, 0);
    let _ = first_id;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn steer_rejected_and_unconfirmed(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "opencode").await;
    running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, second_cmd) = input(&pool, &seed, "second").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;

    // Отказ воркера влить ввод не губит его: ввод остаётся в очереди, слот
    // чужого хода цел, причина у команды вливания.
    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, second_id, "k1", &caps()).await?
    else {
        panic!("not requested");
    };
    let outcome = apply_steer(
        &pool,
        steer_command_id,
        InputReceipt::ExplicitRejection {
            reason: "input_too_long".into(),
        },
    )
    .await;
    assert!(matches!(
        outcome,
        ApplyOutcome::Applied {
            input_state: InputState::Queued
        }
    ));
    assert_eq!(state(&pool, second_id).await, InputState::Queued);
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running { input_id: first_id }
    );
    let (_, delivery, _) = steer_row(&pool, second_cmd).await;
    assert_eq!(delivery, "completed");

    // Неподтверждённый приём — пауза, но слот хода не освобождается.
    let (third_id, third_cmd) = input(&pool, &seed, "third").await;
    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, third_id, "k2", &caps()).await?
    else {
        panic!("not requested");
    };
    let outcome = apply_steer(
        &pool,
        steer_command_id,
        InputReceipt::AcceptanceUndetermined,
    )
    .await;
    assert!(matches!(
        outcome,
        ApplyOutcome::Applied {
            input_state: InputState::AcceptanceUnconfirmed
        }
    ));
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running { input_id: first_id }
    );
    // Позднее доказательство снимает паузу, ход тот же.
    let outcome = apply_steer(&pool, steer_command_id, InputReceipt::Accepted).await;
    assert!(matches!(
        outcome,
        ApplyOutcome::Applied {
            input_state: InputState::Accepted
        }
    ));
    assert_eq!(state(&pool, third_id).await, InputState::Accepted);
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running { input_id: first_id }
    );
    let _ = (second_cmd, third_cmd);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn stale_steer_outcome_is_ignored(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    let execution_id = running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, second_cmd) = input(&pool, &seed, "second").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;

    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, second_id, "k", &caps()).await?
    else {
        panic!("not requested");
    };
    // Ход кончился, steer отвергнут, ввод ушёл обычным ходом и принят.
    apply_run_finished(&pool, execution_id, first_cmd, Uuid::now_v7()).await?;
    assert!(matches!(
        apply_steer(
            &pool,
            steer_command_id,
            InputReceipt::DispatchDeferred {
                reason: "steer_no_turn".into(),
            }
        )
        .await,
        ApplyOutcome::Applied {
            input_state: InputState::Queued
        }
    ));
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, second_cmd).await;
    // Опоздавший дубль steer-квитанции ничего не меняет.
    assert!(matches!(
        apply_steer(&pool, steer_command_id, InputReceipt::Accepted).await,
        ApplyOutcome::Ignored(Ignored::Terminal)
            | ApplyOutcome::Ignored(Ignored::NotMonotonic)
            | ApplyOutcome::Ignored(Ignored::StaleAttempt)
    ));
    assert_eq!(state(&pool, second_id).await, InputState::Accepted);
    let _ = first_id;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn several_send_now_in_a_row_keep_order(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (y1_id, y1_cmd) = input(&pool, &seed, "y1").await;
    let (y2_id, y2_cmd) = input(&pool, &seed, "y2").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;

    let SendNowOutcome::Requested {
        steer_command_id: s1,
    } = send_now(&pool, &seed, y1_id, "k1", &caps()).await?
    else {
        panic!("not requested");
    };
    let SendNowOutcome::Requested {
        steer_command_id: s2,
    } = send_now(&pool, &seed, y2_id, "k2", &caps()).await?
    else {
        panic!("not requested");
    };
    assert_ne!(s1, s2);
    apply_steer(&pool, s1, InputReceipt::Accepted).await;
    apply_steer(&pool, s2, InputReceipt::Accepted).await;
    assert_eq!(state(&pool, y1_id).await, InputState::Accepted);
    assert_eq!(state(&pool, y2_id).await, InputState::Accepted);
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running { input_id: first_id }
    );
    let _ = (y1_cmd, y2_cmd);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
async fn dispatch_skips_steer_pending_input(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    let execution_id = running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, _) = input(&pool, &seed, "second").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;
    send_now(&pool, &seed, second_id, "k", &caps()).await?;
    // Пока steer висит, выдача не трогает ввод даже при свободном затворе.
    apply_run_finished(&pool, execution_id, first_cmd, Uuid::now_v7()).await?;
    let before: i32 = sqlx::query_scalar("select dispatch_attempt from commands where id = $1")
        .bind(
            sqlx::query_scalar::<_, Uuid>("select command_id from inputs where id = $1")
                .bind(second_id)
                .fetch_one(&pool)
                .await?,
        )
        .fetch_one(&pool)
        .await?;
    assert!(matches!(
        gate_dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Empty | DispatchOutcome::Blocked(_)
    ));
    let after: i32 = sqlx::query_scalar("select dispatch_attempt from commands where id = $1")
        .bind(
            sqlx::query_scalar::<_, Uuid>("select command_id from inputs where id = $1")
                .bind(second_id)
                .fetch_one(&pool)
                .await?,
        )
        .fetch_one(&pool)
        .await?;
    assert_eq!((before, after), (0, 0));
    let _ = first_id;
    Ok(())
}

/// Codex отвечает ошибкой на `turn/steer` (любой код, кроме -32600): воркер
/// пишет отказ. Ввод остаётся в очереди и уходит обычным следующим ходом,
/// владелец видит причину в событии.
#[sqlx::test(migrations = "./migrations")]
async fn codex_steer_rejection_keeps_input_for_next_turn(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    let execution_id = running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, second_cmd) = input(&pool, &seed, "second").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;
    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, second_id, "k", &caps()).await?
    else {
        panic!("not requested");
    };
    let outcome = apply_steer(
        &pool,
        steer_command_id,
        InputReceipt::ExplicitRejection {
            reason: "Internal error: turn cannot be steered".into(),
        },
    )
    .await;
    assert!(matches!(
        outcome,
        ApplyOutcome::Applied {
            input_state: InputState::Queued
        }
    ));
    let (delivery, result): (String, Value) =
        sqlx::query_as("select delivery_state, result from commands where id = $1")
            .bind(steer_command_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(delivery, "completed");
    assert_eq!(
        result,
        json!({"reason": "Internal error: turn cannot be steered"})
    );
    let reject_reason: Option<String> =
        sqlx::query_scalar("select reject_reason from inputs where id = $1")
            .bind(second_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(reject_reason, None);
    let payload: Value = sqlx::query_scalar(
        "select payload from events where kind = 'input_state_changed'
         and payload->>'input_id' = $1 order by scope_seq desc limit 1",
    )
    .bind(second_id.to_string())
    .fetch_one(&pool)
    .await?;
    assert_eq!(payload["state"], json!("queued"));
    assert_eq!(payload["cause"], json!("steer_rejected"));
    assert_eq!(
        payload["reason"],
        json!("Internal error: turn cannot be steered")
    );
    assert_eq!(payload["command_id"], json!(steer_command_id));
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running { input_id: first_id }
    );
    // Ход кончился: ввод уходит обычной доставкой.
    apply_run_finished(&pool, execution_id, first_cmd, Uuid::now_v7()).await?;
    assert!(matches!(
        gate_dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Dispatched { .. }
    ));
    assert_eq!(state(&pool, second_id).await, InputState::Dispatched);
    let _ = second_cmd;
    Ok(())
}

async fn queue_pauses(pool: &PgPool, conversation_id: Uuid) -> Value {
    sqlx::query_scalar("select to_jsonb(pause_reasons) from queue_state where conversation_id = $1")
        .bind(conversation_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn steer_delivery(pool: &PgPool, steer_command_id: Uuid) -> String {
    sqlx::query_scalar("select delivery_state from commands where id = $1")
        .bind(steer_command_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Вливание дошло до машины, а исполнение кончилось без итога: ввод не висит
/// вечно и не уходит второй раз молча, владелец решает сам.
#[sqlx::test(migrations = "./migrations")]
async fn execution_end_settles_a_stored_steer_as_undetermined(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    let execution_id = running(&pool, &seed).await;
    let (_, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, _) = input(&pool, &seed, "second").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;
    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, second_id, "k", &caps()).await?
    else {
        panic!("not requested");
    };
    sqlx::query(
        "update commands set delivery_state = 'stored_on_machine', dispatch_claimed_at = now(),
                stored_at = now() where id = $1",
    )
    .bind(steer_command_id)
    .execute(&pool)
    .await?;
    executions::set_state(
        &pool,
        execution_id,
        ExecutionState::Stopping,
        None,
        Uuid::now_v7(),
    )
    .await?;
    executions::set_state(
        &pool,
        execution_id,
        ExecutionState::Failed,
        Some("supervisor_died"),
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(steer_delivery(&pool, steer_command_id).await, "completed");
    assert_eq!(
        state(&pool, second_id).await,
        InputState::AcceptanceUnconfirmed
    );
    assert!(
        queue_pauses(&pool, seed.conversation_id)
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                == &json!({"reason": "unconfirmed_delivery", "input_id": second_id})),
        "owner decides about the unconfirmed input"
    );
    Ok(())
}

/// Вливание так и не ушло с хаба, а исполнение кончилось: ввод просто
/// остаётся в очереди и уходит следующим ходом.
#[sqlx::test(migrations = "./migrations")]
async fn execution_end_returns_an_unsent_steer_to_the_queue(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    let execution_id = running(&pool, &seed).await;
    let (_, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, _) = input(&pool, &seed, "second").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;
    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, second_id, "k", &caps()).await?
    else {
        panic!("not requested");
    };
    executions::set_state(
        &pool,
        execution_id,
        ExecutionState::Stopping,
        None,
        Uuid::now_v7(),
    )
    .await?;
    executions::set_state(
        &pool,
        execution_id,
        ExecutionState::Failed,
        Some("supervisor_died"),
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(steer_delivery(&pool, steer_command_id).await, "completed");
    assert_eq!(state(&pool, second_id).await, InputState::Queued);
    Ok(())
}

/// Итог вливания не пришёл за отведённый срок: хаб считает приём
/// неподтверждённым, повторно ничего не отправляет.
#[sqlx::test(migrations = "./migrations")]
async fn steer_without_outcome_expires_as_undetermined(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    running(&pool, &seed).await;
    let (first_id, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, _) = input(&pool, &seed, "second").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;
    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, second_id, "k", &caps()).await?
    else {
        panic!("not requested");
    };
    sqlx::query("update commands set delivery_state = 'stored_on_machine', dispatch_claimed_at = now() where id = $1")
        .bind(steer_command_id)
        .execute(&pool)
        .await?;
    // Свежее вливание срок не трогает.
    assert_eq!(
        expire_steers(&pool, std::time::Duration::from_secs(600)).await?,
        0
    );
    assert_eq!(
        steer_delivery(&pool, steer_command_id).await,
        "stored_on_machine"
    );
    sqlx::query("update commands set created_at = now() - interval '1 hour' where id = $1")
        .bind(steer_command_id)
        .execute(&pool)
        .await?;
    assert_eq!(
        expire_steers(&pool, std::time::Duration::from_secs(600)).await?,
        1
    );
    assert_eq!(steer_delivery(&pool, steer_command_id).await, "completed");
    assert_eq!(
        state(&pool, second_id).await,
        InputState::AcceptanceUnconfirmed
    );
    assert_eq!(
        slot(&pool, seed.conversation_id).await,
        RunSlot::Running { input_id: first_id }
    );
    assert_eq!(
        expire_steers(&pool, std::time::Duration::from_secs(600)).await?,
        0
    );
    // Позднее доказательство воркера всё ещё принимается.
    assert!(matches!(
        apply_steer(&pool, steer_command_id, InputReceipt::Accepted).await,
        ApplyOutcome::Applied {
            input_state: InputState::Accepted
        }
    ));
    Ok(())
}

/// Ход кончился, пока вливание A в пути: B, поставленный позже, не обгоняет A.
/// Выдача ждёт итога вливания, затем A уходит первым.
#[sqlx::test(migrations = "./migrations")]
async fn later_input_waits_for_the_steered_head(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    let execution_id = running(&pool, &seed).await;
    let (_, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, _) = input(&pool, &seed, "second").await;
    let (third_id, _) = input(&pool, &seed, "third").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;
    let SendNowOutcome::Requested { steer_command_id } =
        send_now(&pool, &seed, second_id, "k", &caps()).await?
    else {
        panic!("not requested");
    };
    apply_run_finished(&pool, execution_id, first_cmd, Uuid::now_v7()).await?;
    assert!(matches!(
        gate_dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Blocked(_) | DispatchOutcome::Empty
    ));
    assert_eq!(state(&pool, third_id).await, InputState::Queued);
    apply_steer(
        &pool,
        steer_command_id,
        InputReceipt::DispatchDeferred {
            reason: "steer_no_turn".into(),
        },
    )
    .await;
    assert!(matches!(
        gate_dispatch(&pool, seed.conversation_id).await,
        DispatchOutcome::Dispatched { input_id, .. } if input_id == second_id
    ));
    assert_eq!(state(&pool, third_id).await, InputState::Queued);
    Ok(())
}

/// Запрос вливания виден другим вкладкам событием; закрытый разговор не
/// принимает ни вливание, ни правку.
#[sqlx::test(migrations = "./migrations")]
async fn send_now_emits_event_and_respects_closed_queue(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    harness(&pool, &seed, "codex").await;
    running(&pool, &seed).await;
    let (_, first_cmd) = input(&pool, &seed, "first").await;
    let (second_id, _) = input(&pool, &seed, "second").await;
    let (third_id, _) = input(&pool, &seed, "third").await;
    gate_dispatch(&pool, seed.conversation_id).await;
    accept_turn(&pool, first_cmd).await;
    let correlation_id = Uuid::now_v7();
    let SendNowOutcome::Requested { steer_command_id } = request_send_now(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        second_id,
        "k",
        correlation_id,
        &caps(),
    )
    .await?
    else {
        panic!("not requested");
    };
    let (payload, correlation): (Value, Uuid) = sqlx::query_as(
        "select payload, correlation_id from events where kind = 'input_state_changed'
         and payload->>'input_id' = $1 order by scope_seq desc limit 1",
    )
    .bind(second_id.to_string())
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        payload,
        json!({"input_id": second_id, "state": "queued", "cause": "steer_requested", "command_id": steer_command_id})
    );
    assert_eq!(correlation, correlation_id);
    sqlx::query("update queue_state set closed = true where conversation_id = $1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;
    assert!(matches!(
        send_now(&pool, &seed, third_id, "k2", &caps()).await,
        Err(ControlError::InvalidRequest {
            reason: "conversation_closed"
        })
    ));
    assert!(matches!(
        canterlot_hub::delivery::control::request_edit_input(
            &pool,
            canterlot_hub::delivery::control::EditInput {
                owner_id: seed.owner_id,
                conversation_id: seed.conversation_id,
                input_id: third_id,
                text: "правка".into(),
                attachment_ids: Vec::new(),
                expected_revision: 1,
                idempotency_key: "edit-closed".into(),
                correlation_id: Uuid::now_v7(),
            },
        )
        .await,
        Err(ControlError::InvalidRequest {
            reason: "conversation_closed"
        })
    ));
    Ok(())
}
