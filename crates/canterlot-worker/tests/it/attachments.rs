#![forbid(unsafe_code)]

use canterlot_proto::worker::v1::{AttachmentChunk, AttachmentRef};
use canterlot_worker::attachments::{
    AttachmentSource, AttachmentStore, ChunkStream, FetchError, RetryPolicy, SourceSlot,
    safe_file_name,
};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[allow(dead_code)]
use crate::support;

type Reply = Result<Vec<Result<AttachmentChunk, FetchError>>, FetchError>;
#[derive(Default)]
struct Source {
    replies: Mutex<VecDeque<Reply>>,
    offsets: Mutex<Vec<u64>>,
}
#[async_trait::async_trait]
impl AttachmentSource for Source {
    async fn open(&self, _: Uuid, offset: u64) -> Result<ChunkStream, FetchError> {
        self.offsets.lock().unwrap().push(offset);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected fetch")?;
        Ok(Box::pin(tokio_stream::iter(reply)))
    }
}
fn reference(bytes: &[u8]) -> AttachmentRef {
    AttachmentRef {
        attachment_id: Uuid::now_v7().to_string(),
        name: "../../private.bin".into(),
        mime: "application/octet-stream".into(),
        size: bytes.len() as u64,
        sha256: canterlot_hub::attachments::blobstore::hex(&Sha256::digest(bytes)),
        image: false,
    }
}
fn chunk(offset: u64, bytes: &[u8], last: bool) -> Result<AttachmentChunk, FetchError> {
    Ok(AttachmentChunk {
        offset,
        data: bytes.to_vec(),
        last,
    })
}
fn source(replies: Vec<Reply>) -> (Arc<Source>, SourceSlot) {
    let source = Arc::new(Source {
        replies: Mutex::new(replies.into()),
        ..Default::default()
    });
    let (_, rx) = watch::channel(Some(source.clone() as Arc<dyn AttachmentSource>));
    (source, rx)
}
fn policy() -> RetryPolicy {
    RetryPolicy {
        content_attempts: 2,
        retry: Duration::from_millis(1),
        backoff_cap: Duration::from_millis(5),
    }
}
fn temp(root: &std::path::Path, r: &AttachmentRef) -> std::path::PathBuf {
    root.join("attachments/.tmp")
        .join(format!("{}.part", r.attachment_id))
}

#[test]
fn published_name_is_safe_and_unique() {
    for name in [
        "../../etc/passwd",
        "\0secret",
        "日本語",
        &"a".repeat(300),
        ".hidden",
        "",
    ] {
        let id = Uuid::now_v7();
        let result = safe_file_name(id, name);
        let slug = &result[37..];
        assert!(result.starts_with(&format!("{id}-")));
        assert!(!slug.is_empty() && slug.len() <= 64 && !slug.starts_with('.'));
        assert!(
            slug.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        );
        assert_ne!(result, safe_file_name(Uuid::now_v7(), name));
    }
    assert!(safe_file_name(Uuid::now_v7(), "").ends_with("-file"));
}

