use axum::http::Method;
use canterlot_hub::{
    activity,
    conversations::{self, AgentListQuery, ListFilter, ListQuery},
    executions,
    http::{self, AppState},
    model::ExecutionState,
    search::{self, SearchQuery},
    session::{CAPABILITY_SIDE_CHAT_V1, Registry, TestLease},
    side,
    testkit::{self, TestClient},
};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    pool: PgPool,
    client: TestClient,
    owner: Uuid,
    machine: Uuid,
    registry: Registry,
    leases: Vec<TestLease>,
    _dir: tempfile::TempDir,
}

async fn fixture(pool: PgPool) -> Fixture {
    let (deps, dir) = testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let registry = deps.registry.clone();
    let owner = deps.installation.owner_id;
    let machine = Uuid::now_v7();
    sqlx::query(
        "insert into machines(id,owner_id,name,availability) values($1,$2,'Машина','available')",
    )
    .bind(machine)
    .bind(owner)
    .execute(&pool)
    .await
    .unwrap();
    testkit::seed_catalog(&pool, machine).await;
    use sha2::Digest;
    sqlx::query("insert into machine_credentials(machine_id,token_sha256) values($1,$2)")
        .bind(machine)
        .bind(sha2::Sha256::digest(machine.as_bytes()).as_slice())
        .execute(&pool)
        .await
        .unwrap();
    testkit::seed_test_session(&pool, owner).await;
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let mut f = Fixture {
        pool,
        client,
        owner,
        machine,
        registry,
        leases: Vec::new(),
        _dir: dir,
    };
    f.caps(&["pilot_v1", "instructions_v1", CAPABILITY_SIDE_CHAT_V1]);
    f
}

impl Fixture {
    fn caps(&mut self, capabilities: &[&str]) {
        self.leases.clear();
        let caps: Vec<String> = capabilities.iter().map(|c| (*c).to_owned()).collect();
        self.leases
            .push(self.registry.connect_for_tests(self.machine, &caps));
    }

    fn lose_side_capability(&mut self) {
        self.caps(&["pilot_v1", "instructions_v1"]);
    }

