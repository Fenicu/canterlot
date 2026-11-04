use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    activity::{self, Thresholds},
    config::{SchedulerConfig, SessionToolsConfig},
    conversations::{self, SubmitMessage},
    delivery::ContentBlock,
    dispatcher::Dispatcher,
    scheduler::Scheduler,
    session_tools::{self, Caller, ToolContext, settle::SettleConfig, subs},
    snapshot, testkit,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Fx {
    pool: PgPool,
    context: ToolContext,
    owner: Uuid,
    machine: Uuid,
    orchestrator: Uuid,
    child: Uuid,
    child2: Uuid,
    child3: Uuid,
    execution: Uuid,
}

fn thresholds() -> Thresholds {
    Thresholds {
        stalled_generating: Duration::from_secs(300),
        stalled_tool: Duration::from_secs(1800),
        idle_quiet: Duration::from_secs(5),
    }
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let seed = testkit::seed(&pool).await;
        let child = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
        let child2 = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
        let child3 = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
        // Без исполнения цель устоялась в idle, когда пройдёт окно тишины.
        let execution = Uuid::now_v7();
        sqlx::query("update machines set availability='available' where id=$1")
            .bind(seed.machine_id)
            .execute(&pool)
            .await
            .unwrap();
        Self {
            context: ToolContext {
                pool: pool.clone(),
                config: Arc::new(SessionToolsConfig::default()),
                settle: SettleConfig {
                    idle_quiet: Duration::from_secs(5),
                },
                alarms_enabled: true,
                dispatcher: None,
                registry: canterlot_hub::session::Registry::new(Default::default()),
                shutdown: CancellationToken::new(),
            },
            pool,
            owner: seed.owner_id,
            machine: seed.machine_id,
            orchestrator: seed.conversation_id,
            child,
            child2,
            child3,
            execution,
        }
    }

    fn caller(&self) -> Caller {
        Caller {
            owner_id: self.owner,
            conversation_id: self.orchestrator,
            execution_id: self.execution,
            machine_id: self.machine,
            call_id: Uuid::now_v7(),
        }
    }

    async fn call(&self, tool: &str, args: Value) -> Result<Value, session_tools::ToolError> {
        session_tools::call(&self.context, &self.caller(), tool, args).await
    }

    async fn arm_at(&self, key: &str, at: DateTime<Utc>) -> Value {
        self.call(
            "alarms.set",
            json!({"at": at.to_rfc3339(), "key": key, "note": "wave C"}),
        )
        .await
        .unwrap()
    }

    async fn refresh(&self, at: DateTime<Utc>) {
        self.refresh_id(self.child, at).await;
    }

    async fn refresh_id(&self, id: Uuid, at: DateTime<Utc>) {
        activity::refresh(&self.pool, id, at, &thresholds(), Uuid::now_v7())
            .await
            .unwrap();
    }

    async fn subscribe(&self, target: Uuid) {
        let mut tx = self.pool.begin().await.unwrap();
        subs::subscribe_in_tx(&mut tx, self.owner, self.orchestrator, target, 0)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn event_jobs(&self) -> i64 {
        sqlx::query_scalar(
            "select count(*) from scheduled_jobs
             where kind='session_task' and conversation_id=$1
               and payload->>'action'='event'",
        )
        .bind(self.orchestrator)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn alarm_inputs(&self) -> Vec<(String, Value)> {
        sqlx::query_as(
            "select content->0->>'text',origin from inputs
             where conversation_id=$1 order by input_seq",
        )
        .bind(self.orchestrator)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn fill_queue(&self) {
        for n in 0..100 {
            conversations::submit_message(
                &self.pool,
                SubmitMessage {
                    owner_id: self.owner,
                    conversation_id: self.orchestrator,
                    key: Uuid::now_v7().to_string(),
                    content: vec![ContentBlock::Text {
                        text: format!("work {n}"),
                    }],
                    attachment_ids: vec![],
                    retry_of: None,
                    received_at: Utc::now(),
                },
                Uuid::now_v7(),
            )
            .await
            .unwrap();
        }
    }
}

fn scheduler(pool: &PgPool, config: SchedulerConfig) -> Scheduler {
    Scheduler::new(pool.clone(), Dispatcher::channel().0, config)
}

async fn count(pool: &PgPool, query: &'static str, id: Uuid) -> i64 {
    sqlx::query_scalar(query)
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn time_alarms_firing_within_five_seconds_coalesce_into_one_input(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    f.arm_at("gate", now + TimeDelta::seconds(31)).await;
    f.arm_at("disk", now + TimeDelta::seconds(33)).await;
    let sch = scheduler(&f.pool, Default::default());

    assert_eq!(
        sch.tick(now + TimeDelta::seconds(31)).await.unwrap().fired,
        0
    );
    assert_eq!(
        sch.tick(now + TimeDelta::seconds(36)).await.unwrap().fired,
        1
    );

    assert_eq!(
        count(
            &f.pool,
            "select count(*) from inputs where conversation_id=$1",
            f.orchestrator
        )
        .await,
        1
    );
    let (content, origin): (Value, Value) =
        sqlx::query_as("select content,origin from inputs where conversation_id=$1")
            .bind(f.orchestrator)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    let text = content[0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("2 alarms fired"), "{text}");
    assert!(text.contains("\"gate\""), "{text}");
    assert!(text.contains("\"disk\""), "{text}");
    assert_eq!(origin["kind"], "alarm");
    assert_eq!(origin["event"]["kind"], "alarm");

    let delivered: Vec<(Option<Uuid>, String)> = sqlx::query_as(
        "select delivered_input_id,state from alarms where conversation_id=$1 order by key",
    )
    .bind(f.orchestrator)
    .fetch_all(&f.pool)
    .await
    .unwrap();
    assert_eq!(delivered.len(), 2);
    assert!(
        delivered
            .iter()
            .all(|(input, state)| input.is_some() && state == "fired")
    );

    assert_eq!(
        sch.tick(now + TimeDelta::seconds(41)).await.unwrap().fired,
        0
    );
    assert_eq!(
        count(
            &f.pool,
            "select count(*) from inputs where conversation_id=$1",
            f.orchestrator
        )
        .await,
        1
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn restart_and_retry_after_commit_do_not_deliver_twice(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    f.arm_at("gate", now + TimeDelta::seconds(31)).await;
    let first = scheduler(&f.pool, Default::default());
    assert_eq!(
        first
            .tick(now + TimeDelta::seconds(36))
            .await
            .unwrap()
            .fired,
        1
    );
    assert_eq!(
        count(
            &f.pool,
            "select count(*) from inputs where conversation_id=$1",
            f.orchestrator
        )
        .await,
        1
    );

    // Ответ потерялся: задание снова ждёт выполнения, новый планировщик его подхватывает.
    sqlx::query(
        "update scheduled_jobs set state='pending',run_at=$1
         where kind='session_task' and payload->>'action'='alarm_deliver'",
    )
    .bind(now + TimeDelta::seconds(36))
    .execute(&f.pool)
    .await
    .unwrap();
    let restarted = scheduler(&f.pool, Default::default());
    assert_eq!(
        restarted
            .tick(now + TimeDelta::seconds(37))
            .await
            .unwrap()
            .fired,
        0
    );
    assert_eq!(
        count(
            &f.pool,
            "select count(*) from inputs where conversation_id=$1",
            f.orchestrator
        )
        .await,
        1
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn week_ahead_alarm_keeps_queue_patience_from_first_block(pool: PgPool) {
    let f = Fx::new(pool).await;
    let armed = f
        .call("alarms.set", json!({"in_seconds": 7 * 86400}))
        .await
        .unwrap();
    let fire_at = DateTime::parse_from_rfc3339(armed["fires_at"].as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    f.fill_queue().await;
    let sch = scheduler(&f.pool, Default::default());
    let blocked_at = fire_at + TimeDelta::seconds(6);

    assert_eq!(sch.tick(blocked_at).await.unwrap().retried, 1);
    let (blocked_since, created_at): (Option<DateTime<Utc>>, DateTime<Utc>) =
        sqlx::query_as("select blocked_since,created_at from scheduled_jobs where kind='session_task' and payload->>'action'='alarm_deliver'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    let blocked_since = blocked_since.expect("first blocked attempt is recorded");
    assert!(
        (blocked_since - blocked_at).num_seconds().abs() < 5,
        "patience starts at the first blocked attempt, not at {created_at}"
    );

    assert_eq!(
        sch.tick(blocked_at + TimeDelta::hours(23))
            .await
            .unwrap()
            .retried,
        1
    );
    let (again, _): (Option<DateTime<Utc>>, DateTime<Utc>) =
        sqlx::query_as("select blocked_since,created_at from scheduled_jobs where kind='session_task' and payload->>'action'='alarm_deliver'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(again, Some(blocked_since));
}

#[sqlx::test(migrations = "./migrations")]
async fn abandon_marks_the_alarm_failed_with_a_visible_reason(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f.arm_at("gate", now + TimeDelta::seconds(31)).await;
    f.fill_queue().await;
    let sch = scheduler(&f.pool, Default::default());
    let fire_at = now + TimeDelta::seconds(31);
    sch.tick(fire_at + TimeDelta::seconds(6)).await.unwrap();
    let report = sch
        .tick(fire_at + TimeDelta::seconds(6) + TimeDelta::hours(25))
        .await
        .unwrap();
    assert_eq!(report.failed, 1);

    let (state, reason): (String, Option<String>) =
        sqlx::query_as("select state,failure_reason from alarms where id=$1")
            .bind(armed["alarm_id"].as_str().unwrap().parse::<Uuid>().unwrap())
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(state, "failed");
    assert_eq!(reason.as_deref(), Some("delivery_failed"));
}

#[sqlx::test(migrations = "./migrations")]
async fn cancel_by_key_and_id_is_idempotent(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f.arm_at("gate", now + TimeDelta::seconds(60)).await;
    let id = armed["alarm_id"].as_str().unwrap();
    let list = f.call("alarms.list", json!({})).await.unwrap();
    assert_eq!(list["alarms"].as_array().unwrap().len(), 1);

    let cancelled = f
        .call("alarms.cancel", json!({"alarm_id": id}))
        .await
        .unwrap();
    assert_eq!(cancelled["result"], "cancelled");
    assert_eq!(cancelled["alarm"]["state"], "cancelled");
    let again = f
        .call("alarms.cancel", json!({"alarm_id": id}))
        .await
        .unwrap();
    assert_eq!(again["result"], "already_cancelled");
    assert_eq!(
        f.call("alarms.list", json!({})).await.unwrap()["alarms"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        count(
            &f.pool,
            "select count(*) from scheduled_jobs where kind='session_task' and state='pending' and payload->>'action'='alarm_deliver'",
            f.orchestrator
        )
        .await,
        0
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn session_state_condition_fires_after_the_target_settles(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child]},
                "key": "watch"
            }),
        )
        .await
        .unwrap();
    assert_eq!(armed["state"], "armed");

    f.refresh(now - TimeDelta::hours(1)).await;
    f.refresh(now + TimeDelta::seconds(1)).await;
    let sch = scheduler(&f.pool, Default::default());
    assert_eq!(
        sch.tick(now + TimeDelta::seconds(10)).await.unwrap().fired,
        1
    );
    let id: Uuid = armed["alarm_id"].as_str().unwrap().parse().unwrap();
    let (state, reason, delivered): (String, Option<String>, Option<Uuid>) =
        sqlx::query_as("select state,fired_reason,delivered_input_id from alarms where id=$1")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("condition"));
    assert!(delivered.is_some());
}

#[sqlx::test(migrations = "./migrations")]
async fn satisfied_condition_arms_nothing_and_says_so(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    f.refresh(now - TimeDelta::hours(1)).await;
    let satisfied = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child], "states": ["idle"]}
            }),
        )
        .await
        .unwrap();
    assert_eq!(satisfied["state"], "satisfied");
    assert_eq!(satisfied["current"], "idle");
    assert_eq!(
        f.call("alarms.list", json!({})).await.unwrap()["alarms"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn snapshot_after_reconnect_shows_current_alarms(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    f.arm_at("gate", now + TimeDelta::seconds(60)).await;
    let snap = snapshot::conversation(&f.pool, f.owner, f.orchestrator, 10)
        .await
        .unwrap()
        .unwrap();
    let data = serde_json::to_value(&snap.data).unwrap();
    let alarms = data["alarms"].as_array().unwrap();
    assert_eq!(alarms.len(), 1);
    assert_eq!(alarms[0]["key"], "gate");
    assert_eq!(alarms[0]["state"], "armed");

    f.call("alarms.cancel", json!({"key": "gate"}))
        .await
        .unwrap();
    let snap = snapshot::conversation(&f.pool, f.owner, f.orchestrator, 10)
        .await
        .unwrap()
        .unwrap();
    let data = serde_json::to_value(&snap.data).unwrap();
    assert!(data["alarms"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn abandon_handler_marks_an_armed_alarm_failed(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f.arm_at("gate", now + TimeDelta::seconds(60)).await;
    let id = armed["alarm_id"].as_str().unwrap().parse::<Uuid>().unwrap();
    let mut conn = f.pool.acquire().await.unwrap();
    session_tools::jobs::abandon_in_tx(
        &mut conn,
        f.owner,
        Some(f.orchestrator),
        &json!({"action": "alarm_deliver", "alarm_id": id}),
        now,
    )
    .await
    .unwrap();
    let (state, reason): (String, Option<String>) =
        sqlx::query_as("select state,failure_reason from alarms where id=$1")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(state, "failed");
    assert_eq!(reason.as_deref(), Some("delivery_failed"));
}

async fn alarm_state(pool: &PgPool, id: Uuid) -> (String, Option<String>, Value) {
    sqlx::query_as("select state,fired_reason,collected from alarms where id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn alarm_id(value: &Value) -> Uuid {
    value["alarm_id"].as_str().unwrap().parse().unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn all_condition_fires_only_when_every_target_settles(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child, f.child2], "all": true},
                "key": "all"
            }),
        )
        .await
        .unwrap();
    assert_eq!(armed["state"], "armed");
    let id = alarm_id(&armed);

    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    assert_eq!(alarm_state(&f.pool, id).await.0, "armed");
    assert_eq!(f.event_jobs().await, 0, "the condition is not met yet");

    f.refresh_id(f.child2, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child2, now + TimeDelta::seconds(1)).await;
    let (state, reason, collected) = alarm_state(&f.pool, id).await;
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("condition"));
    assert!(collected["sessions"][f.child.to_string()].is_object());
    assert!(collected["sessions"][f.child2.to_string()].is_object());

    let sch = scheduler(&f.pool, Default::default());
    assert_eq!(
        sch.tick(now + TimeDelta::seconds(10)).await.unwrap().fired,
        1
    );
    let inputs = f.alarm_inputs().await;
    assert_eq!(inputs.len(), 1);
    assert!(
        inputs[0].0.contains(&f.child.to_string()),
        "{}",
        inputs[0].0
    );
    assert!(
        inputs[0].0.contains(&f.child2.to_string()),
        "{}",
        inputs[0].0
    );
    assert!(inputs[0].0.contains("idle"), "{}", inputs[0].0);
}

#[sqlx::test(migrations = "./migrations")]
async fn batch_holds_summaries_and_delivers_them_in_one_input(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {
                    "kind": "session_state",
                    "session_ids": [f.child, f.child2],
                    "all": true
                },
                "batch": true,
                "key": "wave"
            }),
        )
        .await
        .unwrap();
    assert_eq!(armed["state"], "armed");
    let id = alarm_id(&armed);

    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    assert_eq!(f.event_jobs().await, 0, "the batch holds the idle result");
    let (state, _, collected) = alarm_state(&f.pool, id).await;
    assert_eq!(state, "armed");
    assert!(collected["sessions"][f.child.to_string()].is_object());

    f.refresh_id(f.child2, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child2, now + TimeDelta::seconds(1)).await;
    assert_eq!(alarm_state(&f.pool, id).await.0, "fired");
    assert_eq!(f.event_jobs().await, 0, "both results are in the batch");

    let sch = scheduler(&f.pool, Default::default());
    assert_eq!(
        sch.tick(now + TimeDelta::seconds(10)).await.unwrap().fired,
        1
    );
    let inputs = f.alarm_inputs().await;
    assert_eq!(inputs.len(), 1, "one input for the whole batch");
    let text = &inputs[0].0;
    assert!(text.contains(&f.child.to_string()), "{text}");
    assert!(text.contains(&f.child2.to_string()), "{text}");
    assert_eq!(text.matches("[canterlot] Session").count(), 2, "{text}");
    assert_eq!(inputs[0].1["kind"], "alarm");
}

#[sqlx::test(migrations = "./migrations")]
async fn cancelling_a_batch_releases_held_summaries(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child, f.child2], "all": true},
                "batch": true,
                "key": "wave"
            }),
        )
        .await
        .unwrap();
    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    assert_eq!(f.event_jobs().await, 0);

    let cancelled = f
        .call("alarms.cancel", json!({"alarm_id": alarm_id(&armed)}))
        .await
        .unwrap();
    assert_eq!(cancelled["result"], "cancelled");
    assert_eq!(f.event_jobs().await, 1, "the held result is released");

    assert_eq!(
        scheduler(&f.pool, Default::default())
            .tick(now + TimeDelta::seconds(10))
            .await
            .unwrap()
            .fired,
        1
    );
    let inputs = f.alarm_inputs().await;
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].0.contains(&f.child.to_string()));
}

