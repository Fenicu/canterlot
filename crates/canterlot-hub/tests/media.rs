//! Картинки, на которые смотрел агент (pilot.md §5.7): приём PutMedia, надёжная публикация,
//! ссылка из блока по записи BlockMedia, выдача владельцу и сборка мусора.

#[path = "support/task9.rs"]
mod support;

use std::{
    os::unix::fs::MetadataExt,
    time::{Duration, SystemTime},
};

use axum::http::Method;
use canterlot_hub::{
    attachments::{PublishStep, gc},
    config::HubTimers,
    executions,
    grpc::{self, HubGrpc},
    ingest::{Ingest, IngestError, IngestOutcome, JournalKind},
    model::ExecutionState,
    testkit::{self, TestClient},
};
use canterlot_proto::{
    limits::{MAX_FILE_CHUNK_BYTES, MAX_MEDIA_BYTES},
    worker::v1::{record_body::Body, *},
};
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use support::{assert_error, content, sha};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tonic::{
    Code, Request,
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint},
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(10);
const DAY: Duration = Duration::from_secs(86400);

/// PNG с заголовком IHDR и произвольным хвостом: хабу важна только сигнатура.
fn png(width: u32, height: u32, tail: usize, seed: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend(content(tail, seed));
    bytes
}

type Client = worker_data_client::WorkerDataClient<Channel>;

struct Media {
    files: support::Fixture,
    hub: HubGrpc,
    token: String,
    execution: Uuid,
    journal: Uuid,
    seq: u64,
    ingest: Ingest,
}

impl Media {
    async fn new(pool: PgPool) -> Self {
        let files = support::Fixture::new(pool.clone()).await;
        let token = Uuid::now_v7().to_string();
        sqlx::query("insert into machine_credentials(machine_id,token_sha256) values($1,$2)")
            .bind(files.machine)
            .bind(Sha256::digest(token.as_bytes()).as_slice())
            .execute(&pool)
            .await
            .unwrap();
        let execution = executions::start(&pool, files.owner, files.conversation, Uuid::now_v7())
            .await
            .unwrap();
        for state in [ExecutionState::Starting, ExecutionState::Running] {
            executions::set_state(&pool, execution.execution_id, state, None, Uuid::now_v7())
                .await
                .unwrap();
        }
        let hub = HubGrpc::new(
            pool.clone(),
            files.state.registry.clone(),
            files.state.installation,
            HubTimers::default(),
        )
        .with_blob_store(files.store.clone());
        let mut media = Self {
            files,
            hub,
            token,
            execution: execution.execution_id,
            journal: Uuid::now_v7(),
            seq: 0,
            ingest: Ingest::new(pool),
        };
        let opened = Body::JournalOpened(JournalOpened {
            execution_id: execution.execution_id.to_string(),
            generation: execution.generation as u64,
            supervisor_protocol_version: 1,
        });
        media.sj(vec![opened]).await.unwrap();
        media
    }

    fn pool(&self) -> &PgPool {
        &self.files.pool
    }

    fn header(&self, id: Uuid, block: Uuid, bytes: &[u8]) -> MediaHeader {
        MediaHeader {
            media_id: id.to_string(),
            execution_id: self.execution.to_string(),
            block_id: block.to_string(),
            mime: "image/png".into(),
            size: bytes.len() as u64,
            sha256: sha(bytes),
            ..Default::default()
        }
    }

    async fn sj(&mut self, bodies: Vec<Body>) -> Result<IngestOutcome, IngestError> {
        let batch = JournalRecords {
            journal_id: self.journal.to_string(),
            records: bodies
                .into_iter()
                .map(|body| {
                    self.seq += 1;
                    JournalRecord {
                        seq: self.seq,
                        body: Some(RecordBody { body: Some(body) }),
                    }
                })
                .collect(),
        };
        self.ingest
            .apply_batch(
                self.files.machine,
                &Sha256::digest(self.token.as_bytes()),
                JournalKind::Sj,
                &batch,
            )
            .await
    }

    async fn committed(&mut self, bodies: Vec<Body>) {
        let outcome = self.sj(bodies).await.unwrap();
        assert_eq!(
            outcome,
            IngestOutcome::Committed {
                up_to_seq: self.seq
            }
        );
    }

