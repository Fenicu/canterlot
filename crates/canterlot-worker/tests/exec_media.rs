#![forbid(unsafe_code)]
#![cfg(all(feature = "synthetic-adapter", feature = "fault-injection"))]
//! Картинки, на которые смотрел агент (pilot.md §5.7): настоящий хаб, управляющая служба,
//! сопровождение и синтетический CLI; загрузка PutMedia и ссылка BlockMedia в SJ.

#[path = "support/attachments.rs"]
mod fixture;
#[allow(dead_code)]
mod support;

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use canterlot_hub::{
    attachments::{PublishStep, PublishTrace},
    fault_injection::Point,
    testkit,
};
use canterlot_proto::{
    limits::MAX_MEDIA_BYTES,
    worker::v1::{StreamKind, record_body::Body},
};
use chrono::{DateTime, Utc};
use fixture::{Fixture, WAIT, sha, until};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

/// PNG с заголовком IHDR и детерминированным хвостом.
fn png(width: u32, height: u32, tail: usize) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend((0..tail).map(|i| (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[3]));
    bytes
}

fn script(steps: Value) -> String {
    json!({ "synthetic": steps }).to_string()
}

#[derive(Default)]
struct Steps(Mutex<Vec<PublishStep>>);

impl PublishTrace for Steps {
    fn step(&self, step: PublishStep) -> Result<(), ()> {
        self.0.lock().unwrap().push(step);
        Ok(())
    }
}

