use crate::support::task3 as support;
use canterlot_hub::{config::HubTimers, testkit};
use canterlot_proto::worker::v1::*;
use chrono::Utc;
use support::*;
use uuid::Uuid;
type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
#[tokio::test]
async fn report_and_metrics_reach_the_hub_over_the_control_stream() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let s = wire_seed(&pool).await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let c = connect(
        &hub,
        &s,
        Uuid::now_v7(),
        true,
        Utc::now().timestamp_millis() as u64,
    )
    .await;
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::MachineReport(report())),
    })
    .await?;
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::MachineMetrics(metrics(2000))),
    })
    .await?;
    wait_until(&pool, "select count(*) from machine_metrics").await;
    assert_eq!(
        count(&pool, "select count(*) from machine_harnesses").await,
        1
    );
    let times:Vec<String>=sqlx::query_scalar("select kind from events where kind in ('machine_report_updated','machine_metrics') order by scope_seq").fetch_all(&pool).await?;
    assert_eq!(times, vec!["machine_report_updated", "machine_metrics"]);
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
#[tokio::test]
async fn machine_row_keeps_the_negotiated_capabilities() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let s = wire_seed(&pool).await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let client = connect_with(
        &hub,
        &s,
        Uuid::now_v7(),
        &["pilot_v1", "claude_histories_v1"],
        1000,
    )
    .await;
    client
        .tx
        .send(ControlUp {
            msg: Some(control_up::Msg::MachineReport(report())),
        })
        .await?;
    // PWA показывает продолжение истории по строке машины, а не по живому соединению.
    wait_until(
        &pool,
        "select count(*) from machines where 'claude_histories_v1' = any(capabilities) and 'pilot_v1' = any(capabilities)",
    )
    .await;
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