    /// Блок вызова инструмента `Read`, к которому прикрепляется картинка.
    async fn tool_block(&mut self) -> Uuid {
        let block = Uuid::now_v7();
        self.committed(vec![Body::BlockStarted(BlockStarted {
            block_id: block.to_string(),
            rev: 1,
            kind: "tool_call".into(),
            agent: "main".into(),
            model: "synthetic-large".into(),
            tool_name: "Read".into(),
            summary: "/work/screen.png".into(),
            ..Default::default()
        })])
        .await;
        block
    }

    fn reference(id: Uuid, block: Uuid, bytes: &[u8], width: u32, height: u32) -> Body {
        Body::BlockMedia(BlockMedia {
            block_id: block.to_string(),
            media_id: id.to_string(),
            mime: "image/png".into(),
            size: bytes.len() as u64,
            sha256: sha(bytes),
            width,
            height,
        })
    }

    async fn rows(&self, id: Uuid) -> i64 {
        sqlx::query_scalar("select count(*) from block_media where id=$1")
            .bind(id)
            .fetch_one(self.pool())
            .await
            .unwrap()
    }

    async fn attached(&self, id: Uuid) -> bool {
        sqlx::query_scalar(
            "select coalesce((select attached_at is not null from block_media where id=$1), false)",
        )
        .bind(id)
        .fetch_one(self.pool())
        .await
        .unwrap()
    }

    async fn history_block(&self, block: Uuid) -> Value {
        let page: Value = self
            .files
            .client
            .send(
                Method::GET,
                &format!(
                    "/api/v1/conversations/{}/history?limit=200",
                    self.files.conversation
                ),
                None,
                None,
            )
            .await
            .json();
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == block.to_string())
            .cloned()
            .expect("block in history")
    }

    async fn download(&self, client: &TestClient, id: Uuid) -> testkit::TestResponse {
        client
            .send(Method::GET, &format!("/api/v1/media/{id}"), None, None)
            .await
    }
}

fn request<T>(body: T, token: &str) -> Request<T> {
    let mut req = Request::new(body);
    let mut bearer = format!("Bearer {token}")
        .parse::<tonic::metadata::MetadataValue<_>>()
        .unwrap();
    bearer.set_sensitive(true);
    req.metadata_mut().insert("authorization", bearer);
    req
}

fn frames(header: Option<MediaHeader>, bytes: &[u8], chunk: usize) -> Vec<MediaChunk> {
    header
        .map(|header| MediaChunk {
            part: Some(media_chunk::Part::Header(header)),
        })
        .into_iter()
        .chain(bytes.chunks(chunk).map(|data| MediaChunk {
            part: Some(media_chunk::Part::Data(data.to_vec())),
        }))
        .collect()
}

async fn put_frames(
    client: &mut Client,
    token: &str,
    frames: Vec<MediaChunk>,
) -> Result<MediaStored, tonic::Status> {
    timeout(
        WAIT,
        client.put_media(request(tokio_stream::iter(frames), token)),
    )
    .await
    .expect("put_media in time")
    .map(tonic::Response::into_inner)
}

async fn put(
    client: &mut Client,
    token: &str,
    header: MediaHeader,
    bytes: &[u8],
) -> Result<MediaStored, tonic::Status> {
    put_frames(
        client,
        token,
        frames(Some(header), bytes, MAX_FILE_CHUNK_BYTES),
    )
    .await
}

async fn server(hub: HubGrpc) -> (Client, CancellationToken, tokio::task::JoinHandle<()>) {
    let pki = testkit::tls::test_pki();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let task = tokio::spawn(async move {
        grpc::serve_with_listener(
            hub,
            listener,
            &pki.server_cert_pem,
            &pki.server_key_pem,
            shutdown.cancelled_owned(),
        )
        .await
        .unwrap();
    });
    let channel: Channel = Endpoint::from_shared(format!("https://{address}"))
        .unwrap()
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(pki.ca_pem))
                .domain_name("localhost"),
        )
        .unwrap()
        .connect_timeout(WAIT)
        .connect()
        .await
        .unwrap();
    (Client::new(channel), stop, task)
}

