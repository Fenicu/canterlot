//! Клиент Timeweb против поддельного Timeweb: каждая ручка сбоев, пагинация, сокрытие токена,
//! предел частоты.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use canterlot_hub::cloud::price::{Sizing, hourly_kopecks, monthly_kopecks};
use canterlot_hub::cloud::rate_limit::RateLimit;
use canterlot_hub::cloud::{
    CloudProvider as _, CreateOutcome, CreateServer, DeleteOutcome, ErrorKind, ListError,
    ServerLookup, ServerStatus, TimewebConfig, TimewebProvider,
};
use serde_json::json;
use tracing_subscriber::fmt::MakeWriter;

use crate::support::fake_timeweb::{CreateMode, DeleteMode, FailPage, FakeTimeweb};

const TOKEN: &str = "tw-test-token-9f3a7c21";
const PROJECT: i64 = 4242;
const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLEEXAMPLEEXAMPLEEXAMPLE0000";

struct Rig {
    fake: FakeTimeweb,
    provider: TimewebProvider,
    config: TimewebConfig,
    _dir: tempfile::TempDir,
}

async fn rig() -> Rig {
    rig_with(|_| {}).await
}

async fn rig_with(tweak: impl FnOnce(&mut TimewebConfig)) -> Rig {
    let fake = FakeTimeweb::start(TOKEN).await;
    let dir = tempfile::tempdir().unwrap();
    let token_file = dir.path().join("token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).unwrap();
    let mut config = TimewebConfig::new(&token_file);
    config.base_url = fake.base_url.clone();
    config.request_timeout = Duration::from_secs(3);
    config.max_rate_limit_retries = 2;
    config.default_backoff = Duration::from_millis(20);
    config.max_backoff = Duration::from_millis(400);
    tweak(&mut config);
    let provider = TimewebProvider::new(config.clone()).unwrap();
    Rig {
        fake,
        provider,
        config,
        _dir: dir,
    }
}

fn create_request(name: &str) -> CreateServer {
    CreateServer {
        name: name.to_owned(),
        comment: "canterlot runner window".to_owned(),
        project_id: PROJECT,
        configurator_id: 151,
        cpu: 32,
        ram_mb: 65_536,
        disk_mb: 81_920,
        os_id: 109,
        ssh_key_ids: vec![71],
        cloud_init: "#cloud-config\n".to_owned(),
        hostname: "runner-a".to_owned(),
    }
}

fn posts_to_servers(rig: &Rig) -> usize {
    rig.fake
        .requests()
        .iter()
        .filter(|r| r.method == "POST" && r.path == "/api/v1/servers")
        .count()
}

fn deletes(rig: &Rig) -> usize {
    rig.fake
        .requests()
        .iter()
        .filter(|r| r.method == "DELETE")
        .count()
}

// ---- конфигуратор и ключи ----

#[tokio::test]
async fn configurator_returns_prices_in_kopecks_and_limits_in_mb() {
    let rig = rig().await;
    let configurator = rig.provider.configurator(151).await.unwrap();
    assert_eq!(configurator.id, 151);
    assert_eq!(configurator.location, "ru-3");
    assert_eq!(configurator.cpu_frequency, "3.0");
    assert_eq!(configurator.prices.cpu, 21_000);
    assert_eq!(configurator.prices.ram_gb, 20_000);
    assert_eq!(configurator.prices.disk_gb, 1_200);
    assert_eq!(configurator.limits.cpu_max, 104);
    assert_eq!(configurator.limits.ram_step_mb, 1024);
    assert_eq!(configurator.limits.disk_min_mb, 10_240);

    // Профиль по умолчанию: 20 480 ₽ в месяц, 2845 копеек в час.
    let sizing = Sizing {
        cpu: 32,
        ram_mb: 65_536,
        disk_mb: 81_920,
    };
    let monthly = monthly_kopecks(&configurator.prices, &sizing).unwrap();
    assert_eq!(monthly, 2_048_000);
    assert_eq!(hourly_kopecks(monthly), Ok(2845));

    // Пресет High-160 из спеки: 133 600 ₽.
    let preset = rig.provider.configurator(133).await.unwrap();
    let sizing = Sizing {
        cpu: 28,
        ram_mb: 112 * 1024,
        disk_mb: 160 * 1024,
    };
    assert_eq!(monthly_kopecks(&preset.prices, &sizing), Ok(13_360_000));
}

#[tokio::test]
async fn unknown_configurator_is_not_found() {
    let rig = rig().await;
    let error = rig.provider.configurator(999).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
}

#[tokio::test]
async fn every_request_carries_the_bearer_token() {
    let rig = rig().await;
    rig.provider.configurator(151).await.unwrap();
    rig.provider.get_server(1).await.unwrap();
    let requests = rig.fake.requests();
    assert!(requests.len() >= 2);
    for request in requests {
        assert_eq!(
            request.authorization.as_deref(),
            Some(format!("Bearer {TOKEN}").as_str())
        );
    }
}

#[tokio::test]
async fn wrong_token_is_an_auth_error() {
    let rig = rig().await;
    rig.fake.control().expected_token = "another-token".to_owned();
    let error = rig.provider.configurator(151).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Auth);
    assert!(!error.retryable());
}

