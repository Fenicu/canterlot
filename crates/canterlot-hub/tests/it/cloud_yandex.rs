//! Клиент Yandex Cloud против поддельного Яндекса: вход, каждый исход создания, операции,
//! машины, списки с пагинацией, удаление, диски, образы и цены.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use canterlot_hub::cloud::rate_limit::RateLimit;
use canterlot_hub::cloud::yandex::{
    CreateInstance, CreateOutcome, DeleteOutcome, InstanceLookup, InstanceStatus, ListError,
    OperationState, Price, PriceError, YandexClient, YandexConfig, YandexConfigError,
    YandexInitError,
};
use canterlot_hub::cloud::{ErrorKind, ProviderError};
use serde_json::json;
use tracing_subscriber::fmt::MakeWriter;

use crate::support::fake_yandex::{
    CreateMode, FOLDER, FailPage, FakeSku, FakeYandex, IMAGE_ID, KEY_ID, SKU_PREEMPTIBLE_RAM,
    SKU_PREEMPTIBLE_VCPU, SKU_PUBLIC_IP, SKU_REGULAR_RAM, SKU_REGULAR_VCPU, SKU_SSD, SUBNET, ZONE,
    street_price, test_key, write_key_file,
};

const GIB: u64 = 1 << 30;
const USER_DATA: &str = "#cloud-config\nssh_keys:\n  ed25519_private: HOST-PRIVATE-KEY-7c1e\n";

struct Rig {
    fake: FakeYandex,
    client: YandexClient,
    config: YandexConfig,
    _dir: tempfile::TempDir,
}

async fn rig() -> Rig {
    rig_with(|_| {}).await
}

async fn rig_with(tweak: impl FnOnce(&mut YandexConfig)) -> Rig {
    let fake = FakeYandex::start().await;
    let dir = tempfile::tempdir().unwrap();
    let key_file = write_key_file(dir.path());
    let mut config = YandexConfig::new(key_file, FOLDER, ZONE, SUBNET);
    config.iam_url = fake.base_url.clone();
    config.compute_url = fake.base_url.clone();
    config.operation_url = fake.base_url.clone();
    config.billing_url = fake.base_url.clone();
    config.request_timeout = Duration::from_secs(3);
    // Предел частоты здесь не проверяется: он не должен растягивать обходы на десятки страниц.
    config.rate_limit = RateLimit {
        max: 10_000,
        per: Duration::from_secs(1),
    };
    config.max_rate_limit_retries = 2;
    config.default_backoff = Duration::from_millis(20);
    config.max_backoff = Duration::from_millis(400);
    tweak(&mut config);
    let client = YandexClient::new(config.clone()).unwrap();
    Rig {
        fake,
        client,
        config,
        _dir: dir,
    }
}

fn create_request(name: &str) -> CreateInstance {
    CreateInstance {
        name: name.to_owned(),
        labels: BTreeMap::from([("canterlot".to_owned(), "runner".to_owned())]),
        cores: 32,
        memory_bytes: 64 * GIB,
        disk_bytes: 80 * GIB,
        preemptible: true,
        image_id: IMAGE_ID.to_owned(),
        user_data: USER_DATA.to_owned(),
    }
}

const NAME: &str = "canterlot-runner-018f0000-0000-7000-8000-0000000000aa";

fn count(rig: &Rig, method: &str, path_prefix: &str) -> usize {
    rig.fake.control().count(method, path_prefix)
}

fn creates(rig: &Rig) -> usize {
    rig.fake
        .requests()
        .iter()
        .filter(|r| r.method == "POST" && r.path == "/compute/v1/instances")
        .count()
}

fn token_exchanges(rig: &Rig) -> usize {
    count(rig, "POST", "/iam/v1/tokens")
}

async fn accepted(rig: &Rig, name: &str) -> (String, String) {
    match rig.client.create(FOLDER, &create_request(name)).await {
        CreateOutcome::Accepted {
            instance_id,
            operation_id,
        } => (instance_id, operation_id),
        other => panic!("create was not accepted: {other:?}"),
    }
}

async fn found(rig: &Rig, id: &str) -> canterlot_hub::cloud::yandex::Instance {
    match rig.client.get(id).await.unwrap() {
        InstanceLookup::Found(instance) => instance,
        InstanceLookup::NotFound => panic!("instance {id} not found"),
    }
}

// ---- вход ----

#[tokio::test]
async fn the_first_call_exchanges_a_signed_jwt_for_a_token_and_reuses_it() {
    let rig = rig().await;
    assert_eq!(token_exchanges(&rig), 0, "the client is lazy");
    assert_eq!(rig.client.list_instances(FOLDER).await.unwrap(), vec![]);
    assert_eq!(rig.client.list_disks(FOLDER).await.unwrap(), vec![]);
    // Подпись PS256, `kid`, `iss`, `aud` и срок проверил сам поддельный IAM: иначе токена нет.
    assert_eq!(token_exchanges(&rig), 1);
    let requests = rig.fake.requests();
    let exchange = &requests[0];
    assert_eq!(exchange.path, "/iam/v1/tokens");
    assert!(exchange.authorization.is_none());
    let jwt = exchange.body.as_ref().unwrap()["jwt"].as_str().unwrap();
    assert_eq!(jwt.split('.').count(), 3);
    for call in &requests[1..] {
        assert_eq!(
            call.authorization.as_deref(),
            Some("Bearer t1.fake-iam-token-0001-5f2c9a71d3")
        );
    }
}

#[tokio::test]
async fn a_401_refreshes_the_token_once_and_retries() {
    let rig = rig().await;
    rig.client.list_instances(FOLDER).await.unwrap();
    rig.fake.control().expire_tokens();
    rig.fake.control().add_instance(NAME);
    let listed = rig.client.list_instances(FOLDER).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(token_exchanges(&rig), 2);
    // Первый список, отказ со старым токеном, повтор с новым.
    assert_eq!(count(&rig, "GET", "/compute/v1/instances"), 3);
    let last = rig.fake.requests().pop().unwrap();
    assert_eq!(
        last.authorization.as_deref(),
        Some("Bearer t1.fake-iam-token-0002-5f2c9a71d3")
    );
}