#[sqlx::test(migrations = "./migrations")]
async fn batch_release_drops_a_summary_after_unsubscribe_or_accept(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child, f.child2], "all": true},
                "batch": true,
                "key": "wave"
            }),
        )
        .await
        .unwrap();
    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    f.refresh_id(f.child2, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child2, now + TimeDelta::seconds(1)).await;

    let mut tx = f.pool.begin().await.unwrap();
    subs::unsubscribe_in_tx(&mut tx, f.orchestrator, f.child, "unsubscribed")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query(
        "update session_subscriptions set closed_at=clock_timestamp(),closed_reason='accepted'
         where subscriber_id=$1 and target_id=$2",
    )
    .bind(f.orchestrator)
    .bind(f.child2)
    .execute(&f.pool)
    .await
    .unwrap();

    f.call("alarms.cancel", json!({"alarm_id": alarm_id(&armed)}))
        .await
        .unwrap();
    assert_eq!(
        f.event_jobs().await,
        0,
        "neither summary survives a dead subscription"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn batch_release_drops_a_summary_after_resubscribe_with_a_new_generation(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child, f.child2], "all": true},
                "batch": true,
                "key": "wave"
            }),
        )
        .await
        .unwrap();
    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    let before: DateTime<Utc> = sqlx::query_scalar(
        "select armed_at from session_subscriptions where subscriber_id=$1 and target_id=$2",
    )
    .bind(f.orchestrator)
    .bind(f.child)
    .fetch_one(&f.pool)
    .await
    .unwrap();

    f.subscribe(f.child).await;
    let after: DateTime<Utc> = sqlx::query_scalar(
        "select armed_at from session_subscriptions where subscriber_id=$1 and target_id=$2",
    )
    .bind(f.orchestrator)
    .bind(f.child)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(after > before, "resubscribe starts a new generation");

    f.call("alarms.cancel", json!({"alarm_id": alarm_id(&armed)}))
        .await
        .unwrap();
    assert_eq!(f.event_jobs().await, 0, "the old generation is stale");
}

