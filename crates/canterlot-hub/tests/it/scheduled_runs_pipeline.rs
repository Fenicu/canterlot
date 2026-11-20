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
            account: "owner".into(),
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
            account: "owner".into(),
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
         on conflict(provider,account) do update set weekly_reserve_active=true",
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
            account: "owner".into(),
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
            account: "owner".into(),
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

/// 8: два хаба на одной базе. Первый заявил запуск и «завис» за семафором
/// приёма дольше аренды; второй по восстановлению забирает строку и доводит
/// её. Опоздавшая приёмка первого не делает ни второй предпроверки, ни
/// второй сессии: на воркере ровно одна предпроверка, в базе ровно один ввод.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn second_hub_takes_over_a_stalled_admission_with_one_precheck_and_one_session(pool: PgPool) {
    use canterlot_hub::schedules::{admission::admit, claim_due};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let f = Fixture::new(pool.clone()).await;
    let mut session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let registry = f.state.registry.clone();
    let machine = f.seed.machine_id;
    let connection = registry.session_info(machine).unwrap().connection_id;
    let prechecks = std::sync::Arc::new(AtomicUsize::new(0));
    let worker = tokio::spawn({
        let prechecks = prechecks.clone();
        async move {
            while let Some(down) = session.commands.recv().await {
                if let Some(control_down::Msg::SchedulePrecheck(request)) = down.msg {
                    prechecks.fetch_add(1, Ordering::SeqCst);
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
        }
    });
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/schedules",
            Some("two-hubs"),
            Some(json!({
                "name": "Два хаба",
                "cron": "0 9 * * *",
                "target": existing(&f),
                "prompt": "один раз",
                "precheck": "true",
            })),
        )
        .await;
    let id = r.json::<Value>()["id"].as_str().unwrap().to_owned();
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    // Хаб A заявил запуск; его приёмка ещё не пошла (стоит за семафором).
    let now = Utc::now();
    let mut conn = pool.acquire().await.unwrap();
    let claimed_by_a = claim_due(&mut conn, now, 32).await.unwrap();
    drop(conn);
    assert_eq!(claimed_by_a.len(), 1);
    let stalled = claimed_by_a.into_iter().next().unwrap();
    let rid = stalled.run.id;
    // Аренда A истекла: A «умер» с точки зрения других хабов.
    sqlx::query("update schedule_runs set lease_at=now()-interval '2 hours',updated_at=now()-interval '2 hours' where id=$1")
        .bind(rid)
        .execute(&pool)
        .await
        .unwrap();
    // Хаб B (отдельный планировщик на той же базе) восстанавливает и доводит запуск.
    scheduler(&f).tick(Utc::now()).await.unwrap();
    wait_state(&pool, rid, "started").await;
    assert_eq!(prechecks.load(Ordering::SeqCst), 1);
    // Опоздавшая приёмка A просыпается со старой попыткой: ничего не делает.
    admit(&scheduler(&f), stalled).await;
    assert_eq!(prechecks.load(Ordering::SeqCst), 1, "no second precheck");
    assert_eq!(inputs_with_text(&pool, "один раз").await, 1);
    let sessions: i64 =
        sqlx::query_scalar("select count(*) from schedule_runs where id=$1 and state='started'")
            .bind(rid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(sessions, 1);
    worker.abort();
}

/// 8: гибель процесса до commit dispatch (точка отказа): запуск остаётся
/// `claimed`, восстановление после аренды доводит его ровно до одной сессии.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn crash_before_dispatch_commit_is_recovered_into_exactly_one_session(pool: PgPool) {
    use canterlot_hub::schedules::fault::{self, Fault};

    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let id = create(&f, existing(&f), "0 9 * * *").await;
    fault::arm(Uuid::parse_str(&id).unwrap(), Fault::BeforeDispatchCommit);
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let rid = run_id(&pool, &id).await;
    // Приёмка «умерла» до commit: ни сессии, ни ввода, строка не продвинулась.
    wait_crashed(Uuid::parse_str(&id).unwrap(), rid).await;
    let (state, _) = state(&pool, rid).await;
    assert_eq!(state, "claimed");
    assert_eq!(inputs_with_text(&pool, "посмотреть коммиты").await, 0);
    sqlx::query("update schedule_runs set lease_at=now()-interval '2 hours',updated_at=now()-interval '2 hours' where id=$1")
        .bind(rid)
        .execute(&pool)
        .await
        .unwrap();
    scheduler(&f).tick(Utc::now()).await.unwrap();
    wait_state(&pool, rid, "started").await;
    assert_eq!(inputs_with_text(&pool, "посмотреть коммиты").await, 1);
}

/// Ждёт, пока точка отказа сработала и приёмка выбыла из реестра процесса.
async fn wait_crashed(schedule: Uuid, run: Uuid) {
    use canterlot_hub::schedules::{fault, is_admitting};
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while fault::armed(schedule, fault::Fault::BeforeDispatchCommit) || is_admitting(&run) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the admission hits the fault and ends");
}

