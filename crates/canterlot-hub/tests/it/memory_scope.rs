use canterlot_hub::{
    config::LimitsConfig,
    conversations::{self, CreateError, CrunchSetting, NewConversation, ParentLink},
    executions,
    ingest::{Ingest, JournalKind},
    memory::{Classification, classify},
    session::{CAPABILITY_SIDE_CHAT_V1, Registry},
    side,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{
    ExecutionState, JournalRecord, JournalRecords, RecordBody, record_body,
};
use sqlx::PgPool;
use uuid::Uuid;

const WORK: &str = "/h/work";
const BASE: &str = "/h/data/worktrees";

async fn fixture(pool: &PgPool, capabilities: &[&str]) -> (Seed, Registry) {
    let seed = testkit::seed(pool).await;
    testkit::seed_catalog(pool, seed.machine_id).await;
    let capabilities: Vec<String> = capabilities.iter().map(|c| (*c).to_owned()).collect();
    sqlx::query(
        "update machines set availability='available',capabilities=$2,allowed_roots='[\"/h\"]',
           memory_work_roots='[\"/h/work\"]',worktree_root=$3 where id=$1",
    )
    .bind(seed.machine_id)
    .bind(&capabilities)
    .bind(BASE)
    .execute(pool)
    .await
    .unwrap();
    (seed, Registry::new(Default::default()))
}

fn request(seed: &Seed, workdir: &str, parent: Option<Uuid>) -> NewConversation {
    NewConversation {
        owner_id: seed.owner_id,
        machine_id: seed.machine_id,
        harness: "synthetic".into(),
        workdir: workdir.into(),
        model: "synthetic-small".into(),
        effort: "low".into(),
        permission_mode: Some("bypass".into()),
        instruction_profile: None,
        title: None,
        color: None,
        group_ids: Vec::new(),
        crunch: CrunchSetting::default(),
        start: false,
        resume_native_id: None,
        parent: parent.map(|id| ParentLink { id, spawn_depth: 1 }),
        worktree_source: None,
    }
}

async fn create(
    pool: &PgPool,
    registry: &Registry,
    request: NewConversation,
) -> Result<Uuid, CreateError> {
    conversations::create(
        pool,
        &Uuid::now_v7().to_string(),
        request,
        &LimitsConfig::default(),
        Uuid::now_v7(),
        registry,
    )
    .await
    .map(|created| created.conversation_id)
}

async fn scope(pool: &PgPool, id: Uuid) -> (Option<String>, Option<String>) {
    sqlx::query_as("select memory_domain, memory_project from conversations where id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn some(domain: &str, project: &str) -> (Option<String>, Option<String>) {
    (Some(domain.into()), Some(project.into()))
}

#[test]
fn classify_work_personal_neutral() {
    let roots = vec![WORK.to_string()];
    let check = |path: &str| classify(path, &roots, Some(BASE));
    assert_eq!(check("/h/work"), Classification::Work);
    assert_eq!(check("/h/work/x"), Classification::Work);
    assert_eq!(check("/h/workshop"), Classification::Personal);
    assert_eq!(check("/h/work/../play"), Classification::Personal);
    assert_eq!(check("/h/play"), Classification::Personal);
    assert_eq!(check(BASE), Classification::Neutral);
    assert_eq!(check("/h/data/worktrees/0190"), Classification::Neutral);
    assert_eq!(classify("/h/work/x", &roots, None), Classification::Work);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn new_root_under_work_gets_work_and_its_project(pool: PgPool) {
    let (seed, registry) = fixture(&pool, &["pilot_v1", "memory_scope_v1"]).await;
    let work = create(&pool, &registry, request(&seed, "/h/work/p", None))
        .await
        .unwrap();
    assert_eq!(scope(&pool, work).await, some("work", "/h/work/p"));
    let personal = create(&pool, &registry, request(&seed, "/h/play/", None))
        .await
        .unwrap();
    assert_eq!(scope(&pool, personal).await, some("personal", "/h/play"));
    let lookalike = create(&pool, &registry, request(&seed, "/h/workshop", None))
        .await
        .unwrap();
    assert_eq!(
        scope(&pool, lookalike).await,
        some("personal", "/h/workshop")
    );
    let neutral = create(
        &pool,
        &registry,
        request(&seed, "/h/data/worktrees/x", None),
    )
    .await
    .unwrap();
    assert_eq!(scope(&pool, neutral).await, (None, None));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn new_root_without_capability_gets_null(pool: PgPool) {
    let (seed, registry) = fixture(&pool, &["pilot_v1"]).await;
    let id = create(&pool, &registry, request(&seed, "/h/work/p", None))
        .await
        .unwrap();
    assert_eq!(scope(&pool, id).await, (None, None));
    // Возможность есть, но корни ещё не приходили: классифицировать нечем.
    sqlx::query("update machines set capabilities=array['pilot_v1','memory_scope_v1'],memory_work_roots=null where id=$1")
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    let id = create(&pool, &registry, request(&seed, "/h/work/p", None))
        .await
        .unwrap();
    assert_eq!(scope(&pool, id).await, (None, None));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn spawn_side_fork_routine_copy_inherit_parent_scope(pool: PgPool) {
    let (seed, registry) = fixture(&pool, &["pilot_v1", "memory_scope_v1"]).await;
    let _lease = registry.connect_for_tests(
        seed.machine_id,
        &[
            "pilot_v1".to_owned(),
            "memory_scope_v1".to_owned(),
            CAPABILITY_SIDE_CHAT_V1.to_owned(),
        ],
    );
    let parent = create(&pool, &registry, request(&seed, "/h/work/p", None))
        .await
        .unwrap();
    // Делегат без своего каталога, с рабочим каталогом того же домена и в базе копий.
    for workdir in ["/h/work/p", "/h/work/q", "/h/data/worktrees/x"] {
        let child = create(&pool, &registry, request(&seed, workdir, Some(parent)))
            .await
            .unwrap();
        assert_eq!(
            scope(&pool, child).await,
            some("work", "/h/work/p"),
            "{workdir}"
        );
    }
    // Родитель без домена даёт потомка без домена.
    let bare = create(
        &pool,
        &registry,
        request(&seed, "/h/data/worktrees/y", None),
    )
    .await
    .unwrap();
    let child = create(&pool, &registry, request(&seed, "/h/work/q", Some(bare)))
        .await
        .unwrap();
    assert_eq!(scope(&pool, child).await, (None, None));
    // Боковая копия и копия отчёта идут через одну вставку.
    sqlx::query("insert into executions(id,conversation_id,generation,state,native_conversation_id) values($1,$2,1,'stopped','parent-native')")
        .bind(Uuid::now_v7())
        .bind(parent)
        .execute(&pool)
        .await
        .unwrap();
    let side = side::create(&pool, seed.owner_id, parent, Uuid::now_v7(), &registry)
        .await
        .unwrap();
    assert_eq!(
        scope(&pool, side.conversation_id).await,
        some("work", "/h/work/p")
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn child_with_other_domain_workdir_is_domain_mismatch(pool: PgPool) {
    let (seed, registry) = fixture(&pool, &["pilot_v1", "memory_scope_v1"]).await;
    let work = create(&pool, &registry, request(&seed, "/h/work/p", None))
        .await
        .unwrap();
    let personal = create(&pool, &registry, request(&seed, "/h/play", None))
        .await
        .unwrap();
    let before: i64 = sqlx::query_scalar("select count(*) from conversations")
        .fetch_one(&pool)
        .await
        .unwrap();
    let refused = create(&pool, &registry, request(&seed, "/h/play/x", Some(work))).await;
    assert!(matches!(refused, Err(CreateError::DomainMismatch)));
    let refused = create(
        &pool,
        &registry,
        request(&seed, "/h/work/x", Some(personal)),
    )
    .await;
    assert!(matches!(refused, Err(CreateError::DomainMismatch)));
    let after: i64 = sqlx::query_scalar("select count(*) from conversations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after, "отказ не оставляет строк");
}

fn running(
    execution: &executions::StartedExecution,
    state: &str,
    canonical: &str,
    source: &str,
) -> JournalRecord {
    JournalRecord {
        seq: 0,
        body: Some(RecordBody {
            body: Some(record_body::Body::ExecutionState(ExecutionState {
                execution_id: execution.execution_id.to_string(),
                generation: execution.generation as u64,
                state: state.into(),
                outcome: String::new(),
                native_conversation_id: String::new(),
                fresh_fork: false,
                canonical_workdir: canonical.into(),
                canonical_source: source.into(),
            })),
        }),
    }
}

async fn launch(pool: &PgPool, seed: &Seed, conversation: Uuid, canonical: &str) {
    launch_from(pool, seed, conversation, canonical, "").await;
}

/// Запуск изолированной копии: `canonical` — нейтральная копия, `source` — репозиторий.
async fn launch_from(
    pool: &PgPool,
    seed: &Seed,
    conversation: Uuid,
    canonical: &str,
    source: &str,
) {
    let execution = executions::start(pool, seed.owner_id, conversation, Uuid::now_v7())
        .await
        .unwrap();
    let mut records = vec![
        running(&execution, "starting", "", ""),
        running(&execution, "running", canonical, source),
    ];
    for (i, record) in records.iter_mut().enumerate() {
        record.seq = i as u64 + 1;
    }
    Ingest::new(pool.clone())
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &JournalRecords {
                journal_id: Uuid::now_v7().to_string(),
                records,
            },
        )
        .await
        .unwrap();
}

async fn conflicts(pool: &PgPool, conversation: Uuid) -> i64 {
    sqlx::query_scalar(
        "select count(*) from events where kind='memory_domain_conflict' and conversation_id=$1",
    )
    .bind(conversation)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn isolated_launch_is_classified_by_its_source_repo(pool: PgPool) {
    let (seed, registry) = fixture(&pool, &["pilot_v1", "memory_scope_v1"]).await;
    // Личный на вид путь, за которым рабочий репозиторий; копия лежит в нейтральной базе.
    let linked = create(&pool, &registry, request(&seed, "/h/play/link", None))
        .await
        .unwrap();
    launch_from(
        &pool,
        &seed,
        linked,
        "/h/data/worktrees/c1",
        "/h/work/secret",
    )
    .await;
    assert_eq!(scope(&pool, linked).await.0, None);
    assert_eq!(conflicts(&pool, linked).await, 1);

    // Контроль: личный источник оставляет личный домен.
    let honest = create(&pool, &registry, request(&seed, "/h/play/real", None))
        .await
        .unwrap();
    launch_from(&pool, &seed, honest, "/h/data/worktrees/c2", "/h/play/real").await;
    assert_eq!(scope(&pool, honest).await, some("personal", "/h/play/real"));
    assert_eq!(conflicts(&pool, honest).await, 0);

    // Старый воркер источник не шлёт: нейтральная копия по-прежнему ничему не противоречит.
    let old = create(&pool, &registry, request(&seed, "/h/play/old", None))
        .await
        .unwrap();
    launch_from(&pool, &seed, old, "/h/data/worktrees/c3", "").await;
    assert_eq!(scope(&pool, old).await, some("personal", "/h/play/old"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn launch_canonical_path_contradicting_domain_sets_null_and_records_conflict(pool: PgPool) {
    let (seed, registry) = fixture(&pool, &["pilot_v1", "memory_scope_v1"]).await;
    // Личный путь, который симлинк ведёт в рабочий корень.
    let linked = create(&pool, &registry, request(&seed, "/h/play/link", None))
        .await
        .unwrap();
    assert_eq!(scope(&pool, linked).await, some("personal", "/h/play/link"));
    launch(&pool, &seed, linked, "/h/work/secret").await;
    assert_eq!(scope(&pool, linked).await.0, None);
    assert_eq!(conflicts(&pool, linked).await, 1);
    let flagged: bool = sqlx::query_scalar(
        "select memory_domain_conflict_at is not null from conversations where id=$1",
    )
    .bind(linked)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(flagged);
    // Запуск идёт дальше.
    let state: String = sqlx::query_scalar(
        "select state from executions where conversation_id=$1 order by generation desc limit 1",
    )
    .bind(linked)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "running");

    // Рабочий разговор, чей путь ушёл в личный каталог, тоже выключается.
    let work = create(&pool, &registry, request(&seed, "/h/work/p", None))
        .await
        .unwrap();
    launch(&pool, &seed, work, "/h/play").await;
    assert_eq!(scope(&pool, work).await.0, None);
    assert_eq!(conflicts(&pool, work).await, 1);

    // Совпадающий и нейтральный настоящий путь, как и пустой от старого воркера, не мешают.
    for canonical in ["/h/work/p", "/h/data/worktrees/z", ""] {
        let fine = create(&pool, &registry, request(&seed, "/h/work/p", None))
            .await
            .unwrap();
        launch(&pool, &seed, fine, canonical).await;
        assert_eq!(
            scope(&pool, fine).await,
            some("work", "/h/work/p"),
            "{canonical}"
        );
        assert_eq!(conflicts(&pool, fine).await, 0);
    }
}

#[sqlx::test(migrations = false)]
async fn backfill_roots_by_workdir_children_by_parent_broken_chain_null(pool: PgPool) {
    let old = sqlx::migrate::Migrator {
        migrations: canterlot_hub::db::MIGRATOR
            .iter()
            .filter(|migration| migration.version < 119)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(&pool).await.unwrap();
    let seed = testkit::seed(&pool).await;
    sqlx::query("update machines set worktree_root=$2 where id=$1")
        .bind(seed.machine_id)
        .bind(BASE)
        .execute(&pool)
        .await
        .unwrap();
    // Строки в виде схемы 1.42: без полей памяти.
    let add = |workdir: &'static str, parent: Option<Uuid>, side_of: Option<Uuid>, depth: i32| {
        let pool = pool.clone();
        let (owner, machine) = (seed.owner_id, seed.machine_id);
        async move {
            let id = Uuid::now_v7();
            sqlx::query("insert into conversations(id,owner_id,machine_id,harness,workdir,parent_conversation_id,side_of,spawn_depth) values($1,$2,$3,'synthetic',$4,$5,$6,$7)")
                .bind(id).bind(owner).bind(machine).bind(workdir).bind(parent).bind(side_of).bind(depth)
                .execute(&pool).await.unwrap();
            id
        }
    };
    let work = add("/home/u/work/app", None, None, 0).await;
    let work_child = add("/home/u/work/app", Some(work), None, 1).await;
    let work_grandchild = add("/home/u/work/app", Some(work_child), None, 2).await;
    let work_side = add("/home/u/work/app", None, Some(work_child), 0).await;
    let personal = add("/home/u/play", None, None, 0).await;
    let personal_child = add("/home/u/play", Some(personal), None, 1).await;
    let lookalike = add("/home/u/workshop", None, None, 0).await;
    let neutral = add("/h/data/worktrees/x", None, None, 0).await;
    let neutral_child = add("/h/data/worktrees/x", Some(neutral), None, 1).await;
    // Делегат, у которого родитель потерян: цепочка оборвана.
    let broken = add("/home/u/work/app", None, None, 1).await;

    canterlot_hub::db::MIGRATOR.run(&pool).await.unwrap();

    let expected = [
        (work, some("work", "/home/u/work/app")),
        (work_child, some("work", "/home/u/work/app")),
        (work_grandchild, some("work", "/home/u/work/app")),
        (work_side, some("work", "/home/u/work/app")),
        (personal, some("personal", "/home/u/play")),
        (personal_child, some("personal", "/home/u/play")),
        (lookalike, some("personal", "/home/u/workshop")),
        (neutral, (None, None)),
        (neutral_child, (None, None)),
        (broken, (None, None)),
        (seed.conversation_id, some("personal", "/tmp/w")),
    ];
    for (id, want) in expected {
        assert_eq!(scope(&pool, id).await, want, "{id}");
    }
}
