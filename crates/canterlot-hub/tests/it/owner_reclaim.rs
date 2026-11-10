//! Выход из `owner_lost`: то же сопровождение живо, воркер доказал это и пишет факт в CJ, либо
//! группа подтверждённо пуста, машина перезагружена или решил владелец.

use canterlot_hub::{
    delivery::control::{
        self, ResolveOwnerLostOutcome, StopExecutionOutcome, request_stop_execution,
    },
    executions::{self, StartedExecution},
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::ExecutionState as State,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{command, record_body::Body, *};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

struct Run {
    pool: PgPool,
    seed: Seed,
    execution: StartedExecution,
    ingest: Ingest,
    sj: Uuid,
    sj_seq: u64,
    cj: Uuid,
    cj_seq: u64,
}

impl Run {
    async fn new(pool: &PgPool) -> Self {
        let seed = testkit::seed(pool).await;
        let execution =
            executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
                .await
                .unwrap();
        let mut run = Self {
            pool: pool.clone(),
            seed,
            execution,
            ingest: Ingest::new(pool.clone()),
            sj: Uuid::now_v7(),
            sj_seq: 0,
            cj: Uuid::now_v7(),
            cj_seq: 0,
        };
        let opened = Body::JournalOpened(JournalOpened {
            execution_id: run.execution.execution_id.to_string(),
            generation: run.execution.generation as u64,
            supervisor_protocol_version: 1,
        });
        run.send_sj(vec![
            opened,
            run.fact("starting", ""),
            run.fact("running", ""),
        ])
        .await;
        run
    }

    async fn apply(&self, kind: JournalKind, journal: Uuid, first: u64, bodies: Vec<Body>) {
        let last = first + bodies.len() as u64 - 1;
        let batch = JournalRecords {
            journal_id: journal.to_string(),
            records: bodies
                .into_iter()
                .zip(first..)
                .map(|(body, seq)| JournalRecord {
                    seq,
                    body: Some(RecordBody { body: Some(body) }),
                })
                .collect(),
        };
        let outcome = self
            .ingest
            .apply_batch(
                self.seed.machine_id,
                &self.seed.credential_digest(),
                kind,
                &batch,
            )
            .await
            .unwrap();
        assert_eq!(outcome, IngestOutcome::Committed { up_to_seq: last });
    }

    async fn send_sj(&mut self, bodies: Vec<Body>) {
        let first = self.sj_seq + 1;
        self.sj_seq += bodies.len() as u64;
        self.apply(JournalKind::Sj, self.sj, first, bodies).await;
    }

    async fn send_cj(&mut self, bodies: Vec<Body>) {
        let first = self.cj_seq + 1;
        self.cj_seq += bodies.len() as u64;
        self.apply(JournalKind::Cj, self.cj, first, bodies).await;
    }

    fn fact(&self, state: &str, outcome: &str) -> Body {
        Body::ExecutionState(ExecutionState {
            execution_id: self.execution.execution_id.to_string(),
            generation: self.execution.generation as u64,
            state: state.into(),
            outcome: outcome.into(),
            ..Default::default()
        })
    }

    fn reclaim(&self) -> Body {
        self.fact("running", canterlot_proto::OWNER_RECLAIMED)
    }

    async fn lose_owner(&mut self) {
        let lost = self.fact("owner_lost", "");
        self.send_cj(vec![lost]).await;
        assert_eq!(self.state().await.0, "owner_lost");
        assert!(self.pauses().await.contains(&"owner_lost".to_owned()));
    }

    async fn state(&self) -> (String, Option<String>) {
        sqlx::query_as("select state, outcome from executions where id = $1")
            .bind(self.execution.execution_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn pauses(&self) -> Vec<String> {
        let reasons: Value =
            sqlx::query_scalar("select pause_reasons from queue_state where conversation_id = $1")
                .bind(self.seed.conversation_id)
                .fetch_one(&self.pool)
                .await
                .unwrap();
        reasons
            .as_array()
            .unwrap()
            .iter()
            .map(|reason| reason["reason"].as_str().unwrap().to_owned())
            .collect()
    }

    async fn holds(&self) -> Value {
        sqlx::query_scalar("select hold_reasons from queue_state where conversation_id = $1")
            .bind(self.seed.conversation_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn events(&self, kind: &str) -> i64 {
        sqlx::query_scalar("select count(*) from events where execution_id = $1 and kind = $2")
            .bind(self.execution.execution_id)
            .bind(kind)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    fn request(&self, id: &str) -> Body {
        Body::RequestOpened(RequestOpened {
            request_id: id.into(),
            execution_id: self.execution.execution_id.to_string(),
            generation: self.execution.generation as u64,
            kind: "permission".into(),
            payload_json: br#"{"tool_name":"Bash"}"#.to_vec(),
        })
    }

    async fn request_state(&self, id: &str) -> String {
        sqlx::query_scalar("select state from requests where native_request_id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn proven_owner_returns_to_running_and_clears_the_pause(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.lose_owner().await;
    let reclaim = run.reclaim();
    run.send_cj(vec![reclaim]).await;
    assert_eq!(run.state().await, ("running".to_owned(), None));
    assert!(!run.pauses().await.contains(&"owner_lost".to_owned()));
    assert_eq!(run.events("execution_fact_rejected").await, 0);
    let phase = Body::PhaseChanged(PhaseChanged {
        execution_id: run.execution.execution_id.to_string(),
        phase: "awaiting_input".into(),
        ..Default::default()
    });
    run.send_sj(vec![phase]).await;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn repeated_proof_changes_nothing(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.lose_owner().await;
    let reclaim = run.reclaim();
    run.send_cj(vec![reclaim.clone()]).await;
    let events = run.events("execution_state_changed").await;
    run.send_cj(vec![reclaim]).await;
    assert_eq!(run.state().await.0, "running");
    assert_eq!(run.events("execution_state_changed").await, events);
    assert_eq!(run.events("execution_fact_rejected").await, 0);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn execution_lost_while_stopping_returns_to_stopping(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let stopping = run.fact("stopping", "");
    run.send_sj(vec![stopping]).await;
    run.lose_owner().await;
    let reclaim = run.reclaim();
    run.send_cj(vec![reclaim]).await;
    assert_eq!(run.state().await.0, "stopping");
    let stopped = run.fact("stopped", "normal_exit");
    run.send_sj(vec![stopped]).await;
    assert_eq!(run.state().await.0, "stopped");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn plain_running_fact_never_leaves_owner_lost(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.lose_owner().await;
    let running = run.fact("running", "");
    run.send_sj(vec![running.clone()]).await;
    run.send_cj(vec![running]).await;
    assert_eq!(run.state().await.0, "owner_lost");
    assert_eq!(run.events("execution_fact_rejected").await, 2);
    assert!(run.pauses().await.contains(&"owner_lost".to_owned()));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn supervisor_journal_cannot_claim_the_proof(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.lose_owner().await;
    let reclaim = run.reclaim();
    run.send_sj(vec![reclaim]).await;
    assert_eq!(run.state().await.0, "owner_lost");
    assert!(run.pauses().await.contains(&"owner_lost".to_owned()));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn proof_of_another_generation_is_ignored(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.lose_owner().await;
    let Body::ExecutionState(mut reclaim) = run.reclaim() else {
        unreachable!()
    };
    reclaim.generation += 1;
    run.send_cj(vec![Body::ExecutionState(reclaim)]).await;
    assert_eq!(run.state().await.0, "owner_lost");
    assert!(run.pauses().await.contains(&"owner_lost".to_owned()));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terminal_execution_is_never_reclaimed(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.lose_owner().await;
    let failed = run.fact("failed", "supervisor_died");
    run.send_cj(vec![failed]).await;
    let reclaim = run.reclaim();
    run.send_cj(vec![reclaim]).await;
    assert_eq!(
        run.state().await,
        ("failed".to_owned(), Some("supervisor_died".to_owned()))
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn closed_journal_means_a_finished_group(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.lose_owner().await;
    let closed = Body::JournalClosed(JournalClosed {
        execution_id: run.execution.execution_id.to_string(),
        generation: run.execution.generation as u64,
        reason: "normal_exit".into(),
    });
    run.send_sj(vec![closed]).await;
    let reclaim = run.reclaim();
    run.send_cj(vec![reclaim]).await;
    assert_eq!(run.state().await.0, "owner_lost");
    assert!(run.pauses().await.contains(&"owner_lost".to_owned()));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn request_closed_by_the_loss_reopens_when_the_supervisor_repeats_it(
    pool: PgPool,
) -> TestResult {
    let mut run = Run::new(&pool).await;
    let request = run.request("native-1");
    run.send_sj(vec![request.clone()]).await;
    assert_eq!(run.request_state("native-1").await, "open");
    run.lose_owner().await;
    assert_eq!(
        run.request_state("native-1").await,
        "closed_without_decision"
    );
    let reclaim = run.reclaim();
    run.send_cj(vec![reclaim]).await;
    assert_eq!(
        run.request_state("native-1").await,
        "closed_without_decision",
        "only the supervisor knows the request is still waiting"
    );
    run.send_sj(vec![request.clone()]).await;
    assert_eq!(run.request_state("native-1").await, "open");
    assert_eq!(run.events("request_opened").await, 2);
    assert!(
        run.holds()
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|hold| hold["reason"] == "awaiting_user_request"),
        "{:?}",
        run.holds().await
    );
    run.send_sj(vec![request]).await;
    assert_eq!(run.events("request_opened").await, 2, "already open");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn request_closed_by_the_supervisor_stays_closed(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let request = run.request("native-2");
    let closed = Body::RequestClosed(RequestClosed {
        request_id: "native-2".into(),
        reason: "cli_cancelled".into(),
    });
    run.send_sj(vec![request.clone(), closed]).await;
    run.lose_owner().await;
    let reclaim = run.reclaim();
    run.send_cj(vec![reclaim]).await;
    run.send_sj(vec![request]).await;
    assert_eq!(
        run.request_state("native-2").await,
        "closed_without_decision"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn loss_can_repeat_after_a_reclaim(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.lose_owner().await;
    let reclaim = run.reclaim();
    run.send_cj(vec![reclaim]).await;
    run.lose_owner().await;
    let reclaim = run.reclaim();
    run.send_cj(vec![reclaim]).await;
    assert_eq!(run.state().await.0, "running");
    executions::set_state(
        &pool,
        run.execution.execution_id,
        State::Stopping,
        None,
        Uuid::now_v7(),
    )
    .await?;
    Ok(())
}

const CONTINUE_KEY: &str = "owner-exit-resume";

impl Run {
    fn block(id: Uuid) -> Body {
        Body::BlockStarted(BlockStarted {
            block_id: id.to_string(),
            rev: 0,
            kind: "text".into(),
            ..Default::default()
        })
    }

    async fn block_state(&self, id: Uuid) -> (String, Option<String>) {
        sqlx::query_as("select state, incomplete_reason from blocks where block_id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn stop_commands(&self) -> i64 {
        sqlx::query_scalar(
            "select count(*) from commands where conversation_id = $1 and kind = 'stop_execution'",
        )
        .bind(self.seed.conversation_id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn command_result(&self, id: Uuid) -> (String, Value) {
        sqlx::query_as("select delivery_state, result from commands where id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn resolve(&self, key: &str) -> ResolveOwnerLostOutcome {
        control::request_resolve_owner_lost(
            &self.pool,
            self.seed.owner_id,
            self.seed.conversation_id,
            key,
        )
        .await
        .unwrap()
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn confirmed_empty_group_fails_the_execution_and_closes_the_dead_journal(
    pool: PgPool,
) -> TestResult {
    let mut run = Run::new(&pool).await;
    let block = Uuid::now_v7();
    run.send_sj(vec![Run::block(block)]).await;
    run.lose_owner().await;
    let late = Uuid::now_v7();
    run.send_sj(vec![Run::block(late)]).await;
    assert_eq!(
        run.block_state(late).await.0,
        "streaming",
        "late facts land"
    );
    let failed = run.fact("failed", "supervisor_died");
    run.send_cj(vec![failed.clone()]).await;
    assert_eq!(
        run.state().await,
        ("failed".to_owned(), Some("supervisor_died".to_owned()))
    );
    for id in [block, late] {
        assert_eq!(
            run.block_state(id).await,
            ("incomplete".to_owned(), Some("supervisor_died".to_owned()))
        );
    }
    let closed: bool =
        sqlx::query_scalar("select closed from journal_cursors where journal_id = $1")
            .bind(run.sj)
            .fetch_one(&pool)
            .await?;
    assert!(closed);
    assert_eq!(run.events("journal_closed").await, 1);
    run.send_cj(vec![failed]).await;
    assert_eq!(
        run.events("journal_closed").await,
        1,
        "a repeat changes nothing"
    );
    assert_eq!(run.events("execution_fact_rejected").await, 0);
    assert!(run.pauses().await.contains(&"fatal_error".to_owned()));
    assert_eq!(
        executions::live(&mut *pool.acquire().await?, run.seed.conversation_id).await?,
        None
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn after_the_exit_resume_continues_the_native_history_and_clears_the_pauses(
    pool: PgPool,
) -> TestResult {
    let mut run = Run::new(&pool).await;
    sqlx::query("update executions set native_conversation_id = 'native-id' where id = $1")
        .bind(run.execution.execution_id)
        .execute(&pool)
        .await?;
    testkit::seed_recorded_turn(&pool, run.seed.conversation_id).await;
    run.lose_owner().await;
    let refused = executions::start_with_key(
        &pool,
        run.seed.owner_id,
        run.seed.conversation_id,
        "refused-while-lost",
        (true, true),
        Uuid::now_v7(),
        #[cfg(feature = "fault-injection")]
        &Default::default(),
    )
    .await;
    assert!(refused.is_err(), "E-5: no new execution while lost");
    let failed = run.fact("failed", "supervisor_died");
    run.send_cj(vec![failed]).await;
    let resumed = executions::start_with_key(
        &pool,
        run.seed.owner_id,
        run.seed.conversation_id,
        CONTINUE_KEY,
        (true, true),
        Uuid::now_v7(),
        #[cfg(feature = "fault-injection")]
        &Default::default(),
    )
    .await?;
    assert_eq!(resumed.generation, run.execution.generation + 1);
    let payload: Value = sqlx::query_scalar(
        "select payload from commands where execution_id = $1 and kind = 'start_execution'",
    )
    .bind(resumed.execution_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(payload["native_conversation_id"], "native-id");
    assert_eq!(run.pauses().await, Vec::<String>::new());
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reboot_exits_any_live_state_with_the_real_reason(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.lose_owner().await;
    let rebooted = run.fact("stopped", "machine_rebooted");
    run.send_cj(vec![rebooted]).await;
    assert_eq!(
        run.state().await,
        ("stopped".to_owned(), Some("machine_rebooted".to_owned()))
    );
    assert!(run.pauses().await.contains(&"process_exited".to_owned()));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reboot_exits_a_running_execution_without_passing_through_the_loss(
    pool: PgPool,
) -> TestResult {
    let mut run = Run::new(&pool).await;
    let rebooted = run.fact("stopped", "machine_rebooted");
    run.send_cj(vec![rebooted]).await;
    assert_eq!(run.state().await.0, "stopped");
    assert_eq!(run.events("execution_fact_rejected").await, 0);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reboot_exits_an_execution_the_hub_still_sees_as_created(pool: PgPool) -> TestResult {
    let seed = testkit::seed(&pool).await;
    let execution =
        executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7()).await?;
    let mut conn = pool.acquire().await?;
    executions::set_state_in_tx(
        &mut conn,
        execution.execution_id,
        State::Stopped,
        Some("machine_rebooted"),
        Uuid::now_v7(),
    )
    .await?;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stop_of_a_lost_execution_leads_to_the_resolve_action(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.lose_owner().await;
    let outcome = request_stop_execution(
        &pool,
        run.seed.owner_id,
        run.seed.conversation_id,
        true,
        "stop-lost",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(outcome, StopExecutionOutcome::OwnerLost);
    assert_eq!(run.stop_commands().await, 0, "nothing the worker could do");
    assert!(!run.pauses().await.contains(&"user_stop".to_owned()));
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn resolve_is_one_command_and_ends_with_the_exit(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    assert_eq!(
        run.resolve("early").await,
        ResolveOwnerLostOutcome::NotOwnerLost
    );
    run.lose_owner().await;
    let ResolveOwnerLostOutcome::Requested { command_id } = run.resolve("first").await else {
        panic!("a lost execution takes the decision");
    };
    assert_eq!(
        run.resolve("first").await,
        ResolveOwnerLostOutcome::Requested { command_id },
        "same key, same answer"
    );
    assert_eq!(
        run.resolve("second").await,
        ResolveOwnerLostOutcome::AlreadyRequested { command_id }
    );
    let failed = run.fact("failed", "admin_resolved");
    run.send_cj(vec![failed]).await;
    let (state, result) = run.command_result(command_id).await;
    assert_eq!(state, "completed");
    assert_eq!(result["outcome"], "admin_resolved");
    assert_eq!(
        run.resolve("late").await,
        ResolveOwnerLostOutcome::NotOwnerLost
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn resolve_ends_with_a_reclaim_when_the_supervisor_was_alive(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.lose_owner().await;
    let ResolveOwnerLostOutcome::Requested { command_id } = run.resolve("first").await else {
        panic!("a lost execution takes the decision");
    };
    let reclaim = run.reclaim();
    run.send_cj(vec![reclaim]).await;
    assert_eq!(run.state().await.0, "running");
    let (state, result) = run.command_result(command_id).await;
    assert_eq!(state, "completed");
    assert_eq!(result["outcome"], canterlot_proto::OWNER_RECLAIMED);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn resolve_goes_only_to_a_session_that_agreed_to_it(pool: PgPool) -> TestResult {
    use canterlot_hub::{
        config::HubTimers,
        grpc::KNOWN_OPTIONAL_CAPABILITIES,
        outbox::{self, CommandRow, OutboxError},
        session::Registry,
    };
    assert!(KNOWN_OPTIONAL_CAPABILITIES.contains(&"owner_exit_v1"));
    let mut run = Run::new(&pool).await;
    run.lose_owner().await;
    let ResolveOwnerLostOutcome::Requested { command_id } = run.resolve("first").await else {
        panic!("a lost execution takes the decision");
    };
    let row: CommandRow = sqlx::query_as("select * from commands where id = $1")
        .bind(command_id)
        .fetch_one(&pool)
        .await?;
    let registry = Registry::new(HubTimers::default());
    let old = testkit::fake_session(&registry, row.machine_id, &["pilot_v1"], true);
    assert!(matches!(
        outbox::to_proto_for_session(&row, &registry),
        Err(OutboxError::CapabilityMissing)
    ));
    drop(old);
    let registry = Registry::new(HubTimers::default());
    let _new = testkit::fake_session(&registry, row.machine_id, &["owner_exit_v1"], true);
    assert!(matches!(
        outbox::to_proto_for_session(&row, &registry)?.kind,
        Some(command::Kind::ResolveOwnerLost(_))
    ));
    Ok(())
}
