use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    activity::{self, Thresholds},
    config::{SchedulerConfig, SessionToolsConfig},
    conversations::{self, SubmitMessage},
    delivery::ContentBlock,
    dispatcher::Dispatcher,
    limits::{self, DataConfidence, Observation, ObservationSource, Window},
    machine_info,
    scheduler::Scheduler,
    session_tools::{self, Caller, ToolContext, settle::SettleConfig, subs},
    snapshot, testkit,
};
use canterlot_proto::worker::v1::{DiskUsage, MachineMetrics};
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
                limits: Default::default(),
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

    async fn observe(
        &self,
        provider: &str,
        window: Window,
        used: u16,
        resets_at: Option<DateTime<Utc>>,
        observed_at: DateTime<Utc>,
        confidence: DataConfidence,
    ) {
        let mut tx = self.pool.begin().await.unwrap();
        limits::record_observation_in_tx(
            &mut tx,
            self.owner,
            &Observation {
                provider: provider.into(),
                window,
                used_permille: used,
                resets_at,
                observed_at,
                source: ObservationSource::Poll,
                data_confidence: confidence,
                machine_id: self.machine,
                execution_id: None,
            },
            observed_at,
            &self.context.limits,
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    async fn arm_limit(
        &self,
        key: &str,
        provider: &str,
        window: &str,
        below_percent: Option<f64>,
    ) -> Result<Value, session_tools::ToolError> {
        let mut when = json!({"kind": "limit_reset", "provider": provider, "window": window});
        if let Some(below) = below_percent {
            when["below_percent"] = json!(below);
        }
        self.call("alarms.set", json!({"when": when, "key": key}))
            .await
    }

    async fn arm_machine(
        &self,
        key: &str,
        free_disk_gb_above: Option<f64>,
        load_per_core_below: Option<f64>,
        mem_available_gb_above: Option<f64>,
    ) -> Result<Value, session_tools::ToolError> {
        let mut when = json!({"kind": "machine"});
        if let Some(value) = free_disk_gb_above {
            when["free_disk_gb_above"] = json!(value);
        }
        if let Some(value) = load_per_core_below {
            when["load_per_core_below"] = json!(value);
        }
        if let Some(value) = mem_available_gb_above {
            when["mem_available_gb_above"] = json!(value);
        }
        self.call("alarms.set", json!({"when": when, "key": key}))
            .await
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

    async fn alarm_kind_inputs(&self) -> Vec<(String, Value)> {
        sqlx::query_as(
            "select content->0->>'text',origin from inputs
             where conversation_id=$1 and origin->>'kind'='alarm' order by input_seq",
        )
        .bind(self.orchestrator)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn fill_queue(&self) {
        self.fill_queue_n(100).await;
    }

    async fn fill_queue_n(&self, count: usize) {
        for n in 0..count {
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
async fn list_carries_structured_limit_and_machine_conditions(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.arm_limit("usage", "claude", "weekly", Some(40.0))
        .await
        .unwrap();
    f.arm_machine("freed", Some(50.0), Some(0.5), Some(8.0))
        .await
        .unwrap();
    let list = f.call("alarms.list", json!({})).await.unwrap();
    let alarms = list["alarms"].as_array().unwrap();
    let limit = alarms.iter().find(|alarm| alarm["key"] == "usage").unwrap();
    assert_eq!(limit["provider"], "claude");
    assert_eq!(limit["window"], "weekly");
    assert_eq!(limit["below_percent"], 40.0);
    let machine = alarms.iter().find(|alarm| alarm["key"] == "freed").unwrap();
    assert_eq!(machine["free_disk_gb_above"], 50.0);
    assert_eq!(machine["load_per_core_below"], 0.5);
    assert_eq!(machine["mem_available_gb_above"], 8.0);
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
    assert_eq!(
        report.retried, 1,
        "the delivery job asks for a normal retry"
    );
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

/// Запертая строка будильника не откладывает доставку бесконечно: терпение считается
/// с первой заблокированной попытки, через сутки будильник уходит в `failed`.
#[sqlx::test(migrations = "./migrations")]
async fn contended_delivery_gives_up_after_the_queue_patience(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f.arm_at("gate", now + TimeDelta::seconds(31)).await;
    let id = alarm_id(&armed);

    // Чужая транзакция держит строку будильника всё время попыток.
    let mut blocker = f.pool.begin().await.unwrap();
    sqlx::query("select id from alarms where id=$1 for update")
        .bind(id)
        .execute(&mut *blocker)
        .await
        .unwrap();

    let sch = scheduler(&f.pool, Default::default());
    let at = now + TimeDelta::seconds(40);
    let report = sch.tick(at).await.unwrap();
    assert_eq!(report.retried, 1, "contention asks for a retry");
    let blocked_since: Option<DateTime<Utc>> = sqlx::query_scalar(
        "select blocked_since from scheduled_jobs
         where kind='session_task' and payload->>'action'='alarm_deliver'
           and payload->>'alarm_id'=$1",
    )
    .bind(id.to_string())
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let blocked_since = blocked_since.expect("the first contended attempt records its start");
    assert!(
        (blocked_since - at).num_seconds().abs() < 5,
        "patience starts at the contended attempt: {blocked_since}"
    );
    assert!(f.alarm_inputs().await.is_empty());

    // Сутки спустя задание уходит в обработчик отказа, но не ждёт запертую строку:
    // отказ откладывается, а постороннее задание всё равно выполняется.
    let later = at + TimeDelta::hours(25);
    canterlot_hub::scheduler::schedule_message(
        &f.pool,
        f.owner,
        "unrelated",
        canterlot_hub::scheduler::NewScheduled {
            conversation_id: f.orchestrator,
            run_at: later,
            content: vec![ContentBlock::Text {
                text: "unrelated work".into(),
            }],
            attachment_ids: vec![],
        },
        Utc::now(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let report = tokio::time::timeout(Duration::from_secs(5), sch.tick(later))
        .await
        .expect("the scheduler must not wait for the alarm row")
        .unwrap();
    assert!(report.retried >= 1, "the finalization is deferred");
    assert_eq!(report.fired, 1, "an unrelated job still runs");
    assert_eq!(alarm_state(&f.pool, id).await.0, "armed");

    // Строка освободилась: отложенный отказ финализируется без ожидания.
    blocker.commit().await.unwrap();
    let report = sch.tick(later + TimeDelta::seconds(3)).await.unwrap();
    assert_eq!(report.cancelled, 1);
    let (state, reason): (String, Option<String>) =
        sqlx::query_as("select state,failure_reason from alarms where id=$1")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(state, "failed");
    assert_eq!(reason.as_deref(), Some("delivery_contended"));
    assert!(f.alarm_kind_inputs().await.is_empty());
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
        collected["sessions"][f.child.to_string()]["state"],
        "idle",
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
    subs::on_target_deleted_in_tx(&mut tx, f.child, now)
        .await
        .unwrap();
    canterlot_hub::session_tools::alarms::on_target_deleted_in_tx(&mut tx, f.child, now)
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
    assert_eq!(
        f.event_jobs().await,
        1,
        "failed goes out as a normal result"
    );

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
    for (target, offset) in [(f.child, 1), (f.child2, 2), (f.child3, 3)] {
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

/// Ход, чей ответ пришёл в хаб после `finished_at` (повтор журнала после переподключения):
/// граница удержанного итога — идентификатор хода, а не время воркера.
#[sqlx::test(migrations = "./migrations")]
async fn replayed_answer_after_the_run_finished_reaches_the_held_summary(pool: PgPool) {
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
    let command = Uuid::now_v7();
    let finished_at = now + TimeDelta::seconds(1);
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,1,'deliver_input','{}','completed')",
    )
    .bind(command)
    .bind(f.machine)
    .bind(f.child)
    .bind(execution)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at,duration_ms)
         values($1,$2,$3,$4-interval '1 second',$4,1000)",
    )
    .bind(command)
    .bind(execution)
    .bind(f.child)
    .bind(finished_at)
    .execute(&f.pool)
    .await
    .unwrap();
    // Ответ хаба создан позже завершения хода: часы воркера и хаба расходятся.
    sqlx::query(
        "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,input_command_id,parent_block_id,created_seq,created_at)
         values($1,$2,$3,'text','final',1,'replayed answer',$4,null,1,$5)",
    )
    .bind(Uuid::now_v7())
    .bind(execution)
    .bind(f.child)
    .bind(command)
    .bind(finished_at + TimeDelta::seconds(30))
    .execute(&f.pool)
    .await
    .unwrap();

    f.refresh_id(f.child, now + TimeDelta::seconds(40)).await;
    assert_eq!(alarm_state(&f.pool, alarm_id(&armed)).await.0, "fired");
    assert_eq!(
        scheduler(&f.pool, Default::default())
            .tick(now + TimeDelta::seconds(50))
            .await
            .unwrap()
            .fired,
        1
    );
    let inputs = f.alarm_inputs().await;
    let text = &inputs[0].0;
    assert!(text.contains("replayed answer"), "{text}");
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
        session_tools::call(&context, &caller, "alarms.cancel", json!({"alarm_id": id}))
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

#[sqlx::test(migrations = "./migrations")]
async fn abandoned_batch_releases_held_summaries_to_the_subscription(pool: PgPool) {
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
    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    assert_eq!(f.event_jobs().await, 0);

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
    drop(conn);
    assert_eq!(alarm_state(&f.pool, id).await.0, "failed");
    assert_eq!(
        f.event_jobs().await,
        1,
        "the held summary returns to the subscription"
    );

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
    assert!(
        inputs[0].0.contains(&f.child.to_string()),
        "{}",
        inputs[0].0
    );
    assert!(inputs[0].0.contains("is now idle"), "{}", inputs[0].0);
}

#[sqlx::test(migrations = "./migrations")]
async fn settle_after_deadline_keeps_the_timeout(pool: PgPool) {
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

    // Срок истёк, задание срока задержалось, а цель как раз устоялась.
    f.refresh_id(f.child, now + TimeDelta::seconds(10)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(121)).await;
    assert_eq!(
        alarm_state(&f.pool, id).await.0,
        "armed",
        "a settle after the cutoff cannot cancel the watchdog"
    );

    let sch = scheduler(&f.pool, Default::default());
    assert_eq!(
        sch.tick(now + TimeDelta::seconds(126)).await.unwrap().fired,
        1
    );
    let (state, reason, _) = alarm_state(&f.pool, id).await;
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("timed_out"));
    assert!(
        f.alarm_inputs().await[0].0.contains("timed out"),
        "{:?}",
        f.alarm_inputs().await
    );
}

/// Двадцать пачек по две цели с ответами по 8 КиБ: их общий текст не влезает в один ввод.
async fn arm_large_batches(f: &Fx, now: DateTime<Utc>) -> Vec<(Uuid, Uuid)> {
    let mut pairs = Vec::new();
    for n in 0..20 {
        let one = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
        let two = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
        f.call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [one, two], "all": true},
                "batch": true,
                "key": format!("wave-{n}")
            }),
        )
        .await
        .unwrap();
        pairs.push((one, two));
    }
    // Блоки ответов должны быть позже взвода подписки, иначе окно итога их не увидит.
    for (one, two) in &pairs {
        for (target, n) in [(one, 1_i64), (two, 2_i64)] {
            let execution = Uuid::now_v7();
            sqlx::query(
                "insert into executions(id,conversation_id,generation,state,outcome)
                 values($1,$2,1,'stopped','done')",
            )
            .bind(execution)
            .bind(target)
            .execute(&f.pool)
            .await
            .unwrap();
            sqlx::query(
                "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,parent_block_id,created_seq,created_at)
                 values($1,$2,$3,'text','final',1,$4,null,$5,$6)",
            )
            .bind(Uuid::now_v7())
            .bind(execution)
            .bind(target)
            .bind("x".repeat(8192))
            .bind(n)
            // Ответ должен быть позже взвода подписки: взвод идёт стеной, а `now`
            // снят до неё, поэтому берём текущие часы, а не сдвиг от `now`.
            .bind(Utc::now())
            .execute(&f.pool)
            .await
            .unwrap();
        }
        f.refresh_id(*one, now + TimeDelta::seconds(2)).await;
        f.refresh_id(*two, now + TimeDelta::seconds(2)).await;
    }
    pairs
}

/// Отпущенный пачкой итог несёт свою границу: цель успевает сходить ещё раз до
/// доставки события, но оно показывает удержанный ответ, а не новый.
#[sqlx::test(migrations = "./migrations")]
async fn released_batch_event_keeps_the_held_answer(pool: PgPool) {
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
    add_answer_run(
        &f.pool,
        f.machine,
        f.child,
        execution,
        now + TimeDelta::seconds(1),
        1,
        "held answer",
    )
    .await;
    f.refresh_id(f.child, now + TimeDelta::seconds(2)).await;
    assert_eq!(
        alarm_state(&f.pool, alarm_id(&armed)).await.0,
        "armed",
        "only one target settled: the batch holds its answer"
    );

    let cancelled = f
        .call("alarms.cancel", json!({"alarm_id": alarm_id(&armed)}))
        .await
        .unwrap();
    assert_eq!(cancelled["result"], "cancelled");
    assert_eq!(f.event_jobs().await, 1, "the held event is released");

    // Цель успела сходить ещё раз до доставки отпущенного события.
    add_answer_run(
        &f.pool,
        f.machine,
        f.child,
        execution,
        now + TimeDelta::seconds(15),
        2,
        "newer answer",
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
    let (text, _): (String, Option<Value>) = sqlx::query_as(
        "select content->0->>'text',origin from inputs where conversation_id=$1
         order by input_seq desc limit 1",
    )
    .bind(f.orchestrator)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(text.contains("held answer"), "{text}");
    assert!(
        !text.contains("newer answer"),
        "the released event keeps the held boundary: {text}"
    );
}

/// Цель уже устоялась при взводе пачки: её отпечаток не изменится, поэтому она
/// засчитывается сразу, и пачка ждёт только остальных, а не срока.
#[sqlx::test(migrations = "./migrations")]
async fn already_settled_target_completes_a_batch_without_the_deadline(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
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
    assert_eq!(alarm_state(&f.pool, id).await.0, "armed");
    let collected = alarm_state(&f.pool, id).await.2;
    assert_eq!(
        collected["sessions"][f.child.to_string()]["state"],
        "failed",
        "a settled target is recorded at arm time"
    );

    f.refresh_id(f.child2, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child2, now + TimeDelta::seconds(1)).await;
    assert_eq!(
        alarm_state(&f.pool, id).await.0,
        "fired",
        "the batch does not wait for the deadline"
    );
    assert_eq!(
        f.event_jobs().await,
        0,
        "the already-settled failure belongs to the subscription"
    );
    assert_eq!(
        scheduler(&f.pool, Default::default())
            .tick(now + TimeDelta::seconds(10))
            .await
            .unwrap()
            .fired,
        1,
        "one batch input"
    );
    let inputs = f.alarm_inputs().await;
    let batch = inputs
        .iter()
        .find(|(text, _)| text.contains("settled (all, batched)"))
        .expect("the batch input");
    assert!(batch.0.contains("reported separately"), "{}", batch.0);
    assert!(batch.0.contains(&f.child2.to_string()), "{}", batch.0);
}

/// Двадцать сторожевых пачек по две устоявшиеся цели с ответами по 8 КиБ: к сроку
/// накопленные итоги не влезают в один ввод, а задание срока — одно из срабатывающих.
async fn arm_timeout_batches(f: &Fx, now: DateTime<Utc>) -> Vec<Uuid> {
    let still = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
    let mut alarms = Vec::new();
    for n in 0..20 {
        let one = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
        let two = testkit::seed_conversation(&f.pool, f.owner, f.machine).await;
        let armed = f
            .call(
                "alarms.set",
                json!({
                    "when": {
                        "kind": "session_state",
                        "session_ids": [one, two, still],
                        "all": true
                    },
                    "batch": true,
                    "only_on_timeout": true,
                    "deadline_seconds": 120,
                    "key": format!("timeout-{n}")
                }),
            )
            .await
            .unwrap();
        alarms.push(alarm_id(&armed));
        for (target, seq) in [(one, 1_i64), (two, 2_i64)] {
            let execution = Uuid::now_v7();
            sqlx::query(
                "insert into executions(id,conversation_id,generation,state,outcome)
                 values($1,$2,1,'stopped','done')",
            )
            .bind(execution)
            .bind(target)
            .execute(&f.pool)
            .await
            .unwrap();
            sqlx::query(
                "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,parent_block_id,created_seq,created_at)
                 values($1,$2,$3,'text','final',1,$4,null,$5,$6)",
            )
            .bind(Uuid::now_v7())
            .bind(execution)
            .bind(target)
            .bind("y".repeat(8192))
            .bind(seq)
            .bind(Utc::now())
            .execute(&f.pool)
            .await
            .unwrap();
            f.refresh_id(target, now + TimeDelta::seconds(2)).await;
        }
    }
    alarms
}

/// Две части сторожевой доставки: все задания срока срабатывают вместе, и текущее
/// задание доставки не отменяет само себя. Остаток приходит своим заданием.
#[sqlx::test(migrations = "./migrations")]
async fn timeout_delivery_delivers_every_part(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let alarms = arm_timeout_batches(&f, now).await;
    assert_eq!(alarms.len(), 20);

    let sch = scheduler(&f.pool, Default::default());
    let mut at = now + TimeDelta::seconds(130);
    for _ in 0..10 {
        sch.tick(at).await.unwrap();
        let delivered = count(
            &f.pool,
            "select count(*) from alarms where delivered_input_id is not null",
            f.orchestrator,
        )
        .await;
        if delivered == 20 {
            break;
        }
        at += TimeDelta::seconds(2);
    }
    let delivered = count(
        &f.pool,
        "select count(*) from alarms where delivered_input_id is not null",
        f.orchestrator,
    )
    .await;
    assert_eq!(delivered, 20, "every timeout alarm must reach the owner");

    let inputs = f.alarm_inputs().await;
    let total: usize = inputs.iter().map(|(text, _)| text.len()).sum();
    assert!(
        total > canterlot_proto::limits::MAX_INPUT_TEXT_BYTES,
        "the held summaries do not fit one input"
    );
    assert!(inputs.len() >= 2, "the remainder arrives as its own part");
    assert!(
        inputs
            .iter()
            .all(|(text, _)| text.ends_with("This is canterlot automation, not the owner.")),
        "each part is self-contained"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn twenty_large_batches_split_into_bounded_inputs(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    arm_large_batches(&f, now).await;
    assert_eq!(
        count(
            &f.pool,
            "select count(*) from alarms where state='fired'",
            f.orchestrator
        )
        .await,
        20
    );

    // Каждый запуск задания доставляет одну часть; остаток — следующим запуском.
    let sch = scheduler(&f.pool, Default::default());
    let mut at = now + TimeDelta::seconds(20);
    for _ in 0..30 {
        let report = sch.tick(at).await.unwrap();
        assert!(
            report.fired + report.retried + report.cancelled > 0,
            "the tick makes progress"
        );
        let delivered = count(
            &f.pool,
            "select count(*) from alarms where delivered_input_id is not null",
            f.orchestrator,
        )
        .await;
        if delivered == 20 {
            break;
        }
        at += TimeDelta::seconds(2);
    }
    let inputs = f.alarm_inputs().await;
    let total: usize = inputs.iter().map(|(text, _)| text.len()).sum();
    assert!(
        total > canterlot_proto::limits::MAX_INPUT_TEXT_BYTES,
        "the rendered summaries do not fit one input"
    );
    assert!(inputs.len() >= 2, "twenty large batches need several parts");
    assert!(
        inputs
            .iter()
            .all(|(text, _)| text.len() <= canterlot_proto::limits::MAX_INPUT_TEXT_BYTES),
        "every input stays under the wire limit"
    );
    assert!(
        inputs
            .iter()
            .all(|(text, _)| text.ends_with("This is canterlot automation, not the owner.")),
        "each part is self-contained"
    );
    assert_eq!(
        count(
            &f.pool,
            "select count(*) from alarms where delivered_input_id is not null",
            f.orchestrator
        )
        .await,
        20,
        "every alarm is marked delivered once"
    );
    let keys: Vec<String> = sqlx::query_scalar(
        "select idempotency_key from inputs where conversation_id=$1 order by idempotency_key",
    )
    .bind(f.orchestrator)
    .fetch_all(&f.pool)
    .await
    .unwrap();
    assert_eq!(keys.len(), inputs.len());
    let unique: std::collections::BTreeSet<_> = keys.iter().collect();
    assert_eq!(unique.len(), keys.len(), "each part has its own key");
}

/// Одно свободное место в очереди и две части: первая доставляется и коммитится,
/// вторая ждёт освобождения места, повторы не дублируют ввод.
#[sqlx::test(migrations = "./migrations")]
async fn one_free_queue_slot_delivers_one_part_and_the_rest_later(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    arm_large_batches(&f, now).await;
    f.fill_queue_n(99).await;

    let sch = scheduler(&f.pool, Default::default());
    let at = now + TimeDelta::seconds(20);
    let report = sch.tick(at).await.unwrap();
    let first = f.alarm_kind_inputs().await;
    assert_eq!(first.len(), 1, "the last queue slot takes exactly one part");
    assert!(
        first[0].0.len() > 100_000,
        "the part carries held summaries, not empty shells: {} bytes",
        first[0].0.len()
    );
    let first_delivered = count(
        &f.pool,
        "select count(*) from alarms where delivered_input_id is not null",
        f.orchestrator,
    )
    .await;
    assert!(
        (1..20).contains(&first_delivered),
        "only the first part is marked delivered: {first_delivered}"
    );
    assert!(report.retried >= 1, "the rest waits for a free slot");

    // Очередь всё ещё полна: повтор ничего не добавляет и не откатывает.
    let report = sch.tick(at + TimeDelta::seconds(120)).await.unwrap();
    assert_eq!(f.alarm_kind_inputs().await.len(), 1, "no second part yet");
    assert!(report.retried >= 1);
    assert_eq!(
        count(
            &f.pool,
            "select count(*) from alarms where delivered_input_id is not null",
            f.orchestrator,
        )
        .await,
        first_delivered,
        "the committed part stays committed"
    );

    // Место освободилось: остаток доставляется одним вводом.
    sqlx::query(
        "update inputs set state='cancelled' where id=(
           select id from inputs where conversation_id=$1 and state='queued'
           order by input_seq limit 1)",
    )
    .bind(f.orchestrator)
    .execute(&f.pool)
    .await
    .unwrap();
    sch.tick(at + TimeDelta::seconds(240)).await.unwrap();
    assert_eq!(
        f.alarm_kind_inputs().await.len(),
        2,
        "the rest arrives once"
    );
    assert_eq!(
        count(
            &f.pool,
            "select count(*) from alarms where delivered_input_id is not null",
            f.orchestrator,
        )
        .await,
        20,
        "every alarm is delivered"
    );
    sch.tick(at + TimeDelta::seconds(360)).await.unwrap();
    assert_eq!(
        f.alarm_kind_inputs().await.len(),
        2,
        "no duplicate on retry"
    );
    let keys: Vec<String> = sqlx::query_scalar(
        "select idempotency_key from inputs where conversation_id=$1
           and origin->>'kind'='alarm' order by idempotency_key",
    )
    .bind(f.orchestrator)
    .fetch_all(&f.pool)
    .await
    .unwrap();
    let unique: std::collections::BTreeSet<_> = keys.iter().collect();
    assert_eq!(unique.len(), keys.len(), "retries keep their keys");
}

/// Ждём, пока чужая транзакция встанет на блокировке: этого достаточно, чтобы
/// воспроизвести чередование двух порядков без гонки.
async fn wait_until_someone_waits(pool: &PgPool) {
    for _ in 0..300 {
        let waiting: i64 = sqlx::query_scalar(
            "select count(*) from pg_stat_activity
             where datname=current_database() and wait_event_type='Lock'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the other transaction never blocked");
}

/// Доставка будильника держит строку подписки и идёт в очередь получателя, а приём
/// результата держит очередь и закрывает подписку. Очередь берётся первой — цикла нет.
#[sqlx::test(migrations = "./migrations")]
async fn delivery_takes_the_recipient_queue_before_the_subscription(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    f.call(
        "alarms.set",
        json!({
            "when": {"kind": "session_state", "session_ids": [f.child], "all": true},
            "batch": true,
            "key": "wave"
        }),
    )
    .await
    .unwrap();
    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    let (id, owner_id, conversation_id, run_at, payload, attempts): (
        Uuid,
        Uuid,
        Uuid,
        DateTime<Utc>,
        Value,
        i32,
    ) = sqlx::query_as(
        "select id,owner_id,conversation_id,run_at,payload,attempts from scheduled_jobs
         where kind='session_task' and payload->>'action'='alarm_deliver' and conversation_id=$1",
    )
    .bind(f.orchestrator)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let job = canterlot_hub::scheduler::ClaimedJob {
        id,
        kind: canterlot_hub::scheduler::JobKind::SessionTask,
        owner_id,
        conversation_id: Some(conversation_id),
        run_at,
        payload,
        attempts,
    };

    // Приём результата: очередь уже у него.
    let mut accept = f.pool.begin().await.unwrap();
    sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
        .bind(f.orchestrator)
        .execute(&mut *accept)
        .await
        .unwrap();
    let pool = f.pool.clone();
    let delivery = tokio::spawn(async move {
        let mut tx = pool.begin().await.unwrap();
        let outcome = canterlot_hub::session_tools::alarms::deliver_job_in_tx(
            &mut tx,
            &job,
            Utc::now(),
            Uuid::now_v7(),
        )
        .await
        .expect("delivery must not hit a deadlock");
        tx.commit().await.unwrap();
        outcome
    });
    wait_until_someone_waits(&f.pool).await;

    // Второй шаг приёма в той же транзакции, что держит очередь: закрыть подписку.
    subs::close_for_accept_in_tx(&mut accept, f.orchestrator, None, "accepted")
        .await
        .expect("accept must not deadlock with delivery");
    accept.commit().await.unwrap();
    delivery.await.unwrap();
}

/// Замена будильника правит старый будильник и проверяет строки целей, а удаление цели
/// держит разговор и удаляет будильник. Строки разговоров берутся первыми — цикла нет.
#[sqlx::test(migrations = "./migrations")]
async fn replacement_takes_target_conversations_before_the_old_alarm(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let first = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child]},
                "deadline_seconds": 600,
                "key": "watch"
            }),
        )
        .await
        .unwrap();
    assert_eq!(first["state"], "armed");

    // Удаление цели: очередь, затем строка разговора, затем будильники.
    let mut deletion = f.pool.begin().await.unwrap();
    sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
        .bind(f.child)
        .execute(&mut *deletion)
        .await
        .unwrap();
    sqlx::query("select machine_id from conversations where id=$1 for update")
        .bind(f.child)
        .execute(&mut *deletion)
        .await
        .unwrap();

    let context = f.context.clone();
    let caller = f.caller();
    let child = f.child;
    let replacement = tokio::spawn(async move {
        session_tools::call(
            &context,
            &caller,
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [child]},
                "deadline_seconds": 600,
                "key": "watch"
            }),
        )
        .await
    });
    wait_until_someone_waits(&f.pool).await;

    canterlot_hub::session_tools::alarms::on_target_deleted_in_tx(&mut deletion, f.child, now)
        .await
        .expect("deletion must not deadlock with replacement");
    deletion.commit().await.unwrap();
    let replaced = replacement.await.unwrap().unwrap();
    assert_eq!(replaced["state"], "armed");
}