#[sqlx::test(migrations = "./migrations")]
async fn overlapping_batches_keep_each_summary_in_the_first_batch_that_collected_it(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let first = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child, f.child2], "all": true},
                "batch": true,
                "key": "first"
            }),
        )
        .await
        .unwrap();
    let second = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child, f.child2], "all": true},
                "batch": true,
                "key": "second"
            }),
        )
        .await
        .unwrap();

    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    let (_, _, first_collected) = alarm_state(&f.pool, alarm_id(&first)).await;
    let (_, _, second_collected) = alarm_state(&f.pool, alarm_id(&second)).await;
    assert!(first_collected["sessions"][f.child.to_string()].is_object());
    assert!(
        second_collected["sessions"]
            .get(f.child.to_string())
            .and_then(|entry| entry.get("subscription_id"))
            .is_none(),
        "the first batch owns the summary; the second only counts the state"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn watchdog_cancels_silently_when_the_condition_holds(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child]},
                "only_on_timeout": true,
                "deadline_seconds": 120,
                "key": "watch"
            }),
        )
        .await
        .unwrap();
    assert_eq!(armed["state"], "armed");

    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    let (state, reason, _) = alarm_state(&f.pool, alarm_id(&armed)).await;
    assert_eq!(state, "cancelled");
    assert_eq!(reason.as_deref(), Some("condition_met"));
    assert_eq!(f.event_jobs().await, 0, "the watchdog says nothing");
    assert_eq!(
        scheduler(&f.pool, Default::default())
            .tick(now + TimeDelta::minutes(10))
            .await
            .unwrap()
            .fired,
        0
    );
    assert!(f.alarm_inputs().await.is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn watchdog_fires_on_the_deadline_when_the_targets_never_settle(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child]},
                "only_on_timeout": true,
                "deadline_seconds": 120,
                "key": "watch"
            }),
        )
        .await
        .unwrap();
    let id = alarm_id(&armed);
    let sch = scheduler(&f.pool, Default::default());

    assert_eq!(
        sch.tick(now + TimeDelta::seconds(126)).await.unwrap().fired,
        1
    );
    let (state, reason, _) = alarm_state(&f.pool, id).await;
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("timed_out"));
    let inputs = f.alarm_inputs().await;
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].0.contains("timed out"), "{}", inputs[0].0);
}