/// 1: композер выигрывает общий ключ между проверкой и вставкой резерва.
/// Резерв Run Now не вправе закоммитить заявку: откат и конфликт, строки нет.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_now_does_not_commit_a_claim_when_another_operation_wins_the_key(pool: PgPool) {
    use canterlot_hub::schedules::{ScheduleError, admission::reserve_manual};

    let f = Fixture::new(pool.clone()).await;
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    // «Композер»: вставка ключа в открытой транзакции, ещё не закоммиченная.
    let mut winner = pool.begin().await.unwrap();
    sqlx::query(
        "insert into idempotency_keys(owner_id,key,operation,request_hash,response)
         values($1,'race-key','submit_input',$2,'{}'::jsonb)",
    )
    .bind(f.seed.owner_id)
    .bind(vec![1u8, 2, 3])
    .execute(&mut *winner)
    .await
    .unwrap();
    let reserve = {
        let pool = pool.clone();
        let owner = f.seed.owner_id;
        tokio::spawn(async move { reserve_manual(&pool, owner, schedule, "race-key", true).await })
    };
    // Резерв дошёл до вставки ключа и ждёт победителя на уникальном индексе.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "select count(*) from pg_locks l join pg_stat_activity a on a.pid=l.pid
                  where l.locktype='transactionid' and not l.granted and a.datname=current_database()",
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
    .expect("reservation waits for the winner");
    winner.commit().await.unwrap();
    let result = reserve.await.unwrap();
    assert!(
        matches!(result, Err(ScheduleError::Conflict("idempotency_conflict"))),
        "foreign winner must give a conflict, got {:?}",
        result.map(|_| ())
    );
    let runs: i64 = sqlx::query_scalar("select count(*) from schedule_runs where schedule_id=$1")
        .bind(schedule)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 0, "no committed claim paired with a foreign key");
}

/// 2: подтверждённая смена модели на Fable во время предпроверки. Проекция
/// `conversation_derived` не обновлена (держим её), но dispatch читает
/// авторитетную `conversations.model` под замком очереди и видит красное окно.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn model_change_during_precheck_is_read_from_the_authoritative_row(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let mut session: FakeSession =
        testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    sqlx::query("update conversations set model='claude-sonnet-4-5' where id=$1")
        .bind(f.seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    observe(&pool, &f, "claude", limits::Window::WeeklyFable, 990).await;
    let registry = f.state.registry.clone();
    let machine = f.seed.machine_id;
    let connection = registry.session_info(machine).unwrap().connection_id;
    let conversation = f.seed.conversation_id;
    let worker_pool = pool.clone();
    let worker = tokio::spawn(async move {
        while let Some(down) = session.commands.recv().await {
            if let Some(control_down::Msg::SchedulePrecheck(request)) = down.msg {
                // Настройки приняты воркером: меняется только авторитетная строка.
                sqlx::query("update conversations set model='claude-fable-5' where id=$1")
                    .bind(conversation)
                    .execute(&worker_pool)
                    .await
                    .unwrap();
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
            Some("fable-create"),
            Some(json!({
                "name": "Смена модели",
                "cron": "0 9 * * *",
                "target": existing(&f),
                "prompt": "посмотреть коммиты",
                "precheck": "true",
            })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200);
    let id = r.json::<Value>()["id"].as_str().unwrap().to_owned();
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let rid = run_id(&pool, &id).await;
    let detail = wait_state(&pool, rid, "skipped_limits").await;
    assert_eq!(detail["window"], "weekly_fable", "{detail}");
    assert_eq!(inputs_with_text(&pool, "посмотреть коммиты").await, 0);
    worker.abort();
}

/// Ожидающая, ещё не подтверждённая смена модели уходит с вводом при выдаче,
/// поэтому окна проверяются по ней: красное окно Fable пропускает запуск.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn pending_model_change_is_checked_against_its_own_limit_window(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    sqlx::query(
        "update conversations set model='claude-sonnet-4-5',
                pending_settings='{\"model\":\"claude-fable-5\"}'::jsonb where id=$1",
    )
    .bind(f.seed.conversation_id)
    .execute(&pool)
    .await
    .unwrap();
    observe(&pool, &f, "claude", limits::Window::WeeklyFable, 990).await;
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/schedules",
            Some("pending-fable-create"),
            Some(json!({
                "name": "Ожидающая модель",
                "cron": "0 9 * * *",
                "target": existing(&f),
                "prompt": "ожидающая смена",
            })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200);
    let id = r.json::<Value>()["id"].as_str().unwrap().to_owned();
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let rid = run_id(&pool, &id).await;
    let detail = wait_state(&pool, rid, "skipped_limits").await;
    assert_eq!(detail["window"], "weekly_fable", "{detail}");
    assert_eq!(inputs_with_text(&pool, "ожидающая смена").await, 0);
}

/// 5: занятость в приёме считается тем же запросом, что в маршрутизации:
/// сессия с ожидающим толчком держит слот и при явном исполнителе.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dispatch_counts_a_pending_nudge_as_busy_like_routing(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(f.seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update conversations set harness='synthetic',model='synthetic-small' where id=$1")
        .bind(f.seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into conversation_derived(conversation_id) values($1) on conflict do nothing",
    )
    .bind(f.seed.conversation_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into nudge_rules(conversation_id,owner_id,set_by,state) values($1,$2,'owner','pending')",
    )
    .bind(f.seed.conversation_id)
    .bind(f.seed.owner_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("update owners set routing_policy=$2 where id=$1")
        .bind(f.seed.owner_id)
        .bind(json!({"max_parallel": {"synthetic/synthetic-small": 1}}))
        .execute(&pool)
        .await
        .unwrap();
    let id = create(
        &f,
        json!({
            "kind": "new_session",
            "machine_id": f.seed.machine_id,
            "workdir": "/tmp/scheduled-nudge",
            "executor": {"harness": "synthetic", "model": "synthetic-small", "effort": "high"},
        }),
        "0 9 * * *",
    )
    .await;
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let rid = run_id(&pool, &id).await;
    let detail = wait_state(&pool, rid, "skipped_limits").await;
    assert_eq!(detail["reason"], "parallel_busy", "{detail}");
}

async fn insert_run_at(
    pool: &PgPool,
    f: &Fixture,
    schedule: Uuid,
    at: chrono::DateTime<Utc>,
    manual: bool,
    key: &str,
) {
    sqlx::query(
        "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key)
         values($1,$2,$3,$4,1,'started',$5,$6)",
    )
    .bind(Uuid::now_v7())
    .bind(schedule)
    .bind(f.seed.owner_id)
    .bind(at)
    .bind(manual)
    .bind(key)
    .execute(pool)
    .await
    .unwrap();
}

/// 6: ручной запуск с тем же моментом, что у срабатывания по расписанию, его
/// не подавляет: уникальность момента действует только для срабатываний.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn manual_run_with_the_same_timestamp_does_not_suppress_the_cron_occurrence(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    let at = Utc::now() - TimeDelta::seconds(30);
    let at = chrono::DateTime::from_timestamp_micros(at.timestamp_micros()).unwrap();
    insert_run_at(&pool, &f, schedule, at, true, "manual-same-time").await;
    due(&pool, &id, at).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let cron: i64 = sqlx::query_scalar(
        "select count(*) from schedule_runs where schedule_id=$1 and not manual",
    )
    .bind(schedule)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        cron, 1,
        "the cron occurrence is claimed beside the manual run"
    );
}

