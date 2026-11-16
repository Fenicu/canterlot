//! Пуши владельцу через Gotify: постановка в транзакции события, отправка фоновой
//! службой на поддельный сервер Gotify на 127.0.0.1.

use std::{
    collections::VecDeque,
    fs,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    routing::post,
};
use canterlot_hub::{
    activity::{self, Evidence, Thresholds},
    attachments::{self, LocalBlobStore},
    config::{ActivityConfig, AttachmentsConfig, GotifyConfig, HubConfig},
    deletion,
    delivery::{self, ContentBlock, SubmitInput},
    executions,
    ingest::{Ingest, IngestOutcome, JournalKind},
    model::ExecutionState,
    notify::{self, PushSettings, Sender},
    presence::{Presence, TTL},
    session_tools::settle::{SettleFacts, Settled},
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::{
    JournalOpened, JournalRecord, JournalRecords, RecordBody, RequestOpened, record_body,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::time::Instant;
use uuid::Uuid;

const TOKEN: &str = "AbCdEf-gotify-SECRET-token_123";
const APP: &str = "https://app.example.test";

// ---------- поддельный Gotify ----------

#[derive(Clone, Debug)]
struct Captured {
    path: String,
    query: Option<String>,
    key: Option<String>,
    body: Value,
    attempts: Option<i32>,
    retry_scheduled: Option<bool>,
}

#[derive(Clone, Default)]
struct Fake {
    seen: Arc<Mutex<Vec<Captured>>>,
    script: Arc<Mutex<VecDeque<u16>>>,
    probe: Arc<Mutex<Option<PgPool>>>,
}

impl Fake {
    fn seen(&self) -> Vec<Captured> {
        self.seen.lock().unwrap().clone()
    }
    fn script(&self, statuses: &[u16]) {
        self.script.lock().unwrap().extend(statuses);
    }
    /// Строка очереди на момент запроса: попытка обязана быть уже учтена.
    fn probe(&self, pool: &PgPool) {
        *self.probe.lock().unwrap() = Some(pool.clone());
    }
}

async fn message(
    State(fake): State<Fake>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let probe = fake.probe.lock().unwrap().clone();
    let (attempts, retry_scheduled) = match probe {
        Some(pool) => sqlx::query_as::<_, (i32, bool)>(
            "select attempts, run_at > clock_timestamp() from owner_pushes limit 1",
        )
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .map_or((None, None), |(attempts, later)| {
            (Some(attempts), Some(later))
        }),
        None => (None, None),
    };
    fake.seen.lock().unwrap().push(Captured {
        path: uri.path().to_owned(),
        query: uri.query().map(str::to_owned),
        key: headers
            .get("x-gotify-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
        attempts,
        retry_scheduled,
    });
    let status = fake.script.lock().unwrap().pop_front().unwrap_or(200);
    StatusCode::from_u16(status).unwrap()
}

/// Сервер живёт до конца теста: задача уходит вместе с рантаймом теста.
async fn fake_gotify() -> (Fake, String) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/gotify/message", post(message))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (fake, format!("http://{addr}/gotify/"))
}

// ---------- конфиг ----------

const BASE: &str = "database_url_file = 'db'\ngrpc_listen = '127.0.0.1:7443'\ntls_cert = 'cert'\ntls_key = 'key'\nmanifest = 'manifest'\n";

fn load(extra: &str, token: Option<&str>) -> (tempfile::TempDir, Result<HubConfig, String>) {
    let dir = tempfile::tempdir().unwrap();
    if let Some(token) = token {
        fs::write(dir.path().join("gotify-token"), token).unwrap();
    }
    let path = dir.path().join("hub.toml");
    fs::write(&path, format!("{BASE}{extra}")).unwrap();
    let result = HubConfig::load(&path).map_err(|e| e.to_string());
    (dir, result)
}

fn gotify(url: &str, extra: &str) -> (tempfile::TempDir, GotifyConfig) {
    let (dir, config) = load(
        &format!(
            "[notify.gotify]\nurl = '{url}'\ntoken_file = 'gotify-token'\napp_url = '{APP}/'\n{extra}"
        ),
        Some(&format!("{TOKEN}\n")),
    );
    let config = config.unwrap().notify.gotify.expect("gotify section");
    (dir, config)
}

async fn enable(pool: &PgPool) {
    notify::configure(
        pool,
        Some(&PushSettings {
            waiting: true,
            turn_finished: true,
            failures: true,
            timezone: "UTC".into(),
        }),
    )
    .await
    .unwrap();
}

// ---------- журнал логов ----------

static BUFFER: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
struct Capture;
impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        BUFFER
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        Capture
    }
}
fn capture_logs() {
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(Capture)
        .try_init();
}
fn captured_logs() -> String {
    String::from_utf8(
        BUFFER
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .clone(),
    )
    .unwrap()
}

// ---------- сессии ----------

fn thresholds() -> Thresholds {
    Thresholds::from(&ActivityConfig::default())
}

async fn refresh_at(pool: &PgPool, id: Uuid, now: DateTime<Utc>) {
    activity::refresh(pool, id, now, &thresholds(), Uuid::now_v7())
        .await
        .unwrap();
}

async fn seed(pool: &PgPool) -> Seed {
    let s = testkit::seed(pool).await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(s.machine_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("update conversations set title='Сборка\nрелиза' where id=$1")
        .bind(s.conversation_id)
        .execute(pool)
        .await
        .unwrap();
    s
}

async fn delegate_of(pool: &PgPool, s: &Seed) -> Uuid {
    let id = testkit::seed_conversation(pool, s.owner_id, s.machine_id).await;
    sqlx::query("update conversations set parent_conversation_id=$2,title='Делегат' where id=$1")
        .bind(id)
        .bind(s.conversation_id)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn start(pool: &PgPool, owner: Uuid, conversation: Uuid) -> executions::StartedExecution {
    let execution = executions::start(pool, owner, conversation, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    execution
}

async fn finish_turn(
    pool: &PgPool,
    machine: Uuid,
    conversation: Uuid,
    execution: &executions::StartedExecution,
    at: DateTime<Utc>,
) {
    let command = Uuid::now_v7();
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,$5,'deliver_input','{}','completed')",
    )
    .bind(command)
    .bind(machine)
    .bind(conversation)
    .bind(execution.execution_id)
    .bind(execution.generation)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at)
         values($1,$2,$3,$4,$4)",
    )
    .bind(command)
    .bind(execution.execution_id)
    .bind(conversation)
    .bind(at)
    .execute(pool)
    .await
    .unwrap();
}

/// Ввод через обычный приём: ввод владельца (`origin` null) или служебное событие.
async fn input(
    pool: &PgPool,
    seed: &Seed,
    conversation: Uuid,
    key: &str,
    text: &str,
    origin: Option<Value>,
    at: DateTime<Utc>,
) -> delivery::SubmittedInput {
    let submitted = delivery::submit_input(
        pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: conversation,
            idempotency_key: key.to_owned(),
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            received_at: at,
            correlation_id: Uuid::now_v7(),
            origin,
        },
    )
    .await
    .unwrap();
    // Ход по этому вводу уже принят: очередь его не держит, как и в жизни после выдачи.
    sqlx::query("update inputs set state='accepted' where id=$1")
        .bind(submitted.input_id)
        .execute(pool)
        .await
        .unwrap();
    submitted
}

