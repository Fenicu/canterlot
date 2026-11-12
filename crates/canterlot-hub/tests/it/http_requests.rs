use axum::http::Method;
use canterlot_hub::{
    http::{self, AppState, openapi},
    testkit::{self, TestClient, TestResponse},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const ROUTE: &str = "/api/v1/conversations/{id}/requests/{request_id}/decision";

struct Fixture {
    pool: PgPool,
    client: TestClient,
    no_client_header: TestClient,
    owner: Uuid,
    conversation: Uuid,
    execution: Uuid,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
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
        let execution = Uuid::now_v7();
        sqlx::query(
            "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'running')",
        )
        .bind(execution)
        .bind(conversation)
        .execute(&pool)
        .await
        .unwrap();
        let router = http::router(AppState::new(deps));
        Self {
            pool,
            client: TestClient::in_process(router.clone()).as_owner(),
            no_client_header: TestClient::in_process(router)
                .with_session(testkit::TEST_SESSION_TOKEN)
                .with_header("Origin", testkit::TEST_ORIGIN),
            owner,
            conversation,
            execution,
            _dir: dir,
        }
    }

    async fn request(&self, kind: &str, payload: Value) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query("insert into requests(id,conversation_id,execution_id,generation,kind,native_request_id,state,request_payload) values($1,$2,$3,1,$4,$5,'open',$6)")
            .bind(id).bind(self.conversation).bind(self.execution).bind(kind)
            .bind(id.to_string()).bind(payload).execute(&self.pool).await.unwrap();
        id
    }

    fn path(&self, request: Uuid) -> String {
        format!(
            "/api/v1/conversations/{}/requests/{request}/decision",
            self.conversation
        )
    }

    async fn post(&self, request: Uuid, key: &str, body: Value) -> TestResponse {
        self.client
            .send(Method::POST, &self.path(request), Some(key), Some(body))
            .await
    }

    async fn counts(&self) -> (i64, i64) {
        sqlx::query_as("select (select count(*) from commands where kind='deliver_decision'), (select count(*) from idempotency_keys where owner_id=$1)")
            .bind(self.owner).fetch_one(&self.pool).await.unwrap()
    }
}

