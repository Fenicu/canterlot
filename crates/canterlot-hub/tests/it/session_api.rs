use axum::http::Method;
use canterlot_hub::{
    http::{self, AppState},
    model::ExecutionState,
    testkit::{self, TestClient},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn decision_endpoint_unknown_request_answers_404(pool: PgPool) {
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
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let response = client
        .send(
            Method::POST,
            &format!(
                "/api/v1/conversations/{conversation}/requests/{}/decision",
                Uuid::now_v7()
            ),
            Some("decision"),
            Some(serde_json::json!({"kind":"permission","behavior":"allow"})),
        )
        .await;
    assert_eq!(response.status, 404);
    assert_eq!(response.json::<Value>()["error"]["code"], "not_found");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn queue_refusals_answer_409_with_a_published_reason(pool: PgPool) {
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
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let submitted = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{conversation}/inputs"),
            Some("submit"),
            Some(json!({"content":[{"type":"text","text":"первый"}]})),
        )
        .await;
    assert_eq!(submitted.status, 201);
    let input = submitted.json::<Value>()["input_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let edit_path = format!("/api/v1/conversations/{conversation}/inputs/{input}");
    let refused = |status: u16, code: &str, reason: &str, response: testkit::TestResponse| {
        assert_eq!(response.status, status, "{reason}");
        let body = response.json::<Value>();
        assert_eq!(body["error"]["code"], code, "{reason}");
        assert_eq!(body["error"]["details"]["reason"], reason);
    };

    let stale = client
        .send(
            Method::PATCH,
            &edit_path,
            Some("edit-stale"),
            Some(json!({"text":"другой","expected_revision":99})),
        )
        .await;
    refused(409, "invalid_state", "revision_conflict", stale);

    let edit = json!({"text":"второй","expected_revision":1});
    let first = client
        .send(
            Method::PATCH,
            &edit_path,
            Some("edit-1"),
            Some(edit.clone()),
        )
        .await;
    assert_eq!(first.status, 200);
    let replay = client
        .send(Method::PATCH, &edit_path, Some("edit-1"), Some(edit))
        .await;
    assert_eq!(
        replay.status, 200,
        "a retried edit with its key is a replay"
    );
    assert_eq!(replay.json::<Value>(), first.json::<Value>());
    let revision = first.json::<Value>()["revision"].as_i64().unwrap();

    let empty = client
        .send(
            Method::PATCH,
            &edit_path,
            Some("edit-empty"),
            Some(json!({"text":"  ","expected_revision":revision})),
        )
        .await;
    refused(400, "invalid_request", "empty_content", empty);

    let send_now = client
        .send(
            Method::POST,
            &format!("{edit_path}/send-now"),
            Some("send-now"),
            None,
        )
        .await;
    refused(409, "invalid_state", "no_running_turn", send_now);

    let compact = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{conversation}/compact"),
            Some("compact"),
            None,
        )
        .await;
    refused(409, "invalid_state", "no_live_execution", compact);

    let cancelled = client
        .send(
            Method::POST,
            &format!("/api/v1/inputs/{input}/cancel"),
            Some("cancel"),
            None,
        )
        .await;
    assert_eq!(cancelled.status, 200);
    let late = client
        .send(
            Method::PATCH,
            &edit_path,
            Some("edit-late"),
            Some(json!({"text":"третий","expected_revision":revision})),
        )
        .await;
    refused(409, "invalid_state", "input_not_queued", late);
}

/// Закрытый разговор отвечает на правку и «Отправить сейчас» кодом
/// `conversation_closed`, а NUL в тексте — `nul_character` с кодом 400.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn closed_conversation_and_nul_content_answer_with_published_reasons(pool: PgPool) {
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
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let submitted = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{conversation}/inputs"),
            Some("submit"),
            Some(json!({"content":[{"type":"text","text":"первый"}]})),
        )
        .await;
    assert_eq!(submitted.status, 201);
    let input = submitted.json::<Value>()["input_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let edit_path = format!("/api/v1/conversations/{conversation}/inputs/{input}");
    let refused = |status: u16, code: &str, reason: &str, response: testkit::TestResponse| {
        assert_eq!(response.status, status, "{reason}");
        let body = response.json::<Value>();
        assert_eq!(body["error"]["code"], code, "{reason}");
        assert_eq!(body["error"]["details"]["reason"], reason);
    };

    let nul = client
        .send(
            Method::PATCH,
            &edit_path,
            Some("edit-nul"),
            Some(json!({"text":"до\u{0}после","expected_revision":1})),
        )
        .await;
    refused(400, "invalid_request", "nul_character", nul);

    sqlx::query("update queue_state set closed = true where conversation_id = $1")
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
    let edit = client
        .send(
            Method::PATCH,
            &edit_path,
            Some("edit-closed"),
            Some(json!({"text":"поздно","expected_revision":1})),
        )
        .await;
    refused(409, "conversation_closed", "conversation_closed", edit);
    let send_now = client
        .send(
            Method::POST,
            &format!("{edit_path}/send-now"),
            Some("send-now-closed"),
            None,
        )
        .await;
    refused(409, "conversation_closed", "conversation_closed", send_now);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn submit_cancel_interrupt_stop_continue_map_to_domain_outcomes(pool: PgPool) {
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
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let submitted = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{conversation}/inputs"),
            Some("submit"),
            Some(serde_json::json!({"content":[{"type":"text","text":"сообщение"}]})),
        )
        .await;
    assert_eq!(submitted.status, 201);
    assert_eq!(submitted.json::<Value>()["state"], "queued");
    let input = submitted.json::<Value>()["input_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let cancelled = client
        .send(
            Method::POST,
            &format!("/api/v1/inputs/{input}/cancel"),
            Some("cancel"),
            None,
        )
        .await;
    assert_eq!(cancelled.status, 200);
    assert_eq!(cancelled.json::<Value>(), serde_json::json!("cancelled"));
    for (path, body, expected) in [
        (
            "interrupt",
            None,
            serde_json::json!({"requested":{"interrupt_command_id":null,"withdraw_command_id":null}}),
        ),
        (
            "stop",
            Some(serde_json::json!({})),
            serde_json::json!("nothing_to_stop"),
        ),
        (
            "queue/continue",
            None,
            serde_json::json!({"removed":[{"reason":"user_stop"}],"remaining":[]}),
        ),
    ] {
        let response = client
            .send(
                Method::POST,
                &format!("/api/v1/conversations/{conversation}/{path}"),
                Some(path.replace('/', "_").as_str()),
                body,
            )
            .await;
        assert_eq!(response.status, 200);
        assert_eq!(response.json::<Value>(), expected);
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cancel_replays_the_domain_outcome(pool: PgPool) {
    use canterlot_hub::delivery::submit::{self, ContentBlock, SubmitInput};
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
    let submitted = submit::submit_input(
        &pool,
        SubmitInput {
            owner_id: owner,
            conversation_id: conversation,
            idempotency_key: "submit".into(),
            content: vec![ContentBlock::Text {
                text: "hello".into(),
            }],
            received_at: chrono::Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let path = format!("/api/v1/inputs/{}/cancel", submitted.input_id);
    let first = client.send(Method::POST, &path, Some("cancel"), None).await;
    let replay = client.send(Method::POST, &path, Some("cancel"), None).await;
    assert_eq!(first.status, 200);
    assert_eq!(first.json::<Value>(), serde_json::json!("cancelled"));
    assert_eq!(first.body, replay.body);
    let count: i64 = sqlx::query_scalar("select count(*) from idempotency_keys where key='cancel'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn attachment_ids_are_checked_and_part_of_the_request_hash(pool: PgPool) {
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
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    // Машина без `pilot_v1` в сохранённом отчёте вложений не принимает, ввод не создаётся.
    let result = client.send(Method::POST, &format!("/api/v1/conversations/{conversation}/inputs"), Some("input"), Some(serde_json::json!({"content":[{"type":"text","text":"test"}],"attachment_ids":[Uuid::now_v7()]}))).await;
    assert_eq!(result.status, 409);
    assert_eq!(
        result.json::<Value>()["error"]["code"],
        "capability_missing"
    );
    let inputs: i64 = sqlx::query_scalar("select count(*) from inputs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(inputs, 0);
    let path = format!("/api/v1/conversations/{conversation}/inputs");
    let first = client
        .send(
            Method::POST,
            &path,
            Some("bound"),
            Some(serde_json::json!({"content":[{"type":"text","text":"test"}]})),
        )
        .await;
    assert_eq!(first.status, 201);
    let conflict = client.send(Method::POST, &path, Some("bound"), Some(serde_json::json!({"content":[{"type":"text","text":"test"}],"attachment_ids":[Uuid::now_v7()]}))).await;
    assert_eq!(conflict.status, 409);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn resume_carries_the_last_native_conversation_id(pool: PgPool) {
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
    let id = testkit::seed_conversation(&pool, owner, machine).await;
    let started = canterlot_hub::executions::start(&pool, owner, id, Uuid::now_v7())
        .await
        .unwrap();
    sqlx::query(
        "update executions set state='stopped',native_conversation_id='native-id' where id=$1",
    )
    .bind(started.execution_id)
    .execute(&pool)
    .await
    .unwrap();
    testkit::seed_recorded_turn(&pool, id).await;
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,2,'failed')",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let r = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/resume"),
            Some("resume"),
            Some(serde_json::json!({})),
        )
        .await;
    assert_eq!(r.status, 201);
    let command: Uuid = r.json::<Value>()["command_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload["native_conversation_id"], "native-id");
}

/// Сессия с завершившимся исполнением и клиент владельца; третий элемент держит каталог хаба.
async fn ended_session(
    pool: &PgPool,
    state: ExecutionState,
    outcome: &str,
    native: Option<&str>,
    turn: bool,
) -> (Uuid, TestClient, impl Sized) {
    let (deps, dir) = testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let owner = deps.installation.owner_id;
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
        .bind(machine)
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    let id = testkit::seed_conversation(pool, owner, machine).await;
    let started = canterlot_hub::executions::start(pool, owner, id, Uuid::now_v7())
        .await
        .unwrap();
    sqlx::query("update executions set native_conversation_id=$2 where id=$1")
        .bind(started.execution_id)
        .bind(native)
        .execute(pool)
        .await
        .unwrap();
    if turn {
        testkit::seed_recorded_turn(pool, id).await;
    }
    if state == ExecutionState::Stopped {
        sqlx::query("update executions set state='stopping' where id=$1")
            .bind(started.execution_id)
            .execute(pool)
            .await
            .unwrap();
    }
    canterlot_hub::executions::set_state(
        pool,
        started.execution_id,
        state,
        Some(outcome),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    (id, client, dir)
}

async fn started_payload(pool: &PgPool, r: &canterlot_hub::testkit::TestResponse) -> Value {
    assert_eq!(r.status, 201, "{}", String::from_utf8_lossy(&r.body));
    let command: Uuid = r.json::<Value>()["command_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_stale_start_cannot_drop_the_context_of_a_recorded_turn(pool: PgPool) {
    let (id, client, _dir) = ended_session(
        &pool,
        ExecutionState::Stopped,
        "user_stop",
        Some("native-id"),
        true,
    )
    .await;
    let r = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/executions"),
            Some("stale-start"),
            None,
        )
        .await;
    let payload = started_payload(&pool, &r).await;
    assert_eq!(payload["native_conversation_id"], "native-id");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_start_without_a_recorded_turn_begins_a_new_native_conversation(pool: PgPool) {
    let (id, client, _dir) = ended_session(
        &pool,
        ExecutionState::Stopped,
        "user_stop",
        Some("never-used"),
        false,
    )
    .await;
    let r = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/executions"),
            Some("first-start"),
            None,
        )
        .await;
    let payload = started_payload(&pool, &r).await;
    assert_eq!(payload["native_conversation_id"], Value::Null);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn detail_says_whether_a_turn_was_recorded(pool: PgPool) {
    let (fresh, client, _dir) = ended_session(
        &pool,
        ExecutionState::Stopped,
        "user_stop",
        Some("n"),
        false,
    )
    .await;
    let body: Value = client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{fresh}"),
            None,
            None,
        )
        .await
        .json();
    assert_eq!(body["turn_recorded"], false);
    let (used, client, _dir) =
        ended_session(&pool, ExecutionState::Stopped, "user_stop", Some("n"), true).await;
    let body: Value = client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{used}"),
            None,
            None,
        )
        .await
        .json();
    assert_eq!(body["turn_recorded"], true);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_single_action_clears_a_failed_start_and_starts_an_execution(pool: PgPool) {
    let (id, client, _dir) =
        ended_session(&pool, ExecutionState::Failed, "start_rejected", None, false).await;
    let before: (Value, Value) = sqlx::query_as(
        "select pause_reasons, hold_reasons from queue_state where conversation_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(before.0, json!([{"reason":"fatal_error"}]));
    assert!(
        before
            .1
            .as_array()
            .unwrap()
            .contains(&json!({"reason":"no_live_execution"})),
        "{}",
        before.1
    );
    let r = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/resume"),
            Some("after-failure"),
            Some(json!({"continue_queue":true})),
        )
        .await;
    let payload = started_payload(&pool, &r).await;
    assert_eq!(payload["native_conversation_id"], Value::Null);
    let after: (Value, Value) = sqlx::query_as(
        "select pause_reasons, hold_reasons from queue_state where conversation_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(after.0, json!([]));
    assert!(
        !after
            .1
            .as_array()
            .unwrap()
            .contains(&json!({"reason":"no_live_execution"})),
        "{}",
        after.1
    );
    let live: i64 = sqlx::query_scalar(
        "select count(*) from executions where conversation_id=$1 and state='created'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_single_action_clears_an_exited_process_and_keeps_the_context(pool: PgPool) {
    let (id, client, _dir) = ended_session(
        &pool,
        ExecutionState::Stopped,
        "normal_exit",
        Some("native-id"),
        true,
    )
    .await;
    let r = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/resume"),
            Some("after-exit"),
            Some(json!({"continue_queue":true})),
        )
        .await;
    let payload = started_payload(&pool, &r).await;
    assert_eq!(payload["native_conversation_id"], "native-id");
    let pauses: Value =
        sqlx::query_scalar("select pause_reasons from queue_state where conversation_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pauses, json!([]));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn resume_without_a_recorded_turn_starts_a_new_native_conversation(pool: PgPool) {
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
    let id = testkit::seed_conversation(&pool, owner, machine).await;
    let started = canterlot_hub::executions::start(&pool, owner, id, Uuid::now_v7())
        .await
        .unwrap();
    sqlx::query(
        "update executions set state='stopped',native_conversation_id='never-used' where id=$1",
    )
    .bind(started.execution_id)
    .execute(&pool)
    .await
    .unwrap();
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let r = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/resume"),
            Some("resume"),
            Some(serde_json::json!({})),
        )
        .await;
    assert_eq!(r.status, 201);
    let command: Uuid = r.json::<Value>()["command_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload["native_conversation_id"], Value::Null);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn resume_skips_a_newer_execution_that_never_ran_a_turn(pool: PgPool) {
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
    let id = testkit::seed_conversation(&pool, owner, machine).await;
    let started = canterlot_hub::executions::start(&pool, owner, id, Uuid::now_v7())
        .await
        .unwrap();
    sqlx::query(
        "update executions set state='stopped',native_conversation_id='with-turn' where id=$1",
    )
    .bind(started.execution_id)
    .execute(&pool)
    .await
    .unwrap();
    testkit::seed_recorded_turn(&pool, id).await;
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state,native_conversation_id)
         values($1,$2,2,'stopped','without-turn')",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let r = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/resume"),
            Some("resume"),
            Some(serde_json::json!({})),
        )
        .await;
    assert_eq!(r.status, 201);
    let command: Uuid = r.json::<Value>()["command_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload["native_conversation_id"], "with-turn");
}

#[cfg(feature = "fault-injection")]
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn resume_and_continue_is_one_transaction(pool: PgPool) {
    use canterlot_hub::fault_injection::Point;
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
    let id = testkit::seed_conversation(&pool, owner, machine).await;
    sqlx::query("update queue_state set pause_reasons=$2 where conversation_id=$1")
        .bind(id)
        .bind(serde_json::json!([{"reason":"user_stop"}]))
        .execute(&pool)
        .await
        .unwrap();
    let state = AppState::new(deps);
    state
        .scheduler
        .faults
        .drop_next_at(Point::ResumeAfterStart, &id.to_string());
    let client = TestClient::in_process(http::router(state)).as_owner();
    let r = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/resume"),
            Some("resume"),
            Some(serde_json::json!({"continue_queue":true})),
        )
        .await;
    assert_eq!(r.status, 500);
    let count: i64 = sqlx::query_scalar("select count(*) from executions where conversation_id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let pauses: Value =
        sqlx::query_scalar("select pause_reasons from queue_state where conversation_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pauses, serde_json::json!([{"reason":"user_stop"}]));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn retry_of_must_reference_an_unconfirmed_input_of_the_same_conversation(pool: PgPool) {
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
    let id = testkit::seed_conversation(&pool, owner, machine).await;
    let other = testkit::seed_conversation(&pool, owner, machine).await;
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let path = format!("/api/v1/conversations/{id}/inputs");
    let body = serde_json::json!({"content":[{"type":"text","text":"Первое сообщение"}]});
    let first = client
        .send(Method::POST, &path, Some("first"), Some(body.clone()))
        .await;
    assert_eq!(first.status, 201);
    let input: Uuid = first.json::<Value>()["input_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let retry = serde_json::json!({"content":[{"type":"text","text":"Повтор"}],"retry_of":input});
    let invalid = client
        .send(Method::POST, &path, Some("retry"), Some(retry.clone()))
        .await;
    assert_eq!(invalid.status, 422);
    sqlx::query("update inputs set state='acceptance_unconfirmed' where id=$1")
        .bind(input)
        .execute(&pool)
        .await
        .unwrap();
    let foreign = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{other}/inputs"),
            Some("foreign"),
            Some(retry.clone()),
        )
        .await;
    assert_eq!(foreign.status, 422);
    let good = client
        .send(Method::POST, &path, Some("retry"), Some(retry.clone()))
        .await;
    assert_eq!(good.status, 201);
    let replay = client
        .send(Method::POST, &path, Some("retry"), Some(retry))
        .await;
    assert_eq!(good.body, replay.body);
    let conflict = client
        .send(Method::POST, &path, Some("retry"), Some(body))
        .await;
    assert_eq!(conflict.status, 409);
    let linked: Option<Uuid> = sqlx::query_scalar("select retry_of from inputs where id=$1")
        .bind(
            good.json::<Value>()["input_id"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(linked, Some(input));
    let title: String = sqlx::query_scalar("select title from conversations where id=$1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(title, "Первое сообщение");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn title_is_auto_from_first_input_unless_user_set(pool: PgPool) {
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
    let auto = testkit::seed_conversation(&pool, owner, machine).await;
    let user = testkit::seed_conversation(&pool, owner, machine).await;
    sqlx::query("update conversations set title='Моё имя',title_source='user' where id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    for id in [auto, user] {
        let r = client
            .send(
                Method::POST,
                &format!("/api/v1/conversations/{id}/inputs"),
                Some(&id.to_string()),
                Some(serde_json::json!({"content":[{"type":"text","text":"Первое сообщение"}]})),
            )
            .await;
        assert_eq!(r.status, 201);
    }
    for (id, expected) in [(auto, "Первое сообщение"), (user, "Моё имя")] {
        let title: String = sqlx::query_scalar("select title from conversations where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(title, expected);
    }
    let count: i64 = sqlx::query_scalar(
        "select count(*) from events where scope=$1 and kind='conversation_updated'",
    )
    .bind(format!("owner:{owner}:conversations"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

struct Observed {
    path: String,
    first: testkit::TestResponse,
    replay: testkit::TestResponse,
    conflict: testkit::TestResponse,
    keys_after_first: i64,
    keys_after_replay: i64,
    keys_after_conflict: i64,
}

async fn exercise_mutation_replay(pool: PgPool) -> Vec<Observed> {
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
    let id = testkit::seed_conversation(&pool, owner, machine).await;
    let resume_id = testkit::seed_conversation(&pool, owner, machine).await;
    sqlx::query("update conversations set model='synthetic-small',effort='high' where id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let group_response = client
        .send(
            Method::POST,
            "/api/v1/groups",
            Some("setup_group"),
            Some(serde_json::json!({"name":"Группа"})),
        )
        .await;
    assert_eq!(group_response.status, 201);
    let group = group_response.json::<Value>()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let input_body = serde_json::json!({"content":[{"type":"text","text":"Первое"}]});
    let initial = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/inputs"),
            Some("initial"),
            Some(input_body),
        )
        .await;
    assert_eq!(initial.status, 201);
    let input = initial.json::<Value>()["input_id"]
        .as_str()
        .unwrap()
        .to_string();
    let attachment_body = |name: &str| serde_json::json!({"conversation_id":id,"name":name,"size":3,"sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"});
    let mut attachments = Vec::new();
    for (key, name) in [("setup_upload", "abc.txt"), ("setup_cancel", "cancel.txt")] {
        let created = client
            .send(
                Method::POST,
                "/api/v1/attachments",
                Some(key),
                Some(attachment_body(name)),
            )
            .await;
        assert_eq!(created.status, 201);
        let attachment = created.json::<Value>()["id"].as_str().unwrap().to_owned();
        let uploaded = client
            .send_body(
                Method::PATCH,
                &format!("/api/v1/attachments/{attachment}/content"),
                &[("upload-offset", "0".into())],
                axum::body::Body::from("abc"),
            )
            .await;
        assert_eq!(uploaded.status, 204);
        attachments.push(attachment);
    }
    let (uploaded, to_cancel) = (attachments[0].clone(), attachments[1].clone());
    let cases = [
        (
            Method::POST,
            "/api/v1/conversations".into(),
            Some(
                serde_json::json!({"machine_id":machine,"harness":"synthetic","workdir":"/tmp/a","model":"synthetic-small","effort":"high"}),
            ),
            Some(
                serde_json::json!({"machine_id":machine,"harness":"synthetic","workdir":"/tmp/b","model":"synthetic-small","effort":"high"}),
            ),
        ),
        (
            Method::PATCH,
            format!("/api/v1/conversations/{id}"),
            Some(serde_json::json!({"color":"red"})),
            Some(serde_json::json!({"color":"blue"})),
        ),
        (
            Method::PUT,
            format!("/api/v1/conversations/{id}/groups"),
            Some(serde_json::json!({"group_ids":[group]})),
            Some(serde_json::json!({"group_ids":[]})),
        ),
        (
            Method::PUT,
            format!("/api/v1/conversations/{id}/crunch"),
            Some(serde_json::json!({"enabled":true})),
            Some(serde_json::json!({"enabled":false})),
        ),
        (
            Method::POST,
            "/api/v1/groups".into(),
            Some(serde_json::json!({"name":"Другая"})),
            Some(serde_json::json!({"name":"Третья"})),
        ),
        (
            Method::PATCH,
            format!("/api/v1/groups/{group}"),
            Some(serde_json::json!({"position":5})),
            Some(serde_json::json!({"position":6})),
        ),
        (
            Method::DELETE,
            format!("/api/v1/groups/{group}"),
            None,
            None,
        ),
        (
            Method::PATCH,
            format!("/api/v1/conversations/{id}/settings"),
            Some(serde_json::json!({"effort":"low"})),
            Some(serde_json::json!({"effort":"high"})),
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{id}/inputs"),
            Some(serde_json::json!({"content":[{"type":"text","text":"Следующее"}]})),
            Some(serde_json::json!({"content":[{"type":"text","text":"Другое"}]})),
        ),
        (
            Method::POST,
            format!("/api/v1/inputs/{input}/cancel"),
            None,
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{id}/interrupt"),
            None,
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{id}/stop"),
            Some(serde_json::json!({"force":false})),
            Some(serde_json::json!({"force":true})),
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{id}/queue/continue"),
            None,
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{id}/executions"),
            None,
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{id}/spend-reserve"),
            None,
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{resume_id}/resume"),
            Some(serde_json::json!({"continue_queue":false})),
            Some(serde_json::json!({"continue_queue":true})),
        ),
        (
            Method::POST,
            "/api/v1/attachments".into(),
            Some(attachment_body("one.txt")),
            Some(attachment_body("two.txt")),
        ),
        (
            Method::POST,
            format!("/api/v1/attachments/{uploaded}/complete"),
            None,
            Some(
                serde_json::json!({"sha256":"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"}),
            ),
        ),
        (
            Method::DELETE,
            format!("/api/v1/attachments/{to_cancel}"),
            None,
            None,
        ),
    ];
    let keys = |key: String| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "select count(*) from idempotency_keys where owner_id=$1 and key=$2",
            )
            .bind(owner)
            .bind(key)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let mut observed = Vec::new();
    for (i, (method, path, body, changed)) in cases.into_iter().enumerate() {
        let key = format!("matrix_{i}");
        let first = client
            .send(method.clone(), &path, Some(&key), body.clone())
            .await;
        assert!(first.status.is_success(), "{path}: {}", first.status);
        let keys_after_first = keys(key.clone()).await;
        let replay = client
            .send(method.clone(), &path, Some(&key), body.clone())
            .await;
        let keys_after_replay = keys(key.clone()).await;
        let conflict = if let Some(changed) = changed {
            client.send(method, &path, Some(&key), Some(changed)).await
        } else {
            let other = if path.ends_with("/interrupt") {
                format!("/api/v1/conversations/{id}/queue/continue")
            } else {
                format!("/api/v1/conversations/{id}/interrupt")
            };
            client.send(Method::POST, &other, Some(&key), None).await
        };
        let keys_after_conflict = keys(key).await;
        observed.push(Observed {
            path,
            first,
            replay,
            conflict,
            keys_after_first,
            keys_after_replay,
            keys_after_conflict,
        });
    }
    observed
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn mutations_replay_with_same_key_and_conflict_on_other_body(pool: PgPool) {
    for o in exercise_mutation_replay(pool).await {
        assert_eq!(o.first.status, o.replay.status, "{}", o.path);
        assert_eq!(o.first.body, o.replay.body, "{}", o.path);
        assert_eq!(o.conflict.status, 409, "{}", o.path);
        assert_eq!(
            o.conflict.json::<Value>()["error"]["code"],
            "idempotency_conflict",
            "{}",
            o.path
        );
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn every_mutating_endpoint_records_its_key_once(pool: PgPool) {
    for o in exercise_mutation_replay(pool).await {
        assert_eq!(o.keys_after_first, 1, "{}", o.path);
        assert_eq!(o.keys_after_replay, 1, "{}", o.path);
        assert_eq!(o.keys_after_conflict, 1, "{}", o.path);
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn scheduled_first_message_sets_the_auto_title(pool: PgPool) {
    use canterlot_hub::{
        delivery::ContentBlock,
        dispatcher::Dispatcher,
        scheduler::{self, NewScheduled, Scheduler},
    };
    let seed = testkit::seed(&pool).await;
    let now = chrono::Utc::now();
    scheduler::schedule_message(
        &pool,
        seed.owner_id,
        "scheduled",
        NewScheduled {
            conversation_id: seed.conversation_id,
            run_at: now + chrono::TimeDelta::seconds(2),
            content: vec![ContentBlock::Text {
                text: "Запланированная задача".into(),
            }],
            attachment_ids: vec![],
        },
        now,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let scheduler = Scheduler::new(pool.clone(), Dispatcher::channel().0, Default::default());
    scheduler
        .tick(now + chrono::TimeDelta::seconds(3))
        .await
        .unwrap();
    let title: String = sqlx::query_scalar("select title from conversations where id=$1")
        .bind(seed.conversation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(title, "Запланированная задача");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn foreign_ids_are_not_found_even_when_a_key_is_already_bound(pool: PgPool) {
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
    let id = testkit::seed_conversation(&pool, owner, machine).await;
    let foreign = testkit::seed(&pool).await;
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let body = serde_json::json!({"content":[{"type":"text","text":"text"}]});
    let first = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{id}/inputs"),
            Some("bound"),
            Some(body.clone()),
        )
        .await;
    assert_eq!(first.status, 201);
    let r = client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/inputs", foreign.conversation_id),
            Some("bound"),
            Some(body),
        )
        .await;
    assert_eq!(r.status, 404);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn old_text_input_keys_still_replay_after_upgrade(pool: PgPool) {
    use canterlot_hub::{
        conversations::{self, SubmitMessage},
        delivery::{ContentBlock, SubmitInput, submit_input},
        idempotency,
    };
    let seed = testkit::seed(&pool).await;
    let content = vec![ContentBlock::Text {
        text: "text".into(),
    }];
    let at = chrono::Utc::now();
    let first = submit_input(
        &pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: "old_key".into(),
            content: content.clone(),
            received_at: at,
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    let old_hash = idempotency::request_hash(
        "submit_input",
        &serde_json::json!({"conversation_id":seed.conversation_id,"content":content}),
    );
    sqlx::query("update idempotency_keys set request_hash=$2 where key=$1")
        .bind("old_key")
        .bind(old_hash)
        .execute(&pool)
        .await
        .unwrap();
    let replay = conversations::submit_message(
        &pool,
        SubmitMessage {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            key: "old_key".into(),
            content,
            attachment_ids: vec![],
            retry_of: None,
            received_at: at,
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(first, replay);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn foreign_ids_are_not_found_on_every_control_endpoint(pool: PgPool) {
    use canterlot_hub::delivery::submit::{self, ContentBlock, SubmitInput};
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let foreign = testkit::seed(&pool).await;
    let input = submit::submit_input(
        &pool,
        SubmitInput {
            owner_id: foreign.owner_id,
            conversation_id: foreign.conversation_id,
            idempotency_key: "foreign".into(),
            content: vec![ContentBlock::Text {
                text: "чужое".into(),
            }],
            received_at: chrono::Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let c = foreign.conversation_id;
    let text = serde_json::json!({"content":[{"type":"text","text":"x"}]});
    let cases = [
        (
            Method::PATCH,
            format!("/api/v1/conversations/{c}/settings"),
            Some(serde_json::json!({"effort":"low"})),
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{c}/inputs"),
            Some(text),
        ),
        (
            Method::POST,
            format!(
                "/api/v1/conversations/{c}/requests/{}/decision",
                Uuid::now_v7()
            ),
            Some(serde_json::json!({"kind":"permission","behavior":"allow"})),
        ),
        (
            Method::POST,
            format!("/api/v1/inputs/{}/cancel", input.input_id),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{c}/interrupt"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{c}/stop"),
            Some(serde_json::json!({})),
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{c}/queue/continue"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{c}/executions"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{c}/resume"),
            Some(serde_json::json!({"continue_queue":true})),
        ),
    ];
    for (i, (method, path, body)) in cases.into_iter().enumerate() {
        let r = client
            .send(method, &path, Some(&format!("foreign_{i}")), body)
            .await;
        assert_eq!(r.status, 404, "{path}");
        assert_eq!(r.json::<Value>()["error"]["code"], "not_found", "{path}");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auto_title_is_cut_at_a_word_boundary_and_collapses_whitespace(pool: PgPool) {
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
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let cases = [
        (
            format!("{} {}", "а".repeat(50), "б".repeat(20)),
            format!("{}…", "а".repeat(50)),
        ),
        (
            format!("{} {} {}", "ж".repeat(10), "в".repeat(49), "г".repeat(5)),
            format!("{} {}…", "ж".repeat(10), "в".repeat(49)),
        ),
        (
            format!("{} {}", "д".repeat(70), "е"),
            format!("{}…", "д".repeat(60)),
        ),
        (
            format!("{} {}", "з".repeat(58), "и"),
            format!("{} {}", "з".repeat(58), "и"),
        ),
        (
            format!("{} {}", "к".repeat(60), "л"),
            format!("{}…", "к".repeat(60)),
        ),
        (
            "первая\n\nвторая   строка".to_owned(),
            "первая вторая строка".to_owned(),
        ),
    ];
    for (text, expected) in cases {
        let id = testkit::seed_conversation(&pool, owner, machine).await;
        let r = client
            .send(
                Method::POST,
                &format!("/api/v1/conversations/{id}/inputs"),
                Some(&id.to_string()),
                Some(serde_json::json!({"content":[{"type":"text","text":text}]})),
            )
            .await;
        assert_eq!(r.status, 201);
        let title: String = sqlx::query_scalar("select title from conversations where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(title, expected);
    }
}

/// «Отправить повторно как новое» — решение владельца по неподтверждённому
/// вводу: пауза этого ввода снимается, повтор не ждёт ещё и «Продолжить».
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn retry_as_new_clears_the_unconfirmed_delivery_pause(pool: PgPool) {
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
    let id = testkit::seed_conversation(&pool, owner, machine).await;
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let path = format!("/api/v1/conversations/{id}/inputs");
    let first = client
        .send(
            Method::POST,
            &path,
            Some("first"),
            Some(serde_json::json!({"content":[{"type":"text","text":"Первое"}]})),
        )
        .await;
    assert_eq!(first.status, 201);
    let input: Uuid = first.json::<Value>()["input_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let other = Uuid::now_v7();
    sqlx::query("update inputs set state='acceptance_unconfirmed' where id=$1")
        .bind(input)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update queue_state set pause_reasons=$2 where conversation_id=$1")
        .bind(id)
        .bind(serde_json::json!([
            {"reason":"unconfirmed_delivery","input_id":input},
            {"reason":"unconfirmed_delivery","input_id":other}
        ]))
        .execute(&pool)
        .await
        .unwrap();
    let retry = client
        .send(
            Method::POST,
            &path,
            Some("retry"),
            Some(serde_json::json!({"content":[{"type":"text","text":"Повтор"}],"retry_of":input})),
        )
        .await;
    assert_eq!(retry.status, 201);
    let pauses: Value =
        sqlx::query_scalar("select pause_reasons from queue_state where conversation_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        pauses,
        serde_json::json!([{"reason":"unconfirmed_delivery","input_id":other}])
    );
    let changed: i64 = sqlx::query_scalar(
        "select count(*) from events where kind='queue_state_changed' and conversation_id=$1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(changed >= 1);
}