/// Ход по уже принятому вводу: его команда и есть начало хода.
async fn finish_submitted_turn(
    pool: &PgPool,
    conversation: Uuid,
    execution: &executions::StartedExecution,
    command: Uuid,
    at: DateTime<Utc>,
) {
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at)
         values($1,$2,$3,$4,$4)",
    )
    .bind(command)
    .bind(execution.execution_id)
    .bind(conversation)
    .bind(at)
    .execute(pool)
    .await
    .unwrap();
}

fn event_origin(kind: &str) -> Value {
    json!({"kind":"event","chain_depth":0,"event":{"kind":kind}})
}

async fn child_of(pool: &PgPool, seed: &Seed, parent: Uuid, state: &str) -> Uuid {
    let child = testkit::seed_conversation(pool, seed.owner_id, seed.machine_id).await;
    sqlx::query("update conversations set parent_conversation_id=$2,title='Делегат' where id=$1")
        .bind(child)
        .bind(parent)
        .execute(pool)
        .await
        .unwrap();
    set_child_state(pool, child, state).await;
    child
}

async fn set_child_state(pool: &PgPool, child: Uuid, state: &str) {
    sqlx::query(
        "insert into conversation_derived(conversation_id,activity_state,activity_evidence,activity_since)
         values($1,$2,'{}',clock_timestamp())
         on conflict(conversation_id) do update set activity_state=excluded.activity_state,
           activity_evidence=excluded.activity_evidence,activity_since=excluded.activity_since,
           updated_at=clock_timestamp()",
    )
    .bind(child)
    .bind(state)
    .execute(pool)
    .await
    .unwrap();
}

async fn owner_mark(pool: &PgPool, conversation: Uuid) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("select owner_waiting_since from conversations where id=$1")
        .bind(conversation)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Разговор давно простаивает: окно тишины уже прошло.
async fn long_idle(pool: &PgPool, id: Uuid, now: DateTime<Utc>) {
    sqlx::query("update executions set last_event_at=$2 where conversation_id=$1")
        .bind(id)
        .bind(now - TimeDelta::hours(1))
        .execute(pool)
        .await
        .unwrap();
    refresh_at(pool, id, now - TimeDelta::hours(1)).await;
}