#[tokio::test]
async fn a_persistent_401_is_an_auth_error_after_exactly_one_refresh() {
    let rig = rig().await;
    rig.fake.control().refuse_all_tokens = true;
    let error = rig.client.get("fhmfakeinstance0001").await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Auth);
    assert!(!error.retryable());
    assert_eq!(token_exchanges(&rig), 2);
    assert_eq!(count(&rig, "GET", "/compute/v1/instances/"), 2);
}

#[tokio::test]
async fn a_create_answered_401_twice_is_a_definite_refusal() {
    let rig = rig().await;
    rig.fake.control().refuse_all_tokens = true;
    let outcome = rig.client.create(FOLDER, &create_request(NAME)).await;
    assert!(
        matches!(
            outcome,
            CreateOutcome::Rejected {
                http: 401,
                grpc_code: Some(16),
                next_profile: false,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(creates(&rig), 2);
    assert_eq!(rig.fake.instance_count(), 0);
}

#[tokio::test]
async fn a_failed_token_exchange_fails_the_call_without_reaching_the_api() {
    let rig = rig().await;
    rig.fake.control().iam_status = Some(503);
    let error = rig.client.get("fhmfakeinstance0001").await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable);
    assert!(error.retryable());
    let listing = rig.client.list_instances(FOLDER).await.unwrap_err();
    assert!(matches!(listing, ListError::Page { page: 0, .. }));
    assert!(listing.retryable());
    // Создание не уходило: машины точно нет, и это не «неизвестно».
    let outcome = rig.client.create(FOLDER, &create_request(NAME)).await;
    assert!(
        matches!(
            outcome,
            CreateOutcome::Rejected {
                http: 0,
                next_profile: false,
                ..
            }
        ),
        "{outcome:?}"
    );
    let deleted = rig.client.delete("fhmfakeinstance0001").await;
    assert!(
        matches!(
            deleted,
            DeleteOutcome::Failed {
                retryable: true,
                http: None,
                ..
            }
        ),
        "{deleted:?}"
    );
    assert_eq!(count(&rig, "GET", "/compute"), 0);
    assert_eq!(creates(&rig), 0);
    assert_eq!(count(&rig, "DELETE", "/compute"), 0);

    // Ключ не принят: это отказ во входе, а не недоступность.
    rig.fake.control().iam_status = Some(401);
    let error = rig.client.get("fhmfakeinstance0001").await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Auth);

    // IAM ожил: следующий вызов проходит.
    rig.fake.control().iam_status = None;
    assert_eq!(
        rig.client.get("fhmfakeinstance0001").await.unwrap(),
        InstanceLookup::NotFound
    );
}

#[tokio::test]
async fn a_key_file_that_cannot_be_used_is_refused_without_quoting_it() {
    let dir = tempfile::tempdir().unwrap();
    let config = |path: &std::path::Path| YandexConfig::new(path, FOLDER, ZONE, SUBNET);

    let missing = YandexClient::new(config(&dir.path().join("absent.json"))).unwrap_err();
    assert_eq!(missing, YandexInitError::KeyFile("NotFound".to_owned()));

    let garbage = dir.path().join("garbage.json");
    std::fs::write(&garbage, "SECRET-GARBAGE-41d0").unwrap();
    let broken = dir.path().join("broken.json");
    std::fs::write(
        &broken,
        json!({"id": KEY_ID, "service_account_id": "sa", "private_key": "SECRET-GARBAGE-41d0"})
            .to_string(),
    )
    .unwrap();
    for path in [&garbage, &broken] {
        let error = YandexClient::new(config(path)).unwrap_err();
        assert!(matches!(error, YandexInitError::KeyFormat(_)), "{error:?}");
        assert!(!format!("{error} {error:?}").contains("SECRET-GARBAGE"));
    }

    let mut other_platform = config(&write_key_file(dir.path()));
    other_platform.platform = "standard-v2".to_owned();
    assert_eq!(
        YandexClient::new(other_platform).unwrap_err(),
        YandexInitError::Config(YandexConfigError::UnknownPlatform("standard-v2".to_owned()))
    );
    let mut other_disk = config(&write_key_file(dir.path()));
    other_disk.disk_type = "network-ssd-nonreplicated".to_owned();
    assert!(matches!(
        YandexClient::new(other_disk).unwrap_err(),
        YandexInitError::Config(YandexConfigError::UnknownDiskType(_))
    ));
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

#[tokio::test]
async fn no_secret_shows_in_debug_errors_or_logs() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let rig = rig().await;
    // Неряшливый сервер повторяет заголовок Authorization в текстах ошибок.
    rig.fake.control().echo_authorization_in_errors = true;
    let mut shown: Vec<String> = vec![format!("{:?}", rig.client), format!("{:?}", rig.config)];

    let request = create_request(NAME);
    shown.push(format!("{request:?}"));

    // Ошибки по всем путям, каждая с токеном в тексте от сервера.
    rig.fake.control().create = CreateMode::Error {
        http: 400,
        code: 3,
        message: "bad".to_owned(),
    };
    shown.push(format!("{:?}", rig.client.create(FOLDER, &request).await));
    rig.fake.control().create = CreateMode::Accept;
    let (instance_id, _) = accepted(&rig, NAME).await;
    rig.fake.control().fail_next_create_operation = Some((13, "boom".to_owned()));
    let (_, failed_operation) = accepted(&rig, "canterlot-runner-other").await;
    shown.push(format!(
        "{:?}",
        rig.client.operation(&failed_operation).await
    ));
    for status in [403u16, 500] {
        rig.fake.control().fail_next.push_back(status);
        let error = rig.client.get(&instance_id).await.unwrap_err();
        shown.push(format!("{error} {error:?}"));
    }
    rig.fake.control().fail_next.push_back(500);
    let error = rig.client.list_instances(FOLDER).await.unwrap_err();
    shown.push(format!("{error} {error:?}"));
    rig.fake.control().fail_next.push_back(403);
    shown.push(format!("{:?}", rig.client.delete(&instance_id).await));
    rig.fake.control().fail_next.push_back(500);
    let error = rig.client.price(2, 2, 10, true).await.unwrap_err();
    shown.push(format!("{error} {error:?}"));
    rig.fake.control().fail_next.push_back(404);
    let error = rig.client.latest_image("debian-13").await.unwrap_err();
    shown.push(format!("{error} {error:?}"));
    // Отказ после обновления токена: в тексте мог бы оказаться уже второй токен.
    rig.fake.control().refuse_all_tokens = true;
    let error = rig.client.get(&instance_id).await.unwrap_err();
    shown.push(format!("{error} {error:?}"));
    shown.push(format!("{:?}", rig.client));

    let requests = rig.fake.requests();
    let jwts: Vec<String> = requests
        .iter()
        .filter(|r| r.path == "/iam/v1/tokens")
        .map(|r| r.body.as_ref().unwrap()["jwt"].as_str().unwrap().to_owned())
        .collect();
    assert!(jwts.len() >= 2);
    // Строка из середины закрытого ключа: её нет нигде, кроме файла ключа.
    let key_line = test_key()
        .private_pem
        .lines()
        .nth(3)
        .expect("a PEM body line")
        .to_owned();
    assert!(key_line.len() >= 32);

    shown.push(String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned());
    let all = shown.join("\n");
    assert!(all.contains("HTTP 403"), "errors were collected: {all}");
    // Сервер действительно повторял токен: на его месте стоит заглушка.
    assert!(all.contains("authorization: Bearer [redacted]"), "{all}");
    assert!(
        !all.contains("t1.fake-iam-token"),
        "an IAM token leaked: {all}"
    );
    assert!(!all.contains(&key_line), "the private key leaked");
    assert!(!all.contains("PRIVATE KEY"), "the private key leaked");
    for jwt in &jwts {
        assert!(!all.contains(jwt.as_str()), "a JWT leaked");
        // И подпись отдельно, на случай разрезанного JWT.
        assert!(!all.contains(jwt.rsplit('.').next().unwrap()));
    }
    assert!(
        !all.contains("HOST-PRIVATE-KEY"),
        "the user-data leaked: {all}"
    );
}

// ---- создание ----

#[tokio::test]
async fn create_sends_the_documented_body_and_returns_both_ids() {
    let rig = rig().await;
    let (instance_id, operation_id) = accepted(&rig, NAME).await;
    assert_eq!(instance_id, "fhmfakeinstance0001");
    assert_eq!(operation_id, "fhmfakeoperation0002");

    let request = rig
        .fake
        .requests()
        .into_iter()
        .find(|r| r.method == "POST" && r.path == "/compute/v1/instances")
        .unwrap();
    // int64 идут строками; диск назван как машина и удаляется вместе с ней.
    assert_eq!(
        request.body.unwrap(),
        json!({
            "folderId": FOLDER,
            "name": NAME,
            "zoneId": ZONE,
            "platformId": "standard-v3",
            "labels": {"canterlot": "runner"},
            "resourcesSpec": {"memory": "68719476736", "cores": "32", "coreFraction": "100"},
            "metadata": {"user-data": USER_DATA},
            "bootDiskSpec": {
                "autoDelete": true,
                "diskSpec": {
                    "name": NAME,
                    "typeId": "network-ssd",
                    "size": "85899345920",
                    "imageId": IMAGE_ID,
                },
            },
            "networkInterfaceSpecs": [{
                "subnetId": SUBNET,
                "primaryV4AddressSpec": {"oneToOneNatSpec": {"ipVersion": "IPV4"}},
            }],
            "schedulingPolicy": {"preemptible": true},
        })
    );
}

#[tokio::test]
async fn create_takes_the_folder_and_preemptibility_from_the_call_not_the_config() {
    // В настройках другой каталог и прерываемая машина: в запрос идут значения вызова.
    let rig = rig_with(|config| {
        config.folder_id = "b1gfakeotherfolder0".to_owned();
        config.preemptible = true;
    })
    .await;
    let mut request = create_request(NAME);
    request.preemptible = false;
    let outcome = rig.client.create(FOLDER, &request).await;
    assert!(
        matches!(outcome, CreateOutcome::Accepted { .. }),
        "{outcome:?}"
    );
    let body = rig.fake.requests().pop().unwrap().body.unwrap();
    assert_eq!(body["folderId"], FOLDER);
    assert_eq!(body["schedulingPolicy"], json!({"preemptible": false}));
    let instance = found(&rig, "fhmfakeinstance0001").await;
    assert!(!instance.preemptible);
    assert_eq!(instance.folder_id, FOLDER);
}

#[tokio::test]
async fn a_second_create_with_the_same_name_is_ambiguous_and_makes_no_second_machine() {
    let rig = rig().await;
    accepted(&rig, NAME).await;
    let outcome = rig.client.create(FOLDER, &create_request(NAME)).await;
    assert_eq!(
        outcome,
        CreateOutcome::Ambiguous {
            why: "the name already exists".to_owned()
        }
    );
    assert_eq!(rig.fake.instance_count(), 1);
}

#[tokio::test]
async fn a_409_that_is_not_a_taken_name_is_a_refusal() {
    let rig = rig().await;
    rig.fake.control().create = CreateMode::Error {
        http: 409,
        code: 10,
        message: "Operation aborted".to_owned(),
    };
    let outcome = rig.client.create(FOLDER, &create_request(NAME)).await;
    assert_eq!(
        outcome,
        CreateOutcome::Rejected {
            http: 409,
            grpc_code: Some(10),
            message: "Operation aborted".to_owned(),
            next_profile: false,
        }
    );
}

#[tokio::test]
async fn a_quota_429_is_a_definite_refusal_that_moves_to_the_next_profile() {
    let rig = rig().await;
    rig.fake.control().create = CreateMode::quota();
    let outcome = rig.client.create(FOLDER, &create_request(NAME)).await;
    assert_eq!(
        outcome,
        CreateOutcome::Rejected {
            http: 429,
            grpc_code: Some(8),
            message: "Quota limit compute.instanceCores.count exceeded".to_owned(),
            next_profile: true,
        }
    );
    // 429 у создания не повторяется: это отказ, а не просьба подождать.
    assert_eq!(creates(&rig), 1);
    assert_eq!(rig.fake.instance_count(), 0);
}

#[tokio::test]
async fn a_precondition_refusal_moves_to_the_next_profile_only_for_quota_or_resources() {
    let rig = rig().await;
    for (message, next_profile) in [
        ("Not enough resources in zone ru-central1-a", true),
        ("Quota limit compute.instanceMemory.size exceeded", true),
        ("The subnet is in another zone", false),
    ] {
        rig.fake.control().create = CreateMode::Error {
            http: 400,
            code: 9,
            message: message.to_owned(),
        };
        let outcome = rig.client.create(FOLDER, &create_request(NAME)).await;
        assert_eq!(
            outcome,
            CreateOutcome::Rejected {
                http: 400,
                grpc_code: Some(9),
                message: message.to_owned(),
                next_profile,
            }
        );
    }
}

#[tokio::test]
async fn other_4xx_refusals_never_move_to_the_next_profile() {
    let rig = rig().await;
    for (http, code, message) in [
        // Слова про квоту в отказе другого рода ничего не значат.
        (400u16, 3, "Illegal argument: quota"),
        (403, 7, "Permission denied"),
        (404, 5, "Subnet not found"),
        (429, 14, "Slow down"),
    ] {
        rig.fake.control().create = CreateMode::Error {
            http,
            code,
            message: message.to_owned(),
        };
        let outcome = rig.client.create(FOLDER, &create_request(NAME)).await;
        assert_eq!(
            outcome,
            CreateOutcome::Rejected {
                http,
                grpc_code: Some(code),
                message: message.to_owned(),
                next_profile: false,
            }
        );
    }
    assert_eq!(creates(&rig), 4);
    assert_eq!(rig.fake.instance_count(), 0);
}

#[tokio::test]
async fn a_request_the_api_finds_invalid_is_a_definite_refusal() {
    let rig = rig().await;
    // Имя не по образцу: отказывает сам поддельный Яндекс, как настоящий.
    let outcome = rig
        .client
        .create(FOLDER, &create_request("Bad_Name-"))
        .await;
    assert!(
        matches!(
            outcome,
            CreateOutcome::Rejected {
                http: 400,
                grpc_code: Some(3),
                next_profile: false,
                ..
            }
        ),
        "{outcome:?}"
    );
}

/// Неоднозначный исход: машина могла появиться, и запрос создания ушёл ровно один раз.
async fn assert_ambiguous(rig: &Rig, expected_why: &str) {
    let outcome = rig.client.create(FOLDER, &create_request(NAME)).await;
    assert_eq!(
        outcome,
        CreateOutcome::Ambiguous {
            why: expected_why.to_owned()
        }
    );
    assert_eq!(creates(rig), 1, "an ambiguous create is never repeated");
}

#[tokio::test]
async fn a_5xx_on_create_is_ambiguous_and_not_a_refusal() {
    let rig = rig().await;
    rig.fake.control().create = CreateMode::unavailable();
    assert_ambiguous(&rig, "HTTP 503").await;
}

#[tokio::test]
async fn a_500_on_create_is_ambiguous_and_not_a_refusal() {
    let rig = rig().await;
    rig.fake.control().create = CreateMode::Error {
        http: 500,
        code: 13,
        // Даже слова про квоту в теле 5xx не делают его отказом.
        message: "Quota service failed".to_owned(),
    };
    assert_ambiguous(&rig, "HTTP 500").await;
}

#[tokio::test]
async fn a_timeout_on_create_is_ambiguous_and_the_machine_may_exist() {
    let rig = rig_with(|config| config.request_timeout = Duration::from_millis(300)).await;
    rig.fake.control().create = CreateMode::AcceptThenHang;
    assert_ambiguous(&rig, "request timed out").await;
    assert_eq!(rig.fake.instance_count(), 1);
}

#[tokio::test]
async fn a_dropped_connection_on_create_is_ambiguous_and_the_machine_may_exist() {
    let rig = rig().await;
    rig.fake.control().create = CreateMode::AcceptThenDrop;
    assert_ambiguous(&rig, "connection failed").await;
    assert_eq!(rig.fake.instance_count(), 1);
}

#[tokio::test]
async fn an_unreadable_success_on_create_is_ambiguous() {
    let rig = rig().await;
    rig.fake.control().create = CreateMode::Garbage;
    assert_ambiguous(
        &rig,
        "success answer without an operation or an instance id",
    )
    .await;
    assert_eq!(rig.fake.instance_count(), 1);
}

// ---- операции ----

#[tokio::test]
async fn a_create_operation_is_pending_until_the_machine_runs() {
    let rig = rig().await;
    rig.fake.control().boot_polls = 2;
    let (_, operation_id) = accepted(&rig, NAME).await;
    assert_eq!(
        rig.client.operation(&operation_id).await.unwrap(),
        OperationState::Pending
    );
    assert_eq!(
        rig.client.operation(&operation_id).await.unwrap(),
        OperationState::Done
    );
}

#[tokio::test]
async fn an_operation_that_fails_after_acceptance_is_failed_only_once_done() {
    let rig = rig().await;
    {
        let mut fake = rig.fake.control();
        fake.fail_next_create_operation = Some((8, "Not enough resources".to_owned()));
        fake.failed_create_rollback_polls = 2;
    }
    let (_, operation_id) = accepted(&rig, NAME).await;
    // Ошибка уже стоит в операции, но откат идёт: исход ещё не окончателен.
    for _ in 0..2 {
        assert_eq!(
            rig.client.operation(&operation_id).await.unwrap(),
            OperationState::Pending
        );
    }
    assert_eq!(
        rig.client.operation(&operation_id).await.unwrap(),
        OperationState::Failed {
            grpc_code: 8,
            message: "Not enough resources".to_owned()
        }
    );
    assert_eq!(rig.fake.instance_count(), 0);
}

#[tokio::test]
async fn an_unknown_operation_is_its_own_state_and_a_bad_id_never_reaches_the_api() {
    let rig = rig().await;
    assert_eq!(
        rig.client.operation("fhm00000000000000000").await.unwrap(),
        OperationState::Unknown
    );
    let before = rig.fake.requests().len();
    let error = rig
        .client
        .operation("../compute/v1/disks")
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Rejected);
    assert_eq!(rig.fake.requests().len(), before);
    // Сбой сервера — ошибка, а не «операции нет».
    rig.fake.control().fail_next.push_back(500);
    let error = rig
        .client
        .operation("fhm00000000000000000")
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable);
}

