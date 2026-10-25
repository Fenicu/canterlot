use std::fmt::Debug;
use std::str::FromStr;

use canterlot_hub::model::{
    CommandKind, DeliveryState, ExecutionState, HoldReason, InputState, PauseReason, RunSlot,
};
use canterlot_hub::testkit::{self, Seed};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn migrations_apply_and_seed_works(pool: PgPool) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    let ids: (Uuid, Uuid, Uuid, String, String) = sqlx::query_as(
        "select o.id, m.id, c.id, c.harness, c.workdir
         from owners o join machines m on m.owner_id = o.id
         join conversations c on c.machine_id = m.id and c.owner_id = o.id
         where c.id = $1",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        ids,
        (
            seed.owner_id,
            seed.machine_id,
            seed.conversation_id,
            "synthetic".into(),
            "/tmp/w".into(),
        )
    );
    for id in [seed.owner_id, seed.machine_id, seed.conversation_id] {
        assert_eq!(id.get_version_num(), 7);
    }

    assert_initial_queue(&pool, seed.conversation_id).await?;
    let conversation_id = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    assert_ne!(conversation_id, seed.conversation_id);
    assert_eq!(conversation_id.get_version_num(), 7);
    let conversation: (Uuid, Uuid, String, String) = sqlx::query_as(
        "select owner_id, machine_id, harness, workdir from conversations where id = $1",
    )
    .bind(conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        conversation,
        (
            seed.owner_id,
            seed.machine_id,
            "synthetic".into(),
            "/tmp/w".into()
        )
    );
    assert_initial_queue(&pool, conversation_id).await?;
    Ok(())
}

