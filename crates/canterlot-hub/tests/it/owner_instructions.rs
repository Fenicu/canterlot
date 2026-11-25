use canterlot_hub::{
    session::CAPABILITY_OWNER_INSTRUCTIONS_V1,
    testkit::{self},
};
use canterlot_proto::worker::v1::{
    OwnerFileMeta, OwnerInstructionsOverview, OwnerMemoryProject, control_down,
};
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
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
        Self {
            machine,
            registry,
            _leases: Vec::new(),
            _dir: dir,
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
            ..Default::default()
        }],
        projects: vec![OwnerMemoryProject {
            id: "proj1".into(),
            files: 2,
            has_index: true,
        }],
    }
}

#[tokio::test]
async fn owner_overview_proxies_a_fake_worker() {
    let pool = canterlot_hub::testkit::db().await;
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
    f.registry
        .finish_owner_overview(f.machine, overview_reply(&request.request_id));
    let reply = task.await.unwrap();
    assert_eq!(reply.files.len(), 1);
    assert_eq!(reply.projects.len(), 1);
    // Чужой ответ игнорируется.
    f.registry
        .finish_owner_overview(Uuid::now_v7(), overview_reply(&request.request_id));
    let _ = session.commands.try_recv();
}