// ---- машина ----

#[tokio::test]
async fn get_shows_the_machine_and_its_public_ip_once_it_runs() {
    let rig = rig().await;
    rig.fake.control().boot_polls = 2;
    let (instance_id, _) = accepted(&rig, NAME).await;
    let booting = found(&rig, &instance_id).await;
    assert_eq!(booting.status, InstanceStatus::Provisioning);
    assert_eq!(booting.public_ip, None);

    let running = found(&rig, &instance_id).await;
    assert_eq!(
        running,
        canterlot_hub::cloud::yandex::Instance {
            id: instance_id.clone(),
            name: NAME.to_owned(),
            folder_id: FOLDER.to_owned(),
            status: InstanceStatus::Running,
            public_ip: Some("203.0.113.2".to_owned()),
            boot_disk_id: Some("fhmfakedisk00000001".to_owned()),
            preemptible: true,
        }
    );
}

#[tokio::test]
async fn get_of_a_missing_machine_is_not_found_and_other_failures_are_errors() {
    let rig = rig().await;
    assert_eq!(
        rig.client.get("fhm00000000000000000").await.unwrap(),
        InstanceLookup::NotFound
    );
    for (status, kind) in [
        (500u16, ErrorKind::Unavailable),
        (403, ErrorKind::Auth),
        (400, ErrorKind::Rejected),
    ] {
        rig.fake.control().fail_next.push_back(status);
        let error = rig.client.get("fhm00000000000000000").await.unwrap_err();
        assert_eq!(error.kind, kind, "{error}");
    }
    let before = rig.fake.requests().len();
    assert!(rig.client.get("a/b").await.is_err());
    assert_eq!(rig.fake.requests().len(), before);
}

