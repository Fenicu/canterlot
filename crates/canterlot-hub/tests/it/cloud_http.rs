//! Окна облачных серверов в HTTP владельца: список с деньгами и «Удалить сейчас».

use canterlot_hub::{
    cloud::windows,
    config::CloudConfig,
    http::{self, AppState},
    machines,
    testkit::{self, Method, TestClient},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Api {
    pool: PgPool,
    client: TestClient,
    anonymous: TestClient,
    owner: Uuid,
    carrier: Uuid,
    _dir: tempfile::TempDir,
}

impl Api {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let owner = deps.installation.owner_id;
        let carrier = machines::add(&pool, owner, "carrier").await.unwrap();
        machines::set_carrier(&pool, carrier.machine_id, true)
            .await
            .unwrap();
        let router = http::router(AppState::new(deps));
        Self {
            pool,
            client: TestClient::in_process(router.clone()).as_owner(),
            // Браузер без сеанса: источник и заголовок клиента есть, cookie нет.
            anonymous: TestClient::in_process(router)
                .with_header("Origin", testkit::TEST_ORIGIN)
                .with_header("X-Canterlot-Client", "1"),
            owner,
            carrier: carrier.machine_id,
            _dir: dir,
        }
    }

    async fn insert(&self, state: &str, minutes_ago: i64, server_id: Option<i64>) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into cloud_windows (id, name, state, trigger, server_id, host_key_public,
                                        price_hourly_kopecks, reservation_kopecks, final_cost_kopecks,
                                        month, hard_cap_minutes, created_at, create_called_at,
                                        profiles, configurator_id)
             values ($1, 'canterlot-runner-' || $1::text, $2, 'owner', $3, 'ssh-ed25519 AAAA', 2845,
                     11380, case when $2 in ('ended', 'failed', 'gone') then 2845 end, '2026-11', 60,
                     now() - make_interval(mins => $4::int),
                     case when $2 = 'create_intent' then null
                          else now() - make_interval(mins => $4::int) end,
                     '[{\"configurator_id\":151,\"cpu\":32,\"ram_gb\":64,\"disk_gb\":80,\"hourly_kopecks\":2845}]',
                     151)",
        )
        .bind(id)
        .bind(state)
        .bind(server_id)
        .bind(i32::try_from(minutes_ago).unwrap())
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    async fn delete(&self, id: Uuid, key: &str, status: u16) -> Value {
        let r = self
            .client
            .send(
                Method::POST,
                &format!("/api/v1/cloud/windows/{id}/delete"),
                Some(key),
                None,
            )
            .await;
        assert_eq!(
            r.status.as_u16(),
            status,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        if r.body.is_empty() {
            Value::Null
        } else {
            r.json()
        }
    }

    async fn list(&self) -> Value {
        let r = self
            .client
            .send(Method::GET, "/api/v1/cloud/windows", None, None)
            .await;
        assert_eq!(
            r.status.as_u16(),
            200,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        r.json()
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_list_has_live_windows_and_the_last_twenty_ended_with_costs(pool: PgPool) {
    let api = Api::new(pool).await;
    assert_eq!(api.list().await["items"], json!([]));

    for age in 0..23 {
        api.insert("ended", 1000 - age, Some(5000 + age)).await;
    }
    let live = api.insert("ready", 5, Some(777)).await;
    let list = api.list().await;
    assert_eq!(
        list["configured"], false,
        "без [runners.cloud] окна не создаются"
    );
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 21, "one live and the last 20 ended");
    assert_eq!(items[0]["id"], live.to_string(), "the newest first");
    assert_eq!(items[0]["state"], "ready");
    assert_eq!(items[0]["price_hourly_kopecks"], 2845);
    assert_eq!(items[0]["reservation_kopecks"], 11380);
    assert_eq!(items[0]["final_cost_kopecks"], Value::Null);
    // Живое окно: расход пока — оплаченные часы с вызова создания, не больше резерва.
    assert_eq!(items[0]["cost_so_far_kopecks"], 2845);
    assert_eq!(items[0]["month"], "2026-11");
    assert_eq!(items[1]["state"], "ended");
    assert_eq!(items[1]["final_cost_kopecks"], 2845);
    assert_eq!(items[1]["cost_so_far_kopecks"], 2845);
    let ids: Vec<&str> = items
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    let mut sorted = ids.clone();
    sorted.dedup();
    assert_eq!(ids, sorted);
    // В ответе нет ни ключей, ни токена.
    let text = list.to_string();
    assert!(!text.contains("PRIVATE") && !text.contains("host_key"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn only_the_owner_sees_the_windows(pool: PgPool) {
    let api = Api::new(pool).await;
    let id = api.insert("ready", 5, Some(777)).await;
    let list = api
        .anonymous
        .send(Method::GET, "/api/v1/cloud/windows", None, None)
        .await;
    assert_eq!(list.status.as_u16(), 401);
    let delete = api
        .anonymous
        .send(
            Method::POST,
            &format!("/api/v1/cloud/windows/{id}/delete"),
            Some(&Uuid::now_v7().to_string()),
            None,
        )
        .await;
    assert_eq!(delete.status.as_u16(), 401);
    let state: String = sqlx::query_scalar("select state from cloud_windows where id = $1")
        .bind(id)
        .fetch_one(&api.pool)
        .await
        .unwrap();
    assert_eq!(state, "ready", "an unauthenticated call changes nothing");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_now_revokes_the_cloud_runner_and_leaves_the_server_to_the_service(pool: PgPool) {
    let api = Api::new(pool).await;
    let id = api.insert("ready", 5, Some(777)).await;
    // Строка раннера `cloud` с токеном и задачей на ней.
    let config: CloudConfig = toml::from_str(&format!(
        "provider='timeweb'\ntoken_file='t'\nproject_id=1\n\
         network_id='network-example'\nnetwork_cidr='192.0.2.0/24'\nrouter_id='018f0000-0000-7000-8000-0000000000aa'\n\
         router_ip='192.0.2.1'\ngateway='192.0.2.5'\n\
         local_ip_pool='192.0.2.10-192.0.2.60'\ndnat_port_range='22100-22199'\n\
         runner_public_key='ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLE'\n\
         carrier_machine_id='{}'\n[[profile]]\nconfigurator_id=151\ncpu=1\nram_gb=1\ndisk_gb=10\navailability_zone='msk-1'\n",
        api.carrier
    ))
    .unwrap();
    let machine = {
        let mut conn = api.pool.acquire().await.unwrap();
        windows::ensure_cloud_machine(&mut conn, &config, true)
            .await
            .unwrap()
    };
    let token = machines::rotate_token(&api.pool, machine, true, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        machines::verify(&api.pool, &token).await.unwrap(),
        Some(machine)
    );
    let job = Uuid::now_v7();
    sqlx::query(
        "insert into jobs (id, owner_id, domain, machine_id, remote, workdir, shell,
                           timeout_seconds, state)
         values ($1, $2, 'personal', $3, true, '/srv/canterlot-runner/jobs/personal/x', 'true',
                 600, 'running')",
    )
    .bind(job)
    .bind(api.owner)
    .bind(machine)
    .execute(&api.pool)
    .await
    .unwrap();

    let key = Uuid::now_v7().to_string();
    let first = api.delete(id, &key, 200).await;
    assert_eq!(first["outcome"], "deleting");
    assert_eq!(first["window"]["state"], "deleting");
    assert_eq!(first["window"]["delete_reason"], "owner");
    assert_eq!(machines::verify(&api.pool, &token).await.unwrap(), None);
    let job_state: String = sqlx::query_scalar("select state from jobs where id = $1")
        .bind(job)
        .fetch_one(&api.pool)
        .await
        .unwrap();
    assert_eq!(job_state, "lost");
    let server_calls: i64 = sqlx::query_scalar(
        "select count(*) from cloud_windows where id = $1 and delete_accepted_at is null",
    )
    .bind(id)
    .fetch_one(&api.pool)
    .await
    .unwrap();
    assert_eq!(
        server_calls, 1,
        "the provider call belongs to the window service"
    );

    // Повтор с тем же ключом отдаёт тот же ответ; с новым ключом окно уже удаляется.
    assert_eq!(api.delete(id, &key, 200).await, first);
    let again = api.delete(id, &Uuid::now_v7().to_string(), 200).await;
    assert_eq!(again["outcome"], "deleting");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_now_cancels_a_create_that_was_never_sent_and_refuses_unknown_windows(pool: PgPool) {
    let api = Api::new(pool).await;
    let intent = api.insert("create_intent", 0, None).await;
    let result = api.delete(intent, &Uuid::now_v7().to_string(), 200).await;
    assert_eq!(result["outcome"], "canceled");
    assert_eq!(result["window"]["state"], "failed");
    assert_eq!(result["window"]["final_cost_kopecks"], 0);

    let ended = api.insert("ended", 90, Some(1)).await;
    let result = api.delete(ended, &Uuid::now_v7().to_string(), 200).await;
    assert_eq!(result["outcome"], "already_ended");

    let unknown = api.insert("create_unknown", 3, None).await;
    let result = api.delete(unknown, &Uuid::now_v7().to_string(), 200).await;
    assert_eq!(result["outcome"], "not_yet");
    assert_eq!(result["window"]["state"], "create_unknown");

    api.delete(Uuid::now_v7(), &Uuid::now_v7().to_string(), 404)
        .await;
}