fn set_mtime(path: &std::path::Path, hours_ago: u64) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(hours_ago * 3600))
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn shared_file_becomes_an_attachment_and_a_block_file(pool: PgPool) {
    let mut f = Media::new(pool).await;
    let (mut client, stop, task) = server(f.hub.clone()).await;
    let block = f.tool_block().await;
    let bytes = b"%PDF-1.7\nshared report".as_slice();
    let id = Uuid::now_v7();
    let header = MediaHeader {
        media_id: id.to_string(),
        execution_id: f.execution.to_string(),
        block_id: block.to_string(),
        mime: "application/pdf".into(),
        size: bytes.len() as u64,
        sha256: sha(bytes),
        name: "report.pdf".into(),
        title: "Отчёт".into(),
        note: "Смотри раздел 2".into(),
        share: true,
        ..Default::default()
    };
    let stored = put(&mut client, &f.token, header, bytes).await.unwrap();
    assert_eq!(stored.media_id, id.to_string());
    let attachment: (String, String, String, i64) =
        sqlx::query_as("select state,name,mime,size_bytes from attachments where id=$1")
            .bind(id)
            .fetch_one(f.pool())
            .await
            .unwrap();
    assert_eq!(
        attachment,
        (
            "ready".into(),
            "report.pdf".into(),
            "application/pdf".into(),
            bytes.len() as i64
        )
    );
    let file: (String, String, String) =
        sqlx::query_as("select name,title,note from block_files where id=$1")
            .bind(id)
            .fetch_one(f.pool())
            .await
            .unwrap();
    assert_eq!(
        file,
        (
            "report.pdf".into(),
            "Отчёт".into(),
            "Смотри раздел 2".into()
        )
    );
    f.committed(vec![Body::BlockFile(BlockFile {
        block_id: block.to_string(),
        attachment_id: id.to_string(),
        name: "report.pdf".into(),
        mime: "application/pdf".into(),
        size: bytes.len() as u64,
        sha256: sha(bytes),
        title: "Отчёт".into(),
        note: "Смотри раздел 2".into(),
        ..Default::default()
    })])
    .await;
    let item = f.history_block(block).await;
    assert_eq!(item["files"][0]["attachment_id"], id.to_string());
    assert_eq!(item["files"][0]["name"], "report.pdf");
    let download = f
        .files
        .client
        .send(
            Method::GET,
            &format!("/api/v1/attachments/{id}/content"),
            None,
            None,
        )
        .await;
    assert_eq!(download.status, 200);
    assert_eq!(download.body, bytes);
    stop.cancel();
    task.await.unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn media_retry_same_checksum_is_idempotent_and_other_checksum_conflicts(pool: PgPool) {
    let mut f = Media::new(pool).await;
    let (mut client, stop, task) = server(f.hub.clone()).await;
    let block = f.tool_block().await;
    let bytes = png(4, 3, 3 * MAX_FILE_CHUNK_BYTES + 5, 1);
    let id = Uuid::now_v7();
    let header = f.header(id, block, &bytes);
    let stored = put(&mut client, &f.token, header.clone(), &bytes)
        .await
        .unwrap();
    assert_eq!(stored.media_id, id.to_string());
    assert_eq!(
        f.files.trace.steps(),
        [
            PublishStep::Verified,
            PublishStep::FileSynced,
            PublishStep::Renamed,
            PublishStep::DirSynced
        ]
    );
    assert_eq!(f.rows(id).await, 1);
    assert_eq!(std::fs::read(f.files.blob_path(id)).unwrap(), bytes);
    assert!(!f.files.staging_path(id).exists());
    let inode = std::fs::metadata(f.files.blob_path(id)).unwrap().ino();

    // Повтор с тем же ID и checksum: тот же ответ, без копирования и второй строки.
    f.files.trace.clear();
    let again = put(&mut client, &f.token, header.clone(), &bytes)
        .await
        .unwrap();
    assert_eq!(again, stored);
    assert!(f.files.trace.steps().is_empty());
    assert_eq!(f.rows(id).await, 1);
    assert_eq!(
        std::fs::metadata(f.files.blob_path(id)).unwrap().ino(),
        inode
    );

    // Другая контрольная сумма или другой блок под тем же ID - конфликт без изменений.
    let other = png(4, 3, 100, 2);
    let error = put(&mut client, &f.token, f.header(id, block, &other), &other)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::AlreadyExists);
    let mut moved = header.clone();
    moved.block_id = Uuid::now_v7().to_string();
    let error = put(&mut client, &f.token, moved, &bytes).await.unwrap_err();
    assert_eq!(error.code(), Code::AlreadyExists);
    let stored_sha: String = sqlx::query_scalar("select sha256 from block_media where id=$1")
        .bind(id)
        .fetch_one(f.pool())
        .await
        .unwrap();
    assert_eq!(stored_sha, sha(&bytes));
    assert_eq!(std::fs::read(f.files.blob_path(id)).unwrap(), bytes);

    // ID чужого пространства (вложения) не принимается.
    let attachment = f
        .files
        .ready("doc.bin", "application/octet-stream", b"document")
        .await;
    let error = put(
        &mut client,
        &f.token,
        f.header(attachment, block, &bytes),
        &bytes,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), Code::AlreadyExists);
    assert_eq!(f.rows(attachment).await, 0);
    stop.cancel();
    timeout(WAIT, task).await.unwrap().unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn crash_after_media_publish_before_commit_leaves_no_reference_and_retry_completes(
    pool: PgPool,
) {
    let mut f = Media::new(pool).await;
    let (mut client, stop, task) = server(f.hub.clone()).await;
    let block = f.tool_block().await;
    let bytes = png(5, 2, 2 * MAX_FILE_CHUNK_BYTES, 3);
    let id = Uuid::now_v7();
    let header = f.header(id, block, &bytes);
    f.files.trace.crash_after(Some(PublishStep::DirSynced));
    let error = put(&mut client, &f.token, header.clone(), &bytes)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Unavailable);
    assert_eq!(f.files.trace.steps().last(), Some(&PublishStep::DirSynced));
    assert!(f.files.blob_path(id).exists());
    assert_eq!(f.rows(id).await, 0);

    // Ссылка на неопубликованные метаданные пропускается, курсор журнала идёт дальше.
    f.committed(vec![Media::reference(id, block, &bytes, 5, 2)])
        .await;
    assert!(!f.attached(id).await);
    assert!(f.files.events("block_media_attached").await.is_empty());

    f.files.trace.crash_after(None);
    f.files.trace.clear();
    put(&mut client, &f.token, header, &bytes).await.unwrap();
    assert_eq!(
        f.files.trace.steps(),
        [PublishStep::Verified, PublishStep::DirSynced],
        "the published blob is verified, not copied again"
    );
    assert_eq!(f.rows(id).await, 1);
    assert!(!f.attached(id).await);
    f.committed(vec![Media::reference(id, block, &bytes, 5, 2)])
        .await;
    assert!(f.attached(id).await);
    let events = f.files.events("block_media_attached").await;
    assert_eq!(
        events,
        [
            json!({"block_id":block,"media_id":id,"mime":"image/png","size":bytes.len(),"width":5,"height":2})
        ]
    );
    // Повтор записи не создаёт второго события.
    f.committed(vec![Media::reference(id, block, &bytes, 5, 2)])
        .await;
    assert_eq!(f.files.events("block_media_attached").await.len(), 1);
    stop.cancel();
    timeout(WAIT, task).await.unwrap().unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn put_media_validates_header_execution_and_content(pool: PgPool) {
    let mut f = Media::new(pool.clone()).await;
    let (mut client, stop, task) = server(f.hub.clone()).await;
    let block = f.tool_block().await;
    let bytes = png(2, 2, 1000, 4);
    let invalid = |change: &dyn Fn(&mut MediaHeader)| {
        let mut header = f.header(Uuid::now_v7(), block, &bytes);
        change(&mut header);
        header
    };
    for header in [
        invalid(&|h| h.mime = "image/svg+xml".into()),
        invalid(&|h| h.mime = "IMAGE/PNG".into()),
        invalid(&|h| h.size = 0),
        invalid(&|h| h.size = MAX_MEDIA_BYTES + 1),
        invalid(&|h| h.sha256 = "zz".into()),
        invalid(&|h| h.media_id = "not-a-uuid".into()),
        invalid(&|h| h.block_id = Uuid::nil().to_string()),
    ] {
        let id = header.media_id.clone();
        let error = put(&mut client, &f.token, header, &bytes)
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument, "{id}");
    }
    // Первый кадр обязан быть заголовком, второй заголовок недопустим.
    let error = put_frames(&mut client, &f.token, frames(None, &bytes, 512))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    let id = Uuid::now_v7();
    let mut twice = frames(Some(f.header(id, block, &bytes)), &bytes, 512);
    twice.insert(1, twice[0].clone());
    let error = put_frames(&mut client, &f.token, twice).await.unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert_eq!(f.rows(id).await, 0);
    // Кадр данных больше файлового чанка протокола.
    let big = png(2, 2, MAX_FILE_CHUNK_BYTES, 5);
    let id = Uuid::now_v7();
    let error = put_frames(
        &mut client,
        &f.token,
        frames(Some(f.header(id, block, &big)), &big, big.len()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);

    // Длина и содержимое сверяются с заголовком; ни строки, ни файлов не остаётся.
    let id = Uuid::now_v7();
    let mut header = f.header(id, block, &bytes);
    header.size += 1;
    let error = put(&mut client, &f.token, header, &bytes)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    let mut header = f.header(id, block, &bytes);
    header.size -= 1;
    let error = put(&mut client, &f.token, header, &bytes)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    let mut header = f.header(id, block, &bytes);
    header.sha256 = sha(b"other");
    let error = put(&mut client, &f.token, header, &bytes)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::DataLoss);
    let html = b"<html><script>alert(1)</script></html>".to_vec();
    let error = put(&mut client, &f.token, f.header(id, block, &html), &html)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert_eq!(f.rows(id).await, 0);
    assert!(!f.files.blob_path(id).exists());
    assert!(!f.files.staging_path(id).exists());

    // Исполнение другой машины неотличимо от отсутствующего.
    let other = testkit::seed(&pool).await;
    let foreign = executions::start(&pool, other.owner_id, other.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    let mut header = f.header(Uuid::now_v7(), block, &bytes);
    header.execution_id = foreign.execution_id.to_string();
    let error = put(&mut client, &f.token, header, &bytes)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::NotFound);
    let mut header = f.header(Uuid::now_v7(), block, &bytes);
    header.execution_id = Uuid::now_v7().to_string();
    let error = put(&mut client, &f.token, header, &bytes)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::NotFound);
    let error = client
        .put_media(Request::new(tokio_stream::iter(frames(
            Some(f.header(Uuid::now_v7(), block, &bytes)),
            &bytes,
            512,
        ))))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Unauthenticated);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from block_media")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    stop.cancel();
    timeout(WAIT, task).await.unwrap().unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn block_media_record_attaches_only_published_media_and_reaches_read_models(pool: PgPool) {
    let mut f = Media::new(pool.clone()).await;
    let (mut client, stop, task) = server(f.hub.clone()).await;
    let block = f.tool_block().await;
    let bytes = png(640, 480, 2000, 6);
    let id = Uuid::now_v7();
    put(&mut client, &f.token, f.header(id, block, &bytes), &bytes)
        .await
        .unwrap();
    assert_eq!(f.history_block(block).await["media"], json!([]));

    // Несовпадение блока, checksum или неизвестный ID: запись пропускается без события.
    let other_block = f.tool_block().await;
    let mut wrong_sha = Media::reference(id, block, &bytes, 640, 480);
    if let Body::BlockMedia(fact) = &mut wrong_sha {
        fact.sha256 = sha(b"other");
    }
    f.committed(vec![
        Media::reference(id, other_block, &bytes, 640, 480),
        wrong_sha,
        Media::reference(Uuid::now_v7(), block, &bytes, 640, 480),
    ])
    .await;
    assert!(!f.attached(id).await);
    assert!(f.files.events("block_media_attached").await.is_empty());

    f.committed(vec![Media::reference(id, block, &bytes, 640, 480)])
        .await;
    let expected =
        json!([{"media_id":id,"mime":"image/png","size":bytes.len(),"width":640,"height":480}]);
    assert_eq!(f.history_block(block).await["media"], expected);
    assert_eq!(f.history_block(other_block).await["media"], json!([]));
    let snapshot: Value = f
        .files
        .client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{}/snapshot", f.files.conversation),
            None,
            None,
        )
        .await
        .json();
    let in_snapshot = snapshot["data"]["history"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == block.to_string())
        .unwrap()
        .clone();
    assert_eq!(in_snapshot["media"], expected);
    // Клиентский вид события виден в диапазоне событий без заглушки.
    let range: Value = f
        .files
        .client
        .send(
            Method::GET,
            &format!(
                "/api/v1/events?scope=conversation:{}&after=0&limit=256",
                f.files.conversation
            ),
            None,
            None,
        )
        .await
        .json();
    let attached: Vec<&Value> = range["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "block_media_attached")
        .collect();
    assert_eq!(attached.len(), 1);
    assert_eq!(attached[0]["payload"]["media_id"], id.to_string());
    assert_eq!(attached[0]["execution_id"], f.execution.to_string());

    // Картинка без размеров отдаётся с null; запись в журнале управления недопустима.
    let plain = png(1, 1, 10, 7);
    let second = Uuid::now_v7();
    put(
        &mut client,
        &f.token,
        f.header(second, block, &plain),
        &plain,
    )
    .await
    .unwrap();
    f.committed(vec![Media::reference(second, block, &plain, 0, 0)])
        .await;
    let media = f.history_block(block).await["media"].clone();
    assert_eq!(media.as_array().unwrap().len(), 2);
    assert_eq!(media[1]["media_id"], second.to_string());
    assert_eq!(media[1]["width"], Value::Null);
    let cj = JournalRecords {
        journal_id: Uuid::now_v7().to_string(),
        records: vec![JournalRecord {
            seq: 1,
            body: Some(RecordBody {
                body: Some(Media::reference(second, block, &plain, 1, 1)),
            }),
        }],
    };
    assert!(
        f.ingest
            .apply_batch(
                f.files.machine,
                &Sha256::digest(f.token.as_bytes()),
                JournalKind::Cj,
                &cj
            )
            .await
            .is_err()
    );
    stop.cancel();
    timeout(WAIT, task).await.unwrap().unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn media_download_has_safe_headers_and_owner_check(pool: PgPool) {
    let mut f = Media::new(pool.clone()).await;
    let (mut client, stop, task) = server(f.hub.clone()).await;
    let block = f.tool_block().await;
    let bytes = png(3, 2, 5000, 8);
    let id = Uuid::now_v7();
    put(&mut client, &f.token, f.header(id, block, &bytes), &bytes)
        .await
        .unwrap();
    // Опубликованная, но не связанная с блоком картинка не выдаётся.
    assert_error(
        &f.download(&f.files.client, id).await,
        404,
        "not_found",
        "never",
    );
    f.committed(vec![Media::reference(id, block, &bytes, 3, 2)])
        .await;
    let response = f.download(&f.files.client, id).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.body.as_ref(), bytes.as_slice());
    let h = &response.headers;
    assert_eq!(h["content-type"], "image/png");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["content-disposition"], "inline");
    assert_eq!(h["etag"], format!("\"{}\"", sha(&bytes)));
    assert_eq!(h["content-security-policy"], "sandbox");
    assert_eq!(h["cache-control"], "private, no-store");
    assert_eq!(h["content-length"], bytes.len().to_string());
    assert!(h.contains_key("x-server-time-ms"));

    let router = canterlot_hub::http::router(f.files.state.clone());
    let stranger = TestClient::in_process(router.clone()).with_header("Remote-User", "attacker");
    assert_error(
        &f.download(&stranger, id).await,
        403,
        "forbidden_user",
        "never",
    );
    let anonymous = TestClient::in_process(router);
    assert_error(
        &f.download(&anonymous, id).await,
        401,
        "unauthenticated",
        "after_condition",
    );
    assert_error(
        &f.download(&f.files.client, Uuid::now_v7()).await,
        404,
        "not_found",
        "never",
    );

    // Картинка разговора другого владельца неотличима от отсутствующей.
    let other = testkit::seed(&pool).await;
    let foreign = executions::start(&pool, other.owner_id, other.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    let foreign_id = Uuid::now_v7();
    sqlx::query("insert into block_media(id,execution_id,conversation_id,block_id,mime,size_bytes,sha256,attached_at,attached_seq) values($1,$2,$3,$4,'image/png',$5,$6,clock_timestamp(),1)")
        .bind(foreign_id).bind(foreign.execution_id).bind(other.conversation_id).bind(Uuid::now_v7())
        .bind(bytes.len() as i64).bind(sha(&bytes)).execute(&pool).await.unwrap();
    std::fs::create_dir_all(f.files.blob_path(foreign_id).parent().unwrap()).unwrap();
    std::fs::copy(f.files.blob_path(id), f.files.blob_path(foreign_id)).unwrap();
    assert_error(
        &f.download(&f.files.client, foreign_id).await,
        404,
        "not_found",
        "never",
    );

    // Пропавший blob даёт тот же 404, а не 500.
    std::fs::remove_file(f.files.blob_path(id)).unwrap();
    assert_error(
        &f.download(&f.files.client, id).await,
        404,
        "not_found",
        "never",
    );
    stop.cancel();
    timeout(WAIT, task).await.unwrap().unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn gc_keeps_referenced_media_and_removes_unreferenced_after_grace(pool: PgPool) {
    let mut f = Media::new(pool.clone()).await;
    let (mut client, stop, task) = server(f.hub.clone()).await;
    let block = f.tool_block().await;
    let mut ids = Vec::new();
    for seed in 0..3 {
        let bytes = png(1, 1, 300, seed);
        let id = Uuid::now_v7();
        put(&mut client, &f.token, f.header(id, block, &bytes), &bytes)
            .await
            .unwrap();
        ids.push((id, bytes));
    }
    let (attached, ref bytes) = ids[0];
    f.committed(vec![Media::reference(attached, block, bytes, 1, 1)])
        .await;
    let (stale, _) = ids[1];
    let (fresh, _) = ids[2];
    let orphan = Uuid::now_v7();
    std::fs::create_dir_all(f.files.blob_path(orphan).parent().unwrap()).unwrap();
    std::fs::write(f.files.blob_path(orphan), b"orphan").unwrap();
    for id in [attached, stale, orphan] {
        set_mtime(&f.files.blob_path(id), 30);
    }
    sqlx::query("update block_media set created_at=clock_timestamp()-interval '30 hours' where id = any($1)")
        .bind(vec![attached, stale])
        .execute(&pool)
        .await
        .unwrap();
    let report = gc::run_once(&pool, f.files.blobs(), Utc::now(), DAY)
        .await
        .unwrap();
    assert_eq!(report.unreferenced_media, 1);
    assert_eq!(report.orphan_blobs, 1);
    assert!(f.files.blob_path(attached).exists());
    assert!(f.attached(attached).await);
    assert!(!f.files.blob_path(stale).exists());
    assert_eq!(f.rows(stale).await, 0);
    assert!(f.files.blob_path(fresh).exists());
    assert_eq!(f.rows(fresh).await, 1);
    assert!(!f.files.blob_path(orphan).exists());
    // Поздняя ссылка на удалённую картинку пропускается.
    let (_, ref stale_bytes) = ids[1];
    f.committed(vec![Media::reference(stale, block, stale_bytes, 1, 1)])
        .await;
    assert!(!f.attached(stale).await);
    stop.cancel();
    timeout(WAIT, task).await.unwrap().unwrap();
}

#[cfg(feature = "fault-injection")]
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn media_uploads_are_limited_per_machine_and_one_body_per_id(pool: PgPool) {
    use canterlot_hub::fault_injection::{Faults, Point};
    let mut f = Media::new(pool).await;
    let faults = Faults::default();
    f.hub.faults = faults.clone();
    let (client, stop, task) = server(f.hub.clone()).await;
    let block = f.tool_block().await;
    let bytes = png(1, 1, 3 * MAX_FILE_CHUNK_BYTES, 9);
    let mut held = Vec::new();
    let mut uploads = Vec::new();
    let first_id = Uuid::now_v7();
    for n in 0..4 {
        let pause = faults.pause_next(Point::BeforeMediaPublish);
        let id = if n == 0 { first_id } else { Uuid::now_v7() };
        let header = f.header(id, block, &bytes);
        let (mut c, token, data) = (client.clone(), f.token.clone(), bytes.clone());
        uploads.push(tokio::spawn(async move {
            put(&mut c, &token, header, &data).await
        }));
        held.push(timeout(WAIT, pause).await.unwrap().unwrap());
    }
    let mut c = client.clone();
    let error = put(
        &mut c,
        &f.token,
        f.header(Uuid::now_v7(), block, &bytes),
        &bytes,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), Code::ResourceExhausted);
    // Пока первое тело не опубликовано, второе с тем же ID получает транспортный отказ.
    held.pop().unwrap().send(()).unwrap();
    timeout(WAIT, uploads.pop().unwrap())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let error = put(&mut c, &f.token, f.header(first_id, block, &bytes), &bytes)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Unavailable);
    for release in held {
        release.send(()).unwrap();
    }
    for upload in uploads {
        timeout(WAIT, upload).await.unwrap().unwrap().unwrap();
    }
    assert_eq!(f.rows(first_id).await, 1);
    stop.cancel();
    timeout(WAIT, task).await.unwrap().unwrap();
}