#[tokio::test]
async fn every_api_status_maps_into_the_neutral_set() {
    let rig = rig().await;
    let id = rig.fake.control().add_instance(NAME);
    for (api, expected) in [
        ("PROVISIONING", InstanceStatus::Provisioning),
        ("STARTING", InstanceStatus::Provisioning),
        ("RUNNING", InstanceStatus::Running),
        ("STOPPING", InstanceStatus::Stopping),
        ("STOPPED", InstanceStatus::Stopped),
        ("ERROR", InstanceStatus::Broken),
        ("CRASHED", InstanceStatus::Broken),
        ("DELETING", InstanceStatus::Deleting),
        ("RESTARTING", InstanceStatus::Other("RESTARTING".to_owned())),
        ("UPDATING", InstanceStatus::Other("UPDATING".to_owned())),
        (
            "HIBERNATING",
            InstanceStatus::Other("HIBERNATING".to_owned()),
        ),
    ] {
        rig.fake
            .control()
            .instances
            .get_mut(&id)
            .unwrap()
            .forced_status = Some(api.to_owned());
        assert_eq!(found(&rig, &id).await.status, expected, "{api}");
        let listed = rig.client.list_instances(FOLDER).await.unwrap();
        assert_eq!(listed[0].status, expected, "{api} in the listing");
    }
}

