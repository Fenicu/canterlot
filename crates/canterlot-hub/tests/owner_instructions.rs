use axum::http::Method;
use canterlot_hub::{
    http::{self, AppState},
    session::CAPABILITY_OWNER_INSTRUCTIONS_V1,
    testkit::{self, TestClient},
};
use canterlot_proto::worker::v1::{
    OwnerFileMeta, OwnerInstructionsFile, OwnerInstructionsOverview, OwnerMemoryProject,
    control_down,
};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    pool: PgPool,
    client: TestClient,
    machine: Uuid,
    registry: canterlot_hub::session::Registry,
    _leases: Vec<canterlot_hub::session::TestLease>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let registry = deps.registry.clone();
        let owner = deps.installation.owner_id;
        let machine = Uuid::now_v7();
        sqlx::query("insert into machines(id,owner_id,name,availability) values($1,$2,'Машина','available')").bind(machine).bind(owner).execute(&pool).await.unwrap();
        testkit::seed_catalog(&pool, machine).await;
        let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
        Self {
            pool,
            client,
            machine,
            registry,
            _leases: Vec::new(),
            _dir: dir,
        }
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        status: u16,
    ) -> Value {
        let r = self
            .client
            .send(method, path, Some(&Uuid::now_v7().to_string()), body)
            .await;
        assert_eq!(
            r.status.as_u16(),
            status,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        if r.body.is_empty() {
            Value::Null
        } else {
            r.json()
        }
    }
}