#[tokio::test]
async fn directory_and_files_have_private_modes_and_live_outside_the_workdir() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();
    let exec = root
        .path()
        .join("executions")
        .join(Uuid::now_v7().to_string());
    fs::create_dir_all(&exec).unwrap();
    let store = AttachmentStore::new(&exec, 2).unwrap();
    let r = reference(b"hello");
    let (_, rx) = source(vec![Ok(vec![chunk(0, b"hello", true)])]);
    let p = store
        .ensure_published(rx, &r, &policy(), CancellationToken::new())
        .await
        .unwrap();
    assert!(p.path.starts_with(exec.join("attachments")) && !p.path.starts_with(work));
    assert_eq!(
        fs::metadata(exec.join("attachments"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(exec.join("attachments/.tmp"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&p.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read(p.path).unwrap(), b"hello");
    assert_eq!(p.name, r.name);
}

#[tokio::test]
async fn fetch_resumes_from_offset_and_checksum_mismatch_is_refetched() {
    let root = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(root.path(), 2).unwrap();
    let r = reference(b"abcdef");
    let (s, rx) = source(vec![
        Ok(vec![chunk(0, b"abc", false), Err(FetchError::Transport)]),
        Ok(vec![chunk(3, b"xxx", true)]),
        Ok(vec![chunk(0, b"abcdef", true)]),
    ]);
    let p = store
        .ensure_published(rx, &r, &policy(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(*s.offsets.lock().unwrap(), [0, 3, 0]);
    assert_eq!(fs::read(p.path).unwrap(), b"abcdef");
}

#[tokio::test]
async fn size_mismatch_is_nonretryable() {
    for (size, chunks) in [(2, vec![chunk(0, b"abc", true)]), (104_857_601, vec![])] {
        let root = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(root.path(), 2).unwrap();
        let mut r = reference(b"ab");
        r.size = size;
        let (s, rx) = source(vec![Ok(chunks)]);
        assert!(matches!(
            store
                .ensure_published(rx, &r, &policy(), CancellationToken::new())
                .await,
            Err(FetchError::SizeMismatch)
        ));
        assert!(s.offsets.lock().unwrap().len() <= 1);
        assert!(!temp(root.path(), &r).exists());
    }
}

/// Пустое вложение: хаб отдаёт один пустой кадр `last`, агент получает файл нулевой длины;
/// повтор находит опубликованный файл без новой выборки.
#[tokio::test]
async fn empty_attachment_is_published_as_a_zero_byte_file() {
    let root = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(root.path(), 2).unwrap();
    let r = reference(b"");
    assert_eq!(r.size, 0);
    let (s, rx) = source(vec![Ok(vec![chunk(0, b"", true)])]);
    let published = store
        .ensure_published(rx.clone(), &r, &policy(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(fs::metadata(&published.path).unwrap().len(), 0);
    assert!(!temp(root.path(), &r).exists());
    let again = store
        .ensure_published(rx, &r, &policy(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(again.path, published.path);
    assert_eq!(*s.offsets.lock().unwrap(), [0]);
}

#[tokio::test]
async fn already_published_same_checksum_is_reported_ready_again() {
    let root = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(root.path(), 2).unwrap();
    let r = reference(b"abc");
    let (s, rx) = source(vec![Ok(vec![chunk(0, b"abc", true)])]);
    let first = store
        .ensure_published(rx.clone(), &r, &policy(), CancellationToken::new())
        .await
        .unwrap();
    let second = store
        .ensure_published(rx, &r, &policy(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(first.path, second.path);
    assert_eq!(*s.offsets.lock().unwrap(), [0]);
}

#[tokio::test]
async fn temp_file_is_removed_on_cancel_or_withdraw() {
    // Отмена и отзыв приходят в хранилище одним токеном: до начала, без источника и во время потока.
    for case in ["before_start", "waiting_for_source", "streaming"] {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(AttachmentStore::new(root.path(), 2).unwrap());
        let r = reference(b"abcdef");
        let (_tx, rx) = if case == "streaming" {
            watch::channel(Some(Arc::new(Stalled) as Arc<dyn AttachmentSource>))
        } else {
            fs::write(temp(root.path(), &r), b"abc").unwrap();
            watch::channel(None)
        };
        let cancel = CancellationToken::new();
        if case == "before_start" {
            cancel.cancel();
        }
        let task = {
            let (store, r, cancel) = (store.clone(), r.clone(), cancel.clone());
            tokio::spawn(async move { store.ensure_published(rx, &r, &policy(), cancel).await })
        };
        if case != "before_start" {
            tokio::time::timeout(Duration::from_secs(2), async {
                while !fs::metadata(temp(root.path(), &r)).is_ok_and(|m| m.len() == 3) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        }
        assert!(matches!(task.await.unwrap(), Err(FetchError::Transport)));
        assert!(!temp(root.path(), &r).exists(), "{case}");
    }
}

struct Stalled;
#[async_trait::async_trait]
impl AttachmentSource for Stalled {
    async fn open(&self, _: Uuid, offset: u64) -> Result<ChunkStream, FetchError> {
        use tokio_stream::StreamExt;
        assert_eq!(offset, 0);
        Ok(Box::pin(
            tokio_stream::iter([chunk(0, b"abc", false)]).chain(tokio_stream::pending()),
        ))
    }
}

#[tokio::test]
async fn remove_temp_spares_a_file_another_fetch_is_writing() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(AttachmentStore::new(root.path(), 2).unwrap());
    let r = reference(b"abcdef");
    let (_tx, rx) = watch::channel(Some(Arc::new(Stalled) as Arc<dyn AttachmentSource>));
    let cancel = CancellationToken::new();
    let task = {
        let (store, r, cancel) = (store.clone(), r.clone(), cancel.clone());
        tokio::spawn(async move { store.ensure_published(rx, &r, &policy(), cancel).await })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while !fs::metadata(temp(root.path(), &r)).is_ok_and(|m| m.len() == 3) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    store.remove_temp(r.attachment_id.parse().unwrap());
    assert_eq!(fs::read(temp(root.path(), &r)).unwrap(), b"abc");
    cancel.cancel();
    assert!(task.await.unwrap().is_err());
    assert!(!temp(root.path(), &r).exists());
}

#[tokio::test]
async fn restart_reuses_published_files() {
    let root = tempfile::tempdir().unwrap();
    let r = reference(b"abc");
    let (s, rx) = source(vec![Ok(vec![chunk(0, b"abc", true)])]);
    let first = AttachmentStore::new(root.path(), 2)
        .unwrap()
        .ensure_published(rx.clone(), &r, &policy(), CancellationToken::new())
        .await
        .unwrap();
    let second = AttachmentStore::new(root.path(), 2)
        .unwrap()
        .ensure_published(rx, &r, &policy(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(first.path, second.path);
    assert_eq!(*s.offsets.lock().unwrap(), [0]);
}

/// Первый поток обрывается после трёх байт, дальше хаб недоступен.
#[derive(Default)]
struct Outage {
    offsets: Mutex<Vec<u64>>,
}
#[async_trait::async_trait]
impl AttachmentSource for Outage {
    async fn open(&self, _: Uuid, offset: u64) -> Result<ChunkStream, FetchError> {
        let first = {
            let mut offsets = self.offsets.lock().unwrap();
            offsets.push(offset);
            offsets.len() == 1
        };
        if !first {
            return Err(FetchError::Transport);
        }
        Ok(Box::pin(tokio_stream::iter([
            chunk(0, b"abc", false),
            Err(FetchError::Transport),
        ])))
    }
}

#[tokio::test]
async fn worker_restart_with_a_half_fetched_attachment_resumes_from_the_temp_file() {
    let root = tempfile::tempdir().unwrap();
    let r = reference(b"abcdef");
    let store = Arc::new(AttachmentStore::new(root.path(), 2).unwrap());
    let outage = Arc::new(Outage::default());
    let (_tx, rx) = watch::channel(Some(outage.clone() as Arc<dyn AttachmentSource>));
    let task = {
        let store = store.clone();
        let r = r.clone();
        tokio::spawn(async move {
            store
                .ensure_published(rx, &r, &policy(), CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while outage.offsets.lock().unwrap().len() < 3 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    // Остановка службы во время сбоя: временный файл остаётся для следующего запуска.
    task.abort();
    assert!(task.await.is_err_and(|error| error.is_cancelled()));
    drop(store);
    let offsets = outage.offsets.lock().unwrap().clone();
    assert_eq!(offsets[0], 0);
    assert!(offsets[1..].iter().all(|offset| *offset == 3));
    assert_eq!(fs::metadata(temp(root.path(), &r)).unwrap().len(), 3);
    let (s, rx) = source(vec![Ok(vec![chunk(3, b"def", true)])]);
    let p = AttachmentStore::new(root.path(), 2)
        .unwrap()
        .ensure_published(rx, &r, &policy(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(*s.offsets.lock().unwrap(), [3]);
    assert_eq!(fs::read(p.path).unwrap(), b"abcdef");
}

#[tokio::test]
async fn resource_exhausted_is_retried_not_rejected() {
    let root = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(root.path(), 2).unwrap();
    let r = reference(b"abc");
    let codes = Arc::new(Mutex::new(
        [
            tonic::Code::ResourceExhausted,
            tonic::Code::Unavailable,
            tonic::Code::Cancelled,
            tonic::Code::DeadlineExceeded,
        ]
        .into(),
    ));
    let (hub, _) = support::ProtocolHub::start(support::Behavior {
        fetch_failures: codes.clone(),
        attachment: b"abc".to_vec(),
        ..Default::default()
    })
    .await;
    let channel = tonic::transport::Endpoint::from_shared(hub.config.hub_url.clone())
        .unwrap()
        .tls_config(tonic::transport::ClientTlsConfig::new().ca_certificate(
            tonic::transport::Certificate::from_pem(fs::read(&hub.config.ca_cert).unwrap()),
        ))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let source = canterlot_worker::session::GrpcAttachmentSource::from_data_channel(
        channel,
        "Bearer protocol-test".parse().unwrap(),
    );
    let (_tx, rx) = watch::channel(Some(Arc::new(source) as Arc<dyn AttachmentSource>));
    store
        .ensure_published(rx, &r, &policy(), CancellationToken::new())
        .await
        .unwrap();
    assert!(codes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn remove_all_removes_execution_attachment_directory() {
    let root = tempfile::tempdir().unwrap();
    let store = AttachmentStore::new(root.path(), 2).unwrap();
    let r = reference(b"abc");
    let (_, rx) = source(vec![Ok(vec![chunk(0, b"abc", true)])]);
    let p = store
        .ensure_published(rx, &r, &policy(), CancellationToken::new())
        .await
        .unwrap();
    assert!(p.path.exists());
    store.remove_all();
    assert!(!root.path().join("attachments").exists());
}

#[tokio::test]
async fn relative_execution_directory_produces_an_absolute_agent_path() {
    let root = tempfile::tempdir().unwrap();
    let cwd = std::env::current_dir().unwrap();
    let relative = std::iter::repeat_n("..", cwd.components().count() - 1)
        .collect::<std::path::PathBuf>()
        .join(root.path().strip_prefix("/").unwrap());
    let store = AttachmentStore::new(&relative, 2).unwrap();
    let r = reference(b"absolute");
    let (_, rx) = source(vec![Ok(vec![chunk(0, b"absolute", true)])]);
    assert!(
        store
            .ensure_published(rx, &r, &policy(), CancellationToken::new())
            .await
            .unwrap()
            .path
            .is_absolute()
    );
}

#[cfg(all(feature = "synthetic-adapter", feature = "fault-injection"))]
#[cfg(all(feature = "synthetic-adapter", feature = "fault-injection"))]
#[path = "support/attachments.rs"]
mod execution_fixture;

#[cfg(all(feature = "synthetic-adapter", feature = "fault-injection"))]
#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn terminal_execution_attachment_dir_is_removed_after_ack(pool: sqlx::PgPool) {
    let f = execution_fixture::Fixture::new(&pool).await;
    let id = f.ready(b"cleanup").await;
    let input = f.submit(&[id], "hello").await;
    f.accepted(&input).await;
    f.hub().faults.drop_next_sj_acks(100);
    f.stop_execution(false).await;
    execution_fixture::until(async || {
        sqlx::query_scalar::<_, bool>("select state='stopped' from executions where id=$1")
            .bind(f.execution)
            .fetch_one(&pool)
            .await
            .unwrap()
    })
    .await;
    assert!(f.published(id).exists());
    f.hub().faults.drop_next_sj_acks(0);
    execution_fixture::until(async || !f.exec_dir().join("attachments").exists()).await;
}
