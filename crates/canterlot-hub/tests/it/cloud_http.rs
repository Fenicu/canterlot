//! Окна облачных машин в HTTP владельца: список с деньгами и «Удалить сейчас».

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

    async fn insert(&self, state: &str, minutes_ago: i64, instance_id: Option<&str>) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into cloud_windows (id, name, state, trigger, folder_id, preemptible,
                                        instance_id, public_ip, host_key_public,
                                        compute_kopecks_per_hour, disk_kopecks_per_hour,
                                        price_hourly_kopecks, reservation_kopecks, final_cost_kopecks,
                                        month, hard_cap_minutes, created_at, create_called_at,
                                        profiles, cores, memory_gb, disk_gb)
             values ($1, 'canterlot-runner-' || $1::text, $2, 'owner', 'b1gfakefolder000000', true,
                     $3, case when $3 is null then null else '203.0.113.7' end,
                     'ssh-ed25519 AAAA', 1646, 160, 1806,
                     7224, case when $2 in ('ended', 'failed', 'gone') then 1806 end, '2026-11', 60,
                     now() - make_interval(mins => $4::int),
                     case when $2 = 'create_intent' then null
                          else now() - make_interval(mins => $4::int) end,
                     '[{\"cores\":32,\"memory_gb\":64,\"disk_gb\":80,\"compute_kopecks_per_hour\":1646,\"disk_kopecks_per_hour\":160}]',
                     32, 64, 80)",
        )
        .bind(id)
        .bind(state)
        .bind(instance_id)
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

#[tokio::test]
async fn the_list_has_live_windows_and_the_last_twenty_ended_with_costs() {
    let pool = canterlot_hub::testkit::db().await;
    let api = Api::new(pool).await;
    assert_eq!(api.list().await["items"], json!([]));

    for age in 0..23 {
        api.insert(
            "ended",
            1000 - age,
            Some(&format!("fhmfakeinstance{age:04}")),
        )
        .await;
    }
    let live = api.insert("ready", 60, Some("fhmfakeinstance0777")).await;
    let list = api.list().await;
    assert_eq!(
        list["configured"], false,
        "без [runners.cloud] окна не создаются"
    );
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 21, "one live and the last 20 ended");
    assert_eq!(items[0]["id"], live.to_string(), "the newest first");
    assert_eq!(items[0]["state"], "ready");
    // Окно в новом виде: номер машины строкой, её адрес, вид и две ставки часа.
    assert_eq!(items[0]["instance_id"], "fhmfakeinstance0777");
    assert_eq!(items[0]["public_ip"], "203.0.113.7");
    assert_eq!(items[0]["preemptible"], true);
    assert_eq!(items[0]["replaces_window_id"], Value::Null);
    assert_eq!(items[0]["compute_kopecks_per_hour"], 1646);
    assert_eq!(items[0]["disk_kopecks_per_hour"], 160);
    assert_eq!(items[0]["price_hourly_kopecks"], 1806);
    assert_eq!(items[0]["stopped_at"], Value::Null);
    assert_eq!(items[0]["disk_gone_at"], Value::Null);
    for gone in ["server_id", "local_ip", "dnat_port", "configurator_id"] {
        assert!(items[0].get(gone).is_none(), "{gone} is not in the view");
    }
    assert_eq!(items[0]["reservation_kopecks"], 7224);
    assert_eq!(items[0]["final_cost_kopecks"], Value::Null);
    // Живое окно: расход пока — секунды с вызова создания по обеим ставкам. Час назад:
    // чуть больше цены часа.
    let cost = items[0]["cost_so_far_kopecks"].as_i64().unwrap();
    assert!((1806..1816).contains(&cost), "{cost}");
    assert_eq!(items[0]["month"], "2026-11");
    assert_eq!(items[1]["state"], "ended");
    assert_eq!(items[1]["final_cost_kopecks"], 1806);
    assert_eq!(items[1]["cost_so_far_kopecks"], 1806);
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

#[tokio::test]
async fn the_displayed_cost_is_not_capped_at_the_reservation() {
    let pool = canterlot_hub::testkit::db().await;
    let api = Api::new(pool).await;
    let id = api
        .insert("ready", 1_000, Some("fhmfakeinstance0778"))
        .await;
    let list = api.list().await;
    let item = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == id.to_string())
        .expect("the live window is listed");
    assert_eq!(item["reservation_kopecks"], 7224);
    // 1000 минут по 1806 копеек в час: 30 100 копеек и секунды сверху, хотя резерв меньше.
    let cost = item["cost_so_far_kopecks"].as_i64().unwrap();
    assert!((30_100..30_110).contains(&cost), "{cost}");
    assert!(cost > 7224);
}