fn overview_reply(request_id: &str) -> OwnerInstructionsOverview {
    OwnerInstructionsOverview {
        request_id: request_id.into(),
        files: vec![OwnerFileMeta {
            kind: "claude_user".into(),
            path: "/home/owner/.claude/CLAUDE.md".into(),
            present: true,
            size: 7,
            mtime_ms: 1_700_000_000_000,
        }],
        projects: vec![OwnerMemoryProject {
            id: "proj1".into(),
            files: 2,
            has_index: true,
        }],
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_overview_proxies_a_fake_worker(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let mut session = testkit::fake_session(
        &f.registry,
        f.machine,
        &[CAPABILITY_OWNER_INSTRUCTIONS_V1],
        true,
    );
    let worker = tokio::spawn(async move {
        let down = session.commands.recv().await.unwrap();
        let Some(control_down::Msg::ReadOwner(request)) = down.msg else {
            panic!("{down:?}");
        };
        assert_eq!(request.kind, "overview");
        assert!(request.project.is_empty() && request.name.is_empty());
        (session, request)
    });
    let task = tokio::spawn({
        let registry = f.registry.clone();
        let machine = f.machine;
        async move {
            registry
                .owner_overview(machine, std::time::Duration::from_secs(5))
                .await
                .unwrap()
        }
    });
    let (mut session, request) = worker.await.unwrap();
    f.registry.finish_owner_overview(f.machine, overview_reply(&request.request_id));
    let reply = task.await.unwrap();
    assert_eq!(reply.files.len(), 1);
    assert_eq!(reply.projects.len(), 1);
    // Чужой ответ игнорируется.
    f.registry.finish_owner_overview(
        Uuid::now_v7(),
        overview_reply(&request.request_id),
    );
    let _ = session.commands.try_recv();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_endpoints_need_a_capable_live_worker(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let overview = format!("/api/v1/machines/{}/owner-instructions", f.machine);
    let file = format!(
        "/api/v1/machines/{}/owner-instructions/file?kind=claude_user",
        f.machine
    );
    // Нет сеанса: машина недоступна.
    let r = f
        .request(Method::GET, &overview, None, 409)
        .await;
    assert_eq!(r["error"]["code"], "machine_unavailable");
    let r = f.request(Method::GET, &file, None, 409).await;
    assert_eq!(r["error"]["code"], "machine_unavailable");
    // Старый воркер без возможности: требуется обновление.
    let mut old = testkit::fake_session(&f.registry, f.machine, &["pilot_v1"], true);
    let r = f
        .request(Method::GET, &overview, None, 409)
        .await;
    assert_eq!(r["error"]["code"], "capability_missing");
    let r = f.request(Method::GET, &file, None, 409).await;
    assert_eq!(r["error"]["code"], "capability_missing");
    assert!(old.commands.try_recv().is_err(), "старому воркеру ничего не шлётся");
    drop(old);
    // Живой воркер с возможностью отвечает через управляющий канал.
    let mut session = testkit::fake_session(
        &f.registry,
        f.machine,
        &[CAPABILITY_OWNER_INSTRUCTIONS_V1],
        true,
    );
    let served = tokio::spawn({
        let machine = f.machine;
        let registry = f.registry.clone();
        async move {
            let down = session.commands.recv().await.unwrap();
            let Some(control_down::Msg::ReadOwner(request)) = down.msg else {
                panic!("{down:?}");
            };
            registry.finish_owner_overview(machine, overview_reply(&request.request_id));
        }
    });
    let body = f.request(Method::GET, &overview, None, 200).await;
    assert_eq!(body["files"][0]["kind"], "claude_user");
    assert_eq!(body["files"][0]["harness"], "claude");
    assert_eq!(body["memory_projects"][0]["id"], "proj1");
    served.await.unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_file_serves_content_and_validates_kinds(pool: PgPool) {
    let f = Fixture::new(pool).await;
    testkit::fake_session(
        &f.registry,
        f.machine,
        &[CAPABILITY_OWNER_INSTRUCTIONS_V1],
        true,
    );
    // Проверки запроса без воркера: вид обязан быть из списка.
    for path in [
        format!(
            "/api/v1/machines/{}/owner-instructions/file?kind=credentials",
            f.machine
        ),
        format!(
            "/api/v1/machines/{}/owner-instructions/file?kind=memory_file",
            f.machine
        ),
        format!(
            "/api/v1/machines/{}/owner-instructions/file?kind=memory_file&project=..%2F..&name=a.md",
            f.machine
        ),
        format!(
            "/api/v1/machines/{}/owner-instructions/file?kind=memory_file&project=proj1&name=..%2F.env",
            f.machine
        ),
        format!(
            "/api/v1/machines/{}/owner-instructions/file?kind=claude_user&project=proj1",
            f.machine
        ),
    ] {
        let r = f.request(Method::GET, &path, None, 400).await;
        assert_eq!(r["error"]["code"], "invalid_request", "{path}");
    }
    // Корректный запрос доходит до воркера и возвращает содержимое.
    let mut session = testkit::fake_session(
        &f.registry,
        f.machine,
        &[CAPABILITY_OWNER_INSTRUCTIONS_V1],
        true,
    );
    let machine = f.machine;
    let registry = f.registry.clone();
    let served = tokio::spawn(async move {
        let down = session.commands.recv().await.unwrap();
        let Some(control_down::Msg::ReadOwner(request)) = down.msg else {
            panic!("{down:?}");
        };
        assert_eq!(
            (request.kind.as_str(), request.project.as_str(), request.name.as_str()),
            ("memory_file", "proj1", "a.md")
        );
        registry.finish_owner_file(machine, OwnerInstructionsFile {
            request_id: request.request_id.clone(),
            kind: request.kind.clone(),
            path: "/home/owner/.claude/projects/proj1/memory/a.md".into(),
            present: true,
            size: 3,
            mtime_ms: 1_700_000_000_000,
            truncated: false,
            content: "# a".into(),
            ..Default::default()
        });
    });
    let body = f
        .request(
            Method::GET,
            &format!(
                "/api/v1/machines/{}/owner-instructions/file?kind=memory_file&project=proj1&name=a.md",
                f.machine
            ),
            None,
            200,
        )
        .await;
    assert_eq!(body["content"], "# a");
    assert_eq!(body["harness"], "claude");
    served.await.unwrap();
}
