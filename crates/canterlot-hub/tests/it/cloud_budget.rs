//! Бюджет облака, карточки предложений, автоматический режим и оповещения (этап C2b).
//! Всё против поддельного Timeweb: деньги и состояния проверяются в базе, а время — аргументом
//! прохода службы, без ожидания.

use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    cloud::{
        service::CloudService,
        windows::{self, Trigger},
    },
    config::CloudConfig,
    crunch::JobOutcome,
    http::{self, AppState},
    machines,
    scheduler::{ClaimedJob, JobKind},
    session::Registry,
    session_tools::{
        Caller, ToolContext,
        cloud_propose::{self, Source},
    },
    testkit::{self, Method, TestClient},
};
use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::fake_timeweb::FakeTimeweb;

const TOKEN: &str = "tw-budget-token-5d1c0e9a";
const PROJECT: i64 = 4242;
const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAMPLE0000";
const HOURLY: i64 = 2845;
const ROUTER: &str = "018f0000-0000-7000-8000-0000000000aa";
const ROUTER_IP: &str = "192.0.2.1";
const GATEWAY: &str = "192.0.2.5";

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn now_micros() -> DateTime<Utc> {
    Utc.timestamp_micros(Utc::now().timestamp_micros())
        .single()
        .unwrap()
}

fn cloud_config(carrier: Uuid) -> CloudConfig {
    toml::from_str(&format!(
        r#"
provider = "timeweb"
token_file = "token"
project_id = {PROJECT}
runner_public_key = "{KEY}"
carrier_machine_id = "{carrier}"
network_id = "network-example"
network_cidr = "192.0.2.0/24"
router_id = "{ROUTER}"
router_ip = "{ROUTER_IP}"
gateway = "{GATEWAY}"
local_ip_pool = "192.0.2.10-192.0.2.60"
dnat_port_range = "22100-22199"

[[profile]]
configurator_id = 151
cpu = 32
ram_gb = 64
disk_gb = 80
availability_zone = "msk-1"
"#
    ))
    .unwrap()
}

struct Rig {
    pool: PgPool,
    _fake: FakeTimeweb,
    service: Arc<CloudService>,
    registry: Registry,
    owner: Uuid,
    carrier: Uuid,
    config: CloudConfig,
    _dir: tempfile::TempDir,
}

impl Rig {
    async fn new(pool: PgPool) -> Self {
        Self::with_config(pool, |_| {}).await
    }

    async fn with_config(pool: PgPool, tweak: impl FnOnce(&mut CloudConfig)) -> Self {
        let owner = crate::runners_hub::owner(&pool).await;
        let carrier = machines::add(&pool, owner, "carrier").await.unwrap();
        machines::set_carrier(&pool, carrier.machine_id, true)
            .await
            .unwrap();
        let fake = FakeTimeweb::start(TOKEN).await;
        crate::auth_pushes::enable(&pool).await;
        let dir = tempfile::tempdir().unwrap();
        let token_file = dir.path().join("token");
        std::fs::write(&token_file, format!("{TOKEN}\n")).unwrap();
        let mut provider_config = canterlot_hub::cloud::TimewebConfig::new(&token_file);
        provider_config.base_url = fake.base_url.clone();
        provider_config.request_timeout = Duration::from_secs(3);
        provider_config.max_rate_limit_retries = 1;
        provider_config.default_backoff = Duration::from_millis(5);
        provider_config.max_backoff = Duration::from_millis(20);
        let provider =
            Arc::new(canterlot_hub::cloud::TimewebProvider::new(provider_config).unwrap());
        let mut config = cloud_config(carrier.machine_id);
        tweak(&mut config);
        let registry = Registry::new(Default::default());
        let service = CloudService::new(pool.clone(), registry.clone(), provider, config.clone());
        Rig {
            pool,
            _fake: fake,
            service,
            registry,
            owner,
            carrier: carrier.machine_id,
            config,
            _dir: dir,
        }
    }

