//! Общая память: инструменты `memory_*` и хранилище `crate::memory::store`.

use std::sync::Arc;

use canterlot_hub::{
    config::{MemoryConfig, SessionToolsConfig},
    db,
    memory::{
        MemoryDomain,
        store::{self, Actor, Kind, Level, MemoryError, MemoryScope, NewEntry, Patch},
    },
    session::Registry,
    session_tools::{
        self, Caller, ToolContext, ToolError, ToolErrorCode as Code, settle::SettleConfig,
    },
    testkit::{self, Seed},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const WORK_A: &str = "/h/work/alpha";
const WORK_B: &str = "/h/work/beta";
const HOME_A: &str = "/h/home/alpha";

struct Fx {
    pool: PgPool,
    seed: Seed,
    memory: MemoryConfig,
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        Self::with(pool, MemoryConfig::default()).await
    }

    async fn with(pool: PgPool, memory: MemoryConfig) -> Self {
        let seed = testkit::seed(&pool).await;
        Self { pool, seed, memory }
    }

    fn context(&self) -> ToolContext {
        ToolContext {
            pool: self.pool.clone(),
            config: Arc::new(SessionToolsConfig::default()),
            memory: Arc::new(self.memory.clone()),
            limits: Default::default(),
            settle: SettleConfig {
                idle_quiet: std::time::Duration::from_secs(5),
                idle_background_max: std::time::Duration::from_secs(15 * 60),
                nudge_enabled: true,
            },
            alarms_enabled: true,
            nudge_enabled: true,
            routing_enabled: true,
            routing_stale: std::time::Duration::from_secs(1800),
            routing_queue_horizon: std::time::Duration::from_secs(6 * 3600),
            spawn_queue: Default::default(),
            guards: Default::default(),
            dispatcher: None,
            cloud: None,
            registry: Registry::new(Default::default()),
            shutdown: tokio_util::sync::CancellationToken::new(),
        }
    }

    /// Разговор с заданным доменом и проектом и вызывающий от его имени.
    async fn session(&self, domain: Option<&str>, project: Option<&str>) -> Caller {
        let conversation_id =
            testkit::seed_conversation(&self.pool, self.seed.owner_id, self.seed.machine_id).await;
        sqlx::query("update conversations set memory_domain=$2, memory_project=$3 where id=$1")
            .bind(conversation_id)
            .bind(domain)
            .bind(project)
            .execute(&self.pool)
            .await
            .unwrap();
        Caller {
            owner_id: self.seed.owner_id,
            conversation_id,
            execution_id: Uuid::now_v7(),
            machine_id: self.seed.machine_id,
            call_id: Uuid::now_v7(),
        }
    }

    async fn call(
        &self,
        caller: &Caller,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, ToolError> {
        session_tools::call(&self.context(), caller, tool, arguments).await
    }

    async fn write(&self, caller: &Caller, subject: &str, body: &str, level: &str) -> Uuid {
        let written = self
            .call(
                caller,
                "memory.write",
                json!({"kind": "gotcha", "subject": subject, "body": body, "level": level}),
            )
            .await
            .unwrap();
        written["id"].as_str().unwrap().parse().unwrap()
    }

    async fn scope(&self, caller: &Caller) -> MemoryScope {
        let mut conn = self.pool.acquire().await.unwrap();
        MemoryScope::of_caller(&mut conn, caller.conversation_id)
            .await
            .unwrap()
            .unwrap()
    }
}

fn error(result: Result<Value, ToolError>) -> ToolError {
    result.expect_err("the call must be refused")
}

fn actor(caller: &Caller) -> Actor {
    Actor::Agent {
        conversation_id: caller.conversation_id,
        harness: "synthetic".into(),
        model: "synthetic-small".into(),
    }
}

fn new_entry(subject: &str) -> NewEntry {
    NewEntry {
        kind: Kind::Gotcha,
        subject: subject.into(),
        body: "body".into(),
        level: Level::Domain,
        valid_until: None,
    }
}

fn owner_scope() -> MemoryScope {
    MemoryScope::owner(MemoryDomain::Work, None)
}

#[test]
fn memory_tools_are_session_tools_with_documented_defaults() {
    for tool in [
        "memory.search",
        "memory.get",
        "memory.write",
        "memory.update",
        "memory.delete",
    ] {
        assert!(session_tools::is_tool(tool), "{tool}");
    }
    let config = MemoryConfig::default();
    assert_eq!(config.max_active_per_domain, 2000);
    assert_eq!(config.writes_per_hour, 60);
    assert_eq!(config.search_limit, 20);
}

#[sqlx::test(migrations = "./migrations")]
async fn search_get_update_delete_never_cross_domain_guessed_id_is_not_found(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let work = fx.session(Some("work"), Some(WORK_A)).await;
    let personal = fx.session(Some("personal"), Some(HOME_A)).await;
    let work_id = fx
        .write(&work, "Work secret plan", "nda body", "domain")
        .await;
    let personal_id = fx
        .write(&personal, "Home plan", "home body", "domain")
        .await;

    let missing = Uuid::now_v7();
    for (caller, foreign) in [(&personal, work_id), (&work, personal_id)] {
        let absent = error(fx.call(caller, "memory.get", json!({"id": missing})).await);
        assert_eq!(absent.code, Code::NotFound);
        // Чужая запись неотличима от несуществующей: тот же код и тот же текст.
        let got = error(fx.call(caller, "memory.get", json!({"id": foreign})).await);
        assert_eq!(got, absent);
        let updated = error(
            fx.call(
                caller,
                "memory.update",
                json!({"id": foreign, "expected_version": 1, "body": "overwritten"}),
            )
            .await,
        );
        assert_eq!(updated.code, Code::NotFound);
        let deleted = error(
            fx.call(
                caller,
                "memory.delete",
                json!({"id": foreign, "expected_version": 1}),
            )
            .await,
        );
        assert_eq!(deleted.code, Code::NotFound);
        for widen in [false, true] {
            let found = fx
                .call(
                    caller,
                    "memory.search",
                    json!({"query": "", "include_other_projects": widen}),
                )
                .await
                .unwrap();
            let ids: Vec<&str> = found["results"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["id"].as_str().unwrap())
                .collect();
            assert!(!ids.contains(&foreign.to_string().as_str()), "{ids:?}");
            let by_text = fx
                .call(
                    caller,
                    "memory.search",
                    json!({"query": "plan", "include_other_projects": widen}),
                )
                .await
                .unwrap();
            assert_eq!(by_text["count"], json!(1), "{by_text}");
        }
    }
    // Запись цела: владелец своего домена видит исходный текст.
    let mut conn = fx.pool.acquire().await.unwrap();
    let kept = store::get(&mut conn, &owner_scope(), work_id, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.body, "nda body");
    assert_eq!(kept.version, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn include_other_projects_stays_in_domain_and_allows_get(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let alpha = fx.session(Some("work"), Some(WORK_A)).await;
    let beta = fx.session(Some("work"), Some(WORK_B)).await;
    let personal = fx.session(Some("personal"), Some(HOME_A)).await;
    let beta_id = fx
        .write(&beta, "Beta deploy rule", "beta only", "project")
        .await;
    fx.write(&personal, "Home deploy rule", "home only", "project")
        .await;

    let narrow = fx
        .call(&alpha, "memory.search", json!({"query": "deploy"}))
        .await
        .unwrap();
    assert_eq!(narrow["count"], json!(0), "{narrow}");
    let wide = fx
        .call(
            &alpha,
            "memory.search",
            json!({"query": "deploy", "include_other_projects": true}),
        )
        .await
        .unwrap();
    assert_eq!(wide["count"], json!(1), "{wide}");
    let row = &wide["results"][0];
    assert_eq!(row["id"], json!(beta_id));
    assert_eq!(row["level"], json!("project"));
    assert_eq!(row["project_label"], json!("beta"));
    assert!(row["excerpt"].as_str().unwrap().len() <= 200);
    let entry = fx
        .call(&alpha, "memory.get", json!({"id": beta_id}))
        .await
        .unwrap();
    assert_eq!(entry["body"], json!("beta only"));
    assert_eq!(entry["domain"], json!("work"));
    assert_eq!(entry["version"], json!(1));
    assert_eq!(entry["created_by"]["kind"], json!("agent"));
    assert_eq!(
        entry["created_by"]["conversation_id"],
        json!(beta.conversation_id)
    );
    // Читать чужой проект можно, менять и архивировать нельзя.
    for refused in [
        fx.call(
            &alpha,
            "memory.update",
            json!({"id": beta_id, "expected_version": 1, "body": "x"}),
        )
        .await,
        fx.call(
            &alpha,
            "memory.delete",
            json!({"id": beta_id, "expected_version": 1}),
        )
        .await,
    ] {
        assert_eq!(error(refused).code, Code::NotFound);
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn null_domain_caller_gets_memory_unavailable(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let none = fx.session(None, None).await;
    let id = Uuid::now_v7().to_string();
    for (tool, arguments) in [
        ("memory.search", json!({"query": "x"})),
        ("memory.get", json!({"id": id})),
        (
            "memory.write",
            json!({"kind": "user", "subject": "s", "body": "b"}),
        ),
        (
            "memory.update",
            json!({"id": id, "expected_version": 1, "body": "b"}),
        ),
        ("memory.delete", json!({"id": id, "expected_version": 1})),
    ] {
        let refused = error(fx.call(&none, tool, arguments).await);
        assert_eq!(refused.code, Code::MemoryUnavailable, "{tool}");
        assert_eq!(refused.code.as_str(), "memory_unavailable");
    }
    let mut conn = fx.pool.acquire().await.unwrap();
    assert!(
        MemoryScope::of_caller(&mut conn, none.conversation_id)
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn write_returns_id_version_level_domain(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let work = fx.session(Some("work"), Some(WORK_A)).await;
    let project = fx
        .call(
            &work,
            "memory.write",
            json!({"kind": "decision", "subject": "Use pnpm", "body": "pnpm everywhere"}),
        )
        .await
        .unwrap();
    assert!(project["id"].as_str().unwrap().parse::<Uuid>().is_ok());
    assert_eq!(project["version"], json!(1));
    assert_eq!(project["level"], json!("project"));
    assert_eq!(project["domain"], json!("work"));
    assert_eq!(project["project_label"], json!("alpha"));
    let domain = fx
        .call(
            &work,
            "memory.write",
            json!({"kind": "user", "subject": "Language", "body": "Russian", "level": "domain"}),
        )
        .await
        .unwrap();
    assert_eq!(domain["level"], json!("domain"));
    assert_eq!(domain["project_label"], Value::Null);
    let events: i64 = sqlx::query_scalar(
        "select count(*) from memory_events where conversation_id = $1 and action = 'create'",
    )
    .bind(work.conversation_id)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(events, 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn same_subject_active_is_subject_exists_with_id(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let work = fx.session(Some("work"), Some(WORK_A)).await;
    let id = fx.write(&work, "Release checklist", "one", "project").await;
    let refused = error(
        fx.call(
            &work,
            "memory.write",
            json!({"kind": "gotcha", "subject": "RELEASE CHECKLIST", "body": "two"}),
        )
        .await,
    );
    assert_eq!(refused.code, Code::SubjectExists);
    assert!(
        refused.message.contains(&id.to_string()),
        "{}",
        refused.message
    );
    assert!(
        refused.message.contains("\"version\":1"),
        "{}",
        refused.message
    );
    // Тот же текст в другом проекте и на уровне домена — другая запись.
    let beta = fx.session(Some("work"), Some(WORK_B)).await;
    fx.write(&beta, "Release checklist", "other project", "project")
        .await;
    fx.write(&beta, "Release checklist", "domain wide", "domain")
        .await;
}

#[sqlx::test(migrations = "./migrations")]
async fn update_wrong_version_is_version_conflict_with_current(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let work = fx.session(Some("work"), Some(WORK_A)).await;
    let id = fx.write(&work, "Counter", "v1", "project").await;
    let updated = fx
        .call(
            &work,
            "memory.update",
            json!({"id": id, "expected_version": 1, "body": "v2"}),
        )
        .await
        .unwrap();
    assert_eq!(updated["version"], json!(2));
    let stale = error(
        fx.call(
            &work,
            "memory.update",
            json!({"id": id, "expected_version": 1, "body": "v3"}),
        )
        .await,
    );
    assert_eq!(stale.code, Code::VersionConflict);
    assert!(
        stale.message.contains("\"current_version\":2"),
        "{}",
        stale.message
    );
    let stale_delete = error(
        fx.call(
            &work,
            "memory.delete",
            json!({"id": id, "expected_version": 1}),
        )
        .await,
    );
    assert_eq!(stale_delete.code, Code::VersionConflict);
    let kept = fx
        .call(&work, "memory.get", json!({"id": id}))
        .await
        .unwrap();
    assert_eq!(kept["body"], json!("v2"));
    // `valid_until: null` снимает срок, пропуск поля оставляет как было.
    fx.call(
        &work,
        "memory.update",
        json!({"id": id, "expected_version": 2, "valid_until": "2030-01-01T00:00:00Z"}),
    )
    .await
    .unwrap();
    let dated = fx
        .call(&work, "memory.get", json!({"id": id}))
        .await
        .unwrap();
    assert!(dated["valid_until"].is_string());
    fx.call(
        &work,
        "memory.update",
        json!({"id": id, "expected_version": 3, "body": "v4"}),
    )
    .await
    .unwrap();
    let kept = fx
        .call(&work, "memory.get", json!({"id": id}))
        .await
        .unwrap();
    assert!(kept["valid_until"].is_string());
    fx.call(
        &work,
        "memory.update",
        json!({"id": id, "expected_version": 4, "valid_until": null}),
    )
    .await
    .unwrap();
    let cleared = fx
        .call(&work, "memory.get", json!({"id": id}))
        .await
        .unwrap();
    assert!(cleared["valid_until"].is_null());
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_updates_one_wins(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let work = fx.session(Some("work"), Some(WORK_A)).await;
    let id = fx.write(&work, "Race", "v1", "project").await;
    let scope = fx.scope(&work).await;
    let patch = |body: &str| Patch {
        body: Some(body.into()),
        ..Default::default()
    };

    let mut first = db::begin(&fx.pool).await.unwrap();
    store::update(
        &mut first,
        &scope,
        actor(&work),
        id,
        1,
        patch("first"),
        &fx.memory,
    )
    .await
    .unwrap();

    let pool = fx.pool.clone();
    let memory = fx.memory.clone();
    let scope_b = scope.clone();
    let actor_b = actor(&work);
    let second = tokio::spawn(async move {
        let mut tx = db::begin(&pool).await.unwrap();
        let result = store::update(
            &mut tx,
            &scope_b,
            actor_b,
            id,
            1,
            patch_of("second"),
            &memory,
        )
        .await;
        tx.commit().await.unwrap();
        result
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "select exists(select 1 from pg_stat_activity
                  where datname = current_database() and wait_event_type = 'Lock'
                    and pid <> pg_backend_pid())",
            )
            .fetch_one(&fx.pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            // Запрос к базе сам занимает время; пауза без нужды не нужна.
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the second update waits for the first");
    first.commit().await.unwrap();

    let lost = second.await.unwrap();
    assert!(
        matches!(
            lost,
            Err(MemoryError::VersionConflict { current_version: 2 })
        ),
        "{lost:?}"
    );
    let kept = fx
        .call(&work, "memory.get", json!({"id": id}))
        .await
        .unwrap();
    assert_eq!(kept["body"], json!("first"));
    assert_eq!(kept["version"], json!(2));
}

fn patch_of(body: &str) -> Patch {
    Patch {
        body: Some(body.into()),
        ..Default::default()
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn archive_create_same_subject_then_restore_is_subject_exists(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let work = fx.session(Some("work"), Some(WORK_A)).await;
    let first = fx.write(&work, "Flaky test", "old", "project").await;
    let deleted = fx
        .call(
            &work,
            "memory.delete",
            json!({"id": first, "expected_version": 1}),
        )
        .await
        .unwrap();
    assert_eq!(deleted["status"], json!("archived"));
    assert_eq!(deleted["version"], json!(2));
    // Архивная запись агенту не видна и не читается.
    assert_eq!(
        error(fx.call(&work, "memory.get", json!({"id": first})).await).code,
        Code::NotFound
    );
    let listed = fx.call(&work, "memory.search", json!({})).await.unwrap();
    assert_eq!(listed["count"], json!(0));
    let second = fx.write(&work, "Flaky test", "new", "project").await;

    let owner = MemoryScope::owner(MemoryDomain::Work, Some(WORK_A.into()));
    let mut tx = db::begin(&fx.pool).await.unwrap();
    let refused = store::restore(&mut tx, &owner, Actor::Owner, first, &fx.memory).await;
    match refused {
        Err(MemoryError::SubjectExists { id, version }) => {
            assert_eq!(id, second);
            assert_eq!(version, 1);
        }
        other => panic!("{other:?}"),
    }
    drop(tx);
    // Архивирование второй записи освобождает тему, и тогда первая возвращается.
    fx.call(
        &work,
        "memory.delete",
        json!({"id": second, "expected_version": 1}),
    )
    .await
    .unwrap();
    let mut tx = db::begin(&fx.pool).await.unwrap();
    let restored = store::restore(&mut tx, &owner, Actor::Owner, first, &fx.memory)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(restored.version, 3);
    // Агент вернуть запись не может: у него нет ни такого вызова, ни такой области.
    let mut conn = fx.pool.acquire().await.unwrap();
    let agent = fx.scope(&work).await;
    assert!(matches!(
        store::restore(&mut conn, &agent, actor(&work), first, &fx.memory).await,
        Err(MemoryError::NotFound)
    ));
}

#[sqlx::test(migrations = "./migrations")]
async fn revisions_capped_at_20(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let work = fx.session(Some("work"), Some(WORK_A)).await;
    let scope = MemoryScope::owner(MemoryDomain::Work, Some(WORK_A.into()));
    let mut conn = fx.pool.acquire().await.unwrap();
    let written = store::write(
        &mut conn,
        &fx.scope(&work).await,
        Actor::Owner,
        NewEntry {
            level: Level::Project,
            ..new_entry("History")
        },
        &fx.memory,
    )
    .await
    .unwrap();
    let mut version = written.version;
    for round in 0..25 {
        let next = store::update(
            &mut conn,
            &scope,
            Actor::Owner,
            written.id,
            version,
            patch_of(&format!("body {round}")),
            &fx.memory,
        )
        .await
        .unwrap();
        version = next.version;
    }
    assert_eq!(version, 26);
    let kept = store::revisions(&mut conn, &scope, written.id)
        .await
        .unwrap();
    assert_eq!(kept.len(), 20);
    // Новые первыми; самые старые из 25 отброшены.
    assert_eq!(kept[0].version, 25);
    assert_eq!(kept[19].version, 6);
    assert!(kept.iter().all(|revision| revision.action == "update"));
}

#[sqlx::test(migrations = "./migrations")]
async fn hard_delete_keeps_contentless_events(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let work = fx.session(Some("work"), Some(WORK_A)).await;
    let id = fx
        .write(&work, "Doomed", "sensitive body text", "project")
        .await;
    fx.call(
        &work,
        "memory.update",
        json!({"id": id, "expected_version": 1, "body": "newer sensitive body"}),
    )
    .await
    .unwrap();
    let scope = MemoryScope::owner(MemoryDomain::Work, None);
    let mut tx = db::begin(&fx.pool).await.unwrap();
    store::hard_delete(&mut tx, &scope, Actor::Owner, id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let entries: i64 = sqlx::query_scalar("select count(*) from memory_entries where id = $1")
        .bind(id)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    let revisions: i64 =
        sqlx::query_scalar("select count(*) from memory_revisions where entry_id = $1")
            .bind(id)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!((entries, revisions), (0, 0));
    let events: Vec<(String,)> =
        sqlx::query_as("select action from memory_events where entry_id = $1 order by id")
            .bind(id)
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    let actions: Vec<&str> = events.iter().map(|(action,)| action.as_str()).collect();
    assert_eq!(actions, ["create", "update", "hard_delete"]);
    // Журнал без содержимого: ни в одном столбце нет текста записи.
    let dump: String =
        sqlx::query_scalar("select coalesce(string_agg(e::text, ' '), '') from memory_events e")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert!(!dump.contains("sensitive"), "{dump}");
    assert!(!dump.contains("Doomed"), "{dump}");
    // Агент жёстко удалить не может.
    let mut conn = fx.pool.acquire().await.unwrap();
    let other = fx.write(&work, "Kept", "b", "project").await;
    assert!(matches!(
        store::hard_delete(&mut conn, &fx.scope(&work).await, actor(&work), other).await,
        Err(MemoryError::NotFound)
    ));
}

#[sqlx::test(migrations = "./migrations")]
async fn rate_limit_61st_write_in_an_hour_is_rate_limited_under_parallel_writes(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let work = fx.session(Some("work"), Some(WORK_A)).await;
    let context = Arc::new(fx.context());
    let mut tasks = Vec::new();
    for number in 0..61 {
        let context = context.clone();
        tasks.push(tokio::spawn(async move {
            session_tools::call(
                &context,
                &work,
                "memory.write",
                json!({"kind": "gotcha", "subject": format!("Note {number}"), "body": "text"}),
            )
            .await
        }));
    }
    let mut written = 0;
    let mut limited = Vec::new();
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => written += 1,
            Err(refused) => limited.push(refused),
        }
    }
    assert_eq!(written, 60);
    assert_eq!(limited.len(), 1, "{limited:?}");
    assert_eq!(limited[0].code, Code::RateLimited);
    assert!(
        limited[0].message.contains("retry_after_s"),
        "{}",
        limited[0].message
    );
    let stored: i64 = sqlx::query_scalar("select count(*) from memory_entries")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(stored, 60);
    // Другой разговор лимитом первого не задет.
    let other = fx.session(Some("work"), Some(WORK_A)).await;
    fx.write(&other, "Fresh note", "text", "project").await;
}

#[sqlx::test(migrations = "./migrations")]
async fn domain_full_at_2000(pool: PgPool) {
    let fx = Fx::with(
        pool,
        MemoryConfig {
            max_active_per_domain: 3,
            ..MemoryConfig::default()
        },
    )
    .await;
    let work = fx.session(Some("work"), Some(WORK_A)).await;
    let personal = fx.session(Some("personal"), Some(HOME_A)).await;
    for number in 0..3 {
        fx.write(&work, &format!("Entry {number}"), "b", "domain")
            .await;
    }
    let full = error(
        fx.call(
            &work,
            "memory.write",
            json!({"kind": "user", "subject": "One more", "body": "b"}),
        )
        .await,
    );
    assert_eq!(full.code, Code::DomainFull);
    assert!(full.message.contains("\"limit\":3"), "{}", full.message);
    // Домены считаются раздельно.
    fx.write(&personal, "Entry 0", "b", "domain").await;
    // Архивирование освобождает место.
    let ids: Vec<(Uuid,)> =
        sqlx::query_as("select id from memory_entries where domain = 'work' limit 1")
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    fx.call(
        &work,
        "memory.delete",
        json!({"id": ids[0].0, "expected_version": 1}),
    )
    .await
    .unwrap();
    fx.write(&work, "One more", "b", "domain").await;
}

#[sqlx::test(migrations = "./migrations")]
async fn bad_level_for_project_level_without_project(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let bare = fx.session(Some("personal"), None).await;
    let refused = error(
        fx.call(
            &bare,
            "memory.write",
            json!({"kind": "user", "subject": "s", "body": "b", "level": "project"}),
        )
        .await,
    );
    assert_eq!(refused.code, Code::BadLevel);
    assert_eq!(refused.code.as_str(), "bad_level");
    let id = fx.write(&bare, "Domain wide", "b", "domain").await;
    let up = error(
        fx.call(
            &bare,
            "memory.update",
            json!({"id": id, "expected_version": 1, "level": "project"}),
        )
        .await,
    );
    assert_eq!(up.code, Code::BadLevel);
}

#[sqlx::test(migrations = "./migrations")]
async fn own_project_before_domain_before_other_projects_expired_last(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let alpha = fx.session(Some("work"), Some(WORK_A)).await;
    let beta = fx.session(Some("work"), Some(WORK_B)).await;
    // Порядок записи обратный ожидаемому: новизна ранг не определяет.
    fx.write(&beta, "deploy topic 4", "deploy checklist", "project")
        .await;
    fx.write(&alpha, "deploy topic 3", "deploy checklist", "domain")
        .await;
    fx.write(&alpha, "deploy topic 2", "deploy checklist", "project")
        .await;
    fx.call(
        &alpha,
        "memory.write",
        json!({"kind": "gotcha", "subject": "deploy topic 1", "body": "deploy checklist",
               "level": "project", "valid_until": "2020-01-01T00:00:00Z"}),
    )
    .await
    .unwrap();
    let found = fx
        .call(
            &alpha,
            "memory.search",
            json!({"query": "deploy", "include_other_projects": true}),
        )
        .await
        .unwrap();
    let rows: Vec<(String, bool)> = found["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["subject"].as_str().unwrap().to_owned(),
                row["expired"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("deploy topic 2".to_owned(), false),
            ("deploy topic 3".to_owned(), false),
            ("deploy topic 4".to_owned(), false),
            ("deploy topic 1".to_owned(), true),
        ]
    );
    // Пустой запрос даёт самые новые записи, истёкшие в конце.
    let newest = fx
        .call(
            &alpha,
            "memory.search",
            json!({"include_other_projects": true}),
        )
        .await
        .unwrap();
    assert_eq!(newest["results"][0]["subject"], json!("deploy topic 2"));
    assert_eq!(newest["results"][3]["subject"], json!("deploy topic 1"));
    let limited = fx
        .call(
            &alpha,
            "memory.search",
            json!({"limit": 2, "include_other_projects": true}),
        )
        .await
        .unwrap();
    assert_eq!(limited["count"], json!(2));
}

#[sqlx::test(migrations = "./migrations")]
async fn exact_code_token_matches_via_simple_config(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let work = fx.session(Some("work"), Some(WORK_A)).await;
    let id = fx
        .write(
            &work,
            "Chrome reload loop",
            "Chrome showed ERR_NETWORK_CHANGED while the VPN reconnected",
            "project",
        )
        .await;
    fx.write(&work, "Unrelated", "nothing about it", "project")
        .await;
    for query in [
        "ERR_NETWORK_CHANGED",
        "err_network_changed",
        "\"VPN reconnected\"",
    ] {
        let found = fx
            .call(&work, "memory.search", json!({"query": query}))
            .await
            .unwrap();
        assert_eq!(found["count"], json!(1), "{query}: {found}");
        assert_eq!(found["results"][0]["id"], json!(id));
    }
    let russian = fx.session(Some("work"), Some(WORK_A)).await;
    fx.write(
        &russian,
        "Правила релиза",
        "Релизы собираются по вторникам",
        "domain",
    )
    .await;
    let found = fx
        .call(&russian, "memory.search", json!({"query": "релиз"}))
        .await
        .unwrap();
    assert_eq!(found["count"], json!(1), "{found}");
}

#[sqlx::test(migrations = "./migrations")]
async fn secrets_and_bad_fields_are_refused_with_metadata(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let work = fx.session(Some("work"), Some(WORK_A)).await;
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123";
    let secret = error(
        fx.call(
            &work,
            "memory.write",
            json!({"kind": "environment", "subject": "CI", "body": format!("use {token} for CI")}),
        )
        .await,
    );
    assert_eq!(secret.code, Code::LooksLikeSecret);
    assert!(
        secret.message.contains("\"class\":\"token_prefix\""),
        "{}",
        secret.message
    );
    assert!(!secret.message.contains(token));
    assert!(
        secret.text().contains("secret_request"),
        "{}",
        secret.text()
    );
    let in_subject = error(
        fx.call(
            &work,
            "memory.write",
            json!({"kind": "environment", "subject": "password = Hunter2Hunter2", "body": "b"}),
        )
        .await,
    );
    assert_eq!(in_subject.code, Code::LooksLikeSecret);
    // Секрет в правке тоже отказ.
    let id = fx.write(&work, "Clean", "fine", "project").await;
    let edited = error(
        fx.call(
            &work,
            "memory.update",
            json!({"id": id, "expected_version": 1, "body": format!("now {token}")}),
        )
        .await,
    );
    assert_eq!(edited.code, Code::LooksLikeSecret);
    // Путь, хост и UUID проходят.
    fx.write(
        &work,
        "Paths",
        "see /h/work/alpha on build-runner-01.internal.example.net, id 0192e5a4-7c3b-7d2a-9f10-3b5c8d1e4a67",
        "project",
    )
    .await;

    let large = error(
        fx.call(
            &work,
            "memory.write",
            json!({"kind": "user", "subject": "Big", "body": "x ".repeat(5000)}),
        )
        .await,
    );
    assert_eq!(large.code, Code::TooLarge);
    assert!(
        large.message.contains("\"limit\":8192"),
        "{}",
        large.message
    );
    for (arguments, field) in [
        (
            json!({"kind": "user", "subject": "", "body": "b"}),
            "subject",
        ),
        (
            json!({"kind": "user", "subject": "two\nlines", "body": "b"}),
            "subject",
        ),
        (
            json!({"kind": "user", "subject": "s".repeat(161), "body": "b"}),
            "subject",
        ),
        (
            json!({"kind": "user", "subject": "s", "body": "  "}),
            "body",
        ),
        (
            json!({"kind": "nonsense", "subject": "s", "body": "b"}),
            "kind",
        ),
        (
            json!({"kind": "user", "subject": "s", "body": "b", "level": "global"}),
            "level",
        ),
        (
            json!({"kind": "user", "subject": "s", "body": "b", "valid_until": "soon"}),
            "valid_until",
        ),
    ] {
        let refused = error(fx.call(&work, "memory.write", arguments).await);
        assert_eq!(refused.code, Code::Invalid, "{field}");
        assert!(
            refused.message.contains(&format!("\"field\":\"{field}\"")),
            "{field}: {}",
            refused.message
        );
    }
    let bad_id = error(
        fx.call(&work, "memory.get", json!({"id": "not-a-uuid"}))
            .await,
    );
    assert_eq!(bad_id.code, Code::Invalid);
    let empty_patch = error(
        fx.call(
            &work,
            "memory.update",
            json!({"id": id, "expected_version": 1}),
        )
        .await,
    );
    assert_eq!(empty_patch.code, Code::Invalid);
    let unknown = error(
        fx.call(
            &work,
            "memory.write",
            json!({"kind": "user", "subject": "s", "body": "b", "domain": "personal"}),
        )
        .await,
    );
    assert_eq!(unknown.code, Code::InvalidArguments);
}

#[sqlx::test(migrations = "./migrations")]
async fn owner_scope_writes_skip_the_detector_but_agents_cannot(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let owner = MemoryScope::owner(MemoryDomain::Personal, None);
    let mut conn = fx.pool.acquire().await.unwrap();
    let written = store::write(
        &mut conn,
        &owner,
        Actor::Owner,
        NewEntry {
            body: "token = Abcdefgh12345678".into(),
            ..new_entry("Owner note")
        },
        &fx.memory,
    )
    .await
    .unwrap();
    assert_eq!(written.level, Level::Domain);
    assert!(matches!(
        store::write(
            &mut conn,
            &owner,
            Actor::Import {
                bundle_id: "bundle".into()
            },
            NewEntry {
                body: "token = Abcdefgh12345678".into(),
                ..new_entry("Imported note")
            },
            &fx.memory,
        )
        .await,
        Err(MemoryError::LooksLikeSecret { .. })
    ));
}