/// 6: если строка срабатывания уже есть, тик всё равно двигает `next_fire_at`.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn claim_tick_advances_the_cursor_when_the_occurrence_row_exists(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    let at = Utc::now() - TimeDelta::seconds(30);
    let at = chrono::DateTime::from_timestamp_micros(at.timestamp_micros()).unwrap();
    insert_run_at(&pool, &f, schedule, at, false, "cron-existing").await;
    due(&pool, &id, at).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let next: chrono::DateTime<Utc> =
        sqlx::query_scalar("select next_fire_at from schedules where id=$1")
            .bind(schedule)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        next > Utc::now(),
        "cursor moved past the existing occurrence: {next}"
    );
    let runs: i64 = sqlx::query_scalar("select count(*) from schedule_runs where schedule_id=$1")
        .bind(schedule)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 1, "no duplicate occurrence row");
}

/// 7: уборка истории снимает только завершённые запуски: заявленный, но ещё
/// не принятый запуск под наплывом новых строк остаётся.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn history_pruning_keeps_active_runs(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    let old = Uuid::now_v7();
    sqlx::query(
        "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key)
         values($1,$2,$3,now()-interval '1 day',1,'claimed',false,'old-claimed')",
    )
    .bind(old)
    .bind(schedule)
    .bind(f.seed.owner_id)
    .execute(&pool)
    .await
    .unwrap();
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
    let (state, _) = state(&pool, old).await;
    assert_eq!(state, "claimed", "an active run is never pruned");
}

