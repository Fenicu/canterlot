//! Конкретная модель за псевдонимом: исполнение, проекция сессии и последнее
//! разрешение псевдонима на машине со сменой версии.
use canterlot_hub::{
    activity::{self, Thresholds},
    config::ActivityConfig,
    conversations, executions,
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::ExecutionState,
    model_resolution,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{record_body::Body, *};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

async fn run_execution(pool: &PgPool, seed: &Seed, blocks: &[(&str, &str)]) -> Uuid {
    let execution = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    let mut bodies = vec![Body::JournalOpened(JournalOpened {
        execution_id: execution.execution_id.to_string(),
        generation: execution.generation as u64,
        supervisor_protocol_version: 1,
    })];
    for (agent, model) in blocks {
        bodies.push(Body::BlockStarted(BlockStarted {
            block_id: Uuid::now_v7().to_string(),
            kind: "text".into(),
            agent: (*agent).into(),
            model: (*model).into(),
            ..Default::default()
        }));
    }
    let count = bodies.len() as u64;
    let batch = JournalRecords {
        journal_id: Uuid::now_v7().to_string(),
        records: bodies
            .into_iter()
            .enumerate()
            .map(|(index, body)| JournalRecord {
                seq: index as u64 + 1,
                body: Some(RecordBody { body: Some(body) }),
            })
            .collect(),
    };
    assert_eq!(
        Ingest::new(pool.clone())
            .apply_batch(
                seed.machine_id,
                &seed.credential_digest(),
                JournalKind::Sj,
                &batch
            )
            .await
            .unwrap(),
        IngestOutcome::Committed { up_to_seq: count }
    );
    for (state, outcome) in [
        (ExecutionState::Stopping, None),
        (ExecutionState::Stopped, Some("normal_exit")),
    ] {
        executions::set_state(pool, execution.execution_id, state, outcome, Uuid::now_v7())
            .await
            .unwrap();
    }
    execution.execution_id
}

async fn set_model(pool: &PgPool, seed: &Seed, model: &str) {
    sqlx::query("update conversations set model = $2 where id = $1")
        .bind(seed.conversation_id)
        .bind(model)
        .execute(pool)
        .await
        .unwrap();
}

async fn resolved_detail(pool: &PgPool, seed: &Seed) -> Option<String> {
    activity::refresh(
        pool,
        seed.conversation_id,
        Utc::now(),
        &Thresholds::from(&ActivityConfig::default()),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    conversations::detail(&mut conn, seed.owner_id, seed.conversation_id)
        .await
        .unwrap()
        .unwrap()
        .model_resolved
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn alias_resolution_is_stored_and_tracks_new_version(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    set_model(&pool, &seed, "fable[1m]").await;
    assert_eq!(resolved_detail(&pool, &seed).await, None);

    // Субагент на Haiku не подменяет модель основного агента.
    let first = run_execution(
        &pool,
        &seed,
        &[("", "claude-fable-5-1"), ("Explore", "claude-haiku-5-5")],
    )
    .await;
    let pair: (Option<String>, Option<String>) =
        sqlx::query_as("select model_requested, model_resolved from executions where id = $1")
            .bind(first)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        pair,
        (Some("fable[1m]".into()), Some("claude-fable-5-1".into()))
    );
    assert_eq!(
        resolved_detail(&pool, &seed).await.as_deref(),
        Some("claude-fable-5-1")
    );

    run_execution(&pool, &seed, &[("main", "claude-fable-5-2")]).await;
    assert_eq!(
        resolved_detail(&pool, &seed).await.as_deref(),
        Some("claude-fable-5-2")
    );
    let mut conn = pool.acquire().await.unwrap();
    let machine = model_resolution::machine_resolutions(&mut conn, seed.machine_id)
        .await
        .unwrap();
    let fable = &machine[&("synthetic".to_owned(), "fable[1m]".to_owned())];
    assert_eq!(fable.model, "claude-fable-5-2");
    assert_eq!(fable.previous.as_deref(), Some("claude-fable-5-1"));
    assert!(fable.changed_at.is_some());
    assert_eq!(machine.len(), 1, "субагент не даёт разрешения псевдониму");

    // Повтор той же версии сохраняет отметку о смене.
    drop(conn);
    run_execution(&pool, &seed, &[("", "claude-fable-5-2")]).await;
    let mut conn = pool.acquire().await.unwrap();
    let again = model_resolution::machine_resolutions(&mut conn, seed.machine_id)
        .await
        .unwrap();
    assert_eq!(
        again[&("synthetic".to_owned(), "fable[1m]".to_owned())].changed_at,
        fable.changed_at
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn switched_alias_falls_back_to_machine_resolution(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    set_model(&pool, &seed, "opus[1m]").await;
    run_execution(&pool, &seed, &[("", "claude-opus-5-5")]).await;
    // Другая сессия той же машины уже видела, во что разрешается `fable[1m]`.
    sqlx::query("insert into model_resolutions (machine_id, harness, alias, resolved, observed_at) values ($1, 'synthetic', 'fable[1m]', 'claude-fable-5-2', now())")
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    set_model(&pool, &seed, "fable[1m]").await;
    assert_eq!(
        resolved_detail(&pool, &seed).await.as_deref(),
        Some("claude-fable-5-2")
    );
    // Блок прежней модели после смены не записывается ни в исполнение, ни на машину.
    let late = run_execution(&pool, &seed, &[("", "claude-opus-5-5")]).await;
    let resolved: Option<String> =
        sqlx::query_scalar("select model_resolved from executions where id = $1")
            .bind(late)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(resolved, None);
    // Полное имя модели — не псевдоним: на машине не запоминается.
    set_model(&pool, &seed, "claude-opus-5-5").await;
    run_execution(&pool, &seed, &[("", "claude-opus-5-5")]).await;
    assert_eq!(
        resolved_detail(&pool, &seed).await.as_deref(),
        Some("claude-opus-5-5")
    );
    let aliases: Vec<String> = sqlx::query_scalar(
        "select alias from model_resolutions where machine_id = $1 order by alias",
    )
    .bind(seed.machine_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(aliases, ["fable[1m]", "opus[1m]"]);
}