    async fn set_budget(&self, kopecks: i64, auto: bool) {
        sqlx::query(
            "update cloud_budget set monthly_budget_kopecks = $1, auto_mode = $2 where singleton",
        )
        .bind(kopecks)
        .bind(auto)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn set_block(&self, code: Option<&str>) {
        sqlx::query(
            "update cloud_budget set provider_block_code=$1, provider_block_at=case when $1 is null then null else now() end where singleton",
        )
        .bind(code)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// Окно с заданным состоянием и ценой: для расчёта расхода месяца.
    async fn insert_window(&self, state: &str, final_cost: Option<i64>, reservation: i64) -> Uuid {
        let id = Uuid::now_v7();
        let month = windows::month_of(Utc::now());
        sqlx::query(
            "insert into cloud_windows (id, name, state, trigger, host_key_public, price_hourly_kopecks,
                                         reservation_kopecks, final_cost_kopecks, month,
                                         hard_cap_minutes, created_at, profiles, create_called_at)
             values ($1, 'canterlot-runner-' || $1::text, $2, 'owner', 'ssh-ed25519 AAAA', $3,
                     $4, $5, $6, 60, now(), '[{\"configurator_id\":151,\"cpu\":32,\"ram_gb\":64,\"disk_gb\":80,\"hourly_kopecks\":2845}]',
                     now())",
        )
        .bind(id)
        .bind(state)
        .bind(HOURLY)
        .bind(reservation)
        .bind(final_cost)
        .bind(&month)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    async fn seed_waiting(&self, minutes: i32) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into jobs (id, owner_id, domain, remote, shell, timeout_seconds, state,
                               labels, carrier_id, expected_minutes)
             values ($1, $2, 'personal', true, 'true', 600, 'waiting_runner',
                     array['cloud','kernel-6.12'], $3, $4)",
        )
        .bind(id)
        .bind(self.owner)
        .bind(self.carrier)
        .bind(minutes)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    async fn static_runner(&self, name: &str) -> Uuid {
        let machine = machines::add(&self.pool, self.owner, name).await.unwrap();
        sqlx::query(
            "update machines set role='runner', parent_machine_id=$2, root_dir='/srv/runner',
                                 rank=20, labels=array['cloud','kernel-6.12'], accepts_remote_work=true
              where id=$1",
        )
        .bind(machine.machine_id)
        .bind(self.carrier)
        .execute(&self.pool)
        .await
        .unwrap();
        machine.machine_id
    }

    async fn observe(&self, runner: Uuid, reachable: bool, age: TimeDelta) {
        sqlx::query(
            "insert into runner_observations (machine_id, reachable, consecutive_failures, observed_at, carrier_id)
             values ($1, $2, 3, now() - ($3::bigint * interval '1 second'), $4)
             on conflict (machine_id) do update set reachable=excluded.reachable,
               consecutive_failures=excluded.consecutive_failures, observed_at=excluded.observed_at",
        )
        .bind(runner)
        .bind(reachable)
        .bind(age.num_seconds())
        .bind(self.carrier)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// Наблюдение, свежее относительно модельного времени прохода.
    async fn observe_at(&self, runner: Uuid, at: DateTime<Utc>) {
        sqlx::query(
            "insert into runner_observations (machine_id, reachable, consecutive_failures, observed_at, carrier_id)
             values ($1, false, 3, $2, $3)
             on conflict (machine_id) do update set reachable=false,
               consecutive_failures=3, observed_at=excluded.observed_at",
        )
        .bind(runner)
        .bind(at)
        .bind(self.carrier)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn card(&self, hours: u32, reason: &str, source: Source) -> Uuid {
        let mut conn = self.pool.acquire().await.unwrap();
        let mut proposer = cloud_propose::system_proposer(&mut conn, self.owner, self.carrier)
            .await
            .unwrap();
        proposer.source = source;
        let pricing = self.service.pricing().await.unwrap();
        cloud_propose::open_in_tx(
            &mut conn,
            &self.config,
            &pricing,
            &proposer,
            hours,
            reason.to_owned(),
        )
        .await
        .unwrap()
        .request_id
    }

    async fn decision(&self, request: Uuid, allow: bool) {
        let payload = serde_json::to_value(DecisionPayload::Permission {
            behavior: if allow {
                PermissionBehavior::Allow
            } else {
                PermissionBehavior::Deny
            },
            message: None,
        })
        .unwrap();
        sqlx::query("update requests set state='decision_saved', decision_payload=$2 where id=$1")
            .bind(request)
            .bind(payload)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn approve(&self, request: Uuid) -> cloud_propose::ApproveOutcome {
        let parent: Uuid = sqlx::query_scalar("select conversation_id from requests where id=$1")
            .bind(request)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        let mut conn = self.pool.acquire().await.unwrap();
        cloud_propose::approve_in_tx(
            &mut conn,
            self.service.as_ref(),
            request,
            parent,
            Uuid::now_v7(),
        )
        .await
        .unwrap()
    }

    async fn request_state(&self, request: Uuid) -> String {
        sqlx::query_scalar("select state from requests where id=$1")
            .bind(request)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn windows_for(&self, proposal: Uuid) -> i64 {
        sqlx::query_scalar("select count(*) from cloud_windows where proposal_id=$1")
            .bind(proposal)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn push_count(&self, code: &str) -> i64 {
        sqlx::query_scalar("select count(*) from owner_pushes where payload->>'code' = $1")
            .bind(code)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    fn tool_context(&self) -> ToolContext {
        ToolContext {
            pool: self.pool.clone(),
            config: Default::default(),
            memory: Default::default(),
            limits: Default::default(),
            settle: canterlot_hub::session_tools::settle::SettleConfig {
                nudge_enabled: true,
                ..Default::default()
            },
            alarms_enabled: true,
            nudge_enabled: true,
            routing_enabled: true,
            routing_stale: Duration::from_secs(1800),
            routing_queue_horizon: Duration::from_secs(6 * 3600),
            spawn_queue: Default::default(),
            guards: Default::default(),
            dispatcher: None,
            cloud: Some(self.service.clone()),
            registry: self.registry.clone(),
            shutdown: tokio_util::sync::CancellationToken::new(),
        }
    }

    async fn caller(&self) -> Caller {
        let conversation = testkit::seed_conversation(&self.pool, self.owner, self.carrier).await;
        let execution = Uuid::now_v7();
        sqlx::query(
            "insert into executions (id, conversation_id, generation, state) values ($1, $2, 1, 'running')",
        )
        .bind(execution)
        .bind(conversation)
        .execute(&self.pool)
        .await
        .unwrap();
        Caller {
            owner_id: self.owner,
            conversation_id: conversation,
            execution_id: execution,
            machine_id: self.carrier,
            call_id: Uuid::now_v7(),
        }
    }
}

struct Api {
    client: TestClient,
}

impl Api {
    async fn new(pool: PgPool, cloud: Option<Arc<CloudService>>) -> Self {
        let (mut deps, _) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        deps.cloud = cloud;
        let router = http::router(AppState::new(deps));
        Self {
            client: TestClient::in_process(router).as_owner(),
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        key: &str,
    ) -> (u16, Value) {
        let response = self.client.send(method, path, Some(key), body).await;
        let status = response.status.as_u16();
        let value = if response.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&response.body).unwrap()
        };
        (status, value)
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn budget_http_reads_spent_and_remaining_and_put_checks_revision(pool: PgPool) -> TestResult {
    let rig = Rig::new(pool.clone()).await;
    rig.set_budget(10_000, false).await;
    rig.insert_window("ended", Some(1_000), 1_000).await;
    rig.insert_window("ready", None, 2_000).await;

    let api = Api::new(pool, Some(rig.service.clone())).await;
    let (status, view) = api
        .send(Method::GET, "/api/v1/cloud/budget", None, "")
        .await;
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["monthly_budget_kopecks"], 10_000);
    assert_eq!(
        view["spent_kopecks"], 3_000,
        "ended final + live reservation"
    );
    assert_eq!(view["remaining_kopecks"], 7_000);
    assert_eq!(view["revision"], 0);
    assert_eq!(view["auto_mode"], false);
    assert_eq!(view["profiles"][0]["hourly_kopecks"], HOURLY);

    let body = json!({"monthly_budget_kopecks": 20_000, "auto_mode": true, "base_revision": 0});
    let (status, updated) = api
        .send(
            Method::PUT,
            "/api/v1/cloud/budget",
            Some(body.clone()),
            "key-1",
        )
        .await;
    assert_eq!(status, 200, "{updated}");
    assert_eq!(updated["revision"], 1);
    assert_eq!(updated["monthly_budget_kopecks"], 20_000);
    assert_eq!(updated["auto_mode"], true);

    let (status, stale) = api
        .send(Method::PUT, "/api/v1/cloud/budget", Some(body), "key-2")
        .await;
    assert_eq!(status, 409, "{stale}");
    assert_eq!(stale["error"]["code"], "stale_revision");
    let (_, check) = api
        .send(Method::GET, "/api/v1/cloud/budget", None, "")
        .await;
    assert_eq!(check["revision"], 1, "a stale put changes nothing");
    assert_eq!(check["auto_mode"], true);

    rig.set_block(Some("no_paid")).await;
    let (_, blocked) = api
        .send(Method::GET, "/api/v1/cloud/budget", None, "")
        .await;
    assert_eq!(blocked["provider_block_code"], "no_paid");
    let (status, _) = api
        .send(
            Method::POST,
            "/api/v1/cloud/budget/clear-block",
            None,
            "clear-1",
        )
        .await;
    assert_eq!(status, 200);
    let (_, cleared) = api
        .send(Method::GET, "/api/v1/cloud/budget", None, "")
        .await;
    assert_eq!(cleared["provider_block_code"], Value::Null);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_form_and_agent_tool_create_cards_not_windows(pool: PgPool) -> TestResult {
    let rig = Rig::new(pool.clone()).await;
    rig.set_budget(1_000_000, false).await;
    let api = Api::new(pool.clone(), Some(rig.service.clone())).await;

    let (status, opened) = api
        .send(
            Method::POST,
            "/api/v1/cloud/proposals",
            Some(json!({"hours": 2, "reason": "сборка"})),
            "propose-1",
        )
        .await;
    assert_eq!(status, 200, "{opened}");
    let request = Uuid::parse_str(opened["request_id"].as_str().unwrap())?;
    let kind: String = sqlx::query_scalar("select kind from requests where id=$1")
        .bind(request)
        .fetch_one(&rig.pool)
        .await?;
    assert_eq!(kind, "cloud_window");
    assert!(opened["reservation_kopecks"].as_i64().unwrap() > 0);
    assert_eq!(opened["over_budget"], false);
    assert_eq!(
        rig.windows_for(request).await,
        0,
        "a card creates no window"
    );

    // Второй вызов с тем же ключом идемпотентен; новый ключ упирается в открытую карточку.
    let (_, again) = api
        .send(
            Method::POST,
            "/api/v1/cloud/proposals",
            Some(json!({"hours": 2, "reason": "сборка"})),
            "propose-1",
        )
        .await;
    assert_eq!(again, opened);
    let (status, _) = api
        .send(
            Method::POST,
            "/api/v1/cloud/proposals",
            Some(json!({"hours": 2, "reason": "ещё"})),
            "propose-2",
        )
        .await;
    assert_eq!(status, 409);

    // Инструмент агента регистрируется и создаёт карточку для своего разговора.
    // Одновременно открытых карточек не бывает, поэтому первую закрываем.
    sqlx::query("update requests set state='declined' where id=$1")
        .bind(request)
        .execute(&rig.pool)
        .await?;
    assert!(canterlot_hub::session_tools::is_tool("runner.propose"));
    let context = rig.tool_context();
    let caller = rig.caller().await;
    let answer = canterlot_hub::session_tools::call(
        &context,
        &caller,
        "runner.propose",
        json!({"hours": 1, "reason": "нужен раннер"}),
    )
    .await
    .expect("the runner_propose tool succeeds");
    assert!(answer["request_id"].is_string());
    let agent_card = Uuid::parse_str(answer["request_id"].as_str().unwrap())?;
    let conversation: Uuid = sqlx::query_scalar("select conversation_id from requests where id=$1")
        .bind(agent_card)
        .fetch_one(&rig.pool)
        .await?;
    assert_eq!(conversation, caller.conversation_id);
    let bad = canterlot_hub::session_tools::call(
        &context,
        &caller,
        "runner.propose",
        json!({"hours": 5, "reason": "слишком много"}),
    )
    .await;
    assert!(bad.is_err(), "hours outside 1..=4 are refused");
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn approving_a_card_creates_exactly_one_window_even_twice(pool: PgPool) -> TestResult {
    let rig = Rig::new(pool).await;
    rig.set_budget(1_000_000, false).await;
    let request = rig.card(2, "сборка", Source::Owner).await;
    rig.decision(request, true).await;

    let first = rig.approve(request).await;
    let window = match first {
        cloud_propose::ApproveOutcome::Created(window) => window,
        other => panic!("expected a window, got {other:?}"),
    };
    assert_eq!(rig.windows_for(request).await, 1);
    let (trigger, proposal): (String, Uuid) =
        sqlx::query_as("select trigger, proposal_id from cloud_windows where id=$1")
            .bind(window)
            .fetch_one(&rig.pool)
            .await?;
    assert_eq!(trigger, Trigger::Card.as_str());
    assert_eq!(proposal, request);
    assert_eq!(rig.request_state(request).await, "decision_applied");
    assert_eq!(
        rig.approve(request).await,
        cloud_propose::ApproveOutcome::AlreadyDecided
    );
    assert_eq!(
        rig.windows_for(request).await,
        1,
        "a second approval creates no window"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn expired_card_is_closed_without_a_window(pool: PgPool) -> TestResult {
    let rig = Rig::new(pool).await;
    rig.set_budget(1_000_000, false).await;
    let request = rig.card(2, "сборка", Source::Agent).await;
    sqlx::query("update requests set expires_at = now() - interval '1 minute' where id=$1")
        .bind(request)
        .execute(&rig.pool)
        .await?;
    rig.decision(request, true).await;
    assert_eq!(
        rig.approve(request).await,
        cloud_propose::ApproveOutcome::Expired
    );
    assert_eq!(rig.request_state(request).await, "expired");
    assert_eq!(rig.windows_for(request).await, 0);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn card_outdated_issues_a_fresh_card_with_the_new_numbers(pool: PgPool) -> TestResult {
    let rig = Rig::new(pool).await;
    rig.set_budget(1_000_000, false).await;
    let request = rig.card(2, "сборка", Source::Agent).await;
    let old: Value = sqlx::query_scalar("select request_payload from requests where id=$1")
        .bind(request)
        .fetch_one(&rig.pool)
        .await?;
    assert_eq!(old["over_budget"], false);
    // Бюджет упал: резерв больше не помещается, признак перерасхода изменился.
    rig.set_budget(0, false).await;
    rig.decision(request, true).await;

    let fresh = match rig.approve(request).await {
        cloud_propose::ApproveOutcome::Outdated(fresh) => fresh,
        other => panic!("expected a fresh card, got {other:?}"),
    };
    assert_ne!(fresh, request);
    let payload: Value = sqlx::query_scalar("select request_payload from requests where id=$1")
        .bind(fresh)
        .fetch_one(&rig.pool)
        .await?;
    assert_eq!(payload["over_budget"], true);
    assert_eq!(payload["reason"], "сборка");
    assert_eq!(payload["source"], "repriced");
    assert_eq!(rig.request_state(request).await, "decision_not_applied");
    assert_eq!(
        rig.windows_for(request).await,
        0,
        "no window on an outdated card"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn over_budget_card_passes_only_with_its_exact_reservation(pool: PgPool) -> TestResult {
    let rig = Rig::new(pool).await;
    rig.set_budget(0, false).await;
    let request = rig.card(2, "перерасход", Source::Owner).await;
    let payload: Value = sqlx::query_scalar("select request_payload from requests where id=$1")
        .bind(request)
        .fetch_one(&rig.pool)
        .await?;
    assert_eq!(payload["over_budget"], true);
    let reservation = payload["reservation_kopecks"].as_i64().unwrap();
    rig.decision(request, true).await;
    // Согласие ровно на резерв: единственный путь, где бюджет можно превысить.
    let created = rig.approve(request).await;
    assert!(
        matches!(created, cloud_propose::ApproveOutcome::Created(_)),
        "{created:?}"
    );
    let stored: i64 =
        sqlx::query_scalar("select reservation_kopecks from cloud_windows where proposal_id=$1")
            .bind(request)
            .fetch_one(&rig.pool)
            .await?;
    assert_eq!(stored, reservation);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_provider_refusal_closes_the_card_without_a_window(pool: PgPool) -> TestResult {
    let rig = Rig::new(pool).await;
    rig.set_budget(1_000_000, false).await;
    let request = rig.card(2, "сборка", Source::Hub).await;
    // Живое окно уже есть: create_in_tx откажет, а карточка закроется с кодом отказа.
    rig.insert_window("ready", None, 1_000).await;
    rig.decision(request, true).await;
    assert_eq!(
        rig.approve(request).await,
        cloud_propose::ApproveOutcome::Refused("live_window_exists".to_owned())
    );
    assert_eq!(rig.request_state(request).await, "decision_not_applied");
    assert_eq!(rig.windows_for(request).await, 0);
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn declining_a_card_reports_to_the_proposer(pool: PgPool) -> TestResult {
    let rig = Rig::new(pool).await;
    rig.set_budget(1_000_000, false).await;
    let request = rig.card(2, "сборка", Source::Agent).await;
    rig.decision(request, false).await;
    let parent: Uuid = sqlx::query_scalar("select conversation_id from requests where id=$1")
        .bind(request)
        .fetch_one(&rig.pool)
        .await?;
    let job = ClaimedJob {
        id: Uuid::now_v7(),
        kind: JobKind::SessionTask,
        owner_id: rig.owner,
        conversation_id: Some(parent),
        run_at: Utc::now(),
        payload: json!({"action": "cloud_window", "request_id": request}),
        attempts: 0,
    };
    let mut conn = rig.pool.acquire().await?;
    let outcome = cloud_propose::run_job(
        &mut conn,
        Some(&rig.service),
        &job,
        parent,
        Utc::now(),
        Uuid::now_v7(),
    )
    .await?;
    assert!(matches!(outcome, JobOutcome::Done(_)));
    assert_eq!(rig.request_state(request).await, "declined");
    assert_eq!(rig.windows_for(request).await, 0);
    let note: String = sqlx::query_scalar(
        "select payload->>'text' from scheduled_jobs
          where conversation_id=$1 and payload->>'action'='note' order by run_at desc limit 1",
    )
    .bind(parent)
    .fetch_one(&rig.pool)
    .await?;
    assert!(
        note.contains("[canterlot]") && note.contains("declined"),
        "{note}"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auto_mode_requires_continuous_unavailability_and_pushes_the_reservation(
    pool: PgPool,
) -> TestResult {
    let rig = Rig::new(pool).await;
    rig.set_budget(1_000_000, true).await;
    let _carrier = testkit::fake_session(&rig.registry, rig.carrier, &["runners_v1"], true);
    let runner = rig.static_runner("runner-a").await;
    rig.seed_waiting(60).await;
    let t0 = now_micros();

    // Свежее наблюдение «offline»: первая отметка ещё не даёт десяти минут.
    rig.observe(runner, false, TimeDelta::seconds(10)).await;
    rig.service.tick(t0).await;
    assert_eq!(rig.windows_for(Uuid::nil()).await, 0);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "select state from cloud_windows order by created_at desc limit 1"
        )
        .fetch_optional(&rig.pool)
        .await?,
        None,
        "a single fresh offline observation is not enough"
    );

    // Через одиннадцать минут непрерывной недоступности окно стартует автоматически.
    rig.observe_at(runner, t0 + TimeDelta::minutes(11) - TimeDelta::seconds(10))
        .await;
    rig.service.tick(t0 + TimeDelta::minutes(11)).await;
    let (trigger, cap): (String, i32) = sqlx::query_as(
        "select trigger, hard_cap_minutes from cloud_windows order by created_at desc limit 1",
    )
    .fetch_one(&rig.pool)
    .await?;
    assert_eq!(trigger, Trigger::Auto.as_str());
    assert_eq!(cap, 90, "min(240, estimate 60 + 30)");
    assert_eq!(rig.push_count("auto_started").await, 1);
    assert_eq!(
        rig.push_count("auto_started").await,
        1,
        "the push is not repeated"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auto_mode_does_not_start_when_a_runner_is_online_unknown_or_short_offline(
    pool: PgPool,
) -> TestResult {
    let rig = Rig::new(pool).await;
    rig.set_budget(1_000_000, true).await;
    let _carrier = testkit::fake_session(&rig.registry, rig.carrier, &["runners_v1"], true);
    let runner = rig.static_runner("runner-a").await;
    rig.seed_waiting(60).await;
    let t0 = now_micros();

    // `unknown`: нет свежего наблюдения.
    rig.service.tick(t0).await;
    assert!(no_windows(&rig.pool).await?);
    // `online`: живая сессия раннера сбрасывает период.
    let _session = testkit::fake_session(&rig.registry, runner, &[], true);
    rig.observe(runner, false, TimeDelta::seconds(10)).await;
    rig.service.tick(t0 + TimeDelta::minutes(12)).await;
    assert!(no_windows(&rig.pool).await?);
    drop(_session);
    // Offline только девять минут.
    sqlx::query("delete from cloud_auto_unavailable")
        .execute(&rig.pool)
        .await?;
    rig.observe_at(runner, t0 + TimeDelta::minutes(13) - TimeDelta::seconds(10))
        .await;
    rig.service.tick(t0 + TimeDelta::minutes(13)).await;
    sqlx::query("update cloud_auto_unavailable set since = $1")
        .bind(t0 + TimeDelta::minutes(5))
        .execute(&rig.pool)
        .await?;
    rig.observe_at(runner, t0 + TimeDelta::minutes(14) - TimeDelta::seconds(10))
        .await;
    rig.service.tick(t0 + TimeDelta::minutes(14)).await;
    assert!(no_windows(&rig.pool).await?);
    Ok(())
}

async fn no_windows(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let count: i64 = sqlx::query_scalar("select count(*) from cloud_windows")
        .fetch_one(pool)
        .await?;
    Ok(count == 0)
}

async fn card_count(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("select count(*) from requests where kind='cloud_window'")
        .fetch_one(pool)
        .await
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auto_mode_is_blocked_by_zero_budget_a_block_and_a_live_window(pool: PgPool) -> TestResult {
    let rig = Rig::new(pool).await;
    let _carrier = testkit::fake_session(&rig.registry, rig.carrier, &["runners_v1"], true);
    let runner = rig.static_runner("runner-a").await;
    rig.observe(runner, false, TimeDelta::seconds(10)).await;
    rig.seed_waiting(120).await;
    let t0 = now_micros();

    rig.set_budget(0, true).await;
    rig.service.tick(t0).await;
    assert!(no_windows(&rig.pool).await?);

    rig.set_budget(10, true).await; // резерв не помещается
    rig.service.tick(t0 + TimeDelta::minutes(1)).await;
    assert!(no_windows(&rig.pool).await?);

    rig.set_budget(1_000_000, true).await;
    rig.set_block(Some("no_paid")).await;
    rig.service.tick(t0 + TimeDelta::minutes(2)).await;
    assert!(no_windows(&rig.pool).await?);
    rig.set_block(None).await;

    rig.insert_window("ready", None, 1_000).await;
    rig.service.tick(t0 + TimeDelta::minutes(3)).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from cloud_windows")
            .fetch_one(&rig.pool)
            .await?,
        1,
        "only the pre-existing live window"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn hub_issues_one_card_per_episode_of_waiting_work(pool: PgPool) -> TestResult {
    let rig = Rig::new(pool).await;
    rig.set_budget(1_000_000, false).await;
    rig.seed_waiting(60).await;
    let t0 = now_micros();

    rig.service.tick(t0).await;
    assert_eq!(card_count(&rig.pool).await?, 1);
    let payload: Value = sqlx::query_scalar(
        "select request_payload from requests where kind='cloud_window' order by created_at desc limit 1",
    )
    .fetch_one(&rig.pool)
    .await?;
    assert_eq!(payload["source"], "hub");
    let active: bool = sqlx::query_scalar("select active from cloud_auto_episode where singleton")
        .fetch_one(&rig.pool)
        .await?;
    assert!(active);

    rig.service.tick(t0 + TimeDelta::minutes(1)).await;
    assert_eq!(
        card_count(&rig.pool).await?,
        1,
        "no second card while the queue persists"
    );

    sqlx::query("delete from jobs where state='waiting_runner'")
        .execute(&rig.pool)
        .await?;
    rig.service.tick(t0 + TimeDelta::minutes(2)).await;
    let active: bool = sqlx::query_scalar("select active from cloud_auto_episode where singleton")
        .fetch_one(&rig.pool)
        .await?;
    assert!(!active, "the episode ends when the queue is empty");

    rig.seed_waiting(60).await;
    rig.service.tick(t0 + TimeDelta::minutes(3)).await;
    assert_eq!(
        card_count(&rig.pool).await?,
        2,
        "a refilled queue starts a new episode"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn budget_alerts_fire_once_per_month_and_never_for_zero(pool: PgPool) -> TestResult {
    let rig = Rig::new(pool).await;
    rig.set_budget(10_000, false).await;
    let t0 = now_micros();
    rig.insert_window("ended", Some(8_000), 8_000).await;
    rig.service.tick(t0).await;
    assert_eq!(rig.push_count("budget_80").await, 1);
    assert_eq!(rig.push_count("budget_100").await, 0);

    rig.insert_window("ended", Some(2_000), 2_000).await;
    rig.service.tick(t0 + TimeDelta::minutes(1)).await;
    assert_eq!(rig.push_count("budget_100").await, 1);
    rig.service.tick(t0 + TimeDelta::minutes(2)).await;
    assert_eq!(
        rig.push_count("budget_80").await,
        1,
        "80 % is not repeated in the same month"
    );
    assert_eq!(rig.push_count("budget_100").await, 1);

    // Новый месяц: метки сброшены, оповещения уходят снова.
    let next = Utc.with_ymd_and_hms(2027, 1, 5, 12, 0, 0).unwrap();
    let month = windows::month_of(next);
    let year: i32 = month[..4].parse()?;
    let mon: u32 = month[5..].parse()?;
    let _ = (year, mon);
    sqlx::query(
        "insert into cloud_windows (id, name, state, trigger, host_key_public, price_hourly_kopecks,
                                     reservation_kopecks, final_cost_kopecks, month, hard_cap_minutes, created_at, profiles)
         values ($1, 'canterlot-runner-' || $1::text, 'ended', 'owner', 'ssh-ed25519 AAAA', $2,
                 $3, $3, $4, 60, $5, '[{\"configurator_id\":151,\"cpu\":1,\"ram_gb\":1,\"disk_gb\":1,\"hourly_kopecks\":2845}]')",
    )
    .bind(Uuid::now_v7())
    .bind(HOURLY)
    .bind(10_000_i64)
    .bind(&month)
    .bind(next)
    .execute(&rig.pool)
    .await?;
    rig.service.tick(next).await;
    assert_eq!(
        rig.push_count("budget_100").await,
        2,
        "a new month alerts again"
    );

    rig.set_budget(0, false).await;
    rig.service.tick(next + TimeDelta::minutes(1)).await;
    assert_eq!(
        rig.push_count("budget_100").await,
        2,
        "zero budget never alerts"
    );
    Ok(())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn runners_list_shows_live_window_cost_and_budget_remainder(pool: PgPool) -> TestResult {
    let rig = Rig::new(pool).await;
    rig.set_budget(1_000_000, false).await;
    rig.insert_window("ready", None, 11_380).await;
    let context = rig.tool_context();
    let caller = rig.caller().await;
    let answer = canterlot_hub::session_tools::inplace_jobs::runners(&context, &caller, json!({}))
        .await
        .expect("runners_list succeeds");
    assert_eq!(answer["budget_monthly_kopecks"], 1_000_000);
    assert_eq!(answer["budget_remaining_kopecks"], 988_620);
    let windows = answer["cloud_windows"].as_array().unwrap();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["state"], "ready");
    assert_eq!(windows[0]["reservation_kopecks"], 11_380);
    assert_eq!(windows[0]["cost_so_far_kopecks"], HOURLY);
    assert!(windows[0]["hard_cap_at"].is_null());
    assert!(windows[0]["uptime_seconds"].as_i64().unwrap() >= 0);
    Ok(())
}
