#[path = "support/task9.rs"]
mod support;

use std::{
    os::unix::fs::MetadataExt,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    http::Method,
};
use canterlot_hub::{
    activity,
    attachments::{self, PublishStep},
    config::ActivityConfig,
    conversations,
    delivery::dispatch::{self, BlockReason, DispatchOutcome, GateInputs},
    executions,
    model::ExecutionState,
    outbox,
    testkit::{self, TestClient},
};
use canterlot_proto::{
    limits::MAX_ATTACHMENT_BYTES,
    worker::v1::{command, content_block},
};
use chrono::{TimeDelta, Utc};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use support::*;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Тест памяти измеряет RSS всего процесса и потому идёт один; остальные делят доступ.
static HEAVY: RwLock<()> = RwLock::const_new(());

const MIB: usize = 1024 * 1024;
const BLOCK: usize = 64 * 1024;

fn frames(bytes: &[u8], frame: usize) -> Body {
    let frames: Vec<Result<Bytes, std::io::Error>> = bytes
        .chunks(frame)
        .map(|c| Ok(Bytes::copy_from_slice(c)))
        .collect();
    Body::from_stream(stream::iter(frames))
}

/// Блок 64 КиБ псевдослучайных данных, детерминированный по номеру.
fn block(index: u64) -> Bytes {
    let mut state = index.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(BLOCK);
    while out.len() < BLOCK {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    Bytes::from(out)
}

fn generated_sha(size: u64) -> String {
    let mut hasher = Sha256::new();
    for index in 0..size.div_ceil(BLOCK as u64) {
        let data = block(index);
        let take = (size - index * BLOCK as u64).min(BLOCK as u64) as usize;
        hasher.update(&data[..take]);
    }
    attachments::blobstore::hex(&hasher.finalize())
}

/// Тело `PATCH` для диапазона сгенерированного файла; `probe` вызывается на каждом кадре.
fn generated_body(start: u64, end: u64, probe: Option<Arc<dyn Fn() + Send + Sync>>) -> Body {
    let first = start / BLOCK as u64;
    let last = end.div_ceil(BLOCK as u64);
    let frames = stream::iter(first..last).map(move |index| {
        if let Some(probe) = &probe {
            probe();
        }
        let data = block(index);
        let from = (start.max(index * BLOCK as u64) - index * BLOCK as u64) as usize;
        let to = (end.min((index + 1) * BLOCK as u64) - index * BLOCK as u64) as usize;
        Ok::<_, std::io::Error>(data.slice(from..to))
    });
    Body::from_stream(frames)
}

fn rss_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .unwrap()
        * 1024
}

async fn upload_generated(
    f: &Fixture,
    id: Uuid,
    size: u64,
    probe: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    let mut offset = 0;
    while offset < size {
        let end = (offset + attachments::UPLOAD_CHUNK_BYTES).min(size);
        let response = f
            .patch(id, offset, generated_body(offset, end, probe.clone()))
            .await;
        assert_eq!(response.status, 204, "{:?}", response.body);
        assert_eq!(upload_offset(&response), Some(end));
        offset = end;
    }
}