#[sqlx::test(migrations = "./migrations")]
async fn released_batch_summary_is_not_delivered_twice_after_a_restart(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child, f.child2], "all": true},
                "batch": true,
                "key": "wave"
            }),
        )
        .await
        .unwrap();
    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    f.call("alarms.cancel", json!({"alarm_id": alarm_id(&armed)}))
        .await
        .unwrap();

    let first = scheduler(&f.pool, Default::default());
    assert_eq!(
        first
            .tick(now + TimeDelta::seconds(10))
            .await
            .unwrap()
            .fired,
        1
    );
    assert_eq!(f.alarm_inputs().await.len(), 1);

    // Ответ потерялся: задание снова ждёт выполнения, новый планировщик его подхватывает.
    sqlx::query(
        "update scheduled_jobs set state='pending',run_at=$1
         where kind='session_task' and payload->>'action'='event'",
    )
    .bind(now + TimeDelta::seconds(10))
    .execute(&f.pool)
    .await
    .unwrap();
    let restarted = scheduler(&f.pool, Default::default());
    assert_eq!(
        restarted
            .tick(now + TimeDelta::seconds(11))
            .await
            .unwrap()
            .fired,
        1
    );
    assert_eq!(
        f.alarm_inputs().await.len(),
        1,
        "the released summary has one input"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn batch_and_subscription_fire_once_at_the_end(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    f.subscribe(f.child).await;
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child, f.child2], "all": true},
                "batch": true,
                "key": "wave"
            }),
        )
        .await
        .unwrap();
    // Second target is watched by a watchdog, not batched.
    let watchdog = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child3]},
                "only_on_timeout": true,
                "deadline_seconds": 3600,
                "key": "watch"
            }),
        )
        .await
        .unwrap();
    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    f.refresh_id(f.child2, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child2, now + TimeDelta::seconds(1)).await;
    f.refresh_id(f.child3, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child3, now + TimeDelta::seconds(1)).await;
    assert_eq!(alarm_state(&f.pool, alarm_id(&armed)).await.0, "fired");
    assert_eq!(
        alarm_state(&f.pool, alarm_id(&watchdog)).await.0,
        "cancelled"
    );
    assert_eq!(f.event_jobs().await, 0, "no separate subscription result");
    assert_eq!(
        scheduler(&f.pool, Default::default())
            .tick(now + TimeDelta::seconds(10))
            .await
            .unwrap()
            .fired,
        1
    );
    assert_eq!(f.alarm_inputs().await.len(), 1);
    assert_eq!(
        scheduler(&f.pool, Default::default())
            .tick(now + TimeDelta::hours(2))
            .await
            .unwrap()
            .fired,
        0,
        "the watchdog stays silent at its deadline"
    );
    assert_eq!(f.alarm_inputs().await.len(), 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn batch_needs_all_and_conditions_only(pool: PgPool) {
    let f = Fx::new(pool).await;
    let error = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child]},
                "batch": true
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, session_tools::ToolErrorCode::InvalidArguments);

    let error = f
        .call("alarms.set", json!({"in_seconds": 60, "batch": true}))
        .await
        .unwrap_err();
    assert_eq!(error.code, session_tools::ToolErrorCode::InvalidArguments);

    let error = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child]},
                "only_on_timeout": true,
                "deadline_seconds": 30
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, session_tools::ToolErrorCode::InvalidArguments);
}

