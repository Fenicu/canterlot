//! Секретные запросы: поколения, обязательства отзыва и отсутствие значения
//! там, где его быть не должно.
//!
//! Главная проверка здесь — не состояние карточки, а два инварианта: значение
//! никогда не попадает в таблицы, события и outbox, а каждое недействительное
//! поколение получает обязательство отзыва в той же транзакции.

use std::time::Duration;

use crate::support::task3;
use canterlot_hub::{
    activity::{self, Thresholds},
    config::HubTimers,
    ingest::{Ingest, IngestOutcome, JournalKind},
    secrets::{self, Answer},
    session::{CAPABILITY_SECRET_REQUESTS_V2, Registry},
    testkit::{self, Seed, TestClient, TestHub},
};
use canterlot_proto::worker::v1::{
    ControlDown, ControlUp, JournalRecord, JournalRecords, RecordBody, control_down, record_body,
};
use serde_json::{Value, json};
use sha2::Digest as _;
use sqlx::PgPool;
use uuid::Uuid;

const CAPS: &[&str] = &["pilot_v1", CAPABILITY_SECRET_REQUESTS_V2];

struct Fx {
    pool: PgPool,
    hub: TestHub,
    client: TestClient,
    seed: Seed,
    registry: Registry,
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
        Self {
            pool,
            hub,
            client,
            seed,
            registry,
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

    async fn obligation(&self, request: Uuid) -> Option<(i64, String, i32)> {
        sqlx::query_as(
            "select up_to, reason, attempts from secret_revocations
             where machine_id=$1 and request_id=$2",
        )
        .bind(self.seed.machine_id)
        .bind(request)
        .fetch_optional(&self.pool)
        .await
        .unwrap()
    }

    async fn revoke_command(&self, request: Uuid, cap: u64, state: &str) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into commands
                (id, machine_id, conversation_id, execution_id, execution_generation,
                 kind, payload, delivery_state)
             values ($1, $2, $3, null, null, 'revoke_secret', $4, $5)",
        )
        .bind(id)
        .bind(self.seed.machine_id)
        .bind(self.seed.conversation_id)
        .bind(json!({
            "request_id": request,
            "conversation_id": self.seed.conversation_id,
            "up_to": cap,
            "reason": "revoked",
        }))
        .bind(state)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    async fn connect(&self, capabilities: &[&str], epoch: &str) -> task3::Client {
        self.connect_journal(capabilities, epoch, Uuid::now_v7())
            .await
    }

    async fn connect_journal(
        &self,
        capabilities: &[&str],
        epoch: &str,
        journal: Uuid,
    ) -> task3::Client {
        task3::connect_with_epoch(&self.hub, &self.seed, journal, capabilities, 1000, epoch).await
    }

    /// Доставляет значение через настоящий ответ владельца и возвращает
    /// поколение из управляющего сообщения.
    async fn send_value(
        &self,
        worker: &mut task3::Client,
        id: Uuid,
        value: &str,
    ) -> canterlot_proto::worker::v1::SecretDeliver {
        let response = self.answer(id, json!({"value": value})).await;
        assert_eq!(
            response.status,
            axum::http::StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let view: Value = response.json();
        assert_eq!(view["state"], "sending");
        assert!(view.get("value").is_none());
        receive(worker, |message| match message.msg {
            Some(control_down::Msg::SecretDeliver(deliver)) => Some(deliver),
            _ => None,
        })
        .await
    }

    async fn delivered(
        &self,
        journal: Uuid,
        seq: u64,
        deliver: &canterlot_proto::worker::v1::SecretDeliver,
        path: &str,
    ) {
        cj(
            &self.pool,
            &self.registry,
            &self.seed,
            journal,
            seq,
            fact_body(record_body::Body::SecretDelivered(
                canterlot_proto::worker::v1::SecretDelivered {
                    request_id: deliver.request_id.clone(),
                    conversation_id: deliver.conversation_id.clone(),
                    generation: deliver.generation,
                    path: path.into(),
                    expires_at_ms: deliver.expires_at_ms,
                },
            )),
        )
        .await;
    }

    async fn removed(&self, journal: Uuid, seq: u64, id: Uuid, cap: u64, reason: &str) {
        self.removed_in(journal, seq, id, self.seed.conversation_id, cap, reason)
            .await;
    }

    async fn removed_in(
        &self,
        journal: Uuid,
        seq: u64,
        id: Uuid,
        conversation_id: Uuid,
        cap: u64,
        reason: &str,
    ) {
        cj(
            &self.pool,
            &self.registry,
            &self.seed,
            journal,
            seq,
            fact_body(record_body::Body::SecretRemoved(
                canterlot_proto::worker::v1::SecretRemoved {
                    request_id: id.to_string(),
                    conversation_id: conversation_id.to_string(),
                    cap,
                    reason: reason.into(),
                },
            )),
        )
        .await;
    }
}

/// Ждёт сообщение, подходящее под предикат: обычный контрольный поток может
/// содержать и другие команды.
async fn receive<T>(
    worker: &mut task3::Client,
    mut pick: impl FnMut(ControlDown) -> Option<T>,
) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let message = tokio::time::timeout(left, worker.stream.message())
            .await
            .expect("control message")
            .unwrap()
            .unwrap();
        if let Some(value) = pick(message) {
            return value;
        }
    }
}