fn key() -> String {
    format!("k{}", Uuid::now_v7().simple())
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn create_validates_size_name_and_ceiling(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let post = |body: Value| {
        let client = f.client.clone();
        async move {
            client
                .send(
                    Method::POST,
                    "/api/v1/attachments",
                    Some(&key()),
                    Some(body),
                )
                .await
        }
    };
    let body = |name: &str, size: u64| json!({"conversation_id": f.conversation, "name": name, "size": size, "sha256": "ab".repeat(32)});
    assert_error(
        &post(body("a.txt", 0)).await,
        400,
        "invalid_request",
        "never",
    );
    assert_error(
        &post(body("a.txt", MAX_ATTACHMENT_BYTES + 1)).await,
        413,
        "resource_limit",
        "never",
    );
    let long = format!("{}.txt", "я".repeat(126));
    assert!(long.len() > 255);
    for bad in [
        "a\u{0}b",
        "line\nbreak",
        "tab\there",
        "esc\u{1b}",
        "dir/",
        "..",
        "a/..",
        "",
        &long,
    ] {
        assert_error(&post(body(bad, 10)).await, 400, "invalid_request", "never");
    }
    for (raw, stored) in [
        ("../../etc/passwd", "passwd"),
        ("C:\\Users\\me\\report.pdf", "report.pdf"),
        ("отчёт за год.xlsx", "отчёт за год.xlsx"),
    ] {
        let created = post(body(raw, 10)).await;
        assert_eq!(created.status, 201);
        assert_eq!(created.json::<Value>()["name"], stored);
    }
    let max_name = "x".repeat(255);
    assert_eq!(post(body(&max_name, 10)).await.status, 201);
    for bad_sha in ["xyz", &"g".repeat(64), &"a".repeat(63)] {
        let mut request = body("a.txt", 10);
        request["sha256"] = json!(bad_sha);
        assert_error(&post(request).await, 400, "invalid_request", "never");
    }
    let mut upper = body("a.txt", 10);
    upper["sha256"] = json!("AB".repeat(32));
    assert_eq!(post(upper).await.json::<Value>()["sha256"], "ab".repeat(32));
    let mut foreign = body("a.txt", 10);
    foreign["conversation_id"] = json!(Uuid::now_v7());
    assert_error(&post(foreign).await, 404, "not_found", "never");
    let mut typed = body("pic.png", 10);
    typed["mime"] = json!("Image/PNG; charset=binary");
    assert_eq!(post(typed).await.json::<Value>()["mime"], "image/png");
    let mut bad_mime = body("pic.png", 10);
    bad_mime["mime"] = json!("not a mime");
    assert_error(&post(bad_mime).await, 400, "invalid_request", "never");
    let created = post(body("a.bin", 10)).await;
    assert_eq!(created.status, 201);
    let view: Value = created.json();
    assert_eq!(view["mime"], "application/octet-stream");
    assert_eq!(view["state"], "created");
    assert_eq!(view["size"], 10);
    assert_eq!(view["uploaded_bytes"], 0);
    assert_eq!(
        view["upload"],
        json!({"offset": 0, "upload_chunk_bytes": 8388608})
    );

    // Объявленный размер является потолком: лишний байт отклоняется, принятое остаётся.
    let data = content(11, 1);
    let id = f
        .create("ceiling.bin", "application/octet-stream", &data[..10])
        .await;
    let over = f.patch_bytes(id, 0, &data).await;
    assert_error(&over, 413, "resource_limit", "never");
    assert_eq!(upload_offset(&over), Some(10));
    let head = f.head(id).await;
    assert_eq!(head.status, 200);
    assert_eq!(upload_offset(&head), Some(10));
    assert_eq!(head.headers["upload-length"], "10");
    assert_eq!(std::fs::read(f.staging_path(id)).unwrap(), &data[..10]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn upload_streams_to_staging_with_continuous_offsets(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let data = content(300 * 1024, 2);
    let id = f
        .create("data.bin", "application/octet-stream", &data)
        .await;
    let first = f.patch_bytes(id, 0, &data[..100 * 1024]).await;
    assert_eq!(first.status, 204);
    assert_eq!(upload_offset(&first), Some(102400));
    for wrong in [0, 50, 102399, 204800] {
        let response = f.patch_bytes(id, wrong, &data[..100]).await;
        assert_error(&response, 409, "offset_mismatch", "after_condition");
        assert_eq!(upload_offset(&response), Some(102400), "offset {wrong}");
    }
    let missing = f
        .client
        .send_body(
            Method::PATCH,
            &format!("/api/v1/attachments/{id}/content"),
            &[],
            Body::from(vec![1u8; 10]),
        )
        .await;
    assert_error(&missing, 400, "invalid_request", "never");
    let rest = f.patch(id, 102400, frames(&data[102400..], 7000)).await;
    assert_eq!(rest.status, 204);
    assert_eq!(upload_offset(&rest), Some(data.len() as u64));
    assert_eq!(std::fs::read(f.staging_path(id)).unwrap(), data);
    assert_eq!(f.state_of(id).await, "uploading");
    let states: Vec<Value> = f
        .events("attachment_state_changed")
        .await
        .into_iter()
        .map(|p| p["state"].clone())
        .collect();
    assert_eq!(states, vec![json!("created"), json!("uploading")]);
    let view = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/attachments/{id}"),
            None,
            None,
        )
        .await;
    assert_eq!(view.json::<Value>()["uploaded_bytes"], data.len());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn interrupted_upload_resumes_from_head_offset(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let data = content(256 * 1024, 3);
    let id = f.create("cut.bin", "application/octet-stream", &data).await;
    let cut: Vec<Result<Bytes, std::io::Error>> = vec![
        Ok(Bytes::copy_from_slice(&data[..65536])),
        Ok(Bytes::copy_from_slice(&data[65536..131072])),
        Err(std::io::Error::other("connection reset")),
    ];
    let interrupted = f.patch(id, 0, Body::from_stream(stream::iter(cut))).await;
    assert_eq!(interrupted.status, 400);
    assert_eq!(upload_offset(&interrupted), Some(131072));
    let head = f.head(id).await;
    assert_eq!(head.status, 200);
    assert_eq!(upload_offset(&head), Some(131072));
    assert_eq!(head.headers["upload-length"], data.len().to_string());
    let resumed = f.patch_bytes(id, 131072, &data[131072..]).await;
    assert_eq!(resumed.status, 204);
    let done = f.complete(id, "resume_done", None).await;
    assert_eq!(done.status, 200);
    assert_eq!(done.json::<Value>()["state"], "ready");
    assert_eq!(std::fs::read(f.blob_path(id)).unwrap(), data);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn complete_verifies_length_and_checksum_then_publishes_durably(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let data = content(200_000, 4);
    let id = f
        .create("durable.bin", "application/octet-stream", &data)
        .await;
    assert_eq!(f.patch_bytes(id, 0, &data[..100_000]).await.status, 204);
    let early = f.complete(id, "early", None).await;
    assert_error(&early, 409, "offset_mismatch", "after_condition");
    assert_eq!(upload_offset(&early), Some(100_000));
    assert_eq!(f.state_of(id).await, "uploading");
    assert!(f.trace.steps().is_empty());
    assert_eq!(
        f.patch_bytes(id, 100_000, &data[100_000..]).await.status,
        204
    );

    let observed = Arc::new(Mutex::new(Vec::new()));
    let handle = tokio::runtime::Handle::current();
    let (pool, blob, sink) = (f.pool.clone(), f.blob_path(id), observed.clone());
    f.trace.hook(Some(Arc::new(move |step| {
        let state: String = handle
            .block_on(
                sqlx::query_scalar("select state from attachments where id=$1")
                    .bind(id)
                    .fetch_one(&pool),
            )
            .unwrap();
        sink.lock().unwrap().push((step, state, blob.exists()));
    })));
    let done = f.complete(id, "done", None).await;
    f.trace.hook(None);
    assert_eq!(done.status, 200);
    let view: Value = done.json();
    assert_eq!(view["state"], "ready");
    assert_eq!(view["uploaded_bytes"], data.len());
    assert_eq!(
        f.trace.steps(),
        vec![
            PublishStep::Verified,
            PublishStep::FileSynced,
            PublishStep::Renamed,
            PublishStep::DirSynced
        ]
    );
    let observed = observed.lock().unwrap().clone();
    assert!(
        observed.iter().all(|(_, state, _)| state == "uploading"),
        "{observed:?}"
    );
    assert_eq!(
        observed
            .iter()
            .map(|(_, _, exists)| *exists)
            .collect::<Vec<_>>(),
        vec![false, false, true, true]
    );
    assert_eq!(f.state_of(id).await, "ready");
    assert_eq!(std::fs::read(f.blob_path(id)).unwrap(), data);
    assert!(!f.staging_path(id).exists());

    // Неверная контрольная сумма: запись `failed`, staging удалён, публикации нет.
    f.trace.clear();
    let declared = content(1000, 5);
    let uploaded = content(1000, 6);
    let bad = f
        .create("bad.bin", "application/octet-stream", &declared)
        .await;
    f.upload(bad, &uploaded).await;
    let refused = f.complete(bad, "bad_done", None).await;
    assert_error(&refused, 422, "checksum_mismatch", "never");
    assert_eq!(f.state_of(bad).await, "failed");
    let reason: Option<String> =
        sqlx::query_scalar("select fail_reason from attachments where id=$1")
            .bind(bad)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(reason.as_deref(), Some("checksum_mismatch"));
    assert!(!f.staging_path(bad).exists());
    assert!(!f.blob_path(bad).exists());
    assert!(f.trace.steps().is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn complete_is_idempotent_and_conflicts_on_different_checksum(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let data = content(5000, 7);
    let id = f
        .create("same.bin", "application/octet-stream", &data)
        .await;
    f.upload(id, &data).await;
    let first = f.complete(id, "k1", None).await;
    assert_eq!(first.status, 200);
    let published = f.trace.steps().len();
    let replay = f.complete(id, "k1", None).await;
    assert_eq!(replay.status, 200);
    assert_eq!(replay.body, first.body);
    let same = f
        .complete(id, "k2", Some(json!({"sha256": sha(&data)})))
        .await;
    assert_eq!(same.status, 200);
    assert_eq!(same.json::<Value>(), first.json::<Value>());
    assert_eq!(f.trace.steps().len(), published, "no second publication");
    let other = sha(b"other content");
    assert_error(
        &f.complete(id, "k3", Some(json!({"sha256": other}))).await,
        409,
        "attachment_conflict",
        "never",
    );
    assert_error(
        &f.complete(id, "k1", Some(json!({"sha256": other}))).await,
        409,
        "idempotency_conflict",
        "never",
    );
    let pending = f
        .create("pending.bin", "application/octet-stream", &data)
        .await;
    f.upload(pending, &data).await;
    assert_error(
        &f.complete(pending, "k4", Some(json!({"sha256": other})))
            .await,
        409,
        "attachment_conflict",
        "never",
    );
    assert_eq!(f.state_of(pending).await, "uploading");
    let ready_events = f
        .events("attachment_state_changed")
        .await
        .into_iter()
        .filter(|p| p["attachment_id"] == id.to_string() && p["state"] == "ready")
        .count();
    assert_eq!(ready_events, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn complete_hashes_outside_a_transaction_and_commits_only_if_still_uploading(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let data = content(4 * MIB, 8);
    let id = f.create("big.bin", "application/octet-stream", &data).await;
    f.upload(id, &data).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handle = tokio::runtime::Handle::current();
    let (pool, sink) = (f.pool.clone(), seen.clone());
    f.trace.hook(Some(Arc::new(move |step| {
        if step != PublishStep::Verified {
            return;
        }
        let observed = handle.block_on(async {
            let open: i64 = sqlx::query_scalar(
                "select count(*) from pg_stat_activity where datname = current_database()
                 and state like 'idle in transaction%' and pid <> pg_backend_pid()",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            let mut tx = pool.begin().await.unwrap();
            let free = sqlx::query("select id from attachments where id=$1 for update nowait")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .is_ok();
            tx.rollback().await.unwrap();
            (open, free)
        });
        sink.lock().unwrap().push(observed);
    })));
    assert_eq!(f.complete(id, "outside", None).await.status, 200);
    assert_eq!(*seen.lock().unwrap(), vec![(0, true)]);

    // Запись, отменённая во время публикации, не становится готовой: commit только из `uploading`.
    let other = f
        .create("cancelled.bin", "application/octet-stream", &data[..1000])
        .await;
    f.upload(other, &data[..1000]).await;
    let handle = tokio::runtime::Handle::current();
    let pool = f.pool.clone();
    f.trace.hook(Some(Arc::new(move |step| {
        if step == PublishStep::DirSynced {
            handle
                .block_on(
                    sqlx::query("update attachments set state='cancelled' where id=$1")
                        .bind(other)
                        .execute(&pool),
                )
                .unwrap();
        }
    })));
    let refused = f.complete(other, "late", None).await;
    f.trace.hook(None);
    assert_error(&refused, 409, "attachment_not_uploadable", "never");
    assert_eq!(f.state_of(other).await, "cancelled");
    assert!(
        f.blob_path(other).exists(),
        "published content stays an orphan"
    );
    let ready = f
        .events("attachment_state_changed")
        .await
        .into_iter()
        .filter(|p| p["attachment_id"] == other.to_string() && p["state"] == "ready")
        .count();
    assert_eq!(ready, 0);
    let report = attachments::gc::run_once(&f.pool, f.blobs(), Utc::now(), Duration::ZERO)
        .await
        .unwrap();
    assert!(report.orphan_blobs >= 1);
    assert!(!f.blob_path(other).exists());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn crash_after_publish_before_commit_leaves_orphan_and_retry_completes(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let data = content(300_000, 9);
    let id = f
        .create("crash.bin", "application/octet-stream", &data)
        .await;
    f.upload(id, &data).await;
    f.trace.crash_after(Some(PublishStep::DirSynced));
    let crashed = f.complete(id, "finish", None).await;
    assert_error(&crashed, 500, "internal", "safe_same_key");
    assert_eq!(f.state_of(id).await, "uploading");
    assert_eq!(std::fs::read(f.blob_path(id)).unwrap(), data);
    assert!(!f.staging_path(id).exists());
    assert_eq!(
        f.count("select count(*) from idempotency_keys where key='finish'")
            .await,
        0
    );
    let ready = |f: &Fixture| {
        let pool = f.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("select count(*) from events where kind='attachment_state_changed' and payload->>'state'='ready'")
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(ready(&f).await, 0);
    // Сборщик с обычной отсрочкой не трогает содержимое активной загрузки.
    let report =
        attachments::gc::run_once(&f.pool, f.blobs(), Utc::now(), Duration::from_secs(86400))
            .await
            .unwrap();
    assert_eq!(report, attachments::gc::GcReport::default());
    let head = f.head(id).await;
    assert_eq!(upload_offset(&head), Some(data.len() as u64));
    let inode = std::fs::metadata(f.blob_path(id)).unwrap().ino();

    f.trace.crash_after(None);
    f.trace.clear();
    let retried = f.complete(id, "finish", None).await;
    assert_eq!(retried.status, 200);
    assert_eq!(retried.json::<Value>()["state"], "ready");
    assert_eq!(
        f.trace.steps(),
        vec![PublishStep::Verified, PublishStep::DirSynced],
        "the retry does not copy the content again"
    );
    assert_eq!(std::fs::metadata(f.blob_path(id)).unwrap().ino(), inode);
    assert_eq!(f.state_of(id).await, "ready");
    assert_eq!(ready(&f).await, 1);

    // Без повтора опубликованное содержимое остаётся orphan и уходит сборщику, а не в ссылку.
    let lost = f
        .create("lost.bin", "application/octet-stream", &data)
        .await;
    f.upload(lost, &data).await;
    f.trace.crash_after(Some(PublishStep::DirSynced));
    assert_eq!(f.complete(lost, "lost", None).await.status, 500);
    f.trace.crash_after(None);
    f.age(lost, 25).await;
    assert_eq!(
        f.submit("link_lost", "файл", &[lost]).await.json::<Value>()["error"]["code"],
        "attachment_not_ready"
    );
    let report = attachments::gc::run_once(
        &f.pool,
        f.blobs(),
        Utc::now() + TimeDelta::seconds(1),
        Duration::from_secs(86400),
    )
    .await
    .unwrap();
    assert_eq!(report.expired_uploads, 1);
    assert_eq!(f.state_of(lost).await, "failed");
    assert_eq!(f.submit("keep", "файл", &[id]).await.status, 201);
    let swept = attachments::gc::run_once(&f.pool, f.blobs(), Utc::now(), Duration::ZERO)
        .await
        .unwrap();
    assert!(swept.orphan_blobs >= 1);
    assert!(!f.blob_path(lost).exists());
    assert!(f.blob_path(id).exists());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ready_row_is_never_visible_before_the_blob_is_durable(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let violations = Arc::new(Mutex::new(Vec::<String>::new()));
    for round in 0..8u8 {
        let data = content(64 * 1024 + round as usize, round);
        let id = f
            .create("watch.bin", "application/octet-stream", &data)
            .await;
        f.upload(id, &data).await;
        let handle = tokio::runtime::Handle::current();
        let (pool, sink, steps_seen) = (
            f.pool.clone(),
            violations.clone(),
            Arc::new(Mutex::new(Vec::new())),
        );
        let hook_steps = steps_seen.clone();
        f.trace.hook(Some(Arc::new(move |step| {
            let state: String = handle
                .block_on(
                    sqlx::query_scalar("select state from attachments where id=$1")
                        .bind(id)
                        .fetch_one(&pool),
                )
                .unwrap();
            if state == "ready" {
                sink.lock().unwrap().push(format!("ready at {step:?}"));
            }
            hook_steps.lock().unwrap().push(step);
        })));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher = {
            let (pool, stop, sink, blob, size) = (
                f.pool.clone(),
                stop.clone(),
                violations.clone(),
                f.blob_path(id),
                data.len() as u64,
            );
            tokio::spawn(async move {
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    let state: String =
                        sqlx::query_scalar("select state from attachments where id=$1")
                            .bind(id)
                            .fetch_one(&pool)
                            .await
                            .unwrap();
                    if state == "ready"
                        && std::fs::metadata(&blob).map(|m| m.len()).ok() != Some(size)
                    {
                        sink.lock().unwrap().push("ready without the blob".into());
                    }
                    tokio::task::yield_now().await;
                }
            })
        };
        let done = f.complete(id, &format!("watch_{round}"), None).await;
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        watcher.await.unwrap();
        f.trace.hook(None);
        assert_eq!(done.status, 200);
        assert_eq!(steps_seen.lock().unwrap().len(), 4);
        assert_eq!(f.state_of(id).await, "ready");
        assert_eq!(std::fs::read(f.blob_path(id)).unwrap(), data);
    }
    assert!(
        violations.lock().unwrap().is_empty(),
        "{:?}",
        violations.lock().unwrap()
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn max_size_upload_succeeds_and_one_byte_more_is_rejected(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let size = MAX_ATTACHMENT_BYTES;
    let sha = generated_sha(size);
    let request = |size: u64| json!({"conversation_id": f.conversation, "name": "max.bin", "size": size, "sha256": sha});
    let over = f
        .client
        .send(
            Method::POST,
            "/api/v1/attachments",
            Some("max_over"),
            Some(request(size + 1)),
        )
        .await;
    assert_error(&over, 413, "resource_limit", "never");
    let created = f
        .client
        .send(
            Method::POST,
            "/api/v1/attachments",
            Some("max"),
            Some(request(size)),
        )
        .await;
    assert_eq!(created.status, 201);
    let id = id_of(&created);
    upload_generated(&f, id, size, None).await;
    let extra = f.patch_bytes(id, size, &[0u8]).await;
    assert_error(&extra, 413, "resource_limit", "never");
    assert_eq!(upload_offset(&extra), Some(size));
    let done = f.complete(id, "max_done", None).await;
    assert_eq!(done.status, 200, "{:?}", done.body);
    assert_eq!(done.json::<Value>()["size"], size);
    assert_eq!(std::fs::metadata(f.blob_path(id)).unwrap().len(), size);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn upload_memory_stays_bounded(pool: PgPool) {
    let _exclusive = HEAVY.write().await;
    let f = Fixture::new(pool).await;
    let size = MAX_ATTACHMENT_BYTES;
    let sha = generated_sha(size);
    let created = f
        .client
        .send(
            Method::POST,
            "/api/v1/attachments",
            Some("memory"),
            Some(json!({"conversation_id": f.conversation, "name": "memory.bin", "size": size, "sha256": sha})),
        )
        .await;
    let id = id_of(&created);
    let baseline = rss_bytes();
    let peak = Arc::new(std::sync::atomic::AtomicU64::new(baseline));
    let probe_peak = peak.clone();
    let probe: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        probe_peak.fetch_max(rss_bytes(), std::sync::atomic::Ordering::SeqCst);
    });
    upload_generated(&f, id, size, Some(probe)).await;
    peak.fetch_max(rss_bytes(), std::sync::atomic::Ordering::SeqCst);
    assert_eq!(f.complete(id, "memory_done", None).await.status, 200);
    peak.fetch_max(rss_bytes(), std::sync::atomic::Ordering::SeqCst);
    let growth = peak.load(std::sync::atomic::Ordering::SeqCst) - baseline;
    eprintln!("rss growth {} KiB", growth / 1024);
    assert!(
        growth < 32 * MIB as u64,
        "RSS grew by {} MiB while 100 MiB streamed",
        growth / MIB as u64
    );
}

/// Тело, которое отдаёт половину и ждёт разрешения перед второй половиной.
fn held_body(bytes: Vec<u8>, release: tokio::sync::watch::Receiver<bool>) -> Body {
    let half = bytes.len() / 2;
    let first = Bytes::copy_from_slice(&bytes[..half]);
    let second = Bytes::copy_from_slice(&bytes[half..]);
    let frames = stream::once(async move { Ok::<_, std::io::Error>(first) }).chain(stream::once(
        async move {
            let mut release = release;
            let _ = release.wait_for(|go| *go).await;
            Ok(second)
        },
    ));
    Body::from_stream(frames)
}

async fn spawn_patch(
    client: TestClient,
    id: Uuid,
    offset: u64,
    body: Body,
) -> tokio::task::JoinHandle<testkit::TestResponse> {
    tokio::spawn(async move {
        client
            .send_body(
                Method::PATCH,
                &format!("/api/v1/attachments/{id}/content"),
                &[("upload-offset", offset.to_string())],
                body,
            )
            .await
    })
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_uploads_are_limited_with_429(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::with(pool, |c| c.max_concurrent_uploads = 2).await;
    let data = content(128 * 1024, 10);
    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(
            f.create("slot.bin", "application/octet-stream", &data)
                .await,
        );
    }
    let (release, wait) = tokio::sync::watch::channel(false);
    let first = spawn_patch(
        f.client.clone(),
        ids[0],
        0,
        held_body(data.clone(), wait.clone()),
    )
    .await;
    let second = spawn_patch(f.client.clone(), ids[1], 0, held_body(data.clone(), wait)).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while f.state.upload_slots.available_permits() > 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let third = f.patch_bytes(ids[2], 0, &data).await;
    assert_error(&third, 429, "too_many_requests", "safe_same_key");
    assert!(!f.staging_path(ids[2]).exists());
    release.send(true).unwrap();
    for task in [first, second] {
        let response = task.await.unwrap();
        assert_eq!(response.status, 204);
        assert_eq!(upload_offset(&response), Some(data.len() as u64));
    }
    assert_eq!(f.patch_bytes(ids[2], 0, &data).await.status, 204);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn upload_accepts_8_mib_chunks_and_config_announces_upload_chunk_bytes(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let config = f
        .client
        .send(Method::GET, "/api/v1/config", None, None)
        .await
        .json::<Value>();
    assert_eq!(config["limits"]["upload_chunk_bytes"], 8 * MIB);
    assert_eq!(
        config["limits"]["max_attachment_bytes"],
        MAX_ATTACHMENT_BYTES
    );
    assert_eq!(config["limits"]["max_attachments_per_input"], 10);
    let data = content(8 * MIB + 10, 11);
    let id = f
        .create("chunk.bin", "application/octet-stream", &data)
        .await;
    let whole = f.patch_bytes(id, 0, &data[..8 * MIB]).await;
    assert_eq!(whole.status, 204);
    assert_eq!(upload_offset(&whole), Some(8 * MIB as u64));
    assert_eq!(
        f.patch_bytes(id, 8 * MIB as u64, &data[8 * MIB..])
            .await
            .status,
        204
    );
    assert_eq!(f.complete(id, "chunk_done", None).await.status, 200);

    let big = content(16 * MIB, 12);
    let over = f.create("over.bin", "application/octet-stream", &big).await;
    let refused = f.patch_bytes(over, 0, &big[..8 * MIB + 1]).await;
    assert_error(&refused, 413, "resource_limit", "never");
    assert_eq!(upload_offset(&refused), Some(8 * MIB as u64));
    assert_eq!(upload_offset(&f.head(over).await), Some(8 * MIB as u64));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn upload_body_over_a_socket_has_no_short_request_timeout(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let mut http = testkit::test_http_config();
    http.body_read_timeout = Duration::from_millis(300);
    let hub = testkit::spawn_test_hub_http(pool.clone(), Default::default(), http).await;
    let owner = hub.installation.owner_id;
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
        .bind(machine)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    let conversation = testkit::seed_conversation(&pool, owner, machine).await;
    let client = TestClient::over_socket(hub.http_addr()).await.as_owner();
    let data = content(10 * 1024, 13);
    let created = client
        .send(
            Method::POST,
            "/api/v1/attachments",
            Some("socket"),
            Some(json!({"conversation_id": conversation, "name": "slow.bin", "size": data.len(), "sha256": sha(&data)})),
        )
        .await;
    assert_eq!(created.status, 201);
    let id = id_of(&created);
    let pieces: Vec<Bytes> = data.chunks(1024).map(Bytes::copy_from_slice).collect();
    // Десять кадров с паузой 100 мс: тело дольше `body_read_timeout`, но без простоя.
    let slow = stream::iter(pieces).then(|piece| async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Ok::<_, std::io::Error>(piece)
    });
    let response = client
        .send_body(
            Method::PATCH,
            &format!("/api/v1/attachments/{id}/content"),
            &[("upload-offset", "0".into())],
            Body::from_stream(slow),
        )
        .await;
    assert_eq!(response.status, 204);
    assert_eq!(upload_offset(&response), Some(data.len() as u64));
    let _ = hub.shutdown.send(());
    hub.join.await.unwrap().unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn concurrent_patches_of_one_attachment_never_interleave(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let data = content(256 * 1024, 14);
    let id = f
        .create("race.bin", "application/octet-stream", &data)
        .await;
    let (release, wait) = tokio::sync::watch::channel(false);
    let slow = spawn_patch(
        f.client.clone(),
        id,
        0,
        held_body(data[..128 * 1024].to_vec(), wait),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while std::fs::metadata(f.staging_path(id))
            .map(|m| m.len())
            .unwrap_or(0)
            < 64 * 1024
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    // Клиент не дождался ответа и шлёт тот же кусок заново: второе тело не пишется.
    let resent = f.patch_bytes(id, 0, &data[..128 * 1024]).await;
    assert_error(&resent, 409, "upload_in_progress", "after_condition");
    assert_eq!(upload_offset(&resent), Some(64 * 1024));
    let head = f.head(id).await;
    assert_eq!(head.status, 409);
    assert_eq!(upload_offset(&head), Some(64 * 1024));
    release.send(true).unwrap();
    let first = slow.await.unwrap();
    assert_eq!(first.status, 204);
    assert_eq!(upload_offset(&first), Some(128 * 1024));
    let rest = f.patch_bytes(id, 128 * 1024, &data[128 * 1024..]).await;
    assert_eq!(rest.status, 204);
    let done = f.complete(id, "race_done", None).await;
    assert_eq!(
        done.status, 200,
        "the content matches the declared checksum"
    );
    assert_eq!(std::fs::read(f.blob_path(id)).unwrap(), data);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn chunks_after_failed_or_cancelled_are_refused(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let data = content(4096, 15);
    let cancelled = f
        .create("cancel.bin", "application/octet-stream", &data)
        .await;
    assert_eq!(f.patch_bytes(cancelled, 0, &data[..1000]).await.status, 204);
    assert_eq!(f.cancel(cancelled, "cancel").await.status, 204);
    assert_eq!(f.cancel(cancelled, "cancel").await.status, 204);
    assert_eq!(f.state_of(cancelled).await, "cancelled");
    assert!(!f.staging_path(cancelled).exists());
    let late = f.patch_bytes(cancelled, 1000, &data[1000..]).await;
    assert_error(&late, 409, "attachment_not_uploadable", "never");
    assert_eq!(f.head(cancelled).await.status, 409);
    assert_error(
        &f.complete(cancelled, "cancel_done", None).await,
        409,
        "attachment_not_uploadable",
        "never",
    );
    assert!(!f.staging_path(cancelled).exists());

    let failed = f
        .create("fail.bin", "application/octet-stream", &data)
        .await;
    f.upload(failed, &content(4096, 16)).await;
    assert_eq!(f.complete(failed, "fail_done", None).await.status, 422);
    assert_error(
        &f.patch_bytes(failed, 0, &data).await,
        409,
        "attachment_not_uploadable",
        "never",
    );

    let expired = f
        .create("expired.bin", "application/octet-stream", &data)
        .await;
    assert_eq!(f.patch_bytes(expired, 0, &data[..100]).await.status, 204);
    f.age(expired, 25).await;
    attachments::gc::run_once(&f.pool, f.blobs(), Utc::now(), Duration::from_secs(86400))
        .await
        .unwrap();
    assert_eq!(f.state_of(expired).await, "failed");
    assert_error(
        &f.patch_bytes(expired, 100, &data[100..]).await,
        409,
        "attachment_not_uploadable",
        "never",
    );

    let ready = f
        .ready("ready.bin", "application/octet-stream", &data)
        .await;
    assert_error(
        &f.patch_bytes(ready, data.len() as u64, &[1]).await,
        409,
        "attachment_not_uploadable",
        "never",
    );
}

async fn inputs_of(f: &Fixture, conversation: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from inputs where conversation_id=$1")
        .bind(conversation)
        .fetch_one(&f.pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn submit_with_unready_attachment_is_refused_and_creates_no_input(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let data = content(2048, 17);
    let id = f
        .create("late.bin", "application/octet-stream", &data)
        .await;
    for round in 0..2usize {
        let refused = f.submit("unready", "смотри файл", &[id]).await;
        assert_error(&refused, 409, "attachment_not_ready", "after_condition");
        assert_eq!(inputs_of(&f, f.conversation).await, 0);
        assert_eq!(
            f.count("select count(*) from commands where kind='deliver_input'")
                .await,
            0
        );
        assert_eq!(f.count("select count(*) from input_attachments").await, 0);
        assert!(f.events("input_queued").await.is_empty());
        assert_eq!(
            f.count("select count(*) from idempotency_keys where key='unready'")
                .await,
            0
        );
        let part = &data[round * 1024..(round + 1) * 1024];
        assert_eq!(
            f.patch_bytes(id, (round * 1024) as u64, part).await.status,
            204
        );
    }
    assert_eq!(f.complete(id, "late_done", None).await.status, 200);
    let accepted = f.submit("unready", "смотри файл", &[id]).await;
    assert_eq!(accepted.status, 201);
    let input: Uuid = accepted.json::<Value>()["input_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let content: Value = sqlx::query_scalar("select content from inputs where id=$1")
        .bind(input)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        content,
        json!([
            {"type":"text","text":"смотри файл"},
            {"type":"attachment","attachment_id":id,"name":"late.bin","mime":"application/octet-stream","size":2048,"sha256":sha(&data),"image":false}
        ])
    );
    let link: (i32, String) = sqlx::query_as(
        "select position, machine_state from input_attachments where input_id=$1 and attachment_id=$2",
    )
    .bind(input)
    .bind(id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(link, (0, "pending".into()));
    let payload: Value = sqlx::query_scalar(
        "select c.payload from commands c join inputs i on i.command_id=c.id where i.id=$1",
    )
    .bind(input)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(payload["attachment_ids"], json!([id]));
    assert_eq!(payload["content"], content);
    assert_eq!(f.events("input_queued").await[0]["content"], content);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn foreign_owner_or_conversation_attachment_is_rejected(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let data = content(512, 18);
    let other_conversation = f.conversation().await;
    let elsewhere = f
        .ready_in(
            other_conversation,
            "else.bin",
            "application/octet-stream",
            &data,
        )
        .await;
    assert_error(
        &f.submit("elsewhere", "t", &[elsewhere]).await,
        409,
        "attachment_not_ready",
        "after_condition",
    );
    assert_eq!(
        f.submit_to(other_conversation, "own", "t", &[elsewhere])
            .await
            .status,
        201
    );
    // Вложение другого владельца ведёт себя как отсутствующее.
    let stranger = Uuid::now_v7();
    let stranger_machine = Uuid::now_v7();
    sqlx::query("insert into owners(id) values($1)")
        .bind(stranger)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'other')")
        .bind(stranger_machine)
        .bind(stranger)
        .execute(&f.pool)
        .await
        .unwrap();
    let stranger_conversation =
        testkit::seed_conversation(&f.pool, stranger, stranger_machine).await;
    let foreign = Uuid::now_v7();
    sqlx::query(
        "insert into attachments(id,owner_id,conversation_id,name,size_bytes,sha256,state)
         values($1,$2,$3,'secret.txt',512,$4,'ready')",
    )
    .bind(foreign)
    .bind(stranger)
    .bind(stranger_conversation)
    .bind(sha(&data))
    .execute(&f.pool)
    .await
    .unwrap();
    for id in [foreign, Uuid::now_v7()] {
        assert_error(
            &f.submit(&key(), "t", &[id]).await,
            409,
            "attachment_not_ready",
            "after_condition",
        );
        for response in [
            f.client
                .send(
                    Method::GET,
                    &format!("/api/v1/attachments/{id}"),
                    None,
                    None,
                )
                .await,
            f.client
                .send(
                    Method::GET,
                    &format!("/api/v1/attachments/{id}/content"),
                    None,
                    None,
                )
                .await,
            f.patch_bytes(id, 0, &data).await,
            f.complete(id, &key(), None).await,
            f.cancel(id, &key()).await,
        ] {
            assert_error(&response, 404, "not_found", "never");
        }
        assert_eq!(f.head(id).await.status, 404);
    }
    assert_eq!(inputs_of(&f, f.conversation).await, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ten_attachments_per_input_and_the_eleventh_is_rejected(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let mut ids = Vec::new();
    for i in 0..11u8 {
        ids.push(
            f.ready(&format!("f{i}.txt"), "text/plain", &content(100, i))
                .await,
        );
    }
    assert_error(
        &f.submit("eleven", "t", &ids).await,
        413,
        "resource_limit",
        "never",
    );
    assert_eq!(inputs_of(&f, f.conversation).await, 0);
    assert_error(
        &f.submit("duplicate", "t", &[ids[0], ids[0]]).await,
        400,
        "invalid_request",
        "never",
    );
    let mut ten: Vec<Uuid> = ids[..10].to_vec();
    ten.reverse();
    let accepted = f.submit("ten", "t", &ten).await;
    assert_eq!(accepted.status, 201);
    let input = accepted.json::<Value>()["input_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let linked: Vec<Uuid> = sqlx::query_scalar(
        "select attachment_id from input_attachments where input_id=$1::uuid order by position",
    )
    .bind(&input)
    .fetch_all(&f.pool)
    .await
    .unwrap();
    assert_eq!(linked, ten);
    let content: Value = sqlx::query_scalar("select content from inputs where id=$1::uuid")
        .bind(&input)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    let order: Vec<Uuid> = content.as_array().unwrap()[1..]
        .iter()
        .map(|b| b["attachment_id"].as_str().unwrap().parse().unwrap())
        .collect();
    assert_eq!(order, ten);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn image_attachment_is_refused_for_a_harness_without_images(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let png = f
        .ready("pic.png", "image/png", b"\x89PNG\r\n\x1a\nrest")
        .await;
    let svg = f.ready("pic.svg", "image/svg+xml", b"<svg/>").await;
    let text = f.ready("notes.txt", "text/plain", b"notes").await;
    sqlx::query("update machine_harnesses set images=false where machine_id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    for id in [png, svg] {
        assert_error(
            &f.submit(&key(), "t", &[text, id]).await,
            422,
            "attachment_unsupported",
            "never",
        );
    }
    assert_eq!(inputs_of(&f, f.conversation).await, 0);
    assert_eq!(f.submit("text_only", "t", &[text]).await.status, 201);
    sqlx::query("update machine_harnesses set images=true where machine_id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    let accepted = f.submit("images", "t", &[png, svg]).await;
    assert_eq!(accepted.status, 201);
    let content: Value = sqlx::query_scalar("select content from inputs where id=$1::uuid")
        .bind(accepted.json::<Value>()["input_id"].as_str().unwrap())
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(content[1]["image"], true);
    assert_eq!(content[2]["image"], false, "svg is passed as a plain file");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn submit_with_attachments_replays_identically(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let a = f.ready("a.txt", "text/plain", b"a").await;
    let b = f.ready("b.txt", "text/plain", b"b").await;
    let first = f.submit("replay", "два файла", &[a, b]).await;
    assert_eq!(first.status, 201);
    let replay = f.submit("replay", "два файла", &[a, b]).await;
    assert_eq!(replay.status, first.status);
    assert_eq!(replay.body, first.body);
    assert_eq!(inputs_of(&f, f.conversation).await, 1);
    assert_eq!(f.count("select count(*) from input_attachments").await, 2);
    assert_eq!(
        f.count("select count(*) from idempotency_keys where key='replay'")
            .await,
        1
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn same_key_with_another_attachment_set_conflicts(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let a = f.ready("a.txt", "text/plain", b"a").await;
    let b = f.ready("b.txt", "text/plain", b"b").await;
    assert_eq!(f.submit("set", "t", &[a]).await.status, 201);
    for other in [vec![b], vec![a, b], vec![]] {
        assert_error(
            &f.submit("set", "t", &other).await,
            409,
            "idempotency_conflict",
            "never",
        );
    }
    assert_eq!(f.submit("ordered", "t", &[a, b]).await.status, 201);
    assert_error(
        &f.submit("ordered", "t", &[b, a]).await,
        409,
        "idempotency_conflict",
        "never",
    );
    assert_eq!(inputs_of(&f, f.conversation).await, 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn machine_without_pilot_capability_or_with_limits_mismatch_refuses_attachments(
    pool: PgPool,
) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let a = f.ready("a.txt", "text/plain", b"a").await;
    let schedule = |key: &str| {
        let client = f.client.clone();
        let path = format!("/api/v1/conversations/{}/scheduled", f.conversation);
        let key = key.to_owned();
        let run_at = (Utc::now() + TimeDelta::minutes(5)).to_rfc3339();
        async move {
            client
                .send(
                    Method::POST,
                    &path,
                    Some(&key),
                    Some(json!({"run_at": run_at, "content":[{"type":"text","text":"later"}], "attachment_ids":[a]})),
                )
                .await
        }
    };
    for (caps, limits_ok, code, retry) in [
        ("{}", true, "capability_missing", "after_condition"),
        ("{pilot_v1}", false, "limits_mismatch", "never"),
    ] {
        sqlx::query("update machines set capabilities=$2::text[], limits_ok=$3 where id=$1")
            .bind(f.machine)
            .bind(caps)
            .bind(limits_ok)
            .execute(&f.pool)
            .await
            .unwrap();
        assert_error(&f.submit(&key(), "t", &[a]).await, 409, code, retry);
        assert_error(&schedule(&key()).await, 409, code, retry);
        assert_eq!(f.count("select count(*) from input_attachments").await, 0);
        assert_eq!(f.count("select count(*) from scheduled_jobs").await, 0);
        // Ввод без вложений от этого не зависит.
        assert_eq!(f.submit(&key(), "plain", &[]).await.status, 201);
    }
    sqlx::query("update machines set capabilities='{pilot_v1}', limits_ok=true where id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(f.submit("restored", "t", &[a]).await.status, 201);
    assert_eq!(schedule("restored_schedule").await.status, 201);
}

async fn running_execution(f: &Fixture, conversation: Uuid) {
    let execution = executions::start(&f.pool, f.owner, conversation, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&f.pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
}

async fn command_row(f: &Fixture, input: Uuid) -> outbox::CommandRow {
    sqlx::query_as(
        "select c.id, c.machine_id, c.conversation_id, c.execution_id, c.execution_generation, c.kind, c.payload, c.dispatch_attempt
         from commands c join inputs i on i.command_id = c.id where i.id = $1",
    )
    .bind(input)
    .fetch_one(&f.pool)
    .await
    .unwrap()
}

fn gate(f: &Fixture) -> GateInputs {
    GateInputs {
        machine_available: true,
        machine_reconciled: true,
        session_capabilities: f.state.registry.capabilities(f.machine),
    }
}

fn input_id(response: &testkit::TestResponse) -> Uuid {
    response.json::<Value>()["input_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn inputs_with_attachments_or_settings_wait_for_a_pilot_session(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let data = b"\x89PNG\r\n\x1a\nimage";
    let a = f.ready("shot.png", "image/png", data).await;
    running_execution(&f, f.conversation).await;
    let input = input_id(&f.submit("with_file", "посмотри", &[a]).await);

    let legacy = testkit::fake_session(&f.state.registry, f.machine, &[], true);
    let outcome = dispatch::dispatch_next(&f.pool, f.conversation, gate(&f), Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(
        outcome,
        DispatchOutcome::Blocked(vec![BlockReason::CapabilityMissing { input_id: input }])
    );
    let state: String = sqlx::query_scalar("select state from inputs where id=$1")
        .bind(input)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(state, "queued");
    let stored: Value =
        sqlx::query_scalar("select hold_reasons from queue_state where conversation_id=$1")
            .bind(f.conversation)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert!(!stored.to_string().contains("capability_missing"));
    let detail = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{}", f.conversation),
            None,
            None,
        )
        .await
        .json::<Value>();
    assert!(
        detail["holds"]
            .as_array()
            .unwrap()
            .contains(&json!({"reason":"capability_missing","input_id":input}))
    );
    assert!(matches!(
        outbox::to_proto_for_session(&command_row(&f, input).await, &f.state.registry),
        Err(outbox::OutboxError::CapabilityMissing)
    ));
    drop(legacy);

    let pilot = testkit::fake_session(&f.state.registry, f.machine, &["pilot_v1"], true);
    let outcome = dispatch::dispatch_next(&f.pool, f.conversation, gate(&f), Uuid::now_v7())
        .await
        .unwrap();
    assert!(
        matches!(outcome, DispatchOutcome::Dispatched { input_id, .. } if input_id == input),
        "{outcome:?}"
    );
    let command =
        outbox::to_proto_for_session(&command_row(&f, input).await, &f.state.registry).unwrap();
    let Some(command::Kind::DeliverInput(deliver)) = command.kind else {
        panic!("deliver_input expected");
    };
    assert!(
        matches!(&deliver.content[0].block, Some(content_block::Block::Text(t)) if t.text == "посмотри")
    );
    let Some(content_block::Block::Attachment(reference)) = &deliver.content[1].block else {
        panic!("attachment ref expected");
    };
    assert_eq!(reference.attachment_id, a.to_string());
    assert_eq!(reference.name, "shot.png");
    assert_eq!(reference.mime, "image/png");
    assert_eq!(reference.size, data.len() as u64);
    assert_eq!(reference.sha256, sha(data));
    assert!(reference.image);
    drop(pilot);

    // Настройки следующего хода ждут сеанса с `pilot_v1` так же, как вложения.
    let other = f.conversation().await;
    sqlx::query("update conversations set model='synthetic-small',effort='high' where id=$1")
        .bind(other)
        .execute(&f.pool)
        .await
        .unwrap();
    running_execution(&f, other).await;
    conversations::change_settings(
        &f.pool,
        f.owner,
        other,
        "settings",
        conversations::SettingsPatch {
            effort: Some("low".into()),
            ..Default::default()
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let text = input_id(&f.submit_to(other, "settings_input", "t", &[]).await);
    let legacy = testkit::fake_session(&f.state.registry, f.machine, &[], true);
    assert_eq!(
        dispatch::dispatch_next(&f.pool, other, gate(&f), Uuid::now_v7())
            .await
            .unwrap(),
        DispatchOutcome::Blocked(vec![BlockReason::CapabilityMissing { input_id: text }])
    );
    drop(legacy);
    let _pilot = testkit::fake_session(&f.state.registry, f.machine, &["pilot_v1"], true);
    assert!(matches!(
        dispatch::dispatch_next(&f.pool, other, gate(&f), Uuid::now_v7())
            .await
            .unwrap(),
        DispatchOutcome::Dispatched { input_id, .. } if input_id == text
    ));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn attachments_can_be_queued_while_the_machine_is_offline(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    assert!(f.state.registry.session_info(f.machine).is_none());
    let a = f.ready("offline.txt", "text/plain", b"offline").await;
    let accepted = f.submit("offline", "t", &[a]).await;
    assert_eq!(accepted.status, 201);
    let input = input_id(&accepted);
    let (state, delivery): (String, String) = sqlx::query_as(
        "select i.state, c.delivery_state from inputs i join commands c on c.id=i.command_id where i.id=$1",
    )
    .bind(input)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!((state.as_str(), delivery.as_str()), ("queued", "held"));
    assert_eq!(f.count("select count(*) from input_attachments").await, 1);
    let b = f.ready("later.txt", "text/plain", b"later").await;
    let scheduled = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/scheduled", f.conversation),
            Some("offline_schedule"),
            Some(json!({"run_at": (Utc::now() + TimeDelta::minutes(5)).to_rfc3339(), "content":[], "attachment_ids":[b]})),
        )
        .await;
    assert_eq!(scheduled.status, 201, "{:?}", scheduled.body);
    assert_eq!(scheduled.json::<Value>()["attachment_ids"], json!([b]));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dispatched_input_waiting_for_files_shows_delivering_attachments_end_to_end(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    let a = f.ready("report.pdf", "application/pdf", b"%PDF-1.7").await;
    running_execution(&f, f.conversation).await;
    let input = input_id(&f.submit("deliver", "прочитай", &[a]).await);
    let _pilot = testkit::fake_session(&f.state.registry, f.machine, &["pilot_v1"], true);
    let outcome = dispatch::dispatch_next(&f.pool, f.conversation, gate(&f), Uuid::now_v7())
        .await
        .unwrap();
    assert!(
        matches!(outcome, DispatchOutcome::Dispatched { .. }),
        "{outcome:?}"
    );
    let thresholds = activity::Thresholds::from(&ActivityConfig::default());
    let detail = |f: &Fixture| {
        let client = f.client.clone();
        let path = format!("/api/v1/conversations/{}", f.conversation);
        async move {
            client
                .send(Method::GET, &path, None, None)
                .await
                .json::<Value>()
        }
    };
    activity::refresh(
        &f.pool,
        f.conversation,
        Utc::now(),
        &thresholds,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let waiting = detail(&f).await;
    assert_eq!(waiting["activity"]["state"], "working");
    assert_eq!(
        waiting["activity"]["evidence"]["phase"],
        "delivering_attachments"
    );
    assert!(
        waiting["holds"]
            .as_array()
            .unwrap()
            .contains(&json!({"reason":"attachments_pending","input_id":input})),
        "{}",
        waiting["holds"]
    );
    sqlx::query("update input_attachments set machine_reason='hub_unreachable', retryable=true where input_id=$1")
        .bind(input)
        .execute(&f.pool)
        .await
        .unwrap();
    let retrying = detail(&f).await;
    assert!(retrying["holds"].as_array().unwrap().contains(
        &json!({"reason":"attachments_pending","input_id":input,"machine_reason":"hub_unreachable"})
    ));
    assert!(
        !sqlx::query_scalar::<_, Value>(
            "select hold_reasons from queue_state where conversation_id=$1"
        )
        .bind(f.conversation)
        .fetch_one(&f.pool)
        .await
        .unwrap()
        .to_string()
        .contains("attachments_pending"),
        "the hold is derived, never stored"
    );
    sqlx::query(
        "update input_attachments set machine_state='ready', machine_reason=null where input_id=$1",
    )
    .bind(input)
    .execute(&f.pool)
    .await
    .unwrap();
    activity::refresh(
        &f.pool,
        f.conversation,
        Utc::now(),
        &thresholds,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let ready = detail(&f).await;
    assert_ne!(
        ready["activity"]["evidence"]["phase"],
        "delivering_attachments"
    );
    assert!(!ready["holds"].to_string().contains("attachments_pending"));
}

async fn download(f: &Fixture, client: &TestClient, id: Uuid) -> testkit::TestResponse {
    let _ = f;
    client
        .send(
            Method::GET,
            &format!("/api/v1/attachments/{id}/content"),
            None,
            None,
        )
        .await
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn download_requires_owner_and_sets_safe_headers(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
    let id = f.ready("отчёт 1.png", "image/png", &png).await;
    let response = download(&f, &f.client, id).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body.as_ref(), png.as_slice());
    let h = &response.headers;
    assert_eq!(h["content-type"], "image/png");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(
        h["content-disposition"],
        "attachment; filename*=UTF-8''%D0%BE%D1%82%D1%87%D1%91%D1%82%201.png"
    );
    assert_eq!(h["content-security-policy"], "sandbox");
    assert_eq!(h["cache-control"], "private, no-store");
    assert_eq!(h["content-length"], png.len().to_string());

    let stranger = TestClient::in_process(canterlot_hub::http::router(f.state.clone()))
        .with_header("Remote-User", "attacker");
    assert_error(
        &download(&f, &stranger, id).await,
        403,
        "forbidden_user",
        "never",
    );
    let anonymous = TestClient::in_process(canterlot_hub::http::router(f.state.clone()));
    assert_error(
        &download(&f, &anonymous, id).await,
        401,
        "unauthenticated",
        "after_condition",
    );
    let untrusted = TestClient::in_process(canterlot_hub::http::router(f.state.clone()))
        .with_peer("203.0.113.5:4000".parse().unwrap())
        .as_owner();
    assert_eq!(download(&f, &untrusted, id).await.status, 401);
    assert_error(
        &download(&f, &f.client, Uuid::now_v7()).await,
        404,
        "not_found",
        "never",
    );

    let forged = f
        .ready("fake.png", "image/png", b"<html>not a png</html>")
        .await;
    assert_eq!(
        download(&f, &f.client, forged).await.headers["content-type"],
        "application/octet-stream"
    );
    let pending = f
        .create("pending.bin", "application/octet-stream", b"12345")
        .await;
    assert_error(
        &download(&f, &f.client, pending).await,
        409,
        "attachment_not_ready",
        "after_condition",
    );
    assert_eq!(f.cancel(pending, "drop_pending").await.status, 204);
    assert_error(
        &download(&f, &f.client, pending).await,
        409,
        "attachment_unavailable",
        "never",
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn html_named_file_is_served_as_octet_stream_attachment(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let html = b"<html><body><script>alert(document.cookie)</script></body></html>";
    let name = "<img src=x onerror=alert(1)>\";x=.html";
    let id = f.ready(name, "text/html", html).await;
    let response = download(&f, &f.client, id).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.headers["content-type"], "application/octet-stream");
    assert_eq!(response.headers["x-content-type-options"], "nosniff");
    assert_eq!(response.headers["content-security-policy"], "sandbox");
    let disposition = response.headers["content-disposition"].to_str().unwrap();
    assert_eq!(
        disposition,
        "attachment; filename*=UTF-8''%3Cimg%20src%3Dx%20onerror%3Dalert%281%29%3E%22%3Bx%3D.html"
    );
    let encoded = disposition.split_once("''").unwrap().1;
    assert!(!encoded.contains(['<', '>', '"', ';', '/', ' ', '=', '(']));
    assert_eq!(response.body.as_ref(), html);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn svg_is_served_as_octet_stream(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>";
    let id = f.ready("logo.svg", "image/svg+xml", svg).await;
    let response = download(&f, &f.client, id).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.headers["content-type"], "application/octet-stream");
    assert_eq!(response.headers["content-security-policy"], "sandbox");
    assert!(
        response.headers["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn attachment_events_reach_the_conversation_scope(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let ready = f.ready("ready.txt", "text/plain", b"ready").await;
    let cancelled = f.create("cancel.txt", "text/plain", b"cancel").await;
    assert_eq!(f.cancel(cancelled, "cancel_it").await.status, 204);
    let failed = f.create("failed.txt", "text/plain", b"right").await;
    f.upload(failed, b"wrong").await;
    assert_eq!(f.complete(failed, "fail_it", None).await.status, 422);
    assert_eq!(f.cancel(ready, "release_it").await.status, 204);
    let range = f
        .client
        .send(
            Method::GET,
            &format!(
                "/api/v1/events?scope=conversation:{}&after=0",
                f.conversation
            ),
            None,
            None,
        )
        .await
        .json::<Value>();
    let schema: Value =
        serde_json::from_str(include_str!("../../../docs/api/events.schema.json")).unwrap();
    let mut root = schema.clone();
    root["$ref"] = json!("#/$defs/attachment_state_changed");
    root.as_object_mut().unwrap().remove("oneOf");
    let validator = jsonschema::validator_for(&root).unwrap();
    let mut seen = Vec::new();
    for event in range["events"].as_array().unwrap() {
        if event["kind"] != "attachment_state_changed" {
            continue;
        }
        assert!(validator.is_valid(event), "{event}");
        assert_eq!(event["scope"], format!("conversation:{}", f.conversation));
        seen.push((
            event["payload"]["attachment_id"]
                .as_str()
                .unwrap()
                .to_owned(),
            event["payload"]["state"].as_str().unwrap().to_owned(),
            event["payload"]["reason"].as_str().map(str::to_owned),
        ));
    }
    let of = |id: Uuid| {
        seen.iter()
            .filter(|(a, _, _)| *a == id.to_string())
            .map(|(_, s, r)| (s.clone(), r.clone()))
            .collect::<Vec<_>>()
    };
    let s = |v: &str| (v.to_owned(), None);
    assert_eq!(
        of(ready),
        vec![s("created"), s("uploading"), s("ready"), s("released")]
    );
    assert_eq!(of(cancelled), vec![s("created"), s("cancelled")]);
    assert_eq!(
        of(failed),
        vec![
            s("created"),
            s("uploading"),
            ("failed".into(), Some("checksum_mismatch".into()))
        ]
    );
    // Ни имя файла, ни содержимое в событие не попадают.
    assert!(!range.to_string().contains("ready.txt"));
}

async fn scheduled(f: &Fixture, key: &str, ids: &[Uuid]) -> testkit::TestResponse {
    f.client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/scheduled", f.conversation),
            Some(key),
            Some(json!({"run_at": (Utc::now() + TimeDelta::seconds(2)).to_rfc3339(), "content":[{"type":"text","text":"по расписанию"}], "attachment_ids": ids})),
        )
        .await
}

async fn job_links(f: &Fixture, job: &str) -> i64 {
    sqlx::query_scalar("select count(*) from scheduled_job_attachments where job_id=$1::uuid")
        .bind(job)
        .fetch_one(&f.pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn scheduled_message_carries_attachments_and_fails_cleanly_when_they_vanish(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let a = f.ready("plan.txt", "text/plain", b"plan").await;
    let pending = f.create("unready.txt", "text/plain", b"x").await;
    assert_error(
        &scheduled(&f, "unready", &[pending]).await,
        409,
        "attachment_not_ready",
        "after_condition",
    );
    assert_eq!(f.count("select count(*) from scheduled_jobs").await, 0);
    let created = scheduled(&f, "carry", &[a]).await;
    assert_eq!(created.status, 201);
    let job = created.json::<Value>()["id"].as_str().unwrap().to_owned();
    assert_eq!(job_links(&f, &job).await, 1);
    // Ссылку держит отложенное сообщение: сборщик вложение не отдаёт.
    f.age(a, 48).await;
    let report = attachments::gc::run_once(&f.pool, f.blobs(), Utc::now(), Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(report.released, 0);
    assert_eq!(f.state_of(a).await, "ready");
    let tick = f
        .state
        .scheduler
        .tick(Utc::now() + TimeDelta::seconds(5))
        .await
        .unwrap();
    assert_eq!(tick.fired, 1);
    let content: Value = sqlx::query_scalar(
        "select content from inputs where conversation_id=$1 and idempotency_key=$2",
    )
    .bind(f.conversation)
    .bind(format!("sched:{job}"))
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(content[1]["attachment_id"], a.to_string());
    assert_eq!(f.count("select count(*) from input_attachments").await, 1);
    assert_eq!(job_links(&f, &job).await, 0);

    let b = f.ready("vanish.txt", "text/plain", b"vanish").await;
    let vanishing = scheduled(&f, "vanish", &[b]).await;
    assert_eq!(vanishing.status, 201);
    let job = vanishing.json::<Value>()["id"].as_str().unwrap().to_owned();
    sqlx::query("update attachments set state='released' where id=$1")
        .bind(b)
        .execute(&f.pool)
        .await
        .unwrap();
    let before = inputs_of(&f, f.conversation).await;
    let tick = f
        .state
        .scheduler
        .tick(Utc::now() + TimeDelta::seconds(5))
        .await
        .unwrap();
    assert_eq!(tick.failed, 1);
    assert_eq!(inputs_of(&f, f.conversation).await, before);
    let (state, result): (String, Value) =
        sqlx::query_as("select state, result from scheduled_jobs where id=$1::uuid")
            .bind(&job)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(state, "failed");
    assert_eq!(result, json!({"error":"attachment_unavailable"}));
    assert_eq!(job_links(&f, &job).await, 0);
    let last = f.events("scheduled_changed").await.pop().unwrap();
    assert_eq!(last["state"], "failed");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cancelling_or_failing_a_scheduled_job_deletes_its_attachment_links(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool).await;
    let a = f.ready("a.txt", "text/plain", b"a").await;
    let job = scheduled(&f, "cancel_job", &[a]).await.json::<Value>()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(job_links(&f, &job).await, 1);
    let cancelled = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/scheduled/{job}"),
            Some("drop_job"),
            None,
        )
        .await;
    assert_eq!(cancelled.status, 200);
    assert_eq!(job_links(&f, &job).await, 0);
    f.age(a, 48).await;
    let report =
        attachments::gc::run_once(&f.pool, f.blobs(), Utc::now(), Duration::from_secs(86400))
            .await
            .unwrap();
    assert_eq!(report.released, 1);
    assert_eq!(f.state_of(a).await, "released");

    let b = f.ready("b.txt", "text/plain", b"b").await;
    let failing = scheduled(&f, "fail_job", &[b]).await.json::<Value>()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    sqlx::query("update queue_state set closed=true where conversation_id=$1")
        .bind(f.conversation)
        .execute(&f.pool)
        .await
        .unwrap();
    let tick = f
        .state
        .scheduler
        .tick(Utc::now() + TimeDelta::seconds(5))
        .await
        .unwrap();
    assert_eq!(tick.failed, 1);
    assert_eq!(job_links(&f, &failing).await, 0);
    sqlx::query("update queue_state set closed=false where conversation_id=$1")
        .bind(f.conversation)
        .execute(&f.pool)
        .await
        .unwrap();

    let c = f.ready("c.txt", "text/plain", b"c").await;
    let archived = scheduled(&f, "archive_job", &[c]).await.json::<Value>()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let archive = f
        .client
        .send(
            Method::PATCH,
            &format!("/api/v1/conversations/{}", f.conversation),
            Some("archive"),
            Some(json!({"archived": true, "force": true})),
        )
        .await;
    assert_eq!(archive.status, 200, "{:?}", archive.body);
    assert_eq!(job_links(&f, &archived).await, 0);
    assert_eq!(
        f.count("select count(*) from scheduled_job_attachments")
            .await,
        0
    );
}

async fn signal_at(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("process barrier");
}

#[tokio::test]
async fn cross_process_upload_child() {
    let Ok(database) = std::env::var("ATTACHMENT_TEST_DB") else {
        return;
    };
    let pool = PgPool::connect(&database).await.unwrap();
    let config = canterlot_hub::config::AttachmentsConfig {
        blob_dir: std::env::var("ATTACHMENT_TEST_BLOBS").unwrap().into(),
        ..Default::default()
    };
    let store = attachments::LocalBlobStore::open(&config, Arc::new(attachments::NoTrace)).unwrap();
    let owner = std::env::var("ATTACHMENT_TEST_OWNER")
        .unwrap()
        .parse()
        .unwrap();
    let id = std::env::var("ATTACHMENT_TEST_ID")
        .unwrap()
        .parse()
        .unwrap();
    let signal = std::path::PathBuf::from(std::env::var("ATTACHMENT_TEST_SIGNAL").unwrap());
    let barrier = signal.clone();
    let body = stream::once(async move {
        std::fs::write(barrier.join("started"), []).unwrap();
        signal_at(&barrier.join("release")).await;
        Ok(Bytes::from_static(b"GOOD"))
    })
    .boxed();
    assert_eq!(
        attachments::upload::upload_chunk(
            &pool,
            &store,
            owner,
            id,
            0,
            body,
            attachments::UPLOAD_CHUNK_BYTES,
            Uuid::now_v7()
        )
        .await
        .unwrap(),
        4
    );
    std::fs::write(signal.join("uploaded"), []).unwrap();
    signal_at(&signal.join("published")).await;
    let result = attachments::upload::upload_chunk(
        &pool,
        &store,
        owner,
        id,
        0,
        stream::iter([Ok(Bytes::from_static(b"EVIL"))]).boxed(),
        attachments::UPLOAD_CHUNK_BYTES,
        Uuid::now_v7(),
    )
    .await;
    assert!(matches!(
        result,
        Err(attachments::AttachmentError::NotUploadable)
    ));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn two_hub_processes_cannot_mutate_a_ready_linked_blob(pool: PgPool) {
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool.clone()).await;
    let id = f
        .create("fixture.bin", "application/octet-stream", b"GOOD")
        .await;
    let signal = tempfile::tempdir().unwrap();
    use sqlx::ConnectOptions;
    let database = pool.connect_options().to_url_lossy().to_string();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "cross_process_upload_child"])
        .env("ATTACHMENT_TEST_DB", database)
        .env("ATTACHMENT_TEST_BLOBS", &f.config.blob_dir)
        .env("ATTACHMENT_TEST_OWNER", f.owner.to_string())
        .env("ATTACHMENT_TEST_ID", id.to_string())
        .env("ATTACHMENT_TEST_SIGNAL", signal.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    signal_at(&signal.path().join("started")).await;
    let result = attachments::upload::upload_chunk(
        &pool,
        f.blobs(),
        f.owner,
        id,
        0,
        stream::iter([Ok(Bytes::from_static(b"GOOD"))]).boxed(),
        attachments::UPLOAD_CHUNK_BYTES,
        Uuid::now_v7(),
    )
    .await;
    assert!(matches!(
        result,
        Err(attachments::AttachmentError::UploadInProgress { .. })
    ));
    assert_error(
        &f.complete(id, &key(), None).await,
        409,
        "upload_in_progress",
        "after_condition",
    );
    assert_error(
        &f.cancel(id, &key()).await,
        409,
        "upload_in_progress",
        "after_condition",
    );
    f.age(id, 25).await;
    let report =
        attachments::gc::run_once(&pool, f.blobs(), Utc::now(), Duration::from_secs(86400))
            .await
            .unwrap();
    assert_eq!(report.expired_uploads, 0);
    assert_eq!(f.state_of(id).await, "uploading");
    std::fs::write(signal.path().join("release"), []).unwrap();
    signal_at(&signal.path().join("uploaded")).await;
    assert_eq!(upload_offset(&f.head(id).await), Some(4));
    assert_eq!(f.complete(id, &key(), None).await.status, 200);
    assert_eq!(f.submit(&key(), "read", &[id]).await.status, 201);
    std::fs::write(signal.path().join("published"), []).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(30), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    let bytes = std::fs::read(f.blob_path(id)).unwrap();
    assert!(bytes == b"GOOD");
    assert_eq!(sha(&bytes), sha(b"GOOD"));
    assert_eq!(f.state_of(id).await, "ready");
}

#[tokio::test]
async fn directory_durability_child() {
    use attachments::BlobStore;
    let Ok(root) = std::env::var("ATTACHMENT_TRACE_ROOT") else {
        return;
    };
    let config = canterlot_hub::config::AttachmentsConfig {
        blob_dir: std::path::Path::new(&root).join("fresh/nested/blob-root"),
        ..Default::default()
    };
    let trace = Arc::new(Recorder::default());
    let store = attachments::LocalBlobStore::open(&config, trace.clone()).unwrap();
    let id = Uuid::now_v7();
    let shard = store.blob_path(id).parent().unwrap().to_owned();
    trace.hook(Some(Arc::new(move |step| {
        if step == PublishStep::FileSynced {
            std::fs::create_dir(&shard).unwrap();
        }
    })));
    store
        .append_staging(
            id,
            0,
            stream::iter([Ok(Bytes::from_static(b"GOOD"))]).boxed(),
            4,
        )
        .await
        .unwrap();
    store.publish(id, 4, &sha(b"GOOD")).await.unwrap();
}

fn directory_trace() -> (tempfile::TempDir, Vec<String>) {
    let root = tempfile::tempdir().unwrap();
    let trace = root.path().join("syscalls");
    let status = std::process::Command::new("strace")
        .args(["-f", "-qq", "-yy", "-e", "trace=fsync,mkdir,rename", "-o"])
        .arg(&trace)
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "directory_durability_child"])
        .env("ATTACHMENT_TRACE_ROOT", root.path())
        .status()
        .unwrap();
    assert!(status.success());
    let calls = std::fs::read_to_string(trace)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    (root, calls)
}

#[test]
fn startup_syncs_blob_directory_ancestors() {
    let (root, calls) = directory_trace();
    let sync = |path: &std::path::Path| {
        calls
            .iter()
            .position(|line| {
                line.contains("fsync(") && line.contains(&format!("<{}>", path.display()))
            })
            .expect("directory parent must be synced")
    };
    let base = root.path();
    let first = sync(base);
    let next = sync(&base.join("fresh"));
    let nested = sync(&base.join("fresh/nested"));
    let blob = sync(&base.join("fresh/nested/blob-root"));
    assert!(first < next && next < nested && nested < blob);
}

#[test]
fn publication_syncs_an_already_created_shard() {
    let (root, calls) = directory_trace();
    let base = root.path();
    let sync = |path: &std::path::Path| {
        calls
            .iter()
            .position(|line| {
                line.contains("fsync(") && line.contains(&format!("<{}>", path.display()))
            })
            .expect("shard parent must be synced")
    };
    let shard_mkdir = calls
        .iter()
        .position(|line| line.contains("mkdir(") && line.contains("/blobs/01\""))
        .unwrap();
    let parent = sync(&base.join("fresh/nested/blob-root/blobs"));
    let rename = calls
        .iter()
        .position(|line| line.contains("rename("))
        .unwrap();
    assert!(shard_mkdir < parent && parent < rename);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn aborted_publication_holds_the_volume_lock_until_io_finishes(pool: PgPool) {
    use attachments::BlobStore;
    let _shared = HEAVY.read().await;
    let f = Fixture::new(pool.clone()).await;
    let id = f
        .create("fixture.bin", "application/octet-stream", b"GOOD")
        .await;
    f.upload(id, b"GOOD").await;
    let barrier = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let wait = barrier.clone();
    let (entered, started) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    f.trace.hook(Some(Arc::new(move |step| {
        if step == PublishStep::FileSynced {
            entered.lock().unwrap().take().unwrap().send(()).unwrap();
            let (mutex, condvar) = &*wait;
            drop(
                condvar
                    .wait_while(mutex.lock().unwrap(), |released| !*released)
                    .unwrap(),
            );
        }
    })));
    let store = f.store.clone();
    let owner = f.owner;
    let publish = tokio::spawn(async move {
        attachments::complete(&pool, &*store, owner, id, None, "aborted", Uuid::now_v7()).await
    });
    tokio::time::timeout(Duration::from_secs(10), started)
        .await
        .unwrap()
        .unwrap();
    publish.abort();
    assert!(publish.await.unwrap_err().is_cancelled());
    let locked = f.store.try_lock(id).unwrap().is_none();
    let (mutex, condvar) = &*barrier;
    *mutex.lock().unwrap() = true;
    condvar.notify_one();
    assert!(
        locked,
        "cancelled request must not release ongoing publication I/O"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if f.store.try_lock(id).unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    f.trace.hook(None);
    assert_eq!(f.complete(id, "recovered", None).await.status, 200);
    assert!(std::fs::read(f.blob_path(id)).unwrap() == b"GOOD");
}