/// 7: повтор, прочитавший `claimed` перед финалом, не получает 404, если
/// уборка успела снять строку: итог берётся из долговечной записи ключа.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn replay_rereads_the_durable_record_when_the_history_row_is_gone(pool: PgPool) {
    use canterlot_hub::schedules::{
        RunState,
        admission::{Reservation, reserve_manual, settle_existing},
    };

    let f = Fixture::new(pool.clone()).await;
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    let Reservation::Fresh { run, .. } =
        reserve_manual(&pool, f.seed.owner_id, schedule, "gone-key", true)
            .await
            .unwrap()
    else {
        panic!("fresh reservation");
    };
    // Итог записан в ключ, строку истории уже убрали.
    let mut finished = run.clone();
    finished.state = RunState::Started;
    sqlx::query("update idempotency_keys set response=$3 where owner_id=$1 and key=$2")
        .bind(f.seed.owner_id)
        .bind("gone-key")
        .bind(serde_json::to_value(&finished).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("delete from schedule_runs where id=$1")
        .bind(run.id)
        .execute(&pool)
        .await
        .unwrap();
    let settled = settle_existing(&pool, f.seed.owner_id, run).await.unwrap();
    assert_eq!(settled.state, RunState::Started);
}

/// 11: после общего срока ключа Run Now с тем же запросом получает прежний
/// запуск из истории, другой запрос под тем же ключом — конфликт.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_now_replays_from_history_after_the_key_expired(pool: PgPool) {
    use canterlot_hub::schedules::{
        ScheduleError,
        admission::{Reservation, reserve_manual},
    };

    let f = Fixture::new(pool.clone()).await;
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    let Reservation::Fresh { run, .. } =
        reserve_manual(&pool, f.seed.owner_id, schedule, "expired-key", true)
            .await
            .unwrap()
    else {
        panic!("fresh reservation");
    };
    sqlx::query("update schedule_runs set state='started' where id=$1")
        .bind(run.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("delete from idempotency_keys where owner_id=$1 and key='expired-key'")
        .bind(f.seed.owner_id)
        .execute(&pool)
        .await
        .unwrap();
    match reserve_manual(&pool, f.seed.owner_id, schedule, "expired-key", true).await {
        Ok(Reservation::Existing(again)) => assert_eq!(again.id, run.id),
        other => panic!("expected the retained run, got {:?}", other.map(|_| ())),
    }
    let other = reserve_manual(&pool, f.seed.owner_id, schedule, "expired-key", false).await;
    assert!(
        matches!(other, Err(ScheduleError::Conflict("idempotency_conflict"))),
        "a different request under the same key conflicts"
    );
}

/// 7: повтор Run Now после удаления расписания (каскад убрал активный запуск)
/// отвечает так же, как для удалённого расписания, а не прежним `claimed`.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_now_replay_after_the_schedule_was_deleted_is_not_found(pool: PgPool) {
    use canterlot_hub::schedules::admission::{Reservation, reserve_manual};

    let f = Fixture::new(pool.clone()).await;
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule = Uuid::parse_str(&id).unwrap();
    let Reservation::Fresh { run, .. } =
        reserve_manual(&pool, f.seed.owner_id, schedule, "deleted-key", true)
            .await
            .unwrap()
    else {
        panic!("fresh reservation");
    };
    assert_eq!(run.state, canterlot_hub::schedules::RunState::Claimed);
    sqlx::query("delete from schedules where id=$1")
        .bind(schedule)
        .execute(&pool)
        .await
        .unwrap();
    let replay = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/schedules/{id}/run"),
            Some("deleted-key"),
            Some(json!({"run_anyway": true})),
        )
        .await;
    assert_eq!(
        replay.status.as_u16(),
        404,
        "{}",
        String::from_utf8_lossy(&replay.body)
    );
}

/// 8: временный замок расписания (55P03 по `lock_timeout`) не оставляет запуск
/// в `prechecking` до аренды: итог повторяется ограниченное число раз.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn finalization_retries_a_lock_timeout(pool: PgPool) {
    use canterlot_hub::schedules::{RunState, admission::write_outcome_retrying};
    use sqlx::postgres::PgPoolOptions;

    let f = Fixture::new(pool.clone()).await;
    let id = create(&f, existing(&f), "0 9 * * *").await;
    let schedule_id = Uuid::parse_str(&id).unwrap();
    let rid = Uuid::now_v7();
    let attempt = Uuid::now_v7();
    sqlx::query(
        "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key,attempt_id,lease_at)
         values($1,$2,$3,now(),1,'prechecking',false,'lock-timeout',$4,now())",
    )
    .bind(rid)
    .bind(schedule_id)
    .bind(f.seed.owner_id)
    .bind(attempt)
    .execute(&pool)
    .await
    .unwrap();
    // Пул с коротким `lock_timeout`, как в боевом пуле (там пять секунд).
    let short = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("set lock_timeout = '150ms'")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let schedule = canterlot_hub::schedules::get(&mut conn, f.seed.owner_id, schedule_id)
        .await
        .unwrap()
        .unwrap();
    let run = canterlot_hub::schedules::get_run(&mut conn, f.seed.owner_id, rid)
        .await
        .unwrap()
        .unwrap();
    drop(conn);
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("select id from schedules where id=$1 for update")
        .bind(schedule_id)
        .execute(&mut *holder)
        .await
        .unwrap();
    // Замок отпускается после того, как таймаут сработал хотя бы раз: ожидание,
    // его отказ и повторное ожидание второй попытки.
    let release = {
        let pool = pool.clone();
        tokio::spawn(async move {
            until_waiters(&pool, "first attempt waits", |n| n >= 1).await;
            until_waiters(&pool, "first attempt timed out", |n| n == 0).await;
            until_waiters(&pool, "second attempt waits", |n| n >= 1).await;
            holder.commit().await.unwrap();
        })
    };
    write_outcome_retrying(
        &short,
        &schedule,
        &run,
        RunState::Prechecking,
        RunState::Failed,
        &json!({"reason": "database_unavailable"}),
        Uuid::now_v7(),
    )
    .await
    .expect("a temporary lock is retried");
    release.await.unwrap();
    assert_eq!(state(&pool, rid).await.0, "failed");
}