/// Запертый будильник пропускает оценку, а подписка успевает закрыться обычным `idle`.
/// После освобождения пачка признаёт этот итог отдельным и срабатывает сразу, не ждав срока.
#[sqlx::test(migrations = "./migrations")]
async fn locked_alarm_counts_an_idle_subscription_closed_separately(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let armed = f
        .call(
            "alarms.set",
            json!({
                "when": {"kind": "session_state", "session_ids": [f.child], "all": true},
                "batch": true,
                "deadline_seconds": 600,
                "key": "wave"
            }),
        )
        .await
        .unwrap();
    let id = alarm_id(&armed);

    let mut blocker = f.pool.begin().await.unwrap();
    sqlx::query("select id from alarms where id=$1 for update")
        .bind(id)
        .execute(&mut *blocker)
        .await
        .unwrap();
    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    assert_eq!(alarm_state(&f.pool, id).await.0, "armed");
    let reason: Option<String> = sqlx::query_scalar(
        "select closed_reason from session_subscriptions
         where subscriber_id=$1 and target_id=$2",
    )
    .bind(f.orchestrator)
    .bind(f.child)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        reason.as_deref(),
        Some("idle"),
        "the subscription closed itself"
    );
    assert_eq!(f.event_jobs().await, 1, "the ordinary event is queued");

    blocker.commit().await.unwrap();
    f.refresh_id(f.child, now + TimeDelta::seconds(10)).await;
    assert_eq!(
        alarm_state(&f.pool, id).await.0,
        "fired",
        "the batch adopts the separately closed result without the deadline"
    );
    let collected = alarm_state(&f.pool, id).await.2;
    assert_eq!(
        collected["sessions"][f.child.to_string()]["ownership"],
        "separate"
    );
    assert_eq!(f.event_jobs().await, 1, "no second ordinary event");

    let report = scheduler(&f.pool, Default::default())
        .tick(now + TimeDelta::seconds(20))
        .await
        .unwrap();
    assert!(report.fired >= 1, "the batch delivery runs");
    let inputs = f.alarm_kind_inputs().await;
    assert_eq!(inputs.len(), 1);
    assert!(
        inputs[0].0.contains("reported separately"),
        "the batch says the result came separately: {}",
        inputs[0].0
    );
}