#[sqlx::test(migrations = "./migrations")]
async fn locked_alarm_does_not_lose_its_deadline_job(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child]},
                "only_on_timeout": true,
                "deadline_seconds": 120,
                "key": "watch"
            }),
        )
        .await
        .unwrap();
    let id = alarm_id(&armed);

    // Чужая транзакция держит строку будильника, когда наступает срок.
    let mut blocked = f.pool.begin().await.unwrap();
    sqlx::query("select id from alarms where id=$1 for update")
        .bind(id)
        .execute(&mut *blocked)
        .await
        .unwrap();
    let sch = scheduler(&f.pool, Default::default());
    let report = sch.tick(now + TimeDelta::seconds(126)).await.unwrap();
    assert_eq!(report.retried, 1, "the delivery job asks for a normal retry");
    assert_eq!(report.cancelled, 0, "a locked alarm is not settled");
    assert_eq!(alarm_state(&f.pool, id).await.0, "armed");
    assert!(f.alarm_inputs().await.is_empty());

    blocked.commit().await.unwrap();
    assert_eq!(
        sch.tick(now + TimeDelta::seconds(130)).await.unwrap().fired,
        1
    );
    let (state, reason, _) = alarm_state(&f.pool, id).await;
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("timed_out"));
    assert_eq!(f.alarm_inputs().await.len(), 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn locked_alarm_does_not_miss_target_deletion(pool: PgPool) {
    let f = Fx::new(pool).await;
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child], "states": ["deleted"]},
                "key": "gone"
            }),
        )
        .await
        .unwrap();
    let id = alarm_id(&armed);

    let mut blocked = f.pool.begin().await.unwrap();
    sqlx::query("select id from alarms where id=$1 for update")
        .bind(id)
        .execute(&mut *blocked)
        .await
        .unwrap();
    let pool = f.pool.clone();
    let child = f.child;
    let deleted = tokio::spawn(async move {
        let mut tx = pool.begin().await.unwrap();
        canterlot_hub::session_tools::alarms::on_target_deleted_in_tx(&mut tx, child, Utc::now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !deleted.is_finished(),
        "deletion waits for the alarm row instead of skipping it"
    );

    blocked.commit().await.unwrap();
    deleted.await.unwrap();
    let (state, reason, collected) = alarm_state(&f.pool, id).await;
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("condition"));
    assert_eq!(
        collected["sessions"][f.child.to_string()]["state"],
        "deleted"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn arm_path_uses_completion_for_satisfied_condition(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    f.refresh(now - TimeDelta::hours(1)).await;

    let satisfied = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child], "states": ["idle"]}
            }),
        )
        .await
        .unwrap();
    assert_eq!(satisfied["state"], "satisfied");
    assert_eq!(satisfied["current"], "idle");

    // Первый итог `all` собирается через то же решение о завершении.
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child, f.child2], "all": true},
                "key": "all"
            }),
        )
        .await
        .unwrap();
    assert_eq!(armed["state"], "armed");
    let (_, _, collected) = alarm_state(&f.pool, alarm_id(&armed)).await;
    assert_eq!(
        collected["sessions"][f.child.to_string()]["state"], "idle",
        "the already settled target is part of the initial all collection"
    );
    assert!(collected["sessions"].get(f.child2.to_string()).is_none());
}

