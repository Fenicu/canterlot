//! Выход из `owner_lost`: то же сопровождение живо, воркер доказал это и пишет факт в CJ.

use canterlot_hub::{
    executions::{self, StartedExecution},
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::ExecutionState as State,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{record_body::Body, *};
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