#[tokio::test]
async fn ensure_ssh_key_is_idempotent_by_name_and_never_deletes() {
    let rig = rig().await;
    let first = rig.provider.ensure_ssh_key("window-a", KEY).await.unwrap();
    let again = rig.provider.ensure_ssh_key("window-a", KEY).await.unwrap();
    assert_eq!(first, again);
    // Тот же ключ с другим комментарием — тот же ключ.
    let with_comment = rig
        .provider
        .ensure_ssh_key("window-a", &format!("{KEY} carrier"))
        .await
        .unwrap();
    assert_eq!(first, with_comment);
    let other = rig.provider.ensure_ssh_key("window-b", KEY).await.unwrap();
    assert_ne!(first, other);

    assert_eq!(rig.fake.control().ssh_keys.len(), 2);
    assert_eq!(rig.fake.control().count("POST", "/api/v1/ssh-keys"), 2);
    assert_eq!(deletes(&rig), 0);
    let created = rig
        .fake
        .requests()
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    assert_eq!(created.body.unwrap()["is_default"], json!(false));
}

#[tokio::test]
async fn ensure_ssh_key_refuses_a_name_taken_by_another_key() {
    let rig = rig().await;
    rig.provider.ensure_ssh_key("window-a", KEY).await.unwrap();
    let other = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOTHEROTHEROTHEROTHEROTHEROTHER000";
    let error = rig
        .provider
        .ensure_ssh_key("window-a", other)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert_eq!(deletes(&rig), 0);
    assert_eq!(rig.fake.control().ssh_keys.len(), 1);
}

// ---- создание ----

#[tokio::test]
async fn create_sends_the_documented_body_and_returns_the_id() {
    let rig = rig().await;
    let outcome = rig
        .provider
        .create_server(&create_request("canterlot-runner-w1"))
        .await;
    let CreateOutcome::Created { server_id } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        rig.fake.control().servers[&server_id].name,
        "canterlot-runner-w1"
    );

    let body = rig
        .fake
        .requests()
        .into_iter()
        .find(|r| r.method == "POST" && r.path == "/api/v1/servers")
        .unwrap()
        .body
        .unwrap();
    assert_eq!(
        body["configuration"],
        json!({"configurator_id": 151, "cpu": 32, "ram": 65_536, "disk": 81_920})
    );
    assert_eq!(body["os_id"], json!(109));
    assert_eq!(body["ssh_keys_ids"], json!([71]));
    assert_eq!(body["project_id"], json!(PROJECT));
    assert_eq!(body["name"], json!("canterlot-runner-w1"));
    assert_eq!(body["comment"], json!("canterlot runner window"));
    assert_eq!(body["hostname"], json!("runner-a"));
    assert_eq!(body["cloud_init"], json!("#cloud-config\n"));
    assert!(body.get("preset_id").is_none());
    assert!(body.get("image_id").is_none());
}

#[tokio::test]
async fn definite_refusals_are_rejected_with_code_and_message() {
    for (status, message) in [
        (400, "Bad configuration"),
        (403, "Forbidden"),
        (409, "Name busy"),
    ] {
        let rig = rig().await;
        rig.fake.control().create = CreateMode::Reject {
            status,
            message: message.to_owned(),
        };
        let outcome = rig
            .provider
            .create_server(&create_request("canterlot-runner-w1"))
            .await;
        assert_eq!(
            outcome,
            CreateOutcome::Rejected {
                code: status,
                message: message.to_owned()
            }
        );
        assert_eq!(posts_to_servers(&rig), 1, "a refusal is never retried");
    }
}