#[sqlx::test(migrations = "./migrations")]
async fn deletion_produces_exactly_one_notice(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child, f.child2], "all": true},
                "batch": true,
                "key": "wave"
            }),
        )
        .await
        .unwrap();
    f.refresh_id(f.child2, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child2, now + TimeDelta::seconds(1)).await;

    // Удаление цели: обычная подписка шлёт одно сообщение, пачка только засчитывает.
    let mut tx = f.pool.begin().await.unwrap();
    subs::on_target_deleted_in_tx(&mut tx, f.child, now).await.unwrap();
    canterlot_hub::session_tools::alarms::on_target_deleted_in_tx(
        &mut tx,
        f.child,
        now,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // В настоящем удалении разговор уже убран, поэтому итог события говорит «was deleted».
    sqlx::query("delete from session_subscriptions where target_id=$1 or subscriber_id=$1")
        .bind(f.child)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("delete from queue_state where conversation_id=$1")
        .bind(f.child)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("delete from conversations where id=$1")
        .bind(f.child)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(alarm_state(&f.pool, alarm_id(&armed)).await.0, "fired");

    assert_eq!(
        scheduler(&f.pool, Default::default())
            .tick(now + TimeDelta::seconds(10))
            .await
            .unwrap()
            .fired,
        2,
        "the deletion notice and the batch are separate deliveries"
    );
    let inputs = f.alarm_inputs().await;
    let notices = inputs
        .iter()
        .filter(|(text, _)| text.contains("was deleted"))
        .count();
    assert_eq!(notices, 1, "deletion is announced once: {inputs:?}");
    let batch = inputs
        .iter()
        .find(|(text, _)| text.contains("settled (all, batched)"))
        .expect("the batch input");
    assert!(batch.0.contains("reported separately"), "{}", batch.0);
    assert!(!batch.0.contains("was deleted"), "{}", batch.0);
}

