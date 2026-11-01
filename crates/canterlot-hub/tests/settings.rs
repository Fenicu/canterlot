use axum::http::Method;
use canterlot_hub::{
    http::{self, AppState},
    presets::{Actor, NewPreset, PresetPatch},
    testkit::{self, TestClient},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unsupported_setting_returns_422_with_reason(pool: PgPool) {
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let owner = deps.installation.owner_id;
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
        .bind(machine)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    testkit::seed_catalog(&pool, machine).await;
    let conversation = testkit::seed_conversation(&pool, owner, machine).await;
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let response = client
        .send(
            Method::PATCH,
            &format!("/api/v1/conversations/{conversation}/settings"),
            Some("settings"),
            Some(json!({"model":"missing"})),
        )
        .await;
    assert_eq!(response.status, 422);
    assert_eq!(response.json::<Value>()["error"]["code"], "model_unknown");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn session_on_a_model_gone_from_the_catalog_keeps_its_other_settings_open(pool: PgPool) {
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let owner = deps.installation.owner_id;
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
        .bind(machine)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    testkit::seed_catalog(&pool, machine).await;
    let conversation = testkit::seed_conversation(&pool, owner, machine).await;
    sqlx::query("update conversations set model='opus',effort='high' where id=$1")
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let path = format!("/api/v1/conversations/{conversation}/settings");
    let kept = client
        .send(
            Method::PATCH,
            &path,
            Some("effort"),
            Some(json!({"effort":"low"})),
        )
        .await;
    assert_eq!(kept.status, 200, "{}", String::from_utf8_lossy(&kept.body));
    let gone = client
        .send(
            Method::PATCH,
            &path,
            Some("model"),
            Some(json!({"model":"sonnet"})),
        )
        .await;
    assert_eq!(gone.status, 422);
    assert_eq!(gone.json::<Value>()["error"]["code"], "model_unknown");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn pending_settings_are_visible_in_detail_and_event(pool: PgPool) {
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let owner = deps.installation.owner_id;
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
        .bind(machine)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    testkit::seed_catalog(&pool, machine).await;
    let conversation = testkit::seed_conversation(&pool, owner, machine).await;
    sqlx::query("update conversations set model='synthetic-small',effort='high' where id=$1")
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let path = format!("/api/v1/conversations/{conversation}");
    let result = client
        .send(
            Method::PATCH,
            &format!("{path}/settings"),
            Some("settings"),
            Some(json!({"effort":"low"})),
        )
        .await;
    assert_eq!(result.status, 200);
    assert_eq!(result.json::<Value>()["pending"]["effort"], "low");
    let submitted = client
        .send(
            Method::POST,
            &format!("{path}/inputs"),
            Some("input"),
            Some(json!({"content":[{"type":"text","text":"text"}]})),
        )
        .await;
    assert_eq!(submitted.status, 201);
    let input = submitted.json::<Value>()["input_id"].clone();
    let detail = client.send(Method::GET, &path, None, None).await;
    assert_eq!(
        detail.json::<Value>()["settings"]["pending"]["effort"],
        "low"
    );
    let expected = json!({"reason":"capability_missing","input_id":input});
    assert!(
        detail.json::<Value>()["holds"]
            .as_array()
            .unwrap()
            .contains(&expected)
    );
    assert!(
        detail.json::<Value>()["queue"]["hold_reasons"]
            .as_array()
            .unwrap()
            .contains(&expected)
    );
    sqlx::query("update inputs set state='dispatched' where id=$1")
        .bind(input.as_str().unwrap().parse::<Uuid>().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update commands set payload=payload || $2 where id=(select command_id from inputs where id=$1)").bind(input.as_str().unwrap().parse::<Uuid>().unwrap()).bind(json!({"settings":{"effort":"low"},"settings_rev":1})).execute(&pool).await.unwrap();
    let detail = client.send(Method::GET, &path, None, None).await;
    assert!(
        detail.json::<Value>()["queue"]["hold_reasons"]
            .as_array()
            .unwrap()
            .contains(&expected)
    );
    let event: Value = sqlx::query_scalar("select payload from events where scope=$1 and kind='conversation_settings_changed' order by scope_seq desc limit 1").bind(format!("conversation:{conversation}")).fetch_one(&pool).await.unwrap();
    assert_eq!(event["pending"]["effort"], "low");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn settings_promotion_ignores_keys_changed_after_dispatch(pool: PgPool) {
    use canterlot_hub::{
        conversations,
        delivery::submit::{self, ContentBlock, SubmitInput},
    };
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let owner = deps.installation.owner_id;
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
        .bind(machine)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let conversation = testkit::seed_conversation(&pool, owner, machine).await;
    sqlx::query(
        "update conversations set model='old',effort='high',pending_settings=$2 where id=$1",
    )
    .bind(conversation)
    .bind(json!({"model":"newer","effort":"low"}))
    .execute(&pool)
    .await
    .unwrap();
    let input = submit::submit_input(
        &pool,
        SubmitInput {
            owner_id: owner,
            conversation_id: conversation,
            idempotency_key: "submit".into(),
            content: vec![ContentBlock::Text {
                text: "text".into(),
            }],
            received_at: chrono::Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("update commands set payload=payload || $2 where id=$1")
        .bind(input.command_id)
        .bind(json!({"settings":{"model":"new","effort":"low"},"settings_rev":1}))
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    conversations::promote_settings_in_tx(&mut tx, input.command_id, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let row: (String, String, Value) =
        sqlx::query_as("select model,effort,pending_settings from conversations where id=$1")
            .bind(conversation)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.0, "new");
    assert_eq!(row.1, "low");
    assert_eq!(row.2, json!({"model":"newer"}));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dispatch_gate_holds_settings_inputs_for_a_session_without_pilot_v1(pool: PgPool) {
    use canterlot_hub::{
        delivery::dispatch::{self, BlockReason, DispatchOutcome, GateInputs},
        executions,
        model::ExecutionState,
    };
    let (client, id, _machine, _dir) = settings_client(&pool).await;
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let execution = executions::start(&pool, owner, id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    let path = format!("/api/v1/conversations/{id}");
    let changed = client
        .send(
            Method::PATCH,
            &format!("{path}/settings"),
            Some("settings"),
            Some(json!({"effort":"low"})),
        )
        .await;
    assert_eq!(changed.status, 200);
    let submitted = client
        .send(
            Method::POST,
            &format!("{path}/inputs"),
            Some("input"),
            Some(json!({"content":[{"type":"text","text":"text"}]})),
        )
        .await
        .json::<Value>();
    let input_id: Uuid = submitted["input_id"].as_str().unwrap().parse().unwrap();
    let command_id: Uuid = submitted["command_id"].as_str().unwrap().parse().unwrap();
    let gate = |capabilities: Vec<String>| GateInputs {
        machine_available: true,
        machine_reconciled: true,
        session_capabilities: capabilities,
    };
    let outcome = dispatch::dispatch_next(&pool, id, gate(vec![]), Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(
        outcome,
        DispatchOutcome::Blocked(vec![BlockReason::CapabilityMissing { input_id }])
    );
    let state: String = sqlx::query_scalar("select state from inputs where id=$1")
        .bind(input_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "queued");
    let stored: Value =
        sqlx::query_scalar("select hold_reasons from queue_state where conversation_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!stored.to_string().contains("capability_missing"));
    let detail = client
        .send(Method::GET, &path, None, None)
        .await
        .json::<Value>();
    let expected = json!({"reason":"capability_missing","input_id":input_id});
    assert!(detail["holds"].as_array().unwrap().contains(&expected));
    assert!(
        detail["queue"]["hold_reasons"]
            .as_array()
            .unwrap()
            .contains(&expected)
    );
    let outcome = dispatch::dispatch_next(&pool, id, gate(vec!["pilot_v1".into()]), Uuid::now_v7())
        .await
        .unwrap();
    assert!(matches!(outcome, DispatchOutcome::Dispatched { .. }));
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload["settings"], json!({"effort":"low"}));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dispatch_revalidates_pending_settings_and_drops_invalid_keys_with_an_event(pool: PgPool) {
    use canterlot_hub::{
        delivery::{
            dispatch::{self, GateInputs},
            submit::{self, ContentBlock, SubmitInput},
        },
        executions,
        model::ExecutionState,
    };
    let seed = testkit::seed(&pool).await;
    testkit::seed_catalog(&pool, seed.machine_id).await;
    sqlx::query("update conversations set model='synthetic-small',effort='high' where id=$1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let execution = executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    sqlx::query("update conversations set pending_settings=$2 where id=$1")
        .bind(seed.conversation_id)
        .bind(json!({"effort":"unavailable"}))
        .execute(&pool)
        .await
        .unwrap();
    submit::submit_input(
        &pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: "input".into(),
            content: vec![ContentBlock::Text {
                text: "text".into(),
            }],
            received_at: chrono::Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    dispatch::dispatch_next(
        &pool,
        seed.conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let pending: Value =
        sqlx::query_scalar("select pending_settings from conversations where id=$1")
            .bind(seed.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending, json!({}));
    let event: Value = sqlx::query_scalar("select payload from events where scope=$1 and kind='conversation_settings_changed' order by scope_seq desc limit 1").bind(format!("conversation:{}",seed.conversation_id)).fetch_one(&pool).await.unwrap();
    assert_eq!(event["rejected"]["keys"], json!(["effort"]));
    assert_eq!(event["rejected"]["reason"], "settings_unavailable");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn settings_change_applies_with_next_input_and_survives_redispatch(pool: PgPool) {
    use canterlot_hub::{
        conversations::{self, SettingsPatch},
        delivery::{
            dispatch::{self, DispatchOutcome, GateInputs},
            receipts::{self, InputReceipt},
            submit::{self, ContentBlock, SubmitInput},
        },
        executions,
        model::ExecutionState,
        outbox,
    };
    use canterlot_proto::worker::v1::command;
    let seed = testkit::seed(&pool).await;
    testkit::seed_catalog(&pool, seed.machine_id).await;
    sqlx::query("update conversations set model='synthetic-small',effort='high' where id=$1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let execution = executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    conversations::change_settings(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "settings",
        SettingsPatch {
            effort: Some("low".into()),
            ..Default::default()
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let input = submit::submit_input(
        &pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: "input".into(),
            content: vec![ContentBlock::Text {
                text: "text".into(),
            }],
            received_at: chrono::Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    let gate = GateInputs {
        machine_available: true,
        machine_reconciled: true,
        session_capabilities: vec!["pilot_v1".into()],
    };
    assert!(matches!(
        dispatch::dispatch_next(&pool, seed.conversation_id, gate.clone(), Uuid::now_v7())
            .await
            .unwrap(),
        DispatchOutcome::Dispatched { attempt: 1, .. }
    ));
    let row: outbox::CommandRow = sqlx::query_as("select * from commands where id=$1")
        .bind(input.command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let Some(command::Kind::DeliverInput(proto)) = outbox::to_proto(&row).unwrap().kind else {
        panic!("deliver")
    };
    assert_eq!(proto.settings.unwrap().effort, "low");
    receipts::apply_input_receipt(
        &pool,
        input.command_id,
        1,
        InputReceipt::Withdrawn,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(matches!(
        dispatch::dispatch_next(&pool, seed.conversation_id, gate, Uuid::now_v7())
            .await
            .unwrap(),
        DispatchOutcome::Dispatched { attempt: 2, .. }
    ));
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(input.command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload["settings"]["effort"], "low");
    receipts::apply_input_receipt(
        &pool,
        input.command_id,
        2,
        InputReceipt::Accepted,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let (effort, pending): (String, Value) =
        sqlx::query_as("select effort,pending_settings from conversations where id=$1")
            .bind(seed.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(effort, "low");
    assert_eq!(pending, json!({}));
}

async fn settings_client(pool: &PgPool) -> (TestClient, Uuid, Uuid, tempfile::TempDir) {
    let (deps, dir) = testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let owner = deps.installation.owner_id;
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
        .bind(machine)
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    testkit::seed_catalog(pool, machine).await;
    let id = testkit::seed_conversation(pool, owner, machine).await;
    sqlx::query("update conversations set model='synthetic-small',effort='high' where id=$1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    (
        TestClient::in_process(http::router(AppState::new(deps))).as_owner(),
        id,
        machine,
        dir,
    )
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn model_change_with_an_effort_the_model_lacks_is_refused_at_change_time(pool: PgPool) {
    let (client, id, machine, _dir) = settings_client(&pool).await;
    sqlx::query("update machine_harnesses set models=models || $2::jsonb where machine_id=$1")
        .bind(machine)
        .bind(json!([{"id":"other","efforts":["low"]}]))
        .execute(&pool)
        .await
        .unwrap();
    let r = client
        .send(
            Method::PATCH,
            &format!("/api/v1/conversations/{id}/settings"),
            Some("settings"),
            Some(json!({"model":"other"})),
        )
        .await;
    assert_eq!(r.status, 422);
    assert_eq!(r.json::<Value>()["error"]["code"], "effort_not_allowed");
    let pending: Value =
        sqlx::query_scalar("select pending_settings from conversations where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending, json!({}));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn midsession_unsupported_harness_explains_the_reason(pool: PgPool) {
    let (client, id, machine, _dir) = settings_client(&pool).await;
    sqlx::query("update machine_harnesses set settings_midsession=false where machine_id=$1")
        .bind(machine)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'running')",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let r = client
        .send(
            Method::PATCH,
            &format!("/api/v1/conversations/{id}/settings"),
            Some("settings"),
            Some(json!({"effort":"low"})),
        )
        .await;
    assert_eq!(r.status, 422);
    assert_eq!(
        r.json::<Value>()["error"]["code"],
        "settings_midsession_unsupported"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn rejected_settings_do_not_poison_following_inputs(pool: PgPool) {
    use canterlot_hub::{
        conversations::{self, SettingsPatch},
        delivery::{
            dispatch::{self, DispatchOutcome, GateInputs},
            receipts::{self, InputReceipt},
            submit::{self, ContentBlock, SubmitInput},
        },
        executions,
        model::ExecutionState,
    };
    let seed = testkit::seed(&pool).await;
    testkit::seed_catalog(&pool, seed.machine_id).await;
    sqlx::query("update conversations set model='synthetic-small',effort='high' where id=$1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let execution = executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    conversations::change_settings(
        &pool,
        seed.owner_id,
        seed.conversation_id,
        "settings",
        SettingsPatch {
            effort: Some("low".into()),
            ..Default::default()
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let mk = |key: &str| SubmitInput {
        owner_id: seed.owner_id,
        conversation_id: seed.conversation_id,
        idempotency_key: key.into(),
        content: vec![ContentBlock::Text {
            text: "text".into(),
        }],
        received_at: chrono::Utc::now(),
        correlation_id: Uuid::now_v7(),
        origin: None,
    };
    let first = submit::submit_input(&pool, mk("first")).await.unwrap();
    let second = submit::submit_input(&pool, mk("second")).await.unwrap();
    let gate = GateInputs {
        machine_available: true,
        machine_reconciled: true,
        session_capabilities: vec!["pilot_v1".into()],
    };
    assert!(
        matches!(dispatch::dispatch_next(&pool,seed.conversation_id,gate.clone(),Uuid::now_v7()).await.unwrap(),DispatchOutcome::Dispatched{input_id,..} if input_id==first.input_id)
    );
    sqlx::query("update machine_harnesses set models=$2 where machine_id=$1")
        .bind(seed.machine_id)
        .bind(json!([{"id":"synthetic-small","efforts":["high"]}]))
        .execute(&pool)
        .await
        .unwrap();
    receipts::apply_input_receipt(
        &pool,
        first.command_id,
        1,
        InputReceipt::RejectedBeforeTransmission {
            reason: "settings_unavailable".into(),
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(
        matches!(dispatch::dispatch_next(&pool,seed.conversation_id,gate,Uuid::now_v7()).await.unwrap(),DispatchOutcome::Dispatched{input_id,..} if input_id==second.input_id)
    );
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(second.command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(payload.get("settings").is_none());
    receipts::apply_input_receipt(
        &pool,
        second.command_id,
        1,
        InputReceipt::Accepted,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let (effort,pending,pauses): (String,Value,Value) = sqlx::query_as("select c.effort,c.pending_settings,q.pause_reasons from conversations c join queue_state q on q.conversation_id=c.id where c.id=$1").bind(seed.conversation_id).fetch_one(&pool).await.unwrap();
    assert_eq!(effort, "high");
    assert_eq!(pending, json!({}));
    assert_eq!(pauses, json!([]));
}

#[cfg(feature = "fault-injection")]
#[path = "support/settings_process_guard.rs"]
mod settings_process_guard;

#[cfg(feature = "fault-injection")]
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn settings_survive_worker_restart_between_dispatch_and_accept(pool: PgPool) {
    use canterlot_hub::{
        config::HubTimers,
        conversations::{self, SettingsPatch},
        delivery::{
            control,
            submit::{self, ContentBlock, SubmitInput},
        },
        executions, machines,
    };
    use rustix::process::{Pid, Signal, kill_process};
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    use tokio::{
        process::Command,
        time::{sleep, timeout},
    };
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let mut candidates = vec![deps.parent().unwrap().join("canterlot-worker")];
    let mut artifacts: Vec<_> = std::fs::read_dir(deps)
        .unwrap()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with("canterlot_worker-") || name.ends_with(".d") {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    artifacts.sort_by_key(|a| std::cmp::Reverse(a.0));
    candidates.extend(artifacts.into_iter().map(|(_, path)| path));
    let binary = candidates
        .into_iter()
        .find(|candidate| {
            std::process::Command::new(candidate)
                .args(["synthetic-cli", "--help"])
                .stderr(std::process::Stdio::null())
                .output()
                .is_ok_and(|out| {
                    out.status.success()
                        && out
                            .stdout
                            .windows(b"--profile".len())
                            .any(|part| part == b"--profile")
                })
        })
        .expect("build canterlot-worker with --all-features");
    let root = tempfile::tempdir().unwrap();
    let _guard = settings_process_guard::ProcessGuard::new(root.path(), &binary);
    let hub = testkit::spawn_test_hub(
        pool.clone(),
        HubTimers {
            dispatch_sweep: Duration::from_millis(30),
            ..Default::default()
        },
    )
    .await;
    let machine = machines::add(&pool, hub.installation.owner_id, "worker-test")
        .await
        .unwrap();
    let token = root.path().join("token");
    std::fs::write(&token, &machine.token).unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let ca = root.path().join("ca.pem");
    std::fs::write(&ca, &hub.pki.ca_pem).unwrap();
    let work = root.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let config = root.path().join("worker.toml");
    std::fs::write(&config, format!("hub_url = \"https://localhost:{}\"\nca_cert = {:?}\ntoken_file = {:?}\nmachine_id = \"{}\"\ninstallation_id = \"{}\"\nrestore_generation = \"{}\"\ndata_dir = {:?}\n[execution]\nallowed_workdir_roots = [{:?}]\nsupervisor_exe = {:?}\n[timers]\nheartbeat = \"100ms\"\nbackoff_min = \"100ms\"\nbackoff_max = \"500ms\"\n", hub.addr.port(),ca.to_str().unwrap(),token.to_str().unwrap(),machine.machine_id,hub.installation.installation_id,hub.installation.restore_generation,root.path().join("data").to_str().unwrap(),work.to_str().unwrap(),binary.to_str().unwrap())).unwrap();
    let spawn = || {
        Command::new(&binary)
            .arg("run")
            .arg("--config")
            .arg(&config)
            .env("CANTERLOT_FAULT_HOLD_AT", "before_gate")
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut worker = spawn();
    timeout(Duration::from_secs(45), async {
        while !hub.registry.is_reconciled(machine.machine_id) {
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let id = testkit::seed_conversation(&pool, hub.installation.owner_id, machine.machine_id).await;
    sqlx::query("update conversations set workdir=$2,model='synthetic-large',effort='medium',permission_mode='owner' where id=$1").bind(id).bind(work.to_str().unwrap()).execute(&pool).await.unwrap();
    let execution = executions::start(&pool, hub.installation.owner_id, id, Uuid::now_v7())
        .await
        .unwrap();
    timeout(Duration::from_secs(45), async {
        loop {
            let state: String = sqlx::query_scalar("select state from executions where id=$1")
                .bind(execution.execution_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if state == "running" {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    conversations::change_settings(
        &pool,
        hub.installation.owner_id,
        id,
        "settings",
        SettingsPatch {
            effort: Some("low".into()),
            ..Default::default()
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let input = submit::submit_input(
        &pool,
        SubmitInput {
            owner_id: hub.installation.owner_id,
            conversation_id: id,
            idempotency_key: "input".into(),
            content: vec![ContentBlock::Text {
                text: json!({"synthetic":[{"delta":"ok"}]}).to_string(),
            }],
            received_at: chrono::Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    hub.dispatcher.trigger(id);
    timeout(Duration::from_secs(45), async {
        loop {
            let state: String = sqlx::query_scalar("select state from inputs where id=$1")
                .bind(input.input_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if state == "on_machine" {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let connection = hub
        .registry
        .session_info(machine.machine_id)
        .unwrap()
        .connection_id;
    let before: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(input.command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before["settings"]["effort"], "low");
    kill_process(
        Pid::from_raw(worker.id().unwrap() as i32).unwrap(),
        Signal::KILL,
    )
    .unwrap();
    worker.wait().await.unwrap();
    worker = spawn();
    timeout(Duration::from_secs(45), async {
        loop {
            if hub
                .registry
                .session_info(machine.machine_id)
                .is_some_and(|s| s.connection_id != connection)
                && hub.registry.is_reconciled(machine.machine_id)
            {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    control::request_stop(
        &pool,
        hub.installation.owner_id,
        id,
        "interrupt",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    timeout(Duration::from_secs(45), async {
        loop {
            let state: String = sqlx::query_scalar("select state from inputs where id=$1")
                .bind(input.input_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if state == "queued" {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(
        root.path()
            .join("data/executions")
            .join(execution.execution_id.to_string())
            .join("fault_release"),
        [],
    )
    .unwrap();
    control::continue_queue(
        &pool,
        hub.installation.owner_id,
        id,
        "continue",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    hub.dispatcher.trigger(id);
    timeout(Duration::from_secs(60), async {
        loop {
            let state: String = sqlx::query_scalar("select state from inputs where id=$1")
                .bind(input.input_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if state == "accepted" {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let after: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(input.command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before["settings"], after["settings"]);
    let attempt: i32 = sqlx::query_scalar("select dispatch_attempt from commands where id=$1")
        .bind(input.command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attempt, 2);
    let (effort, pending): (String, Value) =
        sqlx::query_as("select effort,pending_settings from conversations where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(effort, "low");
    assert_eq!(pending, json!({}));
    let events: i64=sqlx::query_scalar("select count(*) from events where scope=$1 and kind='conversation_settings_changed' and payload->'effective'->>'effort'='low'").bind(format!("conversation:{id}")).fetch_one(&pool).await.unwrap();
    assert_eq!(events, 1);
    control::request_stop_execution(
        &pool,
        hub.installation.owner_id,
        id,
        true,
        "stop",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    timeout(Duration::from_secs(45), async {
        loop {
            let state: String = sqlx::query_scalar("select state from executions where id=$1")
                .bind(execution.execution_id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if matches!(state.as_str(), "stopped" | "failed") {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    kill_process(
        Pid::from_raw(worker.id().unwrap() as i32).unwrap(),
        Signal::KILL,
    )
    .unwrap();
    worker.wait().await.unwrap();
    hub.shutdown.send(()).unwrap();
    hub.join.await.unwrap().unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn pending_settings_are_dropped_when_effective_model_disappears(pool: PgPool) {
    use canterlot_hub::{
        delivery::{
            dispatch::{self, GateInputs},
            submit::{self, ContentBlock, SubmitInput},
        },
        executions,
        model::ExecutionState,
    };
    let seed = testkit::seed(&pool).await;
    testkit::seed_catalog(&pool, seed.machine_id).await;
    sqlx::query("update conversations set model='synthetic-small',effort='high' where id=$1")
        .bind(seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let execution = executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    sqlx::query("update conversations set pending_settings=$2 where id=$1")
        .bind(seed.conversation_id)
        .bind(json!({"permission_mode":"bypass"}))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update machine_harnesses set models='[]' where machine_id=$1")
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    let input = submit::submit_input(
        &pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: "input".into(),
            content: vec![ContentBlock::Text {
                text: "text".into(),
            }],
            received_at: chrono::Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    dispatch::dispatch_next(
        &pool,
        seed.conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(input.command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(payload.get("settings").is_none());
    let event: Value=sqlx::query_scalar("select payload from events where scope=$1 and kind='conversation_settings_changed' order by scope_seq desc limit 1").bind(format!("conversation:{}",seed.conversation_id)).fetch_one(&pool).await.unwrap();
    assert_eq!(event["rejected"]["keys"], json!(["permission_mode"]));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn rejected_settings_report_sent_keys_and_preserve_newer_pending_values(pool: PgPool) {
    use canterlot_hub::{
        conversations,
        delivery::submit::{self, ContentBlock, SubmitInput},
    };
    let seed = testkit::seed(&pool).await;
    sqlx::query("update conversations set pending_settings=$2 where id=$1")
        .bind(seed.conversation_id)
        .bind(json!({"effort":"newer"}))
        .execute(&pool)
        .await
        .unwrap();
    let input = submit::submit_input(
        &pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: "input".into(),
            content: vec![ContentBlock::Text {
                text: "text".into(),
            }],
            received_at: chrono::Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("update commands set payload=payload || $2 where id=$1")
        .bind(input.command_id)
        .bind(json!({"settings":{"effort":"old"},"settings_rev":1}))
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    conversations::drop_rejected_settings_in_tx(&mut tx, input.command_id, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let pending: Value =
        sqlx::query_scalar("select pending_settings from conversations where id=$1")
            .bind(seed.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending, json!({"effort":"newer"}));
    let event: Value=sqlx::query_scalar("select payload from events where scope=$1 and kind='conversation_settings_changed' order by scope_seq desc limit 1").bind(format!("conversation:{}",seed.conversation_id)).fetch_one(&pool).await.unwrap();
    assert_eq!(event["rejected"]["keys"], json!(["effort"]));
}

async fn create_preset(pool: &PgPool, owner: Uuid, key: &str, title: &str, text: &str) -> Value {
    canterlot_hub::presets::create(
        pool,
        owner,
        key,
        NewPreset {
            title: title.into(),
            description: None,
            text: text.into(),
        },
        Actor::Owner,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    sqlx::query_scalar("select to_jsonb(t) from (select id::text as id, revision from instruction_presets where owner_id=$1 and title=$2 and deleted_at is null) t")
        .bind(owner)
        .bind(title)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn start_running(pool: &PgPool, owner: Uuid, conversation: Uuid) {
    use canterlot_hub::{executions, model::ExecutionState};
    let execution = executions::start(pool, owner, conversation, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
}

fn gate(capabilities: &[&str]) -> canterlot_hub::delivery::dispatch::GateInputs {
    canterlot_hub::delivery::dispatch::GateInputs {
        machine_available: true,
        machine_reconciled: true,
        session_capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn preset_change_is_pending_until_accepted_then_promoted_with_revision(pool: PgPool) {
    let (client, conversation, _machine, _dir) = settings_client(&pool).await;
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(conversation)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("update conversations set model='synthetic-large',effort='high' where id=$1")
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
    let preset = create_preset(&pool, owner, "preset-1", "Исполнитель", "текст A").await;
    let id = preset["id"].as_str().unwrap();
    let path = format!("/api/v1/conversations/{conversation}");
    let changed = client
        .send(
            Method::PATCH,
            &format!("{path}/settings"),
            Some("settings"),
            Some(json!({"instruction_profile": id})),
        )
        .await;
    assert_eq!(changed.status, 200);
    let view = changed.json::<Value>();
    assert_eq!(view["pending"]["instruction_profile"], id);
    assert_eq!(view["effective"]["instruction_profile"], "clean");
    assert_eq!(
        view["effective"]["instruction_preset"]["revision"],
        Value::Null
    );
    let event: Value = sqlx::query_scalar("select payload from events where scope=$1 and kind='conversation_settings_changed' order by scope_seq desc limit 1").bind(format!("conversation:{conversation}")).fetch_one(&pool).await.unwrap();
    assert_eq!(event["pending"]["instruction_profile"], id);
    start_running(&pool, owner, conversation).await;
    // Отправка входа и диспетч: команда несёт снимок ревизии 1 с текстом.
    let submitted = client
        .send(
            Method::POST,
            &format!("{path}/inputs"),
            Some("input"),
            Some(json!({"content":[{"type":"text","text":"text"}]})),
        )
        .await;
    assert_eq!(submitted.status, 201);
    let command_id: Uuid = submitted.json::<Value>()["command_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let outcome = canterlot_hub::delivery::dispatch::dispatch_next(
        &pool,
        conversation,
        gate(&["pilot_v1", "instructions_v1"]),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        canterlot_hub::delivery::dispatch::DispatchOutcome::Dispatched { .. }
    ));
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload["settings"]["instruction_profile"], id);
    assert_eq!(payload["settings"]["instructions"]["mode"], "preset");
    assert_eq!(payload["settings"]["instructions"]["revision"], 1);
    assert_eq!(payload["settings"]["instructions"]["text"], "текст A");
    // Принятие входа продвигает ссылку и закрепляет версию.
    let mut tx = pool.begin().await.unwrap();
    canterlot_hub::conversations::promote_settings_in_tx(&mut tx, command_id, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let detail = client
        .send(Method::GET, &path, None, None)
        .await
        .json::<Value>();
    assert_eq!(detail["settings"]["effective"]["instruction_profile"], id);
    assert_eq!(
        detail["settings"]["effective"]["instruction_preset"]["revision"],
        1
    );
    assert_eq!(
        detail["settings"]["effective"]["instruction_preset"]["current_revision"],
        1
    );
    assert!(
        detail["settings"]["pending"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn rejected_input_drops_pending_preset(pool: PgPool) {
    let (client, conversation, _machine, _dir) = settings_client(&pool).await;
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(conversation)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("update conversations set model='synthetic-large',effort='high' where id=$1")
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
    let preset = create_preset(&pool, owner, "preset-1", "Исполнитель", "текст A").await;
    let id = preset["id"].as_str().unwrap();
    let path = format!("/api/v1/conversations/{conversation}");
    let changed = client
        .send(
            Method::PATCH,
            &format!("{path}/settings"),
            Some("settings"),
            Some(json!({"instruction_profile": id})),
        )
        .await;
    assert_eq!(changed.status, 200);
    // Отклонение входа очищает висячую ссылку и помечает причину.
    let submitted = client
        .send(
            Method::POST,
            &format!("{path}/inputs"),
            Some("input"),
            Some(json!({"content":[{"type":"text","text":"text"}]})),
        )
        .await;
    assert_eq!(submitted.status, 201);
    let command_id: Uuid = submitted.json::<Value>()["command_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    sqlx::query("update commands set payload=payload || $2 where id=$1")
        .bind(command_id)
        .bind(json!({"settings":{"instruction_profile":id}}))
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    canterlot_hub::conversations::drop_rejected_settings_in_tx(&mut tx, command_id, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let detail = client
        .send(Method::GET, &path, None, None)
        .await
        .json::<Value>();
    assert!(
        detail["settings"]["pending"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    let event: Value = sqlx::query_scalar("select payload from events where scope=$1 and kind='conversation_settings_changed' order by scope_seq desc limit 1").bind(format!("conversation:{conversation}")).fetch_one(&pool).await.unwrap();
    assert_eq!(event["rejected"]["keys"], json!(["instruction_profile"]));
    assert_eq!(event["rejected"]["reason"], "settings_unavailable");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reapplying_same_preset_takes_the_new_revision(pool: PgPool) {
    let (client, conversation, _machine, _dir) = settings_client(&pool).await;
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(conversation)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("update conversations set model='synthetic-large',effort='high' where id=$1")
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
    let preset = create_preset(&pool, owner, "preset-1", "Исполнитель", "текст A").await;
    let id = preset["id"].as_str().unwrap().to_owned();
    let path = format!("/api/v1/conversations/{conversation}");
    client
        .send(
            Method::PATCH,
            &format!("{path}/settings"),
            Some("settings"),
            Some(json!({"instruction_profile": id})),
        )
        .await;
    start_running(&pool, owner, conversation).await;
    // Новую версию подхватывает только явное применение той же ссылки.
    canterlot_hub::presets::update(
        &pool,
        owner,
        id.parse().unwrap(),
        "preset-replace",
        PresetPatch {
            title: None,
            description: None,
            text: Some("текст B".into()),
            base_revision: 1,
        },
        Actor::Owner,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    client
        .send(
            Method::PATCH,
            &format!("{path}/settings"),
            Some("settings-2"),
            Some(json!({"instruction_profile": id})),
        )
        .await;
    let submitted = client
        .send(
            Method::POST,
            &format!("{path}/inputs"),
            Some("input"),
            Some(json!({"content":[{"type":"text","text":"text"}]})),
        )
        .await;
    assert_eq!(submitted.status, 201);
    let command_id: Uuid = submitted.json::<Value>()["command_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let outcome = canterlot_hub::delivery::dispatch::dispatch_next(
        &pool,
        conversation,
        gate(&["pilot_v1", "instructions_v1"]),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        canterlot_hub::delivery::dispatch::DispatchOutcome::Dispatched { .. }
    ));
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload["settings"]["instructions"]["revision"], 2);
    assert_eq!(payload["settings"]["instructions"]["text"], "текст B");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn views_and_events_never_contain_preset_text(pool: PgPool) {
    let (client, conversation, _machine, _dir) = settings_client(&pool).await;
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(conversation)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("update conversations set model='synthetic-large',effort='high' where id=$1")
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
    let preset = create_preset(&pool, owner, "preset-1", "Исполнитель", "СЕКРЕТ-ПРЕСЕТА").await;
    let id = preset["id"].as_str().unwrap();
    let path = format!("/api/v1/conversations/{conversation}");
    client
        .send(
            Method::PATCH,
            &format!("{path}/settings"),
            Some("settings"),
            Some(json!({"instruction_profile": id})),
        )
        .await;
    let detail = client
        .send(Method::GET, &path, None, None)
        .await
        .json::<Value>();
    let events: Vec<Value> =
        sqlx::query_scalar("select payload from events where scope=$1 order by scope_seq")
            .bind(format!("conversation:{conversation}"))
            .fetch_all(&pool)
            .await
            .unwrap();
    let snapshot = canterlot_hub::snapshot::conversations(&pool, owner)
        .await
        .unwrap();
    let snapshot = serde_json::to_value(&snapshot).unwrap();
    for (name, value) in [
        ("detail", detail),
        ("events", Value::Array(events)),
        ("snapshot", snapshot),
    ] {
        let text = serde_json::to_string(&value).unwrap();
        assert!(
            !text.contains("СЕКРЕТ-ПРЕСЕТА"),
            "preset text leaked into {name}"
        );
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn promoting_clean_snapshot_clears_pinned_revision(pool: PgPool) {
    // Регрессия зависания хода 4: снимок «Без инструкций» несёт revision 0,
    // в строке закрепление должно стать NULL, иначе ограничение
    // conversations_instruction_ref отклоняет продвижение и ввод висит.
    let (client, conversation, _machine, _dir) = settings_client(&pool).await;
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(conversation)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("update conversations set model='synthetic-large',effort='low' where id=$1")
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
    let preset = create_preset(&pool, owner, "preset-1", "Исполнитель", "текст A").await;
    let id = preset["id"].as_str().unwrap();
    let path = format!("/api/v1/conversations/{conversation}");
    client
        .send(
            Method::PATCH,
            &format!("{path}/settings"),
            Some("settings"),
            Some(json!({"instruction_profile": id})),
        )
        .await;
    start_running(&pool, owner, conversation).await;
    let submitted = client
        .send(
            Method::POST,
            &format!("{path}/inputs"),
            Some("input"),
            Some(json!({"content":[{"type":"text","text":"text"}]})),
        )
        .await;
    assert_eq!(submitted.status, 201);
    let command_id: Uuid = submitted.json::<Value>()["command_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let outcome = canterlot_hub::delivery::dispatch::dispatch_next(
        &pool,
        conversation,
        gate(&["pilot_v1", "instructions_v1"]),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        canterlot_hub::delivery::dispatch::DispatchOutcome::Dispatched { .. }
    ));
    let mut tx = pool.begin().await.unwrap();
    canterlot_hub::conversations::promote_settings_in_tx(&mut tx, command_id, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // Завершаем первый ход, чтобы освободить слот для второго.
    use canterlot_hub::delivery::receipts::{
        InputReceipt, apply_input_receipt, apply_run_finished,
    };
    for receipt in [
        InputReceipt::Stored,
        InputReceipt::TransmissionStarted,
        InputReceipt::Accepted,
    ] {
        apply_input_receipt(&pool, command_id, 1, receipt, Uuid::now_v7())
            .await
            .unwrap();
    }
    let execution: Uuid = sqlx::query_scalar(
        "select id from executions where conversation_id=$1 and state='running'",
    )
    .bind(conversation)
    .fetch_one(&pool)
    .await
    .unwrap();
    apply_run_finished(&pool, execution, command_id, Uuid::now_v7())
        .await
        .unwrap();
    // Возврат на «Без инструкций» вторым ходом: закрепление очищается.
    client
        .send(
            Method::PATCH,
            &format!("{path}/settings"),
            Some("settings-2"),
            Some(json!({"instruction_profile": "clean"})),
        )
        .await;
    let submitted = client
        .send(
            Method::POST,
            &format!("{path}/inputs"),
            Some("input-2"),
            Some(json!({"content":[{"type":"text","text":"text"}]})),
        )
        .await;
    assert_eq!(submitted.status, 201);
    let command_id: Uuid = submitted.json::<Value>()["command_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let outcome = canterlot_hub::delivery::dispatch::dispatch_next(
        &pool,
        conversation,
        gate(&["pilot_v1", "instructions_v1"]),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        canterlot_hub::delivery::dispatch::DispatchOutcome::Dispatched { .. }
    ));
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload["settings"]["instructions"]["mode"], "clean");
    let mut tx = pool.begin().await.unwrap();
    canterlot_hub::conversations::promote_settings_in_tx(&mut tx, command_id, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let detail = client
        .send(Method::GET, &path, None, None)
        .await
        .json::<Value>();
    assert_eq!(
        detail["settings"]["effective"]["instruction_profile"],
        "clean"
    );
    assert_eq!(
        detail["settings"]["effective"]["instruction_preset"]["revision"],
        Value::Null
    );
    let row: (String, Option<i64>) = sqlx::query_as(
        "select instruction_profile, instruction_preset_revision from conversations where id=$1",
    )
    .bind(conversation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, ("clean".to_owned(), None));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn midsession_effort_change_is_refused_when_the_harness_cannot_apply_it(pool: PgPool) {
    let (client, id, machine, _dir) = settings_client(&pool).await;
    sqlx::query(
        "update machine_harnesses set settings_midsession_effort=false where machine_id=$1",
    )
    .bind(machine)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'running')",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let patch = |key: &'static str, body: Value| {
        let client = &client;
        async move {
            client
                .send(
                    Method::PATCH,
                    &format!("/api/v1/conversations/{id}/settings"),
                    Some(key),
                    Some(body),
                )
                .await
        }
    };
    let r = patch("effort", json!({"effort":"low"})).await;
    assert_eq!(r.status, 422, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(
        r.json::<Value>()["error"]["code"],
        "settings_midsession_unsupported"
    );
    let pending: Value =
        sqlx::query_scalar("select pending_settings from conversations where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending, json!({}));
    // Модель и та же самая глубина размышлений по-прежнему меняются на ходу.
    let r = patch("model", json!({"model":"synthetic-large","effort":"high"})).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn worker_rejection_of_unsupported_settings_drops_them(pool: PgPool) {
    use canterlot_hub::{
        conversations::{self, SettingsPatch},
        delivery::{
            dispatch::{self, DispatchOutcome, GateInputs},
            receipts::{self, InputReceipt},
            submit::{self, ContentBlock, SubmitInput},
        },
        executions,
        model::ExecutionState,
    };
    for reason in [
        "effort_change_unsupported",
        "permission_mode_needs_restart",
        "instructions_change_failed",
        "codex_home_failed",
        "codex_home_unusable",
    ] {
        let seed = testkit::seed(&pool).await;
        testkit::seed_catalog(&pool, seed.machine_id).await;
        sqlx::query("update conversations set model='synthetic-small',effort='high' where id=$1")
            .bind(seed.conversation_id)
            .execute(&pool)
            .await
            .unwrap();
        let execution =
            executions::start(&pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
                .await
                .unwrap();
        for state in [ExecutionState::Starting, ExecutionState::Running] {
            executions::set_state(&pool, execution.execution_id, state, None, Uuid::now_v7())
                .await
                .unwrap();
        }
        conversations::change_settings(
            &pool,
            seed.owner_id,
            seed.conversation_id,
            "settings",
            SettingsPatch {
                effort: Some("low".into()),
                ..Default::default()
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        let mk = |key: &str| SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: key.into(),
            content: vec![ContentBlock::Text {
                text: "text".into(),
            }],
            received_at: chrono::Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        };
        let first = submit::submit_input(&pool, mk("first")).await.unwrap();
        let second = submit::submit_input(&pool, mk("second")).await.unwrap();
        let gate = GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        };
        assert!(
            matches!(dispatch::dispatch_next(&pool,seed.conversation_id,gate.clone(),Uuid::now_v7()).await.unwrap(),DispatchOutcome::Dispatched{input_id,..} if input_id==first.input_id)
        );
        receipts::apply_input_receipt(
            &pool,
            first.command_id,
            1,
            InputReceipt::ExplicitRejection {
                reason: reason.into(),
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        assert!(
            matches!(dispatch::dispatch_next(&pool,seed.conversation_id,gate,Uuid::now_v7()).await.unwrap(),DispatchOutcome::Dispatched{input_id,..} if input_id==second.input_id)
        );
        let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
            .bind(second.command_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            payload.get("settings").is_none(),
            "{reason}: rejected settings ride on the next input: {}",
            payload["settings"]
        );
        let event: Value = sqlx::query_scalar("select payload from events where scope=$1 and kind='conversation_settings_changed' and payload ? 'rejected' order by scope_seq desc limit 1")
            .bind(format!("conversation:{}", seed.conversation_id))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            event["rejected"],
            json!({"keys":["effort"],"reason":reason})
        );
    }
}

async fn resume_stopped_session(
    client: &TestClient,
    pool: &PgPool,
    conversation: Uuid,
) -> canterlot_hub::executions::StartedExecution {
    use canterlot_hub::{executions, model::ExecutionState};
    let started = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{conversation}/resume"),
            Some("resume"),
            Some(json!({})),
        )
        .await;
    assert_eq!(started.status, 201);
    let started: canterlot_hub::executions::StartedExecution = started.json();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, started.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    started
}

async fn stop_live_execution(pool: &PgPool, conversation: Uuid) {
    use canterlot_hub::{executions, model::ExecutionState};
    let execution: Uuid = sqlx::query_scalar(
        "select id from executions where conversation_id=$1 order by generation desc limit 1",
    )
    .bind(conversation)
    .fetch_one(pool)
    .await
    .unwrap();
    executions::set_state(
        pool,
        execution,
        ExecutionState::Stopping,
        None,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    executions::set_state(
        pool,
        execution,
        ExecutionState::Stopped,
        Some("user_stop"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
}

async fn start_command(
    pool: &PgPool,
    command: Uuid,
) -> canterlot_proto::worker::v1::StartExecution {
    use canterlot_hub::outbox;
    use canterlot_proto::worker::v1::command;
    let row = sqlx::query_as::<_, outbox::CommandRow>("select * from commands where id=$1")
        .bind(command)
        .fetch_one(pool)
        .await
        .unwrap();
    let Some(command::Kind::StartExecution(start)) = outbox::to_proto(&row).unwrap().kind else {
        panic!("start_execution expected");
    };
    start
}

async fn settings_columns(pool: &PgPool, conversation: Uuid) -> (String, String, String, Value) {
    sqlx::query_as(
        "select model,effort,permission_mode,pending_settings from conversations where id=$1",
    )
    .bind(conversation)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn effort_changed_on_a_stopped_session_is_used_at_launch_and_the_input_passes(pool: PgPool) {
    use canterlot_hub::delivery::dispatch::{self, DispatchOutcome};
    let (client, id, machine, _dir) = settings_client(&pool).await;
    sqlx::query(
        "update machine_harnesses set settings_midsession_effort=false where machine_id=$1",
    )
    .bind(machine)
    .execute(&pool)
    .await
    .unwrap();
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    start_running(&pool, owner, id).await;
    stop_live_execution(&pool, id).await;
    let path = format!("/api/v1/conversations/{id}");
    let changed = client
        .send(
            Method::PATCH,
            &format!("{path}/settings"),
            Some("settings"),
            Some(json!({"effort":"low"})),
        )
        .await;
    assert_eq!(changed.status, 200);
    assert_eq!(changed.json::<Value>()["pending"]["effort"], "low");

    let started = resume_stopped_session(&client, &pool, id).await;

    let start = start_command(&pool, started.command_id).await;
    assert_eq!(start.effort, "low");
    assert_eq!(start.model, "synthetic-small");
    let (model, effort, _, pending) = settings_columns(&pool, id).await;
    assert_eq!(
        (model.as_str(), effort.as_str()),
        ("synthetic-small", "low")
    );
    assert_eq!(pending, json!({}));
    let event: Value = sqlx::query_scalar("select payload from events where scope=$1 and kind='conversation_settings_changed' order by scope_seq desc limit 1").bind(format!("conversation:{id}")).fetch_one(&pool).await.unwrap();
    assert_eq!(event["effective"]["effort"], "low");
    assert_eq!(event["pending"], json!({}));
    assert!(event.get("rejected").is_none());

    let submitted = client
        .send(
            Method::POST,
            &format!("{path}/inputs"),
            Some("input"),
            Some(json!({"content":[{"type":"text","text":"text"}]})),
        )
        .await;
    assert_eq!(submitted.status, 201);
    let command_id: Uuid = submitted.json::<Value>()["command_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let outcome = dispatch::dispatch_next(&pool, id, gate(&["pilot_v1"]), Uuid::now_v7())
        .await
        .unwrap();
    assert!(matches!(outcome, DispatchOutcome::Dispatched { .. }));
    // Без ключа `settings` воркеру нечего отклонять: `validate` пропускает ввод.
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(payload.get("settings").is_none(), "{payload}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn all_three_launch_settings_are_promoted_together_at_start(pool: PgPool) {
    let (client, id, machine, _dir) = settings_client(&pool).await;
    sqlx::query(
        "update machine_harnesses set settings_midsession=false,permission_modes=$2 where machine_id=$1",
    )
    .bind(machine)
    .bind(json!(["ask", "bypass"]))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("update conversations set permission_mode='ask' where id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    start_running(&pool, owner, id).await;
    stop_live_execution(&pool, id).await;
    let changed = client
        .send(
            Method::PATCH,
            &format!("/api/v1/conversations/{id}/settings"),
            Some("settings"),
            Some(json!({"model":"synthetic-large","effort":"low","permission_mode":"bypass"})),
        )
        .await;
    assert_eq!(changed.status, 200);

    let started = resume_stopped_session(&client, &pool, id).await;

    let start = start_command(&pool, started.command_id).await;
    assert_eq!(
        (
            start.model.as_str(),
            start.effort.as_str(),
            start.permission_mode.as_str()
        ),
        ("synthetic-large", "low", "bypass")
    );
    let (model, effort, permission, pending) = settings_columns(&pool, id).await;
    assert_eq!(
        (model.as_str(), effort.as_str(), permission.as_str()),
        ("synthetic-large", "low", "bypass")
    );
    assert_eq!(pending, json!({}));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stale_pending_value_is_dropped_at_start_and_the_launch_keeps_the_effective_one(
    pool: PgPool,
) {
    let (client, id, machine, _dir) = settings_client(&pool).await;
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    start_running(&pool, owner, id).await;
    stop_live_execution(&pool, id).await;
    let changed = client
        .send(
            Method::PATCH,
            &format!("/api/v1/conversations/{id}/settings"),
            Some("settings"),
            Some(json!({"effort":"low"})),
        )
        .await;
    assert_eq!(changed.status, 200);
    // Каталог машины сменился, пока сессия стояла: усилия `low` больше нет.
    sqlx::query("update machine_harnesses set models=$2 where machine_id=$1")
        .bind(machine)
        .bind(json!([{"id":"synthetic-small","efforts":["high"]}]))
        .execute(&pool)
        .await
        .unwrap();

    let started = resume_stopped_session(&client, &pool, id).await;

    assert_eq!(
        start_command(&pool, started.command_id).await.effort,
        "high"
    );
    let (_, effort, _, pending) = settings_columns(&pool, id).await;
    assert_eq!(effort, "high");
    assert_eq!(pending, json!({}));
    let event: Value = sqlx::query_scalar("select payload from events where scope=$1 and kind='conversation_settings_changed' order by scope_seq desc limit 1").bind(format!("conversation:{id}")).fetch_one(&pool).await.unwrap();
    assert_eq!(
        event["rejected"],
        json!({"keys":["effort"],"reason":"settings_unavailable"})
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn start_refused_by_a_live_execution_does_not_promote_pending_settings(pool: PgPool) {
    use canterlot_hub::delivery::dispatch::{self, DispatchOutcome};
    let (client, id, _machine, _dir) = settings_client(&pool).await;
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    start_running(&pool, owner, id).await;
    let path = format!("/api/v1/conversations/{id}");
    let changed = client
        .send(
            Method::PATCH,
            &format!("{path}/settings"),
            Some("settings"),
            Some(json!({"model":"synthetic-large"})),
        )
        .await;
    assert_eq!(changed.status, 200);

    for route in ["resume", "executions"] {
        let body = (route == "resume").then(|| json!({}));
        let refused = client
            .send(Method::POST, &format!("{path}/{route}"), Some(route), body)
            .await;
        assert_eq!(refused.status, 409, "{route}");
    }

    let (model, _, _, pending) = settings_columns(&pool, id).await;
    assert_eq!(model, "synthetic-small");
    assert_eq!(pending, json!({"model":"synthetic-large"}));
    // Живое исполнение по-прежнему получает смену с вводом.
    let submitted = client
        .send(
            Method::POST,
            &format!("{path}/inputs"),
            Some("input"),
            Some(json!({"content":[{"type":"text","text":"text"}]})),
        )
        .await;
    assert_eq!(submitted.status, 201);
    let command_id: Uuid = submitted.json::<Value>()["command_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let outcome = dispatch::dispatch_next(&pool, id, gate(&["pilot_v1"]), Uuid::now_v7())
        .await
        .unwrap();
    assert!(matches!(outcome, DispatchOutcome::Dispatched { .. }));
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload["settings"], json!({"model":"synthetic-large"}));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn preset_changed_on_a_stopped_session_stays_pending_and_arrives_with_the_input(
    pool: PgPool,
) {
    use canterlot_hub::delivery::dispatch::{self, DispatchOutcome};
    let (client, id, _machine, _dir) = settings_client(&pool).await;
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let preset = create_preset(&pool, owner, "preset-1", "Исполнитель", "текст A").await;
    let preset_id = preset["id"].as_str().unwrap();
    start_running(&pool, owner, id).await;
    stop_live_execution(&pool, id).await;
    let path = format!("/api/v1/conversations/{id}");
    let changed = client
        .send(
            Method::PATCH,
            &format!("{path}/settings"),
            Some("settings"),
            Some(json!({"effort":"low","instruction_profile":preset_id})),
        )
        .await;
    assert_eq!(changed.status, 200);

    let started = resume_stopped_session(&client, &pool, id).await;

    let start = start_command(&pool, started.command_id).await;
    assert_eq!(start.effort, "low");
    assert_eq!(start.instructions.expect("snapshot").mode, "clean");
    let (_, effort, _, pending) = settings_columns(&pool, id).await;
    assert_eq!(effort, "low");
    assert_eq!(pending, json!({"instruction_profile":preset_id}));

    let submitted = client
        .send(
            Method::POST,
            &format!("{path}/inputs"),
            Some("input"),
            Some(json!({"content":[{"type":"text","text":"text"}]})),
        )
        .await;
    assert_eq!(submitted.status, 201);
    let command_id: Uuid = submitted.json::<Value>()["command_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let outcome = dispatch::dispatch_next(
        &pool,
        id,
        gate(&["pilot_v1", "instructions_v1"]),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, DispatchOutcome::Dispatched { .. }));
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload["settings"]["instruction_profile"], preset_id);
    assert_eq!(payload["settings"]["instructions"]["revision"], 1);
    assert!(payload["settings"].get("effort").is_none(), "{payload}");
    let mut tx = pool.begin().await.unwrap();
    canterlot_hub::conversations::promote_settings_in_tx(&mut tx, command_id, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let detail = client
        .send(Method::GET, &path, None, None)
        .await
        .json::<Value>();
    assert_eq!(
        detail["settings"]["effective"]["instruction_profile"],
        preset_id
    );
    assert_eq!(detail["settings"]["effective"]["effort"], "low");
    assert_eq!(
        detail["settings"]["effective"]["instruction_preset"]["revision"],
        1
    );
}