#[tokio::test]
async fn a_preempted_machine_is_stopped_and_has_no_public_ip() {
    let rig = rig().await;
    let id = rig.fake.control().add_instance(NAME);
    assert!(found(&rig, &id).await.public_ip.is_some());
    rig.fake.control().preempt(&id);
    let instance = found(&rig, &id).await;
    assert_eq!(instance.status, InstanceStatus::Stopped);
    assert_eq!(instance.public_ip, None);
    assert!(instance.preemptible);
}

// ---- списки ----

#[tokio::test]
async fn a_listing_walks_every_page_of_the_named_folder() {
    let rig = rig().await;
    let mut ids = Vec::new();
    for index in 0..5 {
        let name = format!("canterlot-runner-{index}");
        ids.push(rig.fake.control().add_instance(&name));
    }
    rig.fake.control().list_page_cap = Some(2);
    let listed = rig.client.list_instances(FOLDER).await.unwrap();
    assert_eq!(listed.iter().map(|i| i.id.clone()).collect::<Vec<_>>(), ids);
    assert_eq!(listed[4].name, "canterlot-runner-4");
    assert_eq!(listed[4].status, InstanceStatus::Running);

    let pages: Vec<_> = rig
        .fake
        .requests()
        .into_iter()
        .filter(|r| r.path == "/compute/v1/instances")
        .collect();
    assert_eq!(pages.len(), 3);
    for page in &pages {
        assert_eq!(page.query["folderId"], FOLDER);
        assert_eq!(page.query["pageSize"], "1000");
    }
    assert!(!pages[0].query.contains_key("pageToken"));
    assert_eq!(pages[1].query["pageToken"], "p1-2");
    assert_eq!(pages[2].query["pageToken"], "p2-4");

    // Каталог называет вызывающий: в другом каталоге этих машин нет.
    assert_eq!(
        rig.client
            .list_instances("b1gfakeotherfolder0")
            .await
            .unwrap(),
        vec![]
    );
    let last = rig.fake.requests().pop().unwrap();
    assert_eq!(last.query["folderId"], "b1gfakeotherfolder0");
}

#[tokio::test]
async fn a_failing_page_fails_the_whole_listing() {
    let rig = rig().await;
    for index in 0..5 {
        let name = format!("canterlot-runner-{index}");
        rig.fake.control().add_instance(&name);
    }
    {
        let mut fake = rig.fake.control();
        fake.list_page_cap = Some(2);
        fake.list_fail_page = Some(FailPage {
            list: "instances",
            page: 2,
            status: 500,
            times: Some(1),
        });
    }
    let error = rig.client.list_instances(FOLDER).await.unwrap_err();
    assert!(
        matches!(
            &error,
            ListError::Page {
                page: 2,
                error: ProviderError {
                    kind: ErrorKind::Unavailable,
                    ..
                }
            }
        ),
        "{error:?}"
    );
    assert!(error.retryable());
    // Отказ был разовым: следующий обход полный.
    assert_eq!(rig.client.list_instances(FOLDER).await.unwrap().len(), 5);
}