#[tokio::test]
async fn connection_dropped_after_accept_is_ambiguous_and_not_retried() {
    let rig = rig().await;
    rig.fake.control().create = CreateMode::AcceptThenDrop;
    let outcome = rig
        .provider
        .create_server(&create_request("canterlot-runner-w1"))
        .await;
    assert!(
        matches!(outcome, CreateOutcome::Ambiguous { .. }),
        "{outcome:?}"
    );
    assert_eq!(posts_to_servers(&rig), 1);
    // Сервер при этом создан: именно поэтому исход неоднозначный.
    assert_eq!(rig.fake.server_count(), 1);
}

#[tokio::test]
async fn timeout_is_ambiguous_and_not_retried() {
    let rig = rig_with(|c| c.request_timeout = Duration::from_millis(300)).await;
    rig.fake.control().create = CreateMode::AcceptThenHang;
    let outcome = rig
        .provider
        .create_server(&create_request("canterlot-runner-w1"))
        .await;
    assert!(
        matches!(outcome, CreateOutcome::Ambiguous { .. }),
        "{outcome:?}"
    );
    assert_eq!(posts_to_servers(&rig), 1);
    assert_eq!(rig.fake.server_count(), 1);
}

#[tokio::test]
async fn server_errors_on_create_are_ambiguous_and_not_retried() {
    for status in [500, 502, 503] {
        let rig = rig().await;
        rig.fake.control().create = CreateMode::Status(status);
        let outcome = rig
            .provider
            .create_server(&create_request("canterlot-runner-w1"))
            .await;
        assert!(
            matches!(outcome, CreateOutcome::Ambiguous { .. }),
            "{status}: {outcome:?}"
        );
        assert_eq!(posts_to_servers(&rig), 1);
    }
}