    async fn parent(&self, harness: &str) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into conversations(id,owner_id,machine_id,harness,workdir,title,model,effort,
                                        permission_mode,instruction_profile)
             values($1,$2,$3,$4,'/tmp/w','Родитель','synthetic-small','low','bypass','clean')",
        )
        .bind(id)
        .bind(self.owner)
        .bind(self.machine)
        .bind(harness)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query("insert into queue_state(conversation_id,hold_reasons) values($1,'[]')")
            .bind(id)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query("insert into conversation_derived(conversation_id,activity_evidence) values($1,'{\"kind\":\"idle\",\"last_turn_at\":null,\"background_work\":null}')")
            .bind(id)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            "insert into executions(id,conversation_id,generation,state,native_conversation_id)
             values($1,$2,1,'stopped','parent-native')",
        )
        .bind(Uuid::now_v7())
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    async fn parent_without_native(&self) -> Uuid {
        let id = self.parent("synthetic").await;
        sqlx::query("update executions set native_conversation_id=null where conversation_id=$1")
            .bind(id)
            .execute(&self.pool)
            .await
            .unwrap();
        id
    }

    async fn create_side(&self, parent: Uuid) -> (u16, Uuid) {
        let response = self
            .client
            .send(
                Method::POST,
                &format!("/api/v1/conversations/{parent}/side"),
                None,
                None,
            )
            .await;
        let status = response.status.as_u16();
        let body: Value = response.json();
        (
            status,
            body["conversation_id"].as_str().unwrap().parse().unwrap(),
        )
    }

    async fn side(&self, parent: Uuid) -> Uuid {
        let (status, id) = self.create_side(parent).await;
        assert!(status == 201 || status == 200);
        id
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_endpoint_creates_returns_existing_and_deletes(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let (status, side_id) = f.create_side(parent).await;
    assert_eq!(status, 201);
    let (again, same) = f.create_side(parent).await;
    assert_eq!(again, 200);
    assert_eq!(same, side_id);
    let row: (String, String, Option<Uuid>, i32, Option<Uuid>, String) = sqlx::query_as(
        "select machine_id::text,permission_mode,parent_conversation_id,spawn_depth,side_of,workdir
           from conversations where id=$1",
    )
    .bind(side_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(row.0, f.machine.to_string());
    assert_eq!(row.1, "read_only");
    assert_eq!(row.2, None);
    assert_eq!(row.3, 0);
    assert_eq!(row.4, Some(parent));
    assert_eq!(row.5, "/tmp/w");

    let response = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{parent}/side"),
            None,
            None,
        )
        .await;
    assert_eq!(response.status.as_u16(), 204);
    let exists: bool = sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
        .bind(side_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert!(!exists);
    let (status, _) = f.create_side(parent).await;
    assert_eq!(status, 201, "после удаления копия создаётся заново");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_endpoint_refuses_capability_native_and_nested(pool: PgPool) {
    let mut f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    f.lose_side_capability();
    let response = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{parent}/side"),
            None,
            None,
        )
        .await;
    assert_eq!(response.status.as_u16(), 409);
    assert_eq!(
        response.json::<Value>()["error"]["code"],
        "capability_missing"
    );

    f.caps(&["pilot_v1", "instructions_v1", CAPABILITY_SIDE_CHAT_V1]);
    let fresh = f.parent_without_native().await;
    let response = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{fresh}/side"),
            None,
            None,
        )
        .await;
    assert_eq!(response.status.as_u16(), 409);
    assert_eq!(
        response.json::<Value>()["error"]["details"]["reason"],
        "side_not_started"
    );

    let side_id = f.side(parent).await;
    let response = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{side_id}/side"),
            None,
            None,
        )
        .await;
    assert_eq!(response.status.as_u16(), 409);
    assert_eq!(
        response.json::<Value>()["error"]["details"]["reason"],
        "side_of_side"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_endpoint_refuses_opencode_mid_turn_and_releases_on_idle(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("opencode").await;
    sqlx::query(
        "update conversation_derived set activity_state='working' where conversation_id=$1",
    )
    .bind(parent)
    .execute(&f.pool)
    .await
    .unwrap();
    let response = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{parent}/side"),
            None,
            None,
        )
        .await;
    assert_eq!(response.status.as_u16(), 409);
    assert_eq!(
        response.json::<Value>()["error"]["details"]["reason"],
        "side_parent_busy"
    );
    sqlx::query("update conversation_derived set activity_state='idle' where conversation_id=$1")
        .bind(parent)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(f.create_side(parent).await.0, 201);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_input_holds_while_opencode_parent_is_busy(pool: PgPool) {
    let mut f = fixture(pool.clone()).await;
    let parent = f.parent("opencode").await;
    let side_id = f.side(parent).await;
    sqlx::query(
        "update conversation_derived set activity_state='working' where conversation_id=$1",
    )
    .bind(parent)
    .execute(&f.pool)
    .await
    .unwrap();
    let response = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{side_id}/inputs"),
            Some("side-hold"),
            Some(json!({"content":[{"type":"text","text":"вопрос"}]})),
        )
        .await;
    assert_eq!(response.status.as_u16(), 201);
    let holds: Value =
        sqlx::query_scalar("select hold_reasons from queue_state where conversation_id=$1")
            .bind(side_id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(holds.to_string().contains("side_parent_busy"));
    let live: bool = sqlx::query_scalar(
        "select exists(select 1 from executions where conversation_id=$1 and state not in ('stopped','failed'))",
    )
    .bind(side_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(!live, "копия не стартует при активном родителе");

    sqlx::query("update conversation_derived set activity_state='idle' where conversation_id=$1")
        .bind(parent)
        .execute(&f.pool)
        .await
        .unwrap();
    side::sweep(&f.pool, Utc::now()).await.unwrap();
    let live: bool = sqlx::query_scalar(
        "select exists(select 1 from executions where conversation_id=$1 and state not in ('stopped','failed'))",
    )
    .bind(side_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(live, "простой родителя отпускает ввод и стартует копию");
    f.leases.clear();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_start_payload_forks_from_parent_and_repeats_instruction(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    executions::start(&f.pool, f.owner, side_id, Uuid::now_v7())
        .await
        .unwrap();
    let payload: Value = sqlx::query_scalar(
        "select payload from commands where conversation_id=$1 and kind='start_execution'
         order by created_at desc limit 1",
    )
    .bind(side_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(payload["fork_from_native"], "parent-native");
    assert_eq!(payload["canterlot_mcp"], "side");
    assert_eq!(payload["isolate_mcp"], true);
    assert_eq!(payload["permission_mode"], "read_only");
    assert!(!payload.to_string().contains("delegate_mcp_servers"));
    assert!(
        payload["instructions"]["text"]
            .as_str()
            .unwrap()
            .contains(side::SIDE_INSTRUCTION)
    );
    let boundary: Value = sqlx::query_scalar("select side_boundary from conversations where id=$1")
        .bind(side_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert!(boundary.get("seq").is_some());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_outbox_requires_side_chat_capability(pool: PgPool) {
    let mut f = fixture(pool).await;
    f.lose_side_capability();
    let row = canterlot_hub::outbox::CommandRow {
        id: Uuid::now_v7(),
        machine_id: f.machine,
        conversation_id: Some(Uuid::now_v7()),
        execution_id: Some(Uuid::now_v7()),
        execution_generation: Some(1),
        kind: "start_execution".into(),
        payload: json!({"harness":"synthetic","workdir":"/tmp/w","canterlot_mcp":"side",
                         "fork_from_native":"parent-native","instructions":{"mode":"clean","text":"x"}}),
        dispatch_attempt: 0,
    };
    assert!(matches!(
        canterlot_hub::outbox::to_proto_for_session(&row, &f.registry),
        Err(canterlot_hub::outbox::OutboxError::CapabilityMissing)
    ));
    f.caps(&["pilot_v1", "instructions_v1", CAPABILITY_SIDE_CHAT_V1]);
    assert!(canterlot_hub::outbox::to_proto_for_session(&row, &f.registry).is_ok());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_preface_carries_state_todos_and_texts_after_boundary(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    sqlx::query("insert into inputs(id,conversation_id,owner_id,input_seq,idempotency_key,content,state,command_id,received_at,created_seq,feed_seq) select $1,$2,$3,1,'k1','[{\"type\":\"text\",\"text\":\"раньше\"}]','accepted',c.id,now(),1,1 from commands c limit 1")
        .bind(Uuid::now_v7())
        .bind(parent)
        .bind(f.owner)
        .execute(&f.pool)
        .await
        .ok();
    sqlx::query("update conversation_derived set activity_state='working', activity_evidence='{\"kind\":\"working\",\"phase\":\"tools\"}' where conversation_id=$1")
        .bind(parent)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("insert into agent_todos(conversation_id,lane_block_id,generation,source_revision,revision,items,omitted) values($1,null,1,1,1,'[{\"text\":\"проверить\",\"status\":\"pending\"}]',0)")
        .bind(parent)
        .execute(&f.pool)
        .await
        .unwrap();
    let cmd = Uuid::now_v7();
    sqlx::query("insert into commands(id,machine_id,conversation_id,kind,delivery_state,payload) values($1,$2,$3,'deliver_input','held','{}')")
        .bind(cmd)
        .bind(f.machine)
        .bind(parent)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("insert into inputs(id,conversation_id,owner_id,input_seq,idempotency_key,content,state,command_id,received_at,created_seq) values($1,$2,$3,2,'k2','[{\"type\":\"text\",\"text\":\"что дальше?\"}]','accepted',$4,now(),5)")
        .bind(Uuid::now_v7())
        .bind(parent)
        .bind(f.owner)
        .bind(cmd)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,created_seq) select $1,id,$2,'text','final',1,$3,6 from executions where conversation_id=$2 limit 1")
        .bind(Uuid::now_v7())
        .bind(parent)
        .bind("первая строка\nсередина\nпоследняя строка")
        .execute(&f.pool)
        .await
        .unwrap();
    // Граница — до этих сообщений: сводка увидит оба.
    sqlx::query("update conversations set side_boundary='{\"seq\":4}' where id=$1")
        .bind(side_id)
        .execute(&f.pool)
        .await
        .unwrap();
    let mut conn = f.pool.acquire().await.unwrap();
    let text = side::compose_preface(&mut conn, side_id)
        .await
        .unwrap()
        .unwrap();
    assert!(text.contains("working"));
    assert!(text.contains("проверить"));
    assert!(text.contains("что дальше?"));
    assert!(text.contains("первая строка"));
    assert!(text.contains("последняя строка"));
    assert!(!text.contains("раньше"));
    assert!(text.chars().count() <= 2000);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_preface_is_fixed_on_the_command_across_retry(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    executions::start(&f.pool, f.owner, side_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        let id: Uuid = sqlx::query_scalar("select id from executions where conversation_id=$1")
            .bind(side_id)
            .fetch_one(&f.pool)
            .await
            .unwrap();
        executions::set_state(&f.pool, id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    let response = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{side_id}/inputs"),
            Some("preface-1"),
            Some(json!({"content":[{"type":"text","text":"вопрос"}]})),
        )
        .await;
    assert_eq!(response.status.as_u16(), 201);
    let outcome = canterlot_hub::delivery::dispatch::dispatch_next(
        &f.pool,
        side_id,
        canterlot_hub::delivery::dispatch::GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        canterlot_hub::delivery::dispatch::DispatchOutcome::Dispatched { .. }
    ));
    let first: Option<String> = sqlx::query_scalar(
        "select payload->>'side_preface' from commands where conversation_id=$1 and kind='deliver_input'",
    )
    .bind(side_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    // Повторная выдача не пересобирает сводку: ввод снова в очереди, слот свободен.
    sqlx::query("update inputs set state='queued' where conversation_id=$1 and state='dispatched'")
        .bind(side_id)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("update queue_state set run_slot='free',run_slot_input_id=null,run_slot_attempt=null where conversation_id=$1")
        .bind(side_id)
        .execute(&f.pool)
        .await
        .unwrap();
    let _ = canterlot_hub::delivery::dispatch::dispatch_next(
        &f.pool,
        side_id,
        canterlot_hub::delivery::dispatch::GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: vec!["pilot_v1".into()],
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let second: Option<String> = sqlx::query_scalar(
        "select payload->>'side_preface' from commands where conversation_id=$1 and kind='deliver_input'",
    )
    .bind(side_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(first, second);
    assert!(first.as_deref().is_some_and(|text| !text.is_empty()));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_hidden_from_lists_counts_and_snapshot(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    let mut conn = f.pool.acquire().await.unwrap();
    let counts = conversations::counts(&mut conn, f.owner).await.unwrap();
    assert_eq!(counts.all, 1);
    for filter in [
        ListFilter::All,
        ListFilter::Pinned,
        ListFilter::Archived,
        ListFilter::Trashed,
    ] {
        let page = conversations::list(
            &mut conn,
            &ListQuery {
                owner_id: f.owner,
                filter,
                q: None,
                cursor: None,
                limit: 50,
            },
        )
        .await
        .unwrap();
        assert!(
            page.items.iter().all(|item| item.attributes.id != side_id),
            "боковой чат попал в список"
        );
    }
    let title = conversations::list(
        &mut conn,
        &ListQuery {
            owner_id: f.owner,
            filter: ListFilter::All,
            q: Some("Родитель".into()),
            cursor: None,
            limit: 50,
        },
    )
    .await
    .unwrap();
    assert!(title.items.iter().all(|item| item.attributes.id != side_id));
    let snapshot = conversations::snapshot_items(&mut conn, f.owner)
        .await
        .unwrap();
    assert!(snapshot.iter().all(|item| item.attributes.id != side_id));
    let group = Uuid::now_v7();
    sqlx::query("insert into conversation_groups(id,owner_id,name,position) values($1,$2,'g',0)")
        .bind(group)
        .bind(f.owner)
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("insert into conversation_group_members(group_id,conversation_id) values($1,$2)")
        .bind(group)
        .bind(side_id)
        .execute(&mut *conn)
        .await
        .unwrap();
    let page = conversations::list(
        &mut conn,
        &ListQuery {
            owner_id: f.owner,
            filter: ListFilter::Group(group),
            q: None,
            cursor: None,
            limit: 50,
        },
    )
    .await
    .unwrap();
    assert!(page.items.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_hidden_from_agent_sessions_and_machine_sessions(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    let mut conn = f.pool.acquire().await.unwrap();
    let items = conversations::agent_list(
        &mut conn,
        &AgentListQuery {
            owner_id: f.owner,
            family: None,
            state: None,
            include_archived: true,
            caller: parent,
            domain: None,
            limit: 50,
        },
    )
    .await
    .unwrap();
    assert!(items.iter().all(|item| item.attributes.id != side_id));
    drop(conn);
    let response = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/machines/{}", f.machine),
            None,
            None,
        )
        .await;
    let body: Value = response.json();
    let text = body.to_string();
    assert!(!text.contains(&side_id.to_string()));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_hidden_from_search_bookmarks_presets_and_plan(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    let mut conn = f.pool.acquire().await.unwrap();
    let command = Uuid::now_v7();
    sqlx::query("insert into commands(id,machine_id,conversation_id,kind,delivery_state,payload) values($1,$2,$3,'deliver_input','held','{}')")
        .bind(command)
        .bind(f.machine)
        .bind(side_id)
        .execute(&mut *conn)
        .await
        .unwrap();
    let input = Uuid::now_v7();
    sqlx::query("insert into inputs(id,conversation_id,owner_id,input_seq,idempotency_key,content,state,command_id,received_at,created_seq,feed_seq) values($1,$2,$3,1,'k1','[{\"type\":\"text\",\"text\":\"боковойуникальный\"}]','accepted',$4,now(),1,1)")
        .bind(input)
        .bind(side_id)
        .bind(f.owner)
        .bind(command)
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    let page = search::search(
        &f.pool,
        &SearchQuery {
            owner_id: f.owner,
            text: "боковойуникальный".into(),
            family: None,
            scope: None,
            conversation_id: None,
            since: None,
            until: None,
            harness: None,
            kind: None,
            cursor: None,
            limit: 20,
        },
    )
    .await
    .unwrap();
    assert!(page.hits.iter().all(|hit| hit.conversation_id != side_id));

    sqlx::query("insert into bookmarks(id,owner_id,conversation_id,target_kind,target_id) values($1,$2,$3,'input',$4)")
        .bind(Uuid::now_v7())
        .bind(f.owner)
        .bind(side_id)
        .bind(input)
        .execute(&f.pool)
        .await
        .unwrap();
    let bookmarks = canterlot_hub::bookmarks::list_owner(&f.pool, f.owner, None, None)
        .await
        .unwrap();
    assert!(
        bookmarks
            .bookmarks
            .iter()
            .all(|row| row.conversation_id != side_id)
    );

    let preset = Uuid::now_v7();
    sqlx::query("insert into instruction_presets(id,owner_id,purpose,title,revision,origin) values($1,$2,'instructions','Свой',1,'owner')")
        .bind(preset)
        .bind(f.owner)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query(
        "update conversations set instruction_profile=$2,instruction_preset_revision=1 where id=$1",
    )
    .bind(side_id)
    .bind(preset.to_string())
    .execute(&f.pool)
    .await
    .unwrap();
    let mut conn = f.pool.acquire().await.unwrap();
    let used = canterlot_hub::presets::used_by(&mut conn, f.owner, &preset.to_string())
        .await
        .unwrap();
    assert_eq!(used, 0);

    sqlx::query("insert into plan_tasks(id,owner_id,orchestrator_conversation_id,number,title,state,updated_at) values($1,$2,$3,1,'задача','waiting',now())")
        .bind(Uuid::now_v7())
        .bind(f.owner)
        .bind(side_id)
        .execute(&mut *conn)
        .await
        .ok();
    let tasks = canterlot_hub::session_tools::plan::snapshot_views(&mut conn, f.owner)
        .await
        .unwrap();
    assert!(tasks.iter().all(|task| task.orchestrator_id != side_id));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_shows_as_parent_in_leases_disk_and_routing(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    let execution = Uuid::now_v7();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'running')",
    )
    .bind(execution)
    .bind(side_id)
    .execute(&f.pool)
    .await
    .unwrap();
    let state = canterlot_proto::worker::v1::LeaseState {
        slots: 1,
        holders: vec![canterlot_proto::worker::v1::LeaseEntry {
            lease_id: "l1".into(),
            kind: "gate".into(),
            weight: 1,
            label: "gate".into(),
            execution_id: execution.to_string(),
            holder: "execution".into(),
            since_ms: 1,
            requested_at_ms: 0,
            position: 0,
            dir: "/tmp".into(),
        }],
        waiters: vec![],
        observed_at_ms: 1,
    };
    canterlot_hub::leases::apply(&f.pool, f.owner, f.machine, &state, None, Uuid::now_v7())
        .await
        .unwrap();
    let lease: Value = sqlx::query_scalar("select lease_state from machines where id=$1")
        .bind(f.machine)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        lease["holders"][0]["conversation_id"].as_str().unwrap(),
        parent.to_string()
    );

    let mut conn = f.pool.acquire().await.unwrap();
    let heavy = canterlot_hub::session_tools::disk::heaviest(
        &mut conn,
        f.owner,
        &json!([{"execution_id": execution, "rss_bytes": 1024}]),
    )
    .await
    .unwrap();
    assert_eq!(heavy[0]["title"], "Родитель");

    // Машина по умолчанию: боковой чат свежее родителя, но выбирает не её.
    let other_machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'Другая')")
        .bind(other_machine)
        .bind(f.owner)
        .execute(&mut *conn)
        .await
        .unwrap();
    let other = Uuid::now_v7();
    sqlx::query("insert into conversations(id,owner_id,machine_id,harness,workdir,title) values($1,$2,$3,'synthetic','/tmp/w','Другая')")
        .bind(other)
        .bind(f.owner)
        .bind(other_machine)
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("update conversations set last_activity_at=now()-interval '2 hours' where id=$1")
        .bind(parent)
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "update conversations set last_activity_at=now()-interval '30 minutes' where id=$1",
    )
    .bind(other)
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("update conversations set last_activity_at=now() where id=$1")
        .bind(side_id)
        .execute(&mut *conn)
        .await
        .unwrap();
    let picked = canterlot_hub::http::routing::default_machine(&mut conn, f.owner)
        .await
        .unwrap();
    assert_eq!(picked, Some(other_machine));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_events_stay_out_of_the_owner_scope(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    let correlation = Uuid::now_v7();
    let mut conn = f.pool.acquire().await.unwrap();
    conversations::emit_attributes(&mut conn, f.owner, side_id, correlation)
        .await
        .unwrap();
    activity::refresh(
        &f.pool,
        side_id,
        Utc::now(),
        &activity::Thresholds::from(&canterlot_hub::config::ActivityConfig::default()),
        correlation,
    )
    .await
    .unwrap();
    let owner_scope = format!("conversations:{}", f.owner);
    let side_scope = format!("conversation:{side_id}");
    let owner_events: i64 =
        sqlx::query_scalar("select count(*) from events where scope=$1 and conversation_id=$2")
            .bind(&owner_scope)
            .bind(side_id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(owner_events, 0);
    let side_events: i64 = sqlx::query_scalar("select count(*) from events where scope=$1")
        .bind(&side_scope)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert!(side_events >= 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn side_usage_rolls_into_parent_and_no_pushes(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    let execution = Uuid::now_v7();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'stopped')",
    )
    .bind(execution)
    .bind(side_id)
    .execute(&f.pool)
    .await
    .unwrap();
    let command = Uuid::now_v7();
    sqlx::query("insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,delivery_state,payload) values($1,$2,$3,$4,1,'deliver_input','completed','{}')")
        .bind(command)
        .bind(f.machine)
        .bind(side_id)
        .bind(execution)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("insert into runs(input_command_id,execution_id,conversation_id,provider) values($1,$2,$3,'claude')")
        .bind(command)
        .bind(execution)
        .bind(side_id)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("insert into run_model_usage(input_command_id,model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens) values($1,'synthetic-small','main',10,20,3,4)")
        .bind(command)
        .execute(&f.pool)
        .await
        .unwrap();
    let usage = canterlot_hub::session_usage::for_conversation(&f.pool, f.owner, parent)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(usage.delegates.turns, 1);
    assert_eq!(usage.delegates.input_tokens, 10);

    // Расход по дням приписан родителю, а свёртка удаления кладёт его туда же.
    let days = canterlot_hub::limits::usage_by_day(
        &f.pool,
        f.owner,
        None,
        Utc::now() - TimeDelta::days(2),
        Utc::now() + TimeDelta::days(1),
        "UTC",
    )
    .await
    .unwrap();
    assert!(days.iter().all(|day| day.conversation_id == parent));
    let mut tx = canterlot_hub::db::begin(&f.pool).await.unwrap();
    canterlot_hub::session_usage::fold_in_tx(&mut tx, side_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let folded: i64 = sqlx::query_scalar(
        "select turns from conversation_usage_rollup where parent_id=$1 and source_id=$2",
    )
    .bind(parent)
    .bind(side_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(folded, 1);

    sqlx::query("insert into push_settings(singleton,waiting,turn_finished,failures) values(true,true,true,true)")
        .execute(&f.pool)
        .await
        .unwrap();
    let mut conn = f.pool.acquire().await.unwrap();
    canterlot_hub::notify::on_request_opened_in_tx(&mut conn, side_id, Uuid::now_v7())
        .await
        .unwrap();
    let pushes: i64 =
        sqlx::query_scalar("select count(*) from owner_pushes where conversation_id=$1")
            .bind(side_id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(pushes, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn trashing_parent_removes_the_side(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    let response = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{parent}"),
            Some("trash-side"),
            None,
        )
        .await;
    assert_eq!(response.status.as_u16(), 204);
    let exists: bool = sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
        .bind(side_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert!(!exists);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn purging_parent_removes_the_side(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    let response = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{parent}"),
            Some("trash-1"),
            None,
        )
        .await;
    assert_eq!(response.status.as_u16(), 204);
    let response = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{parent}/trash"),
            Some("purge-1"),
            None,
        )
        .await;
    assert_eq!(response.status.as_u16(), 204);
    let exists: bool = sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
        .bind(side_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert!(!exists);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn accepting_parent_removes_the_side(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    let grand = f.parent("claude_code").await;
    sqlx::query("update conversations set parent_conversation_id=$2, spawn_depth=1 where id=$1")
        .bind(parent)
        .bind(grand)
        .execute(&f.pool)
        .await
        .unwrap();
    let mut tx = canterlot_hub::db::begin(&f.pool).await.unwrap();
    canterlot_hub::session_tools::accept::begin_in_tx(
        &mut tx,
        f.owner,
        parent,
        false,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let exists: bool = sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
        .bind(side_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert!(!exists);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn idle_side_stops_after_thirty_minutes(pool: PgPool) {
    let f = fixture(pool).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    let execution = Uuid::now_v7();
    sqlx::query("insert into executions(id,conversation_id,generation,state,created_at) values($1,$2,1,'running',now()-interval '2 hours')")
        .bind(execution)
        .bind(side_id)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("update conversation_derived set activity_state='idle',last_activity_at=now()-interval '2 hours' where conversation_id=$1")
        .bind(side_id)
        .execute(&f.pool)
        .await
        .unwrap();
    side::sweep(&f.pool, Utc::now()).await.unwrap();
    let stops: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id=$1 and kind='stop_execution'",
    )
    .bind(side_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(stops, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn fresh_fork_on_later_start_marks_the_side(pool: PgPool) {
    use canterlot_hub::ingest::{Ingest, JournalKind};
    use canterlot_proto::worker::v1::{JournalRecord, JournalRecords, RecordBody, record_body};
    use sha2::Digest;

    let f = fixture(pool.clone()).await;
    let parent = f.parent("claude_code").await;
    let side_id = f.side(parent).await;
    let execution = Uuid::now_v7();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,2,'starting')",
    )
    .bind(execution)
    .bind(side_id)
    .execute(&f.pool)
    .await
    .unwrap();
    let batch = JournalRecords {
        journal_id: Uuid::now_v7().to_string(),
        records: vec![JournalRecord {
            seq: 1,
            body: Some(RecordBody {
                body: Some(record_body::Body::ExecutionState(
                    canterlot_proto::worker::v1::ExecutionState {
                        native_conversation_id: "side-native".into(),
                        execution_id: execution.to_string(),
                        generation: 2,
                        state: "running".into(),
                        outcome: String::new(),
                        fresh_fork: true,
                        canonical_workdir: String::new(),
                    },
                )),
            }),
        }],
    };
    let digest = sha2::Sha256::digest(f.machine.as_bytes()).to_vec();
    Ingest::new(pool)
        .apply_batch(f.machine, &digest, JournalKind::Sj, &batch)
        .await
        .unwrap();
    let (fresh, marked): (bool, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "select e.fresh_fork,c.side_fresh_at from executions e
           join conversations c on c.id=e.conversation_id where e.id=$1",
    )
    .bind(execution)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(fresh);
    assert!(marked.is_some(), "тихая строка о свежем форке");
}