#[tokio::test]
async fn a_repeated_page_token_is_an_error_not_an_endless_walk() {
    let rig = rig().await;
    for index in 0..3 {
        let name = format!("canterlot-runner-{index}");
        rig.fake.control().add_instance(&name);
    }
    rig.fake.control().repeat_page_token = true;
    let error = rig.client.list_instances(FOLDER).await.unwrap_err();
    assert_eq!(error, ListError::RepeatedToken { page: 1 });
    assert_eq!(count(&rig, "GET", "/compute/v1/instances"), 2);
    let error = rig.client.list_disks(FOLDER).await.unwrap_err();
    assert_eq!(error, ListError::RepeatedToken { page: 1 });
}

#[tokio::test]
async fn fifty_pages_are_read_and_the_fifty_first_is_an_error() {
    let rig = rig().await;
    for index in 0..50 {
        let name = format!("canterlot-runner-{index}");
        rig.fake.control().add_instance(&name);
    }
    rig.fake.control().list_page_cap = Some(1);
    assert_eq!(rig.client.list_instances(FOLDER).await.unwrap().len(), 50);

    rig.fake.control().add_instance("canterlot-runner-50");
    let error = rig.client.list_instances(FOLDER).await.unwrap_err();
    assert_eq!(error, ListError::TooManyPages { limit: 50 });
}

#[tokio::test]
async fn a_429_outside_create_is_retried_and_then_given_up_as_retryable() {
    let rig = rig().await;
    rig.fake.control().add_instance(NAME);
    {
        let mut fake = rig.fake.control();
        fake.rate_limit_next = 2;
        fake.retry_after = Some("0".to_owned());
    }
    assert_eq!(rig.client.list_instances(FOLDER).await.unwrap().len(), 1);
    assert_eq!(count(&rig, "GET", "/compute/v1/instances"), 3);

    // Предел не уходит: попытка сдаётся, и ошибка зовёт повторить позже.
    rig.fake.control().rate_limit_next = 100;
    let error = rig.client.list_instances(FOLDER).await.unwrap_err();
    assert!(
        matches!(
            &error,
            ListError::Page {
                page: 0,
                error: ProviderError {
                    kind: ErrorKind::RateLimited,
                    ..
                }
            }
        ),
        "{error:?}"
    );
    assert!(error.retryable());
    assert_eq!(count(&rig, "GET", "/compute/v1/instances"), 6);

    // `Retry-After` больше границы не ждётся вовсе.
    rig.fake.control().retry_after = Some("3600".to_owned());
    let error = rig.client.get("fhmfakeinstance0001").await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::RateLimited);
    assert_eq!(count(&rig, "GET", "/compute/v1/instances/"), 1);
}

#[tokio::test]
async fn a_page_that_is_not_what_the_api_documents_fails_the_listing() {
    let rig = rig().await;
    // Машина без номера: по такому списку нельзя заключить, что нужной машины нет.
    let id = rig.fake.control().add_instance(NAME);
    rig.fake.control().instances.get_mut(&id).unwrap().id = String::new();
    let error = rig.client.list_instances(FOLDER).await.unwrap_err();
    assert!(
        matches!(
            &error,
            ListError::Page {
                error: ProviderError {
                    kind: ErrorKind::Malformed,
                    ..
                },
                ..
            }
        ),
        "{error:?}"
    );
}

// ---- удаление ----

#[tokio::test]
async fn delete_is_accepted_repeats_with_the_same_operation_and_ends_in_404() {
    let rig = rig().await;
    rig.fake.control().delete_polls = 3;
    let (instance_id, _) = accepted(&rig, NAME).await;

    let DeleteOutcome::Accepted { operation_id } = rig.client.delete(&instance_id).await else {
        panic!("delete was not accepted");
    };
    // Повтор, пока удаление идёт: та же операция.
    assert_eq!(
        rig.client.delete(&instance_id).await,
        DeleteOutcome::Accepted {
            operation_id: operation_id.clone()
        }
    );
    assert_eq!(
        found(&rig, &instance_id).await.status,
        InstanceStatus::Deleting
    );
    assert_eq!(
        rig.client.operation(&operation_id).await.unwrap(),
        OperationState::Pending
    );
    assert_eq!(
        rig.client.operation(&operation_id).await.unwrap(),
        OperationState::Done
    );

    assert_eq!(
        rig.client.get(&instance_id).await.unwrap(),
        InstanceLookup::NotFound
    );
    assert_eq!(
        rig.client.delete(&instance_id).await,
        DeleteOutcome::AlreadyGone
    );
    assert_eq!(rig.client.list_instances(FOLDER).await.unwrap(), vec![]);
    // Диск с `autoDelete` ушёл вместе с машиной.
    assert_eq!(rig.client.list_disks(FOLDER).await.unwrap(), vec![]);
}

#[tokio::test]
async fn a_failed_delete_says_whether_a_retry_makes_sense() {
    let rig = rig().await;
    let id = rig.fake.control().add_instance(NAME);
    for (status, retryable, grpc_code) in [
        (500u16, true, 13),
        (503, true, 14),
        (409, true, 10),
        (403, false, 7),
        (400, false, 3),
    ] {
        rig.fake.control().fail_next.push_back(status);
        assert_eq!(
            rig.client.delete(&id).await,
            DeleteOutcome::Failed {
                retryable,
                http: Some(status),
                grpc_code: Some(grpc_code),
                message: "Injected failure".to_owned(),
            },
            "{status}"
        );
    }
    assert_eq!(rig.fake.instance_count(), 1);

    let before = rig.fake.requests().len();
    assert!(matches!(
        rig.client.delete("../disks/x").await,
        DeleteOutcome::Failed {
            retryable: false,
            http: None,
            ..
        }
    ));
    assert_eq!(rig.fake.requests().len(), before);
}

