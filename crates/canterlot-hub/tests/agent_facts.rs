#[path = "support/task3.rs"]
mod support;
use canterlot_hub::{
    blocks,
    config::{HubTimers, LimitsConfig},
    delivery::{
        SubmittedInput,
        receipts::{self, InputReceipt},
    },
    events::Scope,
    executions,
    ingest::{Ingest, IngestError, JournalKind},
    limits::{self, Observation, ObservationSource, Window},
    model::ExecutionState,
    runs, testkit,
};
use canterlot_proto::worker::v1::{record_body::Body, *};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::*;
use uuid::Uuid;
type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn review_fix_agent_metadata_is_truncated_at_char_boundaries(pool: PgPool) {
    let mut r = Run::new(&pool).await;
    let text = "я🦄".repeat(1024);
    let expected = |limit: usize| format!("{}…", text.chars().take(limit - 1).collect::<String>());
    let block = Uuid::now_v7();
    let incomplete = Uuid::now_v7();
    let closed = Uuid::now_v7();
    let start = |id: Uuid| {
        Body::BlockStarted(BlockStarted {
            block_id: id.to_string(),
            kind: "text".into(),
            agent: text.clone(),
            model: text.clone(),
            tool_name: text.clone(),
            summary: text.clone(),
            ..Default::default()
        })
    };
    r.send(vec![
        start(block),
        Body::PhaseChanged(PhaseChanged {
            execution_id: r.execution.execution_id.to_string(),
            phase: "running_tool".into(),
            tool_name: text.clone(),
            summary: text.clone(),
        }),
    ])
    .await;
    let detail: Value = sqlx::query_scalar("select phase_detail from executions where id=$1")
        .bind(r.execution.execution_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(detail["summary"], expected(1024));
    assert_eq!(detail["tool_name"], expected(128));
    r.send(vec![
        Body::BlockFinal(BlockFinal {
            block_id: block.to_string(),
            rev: 1,
            checksum: blocks::checksum(""),
            summary: text.clone(),
            ..Default::default()
        }),
        start(incomplete),
        Body::BlockIncomplete(BlockIncomplete {
            block_id: incomplete.to_string(),
            rev: 1,
            reason: text.clone(),
        }),
        start(closed),
        Body::AdapterDiagnostic(AdapterDiagnostic {
            execution_id: r.execution.execution_id.to_string(),
            kind: text.clone(),
            event_type: text.clone(),
        }),
        Body::JournalClosed(JournalClosed {
            execution_id: r.execution.execution_id.to_string(),
            generation: r.execution.generation as u64,
            reason: text.clone(),
        }),
    ])
    .await;
    let rows: Vec<(String, Value)> =
        sqlx::query_as("select kind,payload from events where scope=$1 order by scope_seq")
            .bind(
                Scope::Conversation {
                    id: r.seed.conversation_id,
                }
                .key(),
            )
            .fetch_all(&pool)
            .await
            .unwrap();
    for (kind, payload) in rows {
        for (field, limit) in [
            ("summary", 1024),
            ("tool_name", 128),
            ("agent", 128),
            ("model", 128),
            ("reason", 1024),
        ] {
            if let Some(value) = payload[field].as_str() {
                assert_eq!(value, expected(limit), "{kind}.{field}");
            }
        }
        if kind == "adapter_diagnostic" {
            assert_eq!(payload["kind"], expected(128));
            assert_eq!(payload["event_type"], expected(128));
        }
    }
    let stored: (String, String, String, String) =
        sqlx::query_as("select agent,model,tool_name,summary from blocks where block_id=$1")
            .bind(block)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        stored,
        (expected(128), expected(128), expected(128), expected(1024))
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn block_started_stores_agent_model_tool_and_summary(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let id = Uuid::now_v7();
    r.send(vec![started(id, "tool_call")]).await;
    let row: (String, String, Option<String>, Option<String>) =
        sqlx::query_as("select agent,model,tool_name,summary from blocks where block_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        row,
        (
            "Explore".into(),
            "synthetic-small".into(),
            Some("Read".into()),
            Some("task".into())
        )
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn subagent_blocks_reference_parent_and_agent(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let parent = Uuid::now_v7();
    let child = Uuid::now_v7();
    let Body::BlockStarted(mut c) = started(child, "text") else {
        unreachable!()
    };
    c.parent_block_id = parent.to_string();
    r.send(vec![started(parent, "tool_call"), Body::BlockStarted(c)])
        .await;
    let row: (Uuid, String) =
        sqlx::query_as("select parent_block_id,agent from blocks where block_id=$1")
            .bind(child)
            .fetch_one(&pool)
            .await?;
    assert_eq!(row, (parent, "Explore".into()));
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn created_seq_matches_the_creation_event_for_blocks_and_inputs(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let input = r.input(&pool).await;
    r.send(vec![started(Uuid::now_v7(), "text")]).await;
    assert_eq!(count(&pool,"select count(*) from blocks b join events e on e.payload->>'block_id'=b.block_id::text and e.kind='block_started' where b.created_seq=e.scope_seq").await,1);
    let ok:bool=sqlx::query_scalar("select i.created_seq=e.scope_seq and i.created_at=e.committed_at from inputs i join events e on e.payload->>'input_id'=i.id::text and e.kind='input_queued' where i.id=$1").bind(input.input_id).fetch_one(&pool).await?;
    assert!(ok);
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn feed_seq_is_set_once_by_the_event_that_moves_the_input_into_the_feed(
    pool: PgPool,
) -> TestResult {
    let r = Run::new(&pool).await;
    let i = r.input(&pool).await;
    let before: Option<i64> = sqlx::query_scalar("select feed_seq from inputs where id=$1")
        .bind(i.input_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(before, None);
    receipts::apply_input_receipt(
        &pool,
        i.command_id,
        1,
        InputReceipt::TransmissionStarted,
        Uuid::now_v7(),
    )
    .await?;
    let seq: i64 = sqlx::query_scalar("select feed_seq from inputs where id=$1")
        .bind(i.input_id)
        .fetch_one(&pool)
        .await?;
    let e: Value = sqlx::query_scalar("select payload from events where scope=$1 and scope_seq=$2")
        .bind(
            Scope::Conversation {
                id: r.seed.conversation_id,
            }
            .key(),
        )
        .bind(seq)
        .fetch_one(&pool)
        .await?;
    assert_eq!(e["state"], "transmitting");
    receipts::apply_input_receipt(
        &pool,
        i.command_id,
        1,
        InputReceipt::Accepted,
        Uuid::now_v7(),
    )
    .await?;
    let after: i64 = sqlx::query_scalar("select feed_seq from inputs where id=$1")
        .bind(i.input_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(seq, after);
    Ok(())
}
#[sqlx::test(migrations = false)]
async fn migration_0008_backfills_created_seq_from_events_or_creation_order(
    pool: PgPool,
) -> TestResult {
    for sql in [
        include_str!("../migrations/0001_core.sql"),
        include_str!("../migrations/0002_sessions.sql"),
        include_str!("../migrations/0003_journal_owner.sql"),
        include_str!("../migrations/0004_last_record_digest.sql"),
        include_str!("../migrations/0005_machine_identity.sql"),
        include_str!("../migrations/0006_execution.sql"),
        include_str!("../migrations/0007_block_chunks.sql"),
    ] {
        sqlx::raw_sql(sql).execute(&pool).await?;
    }
    let s = testkit::seed(&pool).await;
    let execution_id = Uuid::now_v7();
    // Историческая фикстура использует только столбцы схемы до 0008.
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'created')",
    )
    .bind(execution_id)
    .bind(s.conversation_id)
    .execute(&pool)
    .await?;
    for n in 0..3 {
        let b = Uuid::now_v7();
        sqlx::query("insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_at) values($1,$2,$3,'text','final',0,now()+make_interval(secs=>$4))").bind(b).bind(execution_id).bind(s.conversation_id).bind(n as f64).execute(&pool).await?;
        if n == 2 {
            sqlx::query("insert into events(id,scope,scope_seq,kind,conversation_id,payload,correlation_id) values($1,$2,7,'block_started',$3,$4,$5)").bind(Uuid::now_v7()).bind(Scope::Conversation{id:s.conversation_id}.key()).bind(s.conversation_id).bind(json!({"block_id":b})).bind(Uuid::now_v7()).execute(&pool).await?;
        }
    }
    // Старые inputs создаются без нового submit, чтобы миграция видела схему 1.2b.
    for n in 0..2 {
        let cmd = Uuid::now_v7();
        let id = Uuid::now_v7();
        sqlx::query("insert into commands(id,machine_id,conversation_id,kind,payload,delivery_state) values($1,$2,$3,'deliver_input','{}','completed')").bind(cmd).bind(s.machine_id).bind(s.conversation_id).execute(&pool).await?;
        sqlx::query("insert into inputs(id,conversation_id,owner_id,input_seq,idempotency_key,content,state,command_id,received_at) values($1,$2,$3,$4,$5,'[]','accepted',$6,now())").bind(id).bind(s.conversation_id).bind(s.owner_id).bind(n+1_i64).bind(id.to_string()).bind(cmd).execute(&pool).await?;
        if n == 1 {
            for (seq, kind, payload) in [
                (8, "input_queued", json!({"input_id":id})),
                (
                    9,
                    "input_state_changed",
                    json!({"input_id":id,"state":"transmitting"}),
                ),
                (
                    10,
                    "input_state_changed",
                    json!({"input_id":id,"state":"accepted"}),
                ),
            ] {
                sqlx::query("insert into events(id,scope,scope_seq,kind,conversation_id,payload,correlation_id) values($1,$2,$3,$4,$5,$6,$7)").bind(Uuid::now_v7()).bind(Scope::Conversation{id:s.conversation_id}.key()).bind(seq).bind(kind).bind(s.conversation_id).bind(payload).bind(Uuid::now_v7()).execute(&pool).await?;
            }
        }
    }
    sqlx::raw_sql(include_str!("../migrations/0008_agent_facts.sql"))
        .execute(&pool)
        .await?;
    let seqs: Vec<i64> = sqlx::query_scalar("select created_seq from blocks order by created_seq")
        .fetch_all(&pool)
        .await?;
    assert_eq!(seqs, vec![-2, -1, 7]);
    let inputs: Vec<(i64, i64)> =
        sqlx::query_as("select created_seq,feed_seq from inputs order by input_seq")
            .fetch_all(&pool)
            .await?;
    assert_eq!(inputs, vec![(-1, -1), (8, 9)]);
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn block_created_at_and_tool_timestamps_come_from_event_commit_times(
    pool: PgPool,
) -> TestResult {
    let mut r = Run::new(&pool).await;
    let id = Uuid::now_v7();
    r.send(vec![
        started(id, "tool_call"),
        Body::BlockFinal(BlockFinal {
            block_id: id.to_string(),
            rev: 1,
            checksum: blocks::checksum(""),
            summary: "done".into(),
            ..Default::default()
        }),
    ])
    .await;
    let ok:bool=sqlx::query_scalar("select b.created_at=s.committed_at and b.finished_at=f.committed_at and b.summary='done' from blocks b join events s on s.payload->>'block_id'=b.block_id::text and s.kind='block_started' join events f on f.payload->>'block_id'=b.block_id::text and f.kind='block_final' where b.block_id=$1").bind(id).fetch_one(&pool).await?;
    assert!(ok);
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn tool_output_line_count_is_maintained_on_append_and_final(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let id = Uuid::now_v7();
    r.send(vec![
        started(id, "tool_result"),
        Body::BlockDelta(BlockDelta {
            block_id: id.to_string(),
            base_rev: 0,
            rev: 1,
            append: "a\nb\n".into(),
        }),
    ])
    .await;
    let lines: i32 = sqlx::query_scalar("select line_count from blocks where block_id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(lines, 2);
    r.send(vec![Body::BlockFinal(BlockFinal {
        block_id: id.to_string(),
        rev: 2,
        text: "a\nb\nc\n".into(),
        checksum: blocks::checksum("a\nb\nc\n"),
        ..Default::default()
    })])
    .await;
    let lines: i32 = sqlx::query_scalar("select line_count from blocks where block_id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(lines, 3);
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn phase_detail_is_stored_with_tool_name(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    r.send(vec![Body::PhaseChanged(PhaseChanged {
        execution_id: r.execution.execution_id.to_string(),
        phase: "running_tool".into(),
        tool_name: "Read".into(),
        summary: "task".into(),
    })])
    .await;
    let detail: Value = sqlx::query_scalar("select phase_detail from executions where id=$1")
        .bind(r.execution.execution_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(
        detail,
        json!({"phase":"running_tool","tool_name":"Read","summary":"task"})
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_is_created_on_accept_and_finished_with_usage_per_model(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let i = r.input(&pool).await;
    let ms = Utc::now().timestamp_millis();
    record(&pool, &r.seed, &obs(&r.seed, 900, Utc::now())).await;
    r.send(vec![accepted(i.command_id, ms)]).await;
    assert_eq!(
        count(
            &pool,
            "select count(*) from runs where provider='claude' and finished_at is null"
        )
        .await,
        1
    );
    record(&pool, &r.seed, &obs(&r.seed, 905, Utc::now())).await;
    let Body::RunFinished(mut f) = finished(&r, i.command_id, ms + 1000) else {
        unreachable!()
    };
    f.usage.push(ModelUsage {
        model: "synthetic-large".into(),
        agent: "main".into(),
        input_tokens: 30,
        output_tokens: 40,
        cache_read_tokens: 5,
        cache_write_tokens: 6,
    });
    r.send(vec![Body::RunFinished(f)]).await;
    let readings: (i32, i32) =
        sqlx::query_as("select weekly_before_permille,weekly_after_permille from runs")
            .fetch_one(&pool)
            .await?;
    assert_eq!(readings, (900, 905));
    assert_eq!(
        count(&pool, "select count(*) from run_model_usage").await,
        2
    );
    let row:(i64,i64,i64,i64)=sqlx::query_as("select duration_ms,context_tokens,context_window_tokens,(extract(epoch from (finished_at-started_at))*1000)::bigint from runs").fetch_one(&pool).await?;
    assert_eq!(row, (1000, 123, 200000, 1000));
    let u:(i64,i64,i64,i64)=sqlx::query_as("select input_tokens,output_tokens,cache_read_tokens,cache_write_tokens from run_model_usage where model='synthetic-small'").fetch_one(&pool).await?;
    assert_eq!(u, (10, 20, 3, 4));
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn replayed_run_finished_does_not_duplicate_usage(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let i = r.input(&pool).await;
    let ms = Utc::now().timestamp_millis();
    let f = finished(&r, i.command_id, ms + 1000);
    r.send(vec![accepted(i.command_id, ms), f.clone(), f]).await;
    assert_eq!(
        count(&pool, "select count(*) from run_model_usage").await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "select sum(output_tokens)::bigint from run_model_usage"
        )
        .await,
        20
    );
    Ok(())
}
async fn stored_run(pool: &PgPool, r: &Run, ms: i64, duration: u64, late: bool) -> TestResult {
    let i = r.input(pool).await;
    let at = DateTime::from_timestamp_millis(ms).unwrap();
    let now = if late { at + Duration::seconds(90) } else { at };
    let mut tx = pool.begin().await?;
    runs::start_in_tx(&mut tx, i.command_id, &r.reference(), at, now).await?;
    let Body::RunFinished(mut fact) = finished(r, i.command_id, ms + duration as i64) else {
        unreachable!()
    };
    fact.duration_ms = duration;
    runs::finish_in_tx(
        &mut tx,
        &fact,
        &r.reference(),
        at + Duration::milliseconds(duration as i64),
        now + Duration::milliseconds(duration as i64),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_attribution_marks_overlapping_runs_of_one_provider(pool: PgPool) -> TestResult {
    let a = Run::new(&pool).await;
    let b = Run::new(&pool).await;
    let c = Run::new(&pool).await;
    sqlx::query("update conversations set harness='synthetic-codex' where id=$1")
        .bind(c.seed.conversation_id)
        .execute(&pool)
        .await?;
    let ms = Utc::now().timestamp_millis();
    stored_run(&pool, &a, ms, 10000, false).await?;
    stored_run(&pool, &b, ms + 5000, 10000, false).await?;
    stored_run(&pool, &c, ms + 5000, 10000, false).await?;
    assert_eq!(
        count(
            &pool,
            "select count(*) from runs where overlapped and provider='claude'"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from runs where overlapped and provider='codex'"
        )
        .await,
        0
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn late_ingested_run_is_not_exact_and_excluded_from_the_estimate(pool: PgPool) -> TestResult {
    let r = Run::new(&pool).await;
    stored_run(&pool, &r, Utc::now().timestamp_millis(), 1000, true).await?;
    sqlx::query("update runs set weekly_before_permille=900,weekly_after_permille=950")
        .execute(&pool)
        .await?;
    let rows = limits::usage_by_day(
        &pool,
        r.seed.owner_id,
        None,
        Utc::now() - Duration::days(1),
        Utc::now() + Duration::days(1),
        "UTC",
    )
    .await?;
    assert_eq!(
        count(&pool, "select count(*) from runs where not exact").await,
        1
    );
    assert_eq!(rows[0].weekly_delta_permille, 0);
    assert_eq!(rows[0].overlapped_runs, 1);
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn context_exhausted_outcome_is_a_legal_failed_transition(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    r.send(vec![Body::PhaseChanged(PhaseChanged {
        execution_id: r.execution.execution_id.to_string(),
        phase: "generating".into(),
        ..Default::default()
    })])
    .await;
    executions::set_state(
        &pool,
        r.execution.execution_id,
        ExecutionState::Stopping,
        None,
        Uuid::now_v7(),
    )
    .await?;
    executions::set_state(
        &pool,
        r.execution.execution_id,
        ExecutionState::Failed,
        Some("context_exhausted"),
        Uuid::now_v7(),
    )
    .await?;
    let row: (String, Option<Value>, Option<String>) =
        sqlx::query_as("select outcome,phase_detail,phase from executions where id=$1")
            .bind(r.execution.execution_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(row, ("context_exhausted".into(), None, None));
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn last_event_at_is_touched_once_per_batch(pool: PgPool) -> TestResult {
    sqlx::raw_sql("create table touches(n integer); create function touch_count() returns trigger language plpgsql as $$ begin if new.last_event_at is distinct from old.last_event_at then insert into touches values(1); end if; return new; end $$; create trigger touch_count before update on executions for each row execute function touch_count();").execute(&pool).await?;
    let mut r = Run::new(&pool).await;
    r.send(vec![
        started(Uuid::now_v7(), "text"),
        started(Uuid::now_v7(), "tool_call"),
        Body::PhaseChanged(PhaseChanged {
            execution_id: r.execution.execution_id.to_string(),
            phase: "generating".into(),
            ..Default::default()
        }),
    ])
    .await;
    assert_eq!(count(&pool, "select count(*) from touches").await, 1);
    Ok(())
}
fn obs(s: &testkit::Seed, used: u16, at: DateTime<Utc>) -> Observation {
    Observation {
        provider: "claude".into(),
        window: Window::Weekly,
        used_permille: used,
        resets_at: None,
        observed_at: at,
        source: ObservationSource::Session,
        machine_id: s.machine_id,
        execution_id: None,
    }
}
async fn record(pool: &PgPool, s: &testkit::Seed, o: &Observation) -> limits::Recorded {
    let mut tx = pool.begin().await.unwrap();
    let r = limits::record_observation_in_tx(
        &mut tx,
        s.owner_id,
        o,
        Utc::now(),
        &LimitsConfig::default(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    r
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limit_observed_updates_provider_state_and_emits_limits_changed_once(
    pool: PgPool,
) -> TestResult {
    let s = testkit::seed(&pool).await;
    let at = Utc::now();
    let result = record(&pool, &s, &obs(&s, 960, at)).await;
    assert!(result.provider_changed);
    assert!(!result.stale);
    assert_eq!(
        count(
            &pool,
            "select weekly_used_permille::bigint from provider_limits where provider='claude'"
        )
        .await,
        960
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='limits_changed'"
        )
        .await,
        1
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn replayed_stale_observation_never_lowers_the_stored_reading(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    let at = Utc::now();
    record(&pool, &s, &obs(&s, 960, at)).await;
    for time in [at - Duration::minutes(1), at] {
        let r = record(&pool, &s, &obs(&s, 940, time)).await;
        assert!(r.stale);
        assert!(!r.provider_changed);
    }
    assert_eq!(
        count(
            &pool,
            "select weekly_used_permille::bigint from provider_limits"
        )
        .await,
        960
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from limit_snapshots where used_permille=940"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='limits_changed'"
        )
        .await,
        1
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn observation_time_is_machine_time_converted_with_the_session_offset(
    pool: PgPool,
) -> TestResult {
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    for skew in [-600000, 600000] {
        let s = wire_seed(&pool).await;
        let machine_ms = Utc::now().timestamp_millis() + skew;
        let c = connect(&hub, &s, Uuid::now_v7(), true, machine_ms as u64).await;
        let offset = hub.registry.clock_offset(s.machine_id).unwrap();
        assert!((offset.num_milliseconds() + skew).abs() < 1000);
        let at = hub
            .registry
            .to_hub_time(s.machine_id, machine_ms as u64)
            .unwrap();
        c.tx.send(ControlUp {
            msg: Some(control_up::Msg::LimitsReport(LimitsReport {
                observations: vec![observation(if skew < 0 { 960 } else { 970 }, machine_ms)],
            })),
        })
        .await?;
        tokio::time::timeout(WAIT, async {
            loop {
                let stored: bool = sqlx::query_scalar(
                    "select exists(select 1 from limit_snapshots where machine_id=$1)",
                )
                .bind(s.machine_id)
                .fetch_one(&pool)
                .await
                .unwrap();
                if stored {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await?;
        let stored: DateTime<Utc> =
            sqlx::query_scalar("select observed_at from limit_snapshots where machine_id=$1")
                .bind(s.machine_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(stored, at);
        drop(c);
    }
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limits_changed_of_a_batch_is_appended_once_at_the_end(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    use sha2::{Digest, Sha256};
    let credential = Sha256::digest(r.seed.machine_id.to_string().as_bytes()).to_vec();
    sqlx::query("update machine_credentials set token_sha256=$2 where machine_id=$1")
        .bind(r.seed.machine_id)
        .bind(&credential)
        .execute(&pool)
        .await?;
    let _client = connect(
        &hub,
        &r.seed,
        Uuid::now_v7(),
        true,
        Utc::now().timestamp_millis() as u64,
    )
    .await;
    r.ingest = Ingest::new(pool.clone()).with_registry(hub.registry.clone());
    let now = Utc::now().timestamp_millis();
    let batch = r.batch(vec![
        Body::LimitObserved(observation(940, now)),
        started(Uuid::now_v7(), "text"),
        Body::LimitObserved(observation(960, now + 1)),
    ]);
    r.ingest
        .apply_batch(r.seed.machine_id, &credential, JournalKind::Sj, &batch)
        .await?;
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='limits_changed'"
        )
        .await,
        1
    );
    let after:bool=sqlx::query_scalar("select (select min(committed_at) from events where kind='limits_changed') >= (select max(committed_at) from events where kind='block_started')").fetch_one(&pool).await?;
    assert!(after);
    assert_eq!(
        count(
            &pool,
            "select weekly_used_permille::bigint from provider_limits"
        )
        .await,
        960
    );
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn snapshots_are_deduplicated_unless_older_than_min_interval(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    let at = Utc::now();
    assert!(record(&pool, &s, &obs(&s, 940, at)).await.snapshot_stored);
    assert!(
        !record(&pool, &s, &obs(&s, 940, at + Duration::seconds(1)))
            .await
            .snapshot_stored
    );
    assert!(
        record(&pool, &s, &obs(&s, 940, at + Duration::seconds(601)))
            .await
            .snapshot_stored
    );
    assert_eq!(
        count(&pool, "select count(*) from limit_snapshots").await,
        2
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limits_report_from_poll_is_stored_with_poll_source(pool: PgPool) -> TestResult {
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let s = wire_seed(&pool).await;
    let c = connect(
        &hub,
        &s,
        Uuid::now_v7(),
        true,
        Utc::now().timestamp_millis() as u64,
    )
    .await;
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::LimitsReport(LimitsReport {
            observations: vec![observation(920, Utc::now().timestamp_millis())],
        })),
    })
    .await?;
    wait_until(
        &pool,
        "select count(*) from limit_snapshots where source='poll'",
    )
    .await;
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limit_reached_without_crunch_pauses_the_queue_with_provider_window_and_reset(
    pool: PgPool,
) -> TestResult {
    let mut r = Run::new(&pool).await;
    let reset = Utc::now().timestamp_millis() + 100000;
    let fact = Body::LimitReached(LimitReached {
        execution_id: r.execution.execution_id.to_string(),
        provider: "claude".into(),
        window: LimitWindow::FiveHour as i32,
        resets_at_ms: reset as u64,
        ..Default::default()
    });
    r.send(vec![fact.clone(), fact]).await;
    let p: Value =
        sqlx::query_scalar("select pause_reasons from queue_state where conversation_id=$1")
            .bind(r.seed.conversation_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(p.as_array().unwrap().len(), 1);
    assert_eq!(p[0]["provider"], "claude");
    assert_eq!(p[0]["window"], "five_hour");
    assert_eq!(
        DateTime::parse_from_rfc3339(p[0]["resets_at"].as_str().unwrap())?.timestamp_millis(),
        reset
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='limit_reached'"
        )
        .await,
        1
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limit_records_on_a_cj_are_invalid(pool: PgPool) -> TestResult {
    let r = Run::new(&pool).await;
    for body in [
        Body::LimitObserved(observation(960, Utc::now().timestamp_millis())),
        Body::LimitReached(LimitReached {
            execution_id: r.execution.execution_id.to_string(),
            provider: "claude".into(),
            window: LimitWindow::Weekly as i32,
            ..Default::default()
        }),
    ] {
        let b = JournalRecords {
            journal_id: Uuid::now_v7().to_string(),
            records: vec![JournalRecord {
                seq: 1,
                body: Some(RecordBody { body: Some(body) }),
            }],
        };
        assert!(matches!(
            r.ingest
                .apply_batch(
                    r.seed.machine_id,
                    &r.seed.credential_digest(),
                    JournalKind::Cj,
                    &b
                )
                .await,
            Err(IngestError::InvalidRecord)
        ));
    }
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limit_records_of_another_machine_never_touch_foreign_conversations(
    pool: PgPool,
) -> TestResult {
    let mut a = Run::new(&pool).await;
    let b = Run::new(&pool).await;
    let batch = a.batch(vec![Body::LimitReached(LimitReached {
        execution_id: b.execution.execution_id.to_string(),
        provider: "claude".into(),
        window: LimitWindow::Weekly as i32,
        ..Default::default()
    })]);
    assert!(matches!(
        a.ingest
            .apply_batch(
                a.seed.machine_id,
                &a.seed.credential_digest(),
                JournalKind::Sj,
                &batch
            )
            .await,
        Err(IngestError::ForeignExecution)
    ));
    assert_eq!(
        count(
            &pool,
            "select count(*) from queue_state where pause_reasons<>'[]'"
        )
        .await,
        0
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn usage_by_day_groups_by_session_and_model_and_excludes_overlap_from_the_estimate(
    pool: PgPool,
) -> TestResult {
    let a = Run::new(&pool).await;
    let ms = DateTime::parse_from_rfc3339("2026-10-26T22:30:00Z")?.timestamp_millis();
    stored_run(&pool, &a, ms, 1000, false).await?;
    sqlx::query("update runs set weekly_before_permille=900,weekly_after_permille=905")
        .execute(&pool)
        .await?;
    let rows = limits::usage_by_day(
        &pool,
        a.seed.owner_id,
        Some("claude"),
        DateTime::from_timestamp_millis(ms - 1000).unwrap(),
        DateTime::from_timestamp_millis(ms + 10000).unwrap(),
        "Europe/Moscow",
    )
    .await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].day.to_string(), "2026-10-27");
    assert_eq!(rows[0].conversation_id, a.seed.conversation_id);
    assert_eq!(rows[0].model, "synthetic-small");
    assert_eq!(rows[0].weekly_delta_permille, 5);
    sqlx::query("update runs set overlapped=true")
        .execute(&pool)
        .await?;
    let rows = limits::usage_by_day(
        &pool,
        a.seed.owner_id,
        None,
        DateTime::from_timestamp_millis(ms - 1000).unwrap(),
        DateTime::from_timestamp_millis(ms + 10000).unwrap(),
        "UTC",
    )
    .await?;
    assert_eq!(rows[0].weekly_delta_permille, 0);
    assert_eq!(rows[0].overlapped_runs, 1);
    sqlx::query("insert into run_model_usage select input_command_id,'synthetic-large','main',30,40,0,0 from runs").execute(&pool).await?;
    let b = Run::new(&pool).await;
    stored_run(&pool, &b, ms + 20000, 1000, false).await?;
    sqlx::query("update conversations set owner_id=$2 where id=$1")
        .bind(b.seed.conversation_id)
        .bind(a.seed.owner_id)
        .execute(&pool)
        .await?;
    let grouped = limits::usage_by_day(
        &pool,
        a.seed.owner_id,
        None,
        DateTime::from_timestamp_millis(ms - 1000).unwrap(),
        DateTime::from_timestamp_millis(ms + 30000).unwrap(),
        "UTC",
    )
    .await?;
    assert_eq!(grouped.len(), 3);
    assert_eq!(
        grouped
            .iter()
            .filter(|u| u.conversation_id == a.seed.conversation_id)
            .count(),
        2
    );
    assert_eq!(
        grouped
            .iter()
            .filter(|u| u.conversation_id == b.seed.conversation_id)
            .count(),
        1
    );
    let foreign = limits::usage_by_day(
        &pool,
        b.seed.owner_id,
        None,
        DateTime::from_timestamp_millis(ms - 1000).unwrap(),
        DateTime::from_timestamp_millis(ms + 30000).unwrap(),
        "UTC",
    )
    .await?;
    assert!(foreign.is_empty());
    assert!(
        limits::usage_by_day(
            &pool,
            a.seed.owner_id,
            None,
            Utc::now(),
            Utc::now(),
            "bad/zone"
        )
        .await
        .is_err()
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn series_are_ordered_and_bounded_for_both_windows(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    let at = DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    for window in [Window::Weekly, Window::FiveHour] {
        for n in [2, 0, 1] {
            let mut o = obs(&s, 900 + n, at + Duration::seconds(n as i64));
            o.window = window;
            record(&pool, &s, &o).await;
        }
        let rows = limits::series(&pool, "claude", window, at, at + Duration::seconds(2)).await?;
        assert_eq!(rows, vec![(at, 900), (at + Duration::seconds(1), 901)]);
    }
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn usage_rows_carry_the_agent_of_the_model_usage(pool: PgPool) -> TestResult {
    let r = Run::new(&pool).await;
    stored_run(&pool, &r, Utc::now().timestamp_millis(), 1000, false).await?;
    let rows = limits::usage_by_day(
        &pool,
        r.seed.owner_id,
        None,
        Utc::now() - Duration::days(1),
        Utc::now() + Duration::days(1),
        "UTC",
    )
    .await?;
    assert_eq!(rows[0].agent, "Explore");
    assert_eq!(rows[0].input_tokens, 10);
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limits_snapshot_reaches_pilot_sessions_of_that_provider(pool: PgPool) -> TestResult {
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let a = wire_seed(&pool).await;
    let b = wire_seed(&pool).await;
    let c = wire_seed(&pool).await;
    for s in [&a, &b, &c] {
        testkit::seed_catalog(&pool, s.machine_id).await;
    }
    sqlx::query("update machine_harnesses set provider='codex' where machine_id=$1")
        .bind(b.machine_id)
        .execute(&pool)
        .await?;
    let mut ca = connect(&hub, &a, Uuid::now_v7(), true, 1000).await;
    let mut cb = connect(&hub, &b, Uuid::now_v7(), true, 1000).await;
    let mut cc = connect(&hub, &c, Uuid::now_v7(), false, 1000).await;
    record(&pool, &a, &obs(&a, 960, Utc::now())).await;
    assert_eq!(
        limits::push_limits_snapshot(&pool, &hub.registry, "claude").await?,
        1
    );
    let down = tokio::time::timeout(WAIT, ca.stream.message())
        .await??
        .unwrap();
    let Some(control_down::Msg::LimitsSnapshot(s)) = down.msg else {
        panic!("snapshot")
    };
    assert_eq!(s.provider, "claude");
    assert_eq!(s.current[0].used_permille, 960);
    // Машина считает время от 1000 мс: время наблюдения приходит в её часах.
    assert!((1000..61000).contains(&s.current[0].observed_machine_ms));
    for stream in [&mut cb.stream, &mut cc.stream] {
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(80), stream.message())
                .await
                .is_err()
        );
    }
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limits_snapshot_is_sent_on_session_start(pool: PgPool) -> TestResult {
    let s = wire_seed(&pool).await;
    record(&pool, &s, &obs(&s, 960, Utc::now())).await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let mut c = connect(&hub, &s, Uuid::now_v7(), true, 1000).await;
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::MachineReport(report())),
    })
    .await?;
    let down = tokio::time::timeout(WAIT, c.stream.message())
        .await??
        .unwrap();
    let Some(control_down::Msg::LimitsSnapshot(snapshot)) = down.msg else {
        panic!("snapshot")
    };
    assert_eq!(snapshot.current[0].used_permille, 960);
    assert_eq!(snapshot.provider, "claude");
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_execution_carries_pilot_v1_only_when_the_sending_session_negotiated_it(
    pool: PgPool,
) -> TestResult {
    let s = wire_seed(&pool).await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let e = executions::start(&pool, s.owner_id, s.conversation_id, Uuid::now_v7()).await?;
    let row: canterlot_hub::outbox::CommandRow =
        sqlx::query_as("select * from commands where id=$1")
            .bind(e.command_id)
            .fetch_one(&pool)
            .await?;
    let _c = connect(&hub, &s, Uuid::now_v7(), true, 1000).await;
    let cmd = canterlot_hub::outbox::to_proto_for_session(&row, &hub.registry)?;
    let Some(command::Kind::StartExecution(start)) = cmd.kind else {
        panic!("start")
    };
    assert_eq!(start.capabilities, vec!["pilot_v1"]);
    let cmd = canterlot_hub::outbox::to_proto_for_session(&row, &registry())?;
    let Some(command::Kind::StartExecution(start)) = cmd.kind else {
        panic!("start")
    };
    assert!(start.capabilities.is_empty());
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(e.command_id)
        .fetch_one(&pool)
        .await?;
    assert!(payload.get("capabilities").is_none());
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_runs_keep_overlap_and_do_not_deadlock(pool: PgPool) -> TestResult {
    let mut a = Run::new(&pool).await;
    let mut b = Run::new(&pool).await;
    let ia = a.input(&pool).await;
    let ib = b.input(&pool).await;
    let before: i64 = sqlx::query_scalar(
        "select deadlocks from pg_stat_database where datname=current_database()",
    )
    .fetch_one(&pool)
    .await?;
    let ms = Utc::now().timestamp_millis();
    let fa = finished(&a, ia.command_id, ms + 1000);
    let fb = finished(&b, ib.command_id, ms + 1000);
    // Пересечение считается по часам хаба: оба хода сначала начинаются, затем одновременно завершаются.
    let both = async {
        tokio::join!(
            a.send(vec![accepted(ia.command_id, ms)]),
            b.send(vec![accepted(ib.command_id, ms)])
        );
        tokio::join!(a.send(vec![fa]), b.send(vec![fb]));
    };
    tokio::time::timeout(WAIT, both).await?;
    assert_eq!(
        count(&pool, "select count(*) from runs where overlapped").await,
        2
    );
    let after: i64 = sqlx::query_scalar(
        "select deadlocks from pg_stat_database where datname=current_database()",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(before, after);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unmapped_clock_keeps_history_without_changing_current_limits(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    r.send(vec![Body::LimitObserved(observation(
        960,
        Utc::now().timestamp_millis(),
    ))])
    .await;
    assert_eq!(
        count(&pool, "select count(*) from limit_snapshots").await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from provider_limits where weekly_observed_at is not null"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='limit_observation_unmapped'"
        )
        .await,
        1
    );
    assert_eq!(r.ingest.ignored_record_count(), 1);
    Ok(())
}

async fn begin_run(pool: &PgPool, r: &Run, ms: i64) -> SubmittedInput {
    let i = r.input(pool).await;
    let at = DateTime::from_timestamp_millis(ms).unwrap();
    let mut tx = pool.begin().await.unwrap();
    runs::start_in_tx(&mut tx, i.command_id, &r.reference(), at, at)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    i
}
async fn end_run(pool: &PgPool, r: &Run, i: &SubmittedInput, ms: i64, duration: u64) {
    let Body::RunFinished(mut fact) = finished(r, i.command_id, ms + duration as i64) else {
        unreachable!()
    };
    fact.duration_ms = duration;
    let at = DateTime::from_timestamp_millis(ms + duration as i64).unwrap();
    let mut tx = pool.begin().await.unwrap();
    runs::finish_in_tx(&mut tx, &fact, &r.reference(), at, at)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}
async fn overlapped(pool: &PgPool, i: &SubmittedInput) -> bool {
    sqlx::query_scalar("select overlapped from runs where input_command_id=$1")
        .bind(i.command_id)
        .fetch_one(pool)
        .await
        .unwrap()
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_late_finish_clears_the_overlap_of_runs_that_do_not_intersect(
    pool: PgPool,
) -> TestResult {
    let a = Run::new(&pool).await;
    let b = Run::new(&pool).await;
    let ms = Utc::now().timestamp_millis();
    let ia = begin_run(&pool, &a, ms).await;
    let ib = begin_run(&pool, &b, ms + 20000).await;
    end_run(&pool, &b, &ib, ms + 20000, 10000).await;
    assert!(overlapped(&pool, &ib).await);
    end_run(&pool, &a, &ia, ms, 10000).await;
    assert!(!overlapped(&pool, &ia).await);
    assert!(!overlapped(&pool, &ib).await);
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_run_started_inside_a_finished_one_flags_both(pool: PgPool) -> TestResult {
    let a = Run::new(&pool).await;
    let b = Run::new(&pool).await;
    let ms = Utc::now().timestamp_millis();
    let ia = begin_run(&pool, &a, ms).await;
    end_run(&pool, &a, &ia, ms, 10000).await;
    assert!(!overlapped(&pool, &ia).await);
    let ib = begin_run(&pool, &b, ms + 5000).await;
    assert!(overlapped(&pool, &ia).await);
    assert!(overlapped(&pool, &ib).await);
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn runs_of_unrelated_intervals_are_left_untouched(pool: PgPool) -> TestResult {
    let a = Run::new(&pool).await;
    let b = Run::new(&pool).await;
    let ms = Utc::now().timestamp_millis();
    let ia = begin_run(&pool, &a, ms).await;
    end_run(&pool, &a, &ia, ms, 1000).await;
    let ib = begin_run(&pool, &b, ms + 5000).await;
    end_run(&pool, &b, &ib, ms + 5000, 1000).await;
    assert_eq!(
        count(&pool, "select count(*) from runs where overlapped").await,
        0
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_finished_without_a_stored_accept_still_records_usage_as_inexact(
    pool: PgPool,
) -> TestResult {
    let mut r = Run::new(&pool).await;
    let i = r.input(&pool).await;
    let ms = Utc::now().timestamp_millis();
    r.send(vec![finished(&r, i.command_id, ms)]).await;
    let row: (bool, Option<i32>) = sqlx::query_as("select exact,weekly_before_permille from runs")
        .fetch_one(&pool)
        .await?;
    assert_eq!(row, (false, None));
    assert_eq!(
        count(&pool, "select count(*) from run_model_usage").await,
        1
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unknown_machine_time_of_an_accept_uses_the_ingest_time(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let i = r.input(&pool).await;
    r.send(vec![accepted(i.command_id, 0)]).await;
    let row: (bool, bool) =
        sqlx::query_as("select exact,abs(extract(epoch from now()-started_at))<30 from runs")
            .fetch_one(&pool)
            .await?;
    assert_eq!(row, (true, true));
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn usage_counts_the_weekly_delta_of_a_run_once_across_its_models(pool: PgPool) -> TestResult {
    let r = Run::new(&pool).await;
    stored_run(&pool, &r, Utc::now().timestamp_millis(), 1000, false).await?;
    sqlx::query("update runs set weekly_before_permille=900,weekly_after_permille=905")
        .execute(&pool)
        .await?;
    sqlx::query("insert into run_model_usage select input_command_id,'synthetic-large','main',300,400,0,0 from runs").execute(&pool).await?;
    let rows = limits::usage_by_day(
        &pool,
        r.seed.owner_id,
        None,
        Utc::now() - Duration::days(1),
        Utc::now() + Duration::days(1),
        "UTC",
    )
    .await?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().map(|u| u.weekly_delta_permille).sum::<i64>(), 5);
    assert_eq!(
        rows.iter()
            .find(|u| u.model == "synthetic-large")
            .unwrap()
            .weekly_delta_permille,
        5
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_negative_weekly_delta_after_a_window_reset_is_not_summed(pool: PgPool) -> TestResult {
    let r = Run::new(&pool).await;
    stored_run(&pool, &r, Utc::now().timestamp_millis(), 1000, false).await?;
    sqlx::query("update runs set weekly_before_permille=950,weekly_after_permille=20")
        .execute(&pool)
        .await?;
    let rows = limits::usage_by_day(
        &pool,
        r.seed.owner_id,
        None,
        Utc::now() - Duration::days(1),
        Utc::now() + Duration::days(1),
        "UTC",
    )
    .await?;
    assert_eq!(rows[0].weekly_delta_permille, 0);
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_equal_newer_reading_refreshes_the_time_without_limits_changed(
    pool: PgPool,
) -> TestResult {
    let s = testkit::seed(&pool).await;
    let at = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    record(&pool, &s, &obs(&s, 960, at)).await;
    let later = at + Duration::minutes(1);
    let r = record(&pool, &s, &obs(&s, 960, later)).await;
    assert!(!r.stale);
    assert!(!r.provider_changed);
    let seen: DateTime<Utc> = sqlx::query_scalar("select weekly_observed_at from provider_limits")
        .fetch_one(&pool)
        .await?;
    assert_eq!(seen, later);
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='limits_changed'"
        )
        .await,
        1
    );
    let moved = record(&pool, &s, &obs(&s, 961, later + Duration::minutes(1))).await;
    assert!(moved.provider_changed);
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_limit_hit_again_after_the_pause_was_cleared_pauses_again(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let hit = |reset: i64| {
        Body::LimitReached(LimitReached {
            execution_id: r.execution.execution_id.to_string(),
            provider: "claude".into(),
            window: LimitWindow::FiveHour as i32,
            resets_at_ms: reset as u64,
            ..Default::default()
        })
    };
    let now = Utc::now().timestamp_millis();
    let (first, second) = (hit(now + 100000), hit(now + 200000));
    r.send(vec![first]).await;
    sqlx::query("update queue_state set pause_reasons='[]'")
        .execute(&pool)
        .await?;
    r.send(vec![second]).await;
    let p: Value = sqlx::query_scalar("select pause_reasons from queue_state")
        .fetch_one(&pool)
        .await?;
    assert_eq!(p.as_array().unwrap().len(), 1);
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='limit_reached'"
        )
        .await,
        2
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_unknown_provider_is_invalid_and_leaves_no_provider_row(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let mut o = observation(960, Utc::now().timestamp_millis());
    o.provider = "bogus".into();
    let batch = r.batch(vec![Body::LimitObserved(o)]);
    let result = r
        .ingest
        .apply_batch(
            r.seed.machine_id,
            &r.seed.credential_digest(),
            JournalKind::Sj,
            &batch,
        )
        .await;
    assert!(matches!(result, Err(IngestError::InvalidRecord)));
    assert_eq!(
        count(
            &pool,
            "select count(*) from provider_limits where provider='bogus'"
        )
        .await,
        0
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_incomplete_block_gets_its_finish_time(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let id = Uuid::now_v7();
    r.send(vec![
        started(id, "tool_call"),
        Body::BlockIncomplete(BlockIncomplete {
            block_id: id.to_string(),
            rev: 1,
            reason: "interrupted".into(),
        }),
    ])
    .await;
    let ok: bool = sqlx::query_scalar("select b.finished_at=e.committed_at from blocks b join events e on e.payload->>'block_id'=b.block_id::text and e.kind='block_incomplete' where b.block_id=$1").bind(id).fetch_one(&pool).await?;
    assert!(ok);
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn absent_tool_name_and_summary_are_null_in_storage_and_events(pool: PgPool) -> TestResult {
    let mut r = Run::new(&pool).await;
    let id = Uuid::now_v7();
    r.send(vec![
        Body::BlockStarted(BlockStarted {
            block_id: id.to_string(),
            kind: "text".into(),
            ..Default::default()
        }),
        Body::PhaseChanged(PhaseChanged {
            execution_id: r.execution.execution_id.to_string(),
            phase: "generating".into(),
            ..Default::default()
        }),
    ])
    .await;
    let row: (String, String, Option<String>, Option<String>) =
        sqlx::query_as("select agent,model,tool_name,summary from blocks where block_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(row, ("main".into(), String::new(), None, None));
    let payload: Value =
        sqlx::query_scalar("select payload from events where kind='block_started'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(payload["agent"], "main");
    assert!(payload["tool_name"].is_null() && payload["summary"].is_null());
    let detail: Value = sqlx::query_scalar("select phase_detail from executions where id=$1")
        .bind(r.execution.execution_id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(
        detail,
        json!({"phase":"generating","tool_name":null,"summary":null})
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn zero_machine_time_has_no_hub_time(pool: PgPool) -> TestResult {
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let s = wire_seed(&pool).await;
    let _c = connect(&hub, &s, Uuid::now_v7(), true, 5000).await;
    assert!(hub.registry.to_hub_time(s.machine_id, 0).is_none());
    assert!(hub.registry.to_hub_time(s.machine_id, 5000).is_some());
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
