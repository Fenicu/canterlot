#[path = "support/task7.rs"]
mod support;
#[path = "support/task3.rs"]
mod worker;
use canterlot_hub::{
    activity, conversations,
    delivery::control,
    events::{self, CLIENT_KINDS},
    machine_info,
};
use canterlot_proto::worker::v1::{record_body::Body, *};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::BTreeSet;
use support::*;
use uuid::Uuid;
fn schema() -> Value {
    serde_json::from_str(include_str!("../../../docs/api/events.schema.json")).unwrap()
}
fn validator(name: &str) -> jsonschema::Validator {
    let mut s = schema();
    s["$ref"] = json!(format!("#/$defs/{name}"));
    s.as_object_mut().unwrap().remove("oneOf");
    jsonschema::validator_for(&s).unwrap()
}
#[test]
fn every_client_visible_kind_has_a_schema() {
    let s = schema();
    assert_eq!(s["$schema"], "https://json-schema.org/draft/2020-12/schema");
    let unique: BTreeSet<_> = CLIENT_KINDS.iter().collect();
    assert_eq!(unique.len(), CLIENT_KINDS.len());
    for kind in CLIENT_KINDS {
        assert!(s["$defs"].get(kind).is_some(), "missing schema: {kind}");
        validator(kind);
    }
    for kind in [
        "journal_closed",
        "execution_fact_rejected",
        "journal_record_mismatch",
        "local_state_reset",
    ] {
        assert!(!events::is_client_visible(kind));
    }
}
#[test]
fn interrupt_progress_kinds_are_client_visible() {
    for kind in [
        "interrupt_state_changed",
        "queue_paused",
        "adapter_diagnostic",
        "state_divergence",
        "block_revision_gap",
        "block_checksum_mismatch",
        "block_conflicting_final",
    ] {
        assert!(events::is_client_visible(kind));
    }
}
#[test]
fn hidden_placeholder_has_a_schema_and_never_carries_a_payload() {
    let now = chrono::Utc::now();
    let source = |seq, scope, kind: &str| events::StoredEvent {
        id: Uuid::now_v7(),
        scope,
        scope_seq: seq,
        kind: kind.into(),
        conversation_id: Some(Uuid::now_v7()),
        execution_id: Some(Uuid::now_v7()),
        payload: json!({"internal":"private"}),
        correlation_id: Uuid::now_v7(),
        committed_at: now,
    };
    let scope = format!("owner:{}:conversations", Uuid::now_v7());
    let es = events::project_for_client(vec![
        source(1, scope.clone(), "journal_record_mismatch"),
        source(2, scope.clone(), "local_state_reset"),
        source(4, scope, "journal_closed"),
        source(
            5,
            format!("conversation:{}", Uuid::now_v7()),
            "journal_closed",
        ),
    ]);
    assert_eq!(es.len(), 3);
    for e in es {
        let v = serde_json::to_value(e).unwrap();
        assert_eq!(v["payload"], empty());
        assert!(v["conversation_id"].is_null());
        assert!(v["execution_id"].is_null());
        assert!(validator("hidden").is_valid(&v));
        let mut bad = v;
        bad["payload"] = json!({"internal":"private"});
        assert!(!validator("hidden").is_valid(&bad));
    }
}
#[test]
fn ws_ops_have_client_and_server_op_defs() {
    let client = validator("ClientOp");
    let server = validator("ServerOp");
    for op in [
        json!({"op":"subscribe","id":"s1","scope":"conversations","after":"0"}),
        json!({"op":"unsubscribe","id":"s1"}),
        json!({"op":"ping","t":1}),
    ] {
        assert!(client.is_valid(&op));
    }
    for op in [
        json!({"op":"subscribed","id":"s1","scope":"machines","head":"0"}),
        json!({"op":"resync_required","id":"s1","reason":"cursor_ahead","oldest_available_seq":"1"}),
        json!({"op":"lagged","id":"s1","cursor":"2"}),
        json!({"op":"unsubscribed","id":"s1"}),
        json!({"op":"pong","t":1}),
        json!({"op":"error","id":"s1","code":"bad_scope"}),
        json!({"op":"close","reason":"reauth"}),
    ] {
        assert!(server.is_valid(&op));
    }
    assert!(!client.is_valid(
        &json!({"op":"subscribe","id":"s","scope":"owner:internal:machines","after":"0"})
    ));
    assert!(!client.is_valid(&json!({"op":"subscribe","id":"s","scope":"machines","after":0})));
}
/// Вложение проходит все состояния хаба и попадает во ввод блоком `attachment`.
async fn attach_and_submit(f: &Fixture) {
    use axum::http::Method;
    sqlx::query("update machines set capabilities='{pilot_v1}' where id=$1")
        .bind(f.seed.machine_id)
        .execute(&f.pool)
        .await
        .unwrap();
    let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let mut ids = Vec::new();
    for (key, name) in [("attach", "a.txt"), ("attach_cancel", "b.txt")] {
        let created = f
            .client
            .send(
                Method::POST,
                "/api/v1/attachments",
                Some(key),
                Some(json!({"conversation_id": f.seed.conversation_id, "name": name, "size": 3, "sha256": sha})),
            )
            .await;
        assert_eq!(created.status.as_u16(), 201);
        ids.push(created.json::<Value>()["id"].as_str().unwrap().to_owned());
    }
    let uploaded = f
        .client
        .send_body(
            Method::PATCH,
            &format!("/api/v1/attachments/{}/content", ids[0]),
            &[("upload-offset", "0".into())],
            axum::body::Body::from("abc"),
        )
        .await;
    assert_eq!(uploaded.status.as_u16(), 204);
    let done = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/attachments/{}/complete", ids[0]),
            Some("attach_done"),
            None,
        )
        .await;
    assert_eq!(done.status.as_u16(), 200);
    let cancelled = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/attachments/{}", ids[1]),
            Some("attach_drop"),
            None,
        )
        .await;
    assert_eq!(cancelled.status.as_u16(), 204);
    let input = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/inputs", f.seed.conversation_id),
            Some("attach_input"),
            Some(json!({"content":[{"type":"text","text":"файл"}],"attachment_ids":[ids[0]]})),
        )
        .await;
    assert_eq!(input.status.as_u16(), 201);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn emitted_event_payloads_match_published_schema(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let refused = canterlot_proto::worker::v1::JournalRecords {
        journal_id: Uuid::now_v7().to_string(),
        records: vec![canterlot_proto::worker::v1::JournalRecord {
            seq: 1,
            body: Some(Default::default()),
        }],
    };
    let stopped = canterlot_hub::testkit::seed(&pool).await;
    let refused_status = canterlot_hub::grpc::Ingest::cj_records(
        &canterlot_hub::ingest::Ingest::new(pool.clone()),
        stopped.machine_id,
        &stopped.credential_digest(),
        refused,
    )
    .await
    .unwrap_err();
    assert_eq!(refused_status.message(), "unknown_record");
    f.input("schema").await;
    conversations::update_attributes(
        &pool,
        f.seed.owner_id,
        f.seed.conversation_id,
        "attrs",
        conversations::Attributes {
            title: Some("Сессия".into()),
            ..Default::default()
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    conversations::set_crunch(
        &pool,
        f.seed.owner_id,
        f.seed.conversation_id,
        "crunch",
        conversations::CrunchSetting {
            enabled: true,
            resume_text: None,
        },
        &Default::default(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    conversations::change_settings(
        &pool,
        f.seed.owner_id,
        f.seed.conversation_id,
        "settings",
        conversations::SettingsPatch {
            effort: Some("high".into()),
            ..Default::default()
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    canterlot_hub::groups::create_group(&pool, f.seed.owner_id, "group", "Группа", Uuid::now_v7())
        .await
        .unwrap();
    control::request_stop(
        &pool,
        f.seed.owner_id,
        f.seed.conversation_id,
        "stop",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    control::continue_queue(
        &pool,
        f.seed.owner_id,
        f.seed.conversation_id,
        "continue",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    activity::refresh(
        &pool,
        f.seed.conversation_id,
        chrono::Utc::now(),
        &thresholds(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let create = json!({"machine_id":f.seed.machine_id,"harness":"synthetic","workdir":"/tmp/w","model":"synthetic-small","effort":"high"});
    let response = f
        .client
        .send(
            axum::http::Method::POST,
            "/api/v1/conversations",
            Some("create"),
            Some(create),
        )
        .await;
    assert_eq!(response.status.as_u16(), 201);
    let now = chrono::Utc::now();
    canterlot_hub::scheduler::schedule_message(
        &pool,
        f.seed.owner_id,
        "schedule",
        canterlot_hub::scheduler::NewScheduled {
            conversation_id: f.seed.conversation_id,
            run_at: now + chrono::TimeDelta::seconds(30),
            content: vec![canterlot_hub::delivery::ContentBlock::Text {
                text: "test".into(),
            }],
            attachment_ids: vec![],
        },
        now,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    attach_and_submit(&f).await;
    machine_info::apply_report(
        &pool,
        f.seed.owner_id,
        f.seed.machine_id,
        &worker::report(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    machine_info::apply_metrics(
        &pool,
        f.seed.owner_id,
        f.seed.machine_id,
        &worker::metrics(now.timestamp_millis() as u64),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(f.seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    machine_info::project_availability(&pool, &f.state.registry, Uuid::now_v7())
        .await
        .unwrap();
    let mut run = worker::Run::new(&pool).await;
    let i = run.input(&pool).await;
    run.send(vec![
        worker::accepted(i.command_id, now.timestamp_millis()),
        Body::AdapterDiagnostic(AdapterDiagnostic {
            execution_id: run.execution.execution_id.to_string(),
            kind: "test".into(),
            field_names: vec!["alpha".into(), "beta".into()],
            ..Default::default()
        }),
    ])
    .await;
    for phase in [
        "awaiting_input",
        "generating",
        "running_tool",
        "awaiting_permission",
        "awaiting_answer",
        "context_maintenance",
        "unknown",
    ] {
        run.send(vec![Body::PhaseChanged(PhaseChanged {
            execution_id: run.execution.execution_id.to_string(),
            phase: phase.into(),
            tool_name: "Agent".into(),
            summary: "task".into(),
        })])
        .await;
    }
    let block = Uuid::now_v7();
    run.send(vec![
        worker::started(block, "text"),
        Body::BlockDelta(BlockDelta {
            block_id: block.to_string(),
            base_rev: 0,
            rev: 1,
            append: "x".into(),
        }),
    ])
    .await;
    run.send(vec![
        Body::BlockDelta(BlockDelta {
            block_id: block.to_string(),
            base_rev: 8,
            rev: 9,
            append: "y".into(),
        }),
        Body::BlockFinal(BlockFinal {
            block_id: block.to_string(),
            rev: 2,
            text: "x".into(),
            checksum: canterlot_hub::blocks::checksum("x"),
            ..Default::default()
        }),
    ])
    .await;
    run.send(vec![Body::BlockFinal(BlockFinal {
        block_id: block.to_string(),
        rev: 3,
        text: "z".into(),
        checksum: canterlot_hub::blocks::checksum("z"),
        ..Default::default()
    })])
    .await;
    let bad = Uuid::now_v7();
    run.send(vec![
        worker::started(bad, "tool_result"),
        Body::BlockFinal(BlockFinal {
            block_id: bad.to_string(),
            rev: 1,
            text: "x".into(),
            checksum: canterlot_hub::blocks::checksum("different"),
            ..Default::default()
        }),
    ])
    .await;
    let incomplete = Uuid::now_v7();
    run.send(vec![
        worker::started(incomplete, "text"),
        Body::BlockIncomplete(BlockIncomplete {
            block_id: incomplete.to_string(),
            rev: 1,
            reason: "process_exited".into(),
        }),
    ])
    .await;
    // Картинка агента: строка опубликована, ссылка приходит записью сопровождения.
    let viewed = Uuid::now_v7();
    let media = Uuid::now_v7();
    sqlx::query("insert into block_media(id,execution_id,conversation_id,block_id,mime,size_bytes,sha256) values($1,$2,$3,$4,'image/png',3,$5)")
        .bind(media).bind(run.execution.execution_id).bind(run.seed.conversation_id).bind(viewed).bind("a".repeat(64))
        .execute(&pool).await.unwrap();
    run.send(vec![
        worker::started(viewed, "tool_call"),
        Body::BlockMedia(BlockMedia {
            block_id: viewed.to_string(),
            media_id: media.to_string(),
            mime: "image/png".into(),
            size: 3,
            sha256: "a".repeat(64),
            width: 2,
            height: 0,
        }),
    ])
    .await;
    run.send(vec![
        worker::finished(&run, i.command_id, now.timestamp_millis()),
        Body::LimitObserved(worker::observation(104, now.timestamp_millis())),
    ])
    .await;
    control::request_stop(
        &pool,
        run.seed.owner_id,
        run.seed.conversation_id,
        "worker_interrupt",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let (interrupt, attempt): (Uuid, i32) = sqlx::query_as(
        "select id,dispatch_attempt from commands where conversation_id=$1 and kind='interrupt'",
    )
    .bind(run.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    run.send(vec![
        Body::InterruptStatus(InterruptStatus {
            command_id: interrupt.to_string(),
            dispatch_attempt: attempt as u32,
            state: InterruptState::Completed as i32,
        }),
        Body::LimitReached(LimitReached {
            execution_id: run.execution.execution_id.to_string(),
            provider: "claude".into(),
            window: LimitWindow::FiveHour as i32,
            input_command_id: i.command_id.to_string(),
            ..Default::default()
        }),
    ])
    .await;
    let mut run = worker::Run::new(&pool).await;
    sqlx::query("update conversations set crunch_enabled=true where id=$1")
        .bind(run.seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    run.send(vec![Body::LimitReached(LimitReached {
        execution_id: run.execution.execution_id.to_string(),
        provider: "claude".into(),
        window: LimitWindow::Weekly as i32,
        resets_at_ms: (now.timestamp_millis() + 3_600_000) as u64,
        ..Default::default()
    })])
    .await;
    sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) values($1,'wake',$2,$3,$4,$5)")
        .bind(Uuid::now_v7()).bind(run.seed.owner_id).bind(run.seed.conversation_id).bind(now)
        .bind(json!({"provider":"claude","window":"weekly","hold_key":"weekly:claude","verified":true}))
        .execute(&pool).await.unwrap();
    canterlot_hub::scheduler::Scheduler::new(
        pool.clone(),
        canterlot_hub::dispatcher::Dispatcher::channel().0,
        Default::default(),
    )
    .tick(now + chrono::TimeDelta::seconds(1))
    .await
    .unwrap();
    run.ingest
        .reconcile(
            run.seed.machine_id,
            &ReconcileReport {
                executions: vec![ExecutionObservation {
                    execution_id: Uuid::now_v7().to_string(),
                    generation: 1,
                    state: "running".into(),
                    ..Default::default()
                }],
            },
        )
        .await
        .unwrap();
    run.send(vec![Body::JournalClosed(JournalClosed {
        execution_id: run.execution.execution_id.to_string(),
        generation: run.execution.generation as u64,
        reason: "process_exited".into(),
    })])
    .await;
    let mut tx = pool.begin().await.unwrap();
    canterlot_hub::limits::record_observation_in_tx(
        &mut tx,
        f.seed.owner_id,
        &canterlot_hub::limits::Observation {
            provider: "claude".into(),
            window: canterlot_hub::limits::Window::Weekly,
            used_permille: 105,
            resets_at: None,
            observed_at: chrono::Utc::now(),
            source: canterlot_hub::limits::ObservationSource::Poll,
            data_confidence: canterlot_hub::limits::DataConfidence::Exact,
            machine_id: f.seed.machine_id,
            execution_id: None,
        },
        chrono::Utc::now(),
        &Default::default(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let rows: Vec<events::StoredEvent> =
        sqlx::query_as("select * from events order by scope,scope_seq")
            .fetch_all(&pool)
            .await
            .unwrap();
    let mut seen = BTreeSet::new();
    for e in events::project_for_client(rows) {
        seen.insert(e.kind.clone());
        let v = serde_json::to_value(&e).unwrap();
        assert!(
            validator(&e.kind).is_valid(&v),
            "invalid schema for {}: {:?}",
            e.kind,
            validator(&e.kind)
                .iter_errors(&v)
                .map(|error| error.instance_path().to_string())
                .collect::<Vec<_>>()
        );
        let mut bad = v;
        bad["scope_seq"] = json!(3);
        assert!(!validator(&e.kind).is_valid(&bad));
    }
    for k in [
        "input_queued",
        "input_state_changed",
        "conversation_created",
        "conversation_updated",
        "conversation_settings_changed",
        "crunch_changed",
        "crunch_held",
        "crunch_woken",
        "group_changed",
        "queue_paused",
        "derived_changed",
        "scheduled_changed",
        "machine_report_updated",
        "machine_availability_changed",
        "machine_metrics",
        "machine_journal_stops_changed",
        "execution_state_changed",
        "phase_changed",
        "block_started",
        "block_delta",
        "block_final",
        "block_incomplete",
        "block_revision_gap",
        "block_checksum_mismatch",
        "block_conflicting_final",
        "block_media_attached",
        "adapter_diagnostic",
        "run_finished",
        "limits_changed",
        "limit_reached",
        "queue_state_changed",
        "interrupt_state_changed",
        "state_divergence",
        "attachment_state_changed",
        "hidden",
    ] {
        assert!(seen.contains(k), "scenario did not emit {k}");
    }
}
