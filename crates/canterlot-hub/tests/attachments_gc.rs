#[path = "support/task9.rs"]
mod support;

use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use canterlot_hub::{
    attachments::{
        self, BlobError, BlobStore, StoredFile,
        gc::{self, GcReport},
    },
    conversations::{self, SubmitMessage},
    delivery::{ContentBlock, SubmitError},
};
use chrono::{TimeDelta, Utc};
use futures_util::stream::BoxStream;
use serde_json::json;
use sqlx::PgPool;
use support::*;
use uuid::Uuid;

const DAY: Duration = Duration::from_secs(86400);

fn set_mtime(path: &std::path::Path, hours_ago: u64) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(hours_ago * 3600))
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn gc_expires_stale_uploads_after_the_grace(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let data = content(4096, 1);
    let stale = f
        .create("stale.bin", "application/octet-stream", &data)
        .await;
    assert_eq!(f.patch_bytes(stale, 0, &data[..1000]).await.status, 204);
    let never = f
        .create("never.bin", "application/octet-stream", &data)
        .await;
    let active = f
        .create("active.bin", "application/octet-stream", &data)
        .await;
    assert_eq!(f.patch_bytes(active, 0, &data[..1000]).await.status, 204);

    assert_eq!(
        gc::run_once(&f.pool, f.blobs(), Utc::now(), DAY)
            .await
            .unwrap(),
        GcReport::default()
    );
    for id in [stale, never, active] {
        f.age(id, 25).await;
    }
    // Каждая часть загрузки и попытка завершения отмечают активность.
    assert_eq!(
        f.patch_bytes(active, 1000, &data[1000..2000]).await.status,
        204
    );
    let report = gc::run_once(&f.pool, f.blobs(), Utc::now(), DAY)
        .await
        .unwrap();
    assert_eq!(report.expired_uploads, 2);
    for id in [stale, never] {
        assert_eq!(f.state_of(id).await, "failed");
        let reason: Option<String> =
            sqlx::query_scalar("select fail_reason from attachments where id=$1")
                .bind(id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(reason.as_deref(), Some("upload_expired"));
    }
    assert!(!f.staging_path(stale).exists());
    assert_eq!(f.state_of(active).await, "uploading");
    assert!(f.staging_path(active).exists());

    f.age(active, 25).await;
    let early = f.complete(active, "early", None).await;
    assert_eq!(early.status, 409);
    assert_eq!(
        gc::run_once(&f.pool, f.blobs(), Utc::now(), DAY)
            .await
            .unwrap()
            .expired_uploads,
        0,
        "complete refreshes updated_at before verifying"
    );
    // Тело, которое пишется в этом процессе, сборщик не трогает при любой отсрочке.
    f.age(active, 25).await;
    let guard = f.blobs().try_lock(active).unwrap().unwrap();
    assert_eq!(
        gc::run_once(&f.pool, f.blobs(), Utc::now(), DAY)
            .await
            .unwrap()
            .expired_uploads,
        0
    );
    drop(guard);
    assert_eq!(
        gc::run_once(&f.pool, f.blobs(), Utc::now(), DAY)
            .await
            .unwrap()
            .expired_uploads,
        1
    );
    let failed: Vec<serde_json::Value> = f
        .events("attachment_state_changed")
        .await
        .into_iter()
        .filter(|p| p["state"] == "failed")
        .collect();
    assert_eq!(failed.len(), 3);
    assert!(failed.iter().all(|p| p["reason"] == "upload_expired"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn gc_releases_unreferenced_ready_attachments_and_keeps_referenced_or_scheduled_ones(
    pool: PgPool,
) {
    let f = Fixture::new(pool).await;
    let unused = f.ready("unused.txt", "text/plain", b"unused").await;
    let linked = f.ready("linked.txt", "text/plain", b"linked").await;
    let planned = f.ready("planned.txt", "text/plain", b"planned").await;
    let fresh = f.ready("fresh.txt", "text/plain", b"fresh").await;
    assert_eq!(f.submit("link", "t", &[linked]).await.status, 201);
    let schedule = f
        .client
        .send(
            axum::http::Method::POST,
            &format!("/api/v1/conversations/{}/scheduled", f.conversation),
            Some("plan"),
            Some(json!({"run_at": (Utc::now() + TimeDelta::hours(1)).to_rfc3339(), "content":[], "attachment_ids":[planned]})),
        )
        .await;
    assert_eq!(schedule.status, 201);
    for id in [unused, linked, planned] {
        f.age(id, 25).await;
    }
    let report = gc::run_once(&f.pool, f.blobs(), Utc::now(), DAY)
        .await
        .unwrap();
    assert_eq!(
        report,
        GcReport {
            released: 1,
            ..Default::default()
        }
    );
    assert_eq!(f.state_of(unused).await, "released");
    assert!(!f.blob_path(unused).exists());
    for id in [linked, planned, fresh] {
        assert_eq!(f.state_of(id).await, "ready");
        assert!(f.blob_path(id).exists());
    }
    let released: Vec<serde_json::Value> = f
        .events("attachment_state_changed")
        .await
        .into_iter()
        .filter(|p| p["state"] == "released")
        .collect();
    assert_eq!(released.len(), 1);
    assert_eq!(released[0]["attachment_id"], unused.to_string());
    assert_error(
        &f.submit("late_link", "t", &[unused]).await,
        409,
        "attachment_not_ready",
        "after_condition",
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn gc_removes_orphan_blobs_and_staging_without_a_row(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let data = content(2048, 2);
    // Файлы без строки: оставлены сбоем до создания записи или после её удаления.
    let lone_staging = Uuid::now_v7();
    let lone_blob = Uuid::now_v7();
    std::fs::write(f.staging_path(lone_staging), &data).unwrap();
    std::fs::create_dir_all(f.blob_path(lone_blob).parent().unwrap()).unwrap();
    std::fs::write(f.blob_path(lone_blob), &data).unwrap();
    let fresh_orphan = Uuid::now_v7();
    std::fs::write(f.staging_path(fresh_orphan), &data).unwrap();
    // Файлы конечных записей и записи, которой файл уже не нужен.
    let failed = f
        .create("failed.bin", "application/octet-stream", &data)
        .await;
    f.upload(failed, &data).await;
    sqlx::query("update attachments set state='failed' where id=$1")
        .bind(failed)
        .execute(&f.pool)
        .await
        .unwrap();
    let ready = f
        .ready("ready.bin", "application/octet-stream", &data)
        .await;
    assert_eq!(f.submit("keep", "t", &[ready]).await.status, 201);
    std::fs::write(f.staging_path(ready), &data).unwrap();
    let uploading = f.create("up.bin", "application/octet-stream", &data).await;
    assert_eq!(f.patch_bytes(uploading, 0, &data[..100]).await.status, 204);
    let released_blob = f.ready("rel.bin", "application/octet-stream", &data).await;
    sqlx::query("update attachments set state='released' where id=$1")
        .bind(released_blob)
        .execute(&f.pool)
        .await
        .unwrap();
    for path in [
        f.staging_path(lone_staging),
        f.blob_path(lone_blob),
        f.staging_path(failed),
        f.staging_path(ready),
        f.blob_path(ready),
        f.staging_path(uploading),
        f.blob_path(released_blob),
    ] {
        set_mtime(&path, 30);
    }
    let report = gc::run_once(&f.pool, f.blobs(), Utc::now(), DAY)
        .await
        .unwrap();
    assert_eq!(
        report,
        GcReport {
            expired_uploads: 0,
            released: 0,
            orphan_blobs: 2,
            orphan_staging: 3,
            unreferenced_media: 0,
        }
    );
    for gone in [
        f.staging_path(lone_staging),
        f.blob_path(lone_blob),
        f.staging_path(failed),
        f.staging_path(ready),
        f.blob_path(released_blob),
    ] {
        assert!(!gone.exists(), "{gone:?}");
    }
    for kept in [
        f.blob_path(ready),
        f.staging_path(uploading),
        f.staging_path(fresh_orphan),
    ] {
        assert!(kept.exists(), "{kept:?}");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn gc_never_removes_blob_under_concurrent_link(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let mut conversation = f.conversation;
    let (mut linked, mut refused) = (0, 0);
    for round in 0..200u32 {
        if round % 50 == 49 {
            conversation = f.conversation().await;
        }
        let data = content(256, round as u8);
        let id = f
            .ready_in(conversation, "race.bin", "application/octet-stream", &data)
            .await;
        // Разброс задержек покрывает время до блокировки строки в обоих путях.
        let gc_delay = Duration::from_millis(u64::from(round % 13));
        let submit_delay = Duration::from_millis(u64::from((round / 13) % 3));
        let collect = async {
            tokio::time::sleep(gc_delay).await;
            gc::run_once(&f.pool, f.blobs(), Utc::now(), Duration::ZERO).await
        };
        let submit = async {
            tokio::time::sleep(submit_delay).await;
            conversations::submit_message(
                &f.pool,
                SubmitMessage {
                    owner_id: f.owner,
                    conversation_id: conversation,
                    key: format!("race_{round}"),
                    content: vec![ContentBlock::Text { text: "t".into() }],
                    attachment_ids: vec![id],
                    retry_of: None,
                    received_at: Utc::now(),
                },
                Uuid::now_v7(),
            )
            .await
        };
        let (collected, submitted) = tokio::join!(collect, submit);
        collected.unwrap();
        let state = f.state_of(id).await;
        match submitted {
            Ok(_) => {
                linked += 1;
                assert_eq!(state, "ready", "round {round}");
                assert_eq!(
                    std::fs::read(f.blob_path(id)).unwrap(),
                    data,
                    "round {round}: a linked attachment keeps its content"
                );
            }
            Err(SubmitError::Attachment(attachments::AttachmentError::NotReady)) => {
                refused += 1;
                assert_eq!(state, "released", "round {round}");
                let inputs: i64 =
                    sqlx::query_scalar("select count(*) from inputs where idempotency_key=$1")
                        .bind(format!("race_{round}"))
                        .fetch_one(&f.pool)
                        .await
                        .unwrap();
                assert_eq!(inputs, 0);
            }
            Err(other) => panic!("round {round}: {other:?}"),
        }
    }
    let ready: Vec<Uuid> = sqlx::query_scalar("select id from attachments where state='ready'")
        .fetch_all(&f.pool)
        .await
        .unwrap();
    for id in &ready {
        assert!(f.blob_path(*id).exists(), "ready {id} lacks its blob");
    }
    let linked_not_ready: i64 = f
        .count(
            "select count(*) from attachments a join input_attachments ia on ia.attachment_id=a.id
             where a.state <> 'ready'",
        )
        .await;
    assert_eq!(linked_not_ready, 0, "a linked attachment is never released");
    eprintln!("link won {linked}, gc won {refused}");
    assert!(
        linked >= 1,
        "the link must win at least once ({linked}/{refused})"
    );
    assert!(
        refused >= 1,
        "the GC must win at least once ({linked}/{refused})"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn gc_skips_locked_rows_and_is_safe_to_run_twice(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let data = content(1024, 3);
    let ready = f
        .ready("ready.bin", "application/octet-stream", &data)
        .await;
    let uploading = f.create("up.bin", "application/octet-stream", &data).await;
    assert_eq!(f.patch_bytes(uploading, 0, &data[..10]).await.status, 204);
    f.age(ready, 25).await;
    f.age(uploading, 25).await;
    let mut holder = f.pool.begin().await.unwrap();
    sqlx::query("select id from attachments where id=$1 for share")
        .bind(ready)
        .execute(&mut *holder)
        .await
        .unwrap();
    sqlx::query("select id from attachments where id=$1 for update")
        .bind(uploading)
        .execute(&mut *holder)
        .await
        .unwrap();
    let report = tokio::time::timeout(
        Duration::from_secs(5),
        gc::run_once(&f.pool, f.blobs(), Utc::now(), DAY),
    )
    .await
    .expect("locked rows are skipped, not waited for")
    .unwrap();
    assert_eq!(report, GcReport::default());
    assert_eq!(f.state_of(ready).await, "ready");
    assert_eq!(f.state_of(uploading).await, "uploading");
    holder.commit().await.unwrap();
    let report = gc::run_once(&f.pool, f.blobs(), Utc::now(), DAY)
        .await
        .unwrap();
    assert_eq!(
        report,
        GcReport {
            expired_uploads: 1,
            released: 1,
            ..Default::default()
        }
    );
    let events = f.events("attachment_state_changed").await.len();
    assert_eq!(
        gc::run_once(&f.pool, f.blobs(), Utc::now(), DAY)
            .await
            .unwrap(),
        GcReport::default()
    );
    assert_eq!(f.events("attachment_state_changed").await.len(), events);
    assert_eq!(f.state_of(ready).await, "released");
    assert_eq!(f.state_of(uploading).await, "failed");
}

/// Обёртка, которая видит состояние строки в момент удаления blob и может сорвать удаление.
#[derive(Clone)]
struct Observing {
    inner: Arc<dyn BlobStore>,
    pool: PgPool,
    seen: Arc<Mutex<Vec<String>>>,
    fail_next: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl BlobStore for Observing {
    fn try_lock(&self, id: Uuid) -> io::Result<Option<Box<dyn BlobStore>>> {
        Ok(self.inner.try_lock(id)?.map(|inner| {
            let mut store = self.clone();
            store.inner = Arc::from(inner);
            Box::new(store) as Box<dyn BlobStore>
        }))
    }
    async fn staging_len(&self, id: Uuid) -> io::Result<Option<u64>> {
        self.inner.staging_len(id).await
    }
    async fn append_staging(
        &self,
        id: Uuid,
        offset: u64,
        body: BoxStream<'static, Result<Bytes, io::Error>>,
        max_total: u64,
    ) -> Result<u64, BlobError> {
        self.inner.append_staging(id, offset, body, max_total).await
    }
    async fn publish(&self, id: Uuid, len: u64, sha: &str) -> Result<(), BlobError> {
        self.inner.publish(id, len, sha).await
    }
    async fn open(&self, id: Uuid, offset: u64) -> Result<(tokio::fs::File, u64), BlobError> {
        self.inner.open(id, offset).await
    }
    async fn delete_staging(&self, id: Uuid) -> io::Result<()> {
        self.inner.delete_staging(id).await
    }
    async fn delete_blob(&self, id: Uuid) -> io::Result<()> {
        let state: String = sqlx::query_scalar("select state from attachments where id=$1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap();
        self.seen.lock().unwrap().push(state);
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(io::Error::other("disk busy"));
        }
        self.inner.delete_blob(id).await
    }
    async fn scan(&self) -> io::Result<Vec<StoredFile>> {
        self.inner.scan().await
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn gc_commits_released_before_deleting_the_blob(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let store = Observing {
        inner: f.store.clone(),
        pool: f.pool.clone(),
        seen: Arc::new(Mutex::new(Vec::new())),
        fail_next: Arc::new(AtomicBool::new(false)),
    };
    let first = f
        .ready("first.bin", "application/octet-stream", b"first")
        .await;
    f.age(first, 25).await;
    let report = gc::run_once(&f.pool, &store, Utc::now(), DAY)
        .await
        .unwrap();
    assert_eq!(report.released, 1);
    assert_eq!(
        *store.seen.lock().unwrap(),
        vec!["released".to_owned()],
        "another connection already sees the committed release"
    );
    assert!(!f.blob_path(first).exists());

    // Сорванное удаление оставляет `released` без готовой ссылки; файл подбирает orphan-обход.
    let second = f
        .ready("second.bin", "application/octet-stream", b"second")
        .await;
    f.age(second, 25).await;
    store.fail_next.store(true, Ordering::SeqCst);
    let report = gc::run_once(&f.pool, &store, Utc::now(), DAY)
        .await
        .unwrap();
    assert_eq!(report.released, 1);
    assert_eq!(f.state_of(second).await, "released");
    assert!(f.blob_path(second).exists());
    set_mtime(&f.blob_path(second), 30);
    let report = gc::run_once(&f.pool, &store, Utc::now(), DAY)
        .await
        .unwrap();
    assert_eq!(report.orphan_blobs, 1);
    assert!(!f.blob_path(second).exists());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn gc_service_holds_its_singleton_lock_and_stops_on_shutdown(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let orphan = Uuid::now_v7();
    std::fs::write(f.staging_path(orphan), b"orphan").unwrap();
    set_mtime(&f.staging_path(orphan), 30);
    let config = canterlot_hub::config::AttachmentsConfig {
        gc_interval: Duration::from_millis(50),
        ..f.config.clone()
    };
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let store: Arc<dyn BlobStore> = f.store.clone();
    let first = tokio::spawn(gc::run(
        f.pool.clone(),
        store.clone(),
        config.clone(),
        stopped.clone(),
    ));
    let second = tokio::spawn(gc::run(f.pool.clone(), store, config, stopped));
    let held = "select count(*) from pg_locks where locktype='advisory' and granted
        and database=(select oid from pg_database where datname=current_database())
        and objid::bigint=(hashtextextended('canterlot:attachment-gc',0) & 4294967295)";
    tokio::time::timeout(Duration::from_secs(10), async {
        while f.staging_path(orphan).exists() || f.count(held).await != 1 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("one instance holds the lock and runs a pass");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(f.count(held).await, 1, "the second instance stays passive");
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        first.await.unwrap();
        second.await.unwrap();
    })
    .await
    .expect("both instances stop on shutdown");
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.count(held).await != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the lock is released");
}