fn image(f: &Fixture, name: &str, bytes: &[u8]) -> PathBuf {
    let directory = f.root.path().join("images");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// Записи BlockMedia, дошедшие до хаба: различные (журнал, seq) по всем хабам фикстуры.
fn shipped_media(f: &Fixture) -> usize {
    let kind = std::mem::discriminant(&Body::BlockMedia(Default::default()));
    f.hub()
        .faults
        .batches()
        .iter()
        .flat_map(|b| b.records.iter().map(move |r| (b.journal_id.clone(), r)))
        .filter(|(_, r)| r.kind == Some(kind))
        .map(|(journal, r)| (journal, r.seq))
        .collect::<HashSet<_>>()
        .len()
}

#[derive(Debug, sqlx::FromRow)]
struct Row {
    id: Uuid,
    block_id: Uuid,
    attached: bool,
    created_at: DateTime<Utc>,
    attached_at: Option<DateTime<Utc>>,
}

async fn rows(f: &Fixture) -> Vec<Row> {
    sqlx::query_as("select id, block_id, attached_at is not null attached, created_at, attached_at from block_media where execution_id=$1 order by created_at")
        .bind(f.execution)
        .fetch_all(&f.pool)
        .await
        .unwrap()
}

async fn attached(f: &Fixture, count: usize) {
    until(async || rows(f).await.iter().filter(|r| r.attached).count() == count).await;
}

async fn events(f: &Fixture) -> Vec<Value> {
    sqlx::query_scalar("select payload from events where scope=$1 and kind='block_media_attached' order by scope_seq")
        .bind(format!("conversation:{}", f.conversation))
        .fetch_all(&f.pool)
        .await
        .unwrap()
}

async fn tool_block(f: &Fixture, summary: &str) -> Uuid {
    let mut found = None;
    until(async || {
        found = sqlx::query_scalar(
            "select block_id from blocks where execution_id=$1 and kind='tool_call' and summary=$2",
        )
        .bind(f.execution)
        .bind(summary)
        .fetch_optional(&f.pool)
        .await
        .unwrap();
        found.is_some()
    })
    .await;
    found.unwrap()
}

async fn final_text(f: &Fixture, text: &str) {
    until(async || {
        sqlx::query_scalar::<_, bool>(
            "select exists(select 1 from blocks where execution_id=$1 and state='final' and content=$2)",
        )
        .bind(f.execution)
        .bind(text)
        .fetch_one(&f.pool)
        .await
        .unwrap()
    })
    .await;
}

fn files_in(path: &Path) -> Vec<PathBuf> {
    match fs::read_dir(path) {
        Ok(entries) => entries.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
}

fn hub_blob(f: &Fixture, id: Uuid) -> PathBuf {
    let name = id.to_string();
    f.blob_config
        .blob_dir
        .join("blobs")
        .join(&name[name.len() - 2..])
        .join(name)
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn media_is_referenced_only_after_durable_publish(pool: PgPool) {
    let steps = Arc::new(Steps::default());
    let f = Fixture::with_trace(&pool, steps.clone()).await;
    let bytes = png(16, 9, 600_000);
    let path = image(&f, "screen.png", &bytes);
    let path = path.to_str().unwrap();
    let pause = f.hub().faults.pause_next(Point::BeforeMediaPublish);
    let input = f
        .submit(
            &[],
            &script(json!([{"view_image": path}, {"delay_ms": 50}, {"delta": "после картинки"}])),
        )
        .await;
    let release = timeout(WAIT, pause).await.unwrap().unwrap();
    f.accepted(&input).await;
    // Ход и поток SJ идут дальше, пока хаб держит тело картинки до публикации.
    final_text(&f, "после картинки").await;
    let block = tool_block(&f, path).await;
    sleep(Duration::from_millis(300)).await;
    assert!(steps.0.lock().unwrap().is_empty());
    assert!(rows(&f).await.is_empty());
    assert_eq!(shipped_media(&f), 0);
    assert!(files_in(&f.blob_config.blob_dir.join("blobs")).is_empty());
    let copies = files_in(&f.exec_dir().join("media"));
    assert_eq!(copies.len(), 1, "the private copy waits on the worker");
    assert_eq!(fs::read(&copies[0]).unwrap(), bytes);

    release.send(()).unwrap();
    attached(&f, 1).await;
    let row = rows(&f).await.remove(0);
    assert_eq!(row.block_id, block);
    assert_eq!(
        *steps.0.lock().unwrap(),
        [
            PublishStep::Verified,
            PublishStep::FileSynced,
            PublishStep::Renamed,
            PublishStep::DirSynced
        ]
    );
    // Метаданные зафиксированы до ссылки, ссылка пришла записью сопровождения после ответа хаба.
    assert!(row.created_at <= row.attached_at.unwrap());
    assert_eq!(fs::read(hub_blob(&f, row.id)).unwrap(), bytes);
    assert_eq!(shipped_media(&f), 1);
    assert_eq!(
        events(&f).await,
        [
            json!({"block_id":block,"media_id":row.id,"mime":"image/png","size":bytes.len(),"width":16,"height":9})
        ]
    );
    until(async || files_in(&f.exec_dir().join("media")).is_empty()).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn shared_file_is_published_with_the_supervisor_block_id(pool: PgPool) {
    // Сквозной путь вызова share_file: загрузка несёт block_id записи BlockFile,
    // иначе хаб отвергает её без UUID.
    let f = Fixture::new(&pool).await;
    let bytes = b"owner document";
    let path = f.root.path().join("doc.txt");
    fs::write(&path, bytes).unwrap();
    let input = f
        .submit(
            &[],
            &script(json!([
                {"share_file": {"path": path.to_str().unwrap(), "title": "Doc", "note": "see attached"}},
                {"delta": "файл ушёл"},
            ])),
        )
        .await;
    f.accepted(&input).await;
    final_text(&f, "файл ушёл").await;
    let mut found = None;
    until(async || {
        found = sqlx::query_as::<_, (Uuid, Uuid)>(
            "select id, block_id from block_files where execution_id=$1 and attached_at is not null",
        )
        .bind(f.execution)
        .fetch_optional(&f.pool)
        .await
        .unwrap();
        found.is_some()
    })
    .await;
    let (attachment, block) = found.unwrap();
    assert_eq!(block, tool_block(&f, "Doc").await);
    // Блок-носитель стоит в ходе входа под меткой основного агента: лента не
    // открывает для него отдельное сообщение.
    let (turn, agent): (Option<Uuid>, String) =
        sqlx::query_as("select input_command_id, agent from blocks where block_id=$1")
            .bind(block)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(turn, Some(input.command_id));
    assert_eq!(agent, "orchestrator");
    let row: (String, String, i64, String, String, String) = sqlx::query_as(
        "select name, mime, size_bytes, sha256, title, note from block_files where id=$1",
    )
    .bind(attachment)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            "doc.txt".to_owned(),
            "text/plain".to_owned(),
            bytes.len() as i64,
            sha(bytes),
            "Doc".to_owned(),
            "see attached".to_owned(),
        )
    );
    assert_eq!(fs::read(hub_blob(&f, attachment)).unwrap(), bytes);
    let payload: Value = sqlx::query_scalar(
        "select payload from events where scope=$1 and kind='block_file_attached' order by scope_seq",
    )
    .bind(format!("conversation:{}", f.conversation))
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(payload["block_id"], json!(block));
    assert_eq!(payload["attachment_id"], json!(attachment));
    until(async || files_in(&f.exec_dir().join("media")).is_empty()).await;
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn media_upload_resumes_after_reconnect_without_duplicate_record(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    let bytes = png(8, 6, 1_000_000);
    let path = image(&f, "resume.png", &bytes);
    let first = f.hub().faults.pause_next(Point::BeforeMediaChunk);
    let second = f.hub().faults.pause_next(Point::BeforeMediaChunk);
    let input = f.submit(&[], &script(json!([{"view_image": path}]))).await;
    timeout(WAIT, first)
        .await
        .unwrap()
        .unwrap()
        .send(())
        .unwrap();
    let held = timeout(WAIT, second).await.unwrap().unwrap();
    f.accepted(&input).await;
    // Перезапуск управляющей службы посреди загрузки: сопровождение повторяет запрос новой службе.
    f.stop_worker().await;
    // Соединение воркера закрыто: хаб снимает обработчик прерванной загрузки вместе с её паузой.
    // Отпускать паузу нечего, ждать нужно именно этого, иначе send гонится с закрытием потока.
    until(async || held.is_closed()).await;
    sleep(Duration::from_millis(200)).await;
    assert!(rows(&f).await.is_empty());
    assert_eq!(shipped_media(&f), 0);
    f.start_worker().await;
    attached(&f, 1).await;
    sleep(Duration::from_millis(300)).await;
    let rows = rows(&f).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(fs::read(hub_blob(&f, rows[0].id)).unwrap(), bytes);
    assert_eq!(shipped_media(&f), 1);
    assert_eq!(events(&f).await.len(), 1);
    assert!(files_in(&f.blob_config.blob_dir.join("staging")).is_empty());
    assert_eq!(f.transmissions(input.command_id), 1);
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn hub_restart_during_media_upload_completes_after_reconnect(pool: PgPool) {
    let mut f = Fixture::new(&pool).await;
    let bytes = png(30, 20, 1_500_000);
    let path = image(&f, "restart.png", &bytes);
    let first = f.hub().faults.pause_next(Point::BeforeMediaChunk);
    let second = f.hub().faults.pause_next(Point::BeforeMediaChunk);
    let input = f
        .submit(
            &[],
            &script(json!([{"view_image": path}, {"delta": "ответ дошёл"}])),
        )
        .await;
    timeout(WAIT, first)
        .await
        .unwrap()
        .unwrap()
        .send(())
        .unwrap();
    let _held = timeout(WAIT, second).await.unwrap().unwrap();
    f.accepted(&input).await;
    final_text(&f, "ответ дошёл").await;
    until(async || !files_in(&f.blob_config.blob_dir.join("staging")).is_empty()).await;
    let hub = f.hub.take().unwrap();
    let addr = hub.addr;
    let pki = hub.pki.clone();
    let faults = hub.faults.clone();
    hub.shutdown.send(()).unwrap();
    timeout(WAIT, hub.join).await.unwrap().unwrap().unwrap();
    sleep(Duration::from_millis(1500)).await;
    assert!(rows(&f).await.is_empty());
    let copies = files_in(&f.exec_dir().join("media"));
    assert_eq!(copies.len(), 1, "the copy survives the hub outage");
    f.hub = Some(
        testkit::spawn_test_hub_with_attachment_store(
            pool.clone(),
            Default::default(),
            Some(addr),
            pki,
            faults,
            (f.blob_config.clone(), f.blobs.clone()),
        )
        .await,
    );
    attached(&f, 1).await;
    sleep(Duration::from_millis(300)).await;
    let rows = rows(&f).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(fs::read(hub_blob(&f, rows[0].id)).unwrap(), bytes);
    assert_eq!(shipped_media(&f), 1);
    assert_eq!(events(&f).await.len(), 1);
    assert!(files_in(&f.blob_config.blob_dir.join("staging")).is_empty());
    until(async || files_in(&f.exec_dir().join("media")).is_empty()).await;
    assert_eq!(f.transmissions(input.command_id), 1);
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn oversized_or_non_image_media_is_skipped_with_path_kept(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let big = image(&f, "huge.png", &png(4, 4, 0));
    fs::OpenOptions::new()
        .write(true)
        .open(&big)
        .unwrap()
        .set_len(MAX_MEDIA_BYTES + 1)
        .unwrap();
    let text = image(&f, "notes.txt", b"plain text, not an image");
    let small = png(2, 3, 100);
    let fine = image(&f, "fine.png", &small);
    let paths: Vec<_> = [&big, &text, &fine]
        .iter()
        .map(|p| p.to_str().unwrap().to_owned())
        .collect();
    let input = f
        .submit(
            &[],
            &script(json!([
                {"view_image": paths[0]},
                {"view_image": paths[1]},
                {"view_image": {"path": paths[2], "inline": true}},
            ])),
        )
        .await;
    f.accepted(&input).await;
    attached(&f, 1).await;
    sleep(Duration::from_millis(300)).await;
    for (path, size) in [
        (&paths[0], MAX_MEDIA_BYTES + 1),
        (&paths[1], 24),
        (&paths[2], small.len() as u64),
    ] {
        let block = tool_block(&f, path).await;
        let result: String = sqlx::query_scalar(
            "select content from blocks where parent_block_id=$1 and kind='tool_result'",
        )
        .bind(block)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(result, format!("{size} bytes"), "{path}");
    }
    let rows = rows(&f).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].block_id, tool_block(&f, &paths[2]).await);
    assert_eq!(shipped_media(&f), 1);
    assert_eq!(events(&f).await[0]["width"], 2);
    assert_eq!(events(&f).await[0]["height"], 3);
    assert!(files_in(&f.exec_dir().join("media")).is_empty());
    assert_eq!(
        sqlx::query_scalar::<_, String>("select sha256 from block_media where id=$1")
            .bind(rows[0].id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        sha(&small)
    );
}

#[sqlx::test(migrations = "../canterlot-hub/migrations")]
async fn media_upload_does_not_starve_control_or_sj(pool: PgPool) {
    let f = Fixture::new(&pool).await;
    let bytes = png(64, 48, 16 * 1024 * 1024);
    let path = image(&f, "large.png", &bytes);
    let first = f.hub().faults.pause_next(Point::BeforeMediaChunk);
    let second = f.hub().faults.pause_next(Point::BeforeMediaChunk);
    let viewed = f.submit(&[], &script(json!([{"view_image": path}]))).await;
    timeout(WAIT, first)
        .await
        .unwrap()
        .unwrap()
        .send(())
        .unwrap();
    let release = timeout(WAIT, second).await.unwrap().unwrap();
    f.accepted(&viewed).await;
    // Остановленный приём держит окна HTTP/2 своего потока заполненными; следующий ход
    // передаётся и подтверждается через SJ, пока картинка стоит.
    sleep(Duration::from_millis(500)).await;
    let input = f
        .submit(&[], &script(json!([{"delta": "второй ход"}])))
        .await;
    f.accepted(&input).await;
    final_text(&f, "второй ход").await;
    let before = f.probe.heartbeat_acks().len();
    sleep(Duration::from_millis(350)).await;
    assert!(f.probe.heartbeat_acks().len() > before);
    let at = Instant::now();
    let interrupt = Uuid::now_v7();
    sqlx::query("insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state) select $1,c.machine_id,c.id,e.id,e.generation,'interrupt',jsonb_build_object('execution_id',e.id,'execution_generation',e.generation),'pending' from conversations c join executions e on e.conversation_id=c.id where e.id=$2").bind(interrupt).bind(f.execution).execute(&pool).await.unwrap();
    f.hub().dispatcher.trigger(f.conversation);
    timeout(Duration::from_secs(5),until(async ||sqlx::query_scalar::<_,bool>("select delivery_state in ('stored_on_machine','completed') from commands where id=$1").bind(interrupt).fetch_one(&pool).await.unwrap())).await.unwrap();
    assert!(at.elapsed() < Duration::from_secs(5));
    assert!(rows(&f).await.is_empty());
    release.send(()).unwrap();
    attached(&f, 1).await;
    let row = rows(&f).await.remove(0);
    assert_eq!(
        fs::metadata(hub_blob(&f, row.id)).unwrap().len(),
        bytes.len() as u64
    );
    assert!(
        f.probe
            .heartbeat_acks()
            .iter()
            .all(|d| *d < f.config.timers.heartbeat_ack)
    );
    assert_eq!(
        f.hub()
            .faults
            .hellos()
            .iter()
            .filter(|h| h.stream_kind == StreamKind::Control as i32)
            .count(),
        1
    );
    assert_eq!(f.transmissions(viewed.command_id), 1);
    assert_eq!(f.transmissions(input.command_id), 1);
}