fn permission() -> Value {
    json!({"kind":"permission", "behavior":"allow", "message":null})
}
fn question(answer: Value) -> Value {
    json!({"kind":"question", "answers":{"q1":answer}})
}
fn contract() -> Value {
    json!({"questions":[{"id":"q1", "options":[{"label":"Да, точно", "description":"Keep raw"},{"label":"No"}], "multiSelect":false}]})
}
fn assert_error(response: &TestResponse, status: u16, code: &str) -> Value {
    assert_eq!(
        response.status,
        status,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let body = response.json::<Value>();
    assert_eq!(body["error"]["code"], code);
    body["error"]["details"].clone()
}
fn assert_schema(response: &TestResponse, status: &str) {
    let document: Value = serde_json::from_str(&openapi::render()).unwrap();
    let mut schema = document["paths"][ROUTE]["post"]["responses"][status]["content"]["application/json"]["schema"].clone();
    assert!(schema.is_object(), "response schema for {status}");
    schema["components"] = document["components"].clone();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let body = response.json::<Value>();
    let errors: Vec<_> = validator
        .iter_errors(&body)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn post_decision_commits_permission_and_returns_200(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.request("permission", json!({"tool":"Bash"})).await;
    let response = f.post(id, "permission", permission()).await;
    assert_eq!(response.status, 200);
    let body = response.json::<Value>();
    assert_eq!(body["request_id"], id.to_string());
    assert_eq!(body["state"], "decision_saved");
    assert_eq!(body["decision"], permission());
    let winner = body["winner_decision_id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    let stored: (String, Value, Uuid) = sqlx::query_as(
        "select state,decision_payload,winner_decision_id from requests where id=$1",
    )
    .bind(id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(stored, ("decision_saved".into(), permission(), winner));
    let command: (Value, Uuid, i64) = sqlx::query_as("select payload,execution_id,execution_generation from commands where kind='deliver_decision'")
        .fetch_one(&f.pool).await.unwrap();
    assert_eq!(command.0["decision_payload"], permission());
    assert_eq!(command.0["decision_id"], winner.to_string());
    assert_eq!(command.0["request_id"], id.to_string());
    assert_eq!((command.1, command.2), (f.execution, 1));
    assert_eq!(f.counts().await, (1, 1));
    let event: (String, Value) = sqlx::query_as(
        "select kind, payload from events where conversation_id=$1 and kind='request_decided'",
    )
    .bind(f.conversation)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(event.0, "request_decided");
    assert_eq!(event.1["request_id"], id.to_string());
    assert_eq!(event.1["winner_decision_id"], winner.to_string());
    assert_eq!(event.1["decision"], permission());
    assert_schema(&response, "200");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_approval_decision_goes_to_a_job_not_to_the_machine(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let request = f
        .request("spawn_approval", json!({"harness":"codex"}))
        .await;
    assert_error(
        &f.post(request, "wrong", question(json!("Да, точно"))).await,
        422,
        "decision_invalid",
    );
    let response = f.post(request, "key", permission()).await;
    assert_eq!(response.status, 200);
    assert_schema(&response, "200");
    let replay = f.post(request, "key", permission()).await;
    assert_eq!(replay.body, response.body);
    let (commands, _) = f.counts().await;
    assert_eq!(commands, 0);
    let jobs: i64 = sqlx::query_scalar(
        "select count(*) from scheduled_jobs where kind='session_task' and conversation_id=$1
           and payload->>'action'='spawn' and payload->>'request_id'=$2",
    )
    .bind(f.conversation)
    .bind(request.to_string())
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(jobs, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn post_decision_commits_question_answers_and_validates_options(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.request("question", contract()).await;
    for (i, answer) in [
        json!("Да"),
        json!("да, точно"),
        json!("Да, точно "),
        json!(["No"]),
        json!(""),
        json!([]),
    ]
    .into_iter()
    .enumerate()
    {
        let response = f.post(id, &format!("invalid_{i}"), question(answer)).await;
        let details = assert_error(&response, 422, "decision_invalid");
        assert_eq!(details["question_id"], "q1");
        assert_schema(&response, "422");
    }
    assert_eq!(f.counts().await, (0, 0));
    let response = f.post(id, "raw_label", question(json!("Да, точно"))).await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json::<Value>()["decision"],
        question(json!("Да, точно"))
    );
    assert_schema(&response, "200");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn post_decision_validates_multi_question_up_to_four_questions(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let payload = json!({"questions":[
        {"id":"q1", "options":[{"label":"one"}], "multiSelect":false},
        {"id":"q2", "options":[{"label":"A"},{"label":"B"}], "multiSelect":true},
        {"id":"q3", "options":[], "allow_custom":true},
        {"id":"q4", "options":[{"label":"fixed"}], "isOther":true}
    ]});
    let id = f.request("question", payload.clone()).await;
    let valid = json!({"kind":"question", "answers":{"q1":"one", "q2":["B","A"], "q3":"свой ответ", "q4":"other"}});
    let mut invalid = Vec::new();
    let mut missing = valid.clone();
    missing["answers"].as_object_mut().unwrap().remove("q4");
    invalid.push(missing);
    let mut unknown = valid.clone();
    unknown["answers"]["unknown"] = json!("one");
    invalid.push(unknown);
    for value in [
        json!("A"),
        json!(["A", "unknown"]),
        json!(["A", "A"]),
        json!([]),
    ] {
        let mut body = valid.clone();
        body["answers"]["q2"] = value;
        invalid.push(body);
    }
    invalid.push(permission());
    for (i, body) in invalid.into_iter().enumerate() {
        assert_error(
            &f.post(id, &format!("bad_{i}"), body).await,
            422,
            "decision_invalid",
        );
    }
    assert_eq!(f.counts().await, (0, 0));
    assert_eq!(
        f.post(id, "four", valid.clone()).await.json::<Value>()["decision"],
        valid
    );
    for n in [0, 5] {
        let mut malformed = payload.clone();
        malformed["questions"] = json!(
            (0..n)
                .map(|i| json!({"id":format!("q{i}"), "allow_custom":true}))
                .collect::<Vec<_>>()
        );
        let id = f.request("question", malformed).await;
        let body = json!({"kind":"question", "answers":(0..n).map(|i| (format!("q{i}"),json!("x"))).collect::<serde_json::Map<_,_>>()});
        assert_error(
            &f.post(id, &format!("count_{n}"), body).await,
            422,
            "decision_invalid",
        );
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn decision_conflict_returns_409_with_winning_decision_payload(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.request("permission", json!({})).await;
    let deny = json!({"kind":"permission", "behavior":"deny", "message":"Отказ"});
    let (a, b) = tokio::join!(
        f.post(id, "device_a", permission()),
        f.post(id, "device_b", deny)
    );
    let (winner, loser) = if a.status == 200 { (a, b) } else { (b, a) };
    assert_eq!(winner.status, 200);
    let details = assert_error(&loser, 409, "decision_conflict");
    assert_eq!(
        details["winner_decision"],
        winner.json::<Value>()["decision"]
    );
    assert_eq!(details["current_state"], "decision_saved");
    assert_eq!(f.counts().await, (1, 1));
    let events_count: i64 = sqlx::query_scalar(
        "select count(*) from events where conversation_id=$1 and kind='request_decided'",
    )
    .bind(f.conversation)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(events_count, 1);
    assert_schema(&loser, "409");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn identical_key_post_decision_returns_200_replay(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.request("permission", json!({})).await;
    let first = f.post(id, "same", permission()).await;
    assert_eq!(first.status, 200);
    sqlx::query("update requests set state='decision_applied' where id=$1")
        .bind(id)
        .execute(&f.pool)
        .await
        .unwrap();
    let replay = f.post(id, "same", permission()).await;
    assert_eq!(replay.status, 200);
    assert_eq!(replay.body, first.body);
    assert_error(
        &f.post(id, "same", json!({"kind":"permission", "behavior":"deny"}))
            .await,
        409,
        "idempotency_conflict",
    );
    let other = f.request("permission", json!({})).await;
    assert_error(
        &f.post(other, "same", permission()).await,
        409,
        "idempotency_conflict",
    );
    assert_eq!(f.counts().await, (1, 1));
    let events_count: i64 = sqlx::query_scalar(
        "select count(*) from events where conversation_id=$1 and kind='request_decided'",
    )
    .bind(f.conversation)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(events_count, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn post_decision_on_closed_request_returns_409_with_terminal_state(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.request("permission", json!({})).await;
    sqlx::query("update requests set state='closed_without_decision' where id=$1")
        .bind(id)
        .execute(&f.pool)
        .await
        .unwrap();
    let response = f.post(id, "closed", permission()).await;
    let details = assert_error(&response, 409, "request_not_open");
    assert_eq!(details["current_state"], "closed_without_decision");
    assert_eq!(details["reason"], "closed_without_decision");
    assert_schema(&response, "409");
    let id = f.request("permission", json!({})).await;
    sqlx::query("update executions set state='stopped' where id=$1")
        .bind(f.execution)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,2,'running')",
    )
    .bind(Uuid::now_v7())
    .bind(f.conversation)
    .execute(&f.pool)
    .await
    .unwrap();
    let details = assert_error(
        &f.post(id, "advanced", permission()).await,
        409,
        "request_not_open",
    );
    assert_eq!(details["reason"], "generation_advanced");
    assert_eq!(f.counts().await, (0, 0));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn post_decision_requires_idempotency_key_and_client_header(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.request("permission", json!({})).await;
    assert_error(
        &f.client
            .send(Method::POST, &f.path(id), None, Some(permission()))
            .await,
        400,
        "invalid_request",
    );
    assert_error(
        &f.no_client_header
            .send(
                Method::POST,
                &f.path(id),
                Some("no_client"),
                Some(permission()),
            )
            .await,
        403,
        "origin_rejected",
    );
    for body in [
        json!({}),
        json!({"kind":"permission", "behavior":"maybe"}),
        json!({"kind":"permission", "behavior":"allow", "unknown":true}),
    ] {
        assert_error(&f.post(id, "malformed", body).await, 400, "invalid_request");
    }
    assert_eq!(f.counts().await, (0, 0));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn post_decision_unknown_or_foreign_request_is_not_found(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let response = f.post(Uuid::now_v7(), "unknown", permission()).await;
    assert_error(&response, 404, "not_found");
    assert_schema(&response, "404");
    let id = f.request("permission", json!({})).await;
    let other = testkit::seed(&f.pool).await;
    sqlx::query("update conversations set owner_id=$2 where id=$1")
        .bind(f.conversation)
        .bind(other.owner_id)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_error(&f.post(id, "foreign", permission()).await, 404, "not_found");
    assert_eq!(f.counts().await, (0, 0));
}

#[test]
fn openapi_document_drift_test_matches_requests_api() {
    let rendered = openapi::render();
    assert_eq!(rendered, include_str!("../../../../docs/api/openapi.json"));
    let document: Value = serde_json::from_str(&rendered).unwrap();
    let operation = &document["paths"][ROUTE]["post"];
    for status in ["200", "400", "404", "409", "422"] {
        assert!(operation["responses"][status].is_object(), "{status}");
    }
    assert!(operation["responses"]["501"].is_null());
    let variants = document["components"]["schemas"]["DecisionBody"]["oneOf"]
        .as_array()
        .unwrap();
    assert_eq!(variants.len(), 2);
    for variant in variants {
        assert!(
            variant["required"]
                .as_array()
                .unwrap()
                .contains(&json!("kind"))
        );
        assert_eq!(
            variant["properties"]["kind"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    assert!(
        operation["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "X-Canterlot-Client" && p["required"] == true)
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_same_key_decisions_return_the_same_winner_once(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.request("permission", json!({})).await;
    let (a, b) = tokio::join!(
        f.post(id, "same_device", permission()),
        f.post(id, "same_device", permission()),
    );
    assert_eq!(a.status, 200);
    assert_eq!(b.status, 200);
    assert_eq!(a.body, b.body);
    assert_eq!(f.counts().await, (1, 1));
    let events_count: i64 = sqlx::query_scalar(
        "select count(*) from events where conversation_id=$1 and kind='request_decided'",
    )
    .bind(f.conversation)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(events_count, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn post_decision_refuses_nul_in_text_and_keeps_the_request_open(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let permission_request = f.request("permission", json!({"tool":"Bash"})).await;
    let response = f
        .post(
            permission_request,
            "permission",
            json!({"kind":"permission","behavior":"deny","message":"нет\u{0}"}),
        )
        .await;
    assert_error(&response, 422, "decision_invalid");

    let single = f
        .request(
            "question",
            json!({"questions":[{"id":"q1","options":[],"allow_custom":true}]}),
        )
        .await;
    let response = f.post(single, "single", question(json!("a\u{0}b"))).await;
    let details = assert_error(&response, 422, "decision_invalid");
    assert_eq!(details["question_id"], "q1");

    let multiple = f
        .request(
            "question",
            json!({"questions":[{"id":"q1","options":[],"allow_custom":true,"multiSelect":true}]}),
        )
        .await;
    let response = f
        .post(multiple, "multiple", question(json!(["ok", "\u{0}"])))
        .await;
    let details = assert_error(&response, 422, "decision_invalid");
    assert_eq!(details["question_id"], "q1");

    assert_eq!(f.counts().await, (0, 0));
    for request in [permission_request, single, multiple] {
        let state: String = sqlx::query_scalar("select state from requests where id=$1")
            .bind(request)
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(state, "open");
    }
    let accepted = f.post(single, "single-fixed", question(json!("ab"))).await;
    assert_eq!(accepted.status, 200);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn decision_takes_the_queue_lock_before_the_request_row(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let request = f.request("permission", json!({"tool":"Bash"})).await;
    // Пакет SJ держит очередь с первой записи и позже берёт счётчик событий и строку запроса.
    let mut batch = f.pool.begin().await.unwrap();
    sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
        .bind(f.conversation)
        .execute(&mut *batch)
        .await
        .unwrap();
    let (client, path) = (f.client.clone(), f.path(request));
    let decision = tokio::spawn(async move {
        client
            .send(Method::POST, &path, Some("decision"), Some(permission()))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "select count(*) from pg_stat_activity
                 where datname=current_database() and wait_event_type='Lock'
                   and query like '%queue_state%'",
            )
            .fetch_one(&f.pool)
            .await
            .unwrap();
            if waiting > 0 {
                break;
            }
            assert!(
                !decision.is_finished(),
                "the decision committed while the queue was locked"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let row_free: Option<Uuid> =
        sqlx::query_scalar("select id from requests where id=$1 for update skip locked")
            .bind(request)
            .fetch_optional(&mut *batch)
            .await
            .unwrap();
    assert_eq!(row_free, Some(request));
    batch.rollback().await.unwrap();
    assert_eq!(decision.await.unwrap().status, 200);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn post_decision_free_text_requires_an_explicit_true_contract_flag(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for flag in ["allow_custom", "isOther", "is_other"] {
        for value in [json!(true), json!(false), json!("true"), Value::Null] {
            let mut payload = contract();
            payload["questions"][0][flag] = value.clone();
            let id = f.request("question", payload).await;
            let response = f
                .post(id, &id.to_string(), question(json!("Свой ответ")))
                .await;
            if value == true {
                assert_eq!(response.status, 200);
                assert_eq!(
                    response.json::<Value>()["decision"],
                    question(json!("Свой ответ"))
                );
            } else {
                assert_error(&response, 422, "decision_invalid");
            }
        }
    }
    let id = f.request("question", json!({"questions":[{"id":"q1", "multi_select":true, "options":[{"label":"A"},{"label":"B"}]}]})).await;
    assert_eq!(
        f.post(id, "snake_multi", question(json!(["B", "A"])))
            .await
            .status,
        200
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn decision_record_failure_rolls_back_the_winner_command_and_key(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.request("permission", json!({})).await;
    sqlx::raw_sql("create function reject_decision_key() returns trigger language plpgsql as $$ begin if new.key='fail_record' then raise exception 'test record failure'; end if; return new; end $$;
        create trigger reject_decision_key before insert on idempotency_keys for each row execute function reject_decision_key();")
        .execute(&f.pool).await.unwrap();
    let response = f.post(id, "fail_record", permission()).await;
    assert_error(&response, 500, "internal");
    assert_eq!(f.counts().await, (0, 0));
    let row: (String, Option<Uuid>, Option<Value>, Option<String>) = sqlx::query_as("select state,winner_decision_id,decision_payload,idempotency_key from requests where id=$1")
        .bind(id).fetch_one(&f.pool).await.unwrap();
    assert_eq!(row, ("open".into(), None, None, None));
    assert_eq!(f.post(id, "after_rollback", permission()).await.status, 200);
    assert_eq!(f.counts().await, (1, 1));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn post_decision_rejects_changed_payload_after_idempotency_retention(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.request("permission", json!({})).await;
    let first = f.post(id, "retained_request", permission()).await;
    assert_eq!(first.status, 200);
    sqlx::query("delete from idempotency_keys where owner_id=$1")
        .bind(f.owner)
        .execute(&f.pool)
        .await
        .unwrap();
    let changed = json!({"kind":"permission", "behavior":"deny", "message":null});
    assert_error(
        &f.post(id, "retained_request", changed).await,
        409,
        "idempotency_conflict",
    );
    assert_eq!(f.counts().await, (1, 0));
    let replay = f.post(id, "retained_request", permission()).await;
    assert_eq!(replay.status, 200);
    assert_eq!(replay.body, first.body);
    assert_eq!(f.counts().await, (1, 1));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn post_decision_for_missing_request_rolls_back_synchronously(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let mut probe = f.pool.acquire().await.unwrap();
    for _ in 0..10 {
        let missing = Uuid::now_v7();
        let response = f.post(missing, "rollback-probe", permission()).await;
        assert_error(&response, 404, "not_found");
        // Строка разговора блокировалась FOR NO KEY UPDATE: пропуск — висячий откат.
        let free: Option<Uuid> = sqlx::query_scalar(
            "select id from conversations where id=$1 for no key update skip locked",
        )
        .bind(f.conversation)
        .fetch_optional(&mut *probe)
        .await
        .unwrap();
        assert!(free.is_some(), "conversation row still locked");
    }
}

#[derive(Clone, Default)]
struct LogBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

/// Подписчик на весь процесс: при единственном `set_default` точка логирования, впервые
/// сработавшая в параллельном тесте, выключается навсегда и журнал остаётся пустым.
fn captured_logs() -> &'static LogBuffer {
    static LOGS: std::sync::OnceLock<LogBuffer> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = LogBuffer::default();
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_max_level(tracing::Level::TRACE)
                .with_writer(logs.clone())
                .finish(),
        )
        .expect("one global subscriber");
        logs
    })
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn secret_answers_reach_only_the_delivery_command(pool: PgPool) {
    const SECRET: &str = "hunter2-very-secret";
    let logs = captured_logs();
    let f = Fixture::new(pool).await;
    let request = f
        .request(
            "question",
            json!({"questions":[
                {"id":"pin", "text":"PIN?", "options":[], "allow_custom":true, "is_secret":true},
                {"id":"go", "text":"Дальше?", "options":[{"label":"Да"}]}
            ]}),
        )
        .await;
    let body = json!({"kind":"question", "answers":{"pin":SECRET, "go":"Да"}});
    let response = f.post(request, "secret-key", body.clone()).await;
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let view = response.json::<Value>();
    assert_eq!(view["decision"]["answers"]["go"], "Да");
    assert!(view["decision"]["answers"]["pin"].is_string());
    let replay = f.post(request, "secret-key", body.clone()).await;
    assert_eq!(replay.status, 200);
    let conflict = f
        .post(
            request,
            "other-key",
            json!({"kind":"question", "answers":{"pin":"other", "go":"Да"}}),
        )
        .await;
    assert_error(&conflict, 409, "decision_conflict");
    let snapshot = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{}/snapshot", f.conversation),
            None,
            None,
        )
        .await;
    assert_eq!(snapshot.status, 200);
    let stored: Vec<String> = sqlx::query_scalar(
        "select payload::text from events
         union all select response::text from idempotency_keys
         union all select coalesce(decision_payload::text, '') from requests",
    )
    .fetch_all(&f.pool)
    .await
    .unwrap();
    assert!(
        !logs.0.lock().unwrap().is_empty(),
        "the log check needs captured output"
    );
    for (place, text) in [
        (
            "response",
            String::from_utf8_lossy(&response.body).into_owned(),
        ),
        ("replay", String::from_utf8_lossy(&replay.body).into_owned()),
        (
            "conflict",
            String::from_utf8_lossy(&conflict.body).into_owned(),
        ),
        (
            "snapshot",
            String::from_utf8_lossy(&snapshot.body).into_owned(),
        ),
        ("stored", stored.join("\n")),
        (
            "logs",
            String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned(),
        ),
    ] {
        assert!(!text.contains(SECRET), "secret answer leaked into {place}");
    }
    // Команде доставки ответ нужен целиком, пока решение не дошло до воркера.
    let command: Value =
        sqlx::query_scalar("select payload from commands where kind = 'deliver_decision'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(command["decision_payload"]["answers"]["pin"], SECRET);
    canterlot_hub::executions::set_state(
        &f.pool,
        f.execution,
        canterlot_hub::model::ExecutionState::Stopped,
        Some("machine_rebooted"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let command: String =
        sqlx::query_scalar("select payload::text from commands where kind = 'deliver_decision'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(
        !command.contains(SECRET),
        "completed command keeps the secret"
    );
}

#[test]
fn redaction_covers_every_form_of_a_secret_question() {
    use canterlot_hub::requests::{REDACTED_ANSWER, redact_decision};
    let decision = json!({"kind":"question", "answers":{"a":"pin", "b":["x","y"], "c":"open"}});
    let by_question = json!({"questions":[
        {"id":"a", "is_secret":true},
        {"id":"b", "isSecret":true},
        {"id":"c", "is_secret":false}
    ]});
    assert_eq!(
        redact_decision(&by_question, &decision),
        json!({"kind":"question", "answers":{"a":REDACTED_ANSWER, "b":REDACTED_ANSWER, "c":"open"}})
    );
    assert_eq!(
        redact_decision(&json!({"is_secret":true}), &decision)["answers"],
        json!({"a":REDACTED_ANSWER, "b":REDACTED_ANSWER, "c":REDACTED_ANSWER})
    );
    let permission = json!({"kind":"permission", "behavior":"deny", "message":"нет"});
    assert_eq!(redact_decision(&by_question, &permission), permission);
}