async fn wait_disconnected(registry: &Registry, machine: Uuid) {
    for _ in 0..500 {
        if registry.connection_id(machine).is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the old control connection did not end");
}

async fn wait_state(pool: &PgPool, id: Uuid, expected: &str) {
    for _ in 0..200 {
        let state: String = sqlx::query_scalar("select state from secret_requests where id=$1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        if state == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the request never became {expected}");
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
    let label = format!("{body:?}");
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
        "journal {journal} seq {seq}: {outcome:?} {label}"
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
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_value_is_delivered_once_over_the_live_stream_and_never_stored(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let marker = format!("canterlot-secret-PROBE-{}", Uuid::now_v7());
    let row = fx.open("Пароль от базы").await;
    assert_eq!(row.state, secrets::SecretState::Open);
    assert_eq!(fx.row(row.id).await.generation, 0);

    let mut worker = fx.connect(CAPS, "epoch-1").await;
    let deliver = fx.send_value(&mut worker, row.id, &marker).await;
    assert_eq!(deliver.request_id, row.id.to_string());
    assert_eq!(deliver.value, marker.as_bytes());
    assert_eq!(deliver.conversation_id, fx.seed.conversation_id.to_string());
    assert_eq!(deliver.generation, 1);
    assert!(deliver.expires_at_ms > 0);
    assert!(!format!("{deliver:?}").contains(&marker));

    let path = format!(
        "/run/canterlot-secrets/{}/{}",
        fx.seed.conversation_id, row.id
    );
    fx.delivered(Uuid::now_v7(), 1, &deliver, &path).await;

    let done = fx.row(row.id).await;
    assert_eq!(done.state, secrets::SecretState::Delivered);
    assert_eq!(done.generation, 1);
    assert_eq!(done.path.as_deref(), Some(path.as_str()));
    let prompts = fx.inputs().await;
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains(&path) && prompts[0].contains("Пароль от базы"),
        "{}",
        prompts[0]
    );
    assert!(!prompts[0].contains(&marker), "the value is not a message");
    assert_absent_everywhere(&fx.pool, &marker).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_decline_answers_the_card_and_tells_the_agent(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Токен").await;
    let response = fx.answer(row.id, json!({"decline": true})).await;
    assert_eq!(response.status, axum::http::StatusCode::OK);
    assert_eq!(fx.state(row.id).await, "declined");
    assert_eq!(
        fx.inputs().await,
        vec!["Владелец отказал в секрете «Токен»."]
    );
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
    assert!(fx.obligation(row.id).await.is_none(), "nothing was issued");
    assert_absent_everywhere(&fx.pool, "canterlot-secret-PROBE-offline").await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_v1_only_worker_gets_no_deliver_and_no_revoke(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    // Хаб не знает `secret_requests_v1` и не согласует его: для хаба это
    // воркер без возможности секретов.
    let mut worker = fx.connect(&["pilot_v1"], "epoch-1").await;
    let response = fx
        .answer(row.id, json!({"value": "canterlot-secret-PROBE-v1"}))
        .await;
    assert_eq!(response.status, axum::http::StatusCode::CONFLICT);
    assert_eq!(response.json::<Value>()["error"]["code"], "machine_offline");
    assert_eq!(fx.state(row.id).await, "open");
    // В поток не пришло ни значения, ни команды: возможность не согласована.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), worker.stream.message())
            .await
            .is_err(),
        "a v1 worker gets nothing"
    );
    // Даже готовое обязательство не превращается в команду для v1.
    sqlx::query(
        "insert into secret_revocations
            (id, machine_id, conversation_id, request_id, up_to, reason, next_attempt_at)
         values ($1, $2, $3, $4, 1, 'revoked', now())",
    )
    .bind(Uuid::now_v7())
    .bind(fx.seed.machine_id)
    .bind(fx.seed.conversation_id)
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        secrets::emit_due_revocations(&fx.pool, &fx.registry)
            .await
            .unwrap(),
        0
    );
    let commands: i64 =
        sqlx::query_scalar("select count(*) from commands where kind='revoke_secret'")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(commands, 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), worker.stream.message())
            .await
            .is_err(),
        "no revoke command either"
    );
    assert_absent_everywhere(&fx.pool, "canterlot-secret-PROBE-v1").await;
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
    assert_eq!(fx.row(row.id).await.generation, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_delivered_secret_is_revoked_and_the_queued_message_is_withdrawn(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Пароль").await;
    let mut worker = fx.connect(CAPS, "epoch-1").await;
    let deliver = fx.send_value(&mut worker, row.id, "value\n").await;
    let journal = Uuid::now_v7();
    fx.delivered(journal, 1, &deliver, "/run/a").await;
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
    let (up_to, reason, _attempts) = fx
        .obligation(row.id)
        .await
        .expect("revocation is an obligation, not a request-row retry");
    assert_eq!((up_to, reason.as_str()), (1, "revoked"));
    // Поздняя квитанция о доставке не воскрешает отзываемое поколение.
    fx.delivered(journal, 2, &deliver, "/run/late").await;
    assert_eq!(fx.state(row.id).await, "revoking");
    assert!(fx.obligation(row.id).await.is_some());
    // Отправленное, но ещё не принятое сообщение «передан» снимается.
    let queued: Vec<String> =
        sqlx::query_scalar("select state from inputs where conversation_id=$1 order by input_seq")
            .bind(fx.seed.conversation_id)
            .fetch_all(&fx.pool)
            .await
            .unwrap();
    assert_eq!(queued, vec!["cancelled"]);

    // Отзыв владельца уже отправил команду; тик не плодит вторую.
    let _ = secrets::emit_due_revocations(&fx.pool, &fx.registry)
        .await
        .unwrap();
    let (payload, kind): (Value, String) =
        sqlx::query_as("select payload, kind from commands where kind='revoke_secret'")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(kind, "revoke_secret");
    assert_eq!(payload["request_id"], row.id.to_string());
    assert_eq!(
        payload["conversation_id"],
        fx.seed.conversation_id.to_string()
    );
    assert_eq!(payload["up_to"], 1);
    assert_eq!(payload["reason"], "revoked");
    assert!(
        !payload.to_string().contains("value"),
        "the command carries metadata only"
    );

    fx.removed(journal, 3, row.id, 1, "revoked").await;
    let done = fx.row(row.id).await;
    assert_eq!(done.state, secrets::SecretState::Revoked);
    assert!(done.path.is_none());
    assert!(fx.obligation(row.id).await.is_none());
    assert_eq!(
        fx.inputs().await.last().unwrap(),
        "Секрет «Пароль» отозван, файл удалён."
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_timed_out_attempt_becomes_lost_with_an_obligation(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    let mut worker = fx.connect(CAPS, "epoch-1").await;
    let deliver = fx.send_value(&mut worker, row.id, "value\n").await;
    assert_eq!(deliver.generation, 1);
    // Квитанция не приходит: короткий срок теста делает попытку потерянной.
    wait_state(&fx.pool, row.id, "lost").await;
    let lost = fx.row(row.id).await;
    assert_eq!(lost.note.as_deref(), Some(secrets::NOTE_LOST));
    assert_eq!(
        fx.obligation(row.id).await,
        Some((1, "lost".into(), 0)),
        "loss and its obligation share the transaction"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_hub_restart_turns_sending_into_lost_with_an_obligation(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    sqlx::query(
        "update secret_requests set state='sending', generation=1,
                send_deadline_at=now() + interval '60 seconds' where id=$1",
    )
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    // После рестарта живых соединений нет: `sending` не дождётся квитанции.
    // Стартовая служба хаба могла сделать это раньше; итог один и тот же.
    let recovered = secrets::recover_after_restart(&fx.pool, &fx.registry)
        .await
        .unwrap();
    assert!(recovered <= 1);
    assert_eq!(fx.state(row.id).await, "lost");
    assert_eq!(fx.obligation(row.id).await, Some((1, "lost".into(), 0)));
}

/// Стенд возвращается, только когда стартовое восстановление и первая сверка
/// уже позади: тест не гоняется со службой за одни и те же строки.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_stand_returns_only_after_the_startup_sweep(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into secret_requests
            (id, conversation_id, owner_id, machine_id, name, purpose, state,
             generation, send_deadline_at)
         values ($1, $2, $3, $4, 'Ключ', 'проверка старта', 'sending',
                 1, now() + interval '60 seconds')",
    )
    .bind(id)
    .bind(seed.conversation_id)
    .bind(seed.owner_id)
    .bind(seed.machine_id)
    .execute(&pool)
    .await
    .unwrap();
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    // Живой связи нет, поэтому отправка уже потеряна и обязательство создано.
    let (state, note): (String, Option<String>) =
        sqlx::query_as("select state, note from secret_requests where id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "lost");
    assert_eq!(note.as_deref(), Some(secrets::NOTE_LOST));
    let obligation: Option<(i64, String)> = sqlx::query_as(
        "select up_to, reason from secret_revocations where machine_id=$1 and request_id=$2",
    )
    .bind(seed.machine_id)
    .bind(id)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(obligation, Some((1, "lost".into())));
    drop(hub);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn re_filling_a_lost_request_supersedes_the_old_generation(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    sqlx::query("update secret_requests set state='lost', generation=1, note=$2 where id=$1")
        .bind(row.id)
        .bind(secrets::NOTE_LOST)
        .execute(&fx.pool)
        .await
        .unwrap();
    let mut worker = fx.connect(CAPS, "epoch-2").await;
    let deliver = fx.send_value(&mut worker, row.id, "new\n").await;
    assert_eq!(deliver.generation, 2, "a re-fill is a new generation");
    assert_eq!(
        fx.obligation(row.id).await,
        Some((1, "superseded".into(), 0)),
        "the old generation must be removed on the machine"
    );
    assert_eq!(fx.state(row.id).await, "sending");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_late_delivery_receipt_after_loss_is_ignored_and_the_obligation_stays(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    let mut worker = fx.connect(CAPS, "epoch-1").await;
    let deliver = fx.send_value(&mut worker, row.id, "value\n").await;
    wait_state(&fx.pool, row.id, "lost").await;
    assert!(fx.obligation(row.id).await.is_some());

    fx.delivered(Uuid::now_v7(), 1, &deliver, "/run/late").await;
    let late = fx.row(row.id).await;
    assert_eq!(late.state, secrets::SecretState::Lost);
    assert!(late.path.is_none(), "the path did not resurrect");
    assert_eq!(
        fx.obligation(row.id).await,
        Some((1, "lost".into(), 0)),
        "rule 2 already created the obligation for this dead generation"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_expired_delivery_becomes_revoking_until_the_worker_confirms(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    sqlx::query(
        "update secret_requests set state='delivered', generation=1,
                path='/run/expiring', expires_at=now() - interval '1 second' where id=$1",
    )
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    assert_eq!(
        secrets::sweep_expired(&fx.pool, &fx.registry)
            .await
            .unwrap(),
        1
    );
    assert_eq!(fx.state(row.id).await, "revoking");
    assert_eq!(fx.obligation(row.id).await, Some((1, "expired".into(), 0)));

    fx.removed(Uuid::now_v7(), 1, row.id, 1, "expired").await;
    let done = fx.row(row.id).await;
    assert_eq!(done.state, secrets::SecretState::Expired);
    assert!(done.path.is_none());
    assert!(fx.obligation(row.id).await.is_none());
    assert_eq!(
        fx.inputs().await.last().unwrap(),
        "Секрет «Ключ» истёк, файл удалён."
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn session_end_revokes_every_generation_that_may_have_materialised(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let open = fx.open("Открытый").await;
    let delivered = fx.open("Переданный").await;
    let sending = fx.open("Отправляемый").await;
    sqlx::query(
        "update secret_requests set state='delivered', generation=2, path='/run/b' where id=$1",
    )
    .bind(delivered.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query("update secret_requests set state='sending', generation=3 where id=$1")
        .bind(sending.id)
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
    assert_eq!(fx.state(sending.id).await, "revoking");
    assert_eq!(
        fx.obligation(delivered.id).await,
        Some((2, "session_end".into(), 0))
    );
    assert_eq!(
        fx.obligation(sending.id).await,
        Some((3, "session_end".into(), 0))
    );
    assert!(
        fx.obligation(open.id).await.is_none(),
        "nothing was issued for an open request"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn purge_keeps_the_obligation_and_a_later_receipt_completes_it(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Пережить уборку").await;
    sqlx::query(
        "update secret_requests set state='delivered', generation=1, path='/run/p' where id=$1",
    )
    .bind(row.id)
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
    assert_eq!(
        fx.obligation(row.id).await,
        Some((1, "session_end".into(), 0)),
        "the obligation has no FK to the purged rows"
    );

    // Квитанция приходит уже без строки запроса и всё равно закрывает долг.
    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretRemoved(
            canterlot_proto::worker::v1::SecretRemoved {
                request_id: row.id.to_string(),
                conversation_id: fx.seed.conversation_id.to_string(),
                cap: 1,
                reason: "session_end".into(),
            },
        )),
    )
    .await;
    assert!(
        fx.obligation(row.id).await.is_none(),
        "a covering receipt closes the obligation after purge"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_older_cap_receipt_does_not_close_a_newer_obligation(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Новое поколение").await;
    sqlx::query(
        "update secret_requests set state='revoking', generation=2, path='/run/new' where id=$1",
    )
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into secret_revocations
            (id, machine_id, conversation_id, request_id, up_to, reason, next_attempt_at)
         values ($1, $2, $3, $4, 2, 'revoked', now())",
    )
    .bind(Uuid::now_v7())
    .bind(fx.seed.machine_id)
    .bind(fx.seed.conversation_id)
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();

    let journal = Uuid::now_v7();
    fx.removed(journal, 1, row.id, 1, "revoked").await;
    assert_eq!(
        fx.state(row.id).await,
        "revoking",
        "a newer generation stays"
    );
    assert_eq!(
        fx.obligation(row.id).await,
        Some((2, "revoked".into(), 0)),
        "an older cap is not proof about a newer generation"
    );

    fx.removed(journal, 2, row.id, 2, "revoked").await;
    assert_eq!(fx.state(row.id).await, "revoked");
    assert!(fx.obligation(row.id).await.is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_removal_failure_reschedules_the_obligation_and_keeps_revoking(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Не удалился").await;
    sqlx::query(
        "update secret_requests set state='revoking', generation=1, path='/run/x' where id=$1",
    )
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into secret_revocations
            (id, machine_id, conversation_id, request_id, up_to, reason, next_attempt_at)
         values ($1, $2, $3, $4, 1, 'revoked', now())",
    )
    .bind(Uuid::now_v7())
    .bind(fx.seed.machine_id)
    .bind(fx.seed.conversation_id)
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();

    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretRemoveFailed(
            canterlot_proto::worker::v1::SecretRemoveFailed {
                request_id: row.id.to_string(),
                conversation_id: fx.seed.conversation_id.to_string(),
                cap: 1,
                reason: "io".into(),
            },
        )),
    )
    .await;
    let current = fx.row(row.id).await;
    assert_eq!(current.state, secrets::SecretState::Revoking);
    assert!(
        current.note.as_deref().unwrap_or_default().contains("io"),
        "{:?}",
        current.note
    );
    let (up_to, _, attempts) = fx.obligation(row.id).await.unwrap();
    assert_eq!(up_to, 1);
    assert_eq!(attempts, 1);
    let due: bool = sqlx::query_scalar(
        "select next_attempt_at > now() + interval '10 seconds' from secret_revocations
         where machine_id=$1 and request_id=$2",
    )
    .bind(fx.seed.machine_id)
    .bind(row.id)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert!(due, "the retry is not immediate");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_successful_removal_retires_the_revoke_command(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Убранная команда").await;
    sqlx::query("update secret_requests set state='revoking', generation=1 where id=$1")
        .bind(row.id)
        .execute(&fx.pool)
        .await
        .unwrap();
    let command = fx.revoke_command(row.id, 1, "stored_on_machine").await;
    fx.removed(Uuid::now_v7(), 1, row.id, 1, "revoked").await;
    let (state, result): (String, Value) =
        sqlx::query_as("select delivery_state, result from commands where id=$1")
            .bind(command)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(state, "completed");
    assert_eq!(result["removed"], true);
    assert!(fx.obligation(row.id).await.is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failed_removal_retires_the_command_and_the_obligation_resends(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Повтор после ошибки").await;
    sqlx::query("update secret_requests set state='revoking', generation=1 where id=$1")
        .bind(row.id)
        .execute(&fx.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into secret_revocations
            (id, machine_id, conversation_id, request_id, up_to, reason, next_attempt_at)
         values ($1, $2, $3, $4, 1, 'revoked', now())",
    )
    .bind(Uuid::now_v7())
    .bind(fx.seed.machine_id)
    .bind(fx.seed.conversation_id)
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    let command = fx.revoke_command(row.id, 1, "stored_on_machine").await;

    cj(
        &fx.pool,
        &fx.registry,
        &fx.seed,
        Uuid::now_v7(),
        1,
        fact_body(record_body::Body::SecretRemoveFailed(
            canterlot_proto::worker::v1::SecretRemoveFailed {
                request_id: row.id.to_string(),
                conversation_id: fx.seed.conversation_id.to_string(),
                cap: 1,
                reason: "io".into(),
            },
        )),
    )
    .await;
    let state: String = sqlx::query_scalar("select delivery_state from commands where id=$1")
        .bind(command)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(
        state, "completed",
        "a stored_on_machine command must not block the retry"
    );

    // Обязательство — единственный драйвер повтора: снимаем паузу и отправляем.
    sqlx::query("update secret_revocations set next_attempt_at=now()")
        .execute(&fx.pool)
        .await
        .unwrap();
    let _worker = fx.connect(CAPS, "epoch-1").await;
    // Соединение могло отправить обязательство само; важен результат, не тик.
    let _ = secrets::emit_due_revocations(&fx.pool, &fx.registry)
        .await
        .unwrap();
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from commands
         where kind='revoke_secret' and delivery_state <> 'completed'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    if pending != 1 {
        let rows: Vec<(String, Value, String)> = sqlx::query_as(
            "select id::text, payload, delivery_state from commands where kind='revoke_secret'",
        )
        .fetch_all(&fx.pool)
        .await
        .unwrap();
        panic!("the retry is a fresh command: pending={pending} rows={rows:?}");
    }
    let total: i64 = sqlx::query_scalar("select count(*) from commands where kind='revoke_secret'")
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(total, 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_rejected_revoke_is_retired_and_rescheduled(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Отказ воркера").await;
    sqlx::query("update secret_requests set state='revoking', generation=1 where id=$1")
        .bind(row.id)
        .execute(&fx.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into secret_revocations
            (id, machine_id, conversation_id, request_id, up_to, reason, next_attempt_at)
         values ($1, $2, $3, $4, 1, 'revoked', now())",
    )
    .bind(Uuid::now_v7())
    .bind(fx.seed.machine_id)
    .bind(fx.seed.conversation_id)
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    let command = fx.revoke_command(row.id, 1, "pending").await;
    let mut tx = canterlot_hub::db::begin(&fx.pool).await.unwrap();
    secrets::on_revoke_rejected_in_tx(
        &mut tx,
        fx.seed.machine_id,
        command,
        0,
        "invalid",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let state: String = sqlx::query_scalar("select delivery_state from commands where id=$1")
        .bind(command)
        .fetch_one(&fx.pool)
        .await
        .unwrap();
    assert_eq!(state, "completed");
    let attempts: i32 = sqlx::query_scalar(
        "select attempts from secret_revocations where machine_id=$1 and request_id=$2",
    )
    .bind(fx.seed.machine_id)
    .bind(row.id)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(attempts, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_offline_backlog_does_not_starve_a_live_obligation(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let offline = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'Офлайн')")
        .bind(offline)
        .bind(fx.seed.owner_id)
        .execute(&fx.pool)
        .await
        .unwrap();
    for _ in 0..150 {
        sqlx::query(
            "insert into secret_revocations
                (id, machine_id, conversation_id, request_id, up_to, reason, next_attempt_at)
             values ($1, $2, $3, $4, 1, 'revoked', now())",
        )
        .bind(Uuid::now_v7())
        .bind(offline)
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .execute(&fx.pool)
        .await
        .unwrap();
    }
    let _worker = fx.connect(CAPS, "epoch-1").await;
    let live_request = Uuid::now_v7();
    sqlx::query(
        "insert into secret_revocations
            (id, machine_id, conversation_id, request_id, up_to, reason, next_attempt_at)
         values ($1, $2, $3, $4, 1, 'revoked', now())",
    )
    .bind(Uuid::now_v7())
    .bind(fx.seed.machine_id)
    .bind(fx.seed.conversation_id)
    .bind(live_request)
    .execute(&fx.pool)
    .await
    .unwrap();

    // Сеанс мог отправить обязательство сам; проверяем результат, не тик.
    let _ = secrets::emit_due_revocations(&fx.pool, &fx.registry)
        .await
        .unwrap();
    let live: i64 = sqlx::query_scalar(
        "select count(*) from commands
         where kind='revoke_secret' and payload->>'request_id'=$1",
    )
    .bind(live_request.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(live, 1);
    let offline_commands: i64 = sqlx::query_scalar(
        "select count(*) from commands c join secret_revocations r
           on r.request_id = (c.payload->>'request_id')::uuid
         where c.kind='revoke_secret' and r.machine_id=$1",
    )
    .bind(offline)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(offline_commands, 0, "offline machines get no command");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_vanished_obligation_does_not_abort_the_batch(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let _worker = fx.connect(CAPS, "epoch-1").await;
    let gone = fx.open("Исчезнет").await;
    let stays = fx.open("Останется").await;
    let gone_id = Uuid::now_v7();
    let stays_id = Uuid::now_v7();
    for (id, request) in [(gone_id, gone.id), (stays_id, stays.id)] {
        sqlx::query(
            "insert into secret_revocations
                (id, machine_id, conversation_id, request_id, up_to, reason, next_attempt_at)
             values ($1, $2, $3, $4, 1, 'revoked', now())",
        )
        .bind(id)
        .bind(fx.seed.machine_id)
        .bind(fx.seed.conversation_id)
        .bind(request)
        .execute(&fx.pool)
        .await
        .unwrap();
    }

    let (selected, resume) = secrets::test_hooks::arm_before_lock();
    let pool = fx.pool.clone();
    let registry = fx.registry.clone();
    let task = tokio::spawn(async move { secrets::emit_due_revocations(&pool, &registry).await });
    let ids = selected.await.expect("selection");
    assert_eq!(ids.len(), 2, "{ids:?}");
    // Квитанция удалила одно обязательство между выборкой и блокировкой.
    sqlx::query("delete from secret_revocations where id=$1")
        .bind(gone_id)
        .execute(&fx.pool)
        .await
        .unwrap();
    resume.send(()).unwrap();
    assert_eq!(
        task.await.unwrap().unwrap(),
        1,
        "the other obligation emits"
    );
    let commands: i64 = sqlx::query_scalar(
        "select count(*) from commands
         where kind='revoke_secret' and payload->>'request_id'=$1",
    )
    .bind(stays.id.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(commands, 1);
    let vanished: i64 = sqlx::query_scalar(
        "select count(*) from commands
         where kind='revoke_secret' and payload->>'request_id'=$1",
    )
    .bind(gone.id.to_string())
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(vanished, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_huge_attempt_count_does_not_overflow_the_backoff(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Много попыток").await;
    let _worker = fx.connect(CAPS, "epoch-1").await;
    sqlx::query(
        "insert into secret_revocations
            (id, machine_id, conversation_id, request_id, up_to, reason, attempts, next_attempt_at)
         values ($1, $2, $3, $4, 1, 'revoked', 1000, now())",
    )
    .bind(Uuid::now_v7())
    .bind(fx.seed.machine_id)
    .bind(fx.seed.conversation_id)
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();

    // Сеанс мог отправить обязательство сам; важны счётчик и срок.
    let _ = secrets::emit_due_revocations(&fx.pool, &fx.registry)
        .await
        .unwrap();
    let (attempts, within_cap): (i32, bool) = sqlx::query_as(
        "select attempts, next_attempt_at <= now() + interval '11 minutes'
         from secret_revocations where machine_id=$1 and request_id=$2",
    )
    .bind(fx.seed.machine_id)
    .bind(row.id)
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert!(
        attempts >= 1001,
        "the counter grew without overflowing: {attempts}"
    );
    assert!(within_cap, "the capped backoff stays a real interval");
    let (up_to, reason): (i64, String) = sqlx::query_as(
        "select (payload->>'up_to')::bigint, payload->>'reason' from commands
         where kind='revoke_secret'",
    )
    .fetch_one(&fx.pool)
    .await
    .unwrap();
    assert_eq!(up_to, 1);
    assert_eq!(reason, "revoked");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_epoch_change_marks_delivered_requests_lost(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Ключ").await;
    let journal = Uuid::now_v7();
    let mut worker = fx.connect_journal(CAPS, "epoch-a", journal).await;
    let deliver = fx.send_value(&mut worker, row.id, "value\n").await;
    fx.delivered(Uuid::now_v7(), 1, &deliver, "/run/a").await;
    assert_eq!(fx.state(row.id).await, "delivered");
    drop(worker);
    wait_disconnected(&fx.registry, fx.seed.machine_id).await;

    // Новый процесс: tmpfs очищен, эпоха другая; журнал тот же, иначе хаб
    // справедливо заподозрит сброс локального состояния.
    let worker_b = fx.connect_journal(CAPS, "epoch-b", journal).await;
    // Сверку могла успеть сделать сама новая сессия; итог один.
    let reconciled = secrets::on_hub_session_established(
        &fx.pool,
        &fx.registry,
        fx.seed.machine_id,
        worker_b.connection,
    )
    .await
    .unwrap();
    assert!(reconciled <= 1);
    let lost = fx.row(row.id).await;
    assert!(lost.path.is_none(), "the path points at a vanished tmpfs");
    assert_eq!(lost.note.as_deref(), Some(secrets::NOTE_LOST));
    let (up_to, reason, _) = fx.obligation(row.id).await.expect("obligation");
    assert_eq!((up_to, reason.as_str()), (1, "epoch"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_re_filled_generation_gets_its_own_delivered_message(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Дважды переданный").await;
    let mut worker = fx.connect(CAPS, "epoch-1").await;
    let journal = Uuid::now_v7();
    let first = fx.send_value(&mut worker, row.id, "one\n").await;
    assert_eq!(first.generation, 1);
    fx.delivered(journal, 1, &first, "/run/one").await;
    // Потеря без отзыва: карточка снова ждёт владельца, поколение прежнее.
    sqlx::query("update secret_requests set state='lost', note=$2 where id=$1")
        .bind(row.id)
        .bind(secrets::NOTE_LOST)
        .execute(&fx.pool)
        .await
        .unwrap();
    let second = fx.send_value(&mut worker, row.id, "two\n").await;
    assert_eq!(second.generation, 2);
    fx.delivered(journal, 2, &second, "/run/two").await;

    let prompts: Vec<String> = fx
        .inputs()
        .await
        .into_iter()
        .filter(|text| text.contains("передан"))
        .collect();
    assert_eq!(
        prompts.len(),
        2,
        "each generation has its own agent message: {prompts:?}"
    );
    assert!(prompts.iter().any(|text| text.contains("/run/one")));
    assert!(prompts.iter().any(|text| text.contains("/run/two")));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_delivered_secret_message_shows_the_full_expiry_date(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Срок в сообщении").await;
    let mut worker = fx.connect(CAPS, "epoch-1").await;
    let deliver = fx.send_value(&mut worker, row.id, "value\n").await;
    let expires = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(
        i64::try_from(deliver.expires_at_ms).unwrap(),
    )
    .unwrap();
    fx.delivered(Uuid::now_v7(), 1, &deliver, "/run/a").await;

    let text = fx
        .inputs()
        .await
        .into_iter()
        .find(|text| text.contains("передан"))
        .expect("delivered message");
    assert!(
        text.contains(&expires.format("%Y-%m-%d %H:%M").to_string()),
        "expiry must include the date to be unambiguous: {text}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_removal_receipt_from_another_conversation_is_ignored(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Чужой разговор").await;
    sqlx::query("update secret_requests set state='revoking', generation=1 where id=$1")
        .bind(row.id)
        .execute(&fx.pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into secret_revocations
            (id, machine_id, conversation_id, request_id, up_to, reason, next_attempt_at)
         values ($1, $2, $3, $4, 1, 'revoked', now())",
    )
    .bind(Uuid::now_v7())
    .bind(fx.seed.machine_id)
    .bind(fx.seed.conversation_id)
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    let journal = Uuid::now_v7();
    fx.removed_in(journal, 1, row.id, Uuid::now_v7(), 1, "revoked")
        .await;
    assert_eq!(
        fx.state(row.id).await,
        "revoking",
        "another conversation cannot confirm this file"
    );
    assert!(
        fx.obligation(row.id).await.is_some(),
        "the obligation is not closed by a foreign conversation"
    );
    fx.removed_in(journal, 2, row.id, fx.seed.conversation_id, 1, "revoked")
        .await;
    assert_eq!(fx.state(row.id).await, "revoked");
    assert!(fx.obligation(row.id).await.is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_delivery_receipt_from_another_conversation_is_ignored(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Чужой разговор доставки").await;
    sqlx::query("update secret_requests set state='sending', generation=1 where id=$1")
        .bind(row.id)
        .execute(&fx.pool)
        .await
        .unwrap();
    let fact = canterlot_proto::worker::v1::SecretDelivered {
        request_id: row.id.to_string(),
        conversation_id: Uuid::now_v7().to_string(),
        generation: 1,
        path: "/run/foreign".into(),
        expires_at_ms: u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap(),
    };
    let mut tx = canterlot_hub::db::begin(&fx.pool).await.unwrap();
    secrets::apply_delivered_in_tx(&mut tx, fx.seed.machine_id, &fact, Uuid::now_v7(), None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let kept = fx.row(row.id).await;
    assert_eq!(kept.state, secrets::SecretState::Sending);
    assert!(kept.path.is_none(), "a foreign path is not recorded");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_epoch_change_marks_sending_requests_lost(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("В полёте").await;
    // Живое соединение: стартовая сверка не вправе объявить строку потерянной
    // раньше проверки эпохи.
    let worker = fx.connect(CAPS, "epoch-b").await;
    sqlx::query(
        "update secret_requests set state='sending', generation=1,
                secrets_epoch='epoch-a', send_deadline_at=now() + interval '60 seconds'
         where id=$1",
    )
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    let reconciled = secrets::on_hub_session_established(
        &fx.pool,
        &fx.registry,
        fx.seed.machine_id,
        worker.connection,
    )
    .await
    .unwrap();
    assert!(reconciled <= 1);
    let lost = fx.row(row.id).await;
    assert_eq!(lost.state, secrets::SecretState::Lost);
    assert!(lost.send_deadline_at.is_none());
    let (up_to, reason, _) = fx.obligation(row.id).await.expect("obligation");
    assert_eq!((up_to, reason.as_str()), (1, "epoch"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_replayed_delivery_receipt_from_an_old_epoch_becomes_lost(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Квитанция из прошлой загрузки").await;
    let _worker = fx.connect(CAPS, "epoch-b").await;
    sqlx::query(
        "update secret_requests set state='sending', generation=1, secrets_epoch='epoch-a',
                send_deadline_at=now() + interval '60 seconds'
         where id=$1",
    )
    .bind(row.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    let fact = canterlot_proto::worker::v1::SecretDelivered {
        request_id: row.id.to_string(),
        conversation_id: fx.seed.conversation_id.to_string(),
        generation: 1,
        path: "/run/gone".into(),
        expires_at_ms: u64::try_from(
            (chrono::Utc::now() + chrono::TimeDelta::hours(1)).timestamp_millis(),
        )
        .unwrap(),
    };
    let mut tx = canterlot_hub::db::begin(&fx.pool).await.unwrap();
    secrets::apply_delivered_in_tx(
        &mut tx,
        fx.seed.machine_id,
        &fact,
        Uuid::now_v7(),
        Some("epoch-b"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let lost = fx.row(row.id).await;
    assert_eq!(
        lost.state,
        secrets::SecretState::Lost,
        "a receipt from the previous tmpfs cannot certify a file"
    );
    assert!(lost.path.is_none());
    let (up_to, reason, _) = fx.obligation(row.id).await.expect("obligation");
    assert_eq!((up_to, reason.as_str()), (1, "epoch"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_revoke_command_carries_the_hub_expiry_of_the_covered_generation(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Срок в отзыве").await;
    let mut worker = fx.connect(CAPS, "epoch-1").await;
    let deliver = fx.send_value(&mut worker, row.id, "value\n").await;
    assert!(deliver.expires_at_ms > 0);
    fx.delivered(Uuid::now_v7(), 1, &deliver, "/run/a").await;
    let stored: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select generation_expires_at from secret_requests where id=$1")
            .bind(row.id)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(
        stored.map(|at| u64::try_from(at.timestamp_millis()).unwrap()),
        Some(deliver.expires_at_ms),
        "the hub remembers the expiry it sent"
    );

    let response = fx
        .post(
            &format!("/api/v1/secret-requests/{}/revoke", row.id),
            &Uuid::now_v7().to_string(),
            None,
        )
        .await;
    assert_eq!(response.status, axum::http::StatusCode::OK);
    let payload: Value =
        sqlx::query_scalar("select payload from commands where kind='revoke_secret'")
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(
        payload["max_expires_at_ms"], deliver.expires_at_ms,
        "the worker gets the hub-issued horizon, not its own clock"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_session_end_obligation_carries_the_generation_expiry(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let row = fx.open("Срок конца сессии").await;
    let mut worker = fx.connect(CAPS, "epoch-1").await;
    let deliver = fx.send_value(&mut worker, row.id, "value\n").await;
    let mut tx = canterlot_hub::db::begin(&fx.pool).await.unwrap();
    secrets::on_session_end_in_tx(&mut tx, fx.seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let max: i64 =
        sqlx::query_scalar("select max_expires_at_ms from secret_revocations where request_id=$1")
            .bind(row.id)
            .fetch_one(&fx.pool)
            .await
            .unwrap();
    assert_eq!(max, i64::try_from(deliver.expires_at_ms).unwrap());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_delayed_epoch_reconciliation_from_the_previous_connection_changes_nothing(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let old = fx.open("Старая загрузка").await;
    let fresh = fx.open("Новая загрузка").await;
    let journal = Uuid::now_v7();
    let first = fx.connect_journal(CAPS, "epoch-1", journal).await;
    let old_connection = first.connection;
    drop(first);
    wait_disconnected(&fx.registry, fx.seed.machine_id).await;
    let second = fx.connect_journal(CAPS, "epoch-2", journal).await;

    // E2 успела выдать новое поколение, пока задача сверки E1 ещё не дошла.
    sqlx::query(
        "update secret_requests set state='delivered', generation=1,
                secrets_epoch='epoch-1', path='/run/old' where id=$1",
    )
    .bind(old.id)
    .execute(&fx.pool)
    .await
    .unwrap();
    sqlx::query(
        "update secret_requests set state='sending', generation=2,
                secrets_epoch='epoch-2', send_deadline_at=now() + interval '60 seconds'
         where id=$1",
    )
    .bind(fresh.id)
    .execute(&fx.pool)
    .await
    .unwrap();

    assert_eq!(
        secrets::on_hub_session_established(
            &fx.pool,
            &fx.registry,
            fx.seed.machine_id,
            old_connection,
        )
        .await
        .unwrap(),
        0,
        "an obsolete connection's reconciliation does nothing"
    );
    assert_eq!(fx.state(old.id).await, "delivered");
    assert_eq!(fx.state(fresh.id).await, "sending");
    assert!(fx.obligation(old.id).await.is_none());
    assert!(fx.obligation(fresh.id).await.is_none());

    // Сверка текущего соединения видит старую эпоху и не трогает новую выдачу.
    let reconciled = secrets::on_hub_session_established(
        &fx.pool,
        &fx.registry,
        fx.seed.machine_id,
        second.connection,
    )
    .await
    .unwrap();
    assert!(reconciled <= 1);
    assert_eq!(fx.state(old.id).await, "lost");
    assert_eq!(fx.state(fresh.id).await, "sending");
    let (up_to, reason, _) = fx.obligation(old.id).await.expect("obligation");
    assert_eq!((up_to, reason.as_str()), (1, "epoch"));
    assert!(fx.obligation(fresh.id).await.is_none());
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
            hung_generating: Duration::from_secs(600),
            hung_rescue: canterlot_hub::config::HungRescue::Auto,
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
async fn secret_request_is_refused_while_owner_stop_is_pending(pool: PgPool) {
    let fx = Fx::new(pool).await;
    sqlx::query(
        "insert into queue_state (conversation_id, pause_reasons)
         values ($1, '[{\"reason\":\"user_stop\"}]'::jsonb)
         on conflict (conversation_id) do update set pause_reasons = excluded.pause_reasons",
    )
    .bind(fx.seed.conversation_id)
    .execute(&fx.pool)
    .await
    .unwrap();
    let error = secrets::open(
        &fx.pool,
        fx.seed.owner_id,
        fx.seed.conversation_id,
        "Ключ",
        "после остановки",
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
async fn the_session_tool_returns_a_request_id_and_holds_the_limit(pool: PgPool) {
    use canterlot_hub::{
        config::SessionToolsConfig,
        session_tools::{Caller, ToolContext, ToolErrorCode, settle::SettleConfig},
    };
    use std::sync::Arc;
    let fx = Fx::new(pool.clone()).await;
    let context = ToolContext {
        memory: Default::default(),
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
async fn the_whole_path_leaves_no_plaintext_or_hash_in_the_database(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let marker = format!("canterlot-secret-PROBE-{}", Uuid::now_v7());
    let value = format!("{marker}\n");
    let row = fx.open("Пароль").await;
    let mut worker = fx.connect(CAPS, "epoch-1").await;
    let deliver = fx.send_value(&mut worker, row.id, &value).await;
    assert_eq!(deliver.value, value.as_bytes());
    fx.delivered(Uuid::now_v7(), 1, &deliver, "/run/c").await;
    assert_absent_everywhere(&fx.pool, &marker).await;
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
fn _unused(_: Answer, _: ControlUp) {}