async fn settle(
    pool: &PgPool,
    conversation: Uuid,
    state: Settled,
    evidence: &Evidence,
    now: DateTime<Utc>,
    fingerprint: &str,
) {
    let facts = SettleFacts {
        now,
        since: now - TimeDelta::seconds(5),
        last_turn_at: None,
        last_event_at: None,
        phase_since: None,
    };
    let mut tx = pool.begin().await.unwrap();
    notify::on_settled_in_tx(&mut tx, conversation, state, fingerprint, &facts, evidence)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn failure_pushes(pool: &PgPool, conversation: Uuid) -> i64 {
    sqlx::query_scalar(
        "select count(*) from owner_pushes where conversation_id=$1 and kind='failure'",
    )
    .bind(conversation)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn question_batch(
    execution: &executions::StartedExecution,
    request: Uuid,
    text: &str,
) -> JournalRecords {
    question_batch_with(
        execution,
        request,
        &json!({"questions": [{"id": "q1", "text": text, "is_secret": true}]}),
    )
}

fn question_batch_with(
    execution: &executions::StartedExecution,
    request: Uuid,
    payload: &Value,
) -> JournalRecords {
    let bodies = [
        record_body::Body::JournalOpened(JournalOpened {
            execution_id: execution.execution_id.to_string(),
            generation: execution.generation as u64,
            supervisor_protocol_version: 1,
        }),
        record_body::Body::RequestOpened(RequestOpened {
            request_id: request.to_string(),
            execution_id: execution.execution_id.to_string(),
            generation: execution.generation as u64,
            kind: "question".into(),
            payload_json: serde_json::to_vec(payload).unwrap(),
        }),
    ];
    JournalRecords {
        journal_id: Uuid::now_v7().to_string(),
        records: bodies
            .into_iter()
            .enumerate()
            .map(|(i, body)| JournalRecord {
                seq: i as u64 + 1,
                body: Some(RecordBody { body: Some(body) }),
            })
            .collect(),
    }
}

async fn open_question(pool: &PgPool, s: &Seed, records: &JournalRecords) {
    let outcome = Ingest::new(pool.clone())
        .apply_batch(
            s.machine_id,
            &s.credential_digest(),
            JournalKind::Sj,
            records,
        )
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Committed { up_to_seq: 2 });
}

async fn pushes(pool: &PgPool) -> Vec<(Uuid, String, String)> {
    sqlx::query_as("select conversation_id,kind,state from owner_pushes order by id")
        .fetch_all(pool)
        .await
        .unwrap()
}

fn sender(config: &GotifyConfig) -> Sender {
    Sender::new(config)
        .unwrap()
        .with_backoff(vec![Duration::from_millis(1), Duration::from_millis(1)])
}

fn presence_sender(config: &GotifyConfig, presence: &Presence) -> Sender {
    sender(config).with_presence(presence.clone())
}

// ---------- тесты ----------

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_question_of_any_session_is_pushed_once_with_the_key_in_the_header(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let delegate = delegate_of(&pool, &s).await;
    let execution = start(&pool, s.owner_id, delegate).await;
    let request = Uuid::now_v7();
    let records = question_batch(&execution, request, "Выкатить   на прод?");
    open_question(&pool, &s, &records).await;
    // Повтор того же пакета журнала пуша не повторяет.
    let _ = Ingest::new(pool.clone())
        .apply_batch(
            s.machine_id,
            &s.credential_digest(),
            JournalKind::Sj,
            &records,
        )
        .await;

    let report = sender(&config).deliver_due(&pool).await.unwrap();
    assert_eq!(report.sent, 1);
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    let push = &seen[0];
    assert_eq!(push.path, "/gotify/message");
    assert_eq!(push.query, None, "the key never goes into the URL");
    assert_eq!(push.key.as_deref(), Some(TOKEN));
    assert_eq!(push.body["title"], "Делегат");
    assert_eq!(push.body["message"], "Ждёт ответа: Выкатить на прод?");
    assert_eq!(push.body["priority"], 8);
    assert_eq!(
        push.body["extras"]["client::display"]["contentType"],
        "text/plain"
    );
    assert_eq!(
        push.body["extras"]["client::notification"]["click"]["url"],
        format!("{APP}/c/{delegate}")
    );
    assert_eq!(
        pushes(&pool).await,
        [(delegate, "waiting".into(), "sent".into())]
    );
    assert_eq!(sender(&config).deliver_due(&pool).await.unwrap().sent, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_question_in_another_payload_shape_keeps_its_text(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch_with(
            &execution,
            Uuid::now_v7(),
            &json!({"ask_question": {"questions": ["Выкатить?"]}}),
        ),
    )
    .await;
    assert_eq!(sender(&config).deliver_due(&pool).await.unwrap().sent, 1);
    assert_eq!(fake.seen()[0].body["message"], "Ждёт ответа: Выкатить?");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_question_answered_before_delivery_is_not_pushed(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;
    sqlx::query("update requests set state='decision_applied'")
        .execute(&pool)
        .await
        .unwrap();
    let report = sender(&config).deliver_due(&pool).await.unwrap();
    assert_eq!((report.sent, report.cancelled), (0, 1));
    assert!(fake.seen().is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_finished_turn_of_a_root_session_is_pushed_once_and_a_delegate_turn_is_not(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let delegate = delegate_of(&pool, &s).await;
    let now = Utc::now();
    for conversation in [s.conversation_id, delegate] {
        let execution = start(&pool, s.owner_id, conversation).await;
        long_idle(&pool, conversation, now).await;
        finish_turn(
            &pool,
            s.machine_id,
            conversation,
            &execution,
            now - TimeDelta::seconds(1),
        )
        .await;
        refresh_at(&pool, conversation, now).await;
    }
    assert!(pushes(&pool).await.is_empty(), "quiet window");
    for step in [5, 10, 20, 40] {
        for conversation in [s.conversation_id, delegate] {
            refresh_at(&pool, conversation, now + TimeDelta::seconds(step)).await;
        }
    }
    assert_eq!(
        pushes(&pool).await,
        [(s.conversation_id, "turn_finished".into(), "pending".into())]
    );
    let report = sender(&config).deliver_due(&pool).await.unwrap();
    assert_eq!(report.sent, 1);
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].body["title"], "Сборка релиза");
    assert_eq!(seen[0].body["priority"], 5);
    assert!(
        seen[0].body["message"]
            .as_str()
            .unwrap()
            .starts_with("Ход завершён"),
        "{:?}",
        seen[0].body
    );
    assert_eq!(
        seen[0].body["extras"]["client::notification"]["click"]["url"],
        format!("{APP}/c/{}", s.conversation_id)
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_turn_held_by_a_background_task_is_pushed_when_the_background_ends(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    long_idle(&pool, s.conversation_id, now).await;
    // Ход кончился двадцать минут назад, но последнее событие сессии — конец фоновой
    // задачи десять секунд назад: итог свежий, хотя конец хода давно за окном.
    finish_turn(
        &pool,
        s.machine_id,
        s.conversation_id,
        &execution,
        now - TimeDelta::minutes(20),
    )
    .await;
    sqlx::query("update executions set last_event_at=$2 where conversation_id=$1")
        .bind(s.conversation_id)
        .bind(now - TimeDelta::seconds(10))
        .execute(&pool)
        .await
        .unwrap();
    refresh_at(&pool, s.conversation_id, now).await;
    assert_eq!(
        pushes(&pool).await,
        [(s.conversation_id, "turn_finished".into(), "pending".into())]
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_turn_from_the_owner_is_pushed_and_a_turn_from_an_event_is_silent(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    long_idle(&pool, s.conversation_id, now).await;

    let owner = input(
        &pool,
        &s,
        s.conversation_id,
        "owner:one",
        "собери релиз",
        None,
        now,
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        owner.command_id,
        now - TimeDelta::seconds(1),
    )
    .await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(6)).await;
    assert_eq!(turn_finished(&pool, s.conversation_id).await, 1);
    assert_eq!(sender(&config).deliver_due(&pool).await.unwrap().sent, 1);
    assert_eq!(fake.seen()[0].body["message"], "Ход завершён");
    assert_eq!(owner_mark(&pool, s.conversation_id).await, None);

    let event = input(
        &pool,
        &s,
        s.conversation_id,
        "event:one",
        "итог делегата",
        Some(event_origin("session_result")),
        now + TimeDelta::seconds(10),
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        event.command_id,
        now + TimeDelta::seconds(10),
    )
    .await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(16)).await;
    assert_eq!(
        turn_finished(&pool, s.conversation_id).await,
        1,
        "ход от события молчит"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_owner_turn_without_delegates_gives_exactly_one_push(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    long_idle(&pool, s.conversation_id, now).await;
    let owner = input(
        &pool,
        &s,
        s.conversation_id,
        "owner:one",
        "вопрос",
        None,
        now,
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        owner.command_id,
        now - TimeDelta::seconds(1),
    )
    .await;
    for step in [6, 12, 20] {
        refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(step)).await;
    }
    assert_eq!(turn_finished(&pool, s.conversation_id).await, 1);
    let final_pushes: i64 = sqlx::query_scalar(
        "select count(*) from owner_pushes where conversation_id=$1 and payload->>'final'='true'",
    )
    .bind(s.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(final_pushes, 0, "пуш о ходе владельца и есть итог");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_last_delegate_result_sends_one_all_done_push(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    long_idle(&pool, s.conversation_id, now).await;
    let first = child_of(&pool, &s, s.conversation_id, "working").await;
    let second = child_of(&pool, &s, s.conversation_id, "working").await;

    let owner = input(
        &pool,
        &s,
        s.conversation_id,
        "owner:one",
        "сделай две задачи",
        None,
        now,
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        owner.command_id,
        now - TimeDelta::seconds(1),
    )
    .await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(6)).await;
    assert_eq!(turn_finished(&pool, s.conversation_id).await, 1);
    assert!(
        owner_mark(&pool, s.conversation_id).await.is_some(),
        "работа делегатов держит отметку"
    );

    let first_result = input(
        &pool,
        &s,
        s.conversation_id,
        "event:first",
        "первый готов",
        Some(event_origin("session_result")),
        now + TimeDelta::seconds(10),
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        first_result.command_id,
        now + TimeDelta::seconds(10),
    )
    .await;
    set_child_state(&pool, first, "idle").await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(16)).await;
    assert_eq!(
        turn_finished(&pool, s.conversation_id).await,
        1,
        "второй делегат ещё работает"
    );

    let second_result = input(
        &pool,
        &s,
        s.conversation_id,
        "event:second",
        "второй готов",
        Some(event_origin("session_result")),
        now + TimeDelta::seconds(20),
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        second_result.command_id,
        now + TimeDelta::seconds(20),
    )
    .await;
    set_child_state(&pool, second, "idle").await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(26)).await;
    assert_eq!(turn_finished(&pool, s.conversation_id).await, 2);
    assert_eq!(owner_mark(&pool, s.conversation_id).await, None);
    let final_flags: Vec<Option<String>> = sqlx::query_scalar(
        "select payload->>'final' from owner_pushes where conversation_id=$1 order by id",
    )
    .bind(s.conversation_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(final_flags, [None, Some("true".to_owned())]);

    assert_eq!(sender(&config).deliver_due(&pool).await.unwrap().sent, 2);
    let seen = fake.seen();
    assert!(
        seen[1].body["message"]
            .as_str()
            .unwrap()
            .starts_with("Всё готово"),
        "{:?}",
        seen[1].body
    );

    let later = input(
        &pool,
        &s,
        s.conversation_id,
        "event:later",
        "ещё событие",
        Some(event_origin("session_result")),
        now + TimeDelta::seconds(30),
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        later.command_id,
        now + TimeDelta::seconds(30),
    )
    .await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(36)).await;
    assert_eq!(
        turn_finished(&pool, s.conversation_id).await,
        2,
        "после итога события молчат"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_delegate_stuck_on_the_owner_holds_the_all_done_push(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    long_idle(&pool, s.conversation_id, now).await;
    let child = child_of(&pool, &s, s.conversation_id, "waiting_for_user").await;

    let owner = input(
        &pool,
        &s,
        s.conversation_id,
        "owner:one",
        "спроси делегата",
        None,
        now,
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        owner.command_id,
        now - TimeDelta::seconds(1),
    )
    .await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(6)).await;
    assert_eq!(turn_finished(&pool, s.conversation_id).await, 1);

    let waiting = input(
        &pool,
        &s,
        s.conversation_id,
        "event:waiting",
        "делегат ждёт владельца",
        Some(event_origin("session_result")),
        now + TimeDelta::seconds(10),
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        waiting.command_id,
        now + TimeDelta::seconds(10),
    )
    .await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(16)).await;
    assert_eq!(
        turn_finished(&pool, s.conversation_id).await,
        1,
        "ожидающий владельца делегат держит итог"
    );

    set_child_state(&pool, child, "idle").await;
    let settled = input(
        &pool,
        &s,
        s.conversation_id,
        "event:settled",
        "делегат закончил",
        Some(event_origin("session_result")),
        now + TimeDelta::seconds(20),
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        settled.command_id,
        now + TimeDelta::seconds(20),
    )
    .await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(26)).await;
    assert_eq!(turn_finished(&pool, s.conversation_id).await, 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_queued_spawn_holds_the_all_done_push(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    long_idle(&pool, s.conversation_id, now).await;
    let queued = Uuid::now_v7();
    sqlx::query(
        "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,not_before)
         values($1,$2,$3,$4,'{}','rate',$5)",
    )
    .bind(queued)
    .bind(s.owner_id)
    .bind(s.conversation_id)
    .bind(Uuid::now_v7())
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();

    let owner = input(
        &pool,
        &s,
        s.conversation_id,
        "owner:one",
        "запусти делегата",
        None,
        now,
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        owner.command_id,
        now - TimeDelta::seconds(1),
    )
    .await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(6)).await;
    assert_eq!(turn_finished(&pool, s.conversation_id).await, 1);
    assert!(owner_mark(&pool, s.conversation_id).await.is_some());

    let waiting = input(
        &pool,
        &s,
        s.conversation_id,
        "event:waiting",
        "ждём очередь",
        Some(event_origin("session_result")),
        now + TimeDelta::seconds(10),
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        waiting.command_id,
        now + TimeDelta::seconds(10),
    )
    .await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(16)).await;
    assert_eq!(turn_finished(&pool, s.conversation_id).await, 1);

    sqlx::query("update spawn_queue set state='cancelled' where id=$1")
        .bind(queued)
        .execute(&pool)
        .await
        .unwrap();
    let settled = input(
        &pool,
        &s,
        s.conversation_id,
        "event:settled",
        "очередь кончилась",
        Some(event_origin("spawn_queue")),
        now + TimeDelta::seconds(20),
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        settled.command_id,
        now + TimeDelta::seconds(20),
    )
    .await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(26)).await;
    assert_eq!(turn_finished(&pool, s.conversation_id).await, 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_owner_mark_is_reloaded_from_the_database(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    long_idle(&pool, s.conversation_id, now).await;
    input(
        &pool,
        &s,
        s.conversation_id,
        "owner:one",
        "дождись делегата",
        None,
        now,
    )
    .await;
    let mark = owner_mark(&pool, s.conversation_id).await;
    assert!(mark.is_some(), "ввод владельца взводит отметку");

    // Новый процесс хаба читает отметку из строки, а не из памяти: любой свежий
    // запрос видит тот же срок, по которому потом снимется итог.
    let mut reloaded = pool.acquire().await.unwrap();
    let after_restart: Option<DateTime<Utc>> =
        sqlx::query_scalar("select owner_waiting_since from conversations where id=$1")
            .bind(s.conversation_id)
            .fetch_one(&mut *reloaded)
            .await
            .unwrap();
    drop(reloaded);
    assert_eq!(after_restart, mark);

    let result = input(
        &pool,
        &s,
        s.conversation_id,
        "event:result",
        "делегат готов",
        Some(event_origin("session_result")),
        now + TimeDelta::seconds(10),
    )
    .await;
    finish_submitted_turn(
        &pool,
        s.conversation_id,
        &execution,
        result.command_id,
        now + TimeDelta::seconds(10),
    )
    .await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(16)).await;
    assert_eq!(turn_finished(&pool, s.conversation_id).await, 1);
    let final_flag: Option<String> =
        sqlx::query_scalar("select payload->>'final' from owner_pushes where conversation_id=$1")
            .bind(s.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(final_flag.as_deref(), Some("true"));
    assert_eq!(owner_mark(&pool, s.conversation_id).await, None);
}

async fn turn_finished(pool: &PgPool, conversation: Uuid) -> i64 {
    sqlx::query_scalar(
        "select count(*) from owner_pushes where conversation_id=$1 and kind='turn_finished'",
    )
    .bind(conversation)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Строка отложена до следующего прохода: в тесте минуту не ждём, а делаем её созревшей.
async fn make_due(pool: &PgPool) {
    sqlx::query(
        "update owner_pushes set run_at=clock_timestamp()-interval '1 second'
         where state='pending'",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn block(
    pool: &PgPool,
    execution: Uuid,
    conversation: Uuid,
    kind: &str,
    content: &str,
    at: DateTime<Utc>,
) {
    sqlx::query(
        "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,created_at,agent)
         values($1,$2,$3,$4,'final',1,$5,$6,'main')",
    )
    .bind(Uuid::now_v7())
    .bind(execution)
    .bind(conversation)
    .bind(kind)
    .bind(content)
    .bind(at)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_finished_turn_without_text_does_not_show_the_previous_answer(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    long_idle(&pool, s.conversation_id, now).await;
    // Прошлый ход оставил ответ, текущий кончился только вызовом инструмента.
    finish_turn(
        &pool,
        s.machine_id,
        s.conversation_id,
        &execution,
        now - TimeDelta::minutes(30),
    )
    .await;
    block(
        &pool,
        execution.execution_id,
        s.conversation_id,
        "text",
        "Прошлый ответ",
        now - TimeDelta::minutes(30) + TimeDelta::seconds(1),
    )
    .await;
    finish_turn(
        &pool,
        s.machine_id,
        s.conversation_id,
        &execution,
        now - TimeDelta::seconds(10),
    )
    .await;
    block(
        &pool,
        execution.execution_id,
        s.conversation_id,
        "tool_call",
        "",
        now - TimeDelta::seconds(9),
    )
    .await;
    for step in [6, 10] {
        refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(step)).await;
    }
    assert_eq!(sender(&config).deliver_due(&pool).await.unwrap().sent, 1);
    let seen = fake.seen();
    assert_eq!(seen[0].body["message"], "Ход завершён");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_turn_is_pushed_once_even_when_the_state_flaps(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    let now = Utc::now();
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    long_idle(&pool, s.conversation_id, now).await;
    finish_turn(
        &pool,
        s.machine_id,
        s.conversation_id,
        &execution,
        now - TimeDelta::seconds(2),
    )
    .await;
    for step in [6, 10] {
        refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(step)).await;
    }
    assert_eq!(turn_finished(&pool, s.conversation_id).await, 1);
    // Воркер теряется и возвращается без нового хода: `since` новый, ход прежний.
    executions::set_state(
        &pool,
        execution.execution_id,
        ExecutionState::OwnerLost,
        None,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(12)).await;
    executions::set_state(
        &pool,
        execution.execution_id,
        ExecutionState::Running,
        Some(canterlot_proto::OWNER_RECLAIMED),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    for step in [14, 20, 30] {
        refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(step)).await;
    }
    assert_eq!(
        turn_finished(&pool, s.conversation_id).await,
        1,
        "one push per turn"
    );
}

async fn owner_lost(pool: &PgPool, execution: Uuid) {
    executions::set_state(
        pool,
        execution,
        ExecutionState::OwnerLost,
        None,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
}

async fn owner_reclaimed(pool: &PgPool, execution: Uuid) {
    executions::set_state(
        pool,
        execution,
        ExecutionState::Running,
        Some(canterlot_proto::OWNER_RECLAIMED),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_reopened_request_is_pushed_only_if_the_first_push_never_left(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let first = Uuid::now_v7();
    open_question(&pool, &s, &question_batch(&execution, first, "Первый?")).await;
    // Запрос закрылся потерей воркера, доставка отменила строку: пуша не было.
    owner_lost(&pool, execution.execution_id).await;
    let cancelled = sender(&config).deliver_due(&pool).await.unwrap();
    assert_eq!((cancelled.sent, cancelled.cancelled), (0, 1));
    assert!(fake.seen().is_empty());
    // Воркер вернулся и снова ждёт ответа на тот же запрос.
    owner_reclaimed(&pool, execution.execution_id).await;
    open_question(&pool, &s, &question_batch(&execution, first, "Первый?")).await;
    assert_eq!(
        pushes(&pool).await,
        [(s.conversation_id, "waiting".into(), "pending".into())]
    );
    assert_eq!(sender(&config).deliver_due(&pool).await.unwrap().sent, 1);
    assert_eq!(fake.seen().len(), 1);

    // А ушедший пуш повторно открытый запрос не повторяет.
    let second = Uuid::now_v7();
    open_question(&pool, &s, &question_batch(&execution, second, "Второй?")).await;
    assert_eq!(sender(&config).deliver_due(&pool).await.unwrap().sent, 1);
    owner_lost(&pool, execution.execution_id).await;
    owner_reclaimed(&pool, execution.execution_id).await;
    open_question(&pool, &s, &question_batch(&execution, second, "Второй?")).await;
    let pending: i64 =
        sqlx::query_scalar("select count(*) from owner_pushes where state='pending'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending, 0, "a delivered request stays delivered");
    assert_eq!(fake.seen().len(), 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_revived_waiting_push_is_fresh_even_if_the_first_row_is_old(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let request = Uuid::now_v7();
    open_question(&pool, &s, &question_batch(&execution, request, "Первый?")).await;
    // Запрос закрылся потерей воркера, доставка отменила строку.
    owner_lost(&pool, execution.execution_id).await;
    assert_eq!(
        sender(&config).deliver_due(&pool).await.unwrap().cancelled,
        1
    );
    // Строка отменена давно: при возрождении её возраст не должен её топить.
    sqlx::query(
        "update owner_pushes set created_at=clock_timestamp()-interval '31 minutes'
         where dedupe_key=$1",
    )
    .bind(format!("waiting:{request}"))
    .execute(&pool)
    .await
    .unwrap();

    owner_reclaimed(&pool, execution.execution_id).await;
    open_question(&pool, &s, &question_batch(&execution, request, "Первый?")).await;
    assert_eq!(
        pushes(&pool).await,
        [(s.conversation_id, "waiting".into(), "pending".into())]
    );
    let report = sender(&config).deliver_due(&pool).await.unwrap();
    assert_eq!((report.sent, report.dropped), (1, 0));
    assert_eq!(fake.seen().len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_old_idle_session_does_not_push_after_a_restart(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    long_idle(&pool, s.conversation_id, now).await;
    finish_turn(
        &pool,
        s.machine_id,
        s.conversation_id,
        &execution,
        now - TimeDelta::hours(2),
    )
    .await;
    refresh_at(&pool, s.conversation_id, now).await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(30)).await;
    assert!(pushes(&pool).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_late_event_does_not_revive_an_old_turn_push(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    finish_turn(
        &pool,
        s.machine_id,
        s.conversation_id,
        &execution,
        now - TimeDelta::days(3),
    )
    .await;
    // Позднее событие супервизора (например, после перезапуска воркера) освежает
    // `last_event_at`, но трёхдневный ход от этого не становится свежим.
    sqlx::query("update executions set last_event_at=$2 where conversation_id=$1")
        .bind(s.conversation_id)
        .bind(now - TimeDelta::seconds(60))
        .execute(&pool)
        .await
        .unwrap();
    refresh_at(&pool, s.conversation_id, now).await;
    refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(30)).await;
    assert!(pushes(&pool).await.is_empty(), "{:?}", pushes(&pool).await);
}

async fn lose(pool: &PgPool, conversation: Uuid, execution: Uuid) {
    executions::set_state(
        pool,
        execution,
        ExecutionState::OwnerLost,
        None,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    for step in [0, 15, 30] {
        refresh_at(pool, conversation, Utc::now() + TimeDelta::seconds(step)).await;
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failure_of_a_root_session_is_pushed_once_and_a_delegate_failure_is_not(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let delegate = delegate_of(&pool, &s).await;
    let root_run = start(&pool, s.owner_id, s.conversation_id).await;
    let delegate_run = start(&pool, s.owner_id, delegate).await;
    lose(&pool, s.conversation_id, root_run.execution_id).await;
    lose(&pool, delegate, delegate_run.execution_id).await;
    assert_eq!(
        pushes(&pool).await,
        [(s.conversation_id, "failure".into(), "pending".into())]
    );
    assert_eq!(sender(&config).deliver_due(&pool).await.unwrap().sent, 1);
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].body["priority"], 7);
    assert_eq!(seen[0].body["message"], "Сессия упала: воркер потерян");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn one_failure_push_per_episode_survives_a_failed_idle_bounce(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    let failed = Evidence::Failed {
        reason: "process_exited".into(),
    };
    settle(
        &pool,
        s.conversation_id,
        Settled::Failed,
        &failed,
        now,
        "fail-1",
    )
    .await;
    let idle = Evidence::Idle {
        last_turn_at: None,
        background_work: None,
        process_alive: Some(false),
    };
    settle(
        &pool,
        s.conversation_id,
        Settled::Idle,
        &idle,
        now + TimeDelta::seconds(10),
        "idle-1",
    )
    .await;
    settle(
        &pool,
        s.conversation_id,
        Settled::Failed,
        &failed,
        now + TimeDelta::seconds(20),
        "fail-2",
    )
    .await;
    assert_eq!(failure_pushes(&pool, s.conversation_id).await, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn one_failure_push_per_episode_survives_a_stalled_working_bounce(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    let stalled = Evidence::Stalled {
        last_event_at: now - TimeDelta::minutes(40),
        phase: "generating".into(),
        process_alive: Some(true),
        cpu_busy: Some(false),
        link: "available".into(),
    };
    settle(
        &pool,
        s.conversation_id,
        Settled::Stalled,
        &stalled,
        now,
        "stalled-1",
    )
    .await;
    let idle = Evidence::Idle {
        last_turn_at: None,
        background_work: None,
        process_alive: Some(true),
    };
    settle(
        &pool,
        s.conversation_id,
        Settled::Idle,
        &idle,
        now + TimeDelta::seconds(10),
        "idle-2",
    )
    .await;
    settle(
        &pool,
        s.conversation_id,
        Settled::Stalled,
        &stalled,
        now + TimeDelta::seconds(20),
        "stalled-2",
    )
    .await;
    assert_eq!(failure_pushes(&pool, s.conversation_id).await, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_read_episode_never_pushes_again(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    let failed = Evidence::Failed {
        reason: "process_exited".into(),
    };
    settle(
        &pool,
        s.conversation_id,
        Settled::Failed,
        &failed,
        now,
        "fail-1",
    )
    .await;
    let marker: i64 =
        sqlx::query_scalar("select read_seq from failure_push_episodes where conversation_id=$1")
            .bind(s.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("update conversations set last_read_seq=$2 where id=$1")
        .bind(s.conversation_id)
        .bind(marker)
        .execute(&pool)
        .await
        .unwrap();
    // Строку пуша убираем: даже без неё прочитанный эпизод молчит.
    sqlx::query("delete from owner_pushes where conversation_id=$1 and kind='failure'")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update failure_push_episodes set pushed=false where conversation_id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    settle(
        &pool,
        s.conversation_id,
        Settled::Failed,
        &failed,
        now + TimeDelta::seconds(20),
        "fail-2",
    )
    .await;
    assert_eq!(failure_pushes(&pool, s.conversation_id).await, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_new_execution_failing_is_a_new_episode(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    let first = start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    let failed = Evidence::Failed {
        reason: "process_exited".into(),
    };
    settle(
        &pool,
        s.conversation_id,
        Settled::Failed,
        &failed,
        now,
        "fail-1",
    )
    .await;
    sqlx::query("update executions set state='failed',outcome='process_exited' where id=$1")
        .bind(first.execution_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state)
         values($1,$2,2,'running')",
    )
    .bind(Uuid::now_v7())
    .bind(s.conversation_id)
    .execute(&pool)
    .await
    .unwrap();
    settle(
        &pool,
        s.conversation_id,
        Settled::Failed,
        &failed,
        now + TimeDelta::seconds(20),
        "fail-2",
    )
    .await;
    assert_eq!(failure_pushes(&pool, s.conversation_id).await, 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn another_reason_is_a_new_episode(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    start(&pool, s.owner_id, s.conversation_id).await;
    let now = Utc::now();
    settle(
        &pool,
        s.conversation_id,
        Settled::Failed,
        &Evidence::Failed {
            reason: "process_exited".into(),
        },
        now,
        "fail-1",
    )
    .await;
    settle(
        &pool,
        s.conversation_id,
        Settled::Failed,
        &Evidence::Failed {
            reason: "context_exhausted".into(),
        },
        now + TimeDelta::seconds(20),
        "fail-2",
    )
    .await;
    assert_eq!(failure_pushes(&pool, s.conversation_id).await, 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delivery_uses_the_database_clock(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;
    // Часы хаба ушли на час вперёд: по ним свежий пуш был бы протухшим, по часам базы — нет.
    assert_eq!(sender(&config).deliver_due(&pool).await.unwrap().sent, 1);
    // А строка, протухшая по часам базы, не уходит даже при тех же часах хаба.
    let second = Uuid::now_v7();
    open_question(&pool, &s, &question_batch(&execution, second, "Старый?")).await;
    sqlx::query(
        "update owner_pushes set created_at=clock_timestamp()-interval '1 hour'
         where dedupe_key=$1",
    )
    .bind(format!("waiting:{second}"))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(sender(&config).deliver_due(&pool).await.unwrap().dropped, 1);
    assert_eq!(fake.seen().len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_attempt_is_recorded_before_the_request_goes_out(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;
    fake.probe(&pool);
    // Обычная пауза повтора: после ответа строка не должна быть готова к отправке.
    let sender = Sender::new(&config).unwrap();
    assert_eq!(sender.deliver_due(&pool).await.unwrap().sent, 1);
    let seen = fake.seen();
    assert_eq!(
        seen[0].attempts,
        Some(1),
        "attempt is recorded before the POST"
    );
    assert_eq!(
        seen[0].retry_scheduled,
        Some(true),
        "a lost acknowledgement costs a delayed retry, not a resend"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_server_error_is_retried_and_then_delivered(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    fake.script(&[500]);
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;
    let sender = sender(&config);
    let first = sender.deliver_due(&pool).await.unwrap();
    assert_eq!((first.sent, first.retried), (0, 1));
    tokio::time::sleep(Duration::from_millis(2)).await;
    let second = sender.deliver_due(&pool).await.unwrap();
    assert_eq!(second.sent, 1);
    assert_eq!(fake.seen().len(), 2);
    let (state, attempts, status): (String, i32, Option<String>) =
        sqlx::query_as("select state,attempts,last_status from owner_pushes")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (state.as_str(), attempts, status),
        ("sent", 2, Some("200".into()))
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delivery_gives_up_after_the_last_attempt_without_leaking_the_key(pool: PgPool) {
    capture_logs();
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    fake.script(&[500, 503, 500, 500]);
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Секретный вопрос?"),
    )
    .await;
    let sender = sender(&config);
    let mut dropped = 0;
    for _ in 0..5 {
        let report = sender.deliver_due(&pool).await.unwrap();
        dropped += report.dropped;
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(dropped, 1);
    assert_eq!(
        fake.seen().len(),
        3,
        "three attempts, then the push is dropped"
    );
    assert_eq!(pushes(&pool).await[0].2, "dropped");

    let logs = captured_logs();
    assert!(logs.contains("owner_push_dropped"), "{logs}");
    assert!(logs.contains("status=500"), "{logs}");
    assert!(logs.contains(&s.conversation_id.to_string()), "{logs}");
    assert!(!logs.contains(TOKEN), "token leaked into logs");
    assert!(!logs.contains("Секретный"), "message text leaked into logs");
    for rendered in [format!("{config:?}"), format!("{sender:?}")] {
        assert!(
            !rendered.contains(TOKEN),
            "token leaked into Debug: {rendered}"
        );
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_client_error_is_not_retried(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    fake.script(&[401]);
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;
    let report = sender(&config).deliver_due(&pool).await.unwrap();
    assert_eq!(report.dropped, 1);
    assert_eq!(
        sender(&config).deliver_due(&pool).await.unwrap(),
        Default::default()
    );
    assert_eq!(fake.seen().len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_rate_limit_defers_the_rest(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "max_per_minute = 2\n");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    for _ in 0..3 {
        open_question(
            &pool,
            &s,
            &question_batch(&execution, Uuid::now_v7(), "Да?"),
        )
        .await;
    }
    let sender = sender(&config);
    let report = sender.deliver_due(&pool).await.unwrap();
    assert_eq!((report.sent, report.deferred), (2, 1));
    assert_eq!(fake.seen().len(), 2);
    assert_eq!(sender.deliver_due(&pool).await.unwrap().deferred, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn without_the_section_nothing_is_queued(pool: PgPool) {
    let (_dir, config) = load("", None);
    assert!(config.unwrap().notify.gotify.is_none());
    enable(&pool).await;
    notify::configure(&pool, None).await.unwrap();
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;
    lose(&pool, s.conversation_id, execution.execution_id).await;
    assert!(pushes(&pool).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn disabling_at_start_cancels_what_was_pending(pool: PgPool) {
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;
    notify::configure(&pool, None).await.unwrap();
    assert_eq!(pushes(&pool).await[0].2, "cancelled");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn finished_rows_are_cleaned_without_the_section(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    let stale = Uuid::now_v7();
    let fresh = Uuid::now_v7();
    for request in [stale, fresh] {
        open_question(&pool, &s, &question_batch(&execution, request, "Да?")).await;
    }
    assert_eq!(sender(&config).deliver_due(&pool).await.unwrap().sent, 2);
    assert_eq!(fake.seen().len(), 2);
    sqlx::query(
        "update owner_pushes set updated_at=clock_timestamp()-interval '8 days'
         where dedupe_key=$1",
    )
    .bind(format!("waiting:{stale}"))
    .execute(&pool)
    .await
    .unwrap();
    notify::configure(&pool, None).await.unwrap();
    let left: Vec<String> =
        sqlx::query_scalar("select dedupe_key from owner_pushes order by dedupe_key")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(left, [format!("waiting:{fresh}")]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn trash_cancels_pending_pushes_and_purge_removes_them(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;
    for (state, outcome) in [
        (ExecutionState::Stopping, None),
        (ExecutionState::Stopped, Some("user_stop")),
    ] {
        executions::set_state(
            &pool,
            execution.execution_id,
            state,
            outcome,
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    }
    deletion::trash(
        &pool,
        s.owner_id,
        s.conversation_id,
        "trash-1",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(
        pushes(&pool).await,
        [(s.conversation_id, "waiting".into(), "cancelled".into())]
    );
    assert_eq!(
        sender(&config).deliver_due(&pool).await.unwrap(),
        Default::default()
    );
    assert!(fake.seen().is_empty());

    let blobs = tempfile::tempdir().unwrap();
    let store = LocalBlobStore::open(
        &AttachmentsConfig {
            blob_dir: blobs.path().join("blobs"),
            ..Default::default()
        },
        Arc::new(attachments::NoTrace),
    )
    .unwrap();
    deletion::purge(
        &pool,
        &store,
        s.owner_id,
        s.conversation_id,
        Utc::now() + TimeDelta::hours(1),
        "purge-1",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert!(pushes(&pool).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_unknown_timezone_is_refused_at_start(pool: PgPool) {
    let error = notify::configure(
        &pool,
        Some(&PushSettings {
            waiting: true,
            turn_finished: true,
            failures: true,
            timezone: "Mars/Olympus".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error, "notify_timezone_invalid");
}

#[test]
fn the_config_resolves_defaults_and_hides_the_token() {
    let (_dir, config) = load(
        "[http]\ntrusted_proxies = ['127.0.0.1/32']\nowner_login = 'me'\nallowed_origins = ['https://pwa.example.test']\n[notify.gotify]\nurl = 'https://gotify.example.test/'\ntoken_file = 'gotify-token'\n",
        Some(&format!("  {TOKEN}\n")),
    );
    let config = config.unwrap();
    let rendered = format!("{config:?}");
    assert!(!rendered.contains(TOKEN), "{rendered}");
    let gotify = config.notify.gotify.unwrap();
    assert_eq!(gotify.url, "https://gotify.example.test");
    assert_eq!(gotify.app_url(), "https://pwa.example.test");
    assert_eq!(gotify.token.expose(), TOKEN);
    assert_eq!(gotify.timezone, "UTC");
    assert!(gotify.waiting && gotify.turn_finished && gotify.failures);
    assert_eq!(gotify.max_per_minute, 20);
}

#[test]
fn config_errors_are_clear_at_start() {
    let section = |url: &str, extra: &str| {
        format!("[notify.gotify]\nurl = '{url}'\ntoken_file = 'gotify-token'\n{extra}")
    };
    let app = format!("app_url = '{APP}'\n");
    for (extra, token, category) in [
        (
            section("gotify.example.test", &app),
            Some(TOKEN),
            "config_invalid_notify_url",
        ),
        (
            section("ftp://gotify.example.test", &app),
            Some(TOKEN),
            "config_invalid_notify_url",
        ),
        (
            section("https://gotify.example.test/?a=1", &app),
            Some(TOKEN),
            "config_invalid_notify_url",
        ),
        (
            section("https://gotify.example.test", ""),
            Some(TOKEN),
            "config_notify_app_url_missing",
        ),
        (
            section("https://gotify.example.test", "app_url = 'nope'\n"),
            Some(TOKEN),
            "config_invalid_notify_app_url",
        ),
        (
            section("https://gotify.example.test", &app),
            None,
            "notify_token_file_unreadable",
        ),
        (
            section("https://gotify.example.test", &app),
            Some("  \n"),
            "notify_token_file_invalid",
        ),
        (
            section("https://gotify.example.test", &app),
            Some("has space"),
            "notify_token_file_invalid",
        ),
        (
            section(
                "https://gotify.example.test",
                &format!("{app}unknown = 1\n"),
            ),
            Some(TOKEN),
            "config_invalid",
        ),
    ] {
        let (_dir, result) = load(&extra, token);
        let error = result.expect_err(&extra);
        assert!(error.contains(category), "{extra}: {error}");
        assert!(!error.contains(TOKEN), "{error}");
    }
    let (_dir, result) = load(&section("https://gotify.example.test", &app), None);
    assert!(
        result.unwrap_err().contains("gotify-token"),
        "names the token file"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_visible_and_read_chat_suppresses_waiting_and_turn_finished(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let now = Utc::now();
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    long_idle(&pool, s.conversation_id, now).await;
    let turn_at = now - TimeDelta::seconds(1);
    finish_turn(&pool, s.machine_id, s.conversation_id, &execution, turn_at).await;
    sqlx::query(
        "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,created_at,agent,created_seq)
         values($1,$2,$3,'text','final',1,'Готово',$4,'main',42)",
    )
    .bind(Uuid::now_v7())
    .bind(execution.execution_id)
    .bind(s.conversation_id)
    .bind(turn_at)
    .execute(&pool)
    .await
    .unwrap();
    for step in [5, 10, 20, 40] {
        refresh_at(&pool, s.conversation_id, now + TimeDelta::seconds(step)).await;
    }
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;

    let kinds: Vec<String> =
        sqlx::query_scalar("select kind from owner_pushes where state='pending' order by kind")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(kinds, ["turn_finished", "waiting"]);
    let head: i64 =
        sqlx::query_scalar("select coalesce(max((payload->>'seq')::bigint),0) from owner_pushes")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(head > 0, "both pushes carry their feed position");
    sqlx::query("update conversations set last_read_seq=greatest(last_read_seq,$2) where id=$1")
        .bind(s.conversation_id)
        .bind(head)
        .execute(&pool)
        .await
        .unwrap();

    let presence = Presence::default();
    let at = Instant::now();
    presence.report(s.conversation_id, "phone", true, at);
    let report = presence_sender(&config, &presence)
        .deliver_due_at(&pool, at)
        .await
        .unwrap();
    assert_eq!(
        (report.sent, report.suppressed, report.deferred_visible),
        (0, 2, 0)
    );
    assert!(fake.seen().is_empty(), "nothing reached Gotify");
    let rows: Vec<(String, String, Option<String>)> =
        sqlx::query_as("select kind,state,last_status from owner_pushes order by kind")
            .fetch_all(&pool)
            .await
            .unwrap();
    for (kind, state, status) in rows {
        assert_eq!(state, "suppressed", "{kind}");
        assert_eq!(status.as_deref(), Some("suppressed_seen"), "{kind}");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_hidden_chat_does_not_suppress(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;

    let presence = Presence::default();
    let at = Instant::now();
    presence.report(s.conversation_id, "phone", true, at);
    presence.report(s.conversation_id, "phone", false, at);
    assert!(!presence.is_visible(s.conversation_id, at));
    let report = presence_sender(&config, &presence)
        .deliver_due_at(&pool, at)
        .await
        .unwrap();
    assert_eq!((report.sent, report.suppressed), (1, 0));
    assert_eq!(fake.seen().len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_second_device_keeps_a_chat_visible_when_another_goes_hidden(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let first = seed(&pool).await;
    let second = seed(&pool).await;
    for s in [&first, &second] {
        let execution = start(&pool, s.owner_id, s.conversation_id).await;
        open_question(&pool, s, &question_batch(&execution, Uuid::now_v7(), "Да?")).await;
        sqlx::query("update conversations set last_read_seq=1000000 where id=$1")
            .bind(s.conversation_id)
            .execute(&pool)
            .await
            .unwrap();
    }

    let presence = Presence::default();
    let at = Instant::now();
    // На первом чате телефон скрыт, но ноутбук виден; на втором наоборот.
    presence.report(first.conversation_id, "phone", true, at);
    presence.report(first.conversation_id, "phone", false, at);
    presence.report(first.conversation_id, "laptop", true, at);
    presence.report(second.conversation_id, "phone", false, at);
    presence.report(second.conversation_id, "laptop", true, at);

    let report = presence_sender(&config, &presence)
        .deliver_due_at(&pool, at)
        .await
        .unwrap();
    assert_eq!((report.sent, report.suppressed), (0, 2));
    assert!(fake.seen().is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_visible_but_unread_chat_gets_the_push_after_the_postpone(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;

    let presence = Presence::default();
    let at = Instant::now();
    presence.report(s.conversation_id, "phone", true, at);
    let sender = presence_sender(&config, &presence);
    let first = sender.deliver_due_at(&pool, at).await.unwrap();
    assert_eq!(
        (first.sent, first.suppressed, first.deferred_visible),
        (0, 0, 1)
    );
    let (state, status, defers): (String, Option<String>, i32) =
        sqlx::query_as("select state,last_status,visible_defers from owner_pushes")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (state.as_str(), status.as_deref(), defers),
        ("pending", Some("deferred_visible"), 1)
    );

    // Телефон уснул и больше не шлёт отметок: через минуту присутствия уже нет.
    make_due(&pool).await;
    let second = sender
        .deliver_due_at(&pool, at + Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!((second.sent, second.suppressed), (1, 0));
    assert_eq!(fake.seen().len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_still_visible_chat_is_postponed_twice_and_then_gets_the_push(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;

    let presence = Presence::default();
    let at = Instant::now();
    let sender = presence_sender(&config, &presence);
    for (step, expected) in [(0, 1), (60, 2)] {
        presence.report(
            s.conversation_id,
            "phone",
            true,
            at + Duration::from_secs(step),
        );
        make_due(&pool).await;
        let report = sender
            .deliver_due_at(&pool, at + Duration::from_secs(step))
            .await
            .unwrap();
        assert_eq!(report.deferred_visible, 1, "step {step}");
        let defers: i32 = sqlx::query_scalar("select visible_defers from owner_pushes")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(defers, expected);
    }
    // Третья встреча: две отсрочки уже были, пуш уходит, даже если чат всё ещё виден.
    presence.report(
        s.conversation_id,
        "phone",
        true,
        at + Duration::from_secs(120),
    );
    make_due(&pool).await;
    let last = sender
        .deliver_due_at(&pool, at + Duration::from_secs(120))
        .await
        .unwrap();
    assert_eq!((last.sent, last.deferred_visible), (1, 0));
    assert_eq!(fake.seen().len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn presence_expires_after_45_seconds_of_silence(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    let execution = start(&pool, s.owner_id, s.conversation_id).await;
    open_question(
        &pool,
        &s,
        &question_batch(&execution, Uuid::now_v7(), "Да?"),
    )
    .await;

    let presence = Presence::default();
    let at = Instant::now();
    presence.report(s.conversation_id, "phone", true, at);
    assert!(presence.is_visible(s.conversation_id, at + TTL - Duration::from_secs(1)));
    assert!(!presence.is_visible(s.conversation_id, at + TTL));
    let report = presence_sender(&config, &presence)
        .deliver_due_at(&pool, at + TTL)
        .await
        .unwrap();
    assert_eq!((report.sent, report.suppressed), (1, 0));
    assert_eq!(fake.seen().len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sign_in_pushes_are_never_suppressed(pool: PgPool) {
    let (fake, url) = fake_gotify().await;
    let (_dir, config) = gotify(&url, "");
    enable(&pool).await;
    let s = seed(&pool).await;
    notify::on_sign_in(&pool, Uuid::now_v7(), "Firefox 159 · Linux")
        .await
        .unwrap();

    let presence = Presence::default();
    let at = Instant::now();
    presence.report(s.conversation_id, "phone", true, at);
    let report = presence_sender(&config, &presence)
        .deliver_due_at(&pool, at)
        .await
        .unwrap();
    assert_eq!((report.sent, report.suppressed), (1, 0));
    assert_eq!(fake.seen()[0].body["title"], "Новый вход в canterlot");
}
