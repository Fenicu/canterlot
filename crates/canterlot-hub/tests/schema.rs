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
            PauseReason::LimitReached {
                provider: None,
                window: None,
                resets_at: None,
            },
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

async fn execution_for_blocks(pool: &PgPool, seed: &Seed) -> Result<Uuid, sqlx::Error> {
    let execution_id = Uuid::now_v7();
    sqlx::query("insert into executions (id, conversation_id, generation, state) values ($1, $2, 1, 'running')")
        .bind(execution_id).bind(seed.conversation_id).execute(pool).await?;
    Ok(execution_id)
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn blocks_enforce_kind_and_state(pool: PgPool) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    let execution_id = execution_for_blocks(&pool, &seed).await?;
    for kind in [
        "text",
        "reasoning",
        "tool_call",
        "tool_result",
        "warning",
        "error",
    ] {
        for state in ["streaming", "final", "incomplete"] {
            let block_id = Uuid::now_v7();
            sqlx::query("insert into blocks (block_id, execution_id, conversation_id, kind, state, rev) values ($1, $2, $3, $4, $5, 1)")
                .bind(block_id).bind(execution_id).bind(seed.conversation_id).bind(kind).bind(state).execute(&pool).await?;
            let defaults: (String, bool, bool) = sqlx::query_as("select content, created_at is not null, updated_at is not null from blocks where block_id = $1")
                .bind(block_id).fetch_one(&pool).await?;
            assert_eq!(defaults, (String::new(), true, true));
        }
    }
    for (kind, state, constraint) in [
        ("bogus", "streaming", "blocks_kind_check"),
        ("text", "bogus", "blocks_state_check"),
    ] {
        let error = sqlx::query("insert into blocks (block_id, execution_id, conversation_id, kind, state, rev) values ($1, $2, $3, $4, $5, 1)")
            .bind(Uuid::now_v7()).bind(execution_id).bind(seed.conversation_id).bind(kind).bind(state).execute(&pool).await.unwrap_err();
        let error = error.as_database_error().unwrap();
        assert!(error.is_check_violation());
        assert_eq!(error.constraint(), Some(constraint));
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn journal_cursor_links_execution_and_closed_defaults_false(
    pool: PgPool,
) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    let execution_id = execution_for_blocks(&pool, &seed).await?;
    let journal_id = Uuid::now_v7();
    sqlx::query("insert into journal_cursors (machine_id, journal_id, kind, execution_id) values ($1, $2, 'sj', $3)")
        .bind(seed.machine_id).bind(journal_id).bind(execution_id).execute(&pool).await?;
    let cursor: (Uuid, bool) =
        sqlx::query_as("select execution_id, closed from journal_cursors where journal_id = $1")
            .bind(journal_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(cursor, (execution_id, false));
    sqlx::query("update journal_cursors set closed = true where journal_id = $1")
        .bind(journal_id)
        .execute(&pool)
        .await?;
    let error = sqlx::query("insert into journal_cursors (machine_id, journal_id, kind, execution_id) values ($1, $2, 'sj', $3)")
        .bind(seed.machine_id).bind(Uuid::now_v7()).bind(Uuid::now_v7()).execute(&pool).await.unwrap_err();
    assert!(
        error
            .as_database_error()
            .unwrap()
            .is_foreign_key_violation()
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn execution_columns_exist(pool: PgPool) -> Result<(), sqlx::Error> {
    let seed = testkit::seed(&pool).await;
    let execution_id = execution_for_blocks(&pool, &seed).await?;
    let defaults: (Option<String>, Option<String>, bool, bool) = sqlx::query_as("select native_conversation_id, phase, phase_at is null, observed_at is null from executions where id = $1")
        .bind(execution_id).fetch_one(&pool).await?;
    assert_eq!(defaults, (None, None, true, true));
    sqlx::query("update executions set native_conversation_id = 'native', phase = 'awaiting_input', phase_at = clock_timestamp(), observed_at = clock_timestamp() where id = $1")
        .bind(execution_id).execute(&pool).await?;
    let observed: (String, String, bool, bool) = sqlx::query_as("select native_conversation_id, phase, phase_at is not null, observed_at is not null from executions where id = $1")
        .bind(execution_id).fetch_one(&pool).await?;
    assert_eq!(
        observed,
        ("native".into(), "awaiting_input".into(), true, true)
    );
    let input_id = insert_input(&pool, &seed, 1, "queued").await?;
    sqlx::query("update commands set machine_stored_at_ms = 1000 where id = (select command_id from inputs where id = $1)")
        .bind(input_id).execute(&pool).await?;
    sqlx::query("update inputs set transmitted_machine_ms = 1001, agent_confirmed_machine_ms = 1002 where id = $1")
        .bind(input_id).execute(&pool).await?;
    let times: (i64, i64, i64) = sqlx::query_as("select c.machine_stored_at_ms, i.transmitted_machine_ms, i.agent_confirmed_machine_ms from inputs i join commands c on c.id = i.command_id where i.id = $1")
        .bind(input_id).fetch_one(&pool).await?;
    assert_eq!(times, (1000, 1001, 1002));
    Ok(())
}

#[test]
fn quota_hold_and_limit_pause_json_roundtrip() {
    use chrono::{DateTime, Utc};
    let at: DateTime<Utc> = "2026-10-27T12:00:00Z".parse().unwrap();
    let quota = HoldReason::QuotaExhausted {
        provider: "claude".into(),
        window: "five_hour".into(),
        wake_at: at,
    };
    assert_eq!(quota.as_str(), "quota_exhausted");
    assert_json_roundtrip(
        quota,
        json!({"reason":"quota_exhausted", "provider":"claude", "window":"five_hour", "wake_at":"2026-10-27T12:00:00.000Z"}),
    );
    let reserve = HoldReason::WeeklyReserve {
        provider: "codex".into(),
        resets_at: Some(at),
    };
    assert_eq!(reserve.as_str(), "weekly_reserve");
    assert_json_roundtrip(
        reserve,
        json!({"reason":"weekly_reserve", "provider":"codex", "resets_at":"2026-10-27T12:00:00.000Z"}),
    );
    assert_json_roundtrip(
        HoldReason::WeeklyReserve {
            provider: "claude".into(),
            resets_at: None,
        },
        json!({"reason":"weekly_reserve", "provider":"claude", "resets_at":null}),
    );
    let pause = PauseReason::LimitReached {
        provider: Some("claude".into()),
        window: Some("weekly".into()),
        resets_at: Some(at),
    };
    assert_eq!(pause.as_str(), "limit_reached");
    assert_json_roundtrip(
        pause,
        json!({"reason":"limit_reached", "provider":"claude", "window":"weekly", "resets_at":"2026-10-27T12:00:00.000Z"}),
    );
    for name in ["quota_exhausted", "weekly_reserve"] {
        assert!(name.parse::<HoldReason>().is_err());
    }
}

#[test]
fn legacy_limit_reached_pause_json_still_parses() {
    let pause: PauseReason = serde_json::from_str(r#"{"reason":"limit_reached"}"#).unwrap();
    assert_eq!(
        pause,
        PauseReason::LimitReached {
            provider: None,
            window: None,
            resets_at: None
        }
    );
    assert_eq!(
        serde_json::to_value(pause).unwrap(),
        json!({"reason":"limit_reached"})
    );
}

#[test]
fn derived_holds_serialise_in_the_view_but_never_parse_as_stored_holds() {
    use canterlot_hub::model::HoldView;
    let input_id = Uuid::now_v7();
    for (view, expected) in [
        (
            HoldView::MachineNotReconciled,
            json!({"reason":"machine_not_reconciled"}),
        ),
        (
            HoldView::CapabilityMissing { input_id },
            json!({"reason":"capability_missing", "input_id":input_id}),
        ),
    ] {
        assert_eq!(serde_json::to_value(view).unwrap(), expected);
        assert!(serde_json::from_value::<HoldReason>(expected).is_err());
    }
    for reason in ["machine_not_reconciled", "capability_missing"] {
        assert!(reason.parse::<HoldReason>().is_err());
    }
    let view = HoldView::AttachmentsPending {
        input_id,
        reason: Some("hub_unreachable".into()),
    };
    assert_eq!(
        serde_json::to_value(view).unwrap(),
        json!({"reason":"attachments_pending", "input_id":input_id, "machine_reason":"hub_unreachable"})
    );
    for stored in [
        HoldReason::RunInProgress,
        HoldReason::AwaitingUserRequest,
        HoldReason::ContextMaintenance,
        HoldReason::NoLiveExecution,
        HoldReason::ExecutionStarting,
        HoldReason::StorageLow,
        HoldReason::WorkdirBusy {
            conflict_conversation_id: input_id,
        },
        HoldReason::AttachmentsPending { input_id },
        HoldReason::QuotaExhausted {
            provider: "claude".into(),
            window: "five_hour".into(),
            wake_at: "2026-10-27T12:00:00Z".parse().unwrap(),
        },
        HoldReason::WeeklyReserve {
            provider: "codex".into(),
            resets_at: None,
        },
    ] {
        assert_eq!(
            serde_json::to_value(HoldView::from(stored.clone())).unwrap(),
            serde_json::to_value(stored).unwrap()
        );
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn migration_maps_existing_conversations_to_owner(pool: PgPool) -> Result<(), sqlx::Error> {
    // Договор миграции 0022: новые строки чистые по умолчанию; старые значения,
    // отличные от 'clean', стали 'owner'; ссылки UUID требуют закреплённую версию.
    let owner = Uuid::now_v7();
    sqlx::query("insert into owners(id) values($1)")
        .bind(owner)
        .execute(&pool)
        .await?;
    let machine = Uuid::now_v7();
    sqlx::query(
        "insert into machines(id,owner_id,name,availability) values($1,$2,'m','available')",
    )
    .bind(machine)
    .bind(owner)
    .execute(&pool)
    .await?;
    let id: Uuid = sqlx::query_scalar(
        "insert into conversations(id,owner_id,machine_id,harness,workdir) values($1,$2,$3,'synthetic','/tmp/w') returning id",
    )
    .bind(Uuid::now_v7())
    .bind(owner)
    .bind(machine)
    .fetch_one(&pool)
    .await?;
    let (profile, revision): (String, Option<i64>) = sqlx::query_as(
        "select instruction_profile,instruction_preset_revision from conversations where id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(profile, "clean");
    assert_eq!(revision, None);
    // Устаревшее 'owner' проходит проверку; мусор и UUID без версии — нет.
    sqlx::query("update conversations set instruction_profile='owner' where id=$1")
        .bind(id)
        .execute(&pool)
        .await?;
    let bad: Result<(), sqlx::Error> =
        sqlx::query("update conversations set instruction_profile='old-profile' where id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .map(|_| ());
    assert!(bad.is_err());
    let bare: Result<(), sqlx::Error> =
        sqlx::query("update conversations set instruction_profile=$2 where id=$1")
            .bind(id)
            .bind(Uuid::now_v7().to_string())
            .execute(&pool)
            .await
            .map(|_| ());
    assert!(bare.is_err());
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn approved_by_migration_rewrites_only_rows_it_normalizes(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let old = sqlx::migrate::Migrator {
        migrations: canterlot_hub::db::MIGRATOR
            .iter()
            .filter(|migration| migration.version < 25)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(&pool).await?;
    let seed = testkit::seed(&pool).await;
    let execution = Uuid::now_v7();
    sqlx::query(
        "insert into executions (id, conversation_id, generation, state) values ($1, $2, 1, 'running')",
    )
    .bind(execution)
    .bind(seed.conversation_id)
    .execute(&pool)
    .await?;
    let (absent, policy, stale) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    for (block, approved_by) in [
        (absent, None),
        (policy, Some("cli_policy")),
        (stale, Some("owner")),
    ] {
        sqlx::query("insert into blocks (block_id, execution_id, conversation_id, kind, state, rev, approved_by) values ($1, $2, $3, 'text', 'final', 1, $4)")
            .bind(block)
            .bind(execution)
            .bind(seed.conversation_id)
            .bind(approved_by)
            .execute(&pool)
            .await?;
    }
    let version = |block: Uuid| {
        sqlx::query_scalar::<_, String>("select ctid::text from blocks where block_id = $1")
            .bind(block)
            .fetch_one(&pool)
    };
    let (absent_before, policy_before) = (version(absent).await?, version(policy).await?);

    canterlot_hub::db::MIGRATOR.run(&pool).await?;

    assert_eq!(version(absent).await?, absent_before);
    assert_eq!(version(policy).await?, policy_before);
    let normalized: Option<String> =
        sqlx::query_scalar("select approved_by from blocks where block_id = $1")
            .bind(stale)
            .fetch_one(&pool)
            .await?;
    assert_eq!(normalized, None);
    Ok(())
}
