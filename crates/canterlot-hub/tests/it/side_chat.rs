use axum::http::Method;
use canterlot_hub::{
    executions,
    http::{self, AppState},
    session::{CAPABILITY_SIDE_CHAT_V1, Registry, TestLease},
    side,
    testkit::{self, TestClient},
};
use serde_json::Value;
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

#[tokio::test]
async fn side_endpoint_creates_returns_existing_and_deletes() {
    let pool = canterlot_hub::testkit::db().await;
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

#[tokio::test]
async fn side_start_payload_forks_from_parent_and_repeats_instruction() {
    let pool = canterlot_hub::testkit::db().await;
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
