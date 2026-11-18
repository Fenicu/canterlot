use crate::support::task7::Fixture;

use axum::http::Method;
use canterlot_hub::{
    delivery::dispatch::{GateInputs, dispatch_next},
    limits::{DataConfidence, Observation, ObservationSource, Window, record_observation_in_tx},
    testkit::{self, FakeSession},
};
use canterlot_proto::worker::v1::{PrecheckResult, control_down, precheck_result};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn caps() -> Vec<&'static str> {
    vec![
        "pilot_v1",
        "instructions_v1",
        "session_tools_v1",
        "schedule_precheck_v1",
    ]
}

async fn create(f: &Fixture, target: Value, cron: &str) -> String {
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/schedules",
            Some(&format!("pipe-{}", Uuid::now_v7())),
            Some(json!({
                "name": "Проверка",
                "cron": cron,
                "target": target,
                "prompt": "посмотреть коммиты",
            })),
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    r.json::<Value>()["id"].as_str().unwrap().to_owned()
}

async fn due(pool: &PgPool, id: &str, at: chrono::DateTime<Utc>) {
    sqlx::query("update schedules set next_fire_at=$2 where id=$1")
        .bind(Uuid::parse_str(id).unwrap())
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
}

async fn state(pool: &PgPool, run_id: Uuid) -> (String, Value) {
    sqlx::query_as("select state,detail from schedule_runs where id=$1")
        .bind(run_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn run_id(pool: &PgPool, schedule_id: &str) -> Uuid {
    sqlx::query_scalar(
        "select id from schedule_runs where schedule_id=$1 order by created_at desc limit 1",
    )
    .bind(Uuid::parse_str(schedule_id).unwrap())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn wait_state(pool: &PgPool, id: Uuid, expected: &str) -> Value {
    for _ in 0..20_000 {
        let (state, detail) = state(pool, id).await;
        if state == expected {
            return detail;
        }
        tokio::task::yield_now().await;
    }
    let (state, detail) = state(pool, id).await;
    panic!("run never reached {expected}: {state}, {detail}");
}

fn existing(f: &Fixture) -> Value {
    json!({"kind": "existing_session", "conversation_id": f.seed.conversation_id})
}

fn scheduler(f: &Fixture) -> canterlot_hub::scheduler::Scheduler {
    canterlot_hub::scheduler::Scheduler::new(
        f.pool.clone(),
        f.state.dispatcher.clone(),
        Default::default(),
    )
    .with_limits(f.state.registry.clone(), Default::default())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn tick_claims_once_and_queues_one_input(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let now = Utc::now();
    due(&pool, &id, now - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(now).await.unwrap();
    let rid = run_id(&pool, &id).await;
    let detail = wait_state(&pool, rid, "started").await;
    assert!(detail.is_object(), "{detail}");
    let inputs: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and content->0->>'text'='посмотреть коммиты'",
    )
    .bind(f.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(inputs, 1);
    let runs: i64 = sqlx::query_scalar("select count(*) from schedule_runs where schedule_id=$1")
        .bind(Uuid::parse_str(&id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 1);
    scheduler(&f)
        .tick(now + TimeDelta::seconds(1))
        .await
        .unwrap();
    let runs: i64 = sqlx::query_scalar("select count(*) from schedule_runs where schedule_id=$1")
        .bind(Uuid::parse_str(&id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 1, "second tick must not re-claim");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn missed_occurrences_are_counted_in_one_run(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let id = create(&f, existing(&f), "0 9 * * *").await;
    // Фиксированный момент вместо `Utc::now()`: тест шёл через `now - 2 дня`,
    // и запуск в 09:00–09:30 по Москве видел сегодняшний 09:00 в окне
    // catch-up и заявлял `claimed` вместо `missed`.
    let now = chrono::DateTime::parse_from_rfc3339("2026-11-10T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    due(&pool, &id, now - TimeDelta::days(2)).await;
    scheduler(&f).tick(now).await.unwrap();
    let rid = run_id(&pool, &id).await;
    let (state, detail) = state(&pool, rid).await;
    assert_eq!(state, "missed");
    assert!(detail["missed_before"].as_i64().unwrap() >= 2, "{detail}");
    let runs: i64 = sqlx::query_scalar("select count(*) from schedule_runs where schedule_id=$1")
        .bind(Uuid::parse_str(&id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn overlap_and_offline_are_skipped(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    let now = Utc::now();
    sqlx::query(
        "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,idempotency_key)
         values($1,$2,$3,$4,1,'claimed',$5)",
    )
    .bind(Uuid::now_v7())
    .bind(schedule)
    .bind(f.seed.owner_id)
    .bind(now - TimeDelta::minutes(10))
    .bind("earlier")
    .execute(&pool)
    .await
    .unwrap();
    due(&pool, &id, now - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(now).await.unwrap();
    let rid = run_id(&pool, &id).await;
    let _detail = wait_state(&pool, rid, "skipped_overlap").await;

    // Без живого воркера запуск помечается offline.
    let id2 = create(&f, existing(&f), "0 9 * * *").await;
    due(&pool, &id2, now - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(now).await.unwrap();
    let rid2 = run_id(&pool, &id2).await;
    let _detail = wait_state(&pool, rid2, "skipped_machine_offline").await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn red_limits_window_skips_the_run(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let now = Utc::now();
    let mut tx = pool.begin().await.unwrap();
    record_observation_in_tx(
        &mut tx,
        f.seed.owner_id,
        &Observation {
            provider: "claude".into(),
            window: Window::Weekly,
            used_permille: 990,
            resets_at: Some(now + TimeDelta::days(2)),
            observed_at: now,
            source: ObservationSource::Session,
            data_confidence: DataConfidence::Exact,
            machine_id: f.seed.machine_id,
            execution_id: None,
        },
        now,
        &Default::default(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let id = create(&f, existing(&f), "0 9 * * *").await;
    due(&pool, &id, now - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(now).await.unwrap();
    let rid = run_id(&pool, &id).await;
    let (state, detail) = state(&pool, rid).await;
    assert_eq!(state, "skipped_limits", "{detail}");
    assert_eq!(detail["window"], "weekly", "{detail}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn precheck_exit_zero_starts_and_nonzero_skips(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let mut session: FakeSession =
        testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let registry = f.state.registry.clone();
    let machine = f.seed.machine_id;
    let connection = registry
        .session_info(machine)
        .expect("fake session registered")
        .connection_id;
    let worker = tokio::spawn(async move {
        while let Some(down) = session.commands.recv().await {
            if let Some(control_down::Msg::SchedulePrecheck(request)) = down.msg {
                let exit = if request.command.contains("fail") {
                    3
                } else {
                    0
                };
                registry.finish_precheck(
                    machine,
                    connection,
                    PrecheckResult {
                        attempt_id: request.attempt_id,
                        outcome: Some(precheck_result::Outcome::Finished(
                            canterlot_proto::worker::v1::PrecheckFinished {
                                exit_code: exit,
                                output_tail: "tail".into(),
                                timed_out: false,
                            },
                        )),
                    },
                );
            }
        }
    });
    let body = |command: &str| {
        json!({
            "name": "Проверка",
            "cron": "0 9 * * *",
            "target": existing(&f),
            "prompt": "посмотреть коммиты",
            "precheck": command,
        })
    };
    for command in ["true", "false # fail"] {
        let r = f
            .client
            .send(
                Method::POST,
                "/api/v1/schedules",
                Some(&format!("pre-{}", Uuid::now_v7())),
                Some(body(command)),
            )
            .await;
        assert_eq!(r.status.as_u16(), 200);
        let id = r.json::<Value>()["id"].as_str().unwrap().to_owned();
        due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
        scheduler(&f).tick(Utc::now()).await.unwrap();
        let rid = run_id(&pool, &id).await;
        let expected = if command.contains("fail") {
            "skipped_precheck"
        } else {
            "started"
        };
        let detail = wait_state(&pool, rid, expected).await;
        if expected == "skipped_precheck" {
            assert_eq!(detail["exit_code"], 3);
        }
    }
    worker.abort();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn new_session_target_creates_a_session_and_delivers_first_input(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let mut session: FakeSession =
        testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(f.seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    let target = json!({
        "kind": "new_session",
        "machine_id": f.seed.machine_id,
        "workdir": "/tmp/scheduled",
        "executor": {"harness": "synthetic", "model": "synthetic-small", "effort": "high"},
    });
    let id = create(&f, target, "0 9 * * *").await;
    let before: i64 = sqlx::query_scalar("select count(*) from conversations where owner_id=$1")
        .bind(f.seed.owner_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let rid = run_id(&pool, &id).await;
    wait_state(&pool, rid, "started").await;
    let after: i64 = sqlx::query_scalar("select count(*) from conversations where owner_id=$1")
        .bind(f.seed.owner_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, before + 1, "a new session was created");
    let session_id: Uuid = sqlx::query_scalar("select session_id from schedule_runs where id=$1")
        .bind(rid)
        .fetch_one(&pool)
        .await
        .unwrap();
    let input_id: Uuid = sqlx::query_scalar("select input_id from schedule_runs where id=$1")
        .bind(rid)
        .fetch_one(&pool)
        .await
        .unwrap();
    let execution: Uuid = sqlx::query_scalar(
        "select id from executions where conversation_id=$1 order by generation desc limit 1",
    )
    .bind(session_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    for state in [
        canterlot_hub::model::ExecutionState::Starting,
        canterlot_hub::model::ExecutionState::Running,
    ] {
        canterlot_hub::executions::set_state(&pool, execution, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    let outcome = dispatch_next(
        &pool,
        session_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let command = format!("{outcome:?}");
    assert!(
        command.contains(&input_id.to_string()),
        "first input dispatched: {command}"
    );
    let delivered: i64 = sqlx::query_scalar(
        "select count(*) from commands where execution_id=$1 and delivery_state in ('pending','dispatched','on_machine')",
    )
    .bind(execution)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        delivered >= 1,
        "the first input became a command for the worker"
    );
    let _ = &mut session;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn rollback_before_claim_commit_reclaims_exactly_once(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let id = create(&f, existing(&f), "0 9 * * 1-5").await;
    let now = Utc::now();
    due(&pool, &id, now - TimeDelta::seconds(30)).await;
    let mut tx = pool.begin().await.unwrap();
    let claimed = canterlot_hub::schedules::claim_due(&mut tx, now, 32)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    tx.rollback().await.unwrap();
    let runs: i64 = sqlx::query_scalar("select count(*) from schedule_runs where schedule_id=$1")
        .bind(Uuid::parse_str(&id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 0, "rolled-back claim left no run");
    scheduler(&f).tick(now).await.unwrap();
    let runs: i64 = sqlx::query_scalar("select count(*) from schedule_runs where schedule_id=$1")
        .bind(Uuid::parse_str(&id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 1, "next tick claims once");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn replay_after_dispatch_commit_does_not_duplicate_work(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(f.seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    let target = json!({
        "kind": "new_session",
        "machine_id": f.seed.machine_id,
        "workdir": "/tmp/scheduled-replay",
        "executor": {"harness": "synthetic", "model": "synthetic-small", "effort": "high"},
    });
    let id = create(&f, target, "0 9 * * 1-5").await;
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let rid = run_id(&pool, &id).await;
    wait_state(&pool, rid, "started").await;
    let session_id: Uuid = sqlx::query_scalar("select session_id from schedule_runs where id=$1")
        .bind(rid)
        .fetch_one(&pool)
        .await
        .unwrap();
    let input_id: Uuid = sqlx::query_scalar("select input_id from schedule_runs where id=$1")
        .bind(rid)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("update inputs set state='dispatched' where id=$1")
        .bind(input_id)
        .execute(&pool)
        .await
        .unwrap();
    let (sessions_before, inputs_before) = (
        sqlx::query_scalar::<_, i64>("select count(*) from conversations where owner_id=$1")
            .bind(f.seed.owner_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        sqlx::query_scalar::<_, i64>("select count(*) from inputs where owner_id=$1")
            .bind(f.seed.owner_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
    );
    let mut conn = pool.acquire().await.unwrap();
    let schedule =
        canterlot_hub::schedules::get(&mut conn, f.seed.owner_id, Uuid::parse_str(&id).unwrap())
            .await
            .unwrap()
            .unwrap();
    let run = canterlot_hub::schedules::get_run(&mut conn, f.seed.owner_id, rid)
        .await
        .unwrap()
        .unwrap();
    drop(conn);
    let sch = scheduler(&f);
    canterlot_hub::schedules::admission::admit(
        &sch,
        canterlot_hub::schedules::ClaimedRun { run, schedule },
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from conversations where owner_id=$1")
            .bind(f.seed.owner_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        sessions_before,
        "no second session"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from inputs where owner_id=$1")
            .bind(f.seed.owner_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        inputs_before,
        "no duplicate input"
    );
    assert_eq!(
        sqlx::query_scalar::<_, Uuid>("select input_id from schedule_runs where id=$1")
            .bind(rid)
            .fetch_one(&pool)
            .await
            .unwrap(),
        input_id
    );
    let _ = session_id;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_now_is_idempotent_and_run_anyway_bypasses_checks(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let now = Utc::now();
    let mut tx = pool.begin().await.unwrap();
    record_observation_in_tx(
        &mut tx,
        f.seed.owner_id,
        &Observation {
            provider: "claude".into(),
            window: Window::Weekly,
            used_permille: 990,
            resets_at: Some(now + TimeDelta::days(2)),
            observed_at: now,
            source: ObservationSource::Session,
            data_confidence: DataConfidence::Exact,
            machine_id: f.seed.machine_id,
            execution_id: None,
        },
        now,
        &Default::default(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/schedules",
            Some("rn-create"),
            Some(json!({
                "name": "Ручной",
                "cron": "0 9 * * 1-5",
                "target": existing(&f),
                "prompt": "вручную",
                "precheck": "false",
            })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200);
    let id = r.json::<Value>()["id"].as_str().unwrap().to_owned();
    let first = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/schedules/{id}/run"),
            Some("rn-run"),
            Some(json!({"run_anyway": true})),
        )
        .await;
    assert_eq!(
        first.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&first.body)
    );
    let first_id = first.json::<Value>()["id"].as_str().unwrap().to_owned();
    assert_eq!(first.json::<Value>()["state"], "started");
    let replay = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/schedules/{id}/run"),
            Some("rn-run"),
            Some(json!({"run_anyway": true})),
        )
        .await;
    assert_eq!(
        replay.json::<Value>()["id"],
        first_id,
        "client key is idempotent"
    );
    let normal = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/schedules/{id}/run"),
            Some("rn-normal"),
            Some(json!({"run_anyway": false})),
        )
        .await;
    assert_eq!(normal.status.as_u16(), 200);
    assert_eq!(normal.json::<Value>()["state"], "skipped_limits");
}
