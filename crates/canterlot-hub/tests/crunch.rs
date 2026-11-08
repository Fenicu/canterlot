#![allow(clippy::too_many_arguments)]
#[path = "support/task3.rs"]
mod worker;

use std::{collections::HashMap, time::Duration};

use canterlot_hub::{
    activity,
    blocks::ExecutionRef,
    config::{HubTimers, LimitsConfig},
    conversations::{self, CrunchSetting, NewConversation, SettingsPatch, SubmitMessage},
    crunch::{self, JobOutcome, LimitReachedEffect, SpendOutcome},
    delivery::{
        ContentBlock, SubmitInput, control,
        dispatch::{BlockReason, DispatchOutcome, GateInputs, dispatch_next},
        receipts::{self, InputReceipt},
        submit_input,
    },
    dispatcher::Dispatcher,
    executions,
    ingest::Ingest,
    limits::{self, DataConfidence, Observation, ObservationSource, Recorded, Window},
    model::{HoldReason, PauseReason, RunSlot},
    scheduler::{ClaimedJob, JobKind, Scheduler, TickReport},
    session::Registry,
    testkit::{self, FakeLease, FakeSession},
};
use canterlot_proto::worker::v1::{
    BlockDelta, ControlDown, LimitObservation, LimitReached, LimitWindow, control_down,
    record_body::Body,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

fn start() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap()
}
fn secs(n: i64) -> TimeDelta {
    TimeDelta::seconds(n)
}
// Проверки подтверждения показанием: пробуждение по часам им не мешает.
fn patient_config() -> LimitsConfig {
    LimitsConfig {
        unverified_wake_after: Duration::from_secs(30 * 86_400),
        ..Default::default()
    }
}
fn timers() -> HubTimers {
    HubTimers {
        degraded_after: Duration::from_secs(3600),
        unavailable_after: Duration::from_secs(7200),
        ..Default::default()
    }
}
fn proto_window(window: Window) -> i32 {
    match window {
        Window::FiveHour => LimitWindow::FiveHour as i32,
        Window::Weekly => LimitWindow::Weekly as i32,
        Window::Monthly => LimitWindow::Monthly as i32,
    }
}
fn weekly_hold(provider: &str, resets_at: Option<DateTime<Utc>>) -> HoldReason {
    HoldReason::WeeklyReserve {
        provider: provider.into(),
        resets_at,
    }
}
fn quota_hold(provider: &str, window: &str, wake_at: DateTime<Utc>) -> HoldReason {
    HoldReason::QuotaExhausted {
        provider: provider.into(),
        window: window.into(),
        wake_at,
    }
}
fn has_weekly(holds: &[HoldReason], provider: &str) -> bool {
    holds
        .iter()
        .any(|h| matches!(h, HoldReason::WeeklyReserve { provider: p, .. } if p == provider))
}
fn has_quota(holds: &[HoldReason]) -> bool {
    holds
        .iter()
        .any(|h| matches!(h, HoldReason::QuotaExhausted { .. }))
}
fn gate() -> GateInputs {
    GateInputs {
        machine_available: true,
        machine_reconciled: true,
        session_capabilities: vec!["pilot_v1".into()],
    }
}
fn text(t: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text { text: t.into() }]
}

