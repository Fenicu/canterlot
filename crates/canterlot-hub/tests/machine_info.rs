#[path = "support/task3.rs"]
mod support;
use canterlot_hub::{
    config::HubTimers,
    events::Scope,
    machine_info::{self, MetricsOutcome},
    singleton::Singleton,
    testkit,
};
use canterlot_proto::worker::v1::*;
use chrono::Utc;
use serde_json::Value;
use sqlx::PgPool;
use support::*;
use uuid::Uuid;
type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn machine_view_has_no_profiles(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    let mut r = report();
    machine_info::apply_report(&pool, s.owner_id, s.machine_id, &r, Uuid::now_v7()).await?;
    r.harnesses[0].harness = "synthetic-codex".into();
    r.harnesses[0].provider = "codex".into();
    machine_info::apply_report(&pool, s.owner_id, s.machine_id, &r, Uuid::now_v7()).await?;
    assert_eq!(
        count(&pool, "select count(*) from machine_harnesses").await,
        1
    );
    // Таблица профилей инструкций удалена планом 2c: пресеты хранит хаб.
    let tables: i64 = sqlx::query_scalar(
        "select count(*) from information_schema.tables where table_name='machine_profiles'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(tables, 0);
    let row: (Vec<String>, Value, String, String, String) = sqlx::query_as(
        "select capabilities,allowed_roots,worker_version,containment,os from machines where id=$1",
    )
    .bind(s.machine_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        row,
        (
            vec!["pilot_v1".into()],
            serde_json::json!(["/tmp"]),
            "test".into(),
            "process_group".into(),
            "linux".into()
        )
    );
    let n: i64 = sqlx::query_scalar(
        "select count(*) from events where kind='machine_report_updated' and scope=$1",
    )
    .bind(
        Scope::Machines {
            owner_id: s.owner_id,
        }
        .key(),
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(n, 2);
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn report_with_mismatching_attachment_limits_flags_the_machine(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    let mut r = report();
    r.max_attachment_bytes -= 1;
    machine_info::apply_report(&pool, s.owner_id, s.machine_id, &r, Uuid::now_v7()).await?;
    assert_eq!(
        count(&pool, "select count(*) from machines where not limits_ok").await,
        1
    );
    machine_info::apply_report(&pool, s.owner_id, s.machine_id, &report(), Uuid::now_v7()).await?;
    assert_eq!(
        count(&pool, "select count(*) from machines where not limits_ok").await,
        0
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn metrics_upsert_keeps_latest_and_appends_a_machines_event(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    for ms in [1000, 2000] {
        assert_eq!(
            machine_info::apply_metrics(
                &pool,
                s.owner_id,
                s.machine_id,
                &metrics(ms),
                Uuid::now_v7()
            )
            .await?,
            MetricsOutcome::Stored
        );
    }
    assert!(matches!(
        machine_info::apply_metrics(
            &pool,
            s.owner_id,
            s.machine_id,
            &metrics(1500),
            Uuid::now_v7()
        )
        .await?,
        MetricsOutcome::Ignored(_)
    ));
    assert_eq!(
        count(&pool, "select machine_time_ms from machine_metrics").await,
        2000
    );
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='machine_metrics'"
        )
        .await,
        2
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn invalid_metrics_are_ignored_without_ending_the_stream(pool: PgPool) -> TestResult {
    let s = wire_seed(&pool).await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let mut c = connect(&hub, &s, Uuid::now_v7(), true, 1000).await;
    let mut bad = metrics(1000);
    bad.cpu_permille = 1001;
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::MachineMetrics(bad)),
    })
    .await?;
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::MachineMetrics(metrics(2000))),
    })
    .await?;
    wait_until(
        &pool,
        "select count(*) from machine_metrics where machine_time_ms=2000",
    )
    .await;
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::Heartbeat(Heartbeat {
            seq: 7,
            machine_time_ms: 2001,
        })),
    })
    .await?;
    let down = tokio::time::timeout(WAIT, c.stream.message())
        .await??
        .unwrap();
    assert!(matches!(
        down.msg,
        Some(control_down::Msg::HeartbeatAck(HeartbeatAck { seq: 7 }))
    ));
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='machine_metrics'"
        )
        .await,
        1
    );
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn failed_metrics_apply_does_not_end_the_stream(pool: PgPool) -> TestResult {
    let s = wire_seed(&pool).await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    sqlx::raw_sql("create function fail_metrics() returns trigger language plpgsql as $$ begin raise exception 'metrics_outage'; end $$; create trigger fail_metrics before insert on machine_metrics for each row execute function fail_metrics();").execute(&pool).await?;
    let mut c = connect(&hub, &s, Uuid::now_v7(), true, 1000).await;
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::MachineMetrics(metrics(2000))),
    })
    .await?;
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::Heartbeat(Heartbeat {
            seq: 9,
            machine_time_ms: 2001,
        })),
    })
    .await?;
    let down = tokio::time::timeout(WAIT, c.stream.message())
        .await??
        .unwrap();
    assert!(matches!(
        down.msg,
        Some(control_down::Msg::HeartbeatAck(HeartbeatAck { seq: 9 }))
    ));
    assert_eq!(
        count(&pool, "select count(*) from machine_metrics").await,
        0
    );
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn availability_projection_emits_one_event_per_change(pool: PgPool) -> TestResult {
    use canterlot_hub::session::Availability;
    use tokio_stream::wrappers::ReceiverStream;
    use tonic::transport::{Certificate, ClientTlsConfig, Endpoint};

    let s = wire_seed(&pool).await;
    // Фоновый проектор не должен фиксировать промежуточные состояния подключения.
    let projector = Singleton::acquire(&pool.connect_options(), "machine_status")
        .await?
        .unwrap();
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let journal = Uuid::now_v7();
    let c = connect(&hub, &s, journal, true, 1000).await;
    let channel = Endpoint::from_shared(format!("https://{}", hub.addr))?
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(&hub.pki.ca_pem))
                .domain_name("localhost"),
        )?
        .connect()
        .await?;
    let (data_tx, data_rx) = tokio::sync::mpsc::channel(1);
    data_tx
        .send(DataUp {
            msg: Some(data_up::Msg::Hello(Hello {
                installation_id: hub.installation.installation_id.to_string(),
                restore_generation: hub.installation.restore_generation.to_string(),
                machine_id: s.machine_id.to_string(),
                connection_id: c.connection.to_string(),
                cj_journal_id: journal.to_string(),
                boot_id: Uuid::now_v7().to_string(),
                protocol_version: 1,
                stream_kind: StreamKind::Data as i32,
                ..Default::default()
            })),
        })
        .await?;
    let mut request = tonic::Request::new(ReceiverStream::new(data_rx));
    request
        .metadata_mut()
        .insert("authorization", format!("Bearer {}", s.machine_id).parse()?);
    let mut data = worker_data_client::WorkerDataClient::new(channel)
        .data(request)
        .await?
        .into_inner();
    assert!(matches!(
        tokio::time::timeout(WAIT, data.message())
            .await??
            .unwrap()
            .msg,
        Some(data_down::Msg::HelloReply(HelloReply {
            result: Some(hello_reply::Result::Accepted(_)),
        }))
    ));
    tokio::time::timeout(WAIT, async {
        while hub.registry.availability(s.machine_id) != Availability::Available {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await?;
    machine_info::project_availability(&pool, &hub.registry, Uuid::now_v7()).await?;
    machine_info::project_availability(&pool, &hub.registry, Uuid::now_v7()).await?;
    let n = count(
        &pool,
        "select count(*) from events where kind='machine_availability_changed'",
    )
    .await;
    assert_eq!(n, 1);
    drop(c);
    tokio::time::timeout(WAIT, async {
        while hub.registry.session_info(s.machine_id).is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await?;
    machine_info::project_availability(&pool, &hub.registry, Uuid::now_v7()).await?;
    machine_info::project_availability(&pool, &hub.registry, Uuid::now_v7()).await?;
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='machine_availability_changed'"
        )
        .await,
        2
    );
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    projector.release().await;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn hub_start_resets_stored_availability(pool: PgPool) -> TestResult {
    let _s = testkit::seed(&pool).await;
    sqlx::query("update machines set availability='available'")
        .execute(&pool)
        .await?;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let p = pool.clone();
    let task = tokio::spawn(machine_info::run_status_projection(p, registry(), rx));
    wait_until(
        &pool,
        "select count(*) from machines where availability='unavailable'",
    )
    .await;
    stop.send(true)?;
    task.await?;
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='machine_availability_changed'"
        )
        .await,
        1
    );
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn metrics_of_one_machine_never_touch_another(pool: PgPool) -> TestResult {
    let a = testkit::seed(&pool).await;
    let b = Run::new(&pool).await;
    let mut m = metrics(1000);
    m.executions.push(ExecutionProcess {
        execution_id: b.execution.execution_id.to_string(),
        alive: true,
        ..Default::default()
    });
    assert!(matches!(
        machine_info::apply_metrics(&pool, a.owner_id, a.machine_id, &m, Uuid::now_v7()).await?,
        MetricsOutcome::Ignored(_)
    ));
    assert_eq!(
        count(&pool, "select count(*) from machine_metrics").await,
        0
    );
    let mut invalid = metrics(2000);
    invalid.disks[0].label = "/private/path".into();
    assert!(matches!(
        machine_info::apply_metrics(&pool, a.owner_id, a.machine_id, &invalid, Uuid::now_v7())
            .await?,
        MetricsOutcome::Ignored(_)
    ));
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn pilot_capability_is_echoed_and_stored_per_session(pool: PgPool) -> TestResult {
    let a = wire_seed(&pool).await;
    let b = wire_seed(&pool).await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let _a = connect(&hub, &a, Uuid::now_v7(), true, 1000).await;
    let _b = connect(&hub, &b, Uuid::now_v7(), false, 1000).await;
    assert!(hub.registry.has_capability(a.machine_id, "pilot_v1"));
    assert!(!hub.registry.has_capability(b.machine_id, "pilot_v1"));
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn workdir_suggestions_travel_the_control_stream(pool: PgPool) -> TestResult {
    let s = wire_seed(&pool).await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let mut c = connect_with(
        &hub,
        &s,
        Uuid::now_v7(),
        &["pilot_v1", "workdir_suggest_v1"],
        1000,
    )
    .await;
    let registry = hub.registry.clone();
    let machine = s.machine_id;
    let asking = tokio::spawn(async move {
        registry
            .suggest_workdirs(machine, "/srv/ca", std::time::Duration::from_secs(10))
            .await
    });
    let request = loop {
        let down = tokio::time::timeout(WAIT, c.stream.message())
            .await??
            .unwrap();
        if let Some(control_down::Msg::SuggestWorkdirs(request)) = down.msg {
            break request;
        }
    };
    assert_eq!(request.path, "/srv/ca");
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::WorkdirSuggestions(WorkdirSuggestions {
            request_id: request.request_id,
            directories: vec!["/srv/canterlot".into()],
        })),
    })
    .await?;
    assert_eq!(asking.await?, ["/srv/canterlot"]);
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
// Отказ в отчёте закрывает поток, и воркер прислал бы тот же отчёт снова.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn nul_in_a_report_never_refuses_it(pool: PgPool) -> TestResult {
    let s = testkit::seed(&pool).await;
    let mut r = report();
    r.worker_version = "ver\0sion".into();
    r.allowed_roots = vec!["/ro\0ot".into()];
    r.containment = "cont\0ainment".into();
    r.os = "o\0s".into();
    r.home_dir = "/ho\0me".into();
    r.harnesses[0].title = "ti\0tle".into();
    r.harnesses[0].provider = "provi\0der".into();
    r.harnesses[0].permission_modes = vec!["mo\0de".into()];
    r.harnesses[0].models = vec![ModelInfo {
        id: "i\0d".into(),
        title: "ti\0tle".into(),
        efforts: vec!["eff\0ort".into()],
        ..Default::default()
    }];
    machine_info::apply_report(&pool, s.owner_id, s.machine_id, &r, Uuid::now_v7()).await?;
    let models: Value = sqlx::query_scalar("select models from machine_harnesses")
        .fetch_one(&pool)
        .await?;
    assert_eq!(models[0]["id"], "i\u{fffd}d");
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn failed_report_apply_is_retried_after_reconnect(pool: PgPool) -> TestResult {
    let s = wire_seed(&pool).await;
    let hub = testkit::spawn_test_hub(pool.clone(), HubTimers::default()).await;
    let journal = Uuid::now_v7();
    sqlx::raw_sql("create function fail_report() returns trigger language plpgsql as $$ begin raise exception 'report_outage'; end $$; create trigger fail_report before insert on machine_harnesses for each row execute function fail_report();").execute(&pool).await?;
    let mut c = connect(&hub, &s, journal, true, 1000).await;
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::MachineReport(report())),
    })
    .await?;
    let result = tokio::time::timeout(WAIT, c.stream.message()).await?;
    assert!(result.is_err());
    assert_eq!(
        count(&pool, "select count(*) from machine_harnesses").await,
        0
    );
    drop(c);
    sqlx::query("drop trigger fail_report on machine_harnesses")
        .execute(&pool)
        .await?;
    let c = connect(&hub, &s, journal, true, 2000).await;
    c.tx.send(ControlUp {
        msg: Some(control_up::Msg::MachineReport(report())),
    })
    .await?;
    wait_until(&pool, "select count(*) from machine_harnesses").await;
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='machine_report_updated'"
        )
        .await,
        1
    );
    hub.shutdown.send(()).unwrap();
    hub.join.await??;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn singleton_lock_is_dedicated_and_status_projection_recovers_after_loss(
    pool: PgPool,
) -> TestResult {
    let options = pool.connect_options();
    let mut first = Singleton::acquire(&options, "test").await?.unwrap();
    assert!(Singleton::acquire(&options, "test").await?.is_none());
    assert!(first.still_held().await);
    let pid:i32=sqlx::query_scalar("select pid from pg_locks where locktype='advisory' and database=(select oid from pg_database where datname=current_database())").fetch_one(&pool).await?;
    sqlx::query("select pg_terminate_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await?;
    assert!(!first.still_held().await);
    first.release().await;
    let second = Singleton::acquire(&options, "test").await?.unwrap();
    second.release().await;
    let _s = testkit::seed(&pool).await;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(machine_info::run_status_projection(
        pool.clone(),
        registry(),
        rx,
    ));
    let pid=tokio::time::timeout(WAIT,async {loop {let pid:Option<i32>=sqlx::query_scalar("select pid from pg_locks where locktype='advisory' and database=(select oid from pg_database where datname=current_database())").fetch_optional(&pool).await.unwrap();if let Some(pid)=pid {break pid;} tokio::time::sleep(std::time::Duration::from_millis(10)).await;}}).await?;
    sqlx::query("select pg_terminate_backend($1)")
        .bind(pid)
        .execute(&pool)
        .await?;
    tokio::time::timeout(WAIT, async {loop {
        let recovered:bool=sqlx::query_scalar("select exists(select 1 from pg_locks where locktype='advisory' and database=(select oid from pg_database where datname=current_database()) and pid<>$1)").bind(pid).fetch_one(&pool).await.unwrap();
        if recovered {break;}
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }}).await?;
    assert!(!task.is_finished());
    stop.send(true)?;
    tokio::time::timeout(WAIT, task).await??;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn passive_hub_does_not_reset_stored_availability(pool: PgPool) -> TestResult {
    let _s = testkit::seed(&pool).await;
    let lock = Singleton::acquire(&pool.connect_options(), "machine_status")
        .await?
        .unwrap();
    sqlx::query("update machines set availability='available'")
        .execute(&pool)
        .await?;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(machine_info::run_status_projection(
        pool.clone(),
        registry(),
        rx,
    ));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(!task.is_finished());
    assert_eq!(
        count(
            &pool,
            "select count(*) from machines where availability='available'"
        )
        .await,
        1
    );
    stop.send(true)?;
    tokio::time::timeout(WAIT, task).await??;
    lock.release().await;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn passive_hub_takes_over_when_the_holder_leaves(pool: PgPool) -> TestResult {
    let _s = testkit::seed(&pool).await;
    let lock = Singleton::acquire(&pool.connect_options(), "machine_status")
        .await?
        .unwrap();
    sqlx::query("update machines set availability='available'")
        .execute(&pool)
        .await?;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(machine_info::run_status_projection(
        pool.clone(),
        registry(),
        rx,
    ));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    lock.release().await;
    wait_until(
        &pool,
        "select count(*) from machines where availability='unavailable'",
    )
    .await;
    stop.send(true)?;
    tokio::time::timeout(WAIT, task).await??;
    Ok(())
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn report_and_metrics_reach_the_hub_over_the_control_stream(pool: PgPool) -> TestResult {
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