#[tokio::test]
async fn unreachable_host_on_create_is_ambiguous() {
    let rig = rig_with(|c| c.base_url = "http://127.0.0.1:1".to_owned()).await;
    let outcome = rig
        .provider
        .create_server(&create_request("canterlot-runner-w1"))
        .await;
    assert!(
        matches!(outcome, CreateOutcome::Ambiguous { .. }),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn rate_limited_create_waits_for_retry_after_and_then_succeeds() {
    let rig = rig_with(|c| c.max_backoff = Duration::from_secs(5)).await;
    {
        let mut state = rig.fake.control();
        state.rate_limit_next = 1;
        state.retry_after = Some("1".to_owned());
    }
    let outcome = rig
        .provider
        .create_server(&create_request("canterlot-runner-w1"))
        .await;
    assert!(
        matches!(outcome, CreateOutcome::Created { .. }),
        "{outcome:?}"
    );
    // 429 не доходит до создания: сервер один, а запросов два.
    assert_eq!(rig.fake.server_count(), 1);
    assert_eq!(posts_to_servers(&rig), 2);
}

/// Пока `Retry-After` не вышел, повтора нет: запрос ждёт, и на сервере остаётся одна попытка.
#[tokio::test]
async fn nothing_is_resent_before_retry_after_expires() {
    let rig = rig_with(|c| c.max_backoff = Duration::from_secs(30)).await;
    {
        let mut state = rig.fake.control();
        state.rate_limit_next = 1;
        state.retry_after = Some("20".to_owned());
    }
    let waited = tokio::time::timeout(
        Duration::from_millis(300),
        rig.provider
            .create_server(&create_request("canterlot-runner-w1")),
    )
    .await;
    assert!(waited.is_err(), "the call must still be waiting");
    assert_eq!(posts_to_servers(&rig), 1);
    assert_eq!(rig.fake.server_count(), 0);
}

#[tokio::test]
async fn rate_limit_without_retry_after_gives_up_after_the_retries() {
    let rig = rig_with(|c| {
        c.default_backoff = Duration::from_millis(10);
        c.max_backoff = Duration::from_secs(5);
        c.max_rate_limit_retries = 2;
    })
    .await;
    rig.fake.control().rate_limit_next = 100;
    let outcome = rig
        .provider
        .create_server(&create_request("canterlot-runner-w1"))
        .await;
    // Попытка и два повтора; сами паузы проверены на чистой функции `rate_limit_wait`.
    assert_eq!(posts_to_servers(&rig), 3);
    assert!(
        matches!(outcome, CreateOutcome::Ambiguous { .. }),
        "{outcome:?}"
    );
    assert_eq!(rig.fake.server_count(), 0);
}

#[tokio::test]
async fn retry_after_beyond_the_bound_is_not_waited_for() {
    let rig = rig_with(|c| c.max_backoff = Duration::from_millis(300)).await;
    {
        let mut state = rig.fake.control();
        state.rate_limit_next = 100;
        state.retry_after = Some("3600".to_owned());
    }
    let outcome = rig.provider.delete_server(1).await;
    assert_eq!(outcome, DeleteOutcome::Failed { retryable: true });
    assert_eq!(rig.fake.control().count("DELETE", "/api/v1/servers/1"), 1);
}

// ---- чтение сервера ----

#[tokio::test]
async fn get_server_reports_status_and_address_once_booted() {
    let rig = rig_with(|_| {}).await;
    rig.fake.control().boot_polls = 2;
    let CreateOutcome::Created { server_id } = rig
        .provider
        .create_server(&create_request("canterlot-runner-w1"))
        .await
    else {
        panic!("create");
    };
    let ServerLookup::Found(state) = rig.provider.get_server(server_id).await.unwrap() else {
        panic!("not found");
    };
    assert_eq!(state.id, server_id);
    assert_eq!(state.name, "canterlot-runner-w1");
    assert_eq!(state.status, ServerStatus::Installing);
    assert_eq!(state.ipv4, None);

    // Второй опрос ещё `installing`, третий уже `on` с адресом.
    let ServerLookup::Found(state) = rig.provider.get_server(server_id).await.unwrap() else {
        panic!("not found");
    };
    assert_eq!(state.status, ServerStatus::Installing);
    let ServerLookup::Found(state) = rig.provider.get_server(server_id).await.unwrap() else {
        panic!("not found");
    };
    assert_eq!(state.status, ServerStatus::On);
    let address = state.ipv4.expect("address after boot");
    assert!(address.starts_with("203.0.113."), "{address}");
}

#[tokio::test]
async fn get_server_not_found_and_failures() {
    let rig = rig().await;
    assert_eq!(
        rig.provider.get_server(77).await.unwrap(),
        ServerLookup::NotFound
    );
    rig.fake.control().fail_next.push_back(503);
    let error = rig.provider.get_server(77).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable);
    assert!(error.retryable());
}

// ---- удаление ----

#[tokio::test]
async fn delete_outcomes() {
    let cases = [
        (
            DeleteMode::Ok,
            DeleteOutcome::Deleted { quarantined: false },
            true,
        ),
        (
            DeleteMode::NoContent,
            DeleteOutcome::Deleted { quarantined: false },
            true,
        ),
        (
            DeleteMode::Quarantine,
            DeleteOutcome::Deleted { quarantined: true },
            true,
        ),
        (
            DeleteMode::WithHash,
            DeleteOutcome::NeedsConfirmation,
            false,
        ),
        (DeleteMode::Locked, DeleteOutcome::NeedsConfirmation, false),
        (
            DeleteMode::Status(500),
            DeleteOutcome::Failed { retryable: true },
            false,
        ),
        (
            DeleteMode::Status(503),
            DeleteOutcome::Failed { retryable: true },
            false,
        ),
        (
            DeleteMode::Status(403),
            DeleteOutcome::Failed { retryable: false },
            false,
        ),
        (
            DeleteMode::Status(401),
            DeleteOutcome::Failed { retryable: false },
            false,
        ),
        (
            DeleteMode::Status(400),
            DeleteOutcome::Failed { retryable: false },
            false,
        ),
    ];
    for (mode, expected, removed) in cases {
        let rig = rig().await;
        let id = rig
            .fake
            .control()
            .add_server(PROJECT, "canterlot-runner-w1");
        rig.fake.control().delete = mode.clone();
        assert_eq!(rig.provider.delete_server(id).await, expected, "{mode:?}");
        // Ни один исход не повторяется: ровно один DELETE.
        assert_eq!(deletes(&rig), 1, "{mode:?}");
        assert_eq!(rig.fake.server_count(), usize::from(!removed), "{mode:?}");
    }
}

#[tokio::test]
async fn delete_of_a_missing_server_is_already_gone() {
    let rig = rig().await;
    assert_eq!(
        rig.provider.delete_server(12345).await,
        DeleteOutcome::AlreadyGone
    );
}

#[tokio::test]
async fn delete_survives_a_rate_limit_then_deletes() {
    let rig = rig().await;
    let id = rig
        .fake
        .control()
        .add_server(PROJECT, "canterlot-runner-w1");
    {
        let mut state = rig.fake.control();
        state.rate_limit_next = 1;
        state.retry_after = Some("0".to_owned());
    }
    assert_eq!(
        rig.provider.delete_server(id).await,
        DeleteOutcome::Deleted { quarantined: false }
    );
}

#[tokio::test]
async fn delete_rate_limit_exhaustion_is_a_retryable_failure() {
    let rig = rig().await;
    let id = rig
        .fake
        .control()
        .add_server(PROJECT, "canterlot-runner-w1");
    rig.fake.control().rate_limit_next = 100;
    assert_eq!(
        rig.provider.delete_server(id).await,
        DeleteOutcome::Failed { retryable: true }
    );
    assert_eq!(rig.fake.server_count(), 1);
}

// ---- список проекта ----

fn fill_project(rig: &Rig, count: usize) {
    let mut state = rig.fake.control();
    for i in 0..count {
        state.add_server(PROJECT, &format!("canterlot-runner-{i:04}"));
    }
    // Чужой проект в список не попадает.
    for i in 0..5 {
        state.add_server(PROJECT + 1, &format!("other-{i}"));
    }
}

fn list_requests(rig: &Rig) -> Vec<(String, String)> {
    rig.fake
        .requests()
        .into_iter()
        .filter(|r| {
            r.method == "GET" && r.path == format!("/api/v1/projects/{PROJECT}/resources/servers")
        })
        .map(|r| {
            (
                r.query.get("offset").cloned().unwrap_or_default(),
                r.query.get("limit").cloned().unwrap_or_default(),
            )
        })
        .collect()
}

#[tokio::test]
async fn list_walks_every_page() {
    let rig = rig().await;
    fill_project(&rig, 250);
    rig.fake.control().list_page_cap = Some(100);
    let servers = rig.provider.list_project_servers(PROJECT).await.unwrap();
    assert_eq!(servers.len(), 250);
    let mut names: Vec<_> = servers.iter().map(|s| s.name.clone()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 250);
    assert!(names.iter().all(|n| n.starts_with("canterlot-runner-")));
    assert!(servers.iter().all(|s| s.status == ServerStatus::On));
    let offsets: Vec<_> = list_requests(&rig).into_iter().map(|(o, _)| o).collect();
    assert_eq!(offsets, ["0", "100", "200"]);
}

#[tokio::test]
async fn list_follows_a_smaller_server_side_page_than_asked() {
    let rig = rig().await;
    fill_project(&rig, 25);
    rig.fake.control().list_page_cap = Some(10);
    let servers = rig.provider.list_project_servers(PROJECT).await.unwrap();
    assert_eq!(servers.len(), 25);
}

#[tokio::test]
async fn list_of_an_empty_project_is_empty() {
    let rig = rig().await;
    assert!(
        rig.provider
            .list_project_servers(PROJECT)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_failing_page_fails_the_whole_listing() {
    let rig = rig().await;
    fill_project(&rig, 250);
    {
        let mut state = rig.fake.control();
        state.list_page_cap = Some(100);
        state.list_fail_page = Some(FailPage {
            offset: 100,
            status: 500,
            times: None,
        });
    }
    let error = rig
        .provider
        .list_project_servers(PROJECT)
        .await
        .unwrap_err();
    let ListError::Page { offset, error } = error else {
        panic!("{error:?}");
    };
    assert_eq!(offset, 100);
    assert_eq!(error.kind, ErrorKind::Unavailable);
    // Третью страницу не просили: после отказа идти дальше нечего.
    assert_eq!(list_requests(&rig).len(), 2);
}

#[tokio::test]
async fn a_listing_after_a_failed_one_is_complete() {
    let rig = rig().await;
    fill_project(&rig, 150);
    {
        let mut state = rig.fake.control();
        state.list_page_cap = Some(100);
        state.list_fail_page = Some(FailPage {
            offset: 100,
            status: 502,
            times: Some(1),
        });
    }
    assert!(rig.provider.list_project_servers(PROJECT).await.is_err());
    assert_eq!(
        rig.provider
            .list_project_servers(PROJECT)
            .await
            .unwrap()
            .len(),
        150
    );
}

#[tokio::test]
async fn rate_limit_in_the_middle_of_a_listing_is_an_error_not_a_partial_list() {
    let rig = rig().await;
    fill_project(&rig, 150);
    rig.fake.control().list_page_cap = Some(100);
    // Первая страница проходит, на второй 429 без конца.
    rig.fake.control().list_fail_page = Some(FailPage {
        offset: 100,
        status: 429,
        times: None,
    });
    let error = rig
        .provider
        .list_project_servers(PROJECT)
        .await
        .unwrap_err();
    assert!(
        matches!(error, ListError::Page { offset: 100, .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn list_works_when_the_server_ignores_paging() {
    let rig = rig().await;
    fill_project(&rig, 40);
    rig.fake.control().list_ignores_paging = true;
    let servers = rig.provider.list_project_servers(PROJECT).await.unwrap();
    assert_eq!(servers.len(), 40);
    assert_eq!(list_requests(&rig).len(), 1);
}

#[tokio::test]
async fn pages_that_do_not_add_up_to_the_total_are_incomplete() {
    // Заявлено больше, чем отдано.
    let rig = rig().await;
    fill_project(&rig, 3);
    rig.fake.control().list_total_override = Some(7);
    let error = rig
        .provider
        .list_project_servers(PROJECT)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ListError::Incomplete {
            expected: 7,
            got: 3
        }
    );

    // Заявлено меньше, чем отдано: список менялся во время обхода.
    let rig = rig_with(|_| {}).await;
    fill_project(&rig, 3);
    rig.fake.control().list_total_override = Some(2);
    let error = rig
        .provider
        .list_project_servers(PROJECT)
        .await
        .unwrap_err();
    assert!(matches!(error, ListError::Incomplete { .. }), "{error:?}");

    // Сервер игнорирует смещение и отдаёт одно и то же при заявленном большем числе:
    // повтор не засчитывается за новую страницу.
    let rig = rig_with(|_| {}).await;
    fill_project(&rig, 3);
    {
        let mut state = rig.fake.control();
        state.list_ignores_paging = true;
        state.list_total_override = Some(6);
    }
    let error = rig
        .provider
        .list_project_servers(PROJECT)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            ListError::Incomplete {
                expected: 6,
                got: 3
            }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn servers_appear_in_the_listing_only_after_a_few_listings() {
    let rig = rig().await;
    rig.fake.control().listing_hidden_polls = 2;
    let CreateOutcome::Created { server_id } = rig
        .provider
        .create_server(&create_request("canterlot-runner-w1"))
        .await
    else {
        panic!("create");
    };
    // Два листинга сервер не видят, хотя он создан.
    for _ in 0..2 {
        assert!(
            rig.provider
                .list_project_servers(PROJECT)
                .await
                .unwrap()
                .is_empty()
        );
    }
    // Прямой запрос по номеру сервер видит сразу.
    assert!(matches!(
        rig.provider.get_server(server_id).await.unwrap(),
        ServerLookup::Found(_)
    ));
    let listed = rig.provider.list_project_servers(PROJECT).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, server_id);
    assert_eq!(listed[0].name, "canterlot-runner-w1");
}

// ---- токен ----

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
async fn the_token_never_shows_in_debug_errors_or_logs() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let rig = rig().await;
    // Неряшливый сервер повторяет заголовок Authorization в текстах ошибок.
    rig.fake.control().echo_authorization_in_errors = true;
    let mut shown: Vec<String> = vec![format!("{:?}", rig.provider), format!("{:?}", rig.config)];

    // Ошибки по всем путям.
    rig.fake.control().create = CreateMode::Reject {
        status: 400,
        message: "bad".to_owned(),
    };
    shown.push(format!(
        "{:?}",
        rig.provider.create_server(&create_request("n")).await
    ));
    rig.fake.control().create = CreateMode::Status(500);
    shown.push(format!(
        "{:?}",
        rig.provider.create_server(&create_request("n")).await
    ));
    rig.fake.control().fail_next.push_back(500);
    shown.push(format!("{:?}", rig.provider.get_server(1).await));
    rig.fake.control().fail_next.push_back(400);
    shown.push(format!("{:?}", rig.provider.configurator(151).await));
    rig.fake.control().fail_next.push_back(409);
    shown.push(format!("{:?}", rig.provider.ensure_ssh_key("k", KEY).await));
    rig.fake.control().list_fail_page = Some(FailPage {
        offset: 0,
        status: 500,
        times: None,
    });
    let listing = rig
        .provider
        .list_project_servers(PROJECT)
        .await
        .unwrap_err();
    shown.push(format!("{listing:?} {listing}"));
    rig.fake.control().delete = DeleteMode::Status(500);
    let id = rig.fake.control().add_server(PROJECT, "x");
    shown.push(format!("{:?}", rig.provider.delete_server(id).await));
    // Неверный токен у самого сервера.
    rig.fake.control().expected_token = "other".to_owned();
    let auth_error = rig.provider.configurator(151).await.unwrap_err();
    shown.push(format!("{auth_error:?} {auth_error}"));

    for text in &shown {
        assert!(!text.contains(TOKEN), "token leaked: {text}");
    }
    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!logs.is_empty(), "the test must capture some logs");
    assert!(!logs.contains(TOKEN), "token leaked into logs");
}

#[tokio::test]
async fn an_echoed_token_is_scrubbed_from_messages() {
    let rig = rig().await;
    rig.fake.control().echo_authorization_in_errors = true;
    rig.fake.control().create = CreateMode::Reject {
        status: 400,
        message: "rejected".to_owned(),
    };
    let CreateOutcome::Rejected { message, .. } =
        rig.provider.create_server(&create_request("n")).await
    else {
        panic!("rejected expected");
    };
    assert!(message.starts_with("rejected"));
    assert!(!message.contains(TOKEN));
    assert!(message.contains("[redacted]"));
}

#[tokio::test]
async fn the_token_is_read_once_from_the_file_and_trimmed() {
    let rig = rig().await;
    // Файл меняют после создания клиента: он продолжает работать со старым значением.
    std::fs::write(&rig.config.token_file, "something-else\n").unwrap();
    rig.provider.configurator(151).await.unwrap();
}

#[test]
fn construction_errors_do_not_include_token_text() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("absent");
    let error = TimewebProvider::new(TimewebConfig::new(&missing)).unwrap_err();
    assert!(error.to_string().contains("token file"));

    let empty = dir.path().join("empty");
    std::fs::write(&empty, "  \n").unwrap();
    let error = TimewebProvider::new(TimewebConfig::new(&empty)).unwrap_err();
    assert_eq!(
        error,
        canterlot_hub::cloud::timeweb::TimewebInitError::EmptyToken
    );
}

#[test]
fn defaults_match_the_documented_limits() {
    let config = TimewebConfig::new("/srv/runner/token");
    assert_eq!(config.base_url, "https://api.timeweb.cloud");
    assert_eq!(config.request_timeout, Duration::from_secs(30));
    assert_eq!(
        config.rate_limit,
        RateLimit {
            max: 20,
            per: Duration::from_secs(1)
        }
    );
}

// ---- предел частоты ----

#[tokio::test]
async fn client_side_rate_limit_holds_back_the_request_over_the_limit() {
    // Окно в час: третий запрос к той же точке не уйдёт, пока окно не откроется. Сами сроки
    // окна проверены на часах без хода в модульных тестах предела.
    let rig = rig_with(|c| {
        c.rate_limit = RateLimit {
            max: 2,
            per: Duration::from_secs(3600),
        }
    })
    .await;
    rig.provider.get_server(1).await.unwrap();
    rig.provider.get_server(2).await.unwrap();
    let held = tokio::time::timeout(Duration::from_millis(300), rig.provider.get_server(3)).await;
    assert!(held.is_err(), "the third request must be held back");
    assert_eq!(rig.fake.control().count("GET", "/api/v1/servers/"), 2);
    // Другая точка считается отдельно.
    rig.provider.configurator(151).await.unwrap();
}
