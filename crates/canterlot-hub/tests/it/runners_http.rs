//! Раннеры в HTTP API хаба: поля машин, доступность раннера и отказ `machine_is_runner` в
//! маршрутах, которые начинают сессию на раннере.

use canterlot_hub::{
    executions,
    http::{self, AppState},
    machines::{self, RunnerSpec},
    session::Registry,
    testkit::{self, FakeSession, Method, TestClient},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Api {
    pool: PgPool,
    client: TestClient,
    registry: Registry,
    owner: Uuid,
    carrier: Uuid,
    runner: Uuid,
    worker: Uuid,
    sessions: Vec<FakeSession>,
    _dir: tempfile::TempDir,
}

impl Api {
    async fn new(pool: PgPool) -> Self {
        let (deps, dir) =
            testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
        let registry = deps.registry.clone();
        let owner = deps.installation.owner_id;
        let carrier = machines::add(&pool, owner, "carrier").await.unwrap();
        machines::set_carrier(&pool, carrier.machine_id, true)
            .await
            .unwrap();
        let runner = machines::add_runner(
            &pool,
            owner,
            "runner-a",
            &RunnerSpec {
                parent: carrier.machine_id,
                root_dir: "/srv/runner".into(),
                rank: 20,
                labels: vec!["build-host".into(), "kernel-6.12".into()],
            },
        )
        .await
        .unwrap();
        let worker = machines::add(&pool, owner, "worker-a").await.unwrap();
        // Обычной машине нужен каталог, чтобы сессия на ней вообще была возможна.
        testkit::seed_catalog(&pool, worker.machine_id).await;
        let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
        Self {
            pool,
            client,
            registry,
            owner,
            carrier: carrier.machine_id,
            runner: runner.machine_id,
            worker: worker.machine_id,
            sessions: Vec::new(),
            _dir: dir,
        }
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>, status: u16) -> Value {
        let r = self
            .client
            .send(method, path, Some(&Uuid::now_v7().to_string()), body)
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

    async fn machine(&self, id: Uuid) -> Value {
        self.call(Method::GET, &format!("/api/v1/machines/{id}"), None, 200)
            .await
    }

    fn connect(&mut self, id: Uuid) {
        self.sessions
            .push(testkit::fake_session(&self.registry, id, &[], true));
    }
}

#[tokio::test]
async fn machines_expose_role_parent_rank_labels_and_carrier() {
    let pool = canterlot_hub::testkit::db().await;
    let api = Api::new(pool).await;
    let runner = api.machine(api.runner).await;
    assert_eq!(runner["role"], "runner");
    assert_eq!(runner["parent_machine_id"], api.carrier.to_string());
    assert_eq!(runner["rank"], 20);
    assert_eq!(runner["labels"], json!(["build-host", "kernel-6.12"]));
    assert_eq!(runner["is_carrier"], false);
    assert_eq!(runner["accepts_remote_work"], true);

    let carrier = api.machine(api.carrier).await;
    assert_eq!(carrier["role"], "worker");
    assert_eq!(carrier["parent_machine_id"], Value::Null);
    assert_eq!(carrier["is_carrier"], true);

    // Обычная машина хранит прежние поля: доступность из её связи, наблюдения нет.
    let worker = api.machine(api.worker).await;
    assert_eq!(worker["role"], "worker");
    assert_eq!(worker["rank"], 0);
    assert_eq!(worker["labels"], json!([]));
    assert_eq!(worker["observed_at"], Value::Null);
    assert!(
        ["available", "degraded", "unavailable"]
            .contains(&worker["availability"].as_str().unwrap()),
        "{worker}"
    );

    // Список и снимок несут те же поля.
    let list = api.call(Method::GET, "/api/v1/machines", None, 200).await;
    let listed = list
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == api.runner.to_string())
        .expect("the runner is listed");
    assert_eq!(listed["role"], "runner");
    assert_eq!(listed["rank"], 20);
}

#[tokio::test]
async fn a_runners_availability_follows_the_spec_rule() {
    let pool = canterlot_hub::testkit::db().await;
    let mut api = Api::new(pool).await;
    // Ничего не известно.
    let runner = api.machine(api.runner).await;
    assert_eq!(runner["availability"], "unknown");
    assert_eq!(runner["observed_at"], Value::Null);

    // Несущая на связи и три отказа подряд: offline, с возрастом наблюдения.
    api.connect(api.carrier);
    machines::record_runner_observation(&api.pool, api.carrier, api.runner, false, 3, Utc::now())
        .await
        .unwrap();
    let runner = api.machine(api.runner).await;
    assert_eq!(runner["availability"], "offline");
    assert!(runner["observed_at"].is_string(), "{runner}");
    // «С какого времени offline» ставит проектор по времени хаба; правило этапа C отсчитывает
    // десять минут от него.
    canterlot_hub::machine_info::project_availability(&api.pool, &api.registry, Uuid::now_v7())
        .await
        .unwrap();
    let runner = api.machine(api.runner).await;
    let since = runner["offline_since"].as_str().expect("offline_since");
    let since = chrono::DateTime::parse_from_rfc3339(since).unwrap();
    assert!(Utc::now() - since.with_timezone(&Utc) < chrono::TimeDelta::seconds(30));
    assert_eq!(api.machine(api.worker).await["offline_since"], Value::Null);

    // Два отказа — ещё unknown.
    machines::record_runner_observation(&api.pool, api.carrier, api.runner, false, 2, Utc::now())
        .await
        .unwrap();
    assert_eq!(api.machine(api.runner).await["availability"], "unknown");
    machines::record_runner_observation(&api.pool, api.carrier, api.runner, false, 5, Utc::now())
        .await
        .unwrap();
    assert_eq!(api.machine(api.runner).await["availability"], "offline");

    // Устаревшее наблюдение — unknown.
    sqlx::query("update runner_observations set observed_at = now() - interval '4 minutes'")
        .execute(&api.pool)
        .await
        .unwrap();
    assert_eq!(api.machine(api.runner).await["availability"], "unknown");

    // Воркер раннера подключён — online, отметка «offline с» снята.
    api.connect(api.runner);
    canterlot_hub::machine_info::project_availability(&api.pool, &api.registry, Uuid::now_v7())
        .await
        .unwrap();
    let runner = api.machine(api.runner).await;
    assert_eq!(runner["availability"], "online");
    assert_eq!(runner["offline_since"], Value::Null);
}

#[tokio::test]
async fn the_new_session_route_refuses_a_runner() {
    let pool = canterlot_hub::testkit::db().await;
    let api = Api::new(pool).await;
    // Каталог раннеру не нужен: отказ раньше любой проверки каталога и возможностей.
    let body = json!({
        "machine_id": api.runner, "harness": "synthetic", "workdir": "/srv/runner/jobs/personal/x",
        "model": "synthetic-small", "effort": "high",
    });
    let error = api
        .call(
            Method::POST,
            "/api/v1/conversations",
            Some(body.clone()),
            409,
        )
        .await;
    assert_eq!(error["error"]["code"], "machine_is_runner", "{error}");
    let mut with_catalog = body;
    with_catalog["machine_id"] = json!(api.worker);
    with_catalog["workdir"] = json!("/tmp/w");
    // Контроль: обычная машина проходит тот же маршрут (или отказывает другим кодом).
    let r = api
        .client
        .send(
            Method::POST,
            "/api/v1/conversations",
            Some(&Uuid::now_v7().to_string()),
            Some(with_catalog),
        )
        .await;
    assert_ne!(
        r.json::<Value>()["error"]["code"],
        "machine_is_runner",
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let on_runner: i64 =
        sqlx::query_scalar("select count(*) from conversations where machine_id=$1")
            .bind(api.runner)
            .fetch_one(&api.pool)
            .await
            .unwrap();
    assert_eq!(on_runner, 0);
}

#[tokio::test]
async fn an_execution_on_a_runner_is_refused() {
    let pool = canterlot_hub::testkit::db().await;
    let api = Api::new(pool).await;
    // Беседа, оказавшаяся на раннере в обход маршрута создания (например, до смены роли).
    let conversation = testkit::seed_conversation(&api.pool, api.owner, api.runner).await;
    let error = api
        .call(
            Method::POST,
            &format!("/api/v1/conversations/{conversation}/executions"),
            None,
            409,
        )
        .await;
    assert_eq!(error["error"]["code"], "machine_is_runner", "{error}");
    let direct = executions::start(&api.pool, api.owner, conversation, Uuid::now_v7()).await;
    assert!(matches!(
        direct,
        Err(executions::ExecutionError::MachineIsRunner)
    ));
    let started: i64 =
        sqlx::query_scalar("select count(*) from executions where conversation_id=$1")
            .bind(conversation)
            .fetch_one(&api.pool)
            .await
            .unwrap();
    assert_eq!(started, 0);
    // На обычной машине тот же вызов работает.
    let ordinary = testkit::seed_conversation(&api.pool, api.owner, api.worker).await;
    executions::start(&api.pool, api.owner, ordinary, Uuid::now_v7())
        .await
        .unwrap();
}

/// Пауза раннера (выключатель) закрывает новые привязки, а её снятие через HTTP ставит
/// ожидающую задачу на раннер без ожидания таймера.
#[tokio::test]
async fn switching_the_runner_on_places_the_waiting_job() {
    let pool = canterlot_hub::testkit::db().await;
    let mut api = Api::new(pool).await;
    sqlx::query(
        "update machines set capabilities=array['jobs_inplace_v1'], jobs_max_running=1
          where id=$1",
    )
    .bind(api.runner)
    .execute(&api.pool)
    .await
    .unwrap();
    api.connect(api.runner);
    let switch = |on: bool| json!({"accepts_remote_work": on});
    api.call(
        Method::PUT,
        &format!("/api/v1/machines/{}/remote-work", api.runner),
        Some(switch(false)),
        200,
    )
    .await;
    let spec = json!({
        "shell": "make", "placement": ["runner"],
        "source": {"repo": "/srv/src/app", "rev": "0123456789abcdef0123456789abcdef01234567"},
    });
    let id = canterlot_hub::inplace_jobs::owner::submit(
        &api.pool,
        &api.registry,
        api.carrier,
        &spec.to_string(),
    )
    .await
    .unwrap();
    let state = || async {
        sqlx::query_scalar::<_, String>("select state from jobs where id=$1")
            .bind(id)
            .fetch_one(&api.pool)
            .await
            .unwrap()
    };
    assert_eq!(state().await, "waiting_runner");
    api.call(
        Method::PUT,
        &format!("/api/v1/machines/{}/remote-work", api.runner),
        Some(switch(true)),
        200,
    )
    .await;
    assert_eq!(state().await, "staging");
    let listed = api.call(Method::GET, "/api/v1/jobs", None, 200).await;
    let item = &listed["items"][0];
    assert_eq!(item["state"], "staging");
    assert_eq!(item["machine_name"], "runner-a");
    assert_eq!(
        item["source"],
        json!({"repo": "app", "rev": "0123456789ab"})
    );
}