#[sqlx::test(migrations = "./migrations")]
async fn failed_and_idle_complete_an_all_batch_with_the_failure_reported_once(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child, f.child2], "all": true},
                "batch": true,
                "key": "wave"
            }),
        )
        .await
        .unwrap();
    let id = alarm_id(&armed);

    let execution = Uuid::now_v7();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state,outcome)
         values($1,$2,1,'failed','error')",
    )
    .bind(execution)
    .bind(f.child)
    .execute(&f.pool)
    .await
    .unwrap();
    f.refresh_id(f.child, now).await;
    assert_eq!(alarm_state(&f.pool, id).await.0, "armed");
    assert_eq!(f.event_jobs().await, 1, "failed goes out as a normal result");

    let sch = scheduler(&f.pool, Default::default());
    assert_eq!(
        sch.tick(now + TimeDelta::seconds(10)).await.unwrap().fired,
        1
    );

    f.refresh_id(f.child2, now + TimeDelta::seconds(10)).await;
    f.refresh_id(f.child2, now + TimeDelta::seconds(20)).await;
    assert_eq!(
        alarm_state(&f.pool, id).await.0,
        "fired",
        "a failed target satisfies all"
    );
    assert_eq!(
        sch.tick(now + TimeDelta::seconds(30)).await.unwrap().fired,
        1
    );
    let inputs = f.alarm_inputs().await;
    assert_eq!(inputs.len(), 2, "one failure notice and one batch");
    assert_eq!(
        inputs
            .iter()
            .filter(|(text, _)| text.contains("is now failed"))
            .count(),
        1,
        "the failure is reported once"
    );
    let batch = inputs
        .iter()
        .find(|(text, _)| text.contains("settled (all, batched)"))
        .expect("the batch input");
    assert!(batch.0.contains("reported separately"), "{}", batch.0);
    assert!(batch.0.contains(&f.child.to_string()), "{}", batch.0);
}

async fn add_answer_run(
    pool: &PgPool,
    machine: Uuid,
    conversation: Uuid,
    execution: Uuid,
    finished_at: DateTime<Utc>,
    seq: i64,
    answer: &str,
) -> Uuid {
    let command = Uuid::now_v7();
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,1,'deliver_input','{}','completed')",
    )
    .bind(command)
    .bind(machine)
    .bind(conversation)
    .bind(execution)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at,duration_ms)
         values($1,$2,$3,$4-($5::bigint * interval '1 millisecond'),$4,$5)",
    )
    .bind(command)
    .bind(execution)
    .bind(conversation)
    .bind(finished_at)
    .bind(1000_i64)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,input_command_id,parent_block_id,created_seq,created_at)
         values($1,$2,$3,'text','final',1,$4,$5,null,$6,$7)",
    )
    .bind(Uuid::now_v7())
    .bind(execution)
    .bind(conversation)
    .bind(answer)
    .bind(command)
    .bind(seq)
    .bind(finished_at)
    .execute(pool)
    .await
    .unwrap();
    command
}

