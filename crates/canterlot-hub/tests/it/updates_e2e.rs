//! Сквозной путь: воркерский MachineReport по gRPC → применение отчёта хабом →
//! лента `GET /api/v1/updates` и живое событие WSS, как в собранном продукте.

use crate::support::task3::{connect_with, wait_until};
use axum::http::Method;
use canterlot_hub::{
    config::HubTimers,
    testkit::{self, TEST_ORIGIN, TEST_SESSION_TOKEN, TestClient, TestWs, test_http_config},
};
use canterlot_proto::worker::v1::{ControlUp, HarnessInfo, MachineReport, ModelInfo, control_up};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn report() -> MachineReport {
    let mut report = crate::support::task3::report();
    let harness = &mut report.harnesses[0];
    *harness = HarnessInfo {
        harness: "codex".into(),
        provider: "codex".into(),
        title: "Codex".into(),
        models: vec![ModelInfo {
            id: "gpt-5".into(),
            title: "GPT-5".into(),
            ..Default::default()
        }],
        installed_version: "0.164.0".into(),
        latest_version: "0.165.0".into(),
        min_version: "0.164.0".into(),
        update_command: "npm i -g @openai/codex@latest".into(),
        ..Default::default()
    };
    report
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn worker_report_reaches_the_feed_and_the_live_stream(pool: PgPool) -> TestResult {
    let hub =
        testkit::spawn_test_hub_http(pool.clone(), HubTimers::default(), test_http_config()).await;
    let owner = hub.installation.owner_id;
    let machine = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'e2e')")
        .bind(machine)
        .bind(owner)
        .execute(&pool)
        .await?;
    sqlx::query("insert into machine_credentials(machine_id,token_sha256) values($1,$2)")
        .bind(machine)
        .bind(Sha256::digest(machine.to_string().as_bytes()).to_vec())
        .execute(&pool)
        .await?;
    let seed = testkit::Seed {
        owner_id: owner,
        machine_id: machine,
        conversation_id: testkit::seed_conversation(&pool, owner, machine).await,
    };

    let mut ws = TestWs::connect(hub.http_addr(), TEST_ORIGIN, Some(TEST_SESSION_TOKEN)).await?;
    ws.send(json!({"op":"subscribe","id":"feed","scope":"machines","after":"0"}))
        .await;
    assert_eq!(
        ws.next(std::time::Duration::from_secs(5)).await.unwrap()["op"],
        "subscribed"
    );

    let client = connect_with(
        &hub,
        &seed,
        Uuid::now_v7(),
        &["pilot_v1", "cli_versions_v1"],
        chrono::Utc::now().timestamp_millis() as u64,
    )
    .await;
    client
        .tx
        .send(ControlUp {
            msg: Some(control_up::Msg::MachineReport(report())),
        })
        .await?;
    wait_until(&pool, "select count(*) from machine_updates").await;
    // Baseline записана, но ни одного события моделей нет.
    let feed: Value = TestClient::over_socket(hub.http_addr())
        .await
        .as_owner()
        .send(Method::GET, "/api/v1/updates", None, None)
        .await
        .json();
    assert_eq!(feed["items"].as_array().unwrap().len(), 1);
    assert_eq!(feed["items"][0]["kind"], "version_available");
    assert_eq!(feed["items"][0]["machine"], "e2e");
    assert_eq!(feed["items"][0]["harness"], "codex");
    assert_eq!(
        feed["items"][0]["update_command"],
        json!("npm i -g @openai/codex@latest")
    );

    let mut seen = false;
    for _ in 0..12 {
        let Some(frame) = ws.next(std::time::Duration::from_secs(2)).await else {
            break;
        };
        if frame["op"] == "event" && frame["event"]["kind"] == "machine_update" {
            assert_eq!(frame["event"]["payload"]["kind"], "version_available");
            assert_eq!(frame["event"]["payload"]["machine"], "e2e");
            seen = true;
            break;
        }
    }
    assert!(seen, "the new feed item arrived over WSS");

    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