/// Строку подписки держит чужая транзакция: `idle` нельзя ни удержать, ни отдать
/// обычным событием. Пачка ждёт, а на следующем обходе забирает итог себе.
#[sqlx::test(migrations = "./migrations")]
async fn locked_subscription_keeps_batch_ownership_pending_until_the_next_sweep(pool: PgPool) {
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
    let id = alarm_id(&armed);

    // Чужая транзакция держит строку подписки, когда цель успокаивается.
    let mut blocker = f.pool.begin().await.unwrap();
    sqlx::query(
        "select id from session_subscriptions where subscriber_id=$1 and target_id=$2
         for update",
    )
    .bind(f.orchestrator)
    .bind(f.child)
    .execute(&mut *blocker)
    .await
    .unwrap();
    f.refresh_id(f.child, now - TimeDelta::hours(1)).await;
    f.refresh_id(f.child, now + TimeDelta::seconds(1)).await;
    assert_eq!(
        alarm_state(&f.pool, id).await.0,
        "armed",
        "an unresolved result does not satisfy the batch"
    );
    assert_eq!(f.event_jobs().await, 0, "the subscription was skipped");
    let collected = alarm_state(&f.pool, id).await.2;
    assert_eq!(
        collected["sessions"][f.child.to_string()]["ownership"],
        "pending"
    );
    assert!(f.alarm_inputs().await.is_empty());

    blocker.commit().await.unwrap();
    f.refresh_id(f.child, now + TimeDelta::seconds(10)).await;
    assert_eq!(
        alarm_state(&f.pool, id).await.0,
        "fired",
        "the next sweep gives the batch the summary"
    );
    assert_eq!(f.event_jobs().await, 0, "the batch owns the result");
    assert_eq!(
        scheduler(&f.pool, Default::default())
            .tick(now + TimeDelta::seconds(20))
            .await
            .unwrap()
            .fired,
        1
    );
    let inputs = f.alarm_inputs().await;
    assert_eq!(inputs.len(), 1, "delivered once");
    assert!(
        inputs[0].0.contains(&f.child.to_string()),
        "{}",
        inputs[0].0
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn retention_deletes_old_terminal_alarms(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let old = now - TimeDelta::days(31);
    let mut ids = Vec::new();
    for key in [
        "fired",
        "cancelled",
        "expired",
        "failed",
        "armed",
        "undelivered",
        "recent",
    ] {
        ids.push(alarm_id(&f.arm_at(key, now + TimeDelta::seconds(60)).await));
    }
    let [
        fired,
        cancelled,
        expired,
        failed,
        armed,
        undelivered,
        recent,
    ] = <[Uuid; 7]>::try_from(ids).unwrap();
    sqlx::query(
        "update alarms set state='fired',fired_reason='time',fired_at=$2,
           delivered_input_id=$3,delivered_at=$2,updated_at=$2 where id=$1",
    )
    .bind(fired)
    .bind(old)
    .bind(Uuid::now_v7())
    .execute(&f.pool)
    .await
    .unwrap();
    for (id, state) in [
        (cancelled, "cancelled"),
        (expired, "expired"),
        (failed, "failed"),
    ] {
        sqlx::query("update alarms set state=$2,updated_at=$3 where id=$1")
            .bind(id)
            .bind(state)
            .bind(old)
            .execute(&f.pool)
            .await
            .unwrap();
    }
    sqlx::query("update alarms set updated_at=$2 where id=any($1)")
        .bind([armed, undelivered])
        .bind(old)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query(
        "update alarms set state='fired',fired_reason='time',fired_at=$2,updated_at=$2 where id=$1",
    )
    .bind(undelivered)
    .bind(old)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query("update alarms set state='cancelled',updated_at=$2 where id=$1")
        .bind(recent)
        .bind(now)
        .execute(&f.pool)
        .await
        .unwrap();

    scheduler(&f.pool, Default::default())
        .maintenance()
        .await
        .unwrap();

    for id in [fired, cancelled, expired, failed] {
        let found: Option<String> = sqlx::query_scalar("select state from alarms where id=$1")
            .bind(id)
            .fetch_optional(&f.pool)
            .await
            .unwrap();
        assert_eq!(found, None, "old terminal alarm {id} is gone");
    }
    for id in [armed, undelivered, recent] {
        let found: Option<String> = sqlx::query_scalar("select state from alarms where id=$1")
            .bind(id)
            .fetch_optional(&f.pool)
            .await
            .unwrap();
        assert!(found.is_some(), "alarm {id} is kept");
    }
}

fn metrics_with(ms: u64, free_gb: f64, load_per_core: f64, mem_gb: f64) -> MachineMetrics {
    let cores = 4_u32;
    MachineMetrics {
        // Реалистичная отметка: свежесть условия считается по времени выборки.
        machine_time_ms: Utc::now().timestamp_millis() as u64 + ms,
        cpu_cores: cores,
        cpu_permille: 50,
        mem_total_bytes: (mem_gb * 1e9) as u64 + 1,
        mem_available_bytes: (mem_gb * 1e9) as u64,
        disks: vec![DiskUsage {
            label: "data".into(),
            total_bytes: (free_gb * 1e9) as u64 + 1,
            free_bytes: (free_gb * 1e9) as u64,
        }],
        load1_centi: (load_per_core * f64::from(cores) * 100.0).round() as u32,
        ..Default::default()
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn limit_reset_fires_on_a_fresh_exact_reading(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::hours(1);
    f.observe(
        "claude",
        Window::Weekly,
        600,
        Some(reset),
        now,
        DataConfidence::Exact,
    )
    .await;
    let armed = f
        .arm_limit("window", "claude", "weekly", None)
        .await
        .unwrap();
    let id = alarm_id(&armed);
    let after = now + TimeDelta::minutes(10);
    f.observe(
        "claude",
        Window::Weekly,
        100,
        Some(reset + TimeDelta::hours(1)),
        after,
        DataConfidence::Exact,
    )
    .await;
    let (state, reason, _) = alarm_state(&f.pool, id).await;
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("condition"));
    let (trigger, verified): (Option<String>, Option<bool>) = sqlx::query_as(
        "select condition->>'trigger',(condition->>'verified')::boolean from alarms where id=$1",
    )
    .bind(id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(trigger.as_deref(), Some("reset"));
    assert_eq!(verified, Some(true));
    scheduler(&f.pool, Default::default())
        .tick(after + TimeDelta::seconds(6))
        .await
        .unwrap();
    let inputs = f.alarm_kind_inputs().await;
    assert_eq!(inputs.len(), 1);
    let text = &inputs[0].0;
    assert!(text.contains("weekly window reset"), "{text}");
    assert!(text.contains("10% used"), "{text}");
}

#[sqlx::test(migrations = "./migrations")]
async fn limit_reset_reading_without_reset_does_not_fire(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::hours(1);
    f.observe(
        "claude",
        Window::Weekly,
        600,
        Some(reset),
        now,
        DataConfidence::Exact,
    )
    .await;
    let armed = f
        .arm_limit("window", "claude", "weekly", None)
        .await
        .unwrap();
    let id = alarm_id(&armed);
    f.observe(
        "claude",
        Window::Weekly,
        700,
        Some(reset),
        now + TimeDelta::minutes(10),
        DataConfidence::Exact,
    )
    .await;
    assert_eq!(alarm_state(&f.pool, id).await.0, "armed");
    f.observe(
        "claude",
        Window::Weekly,
        500,
        Some(reset),
        now + TimeDelta::hours(2),
        DataConfidence::Exact,
    )
    .await;
    assert_eq!(alarm_state(&f.pool, id).await.0, "armed");
}

#[sqlx::test(migrations = "./migrations")]
async fn limit_reset_estimated_reading_needs_a_second_one(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::hours(1);
    f.observe(
        "claude",
        Window::Weekly,
        600,
        Some(reset),
        now,
        DataConfidence::Exact,
    )
    .await;
    let armed = f
        .arm_limit("window", "claude", "weekly", None)
        .await
        .unwrap();
    let id = alarm_id(&armed);
    f.observe(
        "claude",
        Window::Weekly,
        100,
        Some(reset + TimeDelta::hours(1)),
        now + TimeDelta::hours(2),
        DataConfidence::Estimated,
    )
    .await;
    assert_eq!(
        alarm_state(&f.pool, id).await.0,
        "armed",
        "an estimated reading alone does not prove a reset"
    );
    f.observe(
        "claude",
        Window::Weekly,
        90,
        Some(reset + TimeDelta::hours(2)),
        now + TimeDelta::hours(2) + TimeDelta::minutes(11),
        DataConfidence::Estimated,
    )
    .await;
    let (state, reason, _) = alarm_state(&f.pool, id).await;
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("condition"));
}

/// Оценочный отсчёт с уже сдвинутым `resets_at`, снятый до запомненного срока,
/// не доказывает сброс сам по себе: нужен подтверждающий отсчёт после срока.
#[sqlx::test(migrations = "./migrations")]
async fn limit_reset_estimated_early_advanced_reset_does_not_fire(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::hours(1);
    f.observe(
        "claude",
        Window::Weekly,
        600,
        Some(reset),
        now,
        DataConfidence::Exact,
    )
    .await;
    let armed = f
        .arm_limit("window", "claude", "weekly", None)
        .await
        .unwrap();
    let id = alarm_id(&armed);
    f.observe(
        "claude",
        Window::Weekly,
        100,
        Some(reset + TimeDelta::hours(1)),
        now + TimeDelta::minutes(10),
        DataConfidence::Estimated,
    )
    .await;
    assert_eq!(
        alarm_state(&f.pool, id).await.0,
        "armed",
        "an early estimated reading cannot confirm the captured reset"
    );
}

/// Предшествующий `NotReset` после срока не засчитывается как подтверждение:
/// оценочному сбросу нужны два именно подтверждающих отсчёта после срока.
#[sqlx::test(migrations = "./migrations")]
async fn limit_reset_estimated_confirmation_counts_only_reset_readings(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::hours(1);
    f.observe(
        "claude",
        Window::Weekly,
        600,
        Some(reset),
        now,
        DataConfidence::Exact,
    )
    .await;
    let armed = f
        .arm_limit("window", "claude", "weekly", None)
        .await
        .unwrap();
    let id = alarm_id(&armed);
    f.observe(
        "claude",
        Window::Weekly,
        700,
        Some(reset),
        reset + TimeDelta::minutes(1),
        DataConfidence::Exact,
    )
    .await;
    f.observe(
        "claude",
        Window::Weekly,
        100,
        Some(reset + TimeDelta::hours(1)),
        reset + TimeDelta::minutes(2),
        DataConfidence::Estimated,
    )
    .await;
    assert_eq!(
        alarm_state(&f.pool, id).await.0,
        "armed",
        "a preceding NotReset reading is not a reset confirmation"
    );
    f.observe(
        "claude",
        Window::Weekly,
        90,
        Some(reset + TimeDelta::hours(2)),
        reset + TimeDelta::minutes(3),
        DataConfidence::Estimated,
    )
    .await;
    let (state, reason, _) = alarm_state(&f.pool, id).await;
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("condition"));
}

#[sqlx::test(migrations = "./migrations")]
async fn limit_reset_missing_window_fires_the_condition(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::hours(1);
    f.observe(
        "claude",
        Window::FiveHour,
        400,
        Some(reset),
        now,
        DataConfidence::Exact,
    )
    .await;
    let armed = f
        .arm_limit("window", "claude", "five_hour", None)
        .await
        .unwrap();
    let id = alarm_id(&armed);
    let mut tx = f.pool.begin().await.unwrap();
    limits::record_absence_in_tx(
        &mut tx,
        f.owner,
        "claude",
        Window::FiveHour,
        reset + TimeDelta::minutes(5),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (state, reason, _) = alarm_state(&f.pool, id).await;
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("condition"));
}

#[sqlx::test(migrations = "./migrations")]
async fn limit_reset_without_resets_at_needs_below_percent(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    f.observe(
        "claude",
        Window::Weekly,
        600,
        None,
        now,
        DataConfidence::Exact,
    )
    .await;
    let error = f
        .arm_limit("window", "claude", "weekly", None)
        .await
        .unwrap_err();
    assert_eq!(error.code, session_tools::ToolErrorCode::InvalidArguments);
    assert!(error.message.contains("resets_at"), "{}", error.message);
    let armed = f
        .arm_limit("window", "claude", "weekly", Some(50.0))
        .await
        .unwrap();
    assert_eq!(armed["state"], "armed");
}

#[sqlx::test(migrations = "./migrations")]
async fn limit_reset_below_percent_fires_on_a_fresh_reading(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::hours(1);
    f.observe(
        "claude",
        Window::Weekly,
        600,
        Some(reset),
        now,
        DataConfidence::Exact,
    )
    .await;
    let armed = f
        .arm_limit("window", "claude", "weekly", Some(30.0))
        .await
        .unwrap();
    let id = alarm_id(&armed);
    let after = now + TimeDelta::minutes(10);
    f.observe(
        "claude",
        Window::Weekly,
        200,
        None,
        after,
        DataConfidence::Exact,
    )
    .await;
    let (state, reason, _) = alarm_state(&f.pool, id).await;
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("condition"));
    let trigger: Option<String> =
        sqlx::query_scalar("select condition->>'trigger' from alarms where id=$1")
            .bind(id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(trigger.as_deref(), Some("below_percent"));
    scheduler(&f.pool, Default::default())
        .tick(after + TimeDelta::seconds(6))
        .await
        .unwrap();
    let inputs = f.alarm_kind_inputs().await;
    let text = &inputs[0].0;
    assert!(text.contains("20%"), "{text}");
    assert!(text.contains("below 30%"), "{text}");
}

#[sqlx::test(migrations = "./migrations")]
async fn limit_reset_probe_is_scheduled_at_resets_at_plus_grace(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::hours(1);
    f.observe(
        "claude",
        Window::Weekly,
        600,
        Some(reset),
        now,
        DataConfidence::Exact,
    )
    .await;
    f.arm_limit("window", "claude", "weekly", None)
        .await
        .unwrap();
    let (run_at, payload): (DateTime<Utc>, Value) = sqlx::query_as(
        "select run_at,payload from scheduled_jobs
         where conversation_id=$1 and payload->>'action'='alarm_probe'",
    )
    .bind(f.orchestrator)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        run_at.timestamp_micros(),
        (reset + TimeDelta::seconds(f.context.limits.reset_grace.as_secs() as i64))
            .timestamp_micros()
    );
    assert_eq!(payload["provider"], "claude");
    scheduler(&f.pool, Default::default())
        .tick(run_at)
        .await
        .unwrap();
    let attempts: i32 = sqlx::query_scalar(
        "select attempts from scheduled_jobs
         where conversation_id=$1 and payload->>'action'='alarm_probe'",
    )
    .bind(f.orchestrator)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(
        attempts >= 1,
        "the probe asks for a provider poll: {attempts}"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn limit_reset_fires_by_clock_after_thirty_minutes_without_a_reading(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let reset = now + TimeDelta::hours(1);
    f.observe(
        "claude",
        Window::Weekly,
        600,
        Some(reset),
        now,
        DataConfidence::Exact,
    )
    .await;
    let armed = f
        .arm_limit("window", "claude", "weekly", None)
        .await
        .unwrap();
    let id = alarm_id(&armed);
    let probe_at = reset + TimeDelta::seconds(f.context.limits.reset_grace.as_secs() as i64);
    let sch = scheduler(&f.pool, Default::default());
    sch.tick(probe_at).await.unwrap();
    let by_clock = probe_at + TimeDelta::minutes(30) + TimeDelta::seconds(1);
    sch.tick(by_clock).await.unwrap();
    let (state, reason, _) = alarm_state(&f.pool, id).await;
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("by_clock"));
    let (trigger, verified): (Option<String>, Option<bool>) = sqlx::query_as(
        "select condition->>'trigger',(condition->>'verified')::boolean from alarms where id=$1",
    )
    .bind(id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(trigger.as_deref(), Some("by_clock"));
    assert_eq!(verified, Some(false));
    sch.tick(by_clock + TimeDelta::seconds(6)).await.unwrap();
    let inputs = f.alarm_kind_inputs().await;
    let text = &inputs[0].0;
    assert!(text.contains("by clock"), "{text}");
    assert!(text.contains("no fresh reading"), "{text}");
}

#[sqlx::test(migrations = "./migrations")]
async fn machine_condition_fires_when_every_threshold_holds(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.arm_machine("freed", Some(40.0), Some(2.0), Some(10.0))
        .await
        .unwrap();
    let now = Utc::now();
    assert_eq!(
        machine_info::apply_metrics(
            &f.pool,
            f.owner,
            f.machine,
            &metrics_with(1, 50.0, 0.5, 20.0),
            Uuid::now_v7(),
        )
        .await
        .unwrap(),
        machine_info::MetricsOutcome::Stored
    );
    let (state, reason, _) = sqlx::query_as::<_, (String, Option<String>, Value)>(
        "select state,fired_reason,collected from alarms where conversation_id=$1 and condition->>'kind'='machine'",
    )
    .bind(f.orchestrator)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(state, "fired");
    assert_eq!(reason.as_deref(), Some("condition"));
    scheduler(&f.pool, Default::default())
        .tick(now + TimeDelta::seconds(6))
        .await
        .unwrap();
    let inputs = f.alarm_kind_inputs().await;
    let text = &inputs[0].0;
    assert!(text.contains("50 GB free"), "{text}");
    assert!(text.contains("load 0.5 per core"), "{text}");
    assert!(text.contains("20 GB memory available"), "{text}");
}

#[sqlx::test(migrations = "./migrations")]
async fn machine_condition_waits_for_an_unmet_threshold(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.arm_machine("freed", Some(40.0), Some(2.0), Some(10.0))
        .await
        .unwrap();
    machine_info::apply_metrics(
        &f.pool,
        f.owner,
        f.machine,
        &metrics_with(1, 50.0, 3.0, 20.0),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let state: String = sqlx::query_scalar(
        "select state from alarms where conversation_id=$1 and condition->>'kind'='machine'",
    )
    .bind(f.orchestrator)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(state, "armed");
}

#[sqlx::test(migrations = "./migrations")]
async fn machine_condition_ignores_stale_metrics(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.arm_machine("freed", Some(40.0), Some(2.0), Some(10.0))
        .await
        .unwrap();
    machine_info::apply_metrics(
        &f.pool,
        f.owner,
        f.machine,
        &metrics_with(2000, 1.0, 9.0, 1.0),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(matches!(
        machine_info::apply_metrics(
            &f.pool,
            f.owner,
            f.machine,
            &metrics_with(1000, 50.0, 0.5, 20.0),
            Uuid::now_v7(),
        )
        .await
        .unwrap(),
        machine_info::MetricsOutcome::Ignored("stale_sample")
    ));
    let state: String = sqlx::query_scalar(
        "select state from alarms where conversation_id=$1 and condition->>'kind'='machine'",
    )
    .bind(f.orchestrator)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(state, "armed");
    machine_info::apply_metrics(
        &f.pool,
        f.owner,
        f.machine,
        &metrics_with(3000, 50.0, 0.5, 20.0),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let state: String = sqlx::query_scalar(
        "select state from alarms where conversation_id=$1 and condition->>'kind'='machine'",
    )
    .bind(f.orchestrator)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(state, "fired");
}

/// Свежесть машинного условия считается по времени самой выборки, а не по
/// моменту приёма: задержанная выборка, снятая до взвода, условие не выполняет.
#[sqlx::test(migrations = "./migrations")]
async fn machine_condition_uses_the_sample_timestamp_not_arrival(pool: PgPool) {
    let f = Fx::new(pool).await;
    let armed_at = Utc::now();
    f.arm_machine("freed", Some(40.0), Some(2.0), Some(10.0))
        .await
        .unwrap();
    let mut delayed = metrics_with(1, 50.0, 0.5, 20.0);
    delayed.machine_time_ms = (armed_at - TimeDelta::minutes(2)).timestamp_millis() as u64;
    machine_info::apply_metrics(
        &f.pool,
        f.owner,
        f.machine,
        &delayed,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let state: String = sqlx::query_scalar(
        "select state from alarms where conversation_id=$1 and condition->>'kind'='machine'",
    )
    .bind(f.orchestrator)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(state, "armed", "a sample taken before arming cannot fire");

    let mut fresh = metrics_with(2, 50.0, 0.5, 20.0);
    fresh.machine_time_ms = Utc::now().timestamp_millis() as u64;
    machine_info::apply_metrics(&f.pool, f.owner, f.machine, &fresh, Uuid::now_v7())
        .await
        .unwrap();
    let state: String = sqlx::query_scalar(
        "select state from alarms where conversation_id=$1 and condition->>'kind'='machine'",
    )
    .bind(f.orchestrator)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(state, "fired", "a fresh sample fires");
}

#[sqlx::test(migrations = "./migrations")]
async fn machine_condition_only_sees_the_callers_machine(pool: PgPool) {
    let f = Fx::new(pool).await;
    let other = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'other')")
        .bind(other)
        .bind(f.owner)
        .execute(&f.pool)
        .await
        .unwrap();
    f.arm_machine("freed", Some(40.0), Some(2.0), Some(10.0))
        .await
        .unwrap();
    machine_info::apply_metrics(
        &f.pool,
        f.owner,
        other,
        &metrics_with(1, 50.0, 0.5, 20.0),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let state: String = sqlx::query_scalar(
        "select state from alarms where conversation_id=$1 and condition->>'kind'='machine'",
    )
    .bind(f.orchestrator)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(state, "armed");
    machine_info::apply_metrics(
        &f.pool,
        f.owner,
        f.machine,
        &metrics_with(2, 50.0, 0.5, 20.0),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let state: String = sqlx::query_scalar(
        "select state from alarms where conversation_id=$1 and condition->>'kind'='machine'",
    )
    .bind(f.orchestrator)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(state, "fired");
}

#[sqlx::test(migrations = "./migrations")]
async fn a_machine_alarm_and_a_time_alarm_still_coalesce_into_one_input(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    f.arm_at("time", now + TimeDelta::seconds(35)).await;
    f.arm_machine("freed", Some(40.0), Some(2.0), Some(10.0))
        .await
        .unwrap();
    machine_info::apply_metrics(
        &f.pool,
        f.owner,
        f.machine,
        &metrics_with(1, 50.0, 0.5, 20.0),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    scheduler(&f.pool, Default::default())
        .tick(now + TimeDelta::seconds(40))
        .await
        .unwrap();
    let inputs = f.alarm_kind_inputs().await;
    assert_eq!(inputs.len(), 1);
    let text = &inputs[0].0;
    assert!(text.contains("2 alarms fired"), "{text}");
    assert!(text.contains("\"time\""), "{text}");
    assert!(text.contains("\"freed\""), "{text}");
}
