use crate::support::task7::Fixture;

use axum::http::Method;
use canterlot_hub::{
    delivery::dispatch::{GateInputs, dispatch_next},
    limits::{
        self, DataConfidence, Observation, ObservationSource, Window, record_observation_in_tx,
    },
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

pub(crate) fn scheduler(f: &Fixture) -> canterlot_hub::scheduler::Scheduler {
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
    // Приёмка идёт вне тика: ждать исхода ограниченным опросом, а не разовым чтением.
    let detail = wait_state(&pool, rid, "skipped_limits").await;
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
async fn stranded_claimed_is_readmitted_and_young_prechecking_survives(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    let now = Utc::now();
    // Обрыв после commit заявки, до dispatch: строка `claimed`, приёмка погибла.
    let rid = Uuid::now_v7();
    sqlx::query(
        "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key)
         values($1,$2,$3,$4,1,'claimed',false,$5)",
    )
    .bind(rid)
    .bind(schedule)
    .bind(f.seed.owner_id)
    .bind(now - TimeDelta::hours(2))
    .bind("crashed-claim")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("update schedule_runs set updated_at=$2 where id=$1")
        .bind(rid)
        .bind(now - TimeDelta::hours(1))
        .execute(&pool)
        .await
        .unwrap();
    // Молодая `prechecking` чужой живой приёмки — не наша: тик её не трогает.
    // На другом расписании, чтобы новое правило пересечений (только более
    // ранние запуски блокируют) не мешало возврату.
    let other = create(&f, existing(&f), "0 10 * * *").await;
    let young = Uuid::now_v7();
    sqlx::query(
        "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key)
         values($1,$2,$3,$4,1,'prechecking',false,$5)",
    )
    .bind(young)
    .bind(Uuid::parse_str(&other).unwrap())
    .bind(f.seed.owner_id)
    .bind(now - TimeDelta::hours(3))
    .bind("live-prechecking")
    .execute(&pool)
    .await
    .unwrap();
    scheduler(&f).tick(now).await.unwrap();
    wait_state(&pool, rid, "started").await;
    let (state, _): (String, Value) = state(&pool, young).await;
    assert_eq!(state, "prechecking", "young prechecking is not reaped");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn interrupted_prechecking_is_failed_without_rerun(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    let now = Utc::now();
    // Обрыв посреди предпроверки: воркер уже не ответит, повтор запрещён.
    let rid = Uuid::now_v7();
    sqlx::query(
        "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key,detail)
         values($1,$2,$3,$4,1,'prechecking',false,$5,'{}')",
    )
    .bind(rid)
    .bind(schedule)
    .bind(f.seed.owner_id)
    .bind(now - TimeDelta::hours(2))
    .bind("crashed-prechecking")
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("update schedule_runs set updated_at=$2 where id=$1")
        .bind(rid)
        .bind(now - TimeDelta::hours(1))
        .execute(&pool)
        .await
        .unwrap();
    scheduler(&f).tick(now).await.unwrap();
    let (state, detail) = state(&pool, rid).await;
    assert_eq!(state, "failed");
    assert_eq!(detail["reason"], "precheck_result_unknown", "{detail}");
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn failed_runs_push_once_per_schedule_per_hour(pool: PgPool) {
    use canterlot_hub::notify::{self, PushSettings};
    notify::configure(
        &pool,
        Some(&PushSettings {
            waiting: true,
            turn_finished: true,
            failures: true,
            timezone: "UTC".into(),
        }),
    )
    .await
    .unwrap();
    let f = Fixture::new(pool.clone()).await;
    // Несуществующий разговор: приём падает до воркера.
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/schedules",
            Some("fp-create"),
            Some(json!({
                "name": "Битая цель",
                "cron": "0 9 * * *",
                "target": {"kind": "existing_session", "conversation_id": Uuid::now_v7()},
                "prompt": "не дойдёт",
            })),
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let id = r.json::<Value>()["id"].as_str().unwrap().to_owned();
    for round in 0..2 {
        due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
        scheduler(&f).tick(Utc::now()).await.unwrap();
        let rid = run_id(&pool, &id).await;
        wait_state(&pool, rid, "failed").await;
        let pushes: i64 = sqlx::query_scalar(
            "select count(*) from owner_pushes where kind='schedule_failed'
             and payload->>'schedule_id'=$1",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pushes, 1, "round {round}: one push per rolling hour");
    }
    let payload: Value = sqlx::query_scalar(
        "select payload from owner_pushes where kind='schedule_failed'
         and payload->>'schedule_id'=$1",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(payload["schedule_name"], "Битая цель");
    assert!(payload["reason"].as_str().is_some());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_run_now_with_one_key_dispatches_once(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/schedules",
            Some("cc-create"),
            Some(json!({
                "name": "Гонка",
                "cron": "0 9 * * 1-5",
                "target": existing(&f),
                "prompt": "вместе",
            })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200);
    let id = r.json::<Value>()["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v1/schedules/{id}/run");
    let (first, second) = tokio::join!(
        f.client.send(
            Method::POST,
            &path,
            Some("cc-run"),
            Some(json!({"run_anyway": false}))
        ),
        f.client.send(
            Method::POST,
            &path,
            Some("cc-run"),
            Some(json!({"run_anyway": false}))
        ),
    );
    assert_eq!(
        first.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&first.body)
    );
    assert_eq!(
        second.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&second.body)
    );
    assert_eq!(
        first.json::<Value>()["id"],
        second.json::<Value>()["id"],
        "one key resolves to one durable run"
    );
    let runs: i64 = sqlx::query_scalar("select count(*) from schedule_runs where schedule_id=$1")
        .bind(Uuid::parse_str(&id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 1, "no second run row");
    let inputs: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and content->0->>'text'='вместе'",
    )
    .bind(f.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(inputs, 1, "no duplicate input");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn undelivered_input_and_working_session_block_overlap(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let id = create(&f, existing(&f), "0 9 * * *").await;
    // Первый запуск уходит в работу, ввод никем не забирается.
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let first = run_id(&pool, &id).await;
    wait_state(&pool, first, "started").await;
    // Второй тик видит недоставленный ввод первого запуска.
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let second = run_id(&pool, &id).await;
    assert_ne!(first, second);
    wait_state(&pool, second, "skipped_overlap").await;
    // Работающая сессия тоже блокирует, даже если ввод уже приняли.
    let input: Uuid = sqlx::query_scalar("select input_id from schedule_runs where id=$1")
        .bind(first)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("update inputs set state='accepted' where id=$1")
        .bind(input)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into conversation_derived(conversation_id, activity_state)
         values($1,'working')
         on conflict(conversation_id) do update set activity_state='working'",
    )
    .bind(f.seed.conversation_id)
    .execute(&pool)
    .await
    .unwrap();
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let third = run_id(&pool, &id).await;
    assert_ne!(second, third);
    wait_state(&pool, third, "skipped_overlap").await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn acceptance_unconfirmed_input_blocks_overlap(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let id = create(&f, existing(&f), "0 9 * * *").await;
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let first = run_id(&pool, &id).await;
    wait_state(&pool, first, "started").await;
    let input: Uuid = sqlx::query_scalar("select input_id from schedule_runs where id=$1")
        .bind(first)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("update inputs set state='acceptance_unconfirmed' where id=$1")
        .bind(input)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update conversation_derived set activity_state='idle' where conversation_id=$1")
        .bind(f.seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let second = run_id(&pool, &id).await;
    assert_ne!(first, second);
    wait_state(&pool, second, "skipped_overlap").await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn weekly_reserve_hold_skips_new_session_run(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    sqlx::query(
        "insert into provider_limits(provider, weekly_reserve_active)
         values('claude', true)
         on conflict(provider) do update set weekly_reserve_active=true",
    )
    .execute(&pool)
    .await
    .unwrap();
    let target = json!({
        "kind": "new_session",
        "machine_id": f.seed.machine_id,
        "workdir": "/tmp/scheduled-reserve",
        "executor": {"harness": "synthetic", "model": "synthetic-small", "effort": "high"},
    });
    let id = create(&f, target, "0 9 * * *").await;
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let rid = run_id(&pool, &id).await;
    // Приёмка идёт вне тика: ждать исхода ограниченным опросом, а не разовым чтением.
    let detail = wait_state(&pool, rid, "skipped_limits").await;
    assert_eq!(detail["reason"], "initial_hold", "{detail}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn schedules_deny_unauthenticated_callers(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let anonymous = canterlot_hub::testkit::TestClient::in_process(canterlot_hub::http::router(
        f.state.clone(),
    ))
    .with_header("Origin", canterlot_hub::testkit::TEST_ORIGIN)
    .with_header("X-Canterlot-Client", "1");
    for (method, path) in [
        (Method::GET, "/api/v1/schedules".to_owned()),
        (Method::POST, "/api/v1/schedules".to_owned()),
        (
            Method::GET,
            format!("/api/v1/schedules/{}/runs", Uuid::now_v7()),
        ),
    ] {
        let refused = anonymous.send(method.clone(), &path, None, None).await;
        assert_eq!(refused.status.as_u16(), 401, "{path}");
    }
    let forged = canterlot_hub::testkit::TestClient::in_process(canterlot_hub::http::router(
        f.state.clone(),
    ))
    .with_header("Origin", canterlot_hub::testkit::TEST_ORIGIN)
    .with_header("X-Canterlot-Client", "1")
    .with_session("no-such-session");
    let refused = forged
        .send(Method::GET, "/api/v1/schedules", None, None)
        .await;
    assert_eq!(refused.status.as_u16(), 401);
}

/// A: пока приёмка этого хаба держит запуск, восстановление его не тронет.
/// Снятие стража делает ту же старую строку кандидатом на возврат.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn recovery_skips_a_run_being_admitted_by_this_hub(pool: PgPool) {
    use canterlot_hub::schedules::{AdmittingGuard, RECLAIM_LEASE, recover_interrupted};

    let f = Fixture::new(pool.clone()).await;
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let now = Utc::now();
    let old = now - TimeDelta::hours(2);
    let rid = Uuid::now_v7();
    sqlx::query(
        "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key,attempt_id,lease_at)
         values($1,$2,$3,$4,1,'claimed',false,$5,$6,$7)",
    )
    .bind(rid)
    .bind(Uuid::parse_str(&id).unwrap())
    .bind(f.seed.owner_id)
    .bind(old)
    .bind("guarded-reclaim")
    .bind(Uuid::now_v7())
    .bind(old)
    .execute(&pool)
    .await
    .unwrap();

    let recover = |pool: PgPool| async move {
        recover_interrupted(&pool, Some(RECLAIM_LEASE), now)
            .await
            .unwrap()
    };
    let guard = AdmittingGuard::insert(rid);
    let first = recover(pool.clone()).await;
    assert!(
        !first.iter().any(|run| run.run.id == rid),
        "a run admitted by this hub is not reclaimed"
    );
    drop(guard);
    let second = recover(pool.clone()).await;
    assert!(
        second.iter().any(|run| run.run.id == rid),
        "after the guard is gone the stale run is reclaimed"
    );
}

/// B/2: пока удаление второго расписания держит его замок, восстановление
/// первого обязано завершиться и отпустить счётчик событий владельца: один
/// короткий проход на расписание, без цикла ожидания и без отката пачки.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn recovery_of_one_schedule_does_not_hold_the_event_counter_for_the_next(pool: PgPool) {
    use canterlot_hub::schedules::{RECLAIM_LEASE, recover_interrupted};
    use std::time::Duration;

    let f = Fixture::new(pool.clone()).await;
    let a = Uuid::parse_str(&create(&f, existing(&f), "0 9 * * *").await).unwrap();
    let b = Uuid::parse_str(&create(&f, existing(&f), "0 10 * * *").await).unwrap();
    assert!(a < b, "candidates are visited in schedule id order");
    let now = Utc::now();
    let mut runs = Vec::new();
    for (n, schedule) in [a, b].into_iter().enumerate() {
        let rid = Uuid::now_v7();
        sqlx::query(
            "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key,attempt_id,lease_at)
             values($1,$2,$3,$4,1,'prechecking',false,$5,$6,$7)",
        )
        .bind(rid)
        .bind(schedule)
        .bind(f.seed.owner_id)
        .bind(now - TimeDelta::hours(2))
        .bind(format!("two-schedules-{n}"))
        .bind(Uuid::now_v7())
        .bind(now - TimeDelta::hours(2))
        .execute(&pool)
        .await
        .unwrap();
        runs.push(rid);
    }
    // «Удаление» B: держит замок расписания и будет писать событие.
    let mut deleting = pool.begin().await.unwrap();
    sqlx::query("select id from schedules where id=$1 for update")
        .bind(b)
        .fetch_one(&mut *deleting)
        .await
        .unwrap();
    let recovery = {
        let pool = pool.clone();
        tokio::spawn(async move {
            recover_interrupted(&pool, Some(RECLAIM_LEASE), now)
                .await
                .unwrap()
        })
    };
    // Восстановление A закоммитилось, хотя B ещё занято удалением.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if state(&pool, runs[0]).await.0 == "failed" {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recovery of A must commit while B is locked by a delete");
    // Удаление пишет событие владельца (счётчик свободен) и завершается.
    canterlot_hub::events::append(
        &mut deleting,
        canterlot_hub::events::NewEvent {
            scope: canterlot_hub::events::Scope::Schedules {
                owner_id: f.seed.owner_id,
            },
            kind: "schedule_changed",
            conversation_id: None,
            execution_id: None,
            payload: json!({"deleted": true}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    sqlx::query("delete from schedules where id=$1")
        .bind(b)
        .execute(&mut *deleting)
        .await
        .unwrap();
    deleting.commit().await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), recovery)
        .await
        .expect("recovery finishes after the delete")
        .unwrap();
    assert_eq!(
        state(&pool, runs[0]).await.0,
        "failed",
        "A's recovery was kept"
    );
    let remaining: i64 = sqlx::query_scalar("select count(*) from schedule_runs where id=$1")
        .bind(runs[1])
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(remaining, 0, "delete cascaded to B's run");
}

/// 14: `started` и его событие живут в одной транзакции — по завершении
/// приёмки состояние без события невозможно.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn started_and_its_event_commit_together(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let id = create(&f, existing(&f), "0 9 * * *").await;
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let rid = run_id(&pool, &id).await;
    wait_state(&pool, rid, "started").await;
    let events: Vec<Value> = sqlx::query_scalar(
        "select payload from events where kind='schedule_run_changed'
          and payload->>'id'=$1",
    )
    .bind(rid.to_string())
    .fetch_all(&pool)
    .await
    .unwrap();
    let started: Vec<&Value> = events
        .iter()
        .filter(|event| event["state"] == "started")
        .collect();
    assert_eq!(started.len(), 1, "exactly one started event: {events:?}");
}

/// 16: маршрутизированная цель нового сеанса проходит весь путь через тик:
/// заявка, выбор исполнителя по политике, creation и первый ввод.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn routed_new_session_target_runs_through_the_tick(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(f.seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update owners set routing_policy=$2, spawn_whitelist=$3 where id=$1")
        .bind(f.seed.owner_id)
        .bind(json!({
            "task_types": {
                "audit": {"title": "Проверка", "candidates": [
                    {"harness":"synthetic","model":"synthetic-small","effort":"high"}
                ]}
            }
        }))
        .bind(json!([{"harness":"synthetic","model":"synthetic-small"}]))
        .execute(&pool)
        .await
        .unwrap();
    let target = json!({
        "kind": "new_session",
        "machine_id": f.seed.machine_id,
        "workdir": "/tmp/scheduled-routed",
        "route": {"task_type": "audit"},
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
    assert_eq!(after, before + 1, "the routed session was created");
    let (harness, model): (String, String) = sqlx::query_as(
        "select c.harness,c.model from conversations c
          join schedule_runs r on r.session_id=c.id where r.id=$1",
    )
    .bind(rid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(harness, "synthetic");
    assert_eq!(model, "synthetic-small");
    let inputs: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=(select session_id from schedule_runs where id=$1)
          and content->0->>'text'='посмотреть коммиты'",
    )
    .bind(rid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(inputs, 1, "the routed session got its first input");
}

/// 1: ключ клиента Run Now, совпавший с ключом будущего срабатывания другого
/// расписания, не останавливает пачку заявок тика.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_now_key_equal_to_a_future_occurrence_key_does_not_stall_the_tick(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let a = create(&f, existing(&f), "0 9 * * *").await;
    let b = create(&f, existing(&f), "0 10 * * *").await;
    let now = Utc::now();
    due(&pool, &b, now - TimeDelta::seconds(30)).await;
    let b_next: chrono::DateTime<Utc> =
        sqlx::query_scalar("select next_fire_at from schedules where id=$1")
            .bind(Uuid::parse_str(&b).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    let crafted = format!("schedule:{b}:{}", b_next.to_rfc3339());
    // По HTTP ключ с «:» не пройдёт проверку формата, поэтому резерв берём
    // напрямую: инвариант хранилища не должен зависеть от формата заголовка.
    let reserved = canterlot_hub::schedules::admission::reserve_manual(
        &pool,
        f.seed.owner_id,
        Uuid::parse_str(&a).unwrap(),
        &crafted,
        true,
    )
    .await
    .unwrap();
    assert!(matches!(
        reserved,
        canterlot_hub::schedules::admission::Reservation::Fresh { .. }
    ));
    scheduler(&f).tick(now).await.unwrap();
    let rid = run_id(&pool, &b).await;
    wait_state(&pool, rid, "started").await;
}

async fn inputs_with_text(pool: &PgPool, text: &str) -> i64 {
    sqlx::query_scalar("select count(*) from inputs where content->0->>'text'=$1")
        .bind(text)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// 3: ответ Run Now потерян после commit dispatch; повтор по тому же ключу
/// возвращает начатый запуск, даже когда журнал запусков уже обрезан.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lost_run_now_response_replays_the_started_run_after_pruning(pool: PgPool) {
    use canterlot_hub::schedules::fault::{self, Fault};

    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    fault::arm(schedule, Fault::LostRunNowResponse);
    let path = format!("/api/v1/schedules/{id}/run");
    let lost = f
        .client
        .send(
            Method::POST,
            &path,
            Some("lost-key"),
            Some(json!({"run_anyway": true})),
        )
        .await;
    assert_ne!(lost.status.as_u16(), 200, "the first response is lost");
    // 200 более новых запусков вытесняют исходную строку из журнала.
    for n in 0..201 {
        sqlx::query(
            "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key)
             values($1,$2,$3,now()+make_interval(secs=>$4),1,'skipped_overlap',false,$5)",
        )
        .bind(Uuid::now_v7())
        .bind(schedule)
        .bind(f.seed.owner_id)
        .bind(f64::from(n + 10))
        .bind(format!("newer-{n}"))
        .execute(&pool)
        .await
        .unwrap();
    }
    let mut conn = pool.acquire().await.unwrap();
    canterlot_hub::schedules::prune_runs(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let replay = f
        .client
        .send(
            Method::POST,
            &path,
            Some("lost-key"),
            Some(json!({"run_anyway": true})),
        )
        .await;
    assert_eq!(
        replay.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&replay.body)
    );
    assert_eq!(replay.json::<Value>()["state"], "started");
    assert_eq!(inputs_with_text(&pool, "посмотреть коммиты").await, 1);
}

/// 3: другая мутация API с тем же ключом получает конфликт, пока запуск идёт,
/// и Run Now не отвечает 409 после начатой сессии.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn foreign_mutation_cannot_take_a_reserved_run_now_key(pool: PgPool) {
    use canterlot_hub::schedules::admission::{Reservation, reserve_manual};

    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    let Reservation::Fresh { .. } =
        reserve_manual(&pool, f.seed.owner_id, schedule, "shared-key", true)
            .await
            .unwrap()
    else {
        panic!("first reservation is fresh");
    };
    let other = f
        .client
        .send(
            Method::POST,
            "/api/v1/schedules",
            Some("shared-key"),
            Some(json!({
                "name": "Чужой",
                "cron": "0 8 * * *",
                "target": existing(&f),
                "prompt": "чужой",
            })),
        )
        .await;
    assert_eq!(
        other.status.as_u16(),
        409,
        "{}",
        String::from_utf8_lossy(&other.body)
    );
}

/// 3: заявленный ручной запуск, который доводит восстановление, тоже
/// фиксирует ответ ключа: повтор получает начатый запуск, не вторую сессию.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn recovery_dispatched_manual_claim_finalizes_the_key(pool: PgPool) {
    use canterlot_hub::schedules::admission::{Reservation, reserve_manual};

    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    let Reservation::Fresh { run, .. } =
        reserve_manual(&pool, f.seed.owner_id, schedule, "stranded-manual", true)
            .await
            .unwrap()
    else {
        panic!("fresh");
    };
    // Процесс умер после резерва: аренда давно истекла.
    sqlx::query("update schedule_runs set lease_at=now()-interval '2 hours',updated_at=now()-interval '2 hours' where id=$1")
        .bind(run.id)
        .execute(&pool)
        .await
        .unwrap();
    scheduler(&f).tick(Utc::now()).await.unwrap();
    wait_state(&pool, run.id, "started").await;
    let replay = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/schedules/{id}/run"),
            Some("stranded-manual"),
            Some(json!({"run_anyway": true})),
        )
        .await;
    assert_eq!(
        replay.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&replay.body)
    );
    assert_eq!(replay.json::<Value>()["id"], run.id.to_string());
    assert_eq!(replay.json::<Value>()["state"], "started");
    assert_eq!(inputs_with_text(&pool, "посмотреть коммиты").await, 1);
}

/// 5: охраняемые строки не расходуют предел пачки восстановления:
/// брошенная заявка позади 32 охраняемых всё равно возвращается.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn guarded_runs_do_not_starve_recovery_of_an_abandoned_claim(pool: PgPool) {
    use canterlot_hub::schedules::{AdmittingGuard, RECLAIM_LEASE, recover_interrupted};

    let f = Fixture::new(pool.clone()).await;
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    let now = Utc::now();
    let mut guards = Vec::new();
    let mut abandoned = Uuid::nil();
    for n in 0..33 {
        let rid = Uuid::now_v7();
        sqlx::query(
            "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key,attempt_id,lease_at,updated_at)
             values($1,$2,$3,$4,1,'claimed',false,$5,$6,$7,$7)",
        )
        .bind(rid)
        .bind(schedule)
        .bind(f.seed.owner_id)
        .bind(now - TimeDelta::hours(5) + TimeDelta::minutes(n))
        .bind(format!("starve-{n}"))
        .bind(Uuid::now_v7())
        .bind(now - TimeDelta::hours(3) + TimeDelta::minutes(n))
        .execute(&pool)
        .await
        .unwrap();
        if n < 32 {
            guards.push(AdmittingGuard::insert(rid));
        } else {
            abandoned = rid;
        }
    }
    let recovered = recover_interrupted(&pool, Some(RECLAIM_LEASE), now)
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].run.id, abandoned);
    drop(guards);
}

/// 5: сбой базы в начале dispatch после предпроверки пишет итог из
/// `prechecking`, а не пытается писать из `claimed` (запись ушла бы в пустоту).
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn early_dispatch_error_after_precheck_finalizes_from_prechecking(pool: PgPool) {
    use canterlot_hub::schedules::fault::{self, Fault};

    let f = Fixture::new(pool.clone()).await;
    let mut session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let registry = f.state.registry.clone();
    let machine = f.seed.machine_id;
    let connection = registry.session_info(machine).unwrap().connection_id;
    let worker = tokio::spawn(async move {
        while let Some(down) = session.commands.recv().await {
            if let Some(control_down::Msg::SchedulePrecheck(request)) = down.msg {
                registry.finish_precheck(
                    machine,
                    connection,
                    PrecheckResult {
                        attempt_id: request.attempt_id,
                        outcome: Some(precheck_result::Outcome::Finished(
                            canterlot_proto::worker::v1::PrecheckFinished {
                                exit_code: 0,
                                output_tail: String::new(),
                                timed_out: false,
                            },
                        )),
                    },
                );
            }
        }
    });
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/schedules",
            Some("early-dispatch"),
            Some(json!({
                "name": "Ранний сбой",
                "cron": "0 9 * * *",
                "target": existing(&f),
                "prompt": "не дойдёт",
                "precheck": "true",
            })),
        )
        .await;
    let id = r.json::<Value>()["id"].as_str().unwrap().to_owned();
    fault::arm(Uuid::parse_str(&id).unwrap(), Fault::DispatchBeginError);
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let rid = run_id(&pool, &id).await;
    let detail = wait_state(&pool, rid, "failed").await;
    assert_eq!(detail["reason"], "database_unavailable");
    worker.abort();
}