#[sqlx::test(migrations = "./migrations")]
async fn held_summary_dropped_after_unsubscribe_accept_or_resubscribe_on_ordinary_delivery(
    pool: PgPool,
) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {
                    "kind": "session_state",
                    "session_ids": [f.child, f.child2, f.child3],
                    "all": true
                },
                "batch": true,
                "key": "wave"
            }),
        )
        .await
        .unwrap();
    for (target, offset) in [
        (f.child, 1),
        (f.child2, 2),
        (f.child3, 3),
    ] {
        f.refresh_id(target, now - TimeDelta::hours(1)).await;
        f.refresh_id(target, now + TimeDelta::seconds(offset)).await;
    }
    assert_eq!(alarm_state(&f.pool, alarm_id(&armed)).await.0, "fired");

    let mut tx = f.pool.begin().await.unwrap();
    subs::unsubscribe_in_tx(&mut tx, f.orchestrator, f.child, "unsubscribed")
        .await
        .unwrap();
    subs::close_for_accept_in_tx(&mut tx, f.child2, Some(f.orchestrator), "accepted")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    f.subscribe(f.child3).await;

    assert_eq!(
        scheduler(&f.pool, Default::default())
            .tick(now + TimeDelta::seconds(10))
            .await
            .unwrap()
            .fired,
        1
    );
    let inputs = f.alarm_inputs().await;
    assert_eq!(inputs.len(), 1);
    let text = &inputs[0].0;
    assert!(text.contains("settled (all, batched)"), "{text}");
    assert!(
        !text.contains("is now idle"),
        "no stale summary is rendered: {text}"
    );
    assert!(!text.contains("Final answer"), "{text}");
}

#[sqlx::test(migrations = "./migrations")]
async fn later_run_does_not_replace_a_held_result(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child], "all": true},
                "batch": true,
                "key": "wave"
            }),
        )
        .await
        .unwrap();
    let execution = Uuid::now_v7();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state,outcome)
         values($1,$2,1,'stopped','done')",
    )
    .bind(execution)
    .bind(f.child)
    .execute(&f.pool)
    .await
    .unwrap();
    let t1 = now + TimeDelta::seconds(1);
    add_answer_run(&f.pool, f.machine, f.child, execution, t1, 1, "answer one").await;
    f.refresh_id(f.child, now + TimeDelta::seconds(2)).await;
    assert_eq!(alarm_state(&f.pool, alarm_id(&armed)).await.0, "fired");

    // Цель успела сходить ещё раз до отрисовки удержанного итога.
    add_answer_run(
        &f.pool,
        f.machine,
        f.child,
        execution,
        now + TimeDelta::seconds(15),
        2,
        "answer two",
    )
    .await;
    assert_eq!(
        scheduler(&f.pool, Default::default())
            .tick(now + TimeDelta::seconds(20))
            .await
            .unwrap()
            .fired,
        1
    );
    let inputs = f.alarm_inputs().await;
    let text = &inputs[0].0;
    assert!(text.contains("answer one"), "{text}");
    assert!(
        !text.contains("answer two"),
        "a later run does not replace the held result: {text}"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn cancellation_racing_unsubscribe_does_not_resurrect_the_event(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child, f.child2], "all": true},
                "batch": true,
                "key": "wave"
            }),
        )
        .await
        .unwrap();
    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    assert_eq!(f.event_jobs().await, 0, "the batch holds the result");

    // Отписка держит строку подписки, не завершая транзакцию.
    let mut unsubscribe = f.pool.begin().await.unwrap();
    subs::unsubscribe_in_tx(&mut unsubscribe, f.orchestrator, f.child, "unsubscribed")
        .await
        .unwrap();
    let context = f.context.clone();
    let caller = f.caller();
    let id = alarm_id(&armed);
    let cancel = tokio::spawn(async move {
        session_tools::call(
            &context,
            &caller,
            "alarms.cancel",
            json!({"alarm_id": id}),
        )
        .await
        .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !cancel.is_finished(),
        "release waits for the subscription row before inserting the event"
    );
    unsubscribe.commit().await.unwrap();
    cancel.await.unwrap();

    assert_eq!(
        f.event_jobs().await,
        0,
        "an unsubscribed generation cannot resurrect a held result"
    );
}