#[tokio::test]
async fn a_delete_without_an_answer_is_retryable() {
    let rig = rig_with(|config| config.request_timeout = Duration::from_millis(300)).await;
    let id = rig.fake.control().add_instance(NAME);
    // Токен уже получен; дальше сервер молчит.
    rig.client.list_instances(FOLDER).await.unwrap();
    rig.fake.control().hang_next = 1;
    assert_eq!(
        rig.client.delete(&id).await,
        DeleteOutcome::Failed {
            retryable: true,
            http: None,
            grpc_code: None,
            message: "request timed out".to_owned(),
        }
    );
    // Предел частоты, который не уходит, тоже зовёт повторить позже.
    rig.fake.control().rate_limit_next = 100;
    assert_eq!(
        rig.client.delete(&id).await,
        DeleteOutcome::Failed {
            retryable: true,
            http: None,
            grpc_code: None,
            message: "rate limited".to_owned(),
        }
    );
    assert_eq!(rig.fake.instance_count(), 1);
}

// ---- диски ----

#[tokio::test]
async fn the_boot_disk_is_listed_with_its_machine_and_goes_with_it() {
    let rig = rig().await;
    let (instance_id, _) = accepted(&rig, NAME).await;
    let disks = rig.client.list_disks(FOLDER).await.unwrap();
    assert_eq!(
        disks,
        vec![canterlot_hub::cloud::yandex::Disk {
            id: "fhmfakedisk00000001".to_owned(),
            name: NAME.to_owned(),
            instance_ids: vec![instance_id.clone()],
            status: "READY".to_owned(),
        }]
    );
    // Подключённый диск API удалить не даёт.
    assert_eq!(
        rig.client.delete_disk(&disks[0].id).await,
        DeleteOutcome::Failed {
            retryable: false,
            http: Some(400),
            grpc_code: Some(9),
            message: "Disk fhmfakedisk00000001 is attached to an instance".to_owned(),
        }
    );
    rig.client.delete(&instance_id).await;
    assert_eq!(rig.client.list_instances(FOLDER).await.unwrap(), vec![]);
    assert_eq!(rig.client.list_disks(FOLDER).await.unwrap(), vec![]);
    assert_eq!(
        rig.client.list_disks("b1gfakeotherfolder0").await.unwrap(),
        vec![]
    );
}

#[tokio::test]
async fn a_disk_left_behind_is_listed_as_stray_and_can_be_deleted() {
    let rig = rig().await;
    let (instance_id, _) = accepted(&rig, NAME).await;
    rig.fake.control().leave_disk(&instance_id);
    rig.client.delete(&instance_id).await;
    assert_eq!(rig.client.list_instances(FOLDER).await.unwrap(), vec![]);

    let disks = rig.client.list_disks(FOLDER).await.unwrap();
    assert_eq!(disks.len(), 1);
    assert_eq!(disks[0].name, NAME);
    assert_eq!(disks[0].instance_ids, Vec::<String>::new());

    let DeleteOutcome::Accepted { operation_id } = rig.client.delete_disk(&disks[0].id).await
    else {
        panic!("disk delete was not accepted");
    };
    assert_eq!(
        rig.client.operation(&operation_id).await.unwrap(),
        OperationState::Done
    );
    assert_eq!(rig.client.list_disks(FOLDER).await.unwrap(), vec![]);
    assert_eq!(
        rig.client.delete_disk(&disks[0].id).await,
        DeleteOutcome::AlreadyGone
    );
}

#[tokio::test]
async fn the_disk_listing_is_paginated_and_fails_as_a_whole() {
    let rig = rig().await;
    for index in 0..5 {
        let name = format!("canterlot-runner-{index}");
        rig.fake.control().add_stray_disk(&name);
    }
    rig.fake.control().list_page_cap = Some(2);
    let disks = rig.client.list_disks(FOLDER).await.unwrap();
    assert_eq!(disks.len(), 5);
    assert_eq!(count(&rig, "GET", "/compute/v1/disks"), 3);

    rig.fake.control().list_fail_page = Some(FailPage {
        list: "disks",
        page: 1,
        status: 503,
        times: None,
    });
    let error = rig.client.list_disks(FOLDER).await.unwrap_err();
    assert!(
        matches!(error, ListError::Page { page: 1, .. }),
        "{error:?}"
    );
    // Отказ страницы дисков не мешает списку машин.
    assert_eq!(rig.client.list_instances(FOLDER).await.unwrap(), vec![]);
}

// ---- образ ----

#[tokio::test]
async fn the_latest_image_comes_from_the_standard_images_folder() {
    let rig = rig().await;
    assert_eq!(
        rig.client.latest_image("debian-13").await.unwrap(),
        IMAGE_ID
    );
    let request = rig.fake.requests().pop().unwrap();
    assert_eq!(request.path, "/compute/v1/images:latestByFamily");
    assert_eq!(request.query["folderId"], "standard-images");
    assert_eq!(request.query["family"], "debian-13");

    let error = rig.client.latest_image("no-such-family").await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
}

// ---- цены ----

#[tokio::test]
async fn prices_use_the_version_in_effect_of_the_skus_matched_by_id() {
    let rig = rig().await;
    // В каталоге у каждого SKU есть старая цена (500) и объявленная на будущее (900), а
    // рядом лежат SKU с теми же именами, другими номерами и ценами в десятки рублей.
    // 32×0.34 + 64×0.083 + 0.26352 = 16.45552 ₽; 80×0.0199 = 1.592 ₽.
    let preemptible = rig.client.price(32, 64, 80, true).await.unwrap();
    assert_eq!(
        preemptible,
        Price {
            compute_kopecks_per_hour: 1646,
            disk_kopecks_per_hour: 160,
        }
    );
    assert_eq!(preemptible.total_kopecks_per_hour(), Some(1806));
    // 32×1.24 + 64×0.33 + 0.26352 = 61.06352 ₽; диск стоит столько же.
    let regular = rig.client.price(32, 64, 80, false).await.unwrap();
    assert_eq!(
        regular,
        Price {
            compute_kopecks_per_hour: 6107,
            disk_kopecks_per_hour: 160,
        }
    );
}