/// 8: восстановление не ждёт заблокированное расписание: следующее в пачке
/// обрабатывается сразу.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn recovery_skips_a_locked_schedule(pool: PgPool) {
    use canterlot_hub::schedules::{RECLAIM_LEASE, recover_interrupted};

    let f = Fixture::new(pool.clone()).await;
    let now = Utc::now();
    let mut ids = Vec::new();
    for name in ["a", "b"] {
        let id = Uuid::parse_str(
            &create(
                &f,
                existing(&f),
                &format!("0 {} * * *", if name == "a" { 9 } else { 10 }),
            )
            .await,
        )
        .unwrap();
        sqlx::query(
            "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key,attempt_id,lease_at,updated_at)
             values($1,$2,$3,$4,1,'claimed',false,$5,$6,$7,$7)",
        )
        .bind(Uuid::now_v7())
        .bind(id)
        .bind(f.seed.owner_id)
        .bind(now - TimeDelta::hours(5))
        .bind(format!("skip-locked-{name}"))
        .bind(Uuid::now_v7())
        .bind(now - TimeDelta::hours(3))
        .execute(&pool)
        .await
        .unwrap();
        ids.push(id);
    }
    // Блокируем расписание, чья заявка идёт первой в пачке.
    let first: Uuid = sqlx::query_scalar(
        "select schedule_id from schedule_runs order by schedule_id,updated_at,id limit 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("select id from schedules where id=$1 for update")
        .bind(first)
        .execute(&mut *holder)
        .await
        .unwrap();
    let recovered = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        recover_interrupted(&pool, Some(RECLAIM_LEASE), now),
    )
    .await
    .expect("recovery does not wait for a locked schedule")
    .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_ne!(recovered[0].schedule.id, first);
    holder.rollback().await.unwrap();
}

/// 8: пачка не заполняется строками заблокированного расписания: 33 заявки
/// под одним замком не вытесняют восстановление другого расписания.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn recovery_batch_limit_applies_after_skipping_locked_schedules(pool: PgPool) {
    use canterlot_hub::schedules::{RECLAIM_LEASE, recover_interrupted};

    let f = Fixture::new(pool.clone()).await;
    let now = Utc::now();
    let mut ids = Vec::new();
    for hour in [9, 10] {
        ids.push(
            Uuid::parse_str(&create(&f, existing(&f), &format!("0 {hour} * * *")).await).unwrap(),
        );
    }
    ids.sort();
    let (locked, other) = (ids[0], ids[1]);
    for (schedule, count) in [(locked, 33), (other, 1)] {
        for n in 0..count {
            sqlx::query(
                "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key,attempt_id,lease_at,updated_at)
                 values($1,$2,$3,$4,1,'claimed',false,$5,$6,$7,$7)",
            )
            .bind(Uuid::now_v7())
            .bind(schedule)
            .bind(f.seed.owner_id)
            .bind(now - TimeDelta::hours(5) - TimeDelta::minutes(n))
            .bind(format!("batch-{schedule}-{n}"))
            .bind(Uuid::now_v7())
            .bind(now - TimeDelta::hours(3))
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("select id from schedules where id=$1 for update")
        .bind(locked)
        .execute(&mut *holder)
        .await
        .unwrap();
    let recovered = recover_interrupted(&pool, Some(RECLAIM_LEASE), now)
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1, "the other schedule still recovers");
    assert_eq!(recovered[0].schedule.id, other);
    holder.rollback().await.unwrap();
}

/// Ждёт, пока число ожидающих замок сессий удовлетворит условию.
async fn until_waiters(pool: &PgPool, what: &str, ok: impl Fn(i64) -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !ok(lock_waiters(pool).await) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"));
}