async fn observe(pool: &PgPool, f: &Fixture, provider: &str, window: limits::Window, used: u16) {
    let now = Utc::now();
    let mut tx = pool.begin().await.unwrap();
    record_observation_in_tx(
        &mut tx,
        f.seed.owner_id,
        &Observation {
            provider: provider.into(),
            window,
            used_permille: used,
            resets_at: Some(now + TimeDelta::hours(3)),
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
}

async fn routed_fixture(f: &Fixture, pool: &PgPool, candidates: Value, whitelist: Value) {
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(f.seed.machine_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into machine_harnesses(machine_id,harness,provider,title,permission_modes,models)
         values($1,'synthetic-codex','codex','Codex','[\"bypass\",\"owner\"]','[{\"id\":\"synthetic-codex-small\",\"title\":\"S\",\"efforts\":[\"high\"]}]')
         on conflict do nothing",
    )
    .bind(f.seed.machine_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("update owners set routing_policy=$2, spawn_whitelist=$3 where id=$1")
        .bind(f.seed.owner_id)
        .bind(json!({"task_types": {"audit": {"title": "Проверка", "candidates": candidates}}}))
        .bind(whitelist)
        .execute(pool)
        .await
        .unwrap();
}

/// F: основной кандидат без порога дочерней политики, но с красным окном по
/// правилам приёма, не выбирается: зелёный запасной стартует.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn routing_skips_a_red_primary_without_child_thresholds_for_a_green_backup(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    routed_fixture(
        &f,
        &pool,
        json!([
            {"harness":"synthetic","model":"synthetic-small","effort":"high"},
            {"harness":"synthetic-codex","model":"synthetic-codex-small","effort":"high"}
        ]),
        json!([
            {"harness":"synthetic","model":"synthetic-small"},
            {"harness":"synthetic-codex","model":"synthetic-codex-small"}
        ]),
    )
    .await;
    observe(&pool, &f, "claude", limits::Window::FiveHour, 970).await;
    let id = create(
        &f,
        json!({"kind":"new_session","machine_id":f.seed.machine_id,"workdir":"/tmp/scheduled-backup","route":{"task_type":"audit"}}),
        "0 9 * * *",
    )
    .await;
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let rid = run_id(&pool, &id).await;
    wait_state(&pool, rid, "started").await;
    let harness: String = sqlx::query_scalar(
        "select c.harness from conversations c join schedule_runs r on r.session_id=c.id where r.id=$1",
    )
    .bind(rid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(harness, "synthetic-codex");
}

/// F: кандидат стал занятым после маршрутизации (parallel limit under locks):
/// dispatch выбирает запасного, а не пропускает запуск.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn candidate_that_becomes_busy_after_routing_falls_back(pool: PgPool) {
    use canterlot_hub::schedules::fault::{self, Fault};

    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    routed_fixture(
        &f,
        &pool,
        json!([
            {"harness":"synthetic","model":"synthetic-small","effort":"high"},
            {"harness":"synthetic-codex","model":"synthetic-codex-small","effort":"high"}
        ]),
        json!([
            {"harness":"synthetic","model":"synthetic-small"},
            {"harness":"synthetic-codex","model":"synthetic-codex-small"}
        ]),
    )
    .await;
    sqlx::query("update owners set routing_policy=jsonb_set(routing_policy,'{max_parallel}','{\"synthetic/synthetic-small\":1}') where id=$1")
        .bind(f.seed.owner_id)
        .execute(&pool)
        .await
        .unwrap();
    let id = create(
        &f,
        json!({"kind":"new_session","machine_id":f.seed.machine_id,"workdir":"/tmp/scheduled-busy","route":{"task_type":"audit"}}),
        "0 9 * * *",
    )
    .await;
    // Пара занята только к моменту dispatch: чужая работающая сессия появляется
    // между маршрутизацией и транзакцией dispatch.
    fault::arm(Uuid::parse_str(&id).unwrap(), Fault::BusyAfterRouting);
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let rid = run_id(&pool, &id).await;
    wait_state(&pool, rid, "started").await;
    let harness: String = sqlx::query_scalar(
        "select c.harness from conversations c join schedule_runs r on r.session_id=c.id where r.id=$1",
    )
    .bind(rid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(harness, "synthetic-codex");
}

/// 4: пятичасовое наблюдение, закоммиченное после чтения зелёных лимитов
/// диспетчером, но до его commit, не обходит красное окно у существующей сессии.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn observation_landing_during_existing_session_dispatch_turns_it_red(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let id = create(&f, existing(&f), "0 9 * * *").await;
    // Автор наблюдения держит строку провайдера до commit.
    let now = Utc::now();
    let mut writer = pool.begin().await.unwrap();
    record_observation_in_tx(
        &mut writer,
        f.seed.owner_id,
        &Observation {
            provider: "claude".into(),
            window: limits::Window::FiveHour,
            used_permille: 990,
            resets_at: Some(now + TimeDelta::hours(3)),
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
    due(&pool, &id, now - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(now).await.unwrap();
    // Приём дошёл до разделяемого замка провайдера и ждёт автора наблюдения.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "select count(*) from pg_stat_activity where wait_event_type='Lock' and datname=current_database()",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting > 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("admission waits for the provider lock");
    writer.commit().await.unwrap();
    let rid = run_id(&pool, &id).await;
    wait_state(&pool, rid, "skipped_limits").await;
    assert_eq!(inputs_with_text(&pool, "посмотреть коммиты").await, 0);
}