async fn new_machine(pool: &PgPool, owner: Uuid, provider: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'m')")
        .bind(id)
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    testkit::seed_catalog(pool, id).await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    if provider == "codex" {
        sqlx::query(
            "update machine_harnesses set harness='synthetic-codex',provider='codex' where machine_id=$1",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

struct Env {
    pool: PgPool,
    owner: Uuid,
    claude: Uuid,
    codex: Uuid,
    registry: Registry,
    receivers: HashMap<Uuid, mpsc::Receiver<ControlDown>>,
    _leases: Vec<FakeLease>,
    received: Vec<(Uuid, ControlDown)>,
    dispatcher: Dispatcher,
    triggers: mpsc::Receiver<Uuid>,
    scheduler: Scheduler,
    config: LimitsConfig,
}

impl Env {
    async fn new(pool: &PgPool) -> Self {
        Self::with_config(pool, LimitsConfig::default()).await
    }
    async fn with_config(pool: &PgPool, config: LimitsConfig) -> Self {
        let owner = Uuid::now_v7();
        sqlx::query("insert into owners(id) values($1)")
            .bind(owner)
            .execute(pool)
            .await
            .unwrap();
        let claude = new_machine(pool, owner, "claude").await;
        Self::attach(pool, owner, claude, config).await
    }
    async fn attach(pool: &PgPool, owner: Uuid, claude: Uuid, config: LimitsConfig) -> Self {
        testkit::seed_catalog(pool, claude).await;
        let codex = new_machine(pool, owner, "codex").await;
        let registry = Registry::new(timers());
        let mut receivers = HashMap::new();
        let mut leases = Vec::new();
        for machine in [claude, codex] {
            let FakeSession { commands, lease } =
                testkit::fake_session(&registry, machine, &["pilot_v1", "instructions_v1"], true);
            receivers.insert(machine, commands);
            leases.push(lease);
        }
        let (dispatcher, triggers) = Dispatcher::channel();
        let scheduler = Scheduler::new(pool.clone(), dispatcher.clone(), Default::default())
            .with_limits(registry.clone(), config.clone());
        Self {
            pool: pool.clone(),
            owner,
            claude,
            codex,
            registry,
            receivers,
            _leases: leases,
            received: Vec::new(),
            dispatcher,
            triggers,
            scheduler,
            config,
        }
    }
    fn new_scheduler(&self) -> Scheduler {
        Scheduler::new(
            self.pool.clone(),
            self.dispatcher.clone(),
            Default::default(),
        )
        .with_limits(self.registry.clone(), self.config.clone())
    }
    async fn conversation(&self, crunch: bool, live: bool) -> Uuid {
        self.conversation_on(self.claude, crunch, live).await
    }
    async fn codex_conversation(&self, crunch: bool, live: bool) -> Uuid {
        self.conversation_on(self.codex, crunch, live).await
    }
    async fn conversation_on(&self, machine: Uuid, crunch: bool, live: bool) -> Uuid {
        let id = testkit::seed_conversation(&self.pool, self.owner, machine).await;
        sqlx::query("update conversations set crunch_enabled=$2,model='synthetic-small',effort='low',harness=case when $3 then 'synthetic-codex' else harness end where id=$1")
            .bind(id).bind(crunch).bind(machine == self.codex).execute(&self.pool).await.unwrap();
        if live {
            sqlx::query("insert into executions(id,conversation_id,generation,state) values($1,$2,1,'running')")
                .bind(Uuid::now_v7()).bind(id).execute(&self.pool).await.unwrap();
            sqlx::query("update queue_state set hold_reasons='[]' where conversation_id=$1")
                .bind(id)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        id
    }
    async fn limit_reached(
        &self,
        conversation: Uuid,
        provider: &str,
        window: Window,
        resets_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> LimitReachedEffect {
        let (execution, machine): (Uuid, Uuid) = sqlx::query_as("select e.id,c.machine_id from executions e join conversations c on c.id=e.conversation_id where e.conversation_id=$1 order by e.generation desc limit 1")
            .bind(conversation).fetch_one(&self.pool).await.unwrap();
        let mut tx = self.pool.begin().await.unwrap();
        let effect = limits::on_limit_reached_in_tx(
            &mut tx,
            self.owner,
            &ExecutionRef {
                execution_id: execution,
                conversation_id: conversation,
                machine_id: machine,
            },
            &LimitReached {
                execution_id: execution.to_string(),
                provider: provider.into(),
                window: proto_window(window),
                resets_at_ms: resets_at.map_or(0, |at| at.timestamp_millis() as u64),
                input_command_id: String::new(),
            },
            &self.config,
            now,
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        effect
    }
    async fn observe(
        &self,
        provider: &str,
        window: Window,
        used: u16,
        resets_at: Option<DateTime<Utc>>,
        at: DateTime<Utc>,
    ) -> Recorded {
        self.observe_full(
            provider,
            window,
            used,
            resets_at,
            at,
            at,
            ObservationSource::Session,
        )
        .await
    }
    async fn observe_estimated(
        &self,
        provider: &str,
        window: Window,
        used: u16,
        resets_at: Option<DateTime<Utc>>,
        at: DateTime<Utc>,
    ) -> Recorded {
        self.observe_full_confidence(
            provider,
            window,
            used,
            resets_at,
            at,
            at,
            ObservationSource::Session,
            DataConfidence::Estimated,
        )
        .await
    }
    async fn observe_full(
        &self,
        provider: &str,
        window: Window,
        used: u16,
        resets_at: Option<DateTime<Utc>>,
        observed_at: DateTime<Utc>,
        received_at: DateTime<Utc>,
        source: ObservationSource,
    ) -> Recorded {
        self.observe_full_confidence(
            provider,
            window,
            used,
            resets_at,
            observed_at,
            received_at,
            source,
            DataConfidence::Exact,
        )
        .await
    }
    async fn observe_full_confidence(
        &self,
        provider: &str,
        window: Window,
        used: u16,
        resets_at: Option<DateTime<Utc>>,
        observed_at: DateTime<Utc>,
        received_at: DateTime<Utc>,
        source: ObservationSource,
        data_confidence: DataConfidence,
    ) -> Recorded {
        let machine = if provider == "codex" {
            self.codex
        } else {
            self.claude
        };
        let mut tx = self.pool.begin().await.unwrap();
        let recorded = limits::record_observation_in_tx(
            &mut tx,
            self.owner,
            &Observation {
                provider: provider.into(),
                window,
                used_permille: used,
                resets_at,
                observed_at,
                source,
                data_confidence,
                machine_id: machine,
                execution_id: None,
            },
            received_at,
            &self.config,
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        recorded
    }
    async fn tick(&mut self, now: DateTime<Utc>) -> TickReport {
        let report = self.scheduler.tick(now).await.unwrap();
        self.drain();
        report
    }
    fn drain(&mut self) {
        for (machine, rx) in &mut self.receivers {
            while let Ok(message) = rx.try_recv() {
                self.received.push((*machine, message));
            }
        }
    }
    fn take_polls(&mut self) -> Vec<(Uuid, String)> {
        self.drain();
        let mut polls = Vec::new();
        self.received.retain(|(machine, message)| {
            if let Some(control_down::Msg::PollLimits(p)) = &message.msg {
                assert!(!p.owner_refresh, "the crunch check is not an owner request");
                polls.push((*machine, p.provider.clone()));
                false
            } else {
                true
            }
        });
        polls
    }
    fn triggered(&mut self) -> Vec<Uuid> {
        let mut ids = Vec::new();
        while let Ok(id) = self.triggers.try_recv() {
            ids.push(id);
        }
        ids
    }
    async fn spend(&self, conversation: Uuid, key: &str, now: DateTime<Utc>) -> SpendOutcome {
        crunch::spend_reserve(
            &self.pool,
            self.owner,
            conversation,
            key,
            now,
            &self.dispatcher,
            Uuid::now_v7(),
        )
        .await
        .unwrap()
    }
    async fn submit(&self, conversation: Uuid, key: &str) -> Uuid {
        submit_input(
            &self.pool,
            SubmitInput {
                owner_id: self.owner,
                conversation_id: conversation,
                idempotency_key: key.into(),
                content: text("owner input"),
                received_at: Utc::now(),
                correlation_id: Uuid::now_v7(),
                origin: None,
            },
        )
        .await
        .unwrap()
        .input_id
    }
}

#[derive(sqlx::FromRow, Debug)]
struct Job {
    id: Uuid,
    conversation_id: Option<Uuid>,
    state: String,
    run_at: DateTime<Utc>,
    payload: Value,
    attempts: i32,
    result: Option<Value>,
}
async fn jobs(pool: &PgPool, kind: &str) -> Vec<Job> {
    sqlx::query_as("select id,conversation_id,state,run_at,payload,attempts,result from scheduled_jobs where kind=$1 order by created_at,id")
        .bind(kind).fetch_all(pool).await.unwrap()
}
async fn pending(pool: &PgPool, kind: &str) -> Vec<Job> {
    jobs(pool, kind)
        .await
        .into_iter()
        .filter(|j| j.state == "pending")
        .collect()
}
#[derive(sqlx::FromRow)]
struct QueueRow {
    pause_reasons: sqlx::types::Json<Vec<PauseReason>>,
    hold_reasons: sqlx::types::Json<Vec<HoldReason>>,
    run_slot: String,
    run_slot_input_id: Option<Uuid>,
    run_slot_attempt: Option<i32>,
}
async fn queue(pool: &PgPool, id: Uuid) -> (Vec<PauseReason>, Vec<HoldReason>, RunSlot) {
    let row: QueueRow = sqlx::query_as("select pause_reasons,hold_reasons,run_slot,run_slot_input_id,run_slot_attempt from queue_state where conversation_id=$1")
        .bind(id).fetch_one(pool).await.unwrap();
    (
        row.pause_reasons.0,
        row.hold_reasons.0,
        RunSlot::from_columns(&row.run_slot, row.run_slot_input_id, row.run_slot_attempt).unwrap(),
    )
}
async fn holds(pool: &PgPool, id: Uuid) -> Vec<HoldReason> {
    queue(pool, id).await.1
}
async fn pauses(pool: &PgPool, id: Uuid) -> Vec<PauseReason> {
    queue(pool, id).await.0
}
async fn resume_pending(pool: &PgPool, id: Uuid) -> bool {
    sqlx::query_scalar("select resume_pending from conversations where id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn wake_texts(pool: &PgPool, id: Uuid) -> Vec<String> {
    sqlx::query_scalar("select content->0->>'text' from inputs where conversation_id=$1 and idempotency_key like 'wake:%' order by input_seq")
        .bind(id).fetch_all(pool).await.unwrap()
}
async fn inputs(pool: &PgPool, id: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from inputs where conversation_id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn reserve(pool: &PgPool, provider: &str) -> (bool, Option<DateTime<Utc>>, i16) {
    sqlx::query_as("select weekly_reserve_active,weekly_reserve_resets_at,weekly_low_count from provider_limits where provider=$1")
        .bind(provider).fetch_one(pool).await.unwrap()
}
async fn override_until(pool: &PgPool, id: Uuid) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("select reserve_override_until from conversations where id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn events_of(pool: &PgPool, id: Uuid, kind: &str) -> Vec<Value> {
    sqlx::query_scalar("select payload from events where scope=$1 and kind=$2 order by scope_seq")
        .bind(format!("conversation:{id}"))
        .bind(kind)
        .fetch_all(pool)
        .await
        .unwrap()
}
async fn deadlocks(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select deadlocks from pg_stat_database where datname=current_database()")
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn set_resume_pending(pool: &PgPool, ids: &[Uuid]) {
    sqlx::query("update conversations set resume_pending=true where id=any($1)")
        .bind(ids)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn five_hour_limit_without_crunch_pauses_with_reset_time(pool: PgPool) {
    let env = Env::new(&pool).await;
    let id = env.conversation(false, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(2);
    let effect = env
        .limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    assert_eq!(effect, LimitReachedEffect::Paused);
    let (pauses, holds, _) = queue(&pool, id).await;
    assert_eq!(
        pauses,
        vec![PauseReason::LimitReached {
            provider: Some("claude".into()),
            window: Some("five_hour".into()),
            resets_at: Some(reset),
        }]
    );
    assert!(!has_quota(&holds));
    assert!(!resume_pending(&pool, id).await);
    assert!(jobs(&pool, "wake").await.is_empty());
    let facts = events_of(&pool, id, "limit_reached").await;
    assert_eq!(facts.len(), 1);
    assert_eq!(
        facts[0]["resets_at"],
        reset.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );
    assert!(events_of(&pool, id, "crunch_held").await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn five_hour_limit_with_crunch_holds_and_schedules_wake_at_reset_plus_grace(pool: PgPool) {
    let mut run = worker::Run::new(&pool).await;
    let id = run.seed.conversation_id;
    sqlx::query("update conversations set crunch_enabled=true where id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let input = run.input(&pool).await;
    let now = start();
    run.send(vec![worker::accepted(
        input.command_id,
        now.timestamp_millis(),
    )])
    .await;
    let reset = now + TimeDelta::hours(2);
    run.send(vec![Body::LimitReached(LimitReached {
        execution_id: run.execution.execution_id.to_string(),
        provider: "claude".into(),
        window: LimitWindow::FiveHour as i32,
        resets_at_ms: reset.timestamp_millis() as u64,
        input_command_id: input.command_id.to_string(),
    })])
    .await;
    let (pauses, holds, slot) = queue(&pool, id).await;
    assert!(pauses.is_empty(), "{pauses:?}");
    assert!(holds.contains(&quota_hold("claude", "five_hour", reset)));
    assert_eq!(
        slot,
        RunSlot::Running {
            input_id: input.input_id
        }
    );
    assert!(resume_pending(&pool, id).await);
    let wakes = jobs(&pool, "wake").await;
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].state, "pending");
    assert_eq!(wakes[0].conversation_id, Some(id));
    assert_eq!(wakes[0].run_at, reset + secs(20));
    assert_eq!(wakes[0].payload["provider"], "claude");
    assert_eq!(wakes[0].payload["window"], "five_hour");
    assert_eq!(wakes[0].payload["hold_key"], "quota:claude:five_hour");
    let state: String = sqlx::query_scalar("select state from executions where id=$1")
        .bind(run.execution.execution_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "running");
    let held = events_of(&pool, id, "crunch_held").await;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["provider"], "claude");
    assert_eq!(held[0]["window"], "five_hour");
    assert_eq!(held[0]["resume_pending"], true);
    assert_eq!(
        held[0]["wake_at"],
        reset.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );
    assert_eq!(events_of(&pool, id, "limit_reached").await.len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_single_estimated_reset_reading_does_not_release_the_hold(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.observe(
        "claude",
        Window::FiveHour,
        990,
        Some(reset),
        now - TimeDelta::minutes(1),
    )
    .await;
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    env.observe_estimated(
        "claude",
        Window::FiveHour,
        30,
        Some(reset + TimeDelta::hours(5)),
        reset + secs(60),
    )
    .await;
    let report = env.tick(reset + secs(120)).await;
    assert_eq!((report.fired, report.retried), (0, 1));
    assert!(has_quota(&holds(&pool, id).await));
    assert!(resume_pending(&pool, id).await);
    assert!(wake_texts(&pool, id).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_early_estimated_reset_reading_does_not_release_the_hold(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.observe(
        "claude",
        Window::FiveHour,
        990,
        Some(reset),
        now - TimeDelta::minutes(1),
    )
    .await;
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    // Оценочный отсчёт со сдвинутым сроком снят до срока сброса и сам его не доказывает.
    env.observe_estimated(
        "claude",
        Window::FiveHour,
        100,
        Some(reset + TimeDelta::hours(5)),
        now + TimeDelta::minutes(10),
    )
    .await;
    let report = env.tick(reset + secs(120)).await;
    assert_eq!((report.fired, report.retried), (0, 1));
    assert!(has_quota(&holds(&pool, id).await));
    assert!(resume_pending(&pool, id).await);
    assert!(wake_texts(&pool, id).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_second_estimated_reset_reading_after_the_wake_releases_the_hold(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.observe(
        "claude",
        Window::FiveHour,
        990,
        Some(reset),
        now - TimeDelta::minutes(1),
    )
    .await;
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    env.observe_estimated(
        "claude",
        Window::FiveHour,
        30,
        Some(reset + TimeDelta::hours(5)),
        reset + secs(60),
    )
    .await;
    env.observe_estimated(
        "claude",
        Window::FiveHour,
        20,
        Some(reset + TimeDelta::hours(6)),
        reset + secs(90),
    )
    .await;
    let report = env.tick(reset + secs(120)).await;
    assert_eq!((report.fired, report.retried), (1, 0));
    assert!(!has_quota(&holds(&pool, id).await));
    assert_eq!(
        wake_texts(&pool, id).await,
        vec![LimitsConfig::default().default_resume_text]
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_not_reset_reading_does_not_confirm_an_estimated_reset(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.observe(
        "claude",
        Window::FiveHour,
        990,
        Some(reset),
        now - TimeDelta::minutes(1),
    )
    .await;
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    env.observe(
        "claude",
        Window::FiveHour,
        700,
        Some(reset),
        reset + secs(30),
    )
    .await;
    env.observe_estimated(
        "claude",
        Window::FiveHour,
        100,
        Some(reset + TimeDelta::hours(5)),
        reset + secs(60),
    )
    .await;
    let report = env.tick(reset + secs(120)).await;
    assert_eq!((report.fired, report.retried), (0, 1));
    assert!(has_quota(&holds(&pool, id).await));
    assert!(resume_pending(&pool, id).await);
    assert!(wake_texts(&pool, id).await.is_empty());
    // Второй подтверждающий отсчёт после срока всё-таки снимает удержание.
    env.observe_estimated(
        "claude",
        Window::FiveHour,
        90,
        Some(reset + TimeDelta::hours(6)),
        reset + secs(150),
    )
    .await;
    let report = env.tick(reset + secs(180)).await;
    assert_eq!((report.fired, report.retried), (1, 0));
    assert!(!has_quota(&holds(&pool, id).await));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_exact_reset_reading_still_releases_the_hold_on_its_own(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.observe(
        "claude",
        Window::FiveHour,
        990,
        Some(reset),
        now - TimeDelta::minutes(1),
    )
    .await;
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    env.observe(
        "claude",
        Window::FiveHour,
        30,
        Some(reset + TimeDelta::hours(5)),
        reset + secs(60),
    )
    .await;
    let report = env.tick(reset + secs(120)).await;
    assert_eq!((report.fired, report.retried), (1, 0));
    assert!(!has_quota(&holds(&pool, id).await));
    assert_eq!(
        wake_texts(&pool, id).await,
        vec![LimitsConfig::default().default_resume_text]
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_missing_five_hour_window_wakes_after_an_estimated_reading(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.observe_estimated(
        "claude",
        Window::FiveHour,
        990,
        Some(reset),
        now - TimeDelta::minutes(1),
    )
    .await;
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    let mut tx = pool.begin().await.unwrap();
    limits::record_absence_in_tx(
        &mut tx,
        env.owner,
        "claude",
        Window::FiveHour,
        reset + secs(60),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let report = env.tick(reset + secs(120)).await;
    assert_eq!((report.fired, report.retried), (1, 0));
    assert!(!has_quota(&holds(&pool, id).await));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unknown_reset_time_uses_the_retry_interval(pool: PgPool) {
    let env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let effect = env
        .limit_reached(id, "claude", Window::FiveHour, None, now)
        .await;
    let expected = now + TimeDelta::minutes(30);
    let LimitReachedEffect::Held { wake_job, wake_at } = effect else {
        panic!("held expected")
    };
    assert_eq!(wake_at, expected);
    assert!(
        holds(&pool, id)
            .await
            .contains(&quota_hold("claude", "five_hour", expected))
    );
    let wakes = pending(&pool, "wake").await;
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].id, wake_job);
    assert_eq!(wakes[0].run_at, expected + secs(20));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn repeat_limit_reached_with_a_later_reset_moves_the_wake(pool: PgPool) {
    let env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let first = now + TimeDelta::hours(1);
    let LimitReachedEffect::Held { wake_job, .. } = env
        .limit_reached(id, "claude", Window::FiveHour, Some(first), now)
        .await
    else {
        panic!("held")
    };
    let later = now + TimeDelta::hours(3);
    let again = env
        .limit_reached(id, "claude", Window::FiveHour, Some(later), now + secs(5))
        .await;
    assert_eq!(
        again,
        LimitReachedEffect::Held {
            wake_job,
            wake_at: later
        }
    );
    let quota: Vec<_> = holds(&pool, id)
        .await
        .into_iter()
        .filter(|h| matches!(h, HoldReason::QuotaExhausted { .. }))
        .collect();
    assert_eq!(quota, vec![quota_hold("claude", "five_hour", later)]);
    let wakes = pending(&pool, "wake").await;
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].id, wake_job);
    assert_eq!(wakes[0].run_at, later + secs(20));
    // Более ранний сброс не сдвигает пробуждение назад.
    env.limit_reached(
        id,
        "claude",
        Window::FiveHour,
        Some(now + TimeDelta::hours(2)),
        now + secs(6),
    )
    .await;
    assert!(
        holds(&pool, id)
            .await
            .contains(&quota_hold("claude", "five_hour", later))
    );
    assert_eq!(pending(&pool, "wake").await[0].run_at, later + secs(20));
    // Другое окно того же провайдера - отдельное удержание и отдельное задание.
    let weekly = now + TimeDelta::days(3);
    env.limit_reached(id, "claude", Window::Weekly, Some(weekly), now + secs(7))
        .await;
    let wakes = pending(&pool, "wake").await;
    assert_eq!(wakes.len(), 2);
    assert!(
        wakes
            .iter()
            .any(|j| j.payload["hold_key"] == "quota:claude:weekly")
    );
    assert!(
        holds(&pool, id)
            .await
            .contains(&quota_hold("claude", "weekly", weekly))
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wake_before_a_fresh_observation_requests_a_poll_and_retries_with_backoff(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    let early = env.tick(reset + secs(19)).await;
    assert_eq!((early.fired, early.retried), (0, 0));
    assert!(env.take_polls().is_empty());
    let first = env.tick(reset + secs(20)).await;
    assert_eq!(first.retried, 1);
    let job = &pending(&pool, "wake").await[0];
    assert_eq!(job.attempts, 1);
    assert_eq!(job.run_at, reset + secs(50));
    assert_eq!(env.take_polls(), vec![(env.claude, "claude".to_string())]);
    assert!(has_quota(&holds(&pool, id).await));
    assert_eq!(inputs(&pool, id).await, 0);
    let second = env.tick(reset + secs(50)).await;
    assert_eq!(second.retried, 1);
    let job = &pending(&pool, "wake").await[0];
    assert_eq!(job.attempts, 2);
    assert_eq!(job.run_at, reset + secs(110));
    // Первые проверки опрашивают не чаще poll_min_interval (5 минут).
    assert!(env.take_polls().is_empty());
    assert!(has_quota(&holds(&pool, id).await));
    assert!(resume_pending(&pool, id).await);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn poll_is_requested_after_the_wake_transaction_commits(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    let job = pending(&pool, "wake").await[0].id;
    let mut rx = env.receivers.remove(&env.claude).unwrap();
    sqlx::query("alter table scheduled_jobs add constraint forbid_retry check (attempts = 0)")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        env.scheduler.tick(reset + secs(20)).await.unwrap().retried,
        1
    );
    assert!(rx.try_recv().is_err(), "no poll for a rolled back attempt");
    assert_eq!(pending(&pool, "wake").await[0].attempts, 0);
    let polls: i64 = sqlx::query_scalar("select count(*) from limit_polls")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(polls, 0);
    sqlx::query("alter table scheduled_jobs drop constraint forbid_retry")
        .execute(&pool)
        .await
        .unwrap();
    let observer = {
        let pool = pool.clone();
        tokio::spawn(async move {
            loop {
                let message = rx.recv().await.unwrap();
                if let Some(control_down::Msg::PollLimits(p)) = message.msg {
                    let attempts: i32 =
                        sqlx::query_scalar("select attempts from scheduled_jobs where id=$1")
                            .bind(job)
                            .fetch_one(&pool)
                            .await
                            .unwrap();
                    return (p.provider, attempts);
                }
            }
        })
    };
    let retry_at = pending(&pool, "wake").await[0].run_at;
    env.scheduler.tick(retry_at).await.unwrap();
    let (provider, attempts) = tokio::time::timeout(Duration::from_secs(5), observer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(provider, "claude");
    assert_eq!(attempts, 1, "the retry was committed before the poll");
}

async fn poll_offsets(env: &mut Env, from: DateTime<Utc>, until: DateTime<Utc>) -> Vec<i64> {
    let mut at = from;
    let mut offsets = Vec::new();
    while at <= until {
        let report = env.tick(at).await;
        if env
            .take_polls()
            .iter()
            .any(|(m, p)| *m == env.claude && p == "claude")
        {
            offsets.push((at - from).num_seconds());
        }
        at = report.next_due.unwrap();
    }
    offsets
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn poll_cadence_falls_back_to_the_provider_floor_after_the_backoff(pool: PgPool) {
    let config = LimitsConfig {
        poll_min_interval: Duration::from_secs(30),
        ..patient_config()
    };
    let mut env = Env::with_config(&pool, config).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(3);
    env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
        .await;
    let reset = now + TimeDelta::hours(1);
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    let expected = vec![0, 30, 90, 210, 510, 2310, 4110];
    let wake = poll_offsets(&mut env, reset + secs(20), reset + secs(20 + 4110)).await;
    assert_eq!(wake, expected, "wake cadence");
    sqlx::query("update scheduled_jobs set state='cancelled' where kind='wake'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("delete from limit_polls")
        .execute(&pool)
        .await
        .unwrap();
    let check = pending(&pool, "limit_check").await;
    assert_eq!(check.len(), 1);
    assert_eq!(check[0].run_at, weekly_reset + secs(20));
    let check = poll_offsets(
        &mut env,
        weekly_reset + secs(20),
        weekly_reset + secs(20 + 4110),
    )
    .await;
    assert_eq!(check, expected, "limit_check cadence");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wake_after_verified_reset_releases_hold_and_queues_resume_text_once(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.observe(
        "claude",
        Window::FiveHour,
        990,
        Some(reset),
        now - TimeDelta::minutes(1),
    )
    .await;
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    env.observe(
        "claude",
        Window::FiveHour,
        30,
        Some(reset + TimeDelta::hours(5)),
        reset + secs(60),
    )
    .await;
    env.triggered();
    let report = env.tick(reset + secs(120)).await;
    assert_eq!(report.fired, 1);
    assert!(!has_quota(&holds(&pool, id).await));
    assert_eq!(
        wake_texts(&pool, id).await,
        vec![LimitsConfig::default().default_resume_text]
    );
    assert!(!resume_pending(&pool, id).await);
    let job = &jobs(&pool, "wake").await[0];
    assert_eq!(job.state, "fired");
    let input: Uuid =
        sqlx::query_scalar("select id from inputs where conversation_id=$1 and idempotency_key=$2")
            .bind(id)
            .bind(format!("wake:{}", job.id))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(job.result.as_ref().unwrap()["input_id"], input.to_string());
    let woken = events_of(&pool, id, "crunch_woken").await;
    assert_eq!(woken.len(), 1);
    assert_eq!(woken[0]["input_id"], input.to_string());
    assert_eq!(woken[0]["resume_pending"], false);
    assert!(env.triggered().contains(&id));
    assert!(env.take_polls().is_empty());
    let again = env.tick(reset + secs(600)).await;
    assert_eq!(again.fired, 0);
    assert_eq!(wake_texts(&pool, id).await.len(), 1);
    assert_eq!(inputs(&pool, id).await, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_five_hour_hold_wakes_when_the_provider_no_longer_has_that_window(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.observe(
        "claude",
        Window::FiveHour,
        990,
        Some(reset),
        now - TimeDelta::minutes(1),
    )
    .await;
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    let mut tx = pool.begin().await.unwrap();
    limits::record_absence_in_tx(
        &mut tx,
        env.owner,
        "claude",
        Window::FiveHour,
        reset + secs(60),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    env.triggered();
    let report = env.tick(reset + secs(120)).await;
    assert_eq!(report.fired, 1);
    assert!(!has_quota(&holds(&pool, id).await));
    assert!(env.take_polls().is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn five_hour_wake_is_not_verified_by_a_replayed_pre_limit_observation(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let known = env.conversation(true, true).await;
    let unknown = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    let before = now - TimeDelta::minutes(1);
    env.observe("claude", Window::FiveHour, 990, Some(reset), before)
        .await;
    env.observe("claude", Window::Weekly, 900, None, before)
        .await;
    env.limit_reached(known, "claude", Window::FiveHour, Some(reset), now)
        .await;
    env.limit_reached(unknown, "claude", Window::Weekly, None, now)
        .await;
    // Повтор доналимитного наблюдения - история, текущее значение не меняется.
    assert!(
        env.observe("claude", Window::FiveHour, 990, Some(reset), before)
            .await
            .stale
    );
    assert!(
        env.observe("claude", Window::Weekly, 900, None, before)
            .await
            .stale
    );
    let report = env.tick(now + TimeDelta::hours(2)).await;
    assert_eq!(report.fired, 0);
    assert_eq!(report.retried, 2);
    assert!(has_quota(&holds(&pool, known).await));
    assert!(has_quota(&holds(&pool, unknown).await));
    assert_eq!(inputs(&pool, known).await, 0);
    assert_eq!(inputs(&pool, unknown).await, 0);
    // Окно без времени сброса подтверждается более новым наблюдением после wake_at.
    env.observe(
        "claude",
        Window::Weekly,
        100,
        None,
        now + TimeDelta::minutes(31),
    )
    .await;
    env.tick(now + TimeDelta::hours(3)).await;
    assert!(!has_quota(&holds(&pool, unknown).await));
    assert_eq!(wake_texts(&pool, unknown).await.len(), 1);
    assert!(has_quota(&holds(&pool, known).await));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn hold_wakes_by_the_clock_when_no_reading_arrives_after_the_reset(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let known = env.conversation(true, true).await;
    let unknown = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::minutes(30);
    env.limit_reached(known, "claude", Window::FiveHour, Some(reset), now)
        .await;
    env.limit_reached(unknown, "claude", Window::Weekly, None, now)
        .await;
    let soon = env.tick(reset + TimeDelta::minutes(19)).await;
    assert_eq!(
        (soon.fired, soon.retried),
        (0, 2),
        "the poll still has a chance"
    );
    assert!(has_quota(&holds(&pool, known).await));
    let report = env.tick(reset + TimeDelta::minutes(21)).await;
    assert_eq!((report.fired, report.retried), (2, 0));
    for id in [known, unknown] {
        assert!(!has_quota(&holds(&pool, id).await));
        assert_eq!(wake_texts(&pool, id).await.len(), 1);
    }
    assert_eq!(
        jobs(&pool, "wake").await[0].result.as_ref().unwrap()["by_clock"],
        true
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn fresh_reading_of_a_full_window_keeps_the_hold_past_the_clock(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    env.observe(
        "claude",
        Window::FiveHour,
        1000,
        Some(reset),
        reset + TimeDelta::minutes(1),
    )
    .await;
    let report = env.tick(reset + TimeDelta::hours(2)).await;
    assert_eq!((report.fired, report.retried), (0, 1));
    assert!(has_quota(&holds(&pool, id).await));
    assert_eq!(inputs(&pool, id).await, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn weekly_reserve_ends_by_the_clock_when_no_reading_arrives_after_the_reset(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let idle = env.conversation(true, false).await;
    set_resume_pending(&pool, &[id]).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
        .await;
    env.tick(now).await;
    assert!(reserve(&pool, "claude").await.0);
    let soon = env.tick(weekly_reset + TimeDelta::minutes(19)).await;
    assert_eq!(soon.retried, 1, "the poll still has a chance");
    assert!(reserve(&pool, "claude").await.0);
    env.tick(weekly_reset + TimeDelta::minutes(21)).await;
    assert!(!reserve(&pool, "claude").await.0);
    for session in [id, idle] {
        assert!(
            holds(&pool, session)
                .await
                .iter()
                .all(|h| !matches!(h, HoldReason::WeeklyReserve { .. }))
        );
    }
    let woken = env.triggered();
    assert!(woken.contains(&id) && woken.contains(&idle), "{woken:?}");
    assert_eq!(jobs(&pool, "limit_check").await[0].state, "fired");
    env.tick(weekly_reset + TimeDelta::minutes(21)).await;
    assert_eq!(wake_texts(&pool, id).await.len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn fresh_reading_above_the_threshold_keeps_the_reserve_past_the_clock(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, false).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
        .await;
    env.tick(now).await;
    env.observe(
        "claude",
        Window::Weekly,
        970,
        Some(weekly_reset),
        weekly_reset + TimeDelta::minutes(1),
    )
    .await;
    env.tick(weekly_reset + TimeDelta::hours(2)).await;
    assert!(reserve(&pool, "claude").await.0);
    assert!(
        holds(&pool, id)
            .await
            .iter()
            .any(|h| matches!(h, HoldReason::WeeklyReserve { .. }))
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn custom_resume_text_is_used_and_default_comes_from_config(pool: PgPool) {
    let config = LimitsConfig {
        default_resume_text: "Текст из конфигурации".into(),
        ..Default::default()
    };
    let mut env = Env::with_config(&pool, config).await;
    let custom = env.conversation(true, true).await;
    let fallback = env.conversation(true, true).await;
    sqlx::query("update conversations set crunch_resume_text='Свой текст продолжения' where id=$1")
        .bind(custom)
        .execute(&pool)
        .await
        .unwrap();
    let now = start();
    let reset = now + TimeDelta::hours(1);
    for id in [custom, fallback] {
        env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
            .await;
    }
    env.observe(
        "claude",
        Window::FiveHour,
        10,
        Some(reset + TimeDelta::hours(5)),
        reset + secs(30),
    )
    .await;
    env.tick(reset + secs(60)).await;
    assert_eq!(
        wake_texts(&pool, custom).await,
        vec!["Свой текст продолжения".to_string()]
    );
    assert_eq!(
        wake_texts(&pool, fallback).await,
        vec!["Текст из конфигурации".to_string()]
    );
}

async fn verified_reset(env: &Env, reset: DateTime<Utc>) {
    env.observe(
        "claude",
        Window::FiveHour,
        10,
        Some(reset + TimeDelta::hours(5)),
        reset + secs(30),
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
async fn rereview_nineteen_polls_and_backend_loss_still_wake_after_reset(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + secs(3600);
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    for expected in 1..=19 {
        let job = pending(&pool, "wake").await.remove(0);
        env.tick(job.run_at).await;
        assert_eq!(pending(&pool, "wake").await[0].attempts, expected);
    }
    let job = pending(&pool, "wake").await.remove(0);
    sqlx::raw_sql("create function delay_provider() returns trigger language plpgsql as $$ begin perform pg_sleep(10); return new; end $$; create trigger delay_provider before insert on provider_limits for each row execute function delay_provider();")
        .execute(&pool).await.unwrap();
    let kill = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let pid: Option<i32> = sqlx::query_scalar("select pid from pg_stat_activity where datname=current_database() and wait_event='PgSleep' and query like '%insert into provider_limits%' and pid<>pg_backend_pid()")
                    .fetch_optional(&pool).await.unwrap();
                if let Some(pid) = pid {
                    sqlx::query("select pg_terminate_backend($1)").bind(pid).execute(&pool).await.unwrap();
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
    };
    let (tick, ()) = tokio::join!(env.scheduler.tick(job.run_at), kill);
    assert!(tick.is_ok());
    sqlx::query("drop trigger delay_provider on provider_limits")
        .execute(&pool)
        .await
        .unwrap();
    let after = jobs(&pool, "wake").await.remove(0);
    assert_eq!(after.state, "pending");
    assert_eq!(
        after.attempts, 19,
        "polls and errors must be counted separately"
    );
    assert!(after.run_at <= job.run_at + secs(60));
    let failures: i32 = sqlx::query_scalar("select failures from scheduled_jobs where id=$1")
        .bind(job.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(failures, 1);
    verified_reset(&env, job.run_at).await;
    env.tick(after.run_at.max(job.run_at + secs(31))).await;
    assert!(!has_quota(&holds(&pool, id).await));
    assert_eq!(wake_texts(&pool, id).await.len(), 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn rereview_limit_check_transient_errors_retry_beyond_max_attempts(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let now = start();
    env.observe("claude", Window::Weekly, 960, Some(now + secs(3600)), now)
        .await;
    env.tick(now).await;
    sqlx::query("update scheduled_jobs set attempts=19 where kind='limit_check'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("create function fail_provider() returns trigger language plpgsql as $$ begin raise exception using errcode='57014',message='temporary timeout'; end $$; create trigger fail_provider before insert on provider_limits for each row execute function fail_provider();")
        .execute(&pool).await.unwrap();
    for _ in 0..22 {
        let job = pending(&pool, "limit_check").await.remove(0);
        env.tick(job.run_at).await;
        let after = jobs(&pool, "limit_check").await.remove(0);
        assert_eq!(after.state, "pending");
        assert_eq!(after.attempts, 19);
        assert!(after.run_at > job.run_at && after.run_at <= job.run_at + secs(60));
    }
    sqlx::query("drop trigger fail_provider on provider_limits")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update provider_limits set weekly_reserve_active=false")
        .execute(&pool)
        .await
        .unwrap();
    let job = pending(&pool, "limit_check").await.remove(0);
    env.tick(job.run_at).await;
    assert_eq!(jobs(&pool, "limit_check").await[0].state, "fired");
}

#[sqlx::test(migrations = "./migrations")]
async fn rereview_late_limit_fact_cannot_rearm_owner_stopped_turn(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let now = start();
    let reset = now + secs(3600);
    for (index, execution_stop, continue_first) in
        [(0, false, false), (1, false, true), (2, true, true)]
    {
        let id = env.conversation(true, true).await;
        if execution_stop {
            control::request_stop_execution(
                &pool,
                env.owner,
                id,
                false,
                &format!("stop-{index}"),
                Uuid::now_v7(),
            )
            .await
            .unwrap();
        } else {
            control::request_stop(
                &pool,
                env.owner,
                id,
                &format!("stop-{index}"),
                Uuid::now_v7(),
            )
            .await
            .unwrap();
        }
        if continue_first {
            control::continue_queue(
                &pool,
                env.owner,
                id,
                &format!("continue-{index}"),
                Uuid::now_v7(),
            )
            .await
            .unwrap();
        }
        assert!(!resume_pending(&pool, id).await);
        env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
            .await;
        assert!(
            !resume_pending(&pool, id).await,
            "late fact rearmed owner stop {index}"
        );
        if !continue_first {
            control::continue_queue(
                &pool,
                env.owner,
                id,
                &format!("continue-{index}"),
                Uuid::now_v7(),
            )
            .await
            .unwrap();
        }
        verified_reset(&env, reset).await;
        env.tick(reset + secs(60)).await;
        assert!(wake_texts(&pool, id).await.is_empty());
        assert!(!has_quota(&holds(&pool, id).await));
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn rereview_new_hold_gets_fresh_expiry_after_no_live_skip(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + secs(3600);
    env.observe(
        "claude",
        Window::Weekly,
        10,
        Some(now + TimeDelta::days(1)),
        now,
    )
    .await;
    let owner_input = env.submit(id, "queued-before-limit").await;
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    sqlx::query(
        "update executions set state='stopped',outcome='normal_exit' where conversation_id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    verified_reset(&env, reset).await;
    env.tick(reset + secs(60)).await;
    assert!(resume_pending(&pool, id).await);
    let old_expiry: DateTime<Utc> =
        sqlx::query_scalar("select resume_pending_until from conversations where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,2,'running')",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let DispatchOutcome::Dispatched { command_id, .. } =
        dispatch_next(&pool, id, gate(), Uuid::now_v7())
            .await
            .unwrap()
    else {
        panic!("queued owner input did not dispatch after resume");
    };
    receipts::apply_input_receipt(&pool, command_id, 1, InputReceipt::Accepted, Uuid::now_v7())
        .await
        .unwrap();
    let state: String = sqlx::query_scalar("select state from inputs where id=$1")
        .bind(owner_input)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "accepted");
    let later = now + TimeDelta::days(7);
    let next_reset = later + TimeDelta::days(2);
    env.limit_reached(id, "claude", Window::FiveHour, Some(next_reset), later)
        .await;
    let expiry: DateTime<Utc> =
        sqlx::query_scalar("select resume_pending_until from conversations where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(expiry > next_reset && expiry > old_expiry);
    env.limit_reached(
        id,
        "claude",
        Window::FiveHour,
        Some(next_reset),
        later + secs(1),
    )
    .await;
    let duplicate_expiry: DateTime<Utc> =
        sqlx::query_scalar("select resume_pending_until from conversations where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(expiry, duplicate_expiry);
    verified_reset(&env, next_reset).await;
    env.tick(next_reset + secs(60)).await;
    assert_eq!(wake_texts(&pool, id).await.len(), 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn rereview_sj_stop_marker_blocks_old_turn_but_allows_new_turn(pool: PgPool) {
    let mut run = worker::Run::new(&pool).await;
    let mut env = Env::attach(
        &pool,
        run.seed.owner_id,
        run.seed.machine_id,
        Default::default(),
    )
    .await;
    run.ingest = Ingest::new(pool.clone()).with_registry(env.registry.clone());
    let id = run.seed.conversation_id;
    sqlx::query("update conversations set crunch_enabled=true where id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let now = start();
    let reset = now + secs(3600);
    let first = run.input(&pool).await;
    run.send(vec![worker::accepted(
        first.command_id,
        now.timestamp_millis(),
    )])
    .await;
    control::request_stop(&pool, env.owner, id, "interrupt", Uuid::now_v7())
        .await
        .unwrap();
    control::continue_queue(&pool, env.owner, id, "continue", Uuid::now_v7())
        .await
        .unwrap();
    let fact = |command: Uuid, reset: DateTime<Utc>| {
        Body::LimitReached(LimitReached {
            execution_id: run.execution.execution_id.to_string(),
            provider: "claude".into(),
            window: LimitWindow::FiveHour as i32,
            resets_at_ms: reset.timestamp_millis() as u64,
            input_command_id: command.to_string(),
        })
    };
    let late = fact(first.command_id, reset);
    run.send(vec![late]).await;
    assert!(!resume_pending(&pool, id).await);
    verified_reset(&env, reset).await;
    env.tick(reset + secs(60)).await;
    assert!(wake_texts(&pool, id).await.is_empty());
    run.send(vec![worker::finished(
        &run,
        first.command_id,
        now.timestamp_millis(),
    )])
    .await;
    let second = run.input(&pool).await;
    run.send(vec![worker::accepted(
        second.command_id,
        now.timestamp_millis(),
    )])
    .await;
    run.send(vec![Body::LimitReached(LimitReached {
        execution_id: run.execution.execution_id.to_string(),
        provider: "claude".into(),
        window: LimitWindow::FiveHour as i32,
        resets_at_ms: (reset + secs(7200)).timestamp_millis() as u64,
        input_command_id: second.command_id.to_string(),
    })])
    .await;
    assert!(
        resume_pending(&pool, id).await,
        "a later owner turn may resume after its own limit"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wake_respects_user_pause_and_only_releases_the_hold(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    control::request_stop(&pool, env.owner, id, "stop", Uuid::now_v7())
        .await
        .unwrap();
    verified_reset(&env, reset).await;
    let report = env.tick(reset + secs(60)).await;
    assert_eq!(report.fired, 1);
    assert!(!has_quota(&holds(&pool, id).await));
    assert_eq!(pauses(&pool, id).await, vec![PauseReason::UserStop]);
    assert!(wake_texts(&pool, id).await.is_empty());
    assert!(!resume_pending(&pool, id).await);
    assert_eq!(
        jobs(&pool, "wake").await[0].result.as_ref().unwrap()["skipped"],
        "paused"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wake_with_no_live_execution_releases_the_hold_and_keeps_resume_pending(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    sqlx::query(
        "update executions set state='stopped',outcome='normal_exit' where conversation_id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    verified_reset(&env, reset).await;
    env.tick(reset + secs(60)).await;
    assert!(!has_quota(&holds(&pool, id).await));
    assert_eq!(inputs(&pool, id).await, 0);
    assert!(resume_pending(&pool, id).await);
    let job = &jobs(&pool, "wake").await[0];
    assert_eq!(job.state, "fired");
    assert_eq!(job.result.as_ref().unwrap()["skipped"], "no_live_execution");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wake_with_an_owner_input_queued_sends_no_resume_text(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let before = env.conversation(true, true).await;
    let after = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    let queued = env.submit(before, "queued-before").await;
    for id in [before, after] {
        env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
            .await;
    }
    assert!(resume_pending(&pool, before).await);
    let owner_after = env.submit(after, "queued-after").await;
    assert!(
        !resume_pending(&pool, after).await,
        "an owner input clears resume_pending"
    );
    verified_reset(&env, reset).await;
    env.tick(reset + secs(60)).await;
    for (id, input) in [(before, queued), (after, owner_after)] {
        assert!(!has_quota(&holds(&pool, id).await));
        assert!(wake_texts(&pool, id).await.is_empty());
        assert_eq!(inputs(&pool, id).await, 1);
        let state: String = sqlx::query_scalar("select state from inputs where id=$1")
            .bind(input)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "queued");
        assert!(!resume_pending(&pool, id).await);
    }
    for job in jobs(&pool, "wake").await {
        assert_eq!(job.result.unwrap()["skipped"], "queue_not_empty");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wake_while_weekly_reserve_active_never_sends_resume(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let held = env.conversation(true, true).await;
    let overridden = env.conversation(true, true).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(3);
    env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
        .await;
    env.tick(now).await;
    assert_eq!(
        env.spend(overridden, "spend", now + secs(10)).await,
        SpendOutcome::Continued
    );
    let reset = now + TimeDelta::hours(1);
    for id in [held, overridden] {
        env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now + secs(20))
            .await;
    }
    verified_reset(&env, reset).await;
    env.tick(reset + secs(60)).await;
    for id in [held, overridden] {
        assert!(!has_quota(&holds(&pool, id).await));
        assert!(wake_texts(&pool, id).await.is_empty());
        assert!(resume_pending(&pool, id).await);
    }
    for job in jobs(&pool, "wake").await {
        assert_eq!(job.result.unwrap()["skipped"], "weekly_reserve");
    }
    assert!(has_weekly(&holds(&pool, held).await, "claude"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn observation_95_percent_holds_new_turns_but_running_turn_completes(pool: PgPool) {
    let mut run = worker::Run::new(&pool).await;
    let id = run.seed.conversation_id;
    let mut env = Env::attach(
        &pool,
        run.seed.owner_id,
        run.seed.machine_id,
        LimitsConfig::default(),
    )
    .await;
    let first = run.input(&pool).await;
    let now = start();
    run.send(vec![worker::accepted(
        first.command_id,
        now.timestamp_millis(),
    )])
    .await;
    let second = env.submit(id, "second").await;
    let weekly_reset = now + TimeDelta::days(3);
    env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
        .await;
    // До правила резерв уже действует как выводимое удержание в шлюзе выдачи.
    let DispatchOutcome::Blocked(reasons) = dispatch_next(&pool, id, gate(), Uuid::now_v7())
        .await
        .unwrap()
    else {
        panic!("blocked")
    };
    assert!(
        reasons
            .iter()
            .any(|r| matches!(r, BlockReason::Held(h) if has_weekly(h, "claude")))
    );
    env.tick(now).await;
    let (_, holds_now, slot) = queue(&pool, id).await;
    assert!(holds_now.contains(&weekly_hold("claude", Some(weekly_reset))));
    assert_eq!(
        slot,
        RunSlot::Running {
            input_id: first.input_id
        }
    );
    let stops: i64 = sqlx::query_scalar("select count(*) from commands where conversation_id=$1 and kind in ('interrupt','withdraw_input','stop_execution')")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(stops, 0, "a running turn is not interrupted");
    run.send(vec![worker::finished(
        &run,
        first.command_id,
        now.timestamp_millis() + 1000,
    )])
    .await;
    assert_eq!(queue(&pool, id).await.2, RunSlot::Free);
    let DispatchOutcome::Blocked(reasons) = dispatch_next(&pool, id, gate(), Uuid::now_v7())
        .await
        .unwrap()
    else {
        panic!("the next input must stay queued")
    };
    assert!(
        reasons
            .iter()
            .any(|r| matches!(r, BlockReason::Held(h) if has_weekly(h, "claude")))
    );
    let state: String = sqlx::query_scalar("select state from inputs where id=$1")
        .bind(second)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "queued");
    let execution: String = sqlx::query_scalar("select state from executions where id=$1")
        .bind(run.execution.execution_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(execution, "running");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reserve_entry_withdraws_a_dispatched_untransmitted_input(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let waiting = env.conversation(false, true).await;
    let transmitting = env.conversation(false, true).await;
    let mut commands = HashMap::new();
    for id in [waiting, transmitting] {
        env.submit(id, &format!("input-{id}")).await;
        let DispatchOutcome::Dispatched { command_id, .. } =
            dispatch_next(&pool, id, gate(), Uuid::now_v7())
                .await
                .unwrap()
        else {
            panic!("dispatched")
        };
        commands.insert(id, command_id);
    }
    receipts::apply_input_receipt(
        &pool,
        commands[&transmitting],
        1,
        InputReceipt::TransmissionStarted,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let now = start();
    env.observe(
        "claude",
        Window::Weekly,
        960,
        Some(now + TimeDelta::days(2)),
        now,
    )
    .await;
    env.tick(now).await;
    let rule = &jobs(&pool, "limit_rule").await[0];
    assert_eq!(rule.state, "fired");
    assert_eq!(rule.result.as_ref().unwrap()["withdrawn"], json!([waiting]));
    let withdraw = |target: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("select count(*) from commands where kind='withdraw_input' and target_command_id=$1")
                .bind(target).fetch_one(&pool).await.unwrap()
        }
    };
    assert_eq!(withdraw(commands[&waiting]).await, 1);
    assert_eq!(withdraw(commands[&transmitting]).await, 0);
    for id in [waiting, transmitting] {
        assert!(has_weekly(&holds(&pool, id).await, "claude"));
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn weekly_reserve_hold_covers_all_sessions_of_the_provider_only(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let crunch = env.conversation(true, true).await;
    let plain = env.conversation(false, false).await;
    let closed = env.conversation(false, false).await;
    sqlx::query("update queue_state set closed=true where conversation_id=$1")
        .bind(closed)
        .execute(&pool)
        .await
        .unwrap();
    let codex = env.codex_conversation(true, true).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
        .await;
    env.tick(now).await;
    for id in [crunch, plain] {
        assert!(
            holds(&pool, id)
                .await
                .contains(&weekly_hold("claude", Some(weekly_reset)))
        );
    }
    assert!(!has_weekly(&holds(&pool, closed).await, "claude"));
    assert!(holds(&pool, codex).await.is_empty());
    let checks = pending(&pool, "limit_check").await;
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].payload["provider"], "claude");
    assert_eq!(checks[0].run_at, weekly_reset + secs(20));
    let rule = &jobs(&pool, "limit_rule").await[0];
    let mut entered: Vec<Uuid> =
        serde_json::from_value(rule.result.as_ref().unwrap()["entered"].clone()).unwrap();
    entered.sort();
    let mut expected = vec![crunch, plain];
    expected.sort();
    assert_eq!(entered, expected);
    // Повторное наблюдение не дублирует удержания и проверку.
    env.observe(
        "claude",
        Window::Weekly,
        970,
        Some(weekly_reset),
        now + secs(60),
    )
    .await;
    env.tick(now + secs(60)).await;
    for id in [crunch, plain] {
        assert_eq!(
            holds(&pool, id)
                .await
                .iter()
                .filter(|h| matches!(h, HoldReason::WeeklyReserve { .. }))
                .count(),
            1
        );
    }
    assert_eq!(pending(&pool, "limit_check").await.len(), 1);
    env.submit(codex, "codex-input").await;
    assert!(matches!(
        dispatch_next(&pool, codex, gate(), Uuid::now_v7())
            .await
            .unwrap(),
        DispatchOutcome::Dispatched { .. }
    ));
}

fn new_conversation(env: &Env, start_execution: bool) -> NewConversation {
    NewConversation {
        owner_id: env.owner,
        machine_id: env.claude,
        harness: "synthetic".into(),
        workdir: "/tmp/w".into(),
        model: "synthetic-small".into(),
        effort: "low".into(),
        permission_mode: None,
        instruction_profile: None,
        title: None,
        color: None,
        group_ids: vec![],
        crunch: CrunchSetting::default(),
        start: start_execution,
        resume_native_id: None,
        parent: None,
        worktree_source: None,
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn new_conversation_created_during_reserve_starts_held(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
        .await;
    env.tick(now).await;
    let hold = weekly_hold("claude", Some(weekly_reset));
    for (key, start_execution) in [("create-idle", false), ("create-start", true)] {
        let created = conversations::create(
            &pool,
            key,
            new_conversation(&env, start_execution),
            &env.config,
            Uuid::now_v7(),
            &env.registry,
        )
        .await
        .unwrap();
        let holds_now = holds(&pool, created.conversation_id).await;
        assert!(holds_now.contains(&hold), "{key}: {holds_now:?}");
        let derived: Value = sqlx::query_scalar(
            "select hold_reasons from conversation_derived where conversation_id=$1",
        )
        .bind(created.conversation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            derived
                .as_array()
                .unwrap()
                .iter()
                .any(|h| h["reason"] == "weekly_reserve")
        );
    }
    // Разговор, появившийся мимо правила, получает удержание при возобновлении.
    let raw = testkit::seed_conversation(&pool, env.owner, env.claude).await;
    sqlx::query("update conversations set model='synthetic-small',effort='low' where id=$1")
        .bind(raw)
        .execute(&pool)
        .await
        .unwrap();
    executions::start_with_key(
        &pool,
        env.owner,
        raw,
        "resume-raw",
        (true, false),
        Uuid::now_v7(),
        #[cfg(feature = "fault-injection")]
        &Default::default(),
    )
    .await
    .unwrap();
    assert!(holds(&pool, raw).await.contains(&hold));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn create_racing_reserve_entry_always_ends_held(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let base = start();
    let weekly_reset = base + TimeDelta::days(5);
    env.observe("claude", Window::Weekly, 500, Some(weekly_reset), base)
        .await;
    let (mut initial, mut by_rule) = (0, 0);
    for round in 0..100i64 {
        sqlx::query("update provider_limits set weekly_reserve_active=false,weekly_reserve_resets_at=null,weekly_used_permille=500,weekly_low_count=0,weekly_low_at=null where provider='claude'")
            .execute(&pool).await.unwrap();
        let at = base + TimeDelta::minutes(round + 1);
        let jitter = Uuid::now_v7().as_u128() as u64;
        let (early, late) = (jitter % 800, 2000 + (jitter >> 16) % 4000);
        let (observe_delay, create_delay) = if round % 2 == 0 {
            (early, late)
        } else {
            (late, early)
        };
        let observed = std::sync::atomic::AtomicBool::new(false);
        let observe = async {
            tokio::time::sleep(Duration::from_micros(observe_delay)).await;
            env.observe("claude", Window::Weekly, 960, Some(weekly_reset), at)
                .await;
            observed.store(true, std::sync::atomic::Ordering::Release);
        };
        let create = async {
            tokio::time::sleep(Duration::from_micros(create_delay)).await;
            conversations::create(
                &pool,
                &format!("race-{round}"),
                new_conversation(&env, false),
                &env.config,
                Uuid::now_v7(),
                &env.registry,
            )
            .await
            .unwrap()
            .conversation_id
        };
        // Правило резерва тоже гонится с созданием: планировщик крутится, пока задание не выполнено.
        let rule = async {
            for _ in 0..20_000 {
                env.scheduler.tick(at).await.unwrap();
                if observed.load(std::sync::atomic::Ordering::Acquire)
                    && pending(&pool, "limit_rule").await.is_empty()
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
            panic!("the reserve rule never ran");
        };
        let ((), id, ()) = tokio::join!(observe, create, rule);
        env.tick(at).await;
        assert!(
            has_weekly(&holds(&pool, id).await, "claude"),
            "round {round}"
        );
        let created: Value = sqlx::query_scalar(
            "select payload from events where kind='conversation_created' and conversation_id=$1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        if created["queue"]["hold_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["reason"] == "weekly_reserve")
        {
            initial += 1;
        } else {
            by_rule += 1;
        }
    }
    assert!(initial > 0, "creation after the reserve never happened");
    assert!(by_rule > 0, "creation before the reserve never happened");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reserve_override_continues_one_session_only(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let first = env.conversation(true, true).await;
    let second = env.conversation(true, true).await;
    let codex = env.codex_conversation(true, true).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
        .await;
    env.tick(now).await;
    env.triggered();
    assert_eq!(
        env.spend(first, "spend-1", now + secs(10)).await,
        SpendOutcome::Continued
    );
    assert!(env.triggered().contains(&first));
    assert!(!has_weekly(&holds(&pool, first).await, "claude"));
    assert_eq!(override_until(&pool, first).await, Some(weekly_reset));
    assert!(has_weekly(&holds(&pool, second).await, "claude"));
    assert_eq!(override_until(&pool, second).await, None);
    assert_eq!(
        env.spend(first, "spend-1", now + secs(11)).await,
        SpendOutcome::Continued,
        "same key replays"
    );
    assert_eq!(
        env.spend(first, "spend-2", now + secs(12)).await,
        SpendOutcome::AlreadyContinued
    );
    assert_eq!(
        env.spend(codex, "spend-codex", now + secs(13)).await,
        SpendOutcome::NotHeld
    );
    for id in [first, second] {
        env.submit(id, &format!("turn-{id}")).await;
    }
    assert!(matches!(
        dispatch_next(&pool, first, gate(), Uuid::now_v7())
            .await
            .unwrap(),
        DispatchOutcome::Dispatched { .. }
    ));
    assert!(matches!(
        dispatch_next(&pool, second, gate(), Uuid::now_v7()).await.unwrap(),
        DispatchOutcome::Blocked(reasons) if reasons.iter().any(|r| matches!(r, BlockReason::Held(h) if has_weekly(h, "claude")))
    ));
    // Правило при новом наблюдении не возвращает удержание переопределённой сессии.
    env.observe(
        "claude",
        Window::Weekly,
        965,
        Some(weekly_reset),
        now + secs(60),
    )
    .await;
    env.tick(now + secs(60)).await;
    assert!(!has_weekly(&holds(&pool, first).await, "claude"));
    // Переопределение действует до сброса недели.
    env.observe(
        "claude",
        Window::Weekly,
        20,
        Some(weekly_reset + TimeDelta::days(7)),
        weekly_reset + secs(30),
    )
    .await;
    env.tick(weekly_reset + secs(60)).await;
    assert!(!reserve(&pool, "claude").await.0);
    assert_eq!(override_until(&pool, first).await, None);
    assert!(!has_weekly(&holds(&pool, second).await, "claude"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn weekly_reset_wakes_only_sessions_cut_by_a_limit(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let idle_a = env.conversation(true, true).await;
    let idle_b = env.conversation(true, true).await;
    let cut = env.conversation(true, true).await;
    // У сессии идёт ход: резерв его не прерывает.
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2 where conversation_id=$1",
    )
    .bind(cut)
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
        .await;
    env.tick(now).await;
    for id in [idle_a, idle_b, cut] {
        assert!(has_weekly(&holds(&pool, id).await, "claude"));
    }
    // Идущий ход упёрся в пятичасовой лимит внутри резерва.
    let reset = now + TimeDelta::hours(2);
    env.limit_reached(
        cut,
        "claude",
        Window::FiveHour,
        Some(reset),
        now + TimeDelta::hours(1),
    )
    .await;
    verified_reset(&env, reset).await;
    env.tick(reset + secs(60)).await;
    assert!(!has_quota(&holds(&pool, cut).await));
    assert!(wake_texts(&pool, cut).await.is_empty());
    assert!(resume_pending(&pool, cut).await);
    env.observe(
        "claude",
        Window::Weekly,
        10,
        Some(weekly_reset + TimeDelta::days(7)),
        weekly_reset + secs(30),
    )
    .await;
    env.tick(weekly_reset + secs(60)).await;
    for id in [idle_a, idle_b, cut] {
        assert!(!has_weekly(&holds(&pool, id).await, "claude"));
    }
    assert_eq!(
        wake_texts(&pool, cut).await,
        vec![LimitsConfig::default().default_resume_text]
    );
    assert!(wake_texts(&pool, idle_a).await.is_empty());
    assert!(wake_texts(&pool, idle_b).await.is_empty());
    assert!(!resume_pending(&pool, cut).await);
    let rule = jobs(&pool, "limit_rule")
        .await
        .into_iter()
        .rfind(|j| j.result.as_ref().is_some_and(|r| r["left"] != json!([])))
        .unwrap();
    assert_eq!(rule.result.unwrap()["woken"], json!([cut]));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reset_wakes_all_limit_stopped_crunch_sessions_at_once(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let mut stopped = Vec::new();
    for _ in 0..3 {
        stopped.push(env.conversation(true, true).await);
    }
    let non_crunch = env.conversation(false, true).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
        .await;
    env.tick(now).await;
    // Сессия создана мимо правила: удержания нет.
    let unheld = env.conversation(true, true).await;
    assert!(!has_weekly(&holds(&pool, unheld).await, "claude"));
    let mut all = stopped.clone();
    all.extend([non_crunch, unheld]);
    set_resume_pending(&pool, &all).await;
    env.observe(
        "claude",
        Window::Weekly,
        10,
        Some(weekly_reset + TimeDelta::days(7)),
        weekly_reset + secs(30),
    )
    .await;
    env.tick(weekly_reset + secs(60)).await;
    for id in &stopped {
        assert_eq!(wake_texts(&pool, *id).await.len(), 1, "{id}");
        assert!(!resume_pending(&pool, *id).await);
    }
    for id in [non_crunch, unheld] {
        assert!(wake_texts(&pool, id).await.is_empty());
    }
    assert_eq!(jobs(&pool, "wake").await.len(), 3);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn weekly_reset_wake_is_not_cancelled_by_the_removed_weekly_hold(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let idle = env.conversation(true, true).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
        .await;
    env.tick(now).await;
    set_resume_pending(&pool, &[id]).await;
    env.observe(
        "claude",
        Window::Weekly,
        10,
        Some(weekly_reset + TimeDelta::days(7)),
        weekly_reset + secs(30),
    )
    .await;
    env.tick(weekly_reset + secs(60)).await;
    let wake = &jobs(&pool, "wake").await[0];
    assert_eq!(wake.conversation_id, Some(id));
    assert_eq!(wake.payload["verified"], true);
    assert_eq!(wake.payload["hold_key"], "weekly:claude");
    assert_eq!(wake.state, "fired");
    assert!(wake.result.as_ref().unwrap()["input_id"].is_string());
    assert_eq!(wake_texts(&pool, id).await.len(), 1);
    // Пробуждение после сброса без resume_pending отменяется, удержания не ищется.
    let mut tx = pool.begin().await.unwrap();
    let job_id = Uuid::now_v7();
    sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) values($1,'wake',$2,$3,$4,$5)")
        .bind(job_id).bind(env.owner).bind(idle).bind(weekly_reset)
        .bind(json!({"provider":"claude","window":"weekly","hold_key":"weekly:claude","verified":true}))
        .execute(&mut *tx).await.unwrap();
    let outcome = crunch::handle_wake_in_tx(
        &mut tx,
        &ClaimedJob {
            id: job_id,
            kind: JobKind::Wake,
            owner_id: env.owner,
            conversation_id: Some(idle),
            run_at: weekly_reset,
            payload: json!({"provider":"claude","window":"weekly","hold_key":"weekly:claude","verified":true}),
            attempts: 0,
        },
        &env.config,
        weekly_reset + secs(70),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(outcome, JobOutcome::Cancelled("not_resumable"));
    tx.rollback().await.unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reading_oscillating_around_95_percent_does_not_wake_crunch_sessions(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    set_resume_pending(&pool, &[id]).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    let readings = [
        (949, ObservationSource::Session),
        (951, ObservationSource::Poll),
        (948, ObservationSource::Session),
        (952, ObservationSource::Poll),
    ];
    for (n, (used, source)) in readings.into_iter().enumerate() {
        let at = now + TimeDelta::minutes(n as i64);
        env.observe_full(
            "claude",
            Window::Weekly,
            used,
            Some(weekly_reset),
            at,
            at,
            source,
        )
        .await;
        env.tick(at).await;
        let (active, _, low) = reserve(&pool, "claude").await;
        assert_eq!(active, n >= 1, "after reading {used}");
        assert_eq!(low, 0);
    }
    assert!(jobs(&pool, "wake").await.is_empty());
    assert!(wake_texts(&pool, id).await.is_empty());
    assert!(has_weekly(&holds(&pool, id).await, "claude"));
    assert!(resume_pending(&pool, id).await);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stale_replay_never_clears_the_reserve(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    set_resume_pending(&pool, &[id]).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    env.observe(
        "claude",
        Window::Weekly,
        960,
        Some(weekly_reset),
        now + TimeDelta::minutes(10),
    )
    .await;
    env.tick(now + TimeDelta::minutes(10)).await;
    for (n, resets) in [
        Some(weekly_reset),
        Some(weekly_reset),
        Some(weekly_reset + TimeDelta::days(7)),
    ]
    .into_iter()
    .enumerate()
    {
        let old = now + TimeDelta::minutes(n as i64);
        let recorded = env
            .observe_full(
                "claude",
                Window::Weekly,
                100,
                resets,
                old,
                now + TimeDelta::minutes(20),
                ObservationSource::Poll,
            )
            .await;
        assert!(recorded.stale);
    }
    env.tick(now + TimeDelta::minutes(30)).await;
    let (active, resets_at, low) = reserve(&pool, "claude").await;
    assert!(active);
    assert_eq!(resets_at, Some(weekly_reset));
    assert_eq!(low, 0);
    let used: i32 = sqlx::query_scalar(
        "select weekly_used_permille from provider_limits where provider='claude'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(used, 960);
    assert!(jobs(&pool, "wake").await.is_empty());
    assert!(has_weekly(&holds(&pool, id).await, "claude"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn providers_are_independent(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let claude = env.conversation(true, true).await;
    let codex = env.codex_conversation(true, true).await;
    let now = start();
    env.observe(
        "claude",
        Window::Weekly,
        960,
        Some(now + TimeDelta::days(2)),
        now,
    )
    .await;
    env.tick(now).await;
    assert!(has_weekly(&holds(&pool, claude).await, "claude"));
    assert!(holds(&pool, codex).await.is_empty());
    let reset = now + TimeDelta::hours(1);
    env.limit_reached(codex, "codex", Window::FiveHour, Some(reset), now)
        .await;
    // Наблюдение Claude не подтверждает пробуждение Codex.
    verified_reset(&env, reset).await;
    let report = env.tick(reset + secs(60)).await;
    assert_eq!(report.retried, 1);
    assert!(has_quota(&holds(&pool, codex).await));
    assert_eq!(env.take_polls(), vec![(env.codex, "codex".to_string())]);
    env.submit(codex, "codex-turn").await;
    let DispatchOutcome::Blocked(reasons) = dispatch_next(&pool, codex, gate(), Uuid::now_v7())
        .await
        .unwrap()
    else {
        panic!("held by its own quota")
    };
    assert!(
        !reasons
            .iter()
            .any(|r| matches!(r, BlockReason::Held(h) if has_weekly(h, "claude")))
    );
    env.observe(
        "codex",
        Window::FiveHour,
        10,
        Some(reset + TimeDelta::hours(5)),
        reset + secs(120),
    )
    .await;
    env.tick(reset + secs(180)).await;
    assert!(!has_quota(&holds(&pool, codex).await));
    // Ввод владельца уже стоит в очереди: продолжением становится он.
    assert!(wake_texts(&pool, codex).await.is_empty());
    assert!(!resume_pending(&pool, codex).await);
    assert!(has_weekly(&holds(&pool, claude).await, "claude"));
    assert!(wake_texts(&pool, claude).await.is_empty());
}

#[cfg(feature = "fault-injection")]
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn wake_is_exactly_once_under_two_schedulers(pool: PgPool) {
    use canterlot_hub::fault_injection::Point;
    let env = Env::new(&pool).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    let mut ids = Vec::new();
    for _ in 0..12 {
        let id = env.conversation(true, true).await;
        env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
            .await;
        ids.push(id);
    }
    verified_reset(&env, reset).await;
    let before = deadlocks(&pool).await;
    let first = env.new_scheduler();
    let second = env.new_scheduler();
    let one = first.faults.pause_next(Point::WakeBeforeSubmit);
    let two = second.faults.pause_next(Point::WakeBeforeSubmit);
    let due = reset + secs(60);
    let run = |s: Scheduler| {
        tokio::spawn(async move {
            let mut fired = 0;
            for _ in 0..6 {
                fired += s.tick(due).await.unwrap().fired;
            }
            fired
        })
    };
    let a = run(first);
    let b = run(second);
    let (ra, rb) = tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(one, two) })
        .await
        .unwrap();
    ra.unwrap().send(()).unwrap();
    rb.unwrap().send(()).unwrap();
    let (a, b) = tokio::time::timeout(Duration::from_secs(30), async { tokio::join!(a, b) })
        .await
        .unwrap();
    let (a, b) = (a.unwrap(), b.unwrap());
    assert!(a >= 1 && b >= 1, "{a} {b}");
    assert_eq!(a + b, 12);
    for id in &ids {
        assert_eq!(wake_texts(&pool, *id).await.len(), 1);
    }
    let keys: i64 =
        sqlx::query_scalar("select count(*) from idempotency_keys where key like 'wake:%'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(keys, 12);
    assert_eq!(
        jobs(&pool, "wake")
            .await
            .iter()
            .filter(|j| j.state == "fired")
            .count(),
        12
    );
    assert_eq!(deadlocks(&pool).await, before);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn two_wakes_for_one_reset_queue_one_resume(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    // Наблюдение сброса приходит позже, чем пробуждение по окну: оба порядка.
    for (late, offset) in [(false, 5), (true, 25)] {
        sqlx::query("delete from scheduled_jobs")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("delete from provider_limits")
            .execute(&pool)
            .await
            .unwrap();
        let id = env.conversation(true, true).await;
        env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
            .await;
        env.tick(now).await;
        env.limit_reached(
            id,
            "claude",
            Window::Weekly,
            Some(weekly_reset),
            now + TimeDelta::hours(1),
        )
        .await;
        env.observe_full(
            "claude",
            Window::Weekly,
            10,
            Some(weekly_reset + TimeDelta::days(7)),
            weekly_reset + secs(5),
            weekly_reset + secs(offset),
            ObservationSource::Session,
        )
        .await;
        env.tick(weekly_reset + secs(60)).await;
        assert_eq!(wake_texts(&pool, id).await.len(), 1, "late={late}");
        assert!(holds(&pool, id).await.is_empty(), "late={late}");
        assert!(!resume_pending(&pool, id).await);
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn hub_restart_between_hold_and_wake_still_wakes_once(pool: PgPool) {
    let env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    // Хаб перезапущен: новый планировщик видит сохранённое задание.
    let restarted = env.new_scheduler();
    assert_eq!(restarted.tick(reset).await.unwrap().fired, 0);
    verified_reset(&env, reset).await;
    // Сбой базы в момент пробуждения откатывает всё.
    sqlx::query("alter table scheduled_jobs add constraint forbid_fired check (state <> 'fired')")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(restarted.tick(reset + secs(60)).await.unwrap().retried, 1);
    assert_eq!(inputs(&pool, id).await, 0);
    assert!(has_quota(&holds(&pool, id).await));
    assert!(resume_pending(&pool, id).await);
    sqlx::query("alter table scheduled_jobs drop constraint forbid_fired")
        .execute(&pool)
        .await
        .unwrap();
    drop(restarted);
    let again = env.new_scheduler();
    assert_eq!(again.tick(reset + secs(120)).await.unwrap().fired, 1);
    assert_eq!(again.tick(reset + secs(180)).await.unwrap().fired, 0);
    assert_eq!(
        env.new_scheduler()
            .tick(reset + secs(240))
            .await
            .unwrap()
            .fired,
        0
    );
    assert_eq!(wake_texts(&pool, id).await.len(), 1);
    assert_eq!(inputs(&pool, id).await, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limit_check_notices_weekly_reset_without_live_sessions(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, false).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(2);
    env.observe("claude", Window::Weekly, 960, Some(weekly_reset), now)
        .await;
    env.tick(now).await;
    assert!(env.take_polls().is_empty());
    let report = env.tick(weekly_reset + secs(20)).await;
    assert_eq!(report.retried, 1);
    assert_eq!(env.take_polls(), vec![(env.claude, "claude".to_string())]);
    let check = &pending(&pool, "limit_check").await[0];
    assert_eq!(check.attempts, 1);
    assert_eq!(check.run_at, weekly_reset + secs(50));
    env.observe_full(
        "claude",
        Window::Weekly,
        5,
        Some(weekly_reset + TimeDelta::days(7)),
        weekly_reset + secs(25),
        weekly_reset + secs(26),
        ObservationSource::Poll,
    )
    .await;
    env.tick(weekly_reset + secs(50)).await;
    assert!(!reserve(&pool, "claude").await.0);
    assert!(
        holds(&pool, id)
            .await
            .iter()
            .all(|h| !matches!(h, HoldReason::WeeklyReserve { .. }))
    );
    let checks = jobs(&pool, "limit_check").await;
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].state, "fired");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn poll_requests_are_rate_limited_per_provider_and_skip_unavailable_machines(pool: PgPool) {
    let owner = Uuid::now_v7();
    sqlx::query("insert into owners(id) values($1)")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let registry = Registry::new(timers());
    // Машины без pilot_v1 и без потока данных идут раньше по UUID и должны пропускаться.
    let legacy = new_machine(&pool, owner, "claude").await;
    let degraded = new_machine(&pool, owner, "claude").await;
    let claude = new_machine(&pool, owner, "claude").await;
    let codex = new_machine(&pool, owner, "codex").await;
    let mut legacy_s = testkit::fake_session(&registry, legacy, &[], true);
    let mut degraded_s = testkit::fake_session(&registry, degraded, &["pilot_v1"], false);
    let mut claude_s = testkit::fake_session(&registry, claude, &["pilot_v1"], true);
    let mut codex_s = testkit::fake_session(&registry, codex, &["pilot_v1"], true);
    let polls = |s: &mut FakeSession| {
        let mut out = Vec::new();
        while let Ok(m) = s.commands.try_recv() {
            if let Some(control_down::Msg::PollLimits(p)) = m.msg {
                out.push(p.provider);
            }
        }
        out
    };
    let five = Duration::from_secs(300);
    let now = start();
    assert!(
        crunch::request_poll(&pool, &registry, "claude", five, now)
            .await
            .unwrap()
    );
    assert_eq!(polls(&mut claude_s), vec!["claude"]);
    assert!(polls(&mut legacy_s).is_empty());
    assert!(polls(&mut degraded_s).is_empty());
    assert!(
        !crunch::request_poll(
            &pool,
            &registry,
            "claude",
            five,
            now + TimeDelta::minutes(1)
        )
        .await
        .unwrap()
    );
    assert!(polls(&mut claude_s).is_empty());
    assert!(
        crunch::request_poll(&pool, &registry, "codex", five, now + TimeDelta::minutes(1))
            .await
            .unwrap()
    );
    assert_eq!(polls(&mut codex_s), vec!["codex"]);
    assert!(
        crunch::request_poll(
            &pool,
            &registry,
            "claude",
            five,
            now + TimeDelta::minutes(5)
        )
        .await
        .unwrap()
    );
    assert_eq!(polls(&mut claude_s), vec!["claude"]);
    drop(claude_s);
    assert!(
        !crunch::request_poll(
            &pool,
            &registry,
            "claude",
            five,
            now + TimeDelta::minutes(20)
        )
        .await
        .unwrap()
    );
    let last: DateTime<Utc> =
        sqlx::query_scalar("select last_requested_at from limit_polls where provider='claude'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        last,
        now + TimeDelta::minutes(5),
        "no machine, no slot used"
    );
    assert!(polls(&mut legacy_s).is_empty());
    assert!(polls(&mut degraded_s).is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn disabling_crunch_turns_quota_hold_into_limit_pause_and_cancels_wakes(pool: PgPool) {
    let env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    let detail = conversations::set_crunch(
        &pool,
        env.owner,
        id,
        "crunch-off",
        CrunchSetting {
            enabled: false,
            resume_text: None,
        },
        &env.config,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(!detail.crunch.resume_pending);
    let (paused_now, holds_now, _) = queue(&pool, id).await;
    assert!(!has_quota(&holds_now));
    assert_eq!(
        paused_now,
        vec![PauseReason::LimitReached {
            provider: Some("claude".into()),
            window: Some("five_hour".into()),
            resets_at: Some(reset),
        }]
    );
    assert!(!resume_pending(&pool, id).await);
    let wakes = jobs(&pool, "wake").await;
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].state, "cancelled");
    let changed = events_of(&pool, id, "crunch_changed").await;
    assert_eq!(changed.last().unwrap()["resume_pending"], false);
    assert_eq!(changed.last().unwrap()["enabled"], false);
    // Включение не трогает существующие паузы.
    conversations::set_crunch(
        &pool,
        env.owner,
        id,
        "crunch-on",
        CrunchSetting {
            enabled: true,
            resume_text: None,
        },
        &env.config,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(pauses(&pool, id).await.len(), 1);
    #[cfg(feature = "fault-injection")]
    {
        use canterlot_hub::fault_injection::Point;
        let racing = env.conversation(true, true).await;
        env.limit_reached(racing, "claude", Window::FiveHour, Some(reset), now)
            .await;
        verified_reset(&env, reset).await;
        let scheduler = env.new_scheduler();
        let paused = scheduler.faults.pause_next(Point::WakeBeforeSubmit);
        let tick = {
            let scheduler = scheduler.clone();
            tokio::spawn(async move { scheduler.tick(reset + secs(60)).await.unwrap() })
        };
        let resume = tokio::time::timeout(Duration::from_secs(10), paused)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            conversations::set_crunch(
                &pool,
                env.owner,
                racing,
                "racing-off",
                CrunchSetting {
                    enabled: false,
                    resume_text: None,
                },
                &env.config,
                Uuid::now_v7(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        resume.send(()).unwrap();
        let report = tick.await.unwrap();
        assert_eq!(report.cancelled, 1);
        let job = jobs(&pool, "wake")
            .await
            .into_iter()
            .find(|j| j.conversation_id == Some(racing))
            .unwrap();
        assert_eq!(job.state, "cancelled");
        assert_eq!(job.result.unwrap()["reason"], "hold_gone");
        assert_eq!(inputs(&pool, racing).await, 0);
        assert!(matches!(
            pauses(&pool, racing).await.as_slice(),
            [PauseReason::LimitReached { .. }]
        ));
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lock_order_has_no_deadlocks_across_ingest_dispatch_scheduler_and_http(pool: PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(24)
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let mut run = worker::Run::new(&pool).await;
    let env = Env::attach(
        &pool,
        run.seed.owner_id,
        run.seed.machine_id,
        LimitsConfig::default(),
    )
    .await;
    run.ingest = Ingest::new(pool.clone())
        .with_registry(env.registry.clone())
        .with_limits(env.config.clone());
    let wake_target = env.conversation(true, true).await;
    let spender = env.conversation(true, true).await;
    let settings = env.conversation(false, true).await;
    let stopper = env.conversation(false, true).await;
    let base = start();
    let far = base + TimeDelta::days(10);
    sqlx::query("insert into provider_limits(provider,five_hour_used_permille,five_hour_resets_at,five_hour_observed_at) values('claude',10,$1,$2) on conflict(provider) do update set five_hour_used_permille=10,five_hour_resets_at=$1,five_hour_observed_at=$2")
        .bind(far).bind(base).execute(&pool).await.unwrap();
    let scheduler = env.new_scheduler();
    let thresholds = activity::Thresholds {
        stalled_generating: Duration::from_secs(300),
        stalled_tool: Duration::from_secs(1800),
        idle_quiet: Duration::from_secs(5),
        idle_background_max: std::time::Duration::from_secs(15 * 60),
    };
    let machine_ms = Utc::now().timestamp_millis();
    let before = deadlocks(&pool).await;
    for round in 0..100i64 {
        sqlx::query("update provider_limits set weekly_reserve_active=false,weekly_reserve_resets_at=null,weekly_used_permille=900,weekly_low_count=0 where provider='claude'")
            .execute(&pool).await.unwrap();
        sqlx::query("update conversations set crunch_enabled=true where id=$1")
            .bind(wake_target)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("update queue_state set pause_reasons='[]' where conversation_id=any($1)")
            .bind(vec![wake_target, stopper])
            .execute(&pool)
            .await
            .unwrap();
        let now = base + TimeDelta::minutes(round + 1);
        env.limit_reached(
            wake_target,
            "claude",
            Window::FiveHour,
            Some(now - TimeDelta::hours(1)),
            now - TimeDelta::hours(2),
        )
        .await;
        let fresh = env.conversation(false, true).await;
        let block = Uuid::now_v7();
        let batch = vec![
            worker::started(block, "text"),
            Body::BlockDelta(BlockDelta {
                block_id: block.to_string(),
                base_rev: 0,
                rev: 1,
                append: "x".into(),
            }),
            Body::BlockDelta(BlockDelta {
                block_id: block.to_string(),
                base_rev: 1,
                rev: 2,
                append: "y".into(),
            }),
            Body::LimitObserved(LimitObservation {
                provider: "claude".into(),
                window: LimitWindow::Weekly as i32,
                used_permille: 960,
                resets_at_ms: far.timestamp_millis() as u64,
                observed_machine_ms: (machine_ms + (round + 1) * 60_000) as u64,
                ..Default::default()
            }),
        ];
        let owner = env.owner;
        let effort = if round % 2 == 0 { "high" } else { "low" };
        let (crunch_key, spend_key, settings_key, stop_key) = (
            format!("crunch-off-{round}"),
            format!("spend-{round}"),
            format!("settings-{round}"),
            format!("stop-{round}"),
        );
        let (ingested, submitted, crunch_off, ticked, spent, changed, refreshed, stopped) = tokio::join!(
            async {
                run.send(batch).await;
                Ok::<_, ()>(())
            },
            conversations::submit_message(
                &pool,
                SubmitMessage {
                    owner_id: owner,
                    conversation_id: fresh,
                    key: format!("auto-title-{round}"),
                    content: text("Первая задача раунда"),
                    attachment_ids: vec![],
                    retry_of: None,
                    received_at: Utc::now(),
                },
                Uuid::now_v7(),
            ),
            conversations::set_crunch(
                &pool,
                owner,
                wake_target,
                &crunch_key,
                CrunchSetting {
                    enabled: false,
                    resume_text: None,
                },
                &env.config,
                Uuid::now_v7(),
            ),
            scheduler.tick(now),
            crunch::spend_reserve(
                &pool,
                owner,
                spender,
                &spend_key,
                now,
                &env.dispatcher,
                Uuid::now_v7(),
            ),
            conversations::change_settings(
                &pool,
                owner,
                settings,
                &settings_key,
                SettingsPatch {
                    effort: Some(effort.into()),
                    ..Default::default()
                },
                Uuid::now_v7(),
            ),
            async {
                for id in [wake_target, spender, settings] {
                    activity::refresh(&pool, id, Utc::now(), &thresholds, Uuid::now_v7()).await?;
                }
                Ok::<_, sqlx::Error>(())
            },
            control::request_stop(&pool, owner, stopper, &stop_key, Uuid::now_v7()),
        );
        assert!(ingested.is_ok(), "round {round}");
        assert!(
            submitted.is_ok(),
            "round {round}: {:?}",
            submitted.as_ref().err()
        );
        assert!(
            crunch_off.is_ok(),
            "round {round}: {:?}",
            crunch_off.as_ref().err()
        );
        assert!(ticked.is_ok(), "round {round}: {:?}", ticked.as_ref().err());
        assert!(spent.is_ok(), "round {round}: {:?}", spent.as_ref().err());
        assert!(
            changed.is_ok(),
            "round {round}: {:?}",
            changed.as_ref().err()
        );
        assert!(
            refreshed.is_ok(),
            "round {round}: {:?}",
            refreshed.as_ref().err()
        );
        assert!(
            stopped.is_ok(),
            "round {round}: {:?}",
            stopped.as_ref().err()
        );
    }
    assert_eq!(deadlocks(&pool).await, before);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn final_wave_owner_stop_prevents_resume_at_later_weekly_reset(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let reset = now + TimeDelta::hours(1);
    env.limit_reached(id, "claude", Window::FiveHour, Some(reset), now)
        .await;
    control::request_stop(&pool, env.owner, id, "stop", Uuid::now_v7())
        .await
        .unwrap();
    verified_reset(&env, reset).await;
    env.tick(reset + secs(60)).await;
    control::continue_queue(&pool, env.owner, id, "continue", Uuid::now_v7())
        .await
        .unwrap();
    let week = now + TimeDelta::days(3);
    env.observe(
        "claude",
        Window::Weekly,
        960,
        Some(week),
        now + TimeDelta::days(1),
    )
    .await;
    env.tick(now + TimeDelta::days(1)).await;
    env.observe(
        "claude",
        Window::Weekly,
        10,
        Some(week + TimeDelta::days(7)),
        week + secs(1),
    )
    .await;
    env.tick(week + secs(1)).await;
    assert!(!resume_pending(&pool, id).await);
    assert!(wake_texts(&pool, id).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn final_wave_owner_execution_stop_clears_resume_pending(pool: PgPool) {
    let env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    env.limit_reached(id, "claude", Window::FiveHour, Some(now + secs(3600)), now)
        .await;
    control::request_stop_execution(
        &pool,
        env.owner,
        id,
        false,
        "stop-execution",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(!resume_pending(&pool, id).await);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn final_wave_pending_resume_expires_at_end_of_next_weekly_window(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let id = env.conversation(true, true).await;
    let now = start();
    let week = now + TimeDelta::days(1);
    env.observe("claude", Window::Weekly, 960, Some(week), now)
        .await;
    env.tick(now).await;
    env.limit_reached(id, "claude", Window::FiveHour, Some(now + secs(3600)), now)
        .await;
    sqlx::query(
        "update executions set state='stopped',outcome='normal_exit' where conversation_id=$1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    verified_reset(&env, now + secs(3600)).await;
    env.tick(now + secs(3660)).await;
    assert!(resume_pending(&pool, id).await);
    env.observe(
        "claude",
        Window::Weekly,
        960,
        Some(week + TimeDelta::days(7)),
        week + secs(1),
    )
    .await;
    env.tick(week + secs(1)).await;
    assert!(resume_pending(&pool, id).await);
    let expiry = week + TimeDelta::days(7);
    env.observe(
        "claude",
        Window::Weekly,
        10,
        Some(expiry + TimeDelta::days(7)),
        expiry + secs(1),
    )
    .await;
    env.tick(expiry + secs(1)).await;
    assert!(!resume_pending(&pool, id).await);
    assert!(wake_texts(&pool, id).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn final_wave_limits_events_carry_reserve_window_on_entry_and_rollover(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let now = start();
    let week = now + TimeDelta::days(1);
    for (at, reset) in [(now, week), (week + secs(1), week + TimeDelta::days(7))] {
        env.observe("claude", Window::Weekly, 960, Some(reset), at)
            .await;
        env.tick(at).await;
        let payload:Value=sqlx::query_scalar("select payload from events where kind='limits_changed' order by scope_seq desc limit 1").fetch_one(&pool).await.unwrap();
        assert_eq!(
            payload["weekly_reserve_resets_at"],
            reset.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn final_wave_failed_poll_send_does_not_consume_provider_slot(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let sender = env.registry.command_sender(env.claude).unwrap();
    while sender.try_send(ControlDown::default()).is_ok() {}
    let now = start();
    assert!(
        !crunch::request_poll(
            &pool,
            &env.registry,
            "claude",
            Duration::from_secs(1800),
            now
        )
        .await
        .unwrap()
    );
    env.receivers
        .get_mut(&env.claude)
        .unwrap()
        .recv()
        .await
        .unwrap();
    assert!(
        crunch::request_poll(
            &pool,
            &env.registry,
            "claude",
            Duration::from_secs(1800),
            now + secs(1)
        )
        .await
        .unwrap()
    );
}
