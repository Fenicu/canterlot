//! Цель через боевой composed-роутер: команда владельца → команда воркеру →
//! запись SJ адаптера → состояние и живое событие на области владельца.

use axum::http::Method;
use canterlot_hub::{
    blocks::ExecutionRef,
    delivery::{
        ContentBlock, SubmitInput,
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
        submit_input,
    },
    events::{self, ReadAfter, Scope},
    executions, goals,
    http::{self, AppState},
    ingest::{Ingest, JournalKind},
    model::ExecutionState,
    testkit::{self, TestClient},
};
use canterlot_proto::worker::v1::{
    BlockFinal, BlockStarted, GoalChanged, GoalState as ProtoGoalState, InputAccepted,
    JournalOpened, JournalRecords, RunFinished,
    record_body::Body,
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct F {
    pool: PgPool,
    client: TestClient,
    owner: Uuid,
    machine: Uuid,
    conversation: Uuid,
    execution: Uuid,
    _leases: Vec<canterlot_hub::session::TestLease>,
    _dir: tempfile::TempDir,
}

impl F {
    async fn new(pool: PgPool, capabilities: &[&str]) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let registry = deps.registry.clone();
        let owner = deps.installation.owner_id;
        let machine = Uuid::now_v7();
        let conversation = Uuid::now_v7();
        sqlx::query("insert into machines(id,owner_id,name,availability) values($1,$2,'Машина','available')")
            .bind(machine)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into conversations (id,owner_id,machine_id,harness,workdir) values($1,$2,$3,'codex','/work')")
            .bind(conversation)
            .bind(owner)
            .bind(machine)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into queue_state (conversation_id,hold_reasons) values($1,'[]')")
            .bind(conversation)
            .execute(&pool)
            .await
            .unwrap();
        let correlation = Uuid::now_v7();
        let started = executions::start(&pool, owner, conversation, correlation)
            .await
            .unwrap();
        for state in [ExecutionState::Starting, ExecutionState::Running] {
            executions::set_state(&pool, started.execution_id, state, None, correlation)
                .await
                .unwrap();
        }
        let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
        let lease = registry.connect_for_tests(
            machine,
            &capabilities
                .iter()
                .map(|c| (*c).to_owned())
                .collect::<Vec<_>>(),
        );
        Self {
            pool,
            client,
            owner,
            machine,
            conversation,
            execution: started.execution_id,
            _leases: vec![lease],
            _dir: dir,
        }
    }

    async fn post_goal(&self, body: Value, key: &str) -> canterlot_hub::testkit::TestResponse {
        self.client
            .send(
                Method::POST,
                &format!("/api/v1/conversations/{}/goal", self.conversation),
                Some(key),
                Some(body),
            )
            .await
    }

    async fn apply_adapter_goal(&self, goal: Option<ProtoGoalState>) {
        let fact = GoalChanged {
            execution_id: self.execution.to_string(),
            generation: 1,
            goal,
        };
        let run = ExecutionRef {
            execution_id: self.execution,
            conversation_id: self.conversation,
            machine_id: self.machine,
        };
        let mut tx = self.pool.begin().await.unwrap();
        goals::apply(&mut tx, &run, &fact, Uuid::now_v7())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    async fn goal_events(&self) -> Vec<Value> {
        let ReadAfter::Events(rows) = events::read_after(
            &self.pool,
            &Scope::Conversations {
                owner_id: self.owner,
            },
            0,
            100,
        )
        .await
        .unwrap() else {
            panic!("resync required");
        };
        rows.into_iter()
            .filter(|row| row.kind == "goal_changed")
            .map(|row| {
                assert!(row.scope.ends_with(":conversations"), "{}", row.scope);
                row.payload
            })
            .collect()
    }
}

fn snapshot(tokens: i64) -> ProtoGoalState {
    ProtoGoalState {
        objective: "Подготовить отчёт".into(),
        status: "active".into(),
        tokens_used: tokens,
        time_used_seconds: 5,
        token_budget: Some(1000),
        turns_used: Some(2),
        updated_at_ms: 1_700_000_000_000,
    }
}

#[sqlx::test]
async fn owner_command_goes_to_the_worker_and_adapter_event_updates_all_devices(pool: PgPool) {
    let f = F::new(pool, &["goals_v1"]).await;
    let response = f
        .post_goal(
            json!({"action": "set", "objective": "Подготовить отчёт"}),
            "goal-set-1",
        )
        .await;
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let outcome: Value = response.json();
    assert!(
        outcome["requested"]["command_id"].is_string()
            || outcome["already_requested"]["command_id"].is_string(),
        "{outcome:?}"
    );

    let (kind, payload): (String, Value) = sqlx::query_as(
        "select kind, payload from commands where conversation_id = $1 order by created_at desc, id desc limit 1",
    )
    .bind(f.conversation)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(kind, "goal_command");
    assert_eq!(payload["action"], "set");
    assert_eq!(payload["objective"], "Подготовить отчёт");

    f.apply_adapter_goal(Some(snapshot(12))).await;
    let events = f.goal_events().await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["goal"]["objective"], "Подготовить отчёт");
    assert_eq!(events[0]["goal"]["status"], "active");
    assert_eq!(events[0]["goal"]["tokens_used"], 12);
    assert_eq!(events[0]["goal"]["turns_used"], 2);
    let stored = goals::in_snapshot(&mut f.pool.acquire().await.unwrap(), f.conversation)
        .await
        .unwrap()
        .expect("goal in snapshot");
    assert_eq!(stored.objective, "Подготовить отчёт");

    // Остановка цели тем же путём: команда и событие с `goal: null`.
    // Первую команду хаб уже выдал воркеру — снимаем её из pending.
    sqlx::query("update commands set delivery_state = 'completed' where conversation_id = $1")
        .bind(f.conversation)
        .execute(&f.pool)
        .await
        .unwrap();
    let response = f
        .post_goal(json!({"action": "clear"}), "goal-clear-1")
        .await;
    assert_eq!(response.status, 200);
    let (kind, payload): (String, Value) = sqlx::query_as(
        "select kind, payload from commands where conversation_id = $1 order by created_at desc, id desc limit 1",
    )
    .bind(f.conversation)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(kind, "goal_command");
    assert_eq!(payload["action"], "clear");
    f.apply_adapter_goal(None).await;
    let events = f.goal_events().await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(events[1]["goal"].is_null());
    assert!(
        goals::in_snapshot(&mut f.pool.acquire().await.unwrap(), f.conversation)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test]
async fn goal_command_without_capability_is_refused_clearly(pool: PgPool) {
    let f = F::new(pool, &["pilot_v1"]).await;
    let response = f
        .post_goal(
            json!({"action": "set", "objective": "Подготовить отчёт"}),
            "goal-no-capability",
        )
        .await;
    assert_eq!(
        response.status,
        409,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let error: Value = response.json();
    assert_eq!(error["error"]["code"], "capability_missing");
    let count: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id = $1 and kind = 'goal_command'",
    )
    .bind(f.conversation)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(count, 0, "no command for an incapable worker");
}

#[sqlx::test]
async fn goal_command_for_open_code_harness_is_refused(pool: PgPool) {
    let f = F::new(pool, &["goals_v1"]).await;
    sqlx::query("update conversations set harness = 'opencode' where id = $1")
        .bind(f.conversation)
        .execute(&f.pool)
        .await
        .unwrap();
    let response = f
        .post_goal(
            json!({"action": "set", "objective": "Подготовить отчёт"}),
            "goal-open-code",
        )
        .await;
    assert_eq!(response.status, 409);
    let error: Value = response.json();
    assert_eq!(error["error"]["code"], "invalid_state");
    assert_eq!(error["error"]["details"]["reason"], "unsupported_harness");
}

fn journal(journal_id: Uuid, seq: &mut u64, bodies: Vec<Body>) -> JournalRecords {
    JournalRecords {
        journal_id: journal_id.to_string(),
        records: bodies
            .into_iter()
            .map(|body| {
                *seq += 1;
                canterlot_proto::worker::v1::JournalRecord {
                    seq: *seq,
                    body: Some(canterlot_proto::worker::v1::RecordBody {
                        body: Some(body),
                    }),
                }
            })
            .collect(),
    }
}

#[sqlx::test]
async fn autonomous_goal_turn_reaches_history_and_ends_the_run(pool: PgPool) {
    // Владелец ставит цель по HTTP, а ход работает сам: хаб видит его блоки в ленте,
    // открытый ход — как работу, а приписанный RunFinished закрывает его.
    let f = F::new(pool.clone(), &["goals_v1", "pilot_v1"]).await;
    let credential = vec![7u8; 32];
    sqlx::query(
        "insert into machine_credentials (machine_id, token_sha256) values ($1,$2)
         on conflict do nothing",
    )
    .bind(f.machine)
    .bind(&credential)
    .execute(&f.pool)
    .await
    .unwrap();

    let response = f
        .post_goal(
            json!({"action": "set", "objective": "neutral objective"}),
            "goal-autonomous",
        )
        .await;
    assert_eq!(response.status, 200, "{}", String::from_utf8_lossy(&response.body));

    let owner_input = submit_input(
        &f.pool,
        SubmitInput {
            owner_id: f.owner,
            conversation_id: f.conversation,
            idempotency_key: Uuid::now_v7().to_string(),
            content: vec![ContentBlock::Text {
                text: "owner input".into(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        dispatch_next(
            &f.pool,
            f.conversation,
            GateInputs {
                machine_available: true,
                machine_reconciled: true,
                session_capabilities: vec!["pilot_v1".into()],
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap(),
        DispatchOutcome::Dispatched { .. }
    ));

    let ingest = Ingest::new(f.pool.clone());
    let journal_id = Uuid::now_v7();
    let mut seq = 0;
    let block_id = Uuid::now_v7();
    let text = "autonomous answer";
    ingest
        .apply_batch(
            f.machine,
            &credential,
            JournalKind::Sj,
            &journal(
                journal_id,
                &mut seq,
                vec![
                    Body::JournalOpened(JournalOpened {
                        execution_id: f.execution.to_string(),
                        generation: 1,
                        supervisor_protocol_version: 1,
                    }),
                    Body::InputAccepted(InputAccepted {
                        command_id: owner_input.command_id.to_string(),
                        dispatch_attempt: 1,
                        machine_time_ms: 0,
                    }),
                    Body::BlockStarted(BlockStarted {
                        block_id: block_id.to_string(),
                        kind: "text".into(),
                        input_command_id: owner_input.command_id.to_string(),
                        agent: "orchestrator".into(),
                        model: "codex-mini".into(),
                        ..Default::default()
                    }),
                    Body::BlockFinal(BlockFinal {
                        block_id: block_id.to_string(),
                        rev: 1,
                        checksum: canterlot_hub::blocks::checksum(text),
                        text: text.into(),
                        ..Default::default()
                    }),
                ],
            ),
        )
        .await
        .unwrap();

    let open: i64 = sqlx::query_scalar(
        "select count(*) from runs where input_command_id=$1 and finished_at is null",
    )
    .bind(owner_input.command_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(open, 1, "an open run shows the session as working");

    f.apply_adapter_goal(Some(snapshot(7))).await;
    ingest
        .apply_batch(
            f.machine,
            &credential,
            JournalKind::Sj,
            &journal(
                journal_id,
                &mut seq,
                vec![Body::RunFinished(RunFinished {
                    adopted: true,
                    execution_id: f.execution.to_string(),
                    input_command_id: owner_input.command_id.to_string(),
                    duration_ms: 1000,
                    machine_time_ms: 0,
                    context_tokens: 10,
                    context_window_tokens: 100,
                    usage: vec![],
                    git: None,
                })],
            ),
        )
        .await
        .unwrap();

    let closed: i64 = sqlx::query_scalar(
        "select count(*) from runs where input_command_id=$1 and finished_at is not null",
    )
    .bind(owner_input.command_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(closed, 1, "the adopted turn closes the run: session idle");

    let response = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{}/history", f.conversation),
            None,
            None,
        )
        .await;
    assert_eq!(response.status, 200, "{}", String::from_utf8_lossy(&response.body));
    let page: Value = response.json();
    let items = page["items"].as_array().expect("history items");
    let block = items.iter().find(|item| item["kind"] == "block").unwrap_or_else(|| panic!("{items:?}"));
    assert_eq!(block["content"], text);
    assert_eq!(block["input_command_id"], owner_input.command_id.to_string());
    assert_eq!(block["state"], "final");

    let goal = goals::in_snapshot(&mut f.pool.acquire().await.unwrap(), f.conversation)
        .await
        .unwrap()
        .expect("goal in snapshot");
    assert_eq!(goal.objective, "Подготовить отчёт");
    assert_eq!(goal.status, "active");
}