#[tokio::test]
async fn each_rate_rounds_up_to_a_whole_kopeck() {
    let rig = rig().await;
    // 2×0.34 + 2×0.083 + 0.26352 = 1.10952 ₽ → 111; 10×0.0199 = 0.199 ₽ → 20.
    assert_eq!(
        rig.client.price(2, 2, 10, true).await.unwrap(),
        Price {
            compute_kopecks_per_hour: 111,
            disk_kopecks_per_hour: 20,
        }
    );
    // Ровно целые копейки вверх не уходят: 100×0.0199 = 1.99 ₽.
    assert_eq!(
        rig.client
            .price(2, 2, 100, true)
            .await
            .unwrap()
            .disk_kopecks_per_hour,
        199
    );
}

#[tokio::test]
async fn the_catalog_is_read_in_full_once_and_cached() {
    let rig = rig().await;
    rig.fake.control().sku_page_cap = Some(5);
    rig.client.price(32, 64, 80, true).await.unwrap();
    let pages: Vec<_> = rig
        .fake
        .requests()
        .into_iter()
        .filter(|r| r.path == "/billing/v1/skus")
        .collect();
    assert_eq!(pages.len(), 3);
    for page in &pages {
        assert_eq!(page.query["currency"], "RUB");
        assert_eq!(page.query["pageSize"], "1000");
        assert!(!page.query.contains_key("folderId"));
    }
    // Вторая цена и цена обычной машины берутся из той же таблицы.
    rig.client.price(2, 2, 10, true).await.unwrap();
    rig.client.price(2, 2, 10, false).await.unwrap();
    assert_eq!(count(&rig, "GET", "/billing/v1/skus"), 3);
}

#[tokio::test]
async fn a_failed_catalog_read_is_an_error_and_is_not_cached() {
    let rig = rig().await;
    {
        let mut fake = rig.fake.control();
        fake.sku_page_cap = Some(5);
        fake.list_fail_page = Some(FailPage {
            list: "skus",
            page: 1,
            status: 500,
            times: Some(1),
        });
    }
    let error = rig.client.price(32, 64, 80, true).await.unwrap_err();
    assert!(
        matches!(&error, PriceError::Catalog(ListError::Page { page: 1, .. })),
        "{error:?}"
    );
    assert!(error.retryable());
    assert_eq!(
        rig.client
            .price(32, 64, 80, true)
            .await
            .unwrap()
            .compute_kopecks_per_hour,
        1646
    );
}

#[tokio::test]
async fn a_missing_sku_is_a_pricing_error_for_the_machines_that_need_it() {
    for (sku, preemptible_fails, regular_fails) in [
        (SKU_PREEMPTIBLE_VCPU, true, false),
        (SKU_PREEMPTIBLE_RAM, true, false),
        (SKU_REGULAR_VCPU, false, true),
        (SKU_REGULAR_RAM, false, true),
        (SKU_SSD, true, true),
        (SKU_PUBLIC_IP, true, true),
    ] {
        let rig = rig().await;
        // Соседи с тем же именем остаются в каталоге: по имени цена нашлась бы, и неверная.
        rig.fake.control().remove_sku(sku);
        for (preemptible, fails) in [(true, preemptible_fails), (false, regular_fails)] {
            let result = rig.client.price(32, 64, 80, preemptible).await;
            if fails {
                assert_eq!(result, Err(PriceError::MissingSku(sku.to_owned())), "{sku}");
            } else {
                assert!(result.is_ok(), "{sku}: {result:?}");
            }
        }
    }
}

#[tokio::test]
async fn a_sku_without_one_clear_price_in_effect_fails_closed() {
    let old = "2025-12-31T21:00:00Z";
    let tiers = json!([{
        "type": "STREET_PRICE",
        "effectiveTime": old,
        "pricingExpressions": [{"rates": [
            {"startPricingQuantity": "0", "unitPrice": "0", "currency": "RUB"},
            {"startPricingQuantity": "100", "unitPrice": "0.34", "currency": "RUB"},
        ]}],
    }]);
    let dollars = json!([{
        "type": "STREET_PRICE",
        "effectiveTime": old,
        "pricingExpressions": [{"rates": [
            {"startPricingQuantity": "0", "unitPrice": "0.34", "currency": "USD"},
        ]}],
    }]);
    let contract_only = json!([{
        "type": "CONTRACT_PRICE",
        "effectiveTime": old,
        "pricingExpressions": [{"rates": [
            {"startPricingQuantity": "0", "unitPrice": "0.34", "currency": "RUB"},
        ]}],
    }]);
    for versions in [
        // Действующей цены нет: только объявленная на будущее.
        json!([street_price("2099-01-01T00:00:00Z", "0.34")]),
        json!([]),
        tiers,
        dollars,
        contract_only,
    ] {
        let rig = rig().await;
        rig.fake.control().sku_mut(SKU_PREEMPTIBLE_VCPU).versions = versions.clone();
        let error = rig.client.price(32, 64, 80, true).await.unwrap_err();
        assert!(
            matches!(&error, PriceError::MalformedSku { id, .. } if id == SKU_PREEMPTIBLE_VCPU),
            "{versions}: {error:?}"
        );
        assert!(!error.retryable());
        // Обычной машине этот SKU не нужен.
        assert!(rig.client.price(32, 64, 80, false).await.is_ok());
    }
}

#[tokio::test]
async fn a_sku_in_an_unexpected_unit_or_listed_twice_fails_closed() {
    let rig = rig().await;
    rig.fake.control().sku_mut(SKU_SSD).unit = "gbyte*month".to_owned();
    let error = rig.client.price(32, 64, 80, true).await.unwrap_err();
    assert!(
        matches!(&error, PriceError::MalformedSku { id, .. } if id == SKU_SSD),
        "{error:?}"
    );

    let rig = self::rig().await;
    let twin = FakeSku::new(SKU_PUBLIC_IP, "Public IP address", "fip*hour", "0.01");
    rig.fake.control().skus.push(twin);
    let error = rig.client.price(32, 64, 80, true).await.unwrap_err();
    assert!(
        matches!(&error, PriceError::MalformedSku { id, .. } if id == SKU_PUBLIC_IP),
        "{error:?}"
    );
}