#[tokio::test]
async fn only_the_owner_sees_the_windows() {
    let pool = canterlot_hub::testkit::db().await;
    let api = Api::new(pool).await;
    let id = api.insert("ready", 5, Some("fhmfakeinstance0777")).await;
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

#[tokio::test]
async fn delete_now_revokes_the_cloud_runner_and_leaves_the_instance_to_the_service() {
    let pool = canterlot_hub::testkit::db().await;
    let api = Api::new(pool).await;
    let id = api.insert("ready", 5, Some("fhmfakeinstance0777")).await;
    // Строка раннера `cloud` с токеном и задачей на ней.
    let config: CloudConfig = toml::from_str(&format!(
        "provider='yandex'\nkey_file='k.json'\nfolder_id='b1gfakefolder000000'\n\
         zone='ru-central1-a'\nsubnet_id='e9bfakesubnet000000'\n\
         runner_public_key='ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLE'\n\
         carrier_machine_id='{}'\n[[profile]]\ncores=2\nmemory_gb=2\ndisk_gb=10\n",
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

#[tokio::test]
async fn a_delete_replay_never_revokes_the_next_window() {
    let pool = canterlot_hub::testkit::db().await;
    let api = Api::new(pool).await;
    let config = cloud_config(&api);
    let machine = {
        let mut conn = api.pool.acquire().await.unwrap();
        windows::ensure_cloud_machine(&mut conn, &config, true)
            .await
            .unwrap()
    };
    let first = api.insert("ready", 5, Some("fhmfakeinstance0001")).await;
    let key = Uuid::now_v7().to_string();
    let _ = api.delete(first, &key, 200).await;
    // Первое окно закончилось, началось второе; токен у машины новый.
    sqlx::query("update cloud_windows set state = 'ended' where id = $1")
        .bind(first)
        .execute(&api.pool)
        .await
        .unwrap();
    let second = api.insert("ready", 2, Some("fhmfakeinstance0002")).await;
    let token = machines::rotate_token(&api.pool, machine, true, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        machines::verify(&api.pool, &token).await.unwrap(),
        Some(machine)
    );
    // Повтор старой операции и свежий ключ для закончившегося окна не трогают новый токен.
    api.delete(first, &key, 200).await;
    let again = api.delete(first, &Uuid::now_v7().to_string(), 200).await;
    assert_eq!(again["outcome"], "already_ended");
    assert_eq!(
        machines::verify(&api.pool, &token).await.unwrap(),
        Some(machine),
        "the next window's runner stays active"
    );
    assert_eq!(api.list().await["items"][0]["id"], second.to_string());
}

fn cloud_config(api: &Api) -> CloudConfig {
    toml::from_str(&format!(
        "provider='yandex'\nkey_file='k.json'\nfolder_id='b1gfakefolder000000'\n\
         zone='ru-central1-a'\nsubnet_id='e9bfakesubnet000000'\n\
         runner_public_key='ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEXAMPLEEXAMPLE'\n\
         carrier_machine_id='{}'\n[[profile]]\ncores=2\nmemory_gb=2\ndisk_gb=10\n",
        api.carrier
    ))
    .unwrap()
}

#[tokio::test]
async fn delete_now_cancels_a_create_that_was_never_sent_and_refuses_unknown_windows() {
    let pool = canterlot_hub::testkit::db().await;
    let api = Api::new(pool).await;
    let intent = api.insert("create_intent", 0, None).await;
    let result = api.delete(intent, &Uuid::now_v7().to_string(), 200).await;
    assert_eq!(result["outcome"], "canceled");
    assert_eq!(result["window"]["state"], "failed");
    assert_eq!(result["window"]["final_cost_kopecks"], 0);

    let ended = api.insert("ended", 90, Some("fhmfakeinstance0001")).await;
    let result = api.delete(ended, &Uuid::now_v7().to_string(), 200).await;
    assert_eq!(result["outcome"], "already_ended");

    let unknown = api.insert("create_unknown", 3, None).await;
    let result = api.delete(unknown, &Uuid::now_v7().to_string(), 200).await;
    assert_eq!(result["outcome"], "not_yet");
    assert_eq!(result["window"]["state"], "create_unknown");

    api.delete(Uuid::now_v7(), &Uuid::now_v7().to_string(), 404)
        .await;
}
