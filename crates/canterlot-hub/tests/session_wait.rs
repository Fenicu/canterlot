use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use canterlot_hub::{
    activity::{self, Thresholds},
    config::SessionToolsConfig,
    conversations::{self, SubmitMessage},
    deletion,
    delivery::ContentBlock,
    session_tools::{
        self, Caller, ToolContext, ToolError, ToolErrorCode as Code, settle::SettleConfig, subs,
    },
    testkit,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Fx {
    pool: PgPool,
    context: ToolContext,
    owner: Uuid,
    machine: Uuid,
    waiter: Uuid,
    target: Uuid,
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
        let target = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
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
                dispatcher: None,
                shutdown: CancellationToken::new(),
            },
            pool,
            owner: seed.owner_id,
            machine: seed.machine_id,
            waiter: seed.conversation_id,
            target,
        }
    }

    fn caller(&self) -> Caller {
        Caller {
            owner_id: self.owner,
            conversation_id: self.waiter,
            execution_id: Uuid::now_v7(),
            machine_id: self.machine,
            call_id: Uuid::now_v7(),
        }
    }

    async fn wait(&self, args: Value) -> Result<Value, ToolError> {
        session_tools::call(&self.context, &self.caller(), "sessions.wait", args).await
    }

    async fn refresh(&self, at: DateTime<Utc>) {
        activity::refresh(&self.pool, self.target, at, &thresholds(), Uuid::now_v7())
            .await
            .unwrap();
    }

    /// Цель давно простаивает: окно тишины уже прошло.
    async fn settle_target(&self) {
        self.refresh(Utc::now() - TimeDelta::hours(1)).await;
    }

    /// Цель не осела: у неё есть ввод в очереди.
    async fn busy_target(&self) {
        conversations::submit_message(
            &self.pool,
            SubmitMessage {
                owner_id: self.owner,
                conversation_id: self.target,
                key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text {
                    text: "работа".into(),
                }],
                attachment_ids: vec![],
                retry_of: None,
                received_at: Utc::now(),
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        self.refresh(Utc::now()).await;
    }

    async fn subscribe(&self) {
        let mut tx = self.pool.begin().await.unwrap();
        subs::subscribe_in_tx(&mut tx, self.owner, self.waiter, self.target, 0)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn subscription(&self) -> Value {
        sqlx::query_scalar(
            "select to_jsonb(s) from session_subscriptions s
             where subscriber_id=$1 and target_id=$2",
        )
        .bind(self.waiter)
        .bind(self.target)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn event_jobs(&self) -> i64 {
        sqlx::query_scalar(
            "select count(*) from scheduled_jobs
             where kind='session_task' and conversation_id=$1 and payload->>'action'='event'",
        )
        .bind(self.waiter)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wait_returns_at_once_when_settled(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.settle_target().await;
    let started = Instant::now();
    let result = f
        .wait(json!({"session_id": f.target, "timeout_seconds": 30}))
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(result["state"], "idle");
    assert_eq!(result["timed_out"], false);
    assert_eq!(result["event_suppressed"], false);
    let summary = result["summary"].as_str().unwrap();
    assert!(summary.starts_with("[canterlot] Session"), "{summary}");
    assert!(summary.contains("is now idle."), "{summary}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wait_times_out_and_keeps_subscription(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.busy_target().await;
    f.subscribe().await;
    let started = Instant::now();
    let result = f
        .wait(json!({"session_id": f.target, "timeout_seconds": 1}))
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(result["timed_out"], true);
    assert_eq!(result["event_suppressed"], false);
    assert!(result["summary"].is_null());
    assert_eq!(
        result["hint"], "You stay subscribed; end your turn.",
        "{result}"
    );
    let subscription = f.subscription().await;
    assert!(subscription["closed_at"].is_null());
    assert!(subscription["last_fingerprint"].is_null());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wait_without_subscription_does_not_claim_one(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.busy_target().await;
    let result = f
        .wait(json!({"session_id": f.target, "timeout_seconds": 1}))
        .await
        .unwrap();
    assert_eq!(result["timed_out"], true);
    assert_ne!(result["hint"], "You stay subscribed; end your turn.");
    let rows: i64 = sqlx::query_scalar("select count(*) from session_subscriptions")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "wait never subscribes by itself");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wait_suppresses_event_when_it_wins(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.settle_target().await;
    f.subscribe().await;
    let result = f
        .wait(json!({"session_id": f.target, "timeout_seconds": 30}))
        .await
        .unwrap();
    assert_eq!(result["state"], "idle");
    assert_eq!(result["event_suppressed"], true);
    let subscription = f.subscription().await;
    assert!(!subscription["last_fingerprint"].is_null());
    assert_eq!(
        subscription["closed_reason"], "idle",
        "a terminal result closes the subscription like the evaluator"
    );

    f.refresh(Utc::now()).await;
    assert_eq!(
        f.event_jobs().await,
        0,
        "the evaluator finds nothing to say"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn event_still_arrives_when_evaluator_wins(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.settle_target().await;
    f.subscribe().await;
    f.refresh(Utc::now()).await;
    assert_eq!(f.event_jobs().await, 1);
    let result = f
        .wait(json!({"session_id": f.target, "timeout_seconds": 30}))
        .await
        .unwrap();
    assert_eq!(result["state"], "idle");
    assert_eq!(result["timed_out"], false);
    assert_eq!(result["event_suppressed"], false);
    assert_eq!(f.event_jobs().await, 1, "the queued event is untouched");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wait_picks_up_a_state_reached_while_waiting(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.busy_target().await;
    f.subscribe().await;
    let pool = f.pool.clone();
    let target = f.target;
    let finishing = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1300)).await;
        sqlx::query("update inputs set state='accepted' where conversation_id=$1")
            .bind(target)
            .execute(&pool)
            .await
            .unwrap();
        activity::refresh(
            &pool,
            target,
            Utc::now() - TimeDelta::hours(1),
            &thresholds(),
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    });
    let result = f
        .wait(json!({"session_id": f.target, "timeout_seconds": 20}))
        .await
        .unwrap();
    finishing.await.unwrap();
    assert_eq!(result["timed_out"], false, "{result}");
    assert_eq!(result["state"], "idle");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wait_holds_no_database_connection(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.busy_target().await;
    let single = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(1))
        .connect_with((*f.pool.connect_options()).clone())
        .await
        .unwrap();
    let context = ToolContext {
        pool: single.clone(),
        ..f.context.clone()
    };
    let caller = f.caller();
    let target = f.target;
    let waiting = tokio::spawn(async move {
        session_tools::call(
            &context,
            &caller,
            "sessions.wait",
            json!({"session_id": target, "timeout_seconds": 3}),
        )
        .await
    });
    // Между опросами соединение свободно: чужой запрос проходит в срок.
    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let started = Instant::now();
        let one: i32 = sqlx::query_scalar("select 1")
            .fetch_one(&single)
            .await
            .expect("the single connection is free between polls");
        assert_eq!(one, 1);
        assert!(started.elapsed() < Duration::from_millis(500));
    }
    let result = waiting.await.unwrap().unwrap();
    assert_eq!(result["timed_out"], true);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wait_stops_on_hub_shutdown(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.busy_target().await;
    let shutdown = f.context.shutdown.clone();
    let stopper = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        shutdown.cancel();
    });
    let started = Instant::now();
    let error = f
        .wait(json!({"session_id": f.target, "timeout_seconds": 60}))
        .await
        .unwrap_err();
    stopper.await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(error.code, Code::Internal);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wait_validates_arguments_and_visibility(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.settle_target().await;
    for bad in [0, 301, -1] {
        let error = f
            .wait(json!({"session_id": f.target, "timeout_seconds": bad}))
            .await
            .unwrap_err();
        assert_eq!(error.code, Code::InvalidArguments, "{bad}");
    }
    let error = f
        .wait(json!({"session_id": f.target, "extra": 1}))
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::InvalidArguments);
    let error = f.wait(json!({"session_id": f.waiter})).await.unwrap_err();
    assert_eq!(error.code, Code::SessionNotFound, "waiting for itself");
    let error = f
        .wait(json!({"session_id": Uuid::now_v7()}))
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::SessionNotFound);

    // Делегированная сессия видит только свою семью.
    sqlx::query("update conversations set spawn_depth=1 where id=$1")
        .bind(f.waiter)
        .execute(&f.pool)
        .await
        .unwrap();
    let error = f.wait(json!({"session_id": f.target})).await.unwrap_err();
    assert_eq!(error.code, Code::SessionNotFound);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wait_reports_a_target_deleted_meanwhile(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.busy_target().await;
    let directory = tempfile::tempdir().unwrap();
    let (_, blobs) = testkit::test_blob_store(directory.path());
    let (pool, owner, target) = (f.pool.clone(), f.owner, f.target);
    let deleting = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1300)).await;
        deletion::delete(
            &pool,
            blobs.as_ref(),
            owner,
            target,
            "wait-delete",
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    });
    let result = f
        .wait(json!({"session_id": f.target, "timeout_seconds": 20}))
        .await
        .unwrap();
    deleting.await.unwrap();
    assert_eq!(result["state"], "deleted", "{result}");
    assert_eq!(result["timed_out"], false);
    assert!(!result["summary"].as_str().unwrap().contains("private"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn event_for_an_open_state_is_not_suppressed_by_a_later_wait(pool: PgPool) {
    let f = Fx::new(pool).await;
    f.busy_target().await;
    sqlx::query("update queue_state set pause_reasons='[{\"reason\":\"user_stop\"}]' where conversation_id=$1")
        .bind(f.target)
        .execute(&f.pool)
        .await
        .unwrap();
    f.subscribe().await;
    f.refresh(Utc::now()).await;
    assert_eq!(f.event_jobs().await, 1, "waiting_for_user does not close");
    let result = f
        .wait(json!({"session_id": f.target, "timeout_seconds": 30}))
        .await
        .unwrap();
    assert_eq!(result["state"], "waiting_for_user");
    assert_eq!(result["event_suppressed"], false, "the evaluator won");
    assert!(f.subscription().await["closed_at"].is_null());

    // Второй опрос того же состояния событие не отменяет и не плодит.
    let again = f
        .wait(json!({"session_id": f.target, "timeout_seconds": 30}))
        .await
        .unwrap();
    assert_eq!(again["event_suppressed"], false);
    assert_eq!(f.event_jobs().await, 1);
}
