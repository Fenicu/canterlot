use canterlot_hub::{
    executions::{self, ExecutionError, StartedExecution},
    model::{ExecutionState, HoldReason, PauseReason, RunSlot},
    testkit::{self, Seed},
};
use serde_json::Value;
use sqlx::{PgPool, types::Json};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, PartialEq, sqlx::FromRow)]
struct Queue {
    pause_reasons: Json<Vec<PauseReason>>,
    hold_reasons: Json<Vec<HoldReason>>,
    run_slot: String,
    run_slot_input_id: Option<Uuid>,
    run_slot_attempt: Option<i32>,
    next_input_seq: i64,
    closed: bool,
}

async fn queue(pool: &PgPool, conversation_id: Uuid) -> Result<Queue, sqlx::Error> {
    sqlx::query_as(
        "select pause_reasons, hold_reasons, run_slot, run_slot_input_id,
                run_slot_attempt, next_input_seq, closed
         from queue_state where conversation_id = $1",
    )
    .bind(conversation_id)
    .fetch_one(pool)
    .await
}

async fn execution(pool: &PgPool, id: Uuid) -> Result<(String, Option<String>), sqlx::Error> {
    sqlx::query_as("select state, outcome from executions where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
}

async fn counts(pool: &PgPool) -> Result<(i64, i64, i64, i64), sqlx::Error> {
    sqlx::query_as(
        "select (select count(*) from executions), (select count(*) from commands),
                (select count(*) from events), (select count(*) from scope_counters)",
    )
    .fetch_one(pool)
    .await
}

async fn at_state(
    pool: &PgPool,
    state: ExecutionState,
) -> Result<(Seed, StartedExecution), ExecutionError> {
    let seed = testkit::seed(pool).await;
    let started =
        executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await?;
    let path: &[ExecutionState] = match state {
        ExecutionState::Created => &[],
        ExecutionState::Starting => &[ExecutionState::Starting],
        ExecutionState::Running => &[ExecutionState::Starting, ExecutionState::Running],
        ExecutionState::Stopping => &[
            ExecutionState::Starting,
            ExecutionState::Running,
            ExecutionState::Stopping,
        ],
        ExecutionState::OwnerLost => &[ExecutionState::Starting, ExecutionState::OwnerLost],
        _ => panic!("expected a nonterminal state"),
    };
    for &next in path {
        executions::set_state(pool, started.execution_id, next, None, Uuid::now_v7()).await?;
    }
    Ok((seed, started))
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_creates_generation_and_command(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let correlation_id = Uuid::now_v7();
    let started =
        executions::start(&pool, seed.owner_id, seed.conversation_id, correlation_id).await?;
    assert_eq!(started.generation, 1);
    assert_eq!(started.execution_id.get_version_num(), 7);
    assert_eq!(started.command_id.get_version_num(), 7);
    assert_eq!(
        execution(&pool, started.execution_id).await?,
        ("created".into(), None)
    );
    let command: (Uuid, Uuid, Uuid, i64, String, String, Value) = sqlx::query_as(
        "select machine_id, conversation_id, execution_id, execution_generation,
                kind, delivery_state, payload from commands where id = $1",
    )
    .bind(started.command_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(command.0, seed.machine_id);
    assert_eq!(command.1, seed.conversation_id);
    assert_eq!(command.2, started.execution_id);
    assert_eq!(command.3, 1);
    assert_eq!(command.4, "start_execution");
    assert_eq!(command.5, "pending");
    assert_eq!(command.6["harness"], "synthetic");
    assert_eq!(command.6["workdir"], "/tmp/w");
    let queue = queue(&pool, seed.conversation_id).await?;
    assert!(
        queue
            .hold_reasons
            .0
            .contains(&HoldReason::ExecutionStarting)
    );
    assert!(!queue.hold_reasons.0.contains(&HoldReason::NoLiveExecution));
    assert!(queue.pause_reasons.0.is_empty());
    let event: (String, i64, String, Uuid, Uuid, Uuid, Value) = sqlx::query_as(
        "select scope, scope_seq, kind, conversation_id, execution_id, correlation_id,
                payload from events where kind = 'execution_state_changed'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(event.0, format!("conversation:{}", seed.conversation_id));
    assert_eq!(event.1, 2);
    assert_eq!(event.2, "execution_state_changed");
    assert_eq!(event.3, seed.conversation_id);
    assert_eq!(event.4, started.execution_id);
    assert_eq!(event.5, correlation_id);
    assert_eq!(event.6["state"], "created");
    assert_eq!(event.6["generation"], 1);
    let mut conn = pool.acquire().await?;
    assert_eq!(
        executions::live(&mut conn, seed.conversation_id).await?,
        Some((started.execution_id, 1, ExecutionState::Created))
    );
    assert_eq!(executions::live(&mut conn, Uuid::now_v7()).await?, None);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn second_live_start_is_refused(pool: PgPool) -> TestResult {
    for state in [
        ExecutionState::Created,
        ExecutionState::Starting,
        ExecutionState::Running,
        ExecutionState::Stopping,
        ExecutionState::OwnerLost,
    ] {
        let (seed, first) = at_state(&pool, state).await?;
        let before = counts(&pool).await?;
        let queue_before = queue(&pool, seed.conversation_id).await?;
        assert!(matches!(
            executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await,
            Err(ExecutionError::AlreadyLive { execution_id }) if execution_id == first.execution_id
        ));
        assert_eq!(counts(&pool).await?, before);
        assert_eq!(queue(&pool, seed.conversation_id).await?, queue_before);
        let mut conn = pool.acquire().await?;
        assert_eq!(
            executions::live(&mut conn, seed.conversation_id).await?,
            Some((first.execution_id, 1, state))
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn running_clears_start_holds(pool: PgPool) -> TestResult {
    let (seed, started) = at_state(&pool, ExecutionState::Starting).await?;
    let holds = vec![
        HoldReason::ExecutionStarting,
        HoldReason::NoLiveExecution,
        HoldReason::StorageLow,
        HoldReason::AttachmentsPending {
            input_id: Uuid::now_v7(),
        },
    ];
    sqlx::query(
        "update queue_state set pause_reasons = $2, hold_reasons = $3 where conversation_id = $1",
    )
    .bind(seed.conversation_id)
    .bind(Json(vec![PauseReason::UserStop]))
    .bind(Json(holds.clone()))
    .execute(&pool)
    .await?;
    let correlation_id = Uuid::now_v7();
    executions::set_state(
        &pool,
        started.execution_id,
        ExecutionState::Running,
        None,
        correlation_id,
    )
    .await?;
    let queue = queue(&pool, seed.conversation_id).await?;
    assert_eq!(queue.hold_reasons.0, holds[2..]);
    assert_eq!(queue.pause_reasons.0, [PauseReason::UserStop]);
    let event: (String, Value) =
        sqlx::query_as("select kind, payload from events where correlation_id = $1 and kind = 'execution_state_changed'")
            .bind(correlation_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(event.0, "execution_state_changed");
    assert_eq!(event.1["state"], "running");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn crash_pauses_queue(pool: PgPool) -> TestResult {
    for (terminal, outcome, added_pause) in [
        (
            ExecutionState::Failed,
            "crashed",
            Some(PauseReason::FatalError),
        ),
        (
            ExecutionState::Stopped,
            "normal_exit",
            Some(PauseReason::ProcessExited),
        ),
        (ExecutionState::Stopped, "user_stop", None),
    ] {
        let (seed, started) = at_state(&pool, ExecutionState::Running).await?;
        sqlx::query("update queue_state set pause_reasons = $2 where conversation_id = $1")
            .bind(seed.conversation_id)
            .bind(Json(vec![PauseReason::UserStop]))
            .execute(&pool)
            .await?;
        executions::set_state(
            &pool,
            started.execution_id,
            ExecutionState::Stopping,
            None,
            Uuid::now_v7(),
        )
        .await?;
        let correlation_id = Uuid::now_v7();
        executions::set_state(
            &pool,
            started.execution_id,
            terminal,
            Some(outcome),
            correlation_id,
        )
        .await?;
        let queue = queue(&pool, seed.conversation_id).await?;
        let mut expected = vec![PauseReason::UserStop];
        expected.extend(added_pause);
        assert_eq!(queue.pause_reasons.0, expected);
        assert_eq!(queue.hold_reasons.0, [HoldReason::NoLiveExecution]);
        assert_eq!(
            execution(&pool, started.execution_id).await?,
            (terminal.as_str().into(), Some(outcome.into()))
        );
        let payload: Value =
            sqlx::query_scalar("select payload from events where correlation_id = $1 and kind = 'execution_state_changed'")
                .bind(correlation_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(payload["state"], terminal.as_str());
        assert_eq!(payload["outcome"], outcome);
        let mut conn = pool.acquire().await?;
        assert_eq!(
            executions::live(&mut conn, seed.conversation_id).await?,
            None
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn illegal_transition_rejected(pool: PgPool) -> TestResult {
    let (seed, started) = at_state(&pool, ExecutionState::Created).await?;
    let before = queue(&pool, seed.conversation_id).await?;
    for (to, outcome) in [
        (ExecutionState::Stopped, Some("normal_exit")),
        (ExecutionState::Stopped, Some("machine_rebooted")),
        (ExecutionState::Failed, Some("crashed")),
        (ExecutionState::Running, None),
    ] {
        assert!(matches!(
            executions::set_state(&pool, started.execution_id, to, outcome, Uuid::now_v7()).await,
            Err(ExecutionError::InvalidTransition { from: ExecutionState::Created, to: actual }) if actual == to
        ));
    }
    assert_eq!(queue(&pool, seed.conversation_id).await?, before);
    assert_eq!(counts(&pool).await?, (1, 1, 2, 1));
    executions::set_state(
        &pool,
        started.execution_id,
        ExecutionState::Stopped,
        Some("start_cancelled"),
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(
        execution(&pool, started.execution_id).await?,
        ("stopped".into(), Some("start_cancelled".into()))
    );
    assert_eq!(
        queue(&pool, seed.conversation_id).await?.hold_reasons.0,
        [HoldReason::NoLiveExecution]
    );

    let (_, running) = at_state(&pool, ExecutionState::Running).await?;
    assert!(matches!(
        executions::set_state(
            &pool,
            running.execution_id,
            ExecutionState::Created,
            None,
            Uuid::now_v7()
        )
        .await,
        Err(ExecutionError::InvalidTransition {
            from: ExecutionState::Running,
            to: ExecutionState::Created
        })
    ));
    let (_, failed) = at_state(&pool, ExecutionState::Created).await?;
    executions::set_state(
        &pool,
        failed.execution_id,
        ExecutionState::Failed,
        Some("start_rejected"),
        Uuid::now_v7(),
    )
    .await?;
    for (id, from) in [
        (started.execution_id, ExecutionState::Stopped),
        (failed.execution_id, ExecutionState::Failed),
    ] {
        let before = counts(&pool).await?;
        for to in [
            ExecutionState::Created,
            ExecutionState::Starting,
            ExecutionState::Running,
            ExecutionState::Stopping,
            ExecutionState::Stopped,
            ExecutionState::Failed,
            ExecutionState::OwnerLost,
        ] {
            assert!(matches!(
                executions::set_state(&pool, id, to, None, Uuid::now_v7()).await,
                Err(ExecutionError::InvalidTransition { from: actual_from, to: actual_to }) if actual_from == from && actual_to == to
            ));
        }
        assert_eq!(counts(&pool).await?, before);
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terminal_frees_running_slot_only(pool: PgPool) -> TestResult {
    for slot in [
        RunSlot::Running {
            input_id: Uuid::now_v7(),
        },
        RunSlot::Dispatching {
            input_id: Uuid::now_v7(),
            attempt: 3,
        },
    ] {
        let (seed, started) = at_state(&pool, ExecutionState::Stopping).await?;
        let (name, input_id, attempt) = slot.to_columns();
        sqlx::query("update queue_state set run_slot = $2, run_slot_input_id = $3, run_slot_attempt = $4, hold_reasons = $5 where conversation_id = $1")
            .bind(seed.conversation_id)
            .bind(name)
            .bind(input_id)
            .bind(attempt)
            .bind(Json(vec![HoldReason::RunInProgress, HoldReason::StorageLow]))
            .execute(&pool)
            .await?;
        executions::set_state(
            &pool,
            started.execution_id,
            ExecutionState::Stopped,
            Some("normal_exit"),
            Uuid::now_v7(),
        )
        .await?;
        let queue = queue(&pool, seed.conversation_id).await?;
        let actual = RunSlot::from_columns(
            &queue.run_slot,
            queue.run_slot_input_id,
            queue.run_slot_attempt,
        )?;
        let expected = if matches!(slot, RunSlot::Running { .. }) {
            RunSlot::Free
        } else {
            slot
        };
        assert_eq!(actual, expected);
        assert_eq!(
            queue.hold_reasons.0.contains(&HoldReason::RunInProgress),
            actual != RunSlot::Free
        );
        assert!(queue.hold_reasons.0.contains(&HoldReason::StorageLow));
        assert!(queue.hold_reasons.0.contains(&HoldReason::NoLiveExecution));
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn resume_after_stop_creates_next_generation_without_clearing_pause(
    pool: PgPool,
) -> TestResult {
    let (seed, first) = at_state(&pool, ExecutionState::Stopping).await?;
    sqlx::query("update queue_state set pause_reasons = $2 where conversation_id = $1")
        .bind(seed.conversation_id)
        .bind(Json(vec![PauseReason::UserStop]))
        .execute(&pool)
        .await?;
    executions::set_state(
        &pool,
        first.execution_id,
        ExecutionState::Stopped,
        Some("user_stop"),
        Uuid::now_v7(),
    )
    .await?;
    let second =
        executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await?;
    assert_eq!(second.generation, 2);
    assert_ne!(second.execution_id, first.execution_id);
    assert_ne!(second.command_id, first.command_id);
    assert_eq!(
        queue(&pool, seed.conversation_id).await?.pause_reasons.0,
        [PauseReason::UserStop]
    );
    let command_generation: i64 =
        sqlx::query_scalar("select execution_generation from commands where id = $1")
            .bind(second.command_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(command_generation, 2);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_lost_and_boot_changed_paths(pool: PgPool) -> TestResult {
    for from in [
        ExecutionState::Starting,
        ExecutionState::Running,
        ExecutionState::Stopping,
        ExecutionState::OwnerLost,
    ] {
        let (seed, started) = at_state(&pool, from).await?;
        if from != ExecutionState::Stopping {
            for outcome in [None, Some("normal_exit"), Some("user_stop")] {
                assert!(matches!(
                    executions::set_state(
                        &pool,
                        started.execution_id,
                        ExecutionState::Stopped,
                        outcome,
                        Uuid::now_v7()
                    )
                    .await,
                    Err(ExecutionError::InvalidTransition { .. })
                ));
            }
        }
        executions::set_state(
            &pool,
            started.execution_id,
            ExecutionState::Stopped,
            Some("machine_rebooted"),
            Uuid::now_v7(),
        )
        .await?;
        let pauses = queue(&pool, seed.conversation_id).await?.pause_reasons.0;
        assert!(pauses.contains(&PauseReason::ProcessExited));
        assert_eq!(
            pauses.contains(&PauseReason::OwnerLost),
            from == ExecutionState::OwnerLost
        );
    }
    for from in [
        ExecutionState::Starting,
        ExecutionState::Running,
        ExecutionState::Stopping,
    ] {
        for outcome in ["supervisor_died", "admin_resolved"] {
            let (seed, started) = at_state(&pool, from).await?;
            let before = queue(&pool, seed.conversation_id).await?;
            executions::set_state(
                &pool,
                started.execution_id,
                ExecutionState::OwnerLost,
                None,
                Uuid::now_v7(),
            )
            .await?;
            let lost = queue(&pool, seed.conversation_id).await?;
            assert_eq!(lost.pause_reasons.0, [PauseReason::OwnerLost]);
            assert_eq!(lost.run_slot, before.run_slot);
            assert!(matches!(
                executions::set_state(
                    &pool,
                    started.execution_id,
                    ExecutionState::Running,
                    None,
                    Uuid::now_v7()
                )
                .await,
                Err(ExecutionError::InvalidTransition { .. })
            ));
            executions::set_state(
                &pool,
                started.execution_id,
                ExecutionState::Failed,
                Some(outcome),
                Uuid::now_v7(),
            )
            .await?;
            assert_eq!(
                queue(&pool, seed.conversation_id).await?.pause_reasons.0,
                [PauseReason::OwnerLost, PauseReason::FatalError]
            );
        }
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn workdir_wait_and_start_failure(pool: PgPool) -> TestResult {
    let (seed, started) = at_state(&pool, ExecutionState::Created).await?;
    let before = queue(&pool, seed.conversation_id).await?;
    executions::set_state(
        &pool,
        started.execution_id,
        ExecutionState::Created,
        None,
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(queue(&pool, seed.conversation_id).await?, before);
    assert_eq!(counts(&pool).await?, (1, 1, 3, 1));
    executions::set_state(
        &pool,
        started.execution_id,
        ExecutionState::Starting,
        None,
        Uuid::now_v7(),
    )
    .await?;
    executions::set_state(
        &pool,
        started.execution_id,
        ExecutionState::Stopping,
        None,
        Uuid::now_v7(),
    )
    .await?;
    executions::set_state(
        &pool,
        started.execution_id,
        ExecutionState::Failed,
        Some("start_failed"),
        Uuid::now_v7(),
    )
    .await?;
    assert_eq!(
        queue(&pool, seed.conversation_id).await?.pause_reasons.0,
        [PauseReason::FatalError]
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_checks_owner_and_closed_conversation(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    for (owner_id, conversation_id) in [
        (seed.owner_id, Uuid::now_v7()),
        (Uuid::now_v7(), seed.conversation_id),
    ] {
        assert!(matches!(
            executions::start(&pool, owner_id, conversation_id, Uuid::now_v7()).await,
            Err(ExecutionError::ConversationNotFound)
        ));
    }
    sqlx::query("update queue_state set closed = true where conversation_id = $1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await?;
    assert!(matches!(
        executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await,
        Err(ExecutionError::ConversationClosed)
    ));
    assert_eq!(counts(&pool).await?, (0, 0, 0, 0));
    assert!(matches!(
        executions::set_state(
            &pool,
            Uuid::now_v7(),
            ExecutionState::Running,
            None,
            Uuid::now_v7()
        )
        .await,
        Err(ExecutionError::Db(sqlx::Error::RowNotFound))
    ));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_starts_create_one_live_execution(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let (first, second) = tokio::join!(
        executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()),
        executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
    );
    let (Ok(winner), Err(ExecutionError::AlreadyLive { execution_id })) = (if first.is_ok() {
        (first, second)
    } else {
        (second, first)
    }) else {
        panic!("expected one start and one AlreadyLive");
    };
    assert_eq!(winner.execution_id, execution_id);
    assert_eq!(counts(&pool).await?, (1, 1, 2, 1));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn event_failure_rolls_back_all_mutations(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let initial = queue(&pool, seed.conversation_id).await?;
    sqlx::query("alter table events add constraint reject_execution_events check (kind <> 'execution_state_changed')")
        .execute(&pool)
        .await?;
    assert!(matches!(
        executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await,
        Err(ExecutionError::Db(_))
    ));
    assert_eq!(counts(&pool).await?, (0, 0, 0, 0));
    assert_eq!(queue(&pool, seed.conversation_id).await?, initial);
    sqlx::query("alter table events drop constraint reject_execution_events")
        .execute(&pool)
        .await?;
    let (seed, started) = at_state(&pool, ExecutionState::Stopping).await?;
    sqlx::query("update queue_state set run_slot = 'running', run_slot_input_id = $2 where conversation_id = $1")
        .bind(seed.conversation_id)
        .bind(Uuid::now_v7())
        .execute(&pool)
        .await?;
    let before = queue(&pool, seed.conversation_id).await?;
    let counts_before = counts(&pool).await?;
    sqlx::query("alter table events add constraint reject_execution_events check (kind <> 'execution_state_changed') not valid")
        .execute(&pool)
        .await?;
    assert!(matches!(
        executions::set_state(
            &pool,
            started.execution_id,
            ExecutionState::Failed,
            Some("crashed"),
            Uuid::now_v7()
        )
        .await,
        Err(ExecutionError::Db(_))
    ));
    assert_eq!(
        execution(&pool, started.execution_id).await?,
        ("stopping".into(), None)
    );
    assert_eq!(queue(&pool, seed.conversation_id).await?, before);
    assert_eq!(counts(&pool).await?, counts_before);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_payload_carries_pinned_revision_text_after_edit_and_delete(
    pool: PgPool,
) -> TestResult {
    use canterlot_hub::{conversations, presets};
    let seed = testkit::seed(&pool).await;
    testkit::seed_catalog(&pool, seed.machine_id).await;
    let registry =
        canterlot_hub::session::Registry::new(canterlot_hub::config::HubTimers::default());
    let created = presets::create(
        &pool,
        seed.owner_id,
        "t3-exec",
        presets::NewPreset {
            title: "Исполнитель".into(),
            description: None,
            text: "текст A".into(),
        },
        presets::Actor::Owner,
        Uuid::now_v7(),
    )
    .await?;
    let preset_id = created.summary.id.clone();
    let conversation = conversations::create(
        &pool,
        "t3-exec-conv",
        conversations::NewConversation {
            owner_id: seed.owner_id,
            machine_id: seed.machine_id,
            harness: "synthetic".into(),
            workdir: "/tmp/w".into(),
            model: "synthetic-small".into(),
            effort: "low".into(),
            permission_mode: None,
            instruction_profile: Some(preset_id.clone()),
            title: None,
            color: None,
            group_ids: vec![],
            crunch: Default::default(),
            start: false,
        },
        &canterlot_hub::config::LimitsConfig::default(),
        Uuid::now_v7(),
        &registry,
    )
    .await?
    .conversation_id;
    async fn payload_of(pool: &PgPool, command_id: Uuid) -> Value {
        sqlx::query_scalar("select payload from commands where id=$1")
            .bind(command_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }
    let first = executions::start(&pool, seed.owner_id, conversation, Uuid::now_v7()).await?;
    let payload = payload_of(&pool, first.command_id).await;
    assert_eq!(payload["instructions"]["mode"], "preset");
    assert_eq!(payload["instructions"]["preset_id"], preset_id);
    assert_eq!(payload["instructions"]["revision"], 1);
    assert_eq!(payload["instructions"]["text"], "текст A");
    assert_eq!(payload["instructions"]["title"], created.summary.title);
    executions::set_state(
        &pool,
        first.execution_id,
        ExecutionState::Stopped,
        Some("start_cancelled"),
        Uuid::now_v7(),
    )
    .await?;
    // Правка пресета не меняет закреплённый снимок новых запусков.
    presets::update(
        &pool,
        seed.owner_id,
        preset_id.parse()?,
        "t3-exec-bump",
        presets::PresetPatch {
            title: None,
            description: None,
            text: Some("текст B".into()),
            base_revision: 1,
        },
        presets::Actor::Owner,
        Uuid::now_v7(),
    )
    .await?;
    let second = executions::start(&pool, seed.owner_id, conversation, Uuid::now_v7()).await?;
    let payload = payload_of(&pool, second.command_id).await;
    assert_eq!(payload["instructions"]["revision"], 1);
    assert_eq!(payload["instructions"]["text"], "текст A");
    executions::set_state(
        &pool,
        second.execution_id,
        ExecutionState::Stopped,
        Some("start_cancelled"),
        Uuid::now_v7(),
    )
    .await?;
    // Удалённый пресет: снимок работает со своей копией.
    presets::delete(
        &pool,
        seed.owner_id,
        preset_id.parse()?,
        "t3-exec-del",
        Uuid::now_v7(),
    )
    .await?;
    let third = executions::start(&pool, seed.owner_id, conversation, Uuid::now_v7()).await?;
    let payload = payload_of(&pool, third.command_id).await;
    assert_eq!(payload["instructions"]["revision"], 1);
    assert_eq!(payload["instructions"]["text"], "текст A");
    Ok(())
}