async fn lock_waiters(pool: &PgPool) -> i64 {
    sqlx::query_scalar(
        "select count(*) from pg_stat_activity where wait_event_type='Lock' and datname=current_database()",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// 9: порядок замков нового сеанса по расписанию — машина раньше провайдера,
/// как у обычного создания. Три транзакции (создание держит провайдера и ждёт
/// машину, отзыв держит машину и ждёт учётные данные, приём журнала держит
/// учётные данные и ждёт провайдера) иначе замыкаются в цикл.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn scheduled_new_session_takes_the_machine_lock_before_the_provider_lock(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(f.seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into machine_credentials(machine_id,token_sha256) values($1,$2)")
        .bind(f.seed.machine_id)
        .bind(vec![7u8; 32])
        .execute(&pool)
        .await
        .unwrap();
    // Строка провайдера существует до гонки: замок берётся на ней.
    observe(&pool, &f, "claude", limits::Window::FiveHour, 100).await;
    let id = create(
        &f,
        json!({
            "kind": "new_session",
            "machine_id": f.seed.machine_id,
            "workdir": "/tmp/scheduled-cycle",
            "executor": {"harness": "synthetic", "model": "synthetic-small", "effort": "high"},
        }),
        "0 9 * * *",
    )
    .await;
    // Приём журнала: держит учётные данные.
    let mut ingest = pool.begin().await.unwrap();
    sqlx::query("update machine_credentials set created_at=created_at where machine_id=$1")
        .bind(f.seed.machine_id)
        .execute(&mut *ingest)
        .await
        .unwrap();
    // Отзыв: берёт машину и ждёт учётные данные.
    let revoke = {
        let pool = pool.clone();
        let machine = f.seed.machine_id;
        tokio::spawn(async move { canterlot_hub::machines::revoke(&pool, machine).await })
    };
    until_waiters(&pool, "revoke waits on the credential", |n| n >= 1).await;
    // Приём расписания дошёл до ожидания замка (машины или провайдера).
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    until_waiters(&pool, "admission waits on a lock", |n| n >= 2).await;
    // Приём журнала просит провайдера: без правки порядка цикл замкнулся бы.
    let (owner, machine) = (f.seed.owner_id, f.seed.machine_id);
    let ingest_task = tokio::spawn(async move {
        let now = Utc::now();
        let result = record_observation_in_tx(
            &mut ingest,
            owner,
            &Observation {
                provider: "claude".into(),
                account: "owner".into(),
                window: limits::Window::FiveHour,
                used_permille: 100,
                resets_at: Some(now + TimeDelta::hours(3)),
                observed_at: now,
                source: ObservationSource::Session,
                data_confidence: DataConfidence::Exact,
                machine_id: machine,
                execution_id: None,
            },
            now,
            &Default::default(),
            Uuid::now_v7(),
        )
        .await;
        let _ = ingest.commit().await;
        result
    });
    let settled = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let ingest_result = ingest_task.await.unwrap();
        let revoked = revoke.await.unwrap();
        (ingest_result, revoked)
    })
    .await
    .expect("no lock cycle: every transaction finishes");
    assert!(settled.1.is_ok(), "revoke finished: {:?}", settled.1.err());
    // Жертвой взаимной блокировки не должен стать и приём журнала.
    assert!(
        settled.0.is_ok(),
        "observation finished: {:?}",
        settled.0.err()
    );
    let rid = run_id(&pool, &id).await;
    let detail = wait_state(&pool, rid, "started").await;
    assert!(detail.is_object(), "{detail}");
}

/// Второй хаб: собственный реестр сессий, своя сессия воркера, свой диспетчер
/// доставки и свой планировщик на той же базе. Считает предпроверки, которые
/// дошли до его воркера.
struct SecondHub {
    scheduler: canterlot_hub::scheduler::Scheduler,
    prechecks: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    _worker: tokio::task::JoinHandle<()>,
    _dispatcher: tokio::task::JoinHandle<()>,
    _stop: tokio::sync::watch::Sender<bool>,
}

