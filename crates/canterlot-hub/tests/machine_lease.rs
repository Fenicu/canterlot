//! Зеркало очереди тяжёлых задач машины: хранение последнего состояния, событие
//! клиентам, снимок машин, очистка при пропаже машины, ожидание очереди в
//! активности без ложного `stalled` и одно предупреждение о долгом удержании.

#[path = "support/task3.rs"]
mod support;

use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    activity::{self, Thresholds},
    config::{HubTimers, SessionToolsConfig},
    dispatcher::Dispatcher,
    executions,
    leases::{self, LeaseOutcome},
    machine_info,
    model::ExecutionState,
    scheduler::Scheduler,
    session_tools::{Caller, ToolContext, read, settle::SettleConfig, subs},
    snapshot, testkit,
};
use canterlot_proto::worker::v1::*;
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::*;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const HOLD: Duration = Duration::from_secs(90 * 60);

fn ms(at: DateTime<Utc>) -> u64 {
    at.timestamp_millis() as u64
}

fn entry(
    lease: &str,
    kind: &str,
    execution: Option<Uuid>,
    since: u64,
    position: u32,
) -> LeaseEntry {
    LeaseEntry {
        lease_id: lease.into(),
        kind: kind.into(),
        weight: if kind == "gate" { 2 } else { 1 },
        label: format!("{kind} label"),
        execution_id: execution.map(|id| id.to_string()).unwrap_or_default(),
        holder: if execution.is_some() {
            "execution"
        } else {
            "owner"
        }
        .into(),
        since_ms: since,
        requested_at_ms: since.max(1),
        position,
        dir: "canterlot".into(),
    }
}

fn state(holders: Vec<LeaseEntry>, waiters: Vec<LeaseEntry>, observed: u64) -> LeaseState {
    LeaseState {
        slots: 2,
        holders,
        waiters,
        observed_at_ms: observed,
    }
}

async fn apply(pool: &PgPool, seed: &testkit::Seed, state: &LeaseState) -> LeaseOutcome {
    leases::apply(
        pool,
        seed.owner_id,
        seed.machine_id,
        state,
        None,
        Uuid::now_v7(),
    )
    .await
    .unwrap()
}

