//! Удаление отправителя, чьё вложение переслано в ввод другой сессии (sessions.send /
//! sessions.spawn): вложение переходит к получателю и удаляется с последней ссылкой.

use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    config::SessionToolsConfig,
    deletion,
    session_tools::{self, Caller, ToolContext, settle::SettleConfig},
    testkit,
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sender_with_forwarded_attachment_can_be_deleted(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let a = seed.conversation_id;
    let b = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    sqlx::query("update machines set availability='available',capabilities=array['pilot_v1'],limits_ok=true where id=$1")
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    let context = ToolContext {
        pool: pool.clone(),
        config: Arc::new(SessionToolsConfig::default()),
        limits: Default::default(),
        settle: SettleConfig {
            idle_quiet: Duration::from_secs(5),
            idle_background_max: Duration::from_secs(15 * 60),
            nudge_enabled: true,
        },
        alarms_enabled: true,
        nudge_enabled: true,
        routing_enabled: true,
        routing_stale: Duration::from_secs(1800),
        routing_queue_horizon: Duration::from_secs(6 * 3600),
        spawn_queue: Default::default(),
        guards: Default::default(),
        dispatcher: None,
        registry: canterlot_hub::session::Registry::new(Default::default()),
        shutdown: tokio_util::sync::CancellationToken::new(),
    };
    let file = Uuid::now_v7();
    sqlx::query(
        "insert into attachments (id,owner_id,conversation_id,name,mime,size_bytes,sha256,state,ready_at)
         values($1,$2,$3,'r.pdf','application/pdf',4,$4,'ready',clock_timestamp())",
    )
    .bind(file)
    .bind(seed.owner_id)
    .bind(a)
    .bind("0".repeat(64))
    .execute(&pool)
    .await
    .unwrap();
    let caller = Caller {
        owner_id: seed.owner_id,
        conversation_id: a,
        execution_id: Uuid::now_v7(),
        machine_id: seed.machine_id,
        call_id: Uuid::now_v7(),
    };
    session_tools::call(
        &context,
        &caller,
        "sessions.send",
        json!({"session_id": b, "text": "file", "attachment_ids": [file]}),
    )
    .await
    .unwrap();

    let directory = tempfile::tempdir().unwrap();
    let (config, _) = testkit::test_blob_store(directory.path());
    let store = Arc::new(
        canterlot_hub::attachments::LocalBlobStore::open(
            &config,
            Arc::new(canterlot_hub::attachments::NoTrace),
        )
        .unwrap(),
    );
    let blob = store.blob_path(file);
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::write(&blob, b"%PDF").unwrap();
    let blobs: Arc<dyn canterlot_hub::attachments::BlobStore> = store.clone();
    let result = deletion::delete(
        &pool,
        blobs.as_ref(),
        seed.owner_id,
        a,
        "review-delete-a",
        Uuid::now_v7(),
    )
    .await;
    assert!(
        result.is_ok(),
        "deleting the sender conversation failed: {result:?}"
    );
    let owner: Option<Uuid> =
        sqlx::query_scalar("select conversation_id from attachments where id=$1")
            .bind(file)
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert_eq!(
        owner,
        Some(b),
        "the forwarded attachment must move to the recipient"
    );
    assert!(blob.exists(), "the recipient still references the blob");

    let result = deletion::delete(
        &pool,
        blobs.as_ref(),
        seed.owner_id,
        b,
        "review-delete-b",
        Uuid::now_v7(),
    )
    .await;
    assert!(result.is_ok(), "deleting the recipient failed: {result:?}");
    let left: bool = sqlx::query_scalar("select exists(select 1 from attachments where id=$1)")
        .bind(file)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!left, "the last reference is gone, so is the attachment");
    assert!(!blob.exists(), "the blob is freed with the last reference");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
/// Пустой файл (размер 0, SHA-256 пустого ввода) пересылается как любое готовое вложение:
/// ввод цели получает ссылку на него, а команда машине несёт размер 0.
async fn empty_attachment_is_forwarded_by_sessions_send(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let a = seed.conversation_id;
    let b = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    sqlx::query("update machines set availability='available',capabilities=array['pilot_v1'],limits_ok=true where id=$1")
        .bind(seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    let context = ToolContext {
        pool: pool.clone(),
        config: Arc::new(SessionToolsConfig::default()),
        limits: Default::default(),
        settle: SettleConfig {
            idle_quiet: Duration::from_secs(5),
            idle_background_max: Duration::from_secs(15 * 60),
            nudge_enabled: true,
        },
        alarms_enabled: true,
        nudge_enabled: true,
        routing_enabled: true,
        routing_stale: Duration::from_secs(1800),
        routing_queue_horizon: Duration::from_secs(6 * 3600),
        spawn_queue: Default::default(),
        guards: Default::default(),
        dispatcher: None,
        registry: canterlot_hub::session::Registry::new(Default::default()),
        shutdown: tokio_util::sync::CancellationToken::new(),
    };
    let file = Uuid::now_v7();
    sqlx::query(
        "insert into attachments (id,owner_id,conversation_id,name,mime,size_bytes,sha256,state,ready_at)
         values($1,$2,$3,'empty.txt','text/plain',0,$4,'ready',clock_timestamp())",
    )
    .bind(file)
    .bind(seed.owner_id)
    .bind(a)
    .bind("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    .execute(&pool)
    .await
    .unwrap();
    let caller = Caller {
        owner_id: seed.owner_id,
        conversation_id: a,
        execution_id: Uuid::now_v7(),
        machine_id: seed.machine_id,
        call_id: Uuid::now_v7(),
    };
    session_tools::call(
        &context,
        &caller,
        "sessions.send",
        json!({"session_id": b, "text": "empty file", "attachment_ids": [file]}),
    )
    .await
    .unwrap();

    let forwarded: (i64, i64) = sqlx::query_as(
        "select count(*), coalesce(sum(a.size_bytes),0)::bigint from input_attachments ia
         join inputs i on i.id=ia.input_id join attachments a on a.id=ia.attachment_id
         where ia.attachment_id=$1 and i.conversation_id=$2",
    )
    .bind(file)
    .bind(b)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(forwarded, (1, 0));
}
