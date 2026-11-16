//! Секретные запросы: пределы, состояния, доставка одним управляющим сообщением,
//! отзыв, конец сессии и сверка после переподключения.
//!
//! Главная проверка здесь — не состояние карточки, а отсутствие значения там, где
//! его быть не должно: в таблицах, событиях, outbox и сообщениях журнала.

use std::time::Duration;

use crate::support::task3;
use canterlot_hub::{
    activity::{self, Thresholds},
    config::HubTimers,
    ingest::{Ingest, IngestOutcome, JournalKind},
    secrets::{self, Answer},
    session::{CAPABILITY_SECRET_REQUESTS_V1, Registry},
    testkit::{self, Seed, TestClient, TestHub},
};
use canterlot_proto::worker::v1::{
    ControlDown, ControlUp, JournalRecord, JournalRecords, RecordBody, control_down, record_body,
};
use serde_json::{Value, json};
use sha2::Digest as _;
use sqlx::PgPool;
use uuid::Uuid;

const CAPS: &[&str] = &["pilot_v1", CAPABILITY_SECRET_REQUESTS_V1];

struct Fx {
    pool: PgPool,
    hub: TestHub,
    client: TestClient,
    seed: Seed,
    registry: Registry,
    _dir: tempfile::TempDir,
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let hub = testkit::spawn_test_hub_http(
            pool.clone(),
            HubTimers {
                secret_attempt: Duration::from_millis(400),
                ..Default::default()
            },
            testkit::test_http_config(),
        )
        .await;
        let owner = hub.installation.owner_id;
        let seed = seed_for(&pool, owner).await;
        let client = TestClient::over_socket(hub.http_addr()).await.as_owner();
        let registry = hub.registry.clone();
        let dir = tempfile::tempdir().expect("temp dir");
        Self {
            pool,
            hub,
            client,
            seed,
            registry,
            _dir: dir,
        }
    }

    async fn open(&self, name: &str) -> secrets::SecretRequestView {
        secrets::open(
            &self.pool,
            self.seed.owner_id,
            self.seed.conversation_id,
            name,
            "нужен для проверки",
            Uuid::now_v7(),
        )
        .await
        .unwrap_or_else(|error| panic!("open {name}: {error:?}"))
    }

    async fn post(&self, path: &str, key: &str, body: Option<Value>) -> testkit::TestResponse {
        self.client
            .send(axum::http::Method::POST, path, Some(key), body)
            .await
    }

    async fn answer(&self, id: Uuid, body: Value) -> testkit::TestResponse {
        self.post(
            &format!("/api/v1/secret-requests/{id}/answer"),
            &Uuid::now_v7().to_string(),
            Some(body),
        )
        .await
    }

    async fn state(&self, id: Uuid) -> String {
        sqlx::query_scalar("select state from secret_requests where id=$1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn row(&self, id: Uuid) -> secrets::SecretRequestRow {
        sqlx::query_as("select * from secret_requests where id=$1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn inputs(&self) -> Vec<String> {
        sqlx::query_scalar(
            "select content->0->>'text' from inputs where conversation_id=$1 order by input_seq",
        )
        .bind(self.seed.conversation_id)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

/// Ждёт, пока сервер отпустит старое управляющее соединение: новое с тем же
/// `unavailable_after` иначе не примут.
async fn wait_disconnected(registry: &Registry, machine: Uuid) {
    for _ in 0..500 {
        if registry.connection_id(machine).is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the old control connection did not end");
}

async fn seed_for(pool: &PgPool, owner: Uuid) -> Seed {
    use sha2::{Digest, Sha256};
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'Машина')")
        .bind(machine)
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("insert into machine_credentials(machine_id,token_sha256) values($1,$2)")
        .bind(machine)
        .bind(Sha256::digest(machine.to_string().as_bytes()).to_vec())
        .execute(pool)
        .await
        .unwrap();
    let conversation_id = testkit::seed_conversation(pool, owner, machine).await;
    Seed {
        owner_id: owner,
        machine_id: machine,
        conversation_id,
    }
}

fn fact_body(body: record_body::Body) -> RecordBody {
    RecordBody { body: Some(body) }
}

/// Кладёт запись в управляющий журнал машины; ответ сверяется с точкой применения.
async fn cj(
    pool: &PgPool,
    registry: &Registry,
    seed: &Seed,
    journal: Uuid,
    seq: u64,
    body: RecordBody,
) {
    let batch = JournalRecords {
        journal_id: journal.to_string(),
        records: vec![JournalRecord {
            seq,
            body: Some(body),
        }],
    };
    let outcome = Ingest::new(pool.clone())
        .with_registry(registry.clone())
        .apply_batch(
            seed.machine_id,
            &sha2::Sha256::digest(seed.machine_id.to_string().as_bytes()),
            JournalKind::Cj,
            &batch,
        )
        .await;
    assert!(
        matches!(outcome, Ok(IngestOutcome::Committed { .. })),
        "{outcome:?}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn three_open_requests_hold_the_card_and_the_fourth_is_refused(pool: PgPool) {
    let fx = Fx::new(pool).await;
    for index in 0..secrets::MAX_OPEN_PER_CONVERSATION {
        fx.open(&format!("Секрет {index}")).await;
    }
    let error = secrets::open(
        &fx.pool,
        fx.seed.owner_id,
        fx.seed.conversation_id,
        "Четвёртый",
        "лишний",
        Uuid::now_v7(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, secrets::SecretError::Limit), "{error:?}");

    let response = fx
        .client
        .send(
            axum::http::Method::GET,
            &format!(
                "/api/v1/conversations/{}/secret-requests",
                fx.seed.conversation_id
            ),
            None,
            None,
        )
        .await;
    assert_eq!(response.status, axum::http::StatusCode::OK);
    let listed: Value = response.json();
    assert_eq!(listed["items"].as_array().unwrap().len(), 3);
    assert!(
        listed["items"][0]["id"].as_str().is_some(),
        "newest first with an id"
    );
    assert!(listed["items"][0].get("value").is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_value_is_delivered_once_over_the_live_stream_and_never_stored(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let marker = format!("canterlot-secret-PROBE-{}", Uuid::now_v7());
    let row = fx.open("Пароль от базы").await;
    assert_eq!(row.state, secrets::SecretState::Open);

    let mut worker = task3::connect_with(&fx.hub, &fx.seed, Uuid::now_v7(), CAPS, 1000).await;

    let response = fx.answer(row.id, json!({"value": marker})).await;
    assert_eq!(
        response.status,
        axum::http::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let view: Value = response.json();
    assert_eq!(view["state"], "sending");
    assert!(view.get("value").is_none());

    let delivered = tokio::time::timeout(Duration::from_secs(5), worker.stream.message())
        .await
        .expect("delivery")
        .unwrap()
        .unwrap();
    let Some(control_down::Msg::SecretDeliver(deliver)) = delivered.msg else {
        panic!("expected SecretDeliver, got {delivered:?}");
    };
    assert_eq!(deliver.request_id, row.id.to_string());
    assert_eq!(deliver.value, marker.as_bytes());
    assert_eq!(deliver.conversation_id, fx.seed.conversation_id.to_string());
    assert!(!format!("{deliver:?}").contains(&marker));
    let attempt: Uuid = deliver.attempt_id.parse().unwrap();

    // Квитанция воркера: файл записан.
    let path = format!(
        "/run/canterlot-secrets/{}/{attempt}",
        fx.seed.conversation_id
    );
    let expires = chrono::Utc::now() + chrono::TimeDelta::hours(24);
    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretDelivered(
            canterlot_proto::worker::v1::SecretDelivered {
                attempt_id: attempt.to_string(),
                request_id: row.id.to_string(),
                path: path.clone(),
                expires_at_ms: u64::try_from(expires.timestamp_millis()).unwrap(),
            },
        )),
    )
    .await;

    let done = fx.row(row.id).await;
    assert_eq!(done.state, secrets::SecretState::Delivered);
    assert_eq!(done.path.as_deref(), Some(path.as_str()));
    let prompts = fx.inputs().await;
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains(&path) && prompts[0].contains("Пароль от базы"),
        "{}",
        prompts[0]
    );
    assert!(!prompts[0].contains(&marker), "the value is not a message");

    // Гарантия: значения нет ни в одной таблице, событии и строке outbox.
    assert_absent_everywhere(&fx.pool, &marker).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_decline_answers_the_card_and_tells_the_agent(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Токен").await;
    let response = fx.answer(row.id, json!({"decline": true})).await;
    assert_eq!(response.status, axum::http::StatusCode::OK);
    assert_eq!(fx.state(row.id).await, "declined");
    let prompts = fx.inputs().await;
    assert_eq!(prompts, vec!["Владелец отказал в секрете «Токен»."]);
    // Повторный ответ на закрытый запрос — 409 not_open.
    let again = fx.answer(row.id, json!({"decline": true})).await;
    assert_eq!(again.status, axum::http::StatusCode::CONFLICT);
    assert_eq!(again.json::<Value>()["error"]["code"], "not_open");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_offline_machine_refuses_the_value_and_keeps_the_request_open(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    let response = fx
        .answer(row.id, json!({"value": "canterlot-secret-PROBE-offline"}))
        .await;
    assert_eq!(response.status, axum::http::StatusCode::CONFLICT);
    assert_eq!(response.json::<Value>()["error"]["code"], "machine_offline");
    assert_eq!(fx.state(row.id).await, "open");
    assert_absent_everywhere(&fx.pool, "canterlot-secret-PROBE-offline").await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_idempotency_header_of_the_answer_leaves_no_row(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    let key = format!("answer-key-{}", Uuid::now_v7());
    let response = fx
        .post(
            &format!("/api/v1/secret-requests/{}/answer", row.id),
            &key,
            Some(json!({"decline": true})),
        )
        .await;
    assert_eq!(response.status, axum::http::StatusCode::OK);
    // Ключ ответа нигде не запомнен: тело ответа не хешируется и не хранится.
    let stored: i64 = sqlx::query_scalar("select count(*) from idempotency_keys where key=$1")
        .bind(&key)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(stored, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_value_over_16_kib_answers_413(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Большой").await;
    let value = "x".repeat(secrets::MAX_VALUE_BYTES + 1);
    let response = fx.answer(row.id, json!({"value": value})).await;
    assert_eq!(response.status, axum::http::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(fx.state(row.id).await, "open");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_delivered_secret_is_revoked_and_the_queued_message_is_withdrawn(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Пароль").await;
    // Доставим, не подключая воркер: состояние и путь важны для отзыва.
    let attempt = Uuid::now_v7();
    let journal = Uuid::now_v7();
    sqlx::query("update secret_requests set state='sending', attempt_id=$2 where id=$1")
        .bind(row.id)
        .bind(attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        journal,
        1,
        fact_body(record_body::Body::SecretDelivered(
            canterlot_proto::worker::v1::SecretDelivered {
                attempt_id: attempt.to_string(),
                request_id: row.id.to_string(),
                path: "/run/a".into(),
                expires_at_ms: u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap(),
            },
        )),
    )
    .await;
    assert_eq!(fx.state(row.id).await, "delivered");
    assert_eq!(fx.inputs().await.len(), 1);

    let response = fx
        .post(
            &format!("/api/v1/secret-requests/{}/revoke", row.id),
            &Uuid::now_v7().to_string(),
            None,
        )
        .await;
    assert_eq!(response.status, axum::http::StatusCode::OK);
    assert_eq!(fx.state(row.id).await, "revoking");

    let (payload, kind): (Value, String) = sqlx::query_as(
        "select payload, kind from commands where kind='revoke_secret' and conversation_id=$1",
    )
    .bind(fx.seed.conversation_id)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(kind, "revoke_secret");
    assert_eq!(payload["request_id"], row.id.to_string());
    assert_eq!(payload["attempt_id"], attempt.to_string());

    // Отправленное, но ещё не принятое сообщение «передан» снимается.
    let queued: Vec<String> =
        sqlx::query_scalar("select state from inputs where conversation_id=$1 order by input_seq")
            .bind(fx.seed.conversation_id)
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(queued, vec!["cancelled"]);

    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        journal,
        2,
        fact_body(record_body::Body::SecretRemoved(
            canterlot_proto::worker::v1::SecretRemoved {
                attempt_id: attempt.to_string(),
                reason: "revoked".into(),
            },
        )),
    )
    .await;
    let done = fx.row(row.id).await;
    assert_eq!(done.state, secrets::SecretState::Revoked);
    assert!(done.path.is_none());
    let prompts = fx.inputs().await;
    assert_eq!(
        prompts.last().unwrap(),
        "Секрет «Пароль» отозван, файл удалён."
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_stale_receipt_for_an_old_attempt_is_ignored(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    let old_attempt = Uuid::now_v7();
    // Старая попытка действительно была доставлена и заменена новой.
    sqlx::query(
        "update secret_requests set state='delivered', attempt_id=$2, path='/run/old' where id=$1",
    )
    .bind(row.id)
    .bind(old_attempt)
    .execute(&fx.pool)
    .await
    .unwrap();
    let new_attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='sending', attempt_id=$2, path=null where id=$1")
        .bind(row.id)
        .bind(new_attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretDelivered(
            canterlot_proto::worker::v1::SecretDelivered {
                attempt_id: old_attempt.to_string(),
                request_id: row.id.to_string(),
                path: "/run/stale".into(),
                expires_at_ms: 0,
            },
        )),
    )
    .await;
    let row = fx.row(row.id).await;
    assert_eq!(row.state, secrets::SecretState::Sending);
    assert_eq!(row.attempt_id, Some(new_attempt));
    assert!(
        row.path.is_none(),
        "the old receipt did not resurrect a path"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_hub_restart_turns_sending_into_lost(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    let attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='sending', attempt_id=$2 where id=$1")
        .bind(row.id)
        .bind(attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(secrets::recover_after_restart(&fx.pool).await.unwrap(), 1);
    let row = fx.row(row.id).await;
    assert_eq!(row.state, secrets::SecretState::Lost);
    assert_eq!(row.note.as_deref(), Some(secrets::NOTE_LOST));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_timed_out_attempt_becomes_lost(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    let attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='sending', attempt_id=$2 where id=$1")
        .bind(row.id)
        .bind(attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    secrets::lose_attempt(&fx.pool, attempt, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(fx.state(row.id).await, "lost");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn worker_manifests_reconcile_sending_requests(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    let attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='sending', attempt_id=$2 where id=$1")
        .bind(row.id)
        .bind(attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    let manifest = canterlot_proto::worker::v1::SecretManifest {
        attempt_id: attempt.to_string(),
        request_id: row.id.to_string(),
        conversation_id: fx.seed.conversation_id.to_string(),
        path: "/run/reconciled".into(),
        expires_at_ms: 7,
    };
    secrets::reconcile_manifests(
        &fx.pool,
        &fx.registry,
        fx.seed.machine_id,
        Uuid::nil(),
        &[manifest],
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let done = fx.row(row.id).await;
    assert_eq!(done.state, secrets::SecretState::Delivered);
    assert_eq!(done.path.as_deref(), Some("/run/reconciled"));

    // Второй запрос файла не имеет — он потерян.
    let other = fx.open("Другой").await;
    let other_attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='sending', attempt_id=$2 where id=$1")
        .bind(other.id)
        .bind(other_attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    secrets::reconcile_manifests(
        &fx.pool,
        &fx.registry,
        fx.seed.machine_id,
        Uuid::nil(),
        &[],
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(fx.state(other.id).await, "lost");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_open_secret_keeps_the_session_in_waiting_for_user(pool: PgPool) {
    let fx = Fx::new(pool).await;
    fx.open("Пароль").await;
    activity::refresh(
        &fx.pool,
        fx.seed.conversation_id,
        chrono::Utc::now(),
        &Thresholds {
            stalled_generating: Duration::from_secs(300),
            stalled_tool: Duration::from_secs(1800),
            idle_quiet: Duration::from_secs(5),
            idle_background_max: Duration::from_secs(900),
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let (state, evidence): (String, Value) = sqlx::query_as(
        "select activity_state, activity_evidence from conversation_derived where conversation_id=$1",
    )
    .bind(fx.seed.conversation_id)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(state, "waiting_for_user");
    assert_eq!(evidence["phase"], "awaiting_secret");
    assert_eq!(evidence["question"], "Пароль");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_trashed_session_cancels_open_secrets_and_revokes_delivered_files(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let open = fx.open("Открытый").await;
    let delivered = fx.open("Переданный").await;
    let attempt = Uuid::now_v7();
    sqlx::query(
        "update secret_requests set state='delivered', attempt_id=$2, path='/run/b' where id=$1",
    )
    .bind(delivered.id)
    .bind(attempt)
    .execute(&fx.pool)
    .await
    .unwrap();
    let response = fx
        .client
        .send(
            axum::http::Method::DELETE,
            &format!("/api/v1/conversations/{}", fx.seed.conversation_id),
            Some(&Uuid::now_v7().to_string()),
            None,
        )
        .await;
    assert!(
        response.status.is_success(),
        "{} {}",
        response.status,
        String::from_utf8_lossy(&response.body)
    );
    assert_eq!(fx.state(open.id).await, "cancelled");
    assert_eq!(fx.state(delivered.id).await, "revoking");
    let commands: i64 = sqlx::query_scalar(
        "select count(*) from commands where kind='revoke_secret' and conversation_id=$1 and payload->>'attempt_id'=$2",
    )
    .bind(fx.seed.conversation_id)
    .bind(attempt.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(commands, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_whole_path_leaves_no_plaintext_or_hash_in_the_database(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let marker = format!("canterlot-secret-PROBE-{}", Uuid::now_v7());
    let value = format!("{marker}\n");
    let row = fx.open("Пароль").await;
    let mut worker = task3::connect_with(&fx.hub, &fx.seed, Uuid::now_v7(), CAPS, 1000).await;
    // Значение действительно проходит ответ владельца, а не подставляется в факт.
    let response = fx.answer(row.id, json!({"value": value})).await;
    assert_eq!(
        response.status,
        axum::http::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let delivered = tokio::time::timeout(Duration::from_secs(5), worker.stream.message())
        .await
        .expect("delivery")
        .unwrap()
        .unwrap();
    let Some(control_down::Msg::SecretDeliver(deliver)) = delivered.msg else {
        panic!("expected SecretDeliver, got {delivered:?}");
    };
    assert_eq!(deliver.value, value.as_bytes());
    let attempt: Uuid = deliver.attempt_id.parse().unwrap();
    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretDelivered(
            canterlot_proto::worker::v1::SecretDelivered {
                attempt_id: attempt.to_string(),
                request_id: row.id.to_string(),
                path: "/run/c".into(),
                expires_at_ms: 1,
            },
        )),
    )
    .await;
    assert_absent_everywhere(&fx.pool, &marker).await;
    // И хвостовой перевод строки вместе со значением тоже нигде не осел.
    assert_absent_everywhere(&fx.pool, &value).await;
}

/// Ни одна строка ни одной таблицы не содержит значения, его SHA-256 и SHA-1.
async fn assert_absent_everywhere(pool: &PgPool, marker: &str) {
    use sha2::Digest as _;
    let sha256 = sha2::Sha256::digest(marker.as_bytes());
    let sha1 = sha1_smol::Sha1::from(marker.as_bytes()).digest().bytes();
    let needles = [marker.to_owned(), hex(&sha256), hex(&sha1)];
    let tables: Vec<String> = sqlx::query_scalar(
        "select table_name from information_schema.tables
         where table_schema = 'public' and table_type = 'BASE TABLE'",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    for table in tables {
        let sql = format!("select to_jsonb(t)::text from \"{table}\" t");
        let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_all(pool)
            .await
            .unwrap();
        for row in rows {
            for needle in &needles {
                assert!(
                    !row.contains(needle.as_str()),
                    "table {table} contains {needle}: {row}"
                );
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[allow(dead_code)]
fn _unused(_: Answer, _: ControlUp, _: ControlDown) {}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_expired_file_marks_the_card_expired(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    let attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='sending', attempt_id=$2 where id=$1")
        .bind(row.id)
        .bind(attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretDelivered(
            canterlot_proto::worker::v1::SecretDelivered {
                attempt_id: attempt.to_string(),
                request_id: row.id.to_string(),
                path: "/run/expiring".into(),
                expires_at_ms: 2,
            },
        )),
    )
    .await;
    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretRemoved(
            canterlot_proto::worker::v1::SecretRemoved {
                attempt_id: attempt.to_string(),
                reason: "expired".into(),
            },
        )),
    )
    .await;
    let done = fx.row(row.id).await;
    assert_eq!(done.state, secrets::SecretState::Expired);
    let prompts = fx.inputs().await;
    assert_eq!(prompts.last().unwrap(), "Секрет «Ключ» истёк, файл удалён.");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_session_tool_returns_a_request_id_and_holds_the_limit(pool: PgPool) {
    use canterlot_hub::{
        config::SessionToolsConfig,
        session_tools::{Caller, ToolContext, ToolErrorCode, settle::SettleConfig},
    };
    use std::sync::Arc;
    let fx = Fx::new(pool.clone()).await;
    let context = ToolContext {
        pool: fx.pool.clone(),
        config: Arc::new(SessionToolsConfig::default()),
        limits: Default::default(),
        settle: SettleConfig {
            idle_quiet: Duration::from_secs(5),
            idle_background_max: Duration::from_secs(900),
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
        registry: fx.registry.clone(),
        shutdown: tokio_util::sync::CancellationToken::new(),
    };
    let caller = Caller {
        owner_id: fx.seed.owner_id,
        conversation_id: fx.seed.conversation_id,
        execution_id: Uuid::now_v7(),
        machine_id: fx.seed.machine_id,
        call_id: Uuid::now_v7(),
    };
    let answer = canterlot_hub::session_tools::secret::request(
        &context,
        &caller,
        json!({"name": "Пароль", "purpose": "для деплоя"}),
    )
    .await
    .unwrap();
    assert_eq!(answer["status"], "requested");
    let id: Uuid = answer["request_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(fx.state(id).await, "open");
    for index in 1..secrets::MAX_OPEN_PER_CONVERSATION {
        canterlot_hub::session_tools::secret::request(
            &context,
            &caller,
            json!({"name": format!("Секрет {index}"), "purpose": "ещё"}),
        )
        .await
        .unwrap();
    }
    let error = canterlot_hub::session_tools::secret::request(
        &context,
        &caller,
        json!({"name": "Лишний", "purpose": "не влезет"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ToolErrorCode::TooManySecrets);
    let empty = canterlot_hub::session_tools::secret::request(
        &context,
        &caller,
        json!({"name": "", "purpose": "пусто"}),
    )
    .await
    .unwrap_err();
    assert_eq!(empty.code, ToolErrorCode::InvalidArguments);
    assert!(
        !empty.message.contains("пусто"),
        "the argument is not echoed"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn twenty_open_requests_per_owner_are_the_limit_across_conversations(pool: PgPool) {
    let fx = Fx::new(pool.clone()).await;
    let per_conversation = secrets::MAX_OPEN_PER_CONVERSATION;
    let conversations = (secrets::MAX_OPEN_PER_OWNER + per_conversation - 1) / per_conversation;
    let mut opened = 0;
    for index in 0..conversations {
        let conversation =
            testkit::seed_conversation(&pool, fx.seed.owner_id, fx.seed.machine_id).await;
        let here = per_conversation.min(secrets::MAX_OPEN_PER_OWNER - opened);
        for slot in 0..here {
            secrets::open(
                &fx.pool,
                fx.seed.owner_id,
                conversation,
                &format!("Секрет {index}-{slot}"),
                "предел владельца",
                Uuid::now_v7(),
            )
            .await
            .unwrap();
            opened += 1;
        }
    }
    assert_eq!(opened, secrets::MAX_OPEN_PER_OWNER);
    let extra = testkit::seed_conversation(&pool, fx.seed.owner_id, fx.seed.machine_id).await;
    let error = secrets::open(
        &fx.pool,
        fx.seed.owner_id,
        extra,
        "Двадцать первый",
        "лишний",
        Uuid::now_v7(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, secrets::SecretError::Limit), "{error:?}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_lost_attempt_is_revoked_on_timeout_and_on_restart(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let timed = fx.open("По сроку").await;
    let timed_attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='sending', attempt_id=$2 where id=$1")
        .bind(timed.id)
        .bind(timed_attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    secrets::lose_attempt(&fx.pool, timed_attempt, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(fx.state(timed.id).await, "lost");
    let timed_commands: i64 = sqlx::query_scalar(
        "select count(*) from commands where kind='revoke_secret' and payload->>'attempt_id'=$1",
    )
    .bind(timed_attempt.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(timed_commands, 1, "the timed-out attempt is revoked");

    let restarted = fx.open("После рестарта").await;
    let restart_attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='sending', attempt_id=$2 where id=$1")
        .bind(restarted.id)
        .bind(restart_attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(secrets::recover_after_restart(&fx.pool).await.unwrap(), 1);
    assert_eq!(fx.state(restarted.id).await, "lost");
    let restart_commands: i64 = sqlx::query_scalar(
        "select count(*) from commands where kind='revoke_secret' and payload->>'attempt_id'=$1",
    )
    .bind(restart_attempt.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        restart_commands, 1,
        "restart recovery revokes the attempt it loses"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn session_end_revokes_every_attempt_that_may_have_materialised(pool: PgPool) {
    let fx = Fx::new(pool).await;
    // Сначала состояния, которые не считаются ожиданием владельца.
    let rows = [
        ("sending", fx.open("Отправляется").await.id),
        ("delivered", fx.open("Переданный").await.id),
        ("revoking", fx.open("Отзывается").await.id),
    ];
    let mut attempts = Vec::new();
    for (state, id) in &rows {
        let attempt = Uuid::now_v7();
        sqlx::query(
            "update secret_requests set state=$2, attempt_id=$3, path='/run/end' where id=$1",
        )
        .bind(id)
        .bind(state)
        .bind(attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
        attempts.push((id, attempt));
    }
    let open = fx.open("Открытый").await;
    let lost = fx.open("Потерянный").await;
    let lost_attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='lost', attempt_id=$2 where id=$1")
        .bind(lost.id)
        .bind(lost_attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    attempts.push((&lost.id, lost_attempt));
    let response = fx
        .client
        .send(
            axum::http::Method::DELETE,
            &format!("/api/v1/conversations/{}", fx.seed.conversation_id),
            Some(&Uuid::now_v7().to_string()),
            None,
        )
        .await;
    assert!(response.status.is_success());
    assert_eq!(fx.state(open.id).await, "cancelled");
    for (id, attempt) in &attempts {
        assert_eq!(fx.state(**id).await, "revoking", "row {id}");
        let commands: i64 = sqlx::query_scalar(
            "select count(*) from commands where kind='revoke_secret' and payload->>'attempt_id'=$1",
        )
        .bind(attempt.to_string())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        assert_eq!(commands, 1, "revoke for {attempt}");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_late_receipt_after_loss_or_cancel_triggers_a_revoke(pool: PgPool) {
    let fx = Fx::new(pool).await;
    for state in ["lost", "cancelled"] {
        let row = fx.open(&format!("Поздняя квитанция {state}")).await;
        let attempt = Uuid::now_v7();
        sqlx::query("update secret_requests set state=$2, attempt_id=$3 where id=$1")
            .bind(row.id)
            .bind(state)
            .bind(attempt)
            .execute(&fx.pool)
            .await
            .unwrap();
        cj(
            &fx.pool,
            &fx.registry,
            &fx.seed,
            Uuid::now_v7(),
            1,
            fact_body(record_body::Body::SecretDelivered(
                canterlot_proto::worker::v1::SecretDelivered {
                    attempt_id: attempt.to_string(),
                    request_id: row.id.to_string(),
                    path: "/run/late".into(),
                    expires_at_ms: 1,
                },
            )),
        )
        .await;
        assert_eq!(fx.state(row.id).await, state, "state stays {state}");
        let commands: i64 = sqlx::query_scalar(
            "select count(*) from commands where kind='revoke_secret' and payload->>'attempt_id'=$1",
        )
        .bind(attempt.to_string())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        assert_eq!(commands, 1, "a late receipt revokes {state}");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn revoke_accepts_a_lost_attempt_with_an_id(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Потерянный").await;
    let attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='lost', attempt_id=$2 where id=$1")
        .bind(row.id)
        .bind(attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    let response = fx
        .post(
            &format!("/api/v1/secret-requests/{}/revoke", row.id),
            &Uuid::now_v7().to_string(),
            None,
        )
        .await;
    assert_eq!(response.status, axum::http::StatusCode::OK);
    assert_eq!(fx.state(row.id).await, "revoking");
    let commands: i64 = sqlx::query_scalar(
        "select count(*) from commands where kind='revoke_secret' and payload->>'attempt_id'=$1",
    )
    .bind(attempt.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(commands, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_removed_fact_completes_the_revoke_command(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Отзыв").await;
    let attempt = Uuid::now_v7();
    sqlx::query(
        "update secret_requests set state='delivered', attempt_id=$2, path='/run/r' where id=$1",
    )
    .bind(row.id)
    .bind(attempt)
    .execute(&fx.pool)
    .await
    .unwrap();
    let response = fx
        .post(
            &format!("/api/v1/secret-requests/{}/revoke", row.id),
            &Uuid::now_v7().to_string(),
            None,
        )
        .await;
    assert_eq!(response.status, axum::http::StatusCode::OK);
    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretRemoved(
            canterlot_proto::worker::v1::SecretRemoved {
                attempt_id: attempt.to_string(),
                reason: "revoked".into(),
            },
        )),
    )
    .await;
    assert_eq!(fx.state(row.id).await, "revoked");
    let completed: i64 = sqlx::query_scalar(
        "select count(*) from commands where kind='revoke_secret'
           and payload->>'attempt_id'=$1 and delivery_state='completed'",
    )
    .bind(attempt.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(completed, 1, "the revoke command finishes on the hub");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failed_removal_keeps_revoking_with_a_note(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ошибка удаления").await;
    let attempt = Uuid::now_v7();
    sqlx::query(
        "update secret_requests set state='delivered', attempt_id=$2, path='/run/f' where id=$1",
    )
    .bind(row.id)
    .bind(attempt)
    .execute(&fx.pool)
    .await
    .unwrap();
    fx.post(
        &format!("/api/v1/secret-requests/{}/revoke", row.id),
        &Uuid::now_v7().to_string(),
        None,
    )
    .await;
    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretRemoveFailed(
            canterlot_proto::worker::v1::SecretRemoveFailed {
                attempt_id: attempt.to_string(),
                reason: "io".into(),
            },
        )),
    )
    .await;
    let row = fx.row(row.id).await;
    assert_eq!(row.state, secrets::SecretState::Revoking);
    assert!(
        row.note.as_deref().is_some_and(|note| note.contains("io")),
        "{:?}",
        row.note
    );
    let completed: i64 = sqlx::query_scalar(
        "select count(*) from commands where kind='revoke_secret'
           and payload->>'attempt_id'=$1 and delivery_state='completed'",
    )
    .bind(attempt.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(completed, 1, "a failed removal still finishes the command");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn limits_count_lost_requests(pool: PgPool) {
    let fx = Fx::new(pool).await;
    for index in 0..secrets::MAX_OPEN_PER_CONVERSATION {
        let row = fx.open(&format!("Потерянный {index}")).await;
        sqlx::query("update secret_requests set state='lost', attempt_id=$2 where id=$1")
            .bind(row.id)
            .bind(Uuid::now_v7())
            .execute(&fx.pool)
            .await
            .unwrap();
    }
    let error = secrets::open(
        &fx.pool,
        fx.seed.owner_id,
        fx.seed.conversation_id,
        "Четвёртый",
        "лишний",
        Uuid::now_v7(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, secrets::SecretError::Limit), "{error:?}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn secret_request_is_refused_while_owner_stop_is_pending(pool: PgPool) {
    let fx = Fx::new(pool).await;
    sqlx::query(
        "update queue_state set pause_reasons='[{\"reason\":\"user_stop\"}]'::jsonb
         where conversation_id=$1",
    )
    .bind(fx.seed.conversation_id)
    .execute(&fx.pool)
    .await
    .unwrap();
    let error = secrets::open(
        &fx.pool,
        fx.seed.owner_id,
        fx.seed.conversation_id,
        "После остановки",
        "не нужен",
        Uuid::now_v7(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, secrets::SecretError::OwnerStopped),
        "{error:?}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_manifest_snapshot_does_not_lose_a_delivery_from_its_own_connection(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let _worker = task3::connect_with(&fx.hub, &fx.seed, Uuid::now_v7(), CAPS, 1000).await;
    let connection = fx
        .registry
        .connection_id(fx.seed.machine_id)
        .expect("connection");
    let fresh = fx.open("После снимка").await;
    let fresh_attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='sending', attempt_id=$2 where id=$1")
        .bind(fresh.id)
        .bind(fresh_attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    fx.registry
        .track_secret_attempt(fresh_attempt, fresh.id, fx.seed.machine_id, connection);
    let old = fx.open("До снимка").await;
    let old_attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='sending', attempt_id=$2 where id=$1")
        .bind(old.id)
        .bind(old_attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    secrets::reconcile_manifests(
        &fx.pool,
        &fx.registry,
        fx.seed.machine_id,
        connection,
        &[],
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(
        fx.state(fresh.id).await,
        "sending",
        "a delivery sent on the snapshot connection is not lost"
    );
    assert_eq!(fx.state(old.id).await, "lost", "an older attempt is lost");
    // Настоящая квитанция текущего соединения всё равно применяется.
    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretDelivered(
            canterlot_proto::worker::v1::SecretDelivered {
                attempt_id: fresh_attempt.to_string(),
                request_id: fresh.id.to_string(),
                path: "/run/fresh".into(),
                expires_at_ms: 1,
            },
        )),
    )
    .await;
    assert_eq!(fx.state(fresh.id).await, "delivered");
}

/// Инвариант: после терминального состояния (отозван, истёк, отменён, отклонён)
/// ни один файл этой попытки не может остаться, даже если квитанция о записи
/// пришла позже. Поздняя квитанция обязана поставить отзыв.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_late_receipt_after_any_terminal_state_triggers_a_revoke(pool: PgPool) {
    let fx = Fx::new(pool).await;
    for state in ["expired", "revoked", "declined", "revoking"] {
        let row = fx.open(&format!("Поздняя квитанция {state}")).await;
        let attempt = Uuid::now_v7();
        sqlx::query("update secret_requests set state=$2, attempt_id=$3 where id=$1")
            .bind(row.id)
            .bind(state)
            .bind(attempt)
            .execute(&fx.pool)
            .await
            .unwrap();
        cj(
            &fx.pool,
            &fx.registry,
            &fx.seed,
            Uuid::now_v7(),
            1,
            fact_body(record_body::Body::SecretDelivered(
                canterlot_proto::worker::v1::SecretDelivered {
                    attempt_id: attempt.to_string(),
                    request_id: row.id.to_string(),
                    path: "/run/late-terminal".into(),
                    expires_at_ms: 1,
                },
            )),
        )
        .await;
        assert_eq!(fx.state(row.id).await, state, "state stays {state}");
        let commands: i64 = sqlx::query_scalar(
            "select count(*) from commands where kind='revoke_secret' and payload->>'attempt_id'=$1",
        )
        .bind(attempt.to_string())
        .fetch_one(&fx.pool)
        .await
        .unwrap();
        assert_eq!(commands, 1, "a late receipt revokes a {state} attempt");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_late_receipt_for_a_replaced_attempt_revokes_it_without_a_request_row(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Заменённая попытка").await;
    let old_attempt = Uuid::now_v7();
    let new_attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='sending', attempt_id=$2 where id=$1")
        .bind(row.id)
        .bind(new_attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretDelivered(
            canterlot_proto::worker::v1::SecretDelivered {
                attempt_id: old_attempt.to_string(),
                request_id: row.id.to_string(),
                path: "/run/superseded".into(),
                expires_at_ms: 1,
            },
        )),
    )
    .await;
    let payload: Value = sqlx::query_scalar(
        "select payload from commands
         where kind='revoke_secret' and payload->>'attempt_id'=$1",
    )
    .bind(old_attempt.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        payload["conversation_id"],
        fx.seed.conversation_id.to_string(),
        "the detached command still names the directory"
    );
    assert_eq!(
        fx.state(row.id).await,
        "sending",
        "the new attempt is intact"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn retrying_removals_repeats_a_failed_revoke_with_backoff(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Повтор отзыва").await;
    let attempt = Uuid::now_v7();
    sqlx::query(
        "update secret_requests set state='revoking', attempt_id=$2,
                updated_at=now() - interval '1 hour' where id=$1",
    )
    .bind(row.id)
    .bind(attempt)
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, kind, payload, delivery_state, completed_at)
         values ($1, $2, $3, 'revoke_secret', $4, 'completed', now())",
    )
    .bind(Uuid::now_v7())
    .bind(fx.seed.machine_id)
    .bind(fx.seed.conversation_id)
    .bind(json!({"request_id": row.id, "attempt_id": attempt, "conversation_id": fx.seed.conversation_id}))
    .execute(&fx.pool)
    .await
    .unwrap();

    assert_eq!(secrets::retry_removals(&fx.pool).await.unwrap(), 1);
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from commands where kind='revoke_secret'
           and payload->>'attempt_id'=$1 and delivery_state <> 'completed'",
    )
    .bind(attempt.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(pending, 1, "a fresh revoke is enqueued");
    assert_eq!(
        secrets::retry_removals(&fx.pool).await.unwrap(),
        0,
        "while the command waits for the worker no duplicate is made"
    );

    // Молодая строка ждёт своей очереди: срок растёт с числом попыток.
    let fresh = fx.open("Молодой отзыв").await;
    let fresh_attempt = Uuid::now_v7();
    sqlx::query("update secret_requests set state='revoking', attempt_id=$2 where id=$1")
        .bind(fresh.id)
        .bind(fresh_attempt)
        .execute(&fx.pool)
        .await
        .unwrap();
    assert_eq!(secrets::retry_removals(&fx.pool).await.unwrap(), 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failed_expiry_removal_is_retried(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Истёк, но не удалён").await;
    let attempt = Uuid::now_v7();
    sqlx::query(
        "update secret_requests set state='expired', attempt_id=$2,
                note='Файл не удалён: воркер вернул ошибку (io)',
                updated_at=now() - interval '1 hour' where id=$1",
    )
    .bind(row.id)
    .bind(attempt)
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into commands (id, machine_id, conversation_id, kind, payload, delivery_state, completed_at)
         values ($1, $2, $3, 'revoke_secret', $4, 'completed', now())",
    )
    .bind(Uuid::now_v7())
    .bind(fx.seed.machine_id)
    .bind(fx.seed.conversation_id)
    .bind(json!({"request_id": row.id, "attempt_id": attempt, "conversation_id": fx.seed.conversation_id}))
    .execute(&fx.pool)
    .await
    .unwrap();
    assert_eq!(secrets::retry_removals(&fx.pool).await.unwrap(), 1);
    assert_eq!(
        fx.state(row.id).await,
        "expired",
        "the card keeps its state"
    );
}

/// Окончательная уборка не должна выбрасывать отзыв, который сама же и заказала.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn purge_keeps_the_revoke_and_a_receipt_finishes_it_without_the_row(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Пережить уборку").await;
    let attempt = Uuid::now_v7();
    sqlx::query(
        "update secret_requests set state='delivered', attempt_id=$2, path='/run/p' where id=$1",
    )
    .bind(row.id)
    .bind(attempt)
    .execute(&fx.pool)
    .await
    .unwrap();
    let trash = fx
        .client
        .send(
            axum::http::Method::DELETE,
            &format!("/api/v1/conversations/{}", fx.seed.conversation_id),
            Some(&Uuid::now_v7().to_string()),
            None,
        )
        .await;
    assert_eq!(trash.status, axum::http::StatusCode::NO_CONTENT);
    assert_eq!(fx.state(row.id).await, "revoking");

    let purge = fx
        .client
        .send(
            axum::http::Method::DELETE,
            &format!("/api/v1/conversations/{}/trash", fx.seed.conversation_id),
            Some(&Uuid::now_v7().to_string()),
            None,
        )
        .await;
    assert_eq!(
        purge.status,
        axum::http::StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&purge.body)
    );
    let rows: i64 = sqlx::query_scalar("select count(*) from secret_requests where id=$1")
        .bind(row.id)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "the request row is gone");
    let (conversation, state): (Option<Uuid>, String) = sqlx::query_as(
        "select conversation_id, delivery_state from commands
         where kind='revoke_secret' and payload->>'attempt_id'=$1",
    )
    .bind(attempt.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert!(
        conversation.is_none(),
        "the command was detached before the conversation rows went"
    );
    assert_eq!(state, "pending");

    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretRemoved(
            canterlot_proto::worker::v1::SecretRemoved {
                attempt_id: attempt.to_string(),
                reason: "revoked".into(),
            },
        )),
    )
    .await;
    let state: String = sqlx::query_scalar(
        "select delivery_state from commands
         where kind='revoke_secret' and payload->>'attempt_id'=$1",
    )
    .bind(attempt.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        state, "completed",
        "a receipt finishes the command even without its row"
    );
}

/// Доставка и переподключение не должны разойтись: значение уходит ровно на то
/// соединение, которым помечена попытка, и никогда на чужое.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_delivery_racing_a_reconnect_is_not_tagged_with_the_wrong_connection(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Гонка соединений").await;
    let journal = Uuid::now_v7();
    let first = task3::connect_with(&fx.hub, &fx.seed, journal, CAPS, 1000).await;
    let resume = secrets::test_hooks::arm_before_secret_send();

    let pool_clone = fx.pool.clone();
    let registry = fx.registry.clone();
    let owner = fx.seed.owner_id;
    let id = row.id;
    let task = tokio::spawn(async move {
        secrets::answer(
            &pool_clone,
            &registry,
            owner,
            id,
            Answer::Value("canterlot-secret-PROBE-race".into()),
            Uuid::now_v7(),
        )
        .await
    });

    let resume = resume.await.expect("the delivery paused before sending");
    drop(first);
    wait_disconnected(&fx.registry, fx.seed.machine_id).await;
    let _second = task3::connect_with(&fx.hub, &fx.seed, journal, CAPS, 1000).await;
    resume.send(()).expect("resume the delivery");

    let view = task.await.unwrap().expect("answer");
    assert_eq!(
        view.state,
        secrets::SecretState::Lost,
        "the captured connection was gone: the value is lost, not sent elsewhere"
    );
    let attempt = fx.row(row.id).await.attempt_id.expect("attempt");
    assert!(
        fx.registry.secret_attempt_connection(attempt).is_none(),
        "a lost attempt is not left tagged to an old connection"
    );
}

/// Если отправка не удалась, карточка получает строку из базы, а не выдуманную
/// ревизию: параллельный отзыв не должен затираться состоянием `lost`.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failed_send_reports_the_authoritative_row(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ответ после отказа отправки").await;
    let first = task3::connect_with(&fx.hub, &fx.seed, Uuid::now_v7(), CAPS, 1000).await;
    let resume = secrets::test_hooks::arm_before_secret_send();

    let pool_clone = fx.pool.clone();
    let registry = fx.registry.clone();
    let owner = fx.seed.owner_id;
    let id = row.id;
    let task = tokio::spawn(async move {
        secrets::answer(
            &pool_clone,
            &registry,
            owner,
            id,
            Answer::Value("canterlot-secret-PROBE-failed-send".into()),
            Uuid::now_v7(),
        )
        .await
    });

    let resume = resume.await.expect("the delivery paused before sending");
    // Пока значение не ушло, владелец отзывает запрос: попытка уже `revoking`.
    sqlx::query(
        "update secret_requests set state='revoking', revision=revision+1, updated_at=now()
         where id=$1",
    )
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    drop(first);
    wait_disconnected(&fx.registry, fx.seed.machine_id).await;
    resume.send(()).expect("resume the delivery");

    let view = task.await.unwrap().expect("answer");
    let current = fx.row(row.id).await;
    assert_eq!(
        view.state,
        secrets::SecretState::Revoking,
        "the authoritative state is returned"
    );
    assert_eq!(view.revision, current.revision);
    assert_eq!(view.updated_at, current.updated_at);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_cj_receipt_clears_the_attempt_deadline(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Учёт попытки").await;
    let mut worker = task3::connect_with(&fx.hub, &fx.seed, Uuid::now_v7(), CAPS, 1000).await;
    let response = fx
        .answer(row.id, json!({"value": "canterlot-secret-PROBE-cj"}))
        .await;
    assert_eq!(response.status, axum::http::StatusCode::OK);
    let delivered = tokio::time::timeout(Duration::from_secs(5), worker.stream.message())
        .await
        .expect("delivery")
        .unwrap()
        .unwrap();
    let Some(control_down::Msg::SecretDeliver(deliver)) = delivered.msg else {
        panic!("expected SecretDeliver, got {delivered:?}");
    };
    let attempt: Uuid = deliver.attempt_id.parse().unwrap();
    assert_eq!(
        fx.registry.secret_attempt_connection(attempt),
        Some(worker.connection),
        "the attempt is tracked until a receipt"
    );
    // Квитанция приходит в CJ: учёт снимается после коммита, как и в SJ.
    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretDelivered(
            canterlot_proto::worker::v1::SecretDelivered {
                attempt_id: attempt.to_string(),
                request_id: row.id.to_string(),
                path: "/run/cj".into(),
                expires_at_ms: 1,
            },
        )),
    )
    .await;
    assert!(
        fx.registry.secret_attempt_connection(attempt).is_none(),
        "a CJ receipt clears the in-flight attempt"
    );
}