async fn stored(pool: &PgPool, machine: Uuid) -> Option<Value> {
    sqlx::query_scalar("select lease_state from machines where id=$1")
        .bind(machine)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn lease_events(pool: &PgPool) -> Vec<Value> {
    sqlx::query_scalar(
        "select payload from events where kind='machine_lease_changed' order by scope_seq",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn event_validator() -> jsonschema::Validator {
    let mut schema: Value =
        serde_json::from_str(include_str!("../../../docs/api/events.schema.json")).unwrap();
    schema["$ref"] = json!("#/$defs/machine_lease_changed");
    schema.as_object_mut().unwrap().remove("oneOf");
    jsonschema::validator_for(&schema).unwrap()
}

async fn running(pool: &PgPool, owner: Uuid, conversation: Uuid) -> Uuid {
    let execution = executions::start(pool, owner, conversation, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    execution.execution_id
}

async fn available(pool: &PgPool, machine: Uuid) {
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(machine)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn latest_state_replaces_the_previous_and_reaches_clients(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    let execution = running(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    let first = state(
        vec![entry(
            "a",
            "build",
            Some(execution),
            ms(now - TimeDelta::minutes(3)),
            0,
        )],
        vec![entry("b", "gate", None, 0, 1)],
        ms(now - TimeDelta::seconds(2)),
    );
    assert_eq!(apply(&pool, &s, &first).await, LeaseOutcome::Stored);
    let lease = stored(&pool, s.machine_id).await.unwrap();
    assert_eq!(lease["slots"], 2);
    assert_eq!(lease["holders"][0]["kind"], "build");
    assert_eq!(lease["holders"][0]["holder"], "execution");
    assert_eq!(lease["holders"][0]["execution_id"], json!(execution));
    assert_eq!(
        lease["holders"][0]["conversation_id"],
        json!(s.conversation_id)
    );
    assert!(lease["holders"][0]["position"].is_null());
    assert!(lease["holders"][0]["since"].is_string());
    assert_eq!(lease["waiters"][0]["holder"], "owner");
    assert_eq!(lease["waiters"][0]["position"], 1);
    assert!(lease["waiters"][0]["since"].is_null());
    assert!(lease["waiters"][0]["conversation_id"].is_null());

    // Полное состояние заменяет прежнее, а не дополняет его.
    let second = state(vec![entry("b", "gate", None, ms(now), 0)], vec![], ms(now));
    assert_eq!(apply(&pool, &s, &second).await, LeaseOutcome::Stored);
    let lease = stored(&pool, s.machine_id).await.unwrap();
    assert_eq!(lease["holders"].as_array().unwrap().len(), 1);
    assert_eq!(lease["holders"][0]["lease_id"], "b");
    assert!(lease["waiters"].as_array().unwrap().is_empty());

    // Переставленный старый снимок не затирает свежий.
    assert_eq!(
        apply(&pool, &s, &first).await,
        LeaseOutcome::Ignored("stale_state")
    );
    assert_eq!(
        stored(&pool, s.machine_id).await.unwrap()["holders"][0]["lease_id"],
        "b"
    );

    let events = lease_events(&pool).await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["machine_id"], json!(s.machine_id));
    assert_eq!(events[1]["lease"]["holders"][0]["lease_id"], "b");
    let validator = event_validator();
    let wire: Vec<Value> = sqlx::query_scalar(
        "select jsonb_build_object('v',1,'id',id,'scope','machines','scope_seq',scope_seq::text,
            'scope_seq_from',scope_seq::text,'committed_at',to_char(committed_at at time zone 'utc','YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),
            'committed_at_from',to_char(committed_at at time zone 'utc','YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),
            'conversation_id',null,'execution_id',null,'correlation_id',correlation_id,
            'kind',kind,'payload',payload)
         from events where kind='machine_lease_changed'",
    )
    .fetch_all(&pool)
    .await?;
    for event in &wire {
        let errors: Vec<_> = validator
            .iter_errors(event)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{errors:?}\n{event:#}");
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn malformed_state_is_ignored_without_touching_the_stored_one(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    let now = Utc::now();
    apply(
        &pool,
        &s,
        &state(vec![entry("a", "build", None, ms(now), 0)], vec![], ms(now)),
    )
    .await;
    let mut bad = entry("z", "build", None, ms(now), 0);
    bad.holder = "execution".into();
    bad.execution_id = "not-a-uuid".into();
    assert_eq!(
        apply(&pool, &s, &state(vec![bad], vec![], ms(now) + 1)).await,
        LeaseOutcome::Ignored("execution_id")
    );
    assert_eq!(
        stored(&pool, s.machine_id).await.unwrap()["holders"][0]["lease_id"],
        "a"
    );
    assert_eq!(lease_events(&pool).await.len(), 1);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn machine_snapshot_carries_the_queue(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    let snap = snapshot::machines(&pool, s.owner_id).await?;
    assert!(serde_json::to_value(&snap.data.items[0])?["lease"].is_null());
    let now = Utc::now();
    apply(
        &pool,
        &s,
        &state(vec![entry("g", "gate", None, ms(now), 0)], vec![], ms(now)),
    )
    .await;
    let snap = snapshot::machines(&pool, s.owner_id).await?;
    let view = serde_json::to_value(&snap.data.items[0])?;
    assert_eq!(view["lease"]["holders"][0]["kind"], "gate");
    assert_eq!(view["lease"]["holders"][0]["holder"], "owner");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unavailable_machine_drops_the_queue(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    available(&pool, s.machine_id).await;
    let now = Utc::now();
    apply(
        &pool,
        &s,
        &state(vec![entry("g", "gate", None, ms(now), 0)], vec![], ms(now)),
    )
    .await;
    // Реестр без сеанса машины: проекция видит её пропавшей.
    machine_info::project_availability(&pool, &registry(), Uuid::now_v7()).await?;
    assert_eq!(stored(&pool, s.machine_id).await, None);
    let events = lease_events(&pool).await;
    assert_eq!(events.len(), 2);
    assert!(events[1]["lease"].is_null());
    let snap = snapshot::machines(&pool, s.owner_id).await?;
    assert!(serde_json::to_value(&snap.data.items[0])?["lease"].is_null());
    // Повторная проекция молчит; полное состояние при переподключении заменяет пустоту.
    machine_info::project_availability(&pool, &registry(), Uuid::now_v7()).await?;
    assert_eq!(lease_events(&pool).await.len(), 2);
    apply(
        &pool,
        &s,
        &state(vec![entry("g", "gate", None, ms(now), 0)], vec![], ms(now)),
    )
    .await;
    assert!(stored(&pool, s.machine_id).await.is_some());
    Ok(())
}

fn thresholds() -> Thresholds {
    Thresholds {
        stalled_generating: Duration::from_secs(1800),
        stalled_tool: Duration::from_secs(1800),
        idle_quiet: Duration::from_secs(5),
        idle_background_max: std::time::Duration::from_secs(15 * 60),
    }
}

async fn activity_row(pool: &PgPool, id: Uuid) -> (String, Value) {
    sqlx::query_as("select activity_state,activity_evidence from conversation_derived where conversation_id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn tool_context(pool: &PgPool) -> ToolContext {
    ToolContext {
        pool: pool.clone(),
        config: Arc::new(SessionToolsConfig::default()),
        limits: Default::default(),
        settle: SettleConfig {
            idle_quiet: Duration::from_secs(5),
            idle_background_max: std::time::Duration::from_secs(15 * 60),
            nudge_enabled: true,
        },
        alarms_enabled: true,
        nudge_enabled: true,
        routing_enabled: true,
        routing_stale: Duration::from_secs(1800),
        routing_queue_horizon: Duration::from_secs(6 * 3600),
        spawn_queue: Default::default(),
        guards: Default::default(),
        dispatcher: None,
        registry: canterlot_hub::session::Registry::new(Default::default()),
        shutdown: CancellationToken::new(),
    }
}

/// Сроки, которые дают `stalled` молчащему инструменту без потомков: два часа
/// без событий, живой CLI без CPU. Пока исполнение стоит в очереди, оно работает,
/// подписчик не слышит о зависании, а инструменты чтения показывают место.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn waiting_for_the_queue_is_never_stalled(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    available(&pool, s.machine_id).await;
    let target = testkit::seed_conversation(&pool, s.owner_id, s.machine_id).await;
    let execution = running(&pool, s.owner_id, target).await;
    let now = Utc::now();
    let quiet = now - TimeDelta::hours(2);
    sqlx::query(
        "update executions set phase='running_tool',phase_at=$2,last_event_at=$2 where id=$1",
    )
    .bind(execution)
    .bind(quiet)
    .execute(&pool)
    .await?;
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2,run_slot_since=$3,hold_reasons='[]'
         where conversation_id=$1",
    )
    .bind(target)
    .bind(Uuid::now_v7())
    .bind(quiet)
    .execute(&pool)
    .await?;
    sqlx::query("insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions) values($1,$2,1,1,0,100,50,0,0,0,'[]',$3)")
        .bind(s.machine_id)
        .bind(now)
        .bind(json!([{"execution_id": execution, "alive": true, "cpu_permille": 0, "descendants": 0}]))
        .execute(&pool)
        .await?;

    // Без очереди те же сроки — `stalled`: тест воспроизводит настоящее зависание.
    activity::refresh(&pool, target, now, &thresholds(), Uuid::now_v7()).await?;
    assert_eq!(activity_row(&pool, target).await.0, "stalled");

    let requested = now - TimeDelta::minutes(100);
    apply(
        &pool,
        &s,
        &state(
            vec![entry("g", "gate", None, ms(requested), 0)],
            vec![entry("w", "build", Some(execution), 0, 2)],
            ms(now),
        ),
    )
    .await;
    // Подписка заводится после первого прохода: слышно было бы только новое зависание.
    let mut tx = pool.begin().await?;
    subs::subscribe_in_tx(&mut tx, s.owner_id, s.conversation_id, target, 0).await?;
    tx.commit().await?;
    for later in [now, now + TimeDelta::minutes(30), now + TimeDelta::hours(3)] {
        sqlx::query("update machine_metrics set observed_at=$2 where machine_id=$1")
            .bind(s.machine_id)
            .bind(later)
            .execute(&pool)
            .await?;
        activity::refresh(&pool, target, later, &thresholds(), Uuid::now_v7()).await?;
        let (state, evidence) = activity_row(&pool, target).await;
        assert_eq!(state, "working", "{later}");
        assert_eq!(evidence["kind"], "working");
        assert_eq!(evidence["lease_wait"]["kind"], "build");
        assert_eq!(evidence["lease_wait"]["position"], 2);
        assert!(evidence["lease_wait"]["since"].is_string());
    }
    let stalled: i64 = sqlx::query_scalar(
        "select count(*) from scheduled_jobs where conversation_id=$1 and payload->>'state'='stalled'",
    )
    .bind(s.conversation_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(stalled, 0);

    let context = tool_context(&pool);
    let caller = Caller {
        owner_id: s.owner_id,
        conversation_id: s.conversation_id,
        execution_id: Uuid::now_v7(),
        machine_id: s.machine_id,
        call_id: Uuid::now_v7(),
    };
    let inspected = read::inspect(&context, &caller, json!({"session_id": target}))
        .await
        .unwrap();
    assert_eq!(
        inspected["activity"]["evidence"]["lease_wait"]["position"],
        2
    );
    let listed = read::list(&context, &caller, json!({})).await.unwrap();
    let row = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == json!(target))
        .expect("target in sessions_list");
    assert_eq!(row["activity"]["evidence"]["lease_wait"]["kind"], "build");

    // Очередь прошла: свидетельство исчезает при следующем выводе.
    apply(
        &pool,
        &s,
        &state(
            vec![entry("w", "build", Some(execution), ms(now), 0)],
            vec![],
            ms(now) + 1,
        ),
    )
    .await;
    activity::refresh(
        &pool,
        target,
        now + TimeDelta::hours(3),
        &thresholds(),
        Uuid::now_v7(),
    )
    .await?;
    assert!(
        activity_row(&pool, target)
            .await
            .1
            .get("lease_wait")
            .is_none()
    );
    Ok(())
}

async fn long_hold_jobs(pool: &PgPool) -> Vec<(Uuid, Value)> {
    sqlx::query_as(
        "select conversation_id,payload from scheduled_jobs
         where payload->>'kind'='lease_long_hold' order by id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn long_hold_warns_holder_subscribers_once_and_only_from_fresh_state(
    pool: PgPool,
) -> TestResult {
    let s = testkit::seed(&pool).await;
    let holder = testkit::seed_conversation(&pool, s.owner_id, s.machine_id).await;
    sqlx::query("update conversations set title='Баннер связи' where id=$1")
        .bind(holder)
        .execute(&pool)
        .await?;
    let execution = running(&pool, s.owner_id, holder).await;
    let mut tx = pool.begin().await?;
    subs::subscribe_in_tx(&mut tx, s.owner_id, s.conversation_id, holder, 0).await?;
    tx.commit().await?;
    let now = Utc::now();
    apply(
        &pool,
        &s,
        &state(
            vec![
                entry(
                    "long",
                    "build",
                    Some(execution),
                    ms(now - TimeDelta::minutes(95)),
                    0,
                ),
                entry("short", "build", None, ms(now - TimeDelta::minutes(10)), 0),
            ],
            vec![],
            ms(now),
        ),
    )
    .await;

    // Машина не в сети: состояние не свежее, предупреждения нет.
    let report = leases::long_hold_sweep(&pool, HOLD, now).await?;
    assert_eq!(report, leases::LongHoldReport::default());
    assert!(long_hold_jobs(&pool).await.is_empty());

    // Оркестратор на машине (сессия с делегатом, работает) слышит по общему
    // правилу событий машины, даже без подписки на держателя.
    let orchestrator = testkit::seed_conversation(&pool, s.owner_id, s.machine_id).await;
    let delegate = testkit::seed_conversation(&pool, s.owner_id, s.machine_id).await;
    sqlx::query("update conversations set parent_conversation_id=$2 where id=$1")
        .bind(delegate)
        .bind(orchestrator)
        .execute(&pool)
        .await?;
    sqlx::query(
        "insert into conversation_derived(conversation_id,activity_state,activity_evidence)
         values($1,'working','{}')",
    )
    .bind(orchestrator)
    .execute(&pool)
    .await?;

    available(&pool, s.machine_id).await;
    let report = leases::long_hold_sweep(&pool, HOLD, now).await?;
    assert_eq!(report.leases, 1, "only the lease held past the threshold");
    assert_eq!(report.delivered, 2);
    let jobs = long_hold_jobs(&pool).await;
    let mut targets: Vec<Uuid> = jobs.iter().map(|(to, _)| *to).collect();
    targets.sort();
    let mut want = vec![s.conversation_id, orchestrator];
    want.sort();
    assert_eq!(targets, want);
    for (to, job) in &jobs {
        // Подлежащее — машина, цель задания — сам получатель.
        assert_eq!(job["target_id"], json!(to));
        assert_eq!(job["machine_id"], json!(s.machine_id));
        assert_eq!(job["details"]["lease_kind"], "build");
        assert_eq!(job["details"]["conversation_id"], json!(holder));
    }
    let marks: i64 =
        sqlx::query_scalar("select count(*) from machine_event_marks where kind='lease_long_hold'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(marks, 2);

    // Повторный обход и перезапуск хаба не дают второго события той же аренды.
    let report = leases::long_hold_sweep(&pool, HOLD, now + TimeDelta::minutes(30)).await?;
    assert_eq!(report.leases, 0);
    assert_eq!(long_hold_jobs(&pool).await.len(), 2);

    // Держатель-владелец (терминал): своя аренда — свой эпизод у тех же получателей.
    let report = leases::long_hold_sweep(&pool, HOLD, now + TimeDelta::minutes(85)).await?;
    assert_eq!(report.leases, 1);
    assert_eq!(report.delivered, 2);

    Scheduler::new(pool.clone(), Dispatcher::channel().0, Default::default())
        .tick(now + TimeDelta::minutes(85))
        .await?;
    let delivered: Vec<(Value, Option<Value>)> = sqlx::query_as(
        "select content,origin from inputs where conversation_id=$1 order by input_seq",
    )
    .bind(s.conversation_id)
    .fetch_all(&pool)
    .await?;
    assert_eq!(delivered.len(), 2);
    let texts: Vec<&str> = delivered
        .iter()
        .map(|(content, _)| content[0]["text"].as_str().unwrap())
        .collect();
    assert!(
        texts.contains(
            &format!(
                "[canterlot] Machine \"synthetic\": session «Баннер связи» (id {holder}) has held \
         the build lease \"build label\" for 95 min. Other heavy tasks on the machine queue \
         behind it; check whether the command hangs. Nothing is stopped automatically."
            )
            .as_str()
        ),
        "{texts:#?}"
    );
    assert!(
        texts.contains(
            &"[canterlot] Machine \"synthetic\": the owner's terminal has held the build lease \
          \"build label\" for 95 min. Other heavy tasks on the machine queue behind it; check \
          whether the command hangs. Nothing is stopped automatically."
        ),
        "{texts:#?}"
    );
    for (_, origin) in &delivered {
        let event = &origin.as_ref().unwrap()["event"];
        assert_eq!(event["kind"], "lease_long_hold");
        assert_eq!(event["machine_id"], json!(s.machine_id));
    }
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lease_state_travels_the_control_stream_only_with_the_capability(
    pool: PgPool,
) -> TestResult {
    let s = wire_seed(&pool).await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let c = connect_with(
        &hub,
        &s,
        Uuid::now_v7(),
        &["pilot_v1", "resource_lease_v1"],
        ms(Utc::now()),
    )
    .await;
    assert!(
        hub.registry
            .has_capability(s.machine_id, "resource_lease_v1")
    );
    let now = Utc::now();
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::LeaseState(state(
            vec![entry("g", "gate", None, ms(now), 0)],
            vec![],
            ms(now),
        ))),
    })
    .await?;
    wait_until(
        &pool,
        "select count(*) from machines where lease_state is not null",
    )
    .await;
    drop(c);

    let other = wire_seed(&pool).await;
    let mut c = connect_with(&hub, &other, Uuid::now_v7(), &["pilot_v1"], ms(Utc::now())).await;
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::LeaseState(state(vec![], vec![], ms(now)))),
    })
    .await?;
    let closed = tokio::time::timeout(WAIT, async {
        loop {
            match c.stream.message().await {
                Ok(Some(_)) => continue,
                Ok(None) => break None,
                Err(status) => break Some(status),
            }
        }
    })
    .await?;
    assert_eq!(
        closed.map(|s| s.message().to_owned()).as_deref(),
        Some("capability_missing")
    );
    assert_eq!(stored(&pool, other.machine_id).await, None);
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
