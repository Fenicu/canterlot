#![cfg(all(feature = "testkit", feature = "fault-injection"))]
#[path = "support/task7.rs"]
mod support;

use canterlot_hub::{
    config::HttpConfig,
    events::{self, Scope, WireEvent},
    http,
    subscriptions::{Merger, Outgoing, authorize, resolve_scope},
    testkit::{self, TEST_ORIGIN, TEST_OWNER_LOGIN, TestWs},
};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::{Duration, Instant};
use support::{Fixture, seq};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(5);

struct Hub {
    f: Fixture,
    addr: std::net::SocketAddr,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Hub {
    async fn new(pool: PgPool, edit: impl FnOnce(&mut HttpConfig)) -> Self {
        let mut f = Fixture::new(pool).await;
        edit(std::sync::Arc::make_mut(&mut f.state.http));
        if let Some(path) = &f.state.http.proxy_secret_file {
            f.state.proxy_secret =
                Some(std::sync::Arc::new(http::load_proxy_secret(path).unwrap()));
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = f.state.clone();
        let stop = state.shutdown.clone();
        let task = tokio::spawn(http::serve(state, listener, stop.cancelled_owned()));
        Self { f, addr, task }
    }
    fn scope(&self) -> String {
        format!("conversation:{}", self.f.seed.conversation_id)
    }
    async fn socket(&self) -> TestWs {
        TestWs::connect(self.addr, TEST_ORIGIN, Some(TEST_OWNER_LOGIN))
            .await
            .expect("upgrade")
    }
    async fn subscribe(&self, ws: &mut TestWs, id: &str, after: Option<i64>) -> Value {
        let mut op = json!({"op":"subscribe","id":id,"scope":self.scope()});
        if let Some(after) = after {
            op["after"] = json!(after.to_string());
        }
        ws.send(op).await;
        ws.next(WAIT).await.expect("subscribe response")
    }
    async fn stop(self) {
        self.f.state.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(3), self.task)
            .await
            .expect("HTTP shutdown")
            .unwrap()
            .unwrap();
        assert!(self.f.state.ws_tasks.is_empty());
    }
}

fn wire(n: i64, kind: &str, payload: Value) -> WireEvent {
    let now = Utc::now() + TimeDelta::milliseconds(n);
    events::project_for_client(vec![events::StoredEvent {
        id: Uuid::now_v7(),
        scope: "conversation:00000000-0000-0000-0000-000000000001".into(),
        scope_seq: n,
        kind: kind.into(),
        payload,
        committed_at: now,
        correlation_id: Uuid::now_v7(),
        conversation_id: None,
        execution_id: None,
    }])
    .pop()
    .unwrap()
}
fn delta(n: i64, block: Uuid, base: u64, text: &str) -> WireEvent {
    wire(
        n,
        "block_delta",
        json!({"block_id":block,"base_rev":base,"rev":base+1,"append":text}),
    )
}
fn event_value(e: Outgoing) -> Value {
    let v = serde_json::to_value(e.event).unwrap();
    assert_eq!(seq(&v["scope_seq_from"]), e.scope_seq_from);
    assert_eq!(
        v["committed_at_from"],
        e.committed_at_from
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );
    v
}
fn accept(frame: &Value, cursor: &mut i64) {
    assert_eq!(frame["op"], "event");
    assert_eq!(seq(&frame["event"]["scope_seq_from"]), *cursor + 1);
    *cursor = seq(&frame["event"]["scope_seq"]);
}

#[test]
fn adjacent_deltas_of_one_block_are_merged_with_range() {
    let t = Instant::now();
    let b = Uuid::now_v7();
    let mut m = Merger::new(Duration::from_millis(25), "s", 1047552);
    let first = delta(8, b, 3, "аб");
    let id = first.id;
    let correlation = first.correlation_id;
    assert!(m.push(first, t).is_empty());
    assert!(
        m.push(delta(9, b, 4, "cd"), t + Duration::from_millis(10))
            .is_empty()
    );
    let v = event_value(m.flush().pop().unwrap());
    assert_eq!(v["scope_seq_from"], "8");
    assert_eq!(v["scope_seq"], "9");
    assert_eq!(
        v["payload"],
        json!({"block_id":b,"base_rev":3,"rev":5,"append":"абcd"})
    );
    assert_eq!(v["id"], id.to_string());
    assert_eq!(v["correlation_id"], correlation.to_string());
    assert!(m.flush().is_empty());
    assert!(m.next_deadline().is_none());
    let mut m = Merger::new(Duration::from_millis(25), "s", 1047552);
    m.push(delta(1, b, 0, "a"), t);
    assert_eq!(m.push(delta(2, b, 8, "b"), t).len(), 1);
    assert_eq!(m.push(delta(4, b, 9, "c"), t).len(), 1);
}
#[test]
fn merged_delta_carries_committed_at_from_of_the_oldest_source() {
    let t = Instant::now();
    let b = Uuid::now_v7();
    let mut m = Merger::new(Duration::from_millis(25), "s", 1047552);
    let a = delta(1, b, 0, "a");
    let first = a.committed_at.clone();
    let z = delta(2, b, 1, "z");
    let last = z.committed_at.clone();
    m.push(a, t);
    m.push(z, t);
    let e = m.flush().pop().unwrap();
    assert_eq!(e.event.committed_at_from, first);
    assert_eq!(e.event.committed_at, last);
    event_value(e);
}
#[test]
fn interleaved_blocks_are_not_reordered() {
    let t = Instant::now();
    let a = Uuid::now_v7();
    let b = Uuid::now_v7();
    let mut m = Merger::new(Duration::from_millis(25), "s", 1047552);
    let mut out = vec![];
    for e in [
        delta(1, a, 0, "a"),
        delta(2, b, 0, "b"),
        delta(3, a, 1, "c"),
    ] {
        out.extend(m.push(e, t));
    }
    out.extend(m.flush());
    assert_eq!(
        out.iter().map(|e| e.scope_seq_from).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(
        out.iter()
            .map(|e| e.event.payload["block_id"].clone())
            .collect::<Vec<_>>(),
        [json!(a), json!(b), json!(a)]
    );
}
#[test]
fn placeholder_flushes_a_pending_delta_merge_and_adjacent_placeholders_merge() {
    let t = Instant::now();
    let mut m = Merger::new(Duration::from_millis(25), "s", 1047552);
    m.push(delta(1, Uuid::now_v7(), 0, "a"), t);
    let mut out = m.push(wire(2, "journal_closed", json!({"secret":"private"})), t);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].event.kind, "block_delta");
    assert!(
        m.push(wire(3, "local_state_reset", json!({"secret":"private"})), t)
            .is_empty()
    );
    out.extend(m.push(wire(4, "run_finished", json!({})), t));
    assert_eq!(out.len(), 3);
    assert_eq!(out[1].scope_seq_from, 2);
    assert_eq!(out[1].event.scope_seq, "3");
    assert_eq!(out[1].event.payload, json!({}));
    assert_eq!(out[2].event.kind, "run_finished");
    assert!(m.flush().is_empty());
}
#[test]
fn terminal_events_are_never_merged_or_dropped() {
    let t = Instant::now();
    let mut m = Merger::new(Duration::from_millis(25), "s", 1047552);
    let mut out = vec![];
    let kinds = [
        "block_delta",
        "block_final",
        "block_incomplete",
        "run_finished",
        "execution_state_changed",
        "adapter_diagnostic",
    ];
    out.extend(m.push(delta(1, Uuid::now_v7(), 0, "x"), t));
    for (i, k) in kinds.iter().enumerate().skip(1) {
        out.extend(m.push(wire(i as i64 + 1, k, json!({})), t));
    }
    out.extend(m.flush());
    assert_eq!(out.len(), kinds.len());
    for (e, k) in out.iter().zip(kinds) {
        assert_eq!(e.event.kind, k);
    }
}
#[test]
fn merge_window_is_bounded_to_25ms_and_64kib_serialized() {
    let t = Instant::now();
    let b = Uuid::now_v7();
    let mut m = Merger::new(Duration::from_secs(1), "s", 1047552);
    m.push(delta(1, b, 0, "a"), t);
    assert_eq!(m.next_deadline(), Some(t + Duration::from_millis(25)));
    assert!(m.flush_due(t + Duration::from_micros(24999)).is_empty());
    assert_eq!(m.flush_due(t + Duration::from_millis(25)).len(), 1);
    let mut m = Merger::new(Duration::from_millis(25), "s", 1047552);
    for n in 1..=3 {
        assert!(
            m.push(delta(n, b, n as u64 - 1, &"я".repeat(8192)), t)
                .is_empty()
        );
    }
    let full = m.push(delta(4, b, 3, &"я".repeat(8192)), t);
    assert_eq!(full.len(), 1);
    assert_eq!(
        full[0].event.payload["append"].as_str().unwrap().len(),
        49152
    );
    assert_eq!(full[0].event.scope_seq, "3");
    assert_eq!(m.flush()[0].event.scope_seq, "4");
    let mut m = Merger::new(Duration::from_millis(25), "s", 1047552);
    m.push(delta(1, b, 0, "a"), t);
    assert_eq!(
        m.push(delta(2, b, 1, "b"), t + Duration::from_millis(25))
            .len(),
        1
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ws_upgrade_with_foreign_origin_is_refused(pool: PgPool) {
    let h = Hub::new(pool, |_| {}).await;
    let e = TestWs::connect(h.addr, "https://foreign.test", Some(TEST_OWNER_LOGIN))
        .await
        .err()
        .expect("refusal");
    assert!(matches!(e,tokio_tungstenite::tungstenite::Error::Http(r) if r.status().as_u16()==403));
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ws_upgrade_from_untrusted_peer_is_unauthenticated(pool: PgPool) {
    let h = Hub::new(pool, |c| {
        c.trusted_proxies = vec!["192.0.2.1/32".parse().unwrap()]
    })
    .await;
    let e = TestWs::connect(h.addr, TEST_ORIGIN, Some(TEST_OWNER_LOGIN))
        .await
        .err()
        .expect("refusal");
    assert!(matches!(e,tokio_tungstenite::tungstenite::Error::Http(r) if r.status().as_u16()==401));
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn subscribe_from_cursor_replays_missed_events_in_order(pool: PgPool) {
    let h = Hub::new(pool, |_| {}).await;
    for _ in 0..8 {
        h.f.emit("run_finished", json!({})).await;
    }
    let mut w = h.socket().await;
    assert_eq!(h.subscribe(&mut w, "s", Some(3)).await["head"], "8");
    let mut cursor = 3;
    for _ in 4..=8 {
        accept(&w.next(WAIT).await.unwrap(), &mut cursor);
    }
    assert_eq!(cursor, 8);
    w.close().await;
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn subscription_to_foreign_scope_is_forbidden_and_indistinguishable_from_missing(
    pool: PgPool,
) {
    let h = Hub::new(pool, |_| {}).await;
    let foreign = testkit::seed(&h.f.pool).await;
    let mut conn = h.f.pool.acquire().await.unwrap();
    for scope in [
        Scope::Conversations {
            owner_id: foreign.owner_id,
        },
        Scope::Machines {
            owner_id: foreign.owner_id,
        },
    ] {
        assert!(
            !authorize(&mut conn, h.f.seed.owner_id, &scope)
                .await
                .unwrap()
        );
    }
    drop(conn);
    assert!(
        resolve_scope(
            h.f.seed.owner_id,
            &format!("owner:{}:machines", foreign.owner_id)
        )
        .is_err()
    );
    let mut w = h.socket().await;
    let mut answers = vec![];
    for id in [foreign.conversation_id, Uuid::now_v7()] {
        w.send(json!({"op":"subscribe","id":"s","scope":format!("conversation:{id}")}))
            .await;
        answers.push(w.next(WAIT).await.unwrap());
    }
    assert_eq!(answers[0], answers[1]);
    assert_eq!(
        answers[0],
        json!({"op":"error","id":"s","code":"forbidden"})
    );
    w.send(
        json!({"op":"subscribe","id":"raw","scope":format!("owner:{}:machines",h.f.seed.owner_id)}),
    )
    .await;
    assert_eq!(w.next(WAIT).await.unwrap()["code"], "bad_scope");
    w.close().await;
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn internal_kinds_advance_the_cursor_without_payload(pool: PgPool) {
    let h = Hub::new(pool, |_| {}).await;
    h.f.emit("run_finished", json!({})).await;
    h.f.emit("journal_closed", json!({"private":"internal text"}))
        .await;
    h.f.emit("run_finished", json!({})).await;
    let mut w = h.socket().await;
    h.subscribe(&mut w, "s", Some(0)).await;
    let mut cursor = 0;
    for n in 1..=3 {
        let v = w.next(WAIT).await.unwrap();
        accept(&v, &mut cursor);
        assert_eq!(cursor, n);
        if n == 2 {
            assert_eq!(v["event"]["kind"], "hidden");
            assert_eq!(v["event"]["payload"], json!({}));
            assert!(v["event"]["conversation_id"].is_null());
            assert!(v["event"]["execution_id"].is_null());
        }
    }
    let r =
        h.f.get(&format!("/api/v1/events?scope={}&after=0", h.scope()))
            .await;
    let mut cursor = 0;
    for v in r["events"].as_array().unwrap() {
        accept(&json!({"op":"event","event":v}), &mut cursor);
    }
    assert_eq!(cursor, 3);
    assert_eq!(r["events"][1]["kind"], "hidden");
    assert_eq!(r["events"][1]["payload"], json!({}));
    w.close().await;
    h.stop().await;
}

async fn backlog(h: &Hub, count: i64) {
    backlog_text(h, count, &"x".repeat(16384)).await;
}
async fn backlog_text(h: &Hub, count: i64, text: &str) {
    let b = Uuid::now_v7();
    let mut tx = h.f.pool.begin().await.unwrap();
    for n in 0..count {
        events::append(
            &mut tx,
            events::NewEvent {
                scope: Scope::Conversation {
                    id: h.f.seed.conversation_id,
                },
                kind: "block_delta",
                conversation_id: Some(h.f.seed.conversation_id),
                execution_id: None,
                payload: json!({"block_id":b,"base_rev":n,"rev":n+1,"append":text}),
                correlation_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn review_fix_small_queue_catches_up_across_reconnects(pool: PgPool) {
    for text in ["x".repeat(16384), "\"\\я🦄".repeat(2048)] {
        let h = Hub::new(pool.clone(), |c| c.ws.queue_bytes = 32768).await;
        assert!(h.f.state.http.validate().is_ok());
        assert_eq!(text.len(), 16384);
        backlog_text(&h, 40, &text).await;
        let mut cursor = 0;
        let mut received = String::new();
        let id = "\u{1}".repeat(128);
        for target in [13, 26, 40] {
            let before = cursor;
            let mut w = h.socket().await;
            assert_eq!(
                h.subscribe(&mut w, &id, Some(cursor)).await["op"],
                "subscribed"
            );
            while cursor < target {
                let Some(frame) = w.next(WAIT).await else {
                    break;
                };
                assert!(frame.to_string().len() <= 32768 - 1024);
                let from = cursor + 1;
                accept(&frame, &mut cursor);
                assert_eq!(frame["event"]["kind"], "block_delta");
                assert_eq!(frame["event"]["payload"]["base_rev"], from - 1);
                assert_eq!(frame["event"]["payload"]["rev"], cursor);
                let append = frame["event"]["payload"]["append"].as_str().unwrap();
                assert_eq!(append, text.repeat((cursor - from + 1) as usize));
                received.push_str(append);
            }
            let close = w.close_code;
            w.close().await;
            assert!(
                cursor > before,
                "reconnect must advance: cursor={cursor}, close={close:?}"
            );
            assert!(cursor >= target, "fast reader must catch up");
        }
        assert_eq!(cursor, 40);
        assert_eq!(received, text.repeat(40));
        h.stop().await;
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn review_fix_oversized_summary_commits_and_reaches_ws(pool: PgPool) {
    use canterlot_hub::{
        executions,
        ingest::{Ingest, IngestOutcome, JournalKind},
        model::ExecutionState,
    };
    use canterlot_proto::worker::v1::{
        BlockStarted, JournalOpened, JournalRecord, JournalRecords, RecordBody, record_body::Body,
    };
    let h = Hub::new(pool, |_| {}).await;
    let seed = &h.f.seed;
    sqlx::query("insert into machine_credentials(machine_id,token_sha256) values($1,$2)")
        .bind(seed.machine_id)
        .bind(seed.credential_digest())
        .execute(&h.f.pool)
        .await
        .unwrap();
    let run = executions::start(
        &h.f.pool,
        seed.owner_id,
        seed.conversation_id,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(&h.f.pool, run.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    let ingest = Ingest::new(h.f.pool.clone());
    let journal = Uuid::now_v7().to_string();
    let batch = |seq, body| JournalRecords {
        journal_id: journal.clone(),
        records: vec![JournalRecord {
            seq,
            body: Some(RecordBody { body: Some(body) }),
        }],
    };
    ingest
        .apply_batch(
            seed.machine_id,
            &seed.credential_digest(),
            JournalKind::Sj,
            &batch(
                1,
                Body::JournalOpened(JournalOpened {
                    execution_id: run.execution_id.to_string(),
                    generation: run.generation as u64,
                    supervisor_protocol_version: 1,
                }),
            ),
        )
        .await
        .unwrap();
    let scope = Scope::Conversation {
        id: seed.conversation_id,
    };
    let mut cursor = events::boundary(&mut h.f.pool.acquire().await.unwrap(), &scope)
        .await
        .unwrap();
    let id = Uuid::now_v7();
    let batch = batch(
        2,
        Body::BlockStarted(BlockStarted {
            block_id: id.to_string(),
            kind: "text".into(),
            agent: "main".into(),
            summary: "\u{1}".repeat(180000),
            ..Default::default()
        }),
    );
    canterlot_proto::limits::check_record_batch(&batch).unwrap();
    for _ in 0..2 {
        assert_eq!(
            ingest
                .apply_batch(
                    seed.machine_id,
                    &seed.credential_digest(),
                    JournalKind::Sj,
                    &batch
                )
                .await
                .unwrap(),
            IngestOutcome::Committed { up_to_seq: 2 }
        );
    }
    let mut w = h.socket().await;
    assert_eq!(
        h.subscribe(&mut w, "s", Some(cursor)).await["op"],
        "subscribed"
    );
    let frame = w
        .next(WAIT)
        .await
        .expect("committed fact must reach the client");
    accept(&frame, &mut cursor);
    assert_eq!(frame["event"]["kind"], "block_started");
    let expected = format!("{}…", "\u{1}".repeat(1023));
    assert_eq!(frame["event"]["payload"]["summary"], expected);
    let stored: String = sqlx::query_scalar("select summary from blocks where block_id=$1")
        .bind(id)
        .fetch_one(&h.f.pool)
        .await
        .unwrap();
    assert_eq!(stored, expected);
    w.close().await;
    h.stop().await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn review_fix_oversized_event_has_http_reference_and_resume_advances(pool: PgPool) {
    for budget in [1048576, 4096] {
        let h = Hub::new(pool.clone(), |c| c.ws.queue_bytes = budget).await;
        assert!(h.f.state.http.validate().is_ok());
        h.f.emit(
            "block_delta",
            json!({"block_id":Uuid::now_v7(),"base_rev":0,"rev":1,"append":"я🦄"}),
        )
        .await;
        let payload = json!({"kind":"synthetic","event_type":"\u{1}".repeat(180000)});
        let source = h.f.emit("adapter_diagnostic", payload.clone()).await;
        h.f.emit("run_finished", json!({})).await;
        let mut w = h.socket().await;
        let id = "\u{1}".repeat(128);
        assert_eq!(h.subscribe(&mut w, &id, Some(0)).await["op"], "subscribed");
        let mut cursor = 0;
        accept(&w.next(WAIT).await.unwrap(), &mut cursor);
        let frame = w.next(WAIT).await.expect("oversized placeholder");
        assert!(frame.to_string().len() <= budget - 1024);
        accept(&frame, &mut cursor);
        let event = &frame["event"];
        assert_eq!(event["kind"], "oversized");
        assert_eq!(event["id"], source.id.to_string());
        assert_eq!(event["scope_seq"], "2");
        assert_eq!(event["payload"]["kind"], "adapter_diagnostic");
        let href = event["payload"]["href"].as_str().unwrap();
        let fetched = h.f.get(href).await;
        assert_eq!(fetched["events"][0]["id"], source.id.to_string());
        assert_eq!(fetched["events"][0]["kind"], "adapter_diagnostic");
        assert_eq!(fetched["events"][0]["payload"], payload);
        let schema: Value =
            serde_json::from_str(include_str!("../../../docs/api/events.schema.json")).unwrap();
        assert!(jsonschema::validator_for(&schema).unwrap().is_valid(event));
        w.close().await;
        let mut w = h.socket().await;
        assert_eq!(
            h.subscribe(&mut w, &id, Some(cursor)).await["op"],
            "subscribed"
        );
        let frame = w.next(WAIT).await.unwrap();
        accept(&frame, &mut cursor);
        assert_eq!(frame["event"]["kind"], "run_finished");
        assert_eq!(cursor, 3);
        w.close().await;
        h.stop().await;
    }
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn catch_up_over_a_slow_link_completes_without_lagged(pool: PgPool) {
    let h = Hub::new(pool, |_| {}).await;
    backlog(&h, 2000).await;
    let mut w = h.socket().await;
    h.subscribe(&mut w, "s", Some(0)).await;
    let mut cursor = 0;
    let mut bytes = 0;
    while cursor < 2000 {
        let v = w.next(WAIT).await.unwrap();
        accept(&v, &mut cursor);
        let text = v["event"]["payload"]["append"].as_str().unwrap();
        bytes += text.len();
        tokio::time::sleep(Duration::from_secs_f64(text.len() as f64 / 262144.0)).await;
    }
    assert_eq!(bytes, 2000 * 16384);
    assert!(w.next(Duration::from_millis(50)).await.is_none());
    w.close().await;
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn slow_client_subscription_is_cut_with_cursor_while_other_clients_keep_streaming(
    pool: PgPool,
) {
    use canterlot_hub::fault_injection::Point;
    const EVENTS: i64 = 64;
    let h = Hub::new(pool, |c| {
        c.ws.stall_timeout = Duration::from_millis(500);
        c.ws.queue_messages = 4;
        c.ws.queue_bytes = 65536;
    })
    .await;
    let blocked = h.f.state.ws_faults.pause_next(Point::WsBeforeDataSend);
    let stalled = h.f.state.ws_faults.pause_next(Point::WsStalled);
    let mut slow = h.socket().await;
    h.subscribe(&mut slow, "slow", Some(0)).await;
    h.subscribe(&mut slow, "slow2", Some(0)).await;
    let mut fast = h.socket().await;
    h.subscribe(&mut fast, "fast", Some(0)).await;
    let mut tx = h.f.pool.begin().await.unwrap();
    for _ in 0..EVENTS {
        events::append(&mut tx, events::NewEvent {
            scope: Scope::Conversation { id: h.f.seed.conversation_id },
            kind: "block_delta",
            conversation_id: Some(h.f.seed.conversation_id),
            execution_id: None,
            payload: json!({"block_id":Uuid::now_v7(),"base_rev":0,"rev":1,"append":"x".repeat(16384)}),
            correlation_id: Uuid::now_v7(),
        }).await.unwrap();
    }
    // Writer первого соединения остановлен; чтение начинается после подтверждённого Stalled.
    tx.commit().await.unwrap();
    let mut cursor = 0;
    let fast_read = async {
        while cursor < EVENTS {
            let v = fast.next(WAIT).await.expect("other client keeps streaming");
            accept(&v, &mut cursor);
        }
    };
    let slow_read = async {
        let resume_writer = tokio::time::timeout(WAIT, blocked).await.unwrap().unwrap();
        let resume_close = tokio::time::timeout(WAIT, stalled).await.unwrap().unwrap();
        resume_close.send(()).unwrap();
        resume_writer.send(()).unwrap();
        let mut cursors =
            std::collections::BTreeMap::from([("slow".to_owned(), 0), ("slow2".to_owned(), 0)]);
        let mut lagged = std::collections::BTreeSet::new();
        while let Some(v) = slow.next(WAIT).await {
            let id = v["id"].as_str().unwrap().to_owned();
            if v["op"] == "event" {
                assert!(!lagged.contains(&id));
                accept(&v, cursors.get_mut(&id).unwrap());
            } else {
                assert_eq!(v["op"], "lagged");
                assert!(lagged.insert(id.clone()));
                assert_eq!(seq(&v["cursor"]), cursors[&id]);
            }
        }
        assert_eq!(lagged.len(), 2);
        assert_eq!(slow.close_code, Some(1008));
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(fast_read, slow_read);
    })
    .await
    .expect("both clients complete");
    h.f.emit("run_finished", json!({})).await;
    accept(&fast.next(WAIT).await.unwrap(), &mut cursor);
    assert_eq!(cursor, EVENTS + 1);
    fast.close().await;
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn too_many_subscriptions_is_an_error_not_a_disconnect(pool: PgPool) {
    let h = Hub::new(pool, |_| {}).await;
    let mut w = h.socket().await;
    for n in 0..16 {
        assert_eq!(
            h.subscribe(&mut w, &format!("s{n}"), None).await["op"],
            "subscribed"
        );
    }
    assert_eq!(
        h.subscribe(&mut w, "excess", None).await["code"],
        "too_many_subscriptions"
    );
    w.send(json!({"op":"ping","t":7})).await;
    assert_eq!(w.next(WAIT).await.unwrap(), json!({"op":"pong","t":7}));
    w.close().await;
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn connection_older_than_max_age_is_closed_with_reauth(pool: PgPool) {
    let h = Hub::new(pool, |c| {
        c.ws.max_connection_age = Duration::from_millis(150)
    })
    .await;
    let mut w = h.socket().await;
    assert_eq!(
        w.next(WAIT).await.unwrap(),
        json!({"op":"close","reason":"reauth"})
    );
    assert!(w.next(WAIT).await.is_none());
    assert_eq!(w.close_code, Some(4401));
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unsubscribe_stops_delivery(pool: PgPool) {
    let h = Hub::new(pool, |_| {}).await;
    let mut w = h.socket().await;
    h.subscribe(&mut w, "s", None).await;
    backlog(&h, 30).await;
    w.send(json!({"op":"unsubscribe","id":"s"})).await;
    loop {
        if w.next(WAIT).await.unwrap()["op"] == "unsubscribed" {
            break;
        }
    }
    h.f.emit("run_finished", json!({})).await;
    assert!(w.next(Duration::from_millis(150)).await.is_none());
    assert_eq!(h.subscribe(&mut w, "s", None).await["op"], "subscribed");
    w.close().await;
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn events_wake_the_pump_without_waiting_for_the_poll_interval(pool: PgPool) {
    let h = Hub::new(pool, |c| c.ws.poll_interval = Duration::from_secs(5)).await;
    let mut w = h.socket().await;
    h.subscribe(&mut w, "s", None).await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    let mut tx = h.f.pool.begin().await.unwrap();
    events::append(
        &mut tx,
        events::NewEvent {
            scope: Scope::Conversation {
                id: h.f.seed.conversation_id,
            },
            kind: "run_finished",
            conversation_id: None,
            execution_id: None,
            payload: json!({}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    let t = Instant::now();
    tx.commit().await.unwrap();
    assert_eq!(
        w.next(Duration::from_millis(190)).await.unwrap()["op"],
        "event"
    );
    assert!(t.elapsed() < Duration::from_millis(200));
    w.close().await;
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stale_cursor_over_wss_gets_one_resync_not_a_loop(pool: PgPool) {
    let h = Hub::new(pool, |c| c.ws.poll_interval = Duration::from_millis(25)).await;
    for _ in 0..4 {
        h.f.emit("run_finished", json!({})).await;
    }
    events::purge_before(
        &h.f.pool,
        &Scope::Conversation {
            id: h.f.seed.conversation_id,
        },
        3,
    )
    .await
    .unwrap();
    let mut w = h.socket().await;
    let v = h.subscribe(&mut w, "s", Some(0)).await;
    assert_eq!(
        v,
        json!({"op":"resync_required","id":"s","reason":"cursor_too_old","oldest_available_seq":"3"})
    );
    h.f.emit("run_finished", json!({})).await;
    assert!(w.next(Duration::from_millis(130)).await.is_none());
    let v = h.subscribe(&mut w, "ahead", Some(99)).await;
    assert_eq!(
        v,
        json!({"op":"resync_required","id":"ahead","reason":"cursor_ahead","oldest_available_seq":"3"})
    );
    w.close().await;
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ws_reconnect_during_a_resync_converges_without_a_loop(pool: PgPool) {
    let h = Hub::new(pool.clone(), |_| {}).await;
    for _ in 0..5 {
        h.f.emit("run_finished", json!({})).await;
    }
    let scope = Scope::Conversation {
        id: h.f.seed.conversation_id,
    };
    events::purge_before(&pool, &scope, 4).await.unwrap();
    let mut w = h.socket().await;
    assert_eq!(
        h.subscribe(&mut w, "s", Some(0)).await["op"],
        "resync_required"
    );
    let id = h.f.seed.conversation_id;
    h.f.state.shutdown.cancel();
    assert!(w.next(WAIT).await.is_none());
    assert_eq!(w.close_code, Some(1001));
    h.stop().await;
    let mut h = Hub::new(pool, |_| {}).await;
    h.f.seed.conversation_id = id;
    let path = h.f.path("/snapshot");
    let snapshot = h.f.get(&path).await;
    let mut snapshots = 1;
    let boundary = seq(&snapshot["cursor"]);
    h.f.emit("run_finished", json!({})).await;
    let mut w = h.socket().await;
    assert_eq!(
        h.subscribe(&mut w, "s", Some(boundary)).await["op"],
        "subscribed"
    );
    let mut cursor = boundary;
    let tail = w.next(WAIT).await.unwrap();
    if tail["op"] == "resync_required" {
        snapshots += 1;
    }
    accept(&tail, &mut cursor);
    h.f.emit("run_finished", json!({})).await;
    accept(&w.next(WAIT).await.unwrap(), &mut cursor);
    assert_eq!(snapshots, 1);
    assert_eq!(cursor, boundary + 2);
    assert!(w.next(Duration::from_millis(100)).await.is_none());
    w.close().await;
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn hub_shutdown_closes_sockets_with_1001(pool: PgPool) {
    let h = Hub::new(pool, |_| {}).await;
    let mut w = h.socket().await;
    h.subscribe(&mut w, "s", None).await;
    h.f.state.shutdown.cancel();
    assert!(w.next(WAIT).await.is_none());
    assert_eq!(w.close_code, Some(1001));
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ws_frames_validate_against_client_and_server_op_schemas(pool: PgPool) {
    let h = Hub::new(pool, |_| {}).await;
    let schema: Value =
        serde_json::from_str(include_str!("../../../docs/api/events.schema.json")).unwrap();
    let validator = |name: &str| {
        let mut s = schema.clone();
        s["$ref"] = json!(format!("#/$defs/{name}"));
        s.as_object_mut().unwrap().remove("oneOf");
        jsonschema::validator_for(&s).unwrap()
    };
    let client = validator("ClientOp");
    let server = validator("ServerOp");
    let mut w = h.socket().await;
    for scope in ["conversations".to_owned(), "machines".to_owned(), h.scope()] {
        let op = json!({"op":"subscribe","id":"s","scope":scope,"after":"0"});
        assert!(client.is_valid(&op));
        w.send(op).await;
        let response = w.next(WAIT).await.unwrap();
        assert!(server.is_valid(&response));
        assert_eq!(response["scope"], scope);
        assert!(response["head"].is_string());
        w.send(json!({"op":"unsubscribe","id":"s"})).await;
        loop {
            let r = w.next(WAIT).await.unwrap();
            assert!(server.is_valid(&r));
            if r["op"] == "unsubscribed" {
                break;
            }
        }
    }
    h.subscribe(&mut w, "event", None).await;
    h.f.emit(
        "block_delta",
        json!({"block_id":Uuid::now_v7(),"base_rev":0,"rev":1,"append":"test"}),
    )
    .await;
    let v = w.next(WAIT).await.unwrap();
    assert!(server.is_valid(&v));
    assert_eq!(v["event"]["scope"], h.scope());
    for op in [
        json!({"op":"ping","t":1}),
        json!({"op":"unsubscribe","id":"event"}),
    ] {
        assert!(client.is_valid(&op));
        w.send(op).await;
        assert!(server.is_valid(&w.next(WAIT).await.unwrap()));
    }
    assert!(!client.is_valid(&json!({"op":"subscribe","id":"s","scope":"machines","after":1})));
    w.send(json!({"op":"subscribe","id":"s","scope":"machines","after":1}))
        .await;
    let v = w.next(WAIT).await.unwrap();
    assert_eq!(v["code"], "bad_message");
    assert!(server.is_valid(&v));
    w.close().await;
    h.stop().await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ws_upgrade_requires_identity_secret_and_subprotocol(pool: PgPool) {
    use tokio_tungstenite::{
        client_async,
        tungstenite::{Error, client::IntoClientRequest},
    };
    let secret = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(secret.path(), b"proxy-test-0123456789-abcdefghijklmnop").unwrap();
    let h = Hub::new(pool, |c| c.proxy_secret_file = Some(secret.path().into())).await;
    for (user, proxy, protocol, status) in [
        (
            None,
            Some("proxy-test-0123456789-abcdefghijklmnop"),
            Some("canterlot.v1"),
            401,
        ),
        (Some(TEST_OWNER_LOGIN), None, Some("canterlot.v1"), 401),
        (
            Some(TEST_OWNER_LOGIN),
            Some("wrong-secret"),
            Some("canterlot.v1"),
            401,
        ),
        (
            Some("foreign"),
            Some("proxy-test-0123456789-abcdefghijklmnop"),
            Some("canterlot.v1"),
            403,
        ),
        (
            Some(TEST_OWNER_LOGIN),
            Some("proxy-test-0123456789-abcdefghijklmnop"),
            None,
            400,
        ),
        (
            Some(TEST_OWNER_LOGIN),
            Some("proxy-test-0123456789-abcdefghijklmnop"),
            Some("foreign.v1"),
            400,
        ),
    ] {
        let mut r = format!("ws://{}/api/v1/ws", h.addr)
            .into_client_request()
            .unwrap();
        r.headers_mut()
            .insert("origin", TEST_ORIGIN.parse().unwrap());
        r.headers_mut()
            .insert("x-forwarded-for", "127.0.0.1".parse().unwrap());
        if let Some(v) = user {
            r.headers_mut().insert("remote-user", v.parse().unwrap());
        }
        if let Some(v) = proxy {
            r.headers_mut()
                .insert("x-canterlot-proxy", v.parse().unwrap());
        }
        if let Some(v) = protocol {
            r.headers_mut()
                .insert("sec-websocket-protocol", v.parse().unwrap());
        }
        let tcp = tokio::net::TcpStream::connect(h.addr).await.unwrap();
        let result = client_async(r, tcp).await;
        assert!(matches!(result,Err(Error::Http(r)) if r.status().as_u16()==status));
    }
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn ws_upgrade_keeps_connection_slot_and_survives_body_deadline(pool: PgPool) {
    let h = Hub::new(pool, |c| {
        c.body_read_timeout = Duration::from_millis(30);
        c.max_connections = 1;
    })
    .await;
    let mut w = h.socket().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    w.send(json!({"op":"ping","t":1})).await;
    assert_eq!(w.next(WAIT).await.unwrap(), json!({"op":"pong","t":1}));
    assert!(
        TestWs::connect(h.addr, TEST_ORIGIN, Some(TEST_OWNER_LOGIN))
            .await
            .is_err()
    );
    w.close().await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    h.socket().await.close().await;
    h.stop().await;
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn bad_messages_never_create_a_subscription(pool: PgPool) {
    let h = Hub::new(pool, |_| {}).await;
    let mut w = h.socket().await;
    for after in [
        json!(null),
        json!(-1),
        json!(1),
        json!("-1"),
        json!("9223372036854775808"),
    ] {
        w.send(json!({"op":"subscribe","id":"s","scope":h.scope(),"after":after}))
            .await;
        assert_eq!(w.next(WAIT).await.unwrap()["code"], "bad_message");
    }
    w.send(json!({"op":"subscribe","id":"s","scope":h.scope(),"after":"0","extra":true}))
        .await;
    assert_eq!(w.next(WAIT).await.unwrap()["code"], "bad_message");
    assert_eq!(h.subscribe(&mut w, "s", Some(0)).await["op"], "subscribed");
    assert_eq!(
        h.subscribe(&mut w, "s", Some(0)).await["code"],
        "bad_message"
    );
    w.close().await;
    h.stop().await;
}
#[test]
fn websocket_resource_config_is_bounded() {
    let valid = testkit::test_http_config();
    assert!(valid.validate().is_ok());
    for (field, value) in [
        ("queue_messages", 1),
        ("queue_messages", 257),
        ("queue_bytes", 1024),
        ("queue_bytes", 4095),
        ("queue_bytes", 1048577),
        ("max_subscriptions", 0),
        ("max_subscriptions", 17),
    ] {
        let mut c = valid.clone();
        match field {
            "queue_messages" => c.ws.queue_messages = value,
            "queue_bytes" => c.ws.queue_bytes = value,
            _ => c.ws.max_subscriptions = value,
        }
        assert!(c.validate().is_err(), "{field}={value}");
    }
    for field in [
        "max_connection_age",
        "poll_interval",
        "stall_timeout",
        "delta_window",
    ] {
        let mut c = valid.clone();
        match field {
            "max_connection_age" => c.ws.max_connection_age = Duration::ZERO,
            "poll_interval" => c.ws.poll_interval = Duration::ZERO,
            "stall_timeout" => c.ws.stall_timeout = Duration::ZERO,
            _ => c.ws.delta_window = Duration::ZERO,
        }
        assert!(c.validate().is_err(), "{field}");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lost_bus_notice_is_recovered_by_polling_and_wildcard_wakes_all_scopes(pool: PgPool) {
    let h = Hub::new(pool.clone(), |c| {
        c.ws.poll_interval = Duration::from_millis(50)
    })
    .await;
    let mut w = h.socket().await;
    h.subscribe(&mut w, "s", None).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let bus = canterlot_hub::bus::EventBus::local();
    bus.drop_next_notice(&h.scope());
    h.f.emit("run_finished", json!({})).await;
    assert_eq!(
        w.next(Duration::from_millis(500)).await.unwrap()["event"]["scope_seq"],
        "1"
    );
    w.close().await;
    h.stop().await;

    let h = Hub::new(pool, |c| c.ws.poll_interval = Duration::from_secs(5)).await;
    let mut w = h.socket().await;
    h.subscribe(&mut w, "s", None).await;
    w.send(json!({"op":"subscribe","id":"machines","scope":"machines"}))
        .await;
    assert_eq!(w.next(WAIT).await.unwrap()["op"], "subscribed");
    tokio::time::sleep(Duration::from_millis(30)).await;
    bus.drop_next_notice(&h.scope());
    h.f.emit("run_finished", json!({})).await;
    let machines = Scope::Machines {
        owner_id: h.f.seed.owner_id,
    };
    bus.drop_next_notice(&machines.key());
    support::emit(
        &h.f.pool,
        machines,
        "journal_closed",
        json!({"private":"hidden"}),
    )
    .await;
    bus.publish("*", "*", 0);
    let mut scopes = std::collections::BTreeSet::new();
    for _ in 0..2 {
        let v = w.next(Duration::from_millis(200)).await.unwrap();
        scopes.insert(v["event"]["scope"].as_str().unwrap().to_owned());
    }
    assert_eq!(
        scopes,
        std::collections::BTreeSet::from([h.scope(), "machines".into()])
    );
    w.close().await;
    h.stop().await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn subscribe_without_after_starts_at_its_announced_head_under_a_commit_race(pool: PgPool) {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    let armed = Arc::new(AtomicBool::new(false));
    let acquisitions = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let request_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .before_acquire({
            let armed = armed.clone();
            let acquisitions = acquisitions.clone();
            let entered = entered.clone();
            let release = release.clone();
            move |_, _| {
                let armed = armed.clone();
                let acquisitions = acquisitions.clone();
                let entered = entered.clone();
                let release = release.clone();
                Box::pin(async move {
                    if armed.load(Ordering::SeqCst)
                        && acquisitions.fetch_add(1, Ordering::SeqCst) == 1
                    {
                        entered.notify_one();
                        release.notified().await;
                    }
                    Ok(true)
                })
            }
        })
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let h = Hub::new(request_pool.clone(), |_| {}).await;
    armed.store(true, Ordering::SeqCst);
    let mut w = h.socket().await;
    w.send(json!({"op":"subscribe","id":"s","scope":h.scope()}))
        .await;
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("second acquisition barrier");
    support::emit(
        &pool,
        Scope::Conversation {
            id: h.f.seed.conversation_id,
        },
        "run_finished",
        json!({}),
    )
    .await;
    release.notify_one();
    let ack = w.next(WAIT).await.unwrap();
    assert_eq!(ack["op"], "subscribed");
    let mut cursor = seq(&ack["head"]);
    let last = support::emit(
        &pool,
        Scope::Conversation {
            id: h.f.seed.conversation_id,
        },
        "run_finished",
        json!({}),
    )
    .await
    .scope_seq;
    while cursor < last {
        accept(&w.next(WAIT).await.unwrap(), &mut cursor);
    }
    assert_eq!(cursor, last);
    w.close().await;
    h.stop().await;
    request_pool.close().await;
}