fn second_hub(f: &Fixture) -> SecondHub {
    use canterlot_hub::session::Registry;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let registry = Registry::new(Default::default());
    let mut session = testkit::fake_session(&registry, f.seed.machine_id, &caps(), true);
    let machine = f.seed.machine_id;
    let connection = registry.session_info(machine).unwrap().connection_id;
    let prechecks = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = prechecks.clone();
    let reply = registry.clone();
    let worker = tokio::spawn(async move {
        while let Some(down) = session.commands.recv().await {
            if let Some(control_down::Msg::SchedulePrecheck(request)) = down.msg {
                counter.fetch_add(1, Ordering::SeqCst);
                reply.finish_precheck(
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
        drop(session);
    });
    let (dispatcher, dispatch_rx) = canterlot_hub::dispatcher::Dispatcher::channel();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let dispatch_loop = tokio::spawn(canterlot_hub::dispatcher::run(
        f.pool.clone(),
        registry.clone(),
        dispatch_rx,
        Default::default(),
        stopped,
    ));
    let scheduler =
        canterlot_hub::scheduler::Scheduler::new(f.pool.clone(), dispatcher, Default::default())
            .with_limits(registry, Default::default());
    SecondHub {
        scheduler,
        prechecks,
        _worker: worker,
        _dispatcher: dispatch_loop,
        _stop: stop,
    }
}

async fn routed_schedule(f: &Fixture, pool: &PgPool, precheck: Option<&str>) -> String {
    routed_fixture(
        f,
        pool,
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
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/schedules",
            Some(&format!("routed-{}", Uuid::now_v7())),
            Some(json!({
                "name": "Маршрут и отказ",
                "cron": "0 9 * * *",
                "target": {"kind":"new_session","machine_id":f.seed.machine_id,"workdir":"/tmp/scheduled-failover","route":{"task_type":"audit"}},
                "prompt": "посмотреть коммиты",
                "precheck": precheck,
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

async fn conversations_of_runs(pool: &PgPool, schedule: &str) -> i64 {
    sqlx::query_scalar(
        "select count(distinct r.session_id) from schedule_runs r where r.schedule_id=$1 and r.session_id is not null",
    )
    .bind(Uuid::parse_str(schedule).unwrap())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn titled_conversations(pool: &PgPool, owner: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from conversations where owner_id=$1 and workdir='/tmp/scheduled-failover'")
        .bind(owner)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// 12: маршрутизируемая цель «новая сессия». Хаб A заявил запуск, его приёмка
/// застряла (управляемая остановка) дольше аренды; независимый хаб B со своим
/// реестром и своим воркером доводит запуск. Опоздавшая приёмка A, отпущенная
/// после завершения B, ничего не делает: одна предпроверка на всех воркерах,
/// одна сессия, один ввод.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn routed_new_session_stalled_on_hub_a_is_finished_once_by_hub_b(pool: PgPool) {
    use canterlot_hub::schedules::{admission::admit, claim_due, fault};
    use std::sync::atomic::Ordering;

    let f = Fixture::new(pool.clone()).await;
    // Хаб A: свой воркер на f.state.registry, считает предпроверки.
    let mut session_a = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let checks_a = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let registry_a = f.state.registry.clone();
    let machine = f.seed.machine_id;
    let connection_a = registry_a.session_info(machine).unwrap().connection_id;
    let counter_a = checks_a.clone();
    let worker_a = tokio::spawn(async move {
        while let Some(down) = session_a.commands.recv().await {
            if let Some(control_down::Msg::SchedulePrecheck(request)) = down.msg {
                counter_a.fetch_add(1, Ordering::SeqCst);
                registry_a.finish_precheck(
                    machine,
                    connection_a,
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
    let hub_b = second_hub(&f);
    let id = routed_schedule(&f, &pool, Some("true")).await;
    let schedule = Uuid::parse_str(&id).unwrap();
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    let mut conn = pool.acquire().await.unwrap();
    let claimed = claim_due(&mut conn, Utc::now(), 32).await.unwrap();
    drop(conn);
    let stalled = claimed.into_iter().next().expect("hub A claimed the run");
    let rid = stalled.run.id;
    // Приёмка A стоит на затворе: заявка заявлена, дальше не идёт.
    let gate = fault::arm_stall(schedule);
    let late = {
        let scheduler = scheduler(&f);
        tokio::spawn(async move { admit(&scheduler, stalled).await })
    };
    // Приёмка успела бы дойти до конца за это число переключений, если бы не затвор.
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    assert!(!late.is_finished(), "admission on hub A is stalled");
    sqlx::query("update schedule_runs set lease_at=now()-interval '2 hours',updated_at=now()-interval '2 hours' where id=$1")
        .bind(rid)
        .execute(&pool)
        .await
        .unwrap();
    hub_b.scheduler.tick(Utc::now()).await.unwrap();
    wait_state(&pool, rid, "started").await;
    // Только теперь A оживает: старая попытка не даёт ему ничего сделать.
    gate.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(10), late)
        .await
        .expect("stalled admission ends")
        .unwrap();
    let total_prechecks = checks_a.load(Ordering::SeqCst) + hub_b.prechecks.load(Ordering::SeqCst);
    assert_eq!(total_prechecks, 1, "exactly one precheck across both hubs");
    assert_eq!(hub_b.prechecks.load(Ordering::SeqCst), 1, "hub B ran it");
    assert_eq!(
        titled_conversations(&pool, f.seed.owner_id).await,
        1,
        "one conversation"
    );
    assert_eq!(conversations_of_runs(&pool, &id).await, 1);
    assert_eq!(
        inputs_with_text(&pool, "посмотреть коммиты").await,
        1,
        "one input"
    );
    worker_a.abort();
}

/// 12: тот же маршрут без предпроверки, но гибель приёмки A перед commit
/// dispatch; независимый хаб B довершает ровно одну сессию с одним вводом.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn routed_new_session_crash_before_commit_fails_over_to_hub_b(pool: PgPool) {
    use canterlot_hub::schedules::fault::{self, Fault};

    let f = Fixture::new(pool.clone()).await;
    let _session_a = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let hub_b = second_hub(&f);
    let id = routed_schedule(&f, &pool, None).await;
    let schedule = Uuid::parse_str(&id).unwrap();
    fault::arm(schedule, Fault::BeforeDispatchCommit);
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(Utc::now()).await.unwrap();
    let rid = run_id(&pool, &id).await;
    wait_crashed(schedule, rid).await;
    assert_eq!(state(&pool, rid).await.0, "claimed");
    assert_eq!(
        titled_conversations(&pool, f.seed.owner_id).await,
        0,
        "rolled back"
    );
    assert_eq!(inputs_with_text(&pool, "посмотреть коммиты").await, 0);
    sqlx::query("update schedule_runs set lease_at=now()-interval '2 hours',updated_at=now()-interval '2 hours' where id=$1")
        .bind(rid)
        .execute(&pool)
        .await
        .unwrap();
    hub_b.scheduler.tick(Utc::now()).await.unwrap();
    wait_state(&pool, rid, "started").await;
    assert_eq!(
        titled_conversations(&pool, f.seed.owner_id).await,
        1,
        "one conversation"
    );
    assert_eq!(
        inputs_with_text(&pool, "посмотреть коммиты").await,
        1,
        "one input"
    );
    assert_eq!(hub_b.prechecks.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// 12: покрытие сторожа приёмки в одном процессе. Два планировщика здесь делят один
/// статический реестр `ADMITTING_RUNS`, поэтому «хаб B» видит сторож «хаба A» так, как его
/// не увидел бы настоящий второй процесс: тест проверяет, что строку со сторожем тик не
/// берёт, а после снятия сторожа берёт, и ровно один разговор и один ввод. Межпроцессный
/// захват (аренда без общего сторожа) он не проверяет.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn same_process_guard_holds_the_row_until_the_guard_is_dropped(pool: PgPool) {
    use canterlot_hub::schedules::{admission::admit, claim_due, fault, is_admitting};

    let f = Fixture::new(pool.clone()).await;
    let _session_a = testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true);
    let hub_b = second_hub(&f);
    let id = routed_schedule(&f, &pool, None).await;
    let schedule = Uuid::parse_str(&id).unwrap();
    due(&pool, &id, Utc::now() - TimeDelta::seconds(30)).await;
    let mut conn = pool.acquire().await.unwrap();
    let claimed = claim_due(&mut conn, Utc::now(), 32).await.unwrap();
    drop(conn);
    let held = claimed.into_iter().next().expect("hub A claimed the run");
    let rid = held.run.id;
    // Приёмка A берёт сторож и встаёт на затворе, который никто не отпустит.
    let _gate = fault::arm_guarded_stall(schedule);
    let task = {
        let scheduler = scheduler(&f);
        tokio::spawn(async move { admit(&scheduler, held).await })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !is_admitting(&rid) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("hub A takes the admission guard");
    sqlx::query("update schedule_runs set lease_at=now()-interval '2 hours',updated_at=now()-interval '2 hours' where id=$1")
        .bind(rid)
        .execute(&pool)
        .await
        .unwrap();
    // Аренда истекла, но сторож жив: B строку не берёт (попытка владения прежняя).
    let attempt_of = |pool: PgPool| async move {
        sqlx::query_scalar::<_, Option<Uuid>>("select attempt_id from schedule_runs where id=$1")
            .bind(rid)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let attempt_before = attempt_of(pool.clone()).await;
    hub_b.scheduler.tick(Utc::now()).await.unwrap();
    assert_eq!(state(&pool, rid).await.0, "claimed", "guarded row is left");
    assert_eq!(
        attempt_of(pool.clone()).await,
        attempt_before,
        "hub B did not take over the guarded row"
    );
    assert_eq!(titled_conversations(&pool, f.seed.owner_id).await, 0);
    // A потерян: задача оборвана вместе со сторожем.
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!is_admitting(&rid), "the guard went away with hub A");
    hub_b.scheduler.tick(Utc::now()).await.unwrap();
    wait_state(&pool, rid, "started").await;
    assert_eq!(
        titled_conversations(&pool, f.seed.owner_id).await,
        1,
        "one conversation"
    );
    assert_eq!(conversations_of_runs(&pool, &id).await, 1);
    assert_eq!(
        inputs_with_text(&pool, "посмотреть коммиты").await,
        1,
        "one input"
    );
    let executions: i64 = sqlx::query_scalar(
        "select count(*) from executions e join conversations c on c.id=e.conversation_id
          where c.owner_id=$1 and c.workdir='/tmp/scheduled-failover'",
    )
    .bind(f.seed.owner_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(executions, 1, "one execution");
}