async fn assert_initial_queue(pool: &PgPool, conversation_id: Uuid) -> Result<(), sqlx::Error> {
    let queue: (Value, Value, String, Option<Uuid>, Option<i32>, i64, bool) = sqlx::query_as(
        "select pause_reasons, hold_reasons, run_slot, run_slot_input_id, run_slot_attempt,
                next_input_seq, closed from queue_state where conversation_id = $1",
    )
    .bind(conversation_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(
        queue,
        (
            json!([]),
            json!([{"reason": "no_live_execution"}]),
            "free".into(),
            None,
            None,
            1,
            false,
        )
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn input_seq_unique_per_conversation(pool: PgPool) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    insert_input(&pool, &seed, 1, "queued").await?;
    let error = insert_input(&pool, &seed, 1, "queued").await.unwrap_err();
    let error = error.as_database_error().unwrap();
    assert!(error.is_unique_violation());
    assert_eq!(
        error.constraint(),
        Some("inputs_conversation_id_input_seq_key")
    );
    insert_input(&pool, &seed, 2, "queued").await?;

    let other_seed = Seed {
        conversation_id: testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await,
        ..seed
    };
    insert_input(&pool, &other_seed, 1, "queued").await?;
    Ok(())
}

async fn insert_input(
    pool: &PgPool,
    seed: &Seed,
    input_seq: i64,
    state: &str,
) -> Result<Uuid, sqlx::Error> {
    let command_id = Uuid::now_v7();
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, kind, payload, delivery_state)
         values ($1, $2, $3, 'deliver_input', '{}', 'held')",
    )
    .bind(command_id)
    .bind(seed.machine_id)
    .bind(seed.conversation_id)
    .execute(pool)
    .await?;

    let input_id = Uuid::now_v7();
    sqlx::query(
        "insert into inputs (id, conversation_id, owner_id, input_seq, idempotency_key,
                             content, state, command_id, received_at)
         values ($1, $2, $3, $4, $5, '{}', $6, $7, now())",
    )
    .bind(input_id)
    .bind(seed.conversation_id)
    .bind(seed.owner_id)
    .bind(input_seq)
    .bind(input_id.to_string())
    .bind(state)
    .bind(command_id)
    .execute(pool)
    .await?;
    Ok(input_id)
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn one_live_execution_per_conversation(pool: PgPool) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();
    sqlx::query(
        "insert into executions (id, conversation_id, generation, state)
         values ($1, $2, 1, 'running')",
    )
    .bind(first)
    .bind(seed.conversation_id)
    .execute(&pool)
    .await?;
    let error = sqlx::query(
        "insert into executions (id, conversation_id, generation, state)
         values ($1, $2, 2, 'running')",
    )
    .bind(second)
    .bind(seed.conversation_id)
    .execute(&pool)
    .await
    .unwrap_err();
    let error = error.as_database_error().unwrap();
    assert!(error.is_unique_violation());
    assert_eq!(error.constraint(), Some("executions_one_live"));

    sqlx::query("update executions set state = 'stopped' where id = $1")
        .bind(first)
        .execute(&pool)
        .await?;
    sqlx::query(
        "insert into executions (id, conversation_id, generation, state)
         values ($1, $2, 2, 'running')",
    )
    .bind(second)
    .bind(seed.conversation_id)
    .execute(&pool)
    .await?;
    let states: Vec<String> = sqlx::query_scalar(
        "select state from executions where conversation_id = $1 order by generation",
    )
    .bind(seed.conversation_id)
    .fetch_all(&pool)
    .await?;
    assert_eq!(states, ["stopped", "running"]);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn state_check_constraints(pool: PgPool) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    let error = insert_input(&pool, &seed, 1, "bogus").await.unwrap_err();
    let error = error.as_database_error().unwrap();
    assert!(error.is_check_violation());
    assert_eq!(error.constraint(), Some("inputs_state_check"));
    let count: i64 = sqlx::query_scalar("select count(*) from inputs")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 0);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn model_enums_roundtrip(_pool: PgPool) {
    for (value, name) in [
        (InputState::Queued, "queued"),
        (InputState::Dispatched, "dispatched"),
        (InputState::OnMachine, "on_machine"),
        (InputState::Transmitting, "transmitting"),
        (InputState::Accepted, "accepted"),
        (InputState::AcceptanceUnconfirmed, "acceptance_unconfirmed"),
        (InputState::Cancelled, "cancelled"),
        (InputState::Rejected, "rejected"),
    ] {
        assert_eq!(value.as_str(), name);
        assert_text_roundtrip(value, name);
    }
    for (value, name) in [
        (ExecutionState::Created, "created"),
        (ExecutionState::Starting, "starting"),
        (ExecutionState::Running, "running"),
        (ExecutionState::Stopping, "stopping"),
        (ExecutionState::Stopped, "stopped"),
        (ExecutionState::Failed, "failed"),
        (ExecutionState::OwnerLost, "owner_lost"),
    ] {
        assert_eq!(value.as_str(), name);
        assert_text_roundtrip(value, name);
    }
    for (value, name) in [
        (CommandKind::DeliverInput, "deliver_input"),
        (CommandKind::CancelInput, "cancel_input"),
        (CommandKind::WithdrawInput, "withdraw_input"),
        (CommandKind::Interrupt, "interrupt"),
        (CommandKind::StartExecution, "start_execution"),
        (CommandKind::StopExecution, "stop_execution"),
    ] {
        assert_eq!(value.as_str(), name);
        assert_text_roundtrip(value, name);
    }
    for (value, name) in [
        (DeliveryState::Held, "held"),
        (DeliveryState::Pending, "pending"),
        (DeliveryState::StoredOnMachine, "stored_on_machine"),
        (DeliveryState::Completed, "completed"),
    ] {
        assert_eq!(value.as_str(), name);
        assert_text_roundtrip(value, name);
    }

    let input_id = Uuid::now_v7();
    for (value, name, expected) in [
        (
            PauseReason::UserStop,
            "user_stop",
            json!({"reason": "user_stop"}),
        ),
        (
            PauseReason::FatalError,
            "fatal_error",
            json!({"reason": "fatal_error"}),
        ),
        (
            PauseReason::ProcessExited,
            "process_exited",
            json!({"reason": "process_exited"}),
        ),
        (
            PauseReason::LimitReached,
            "limit_reached",
            json!({"reason": "limit_reached"}),
        ),
        (
            PauseReason::OwnerLost,
            "owner_lost",
            json!({"reason": "owner_lost"}),
        ),
        (
            PauseReason::RestorePending,
            "restore_pending",
            json!({"reason": "restore_pending"}),
        ),
    ] {
        assert_eq!(value.as_str(), name);
        assert_eq!(name.parse::<PauseReason>().unwrap(), value);
        assert_json_roundtrip(value, expected);
    }
    let reason = PauseReason::UnconfirmedDelivery { input_id };
    assert_eq!(reason.as_str(), "unconfirmed_delivery");
    assert_json_roundtrip(
        reason,
        json!({"reason": "unconfirmed_delivery", "input_id": input_id}),
    );
    assert!("unconfirmed_delivery".parse::<PauseReason>().is_err());
    assert!(
        serde_json::from_value::<PauseReason>(json!({"reason": "unconfirmed_delivery"})).is_err()
    );

    for (value, name, expected) in [
        (
            HoldReason::RunInProgress,
            "run_in_progress",
            json!({"reason": "run_in_progress"}),
        ),
        (
            HoldReason::AwaitingUserRequest,
            "awaiting_user_request",
            json!({"reason": "awaiting_user_request"}),
        ),
        (
            HoldReason::ContextMaintenance,
            "context_maintenance",
            json!({"reason": "context_maintenance"}),
        ),
        (
            HoldReason::NoLiveExecution,
            "no_live_execution",
            json!({"reason": "no_live_execution"}),
        ),
        (
            HoldReason::ExecutionStarting,
            "execution_starting",
            json!({"reason": "execution_starting"}),
        ),
        (
            HoldReason::StorageLow,
            "storage_low",
            json!({"reason": "storage_low"}),
        ),
    ] {
        assert_eq!(value.as_str(), name);
        assert_eq!(name.parse::<HoldReason>().unwrap(), value);
        assert_json_roundtrip(value, expected);
    }
    let conflict_conversation_id = Uuid::now_v7();
    let reason = HoldReason::WorkdirBusy {
        conflict_conversation_id,
    };
    assert_eq!(reason.as_str(), "workdir_busy");
    assert_json_roundtrip(
        reason,
        json!({"reason": "workdir_busy", "conflict_conversation_id": conflict_conversation_id}),
    );
    let reason = HoldReason::AttachmentsPending { input_id };
    assert_eq!(reason.as_str(), "attachments_pending");
    assert_json_roundtrip(
        reason,
        json!({"reason": "attachments_pending", "input_id": input_id}),
    );
    for name in [
        "workdir_busy",
        "attachments_pending",
        "machine_not_reconciled",
        "bogus",
    ] {
        assert!(name.parse::<HoldReason>().is_err());
    }
    for name in ["workdir_busy", "attachments_pending"] {
        assert!(serde_json::from_value::<HoldReason>(json!({"reason": name})).is_err());
    }
    assert!("bogus".parse::<InputState>().is_err());
    assert!("bogus".parse::<ExecutionState>().is_err());
    assert!("bogus".parse::<CommandKind>().is_err());
    assert!("bogus".parse::<DeliveryState>().is_err());
    assert!("bogus".parse::<PauseReason>().is_err());

    for (slot, expected) in [
        (RunSlot::Free, ("free", None, None)),
        (
            RunSlot::Dispatching {
                input_id,
                attempt: 3,
            },
            ("dispatching", Some(input_id), Some(3)),
        ),
        (
            RunSlot::Running { input_id },
            ("running", Some(input_id), None),
        ),
    ] {
        let columns = slot.to_columns();
        assert_eq!(slot.as_str(), expected.0);
        assert_eq!(columns, expected);
        assert_eq!(
            RunSlot::from_columns(columns.0, columns.1, columns.2).unwrap(),
            slot
        );
    }
    for (name, id, attempt) in [
        ("bogus", None, None),
        ("free", Some(input_id), None),
        ("free", None, Some(1)),
        ("dispatching", None, Some(1)),
        ("dispatching", Some(input_id), None),
        ("running", None, None),
        ("running", Some(input_id), Some(1)),
    ] {
        assert!(RunSlot::from_columns(name, id, attempt).is_err());
    }
}

fn assert_text_roundtrip<T>(value: T, name: &str)
where
    T: FromStr + Debug + PartialEq + Serialize + DeserializeOwned,
    T::Err: Debug,
{
    assert_eq!(name.parse::<T>().unwrap(), value);
    assert_json_roundtrip(value, json!(name));
}

fn assert_json_roundtrip<T>(value: T, expected: Value)
where
    T: Debug + PartialEq + Serialize + DeserializeOwned,
{
    let encoded = serde_json::to_value(&value).unwrap();
    assert_eq!(encoded, expected);
    assert_eq!(serde_json::from_value::<T>(encoded).unwrap(), value);
}
