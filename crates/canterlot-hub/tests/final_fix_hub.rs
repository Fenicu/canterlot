use canterlot_hub::{
    executions::{self, StartedExecution},
    grpc::Ingest as _,
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
    seed: Seed,
    execution: StartedExecution,
    ingest: Ingest,
    journal: Uuid,
    seq: u64,
}
impl Run {
    async fn new(pool: &PgPool) -> Self {
        let seed = testkit::seed(pool).await;
        let execution =
            executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
                .await
                .unwrap();
        Self {
            seed,
            execution,
            ingest: Ingest::new(pool.clone()),
            journal: Uuid::now_v7(),
            seq: 0,
        }
    }
    fn batch(&mut self, bodies: Vec<Body>) -> JournalRecords {
        JournalRecords {
            journal_id: self.journal.to_string(),
            records: bodies
                .into_iter()
                .map(|body| {
                    self.seq += 1;
                    JournalRecord {
                        seq: self.seq,
                        body: Some(RecordBody { body: Some(body) }),
                    }
                })
                .collect(),
        }
    }
    async fn send(&mut self, bodies: Vec<Body>) {
        let batch = self.batch(bodies);
        assert_eq!(
            self.ingest
                .apply_batch(
                    self.seed.machine_id,
                    &self.seed.credential_digest(),
                    JournalKind::Sj,
                    &batch
                )
                .await
                .unwrap(),
            IngestOutcome::Committed {
                up_to_seq: self.seq
            }
        );
    }
    fn state(&self, state: &str, outcome: &str) -> Body {
        Body::ExecutionState(ExecutionState {
            execution_id: self.execution.execution_id.to_string(),
            generation: self.execution.generation as u64,
            state: state.into(),
            outcome: outcome.into(),
            ..Default::default()
        })
    }
    fn opened(&self) -> Body {
        Body::JournalOpened(JournalOpened {
            execution_id: self.execution.execution_id.to_string(),
            generation: self.execution.generation as u64,
            supervisor_protocol_version: 1,
        })
    }
    fn report(&self, state: &str, outcome: &str) -> ReconcileReport {
        ReconcileReport {
            executions: vec![ExecutionObservation {
                execution_id: self.execution.execution_id.to_string(),
                generation: self.execution.generation as u64,
                state: state.into(),
                outcome: outcome.into(),
                sj_journal_id: self.journal.to_string(),
                ..Default::default()
            }],
        }
    }
}
fn start(id: Uuid) -> Body {
    Body::BlockStarted(BlockStarted {
        block_id: id.to_string(),
        kind: "text".into(),
        ..Default::default()
    })
}
fn delta(id: Uuid, rev: u64, text: String) -> Body {
    Body::BlockDelta(BlockDelta {
        block_id: id.to_string(),
        base_rev: rev - 1,
        rev,
        append: text,
    })
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wave2_n2_sj_batch_and_stop_share_lock_order(pool: PgPool) -> TestResult {
    use canterlot_hub::delivery::control;
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    let mut run = Run::new(&pool).await;
    let block = Uuid::now_v7();
    run.send(vec![
        run.opened(),
        run.state("starting", ""),
        run.state("running", ""),
        start(block),
    ])
    .await;
    sqlx::raw_sql("create function gate_stop_event() returns trigger language plpgsql as $$ begin perform pg_advisory_xact_lock(20927); return new; end $$;
        create trigger gate_stop_event before update on queue_state for each row execute function gate_stop_event()")
        .execute(&pool).await?;
    for round in 0..50 {
        sqlx::query("update queue_state set pause_reasons = '[]' where conversation_id = $1")
            .bind(run.seed.conversation_id)
            .execute(&pool)
            .await?;
        let mut gate = pool.acquire().await?;
        sqlx::query("select pg_advisory_lock(20927)")
            .execute(&mut *gate)
            .await?;
        let stop_pool = pool.clone();
        let owner = run.seed.owner_id;
        let conversation = run.seed.conversation_id;
        let stop = tokio::spawn(async move {
            if round % 2 == 0 {
                control::request_stop(
                    &stop_pool,
                    owner,
                    conversation,
                    &Uuid::now_v7().to_string(),
                    Uuid::now_v7(),
                )
                .await
                .map(|_| ())
            } else {
                control::request_stop_execution(
                    &stop_pool,
                    owner,
                    conversation,
                    false,
                    &Uuid::now_v7().to_string(),
                    Uuid::now_v7(),
                )
                .await
                .map(|_| ())
            }
        });
        timeout(Duration::from_secs(5), async {
            loop {
                if sqlx::query_scalar::<_, bool>("select exists(select 1 from pg_locks l join pg_stat_activity a using(pid) where a.datname = current_database() and l.locktype = 'advisory' and not l.granted)").fetch_one(&pool).await.unwrap() { break; }
                sleep(Duration::from_millis(2)).await;
            }
        }).await.expect("stop did not reach the queue gate");
        let batch = run.batch(vec![
            delta(block, round + 1, "x".into()),
            Body::PhaseChanged(PhaseChanged {
                execution_id: run.execution.execution_id.to_string(),
                phase: "thinking".into(),
                ..Default::default()
            }),
        ]);
        let ingest = Ingest::new(pool.clone());
        let machine = run.seed.machine_id;
        let credential = run.seed.credential_digest();
        let send = tokio::spawn(async move {
            ingest
                .apply_batch(machine, &credential, JournalKind::Sj, &batch)
                .await
        });
        timeout(Duration::from_secs(5), async {
            loop {
                if sqlx::query_scalar::<_, bool>("select exists(select 1 from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock' and query like '%queue_state%' and query like '%for update%' and query not like '%pg_stat_activity%')").fetch_one(&pool).await.unwrap() { break; }
                sleep(Duration::from_millis(2)).await;
            }
        }).await.expect("SJ did not reach its queue lock");
        sqlx::query("select pg_advisory_unlock(20927)")
            .execute(&mut *gate)
            .await?;
        let (stop, send) =
            timeout(Duration::from_secs(5), async { tokio::join!(stop, send) }).await?;
        let stop = stop?;
        let send = send?;
        let deadlocks: i64 = sqlx::query_scalar(
            "select deadlocks from pg_stat_database where datname = current_database()",
        )
        .fetch_one(&pool)
        .await?;
        assert!(
            stop.is_ok() && send.is_ok(),
            "iteration {round}: stop={stop:?}, SJ={send:?}, PostgreSQL deadlocks={deadlocks}"
        );
        assert_eq!(
            send?,
            IngestOutcome::Committed { up_to_seq: run.seq },
            "iteration {round}"
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn created_can_lose_owner_before_first_sj(pool: PgPool) -> TestResult {
    let run = Run::new(&pool).await;
    executions::set_state(
        &pool,
        run.execution.execution_id,
        State::OwnerLost,
        None,
        Uuid::now_v7(),
    )
    .await?;
    let reasons: Value =
        sqlx::query_scalar("select pause_reasons from queue_state where conversation_id = $1")
            .bind(run.seed.conversation_id)
            .fetch_one(&pool)
            .await?;
    assert!(
        reasons
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["reason"] == "owner_lost")
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reconcile_waits_for_closed_sj_and_applies_owner_lost(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.send(vec![
        run.opened(),
        run.state("starting", ""),
        run.state("running", ""),
    ])
    .await;
    run.ingest
        .reconcile(run.seed.machine_id, &run.report("stopped", "normal_exit"))
        .await?;
    let rejected: i64 =
        sqlx::query_scalar("select count(*) from events where kind = 'execution_fact_rejected'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(rejected, 0);
    let state: String = sqlx::query_scalar("select state from executions where id = $1")
        .bind(run.execution.execution_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "running");
    run.ingest
        .reconcile(run.seed.machine_id, &run.report("owner_lost", ""))
        .await?;
    let state: String = sqlx::query_scalar("select state from executions where id = $1")
        .bind(run.execution.execution_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "owner_lost");
    let reasons: Value =
        sqlx::query_scalar("select pause_reasons from queue_state where conversation_id = $1")
            .bind(run.seed.conversation_id)
            .fetch_one(&pool)
            .await?;
    assert!(
        reasons
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["reason"] == "owner_lost")
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unsuccessful_start_facts_complete_command_with_outcome(pool: PgPool) -> TestResult {
    for (state, outcome) in [
        ("failed", "start_failed"),
        ("stopped", "start_cancelled"),
        ("owner_lost", ""),
    ] {
        let mut run = Run::new(&pool).await;
        let mut facts = vec![run.opened(), run.state("starting", "")];
        if state != "owner_lost" {
            facts.push(run.state("stopping", ""));
        }
        facts.push(run.state(state, outcome));
        run.send(facts).await;
        let (delivery, result): (String, Value) = sqlx::query_as(
            "select delivery_state, coalesce(result, '{}'::jsonb) from commands where id = $1",
        )
        .bind(run.execution.command_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(delivery, "completed", "{state}");
        assert_eq!(
            result["outcome"],
            if outcome.is_empty() {
                "owner_lost"
            } else {
                outcome
            }
        );
    }
    let run = Run::new(&pool).await;
    run.ingest
        .reconcile(run.seed.machine_id, &run.report("owner_lost", ""))
        .await?;
    let (delivery, result): (String, Value) = sqlx::query_as(
        "select delivery_state, coalesce(result, '{}'::jsonb) from commands where id = $1",
    )
    .bind(run.execution.command_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(delivery, "completed");
    assert_eq!(result["outcome"], "owner_lost");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terminal_execution_clears_phase_and_counts_late_phase(pool: PgPool) -> TestResult {
    for (terminal, outcome) in [("stopped", "normal_exit"), ("failed", "start_failed")] {
        let mut run = Run::new(&pool).await;
        let phase = || {
            Body::PhaseChanged(PhaseChanged {
                execution_id: run.execution.execution_id.to_string(),
                phase: "thinking".into(),
                ..Default::default()
            })
        };
        run.send(vec![
            run.opened(),
            run.state("starting", ""),
            phase(),
            run.state("stopping", ""),
            run.state(terminal, outcome),
        ])
        .await;
        let phase: Option<String> =
            sqlx::query_scalar("select phase from executions where id = $1")
                .bind(run.execution.execution_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(phase, None);
        run.send(vec![Body::PhaseChanged(PhaseChanged {
            execution_id: run.execution.execution_id.to_string(),
            phase: "thinking".into(),
            ..Default::default()
        })])
        .await;
        let phase: Option<String> =
            sqlx::query_scalar("select phase from executions where id = $1")
                .bind(run.execution.execution_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(phase, None);
        assert_eq!(run.ingest.ignored_record_count(), 1);
        let events: i64 = sqlx::query_scalar(
            "select count(*) from events where execution_id = $1 and kind = 'phase_changed'",
        )
        .bind(run.execution.execution_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(events, 1);
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn omitted_final_after_revision_gap_stays_incomplete(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    for text in ["missing middle", "prefix"] {
        let id = Uuid::now_v7();
        let mut facts = vec![];
        if run.seq == 0 {
            facts.push(run.opened());
        }
        facts.extend([
            start(id),
            delta(id, 1, "prefix".into()),
            delta(id, 3, "gap".into()),
            Body::BlockFinal(BlockFinal {
                block_id: id.to_string(),
                rev: 4,
                checksum: canterlot_hub::blocks::checksum(text),
                text_omitted: true,
                ..Default::default()
            }),
        ]);
        run.send(facts).await;
        let (state, reason, content): (String, Option<String>, String) = sqlx::query_as(
            "select state, incomplete_reason, content from blocks where block_id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(state, "incomplete");
        assert_eq!(reason.as_deref(), Some("revision_gap"));
        assert!(content == "prefix");
        let kinds: Vec<String> =
            sqlx::query_scalar("select kind from events where payload->>'block_id' = $1")
                .bind(id.to_string())
                .fetch_all(&pool)
                .await?;
        assert!(kinds.iter().any(|kind| kind == "block_revision_gap"));
        assert!(!kinds.iter().any(|kind| kind == "block_final"));
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| *kind == "block_checksum_mismatch")
                .count(),
            usize::from(text != "prefix")
        );
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sj_batch_uses_one_transaction_and_acks_only_its_prefix(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    sqlx::query("create table record_transactions (tx bigint not null)")
        .execute(&pool)
        .await?;
    sqlx::query("create function record_transaction() returns trigger language plpgsql as $$ begin insert into record_transactions values (txid_current()); if new.kind = 'block_delta' then raise exception 'rejected delta'; end if; return new; end $$").execute(&pool).await?;
    sqlx::query("create trigger record_transaction after insert on events for each row execute function record_transaction()").execute(&pool).await?;
    let id = Uuid::now_v7();
    let records = run.batch(vec![
        run.opened(),
        start(id),
        delta(id, 1, "rolled back".into()),
        start(Uuid::now_v7()),
    ]);
    let ack = run
        .ingest
        .sj_records(
            run.seed.machine_id,
            &run.seed.credential_digest(),
            records.clone(),
        )
        .await?;
    assert_eq!(ack.up_to_seq, 2);
    let cursor: i64 =
        sqlx::query_scalar("select committed_seq from journal_cursors where journal_id = $1")
            .bind(run.journal)
            .fetch_one(&pool)
            .await?;
    assert_eq!(cursor, 2);
    let rev: i64 = sqlx::query_scalar("select rev from blocks where block_id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(rev, 0);
    let transactions: i64 =
        sqlx::query_scalar("select count(distinct tx) from record_transactions")
            .fetch_one(&pool)
            .await?;
    assert_eq!(transactions, 1);
    let poison = run
        .ingest
        .sj_records(run.seed.machine_id, &run.seed.credential_digest(), records)
        .await
        .unwrap_err();
    assert_eq!(poison.code(), tonic::Code::Internal);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sj_successful_batch_has_one_commit_and_cursor_update(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    sqlx::query("create table record_transactions (tx bigint not null)")
        .execute(&pool)
        .await?;
    sqlx::query("create function record_transaction() returns trigger language plpgsql as $$ begin insert into record_transactions values (txid_current()); return new; end $$").execute(&pool).await?;
    sqlx::query("create trigger record_transaction after insert on events for each row execute function record_transaction()").execute(&pool).await?;
    sqlx::query("create table cursor_updates (n int)")
        .execute(&pool)
        .await?;
    sqlx::query("create function count_cursor_update() returns trigger language plpgsql as $$ begin if new.committed_seq <> old.committed_seq then insert into cursor_updates values (1); end if; return new; end $$").execute(&pool).await?;
    sqlx::query("create trigger count_cursor_update after update on journal_cursors for each row execute function count_cursor_update()").execute(&pool).await?;
    let id = Uuid::now_v7();
    run.send(vec![
        run.opened(),
        start(id),
        delta(id, 1, "one".into()),
        delta(id, 2, "two".into()),
    ])
    .await;
    let transactions: i64 =
        sqlx::query_scalar("select count(distinct tx) from record_transactions")
            .fetch_one(&pool)
            .await?;
    assert_eq!(transactions, 1);
    let updates: i64 = sqlx::query_scalar("select count(*) from cursor_updates")
        .fetch_one(&pool)
        .await?;
    assert_eq!(updates, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sj_ingest_cost_does_not_grow_with_block_size(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    run.send(vec![run.opened(), start(id)]).await;
    let mut random = 0x9e3779b97f4a7c15_u64;
    let bodies: Vec<_> = (1..=1536)
        .map(|rev| {
            let text: String = (0..1024)
                .map(|_| {
                    random ^= random << 13;
                    random ^= random >> 7;
                    random ^= random << 17;
                    (b'!' + (random % 90) as u8) as char
                })
                .collect();
            delta(id, rev, text)
        })
        .collect();
    let mut costs = vec![];
    for chunk in bodies.chunks(256) {
        let batch = run.batch(chunk.to_vec());
        let now = std::time::Instant::now();
        assert_eq!(
            run.ingest
                .apply_batch(
                    run.seed.machine_id,
                    &run.seed.credential_digest(),
                    JournalKind::Sj,
                    &batch
                )
                .await?,
            IngestOutcome::Committed { up_to_seq: run.seq }
        );
        costs.push(now.elapsed().as_secs_f64() * 1000.0 / 256.0);
    }
    println!(
        "sj_ingest_cost_does_not_grow_with_block_size: fresh={:.6} ms/delta; 1.5MiB={:.6} ms/delta; ratio={:.3}",
        costs[0],
        costs[5],
        costs[5] / costs[0]
    );
    assert!(
        costs[5] <= costs[0] * 2.5,
        "per-delta cost grew with block size"
    );
    Ok(())
}

#[test]
fn design_names_and_optional_exec_capability_follow_code() {
    let streams = include_str!("../../../docs/design/streams.md");
    let execution = include_str!("../../../docs/design/execution.md");
    let delivery = include_str!("../../../docs/design/delivery.md");
    let readme = include_str!("../../../docs/design/README.md");
    let recovery = include_str!("../../../docs/design/recovery.md");
    assert!(!streams.contains("`Hello.required_capabilities` или"));
    assert!(streams.contains("block_revision_gap"));
    for old in ["handshake_accepted", "handshake_rejected"] {
        assert!(!streams.contains(old));
    }
    assert!(!delivery.contains("state_divergence_detected"));
    assert!(!readme.contains("not_accepted_proven"));
    assert!(!delivery.contains("not_accepted_proven"));
    assert!(execution.contains("execution_state_changed"));
    assert!(execution.contains("running --> stopping: storage_failure"));
    assert!(execution.contains("storage_failed --> normal"));
    assert!(readme.contains("`stored` (сохранённый статус квитанции)"));
    assert!(readme.contains("kind = cj"));
    assert!(readme.contains("kind = sj"));
    assert!(!recovery.contains("manifest_missing_or_mismatch"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delta_and_incomplete_need_no_content_select_privilege(pool: PgPool) -> TestResult {
    use canterlot_hub::blocks::{self, ExecutionRef};
    let mut run = Run::new(&pool).await;
    let id = Uuid::now_v7();
    run.send(vec![run.opened(), start(id), delta(id, 1, "first".into())])
        .await;
    let stored: String = sqlx::query_scalar("select content from blocks where block_id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await?;
    assert!(stored.is_empty());
    let mut tx = pool.begin().await?;
    // Идентификатор роли состоит только из префикса и hex UUID.
    let role = format!("block_reader_{}", Uuid::now_v7().simple());
    for query in [
        format!("create role {role}"),
        format!("grant usage on schema public to {role}"),
        format!("grant all on all tables in schema public to {role}"),
        format!("revoke select on blocks from {role}"),
        format!(
            "grant select (block_id, execution_id, conversation_id, kind, rev, state, checksum, revision_gap, line_count) on blocks to {role}"
        ),
        format!("set local role {role}"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(query))
            .execute(&mut *tx)
            .await?;
    }
    let execution = ExecutionRef {
        execution_id: run.execution.execution_id,
        conversation_id: run.seed.conversation_id,
        machine_id: run.seed.machine_id,
    };
    blocks::apply_delta(
        &mut tx,
        &execution,
        &BlockDelta {
            block_id: id.to_string(),
            base_rev: 1,
            rev: 2,
            append: " second".into(),
        },
        Uuid::now_v7(),
    )
    .await?;
    blocks::mark_incomplete(
        &mut tx,
        &execution,
        &BlockIncomplete {
            block_id: id.to_string(),
            rev: 2,
            reason: "interrupted".into(),
        },
        Uuid::now_v7(),
    )
    .await?;
    sqlx::query("reset role").execute(&mut *tx).await?;
    let text = blocks::read_text(&mut tx, id).await?;
    assert!(text == "first second");
    // Закрытый блок держит текст в content, дельты удалены той же транзакцией.
    let count: i64 = sqlx::query_scalar("select count(*) from block_chunks where block_id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    assert_eq!(count, 0);
    tx.rollback().await?;
    let mut conn = pool.acquire().await?;
    assert!(blocks::read_text(&mut conn, id).await? == "first");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reconcile_accepts_terminal_only_after_its_bound_sj_is_closed(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    run.send(vec![
        run.opened(),
        run.state("starting", ""),
        run.state("stopping", ""),
    ])
    .await;
    run.ingest
        .reconcile(run.seed.machine_id, &run.report("stopped", "normal_exit"))
        .await?;
    let state: String = sqlx::query_scalar("select state from executions where id = $1")
        .bind(run.execution.execution_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "stopping");
    run.send(vec![Body::JournalClosed(JournalClosed {
        execution_id: run.execution.execution_id.to_string(),
        generation: run.execution.generation as u64,
        reason: "normal_exit".into(),
    })])
    .await;
    let mut foreign = run.report("stopped", "normal_exit");
    foreign.executions[0].sj_journal_id = Uuid::now_v7().to_string();
    run.ingest.reconcile(run.seed.machine_id, &foreign).await?;
    let state: String = sqlx::query_scalar("select state from executions where id = $1")
        .bind(run.execution.execution_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "stopping");
    run.ingest
        .reconcile(run.seed.machine_id, &run.report("stopped", "normal_exit"))
        .await?;
    let state: String = sqlx::query_scalar("select state from executions where id = $1")
        .bind(run.execution.execution_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state, "stopped");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn migration_preserves_existing_streaming_and_materialized_text(pool: PgPool) -> TestResult {
    let run = Run::new(&pool).await;
    sqlx::query("drop table block_chunks")
        .execute(&pool)
        .await?;
    sqlx::query("alter table blocks drop column revision_gap")
        .execute(&pool)
        .await?;
    let mut ids = vec![];
    for state in ["streaming", "final", "incomplete"] {
        let id = Uuid::now_v7();
        sqlx::query("insert into blocks (block_id, execution_id, conversation_id, kind, state, rev, content) values ($1, $2, $3, 'text', $4, 7, 'старый текст')")
            .bind(id).bind(run.execution.execution_id).bind(run.seed.conversation_id).bind(state).execute(&pool).await?;
        ids.push(id);
    }
    canterlot_hub::events::append(
        &mut *pool.acquire().await?,
        canterlot_hub::events::NewEvent {
            scope: canterlot_hub::events::Scope::Conversation {
                id: run.seed.conversation_id,
            },
            kind: "block_revision_gap",
            conversation_id: Some(run.seed.conversation_id),
            execution_id: Some(run.execution.execution_id),
            payload: serde_json::json!({"block_id": ids[0]}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await?;
    sqlx::raw_sql(include_str!("../migrations/0007_block_chunks.sql"))
        .execute(&pool)
        .await?;
    let mut conn = pool.acquire().await?;
    for id in &ids {
        assert!(canterlot_hub::blocks::read_text(&mut conn, *id).await? == "старый текст");
    }
    let content: String = sqlx::query_scalar("select content from blocks where block_id = $1")
        .bind(ids[0])
        .fetch_one(&mut *conn)
        .await?;
    assert!(content.is_empty());
    let gap: bool = sqlx::query_scalar("select revision_gap from blocks where block_id = $1")
        .bind(ids[0])
        .fetch_one(&mut *conn)
        .await?;
    assert!(gap);
    let chunks: i64 = sqlx::query_scalar("select count(*) from block_chunks")
        .fetch_one(&mut *conn)
        .await?;
    assert_eq!(chunks, 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn failed_sj_commit_never_acks_a_prefix(pool: PgPool) -> TestResult {
    let mut run = Run::new(&pool).await;
    sqlx::query("create function reject_commit() returns trigger language plpgsql as $$ begin raise exception 'rejected commit'; end $$").execute(&pool).await?;
    sqlx::query("create constraint trigger reject_commit after insert on events deferrable initially deferred for each row execute function reject_commit()").execute(&pool).await?;
    let mut records = run.batch(vec![
        run.opened(),
        start(Uuid::now_v7()),
        start(Uuid::now_v7()),
    ]);
    records.records[2].body = None;
    let error = run
        .ingest
        .sj_records(run.seed.machine_id, &run.seed.credential_digest(), records)
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Internal);
    let cursors: i64 = sqlx::query_scalar("select count(*) from journal_cursors")
        .fetch_one(&pool)
        .await?;
    let blocks: i64 = sqlx::query_scalar("select count(*) from blocks")
        .fetch_one(&pool)
        .await?;
    assert_eq!(cursors, 0);
    assert_eq!(blocks, 0);
    assert_eq!(run.ingest.ignored_record_count(), 0);
    Ok(())
}
