use crate::support::task7 as support;
use axum::http::Method;
use canterlot_hub::{
    activity,
    conversations::{self, Attributes},
    delivery::{self, ContentBlock, SubmitInput},
    events::{self, NewEvent, ReadAfter, Scope},
    snapshot,
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use support::*;
use tokio::sync::Barrier;
use uuid::Uuid;

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn snapshot_then_tail_loses_no_event_under_concurrent_writers(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let owner = f.seed.owner_id;
    let id = f.seed.conversation_id;
    let deadlocks: i64 = sqlx::query_scalar(
        "select deadlocks from pg_stat_database where datname=current_database()",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let barrier = Arc::new(Barrier::new(14));
    let mut writers = Vec::new();
    let mut readers = Vec::new();
    for w in 0..6 {
        let pool = f.pool.clone();
        let b = barrier.clone();
        writers.push(tokio::spawn(async move {
            b.wait().await;
            for n in 0..10 {
                let mut tx = pool.begin().await.unwrap();
                delivery::submit_input_in_tx(
                    &mut tx,
                    &SubmitInput {
                        owner_id: owner,
                        conversation_id: id,
                        idempotency_key: format!("w{w}_{n}"),
                        content: vec![ContentBlock::Text {
                            text: "test".into(),
                        }],
                        received_at: Utc::now(),
                        correlation_id: Uuid::now_v7(),
                        origin: None,
                    },
                )
                .await
                .unwrap();
                if n % 3 == 0 {
                    tokio::time::sleep(Duration::from_millis(3)).await;
                }
                tx.commit().await.unwrap();
                conversations::update_attributes(
                    &pool,
                    owner,
                    id,
                    &format!("a{w}_{n}"),
                    Attributes {
                        title: Some(format!("{w}:{n}")),
                        ..Default::default()
                    },
                    Uuid::now_v7(),
                )
                .await
                .unwrap();
            }
        }));
    }
    for r in 0..8 {
        let pool = f.pool.clone();
        let b = barrier.clone();
        readers.push(tokio::spawn(async move {
            b.wait().await;
            let mut checkpoints = Vec::new();
            for n in 0..25 {
                let s = snapshot::conversation(&pool, owner, id, 200)
                    .await
                    .unwrap()
                    .unwrap();
                let boundary = s.cursor.parse::<i64>().unwrap();
                let v = serde_json::to_value(s.data).unwrap();
                let mut ids = BTreeSet::new();
                for i in v["queue"]["queued"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .chain(v["history"]["items"].as_array().unwrap())
                {
                    if i["id"].is_string() {
                        ids.insert(i["id"].as_str().unwrap().to_owned());
                    }
                }
                let mut random = [0u8; 1];
                getrandom::fill(&mut random).unwrap();
                tokio::time::sleep(Duration::from_millis(u64::from(random[0] % 6))).await;
                let ReadAfter::Events(tail) =
                    events::read_after(&pool, &Scope::Conversation { id }, boundary, 256)
                        .await
                        .unwrap()
                else {
                    panic!("unexpected resync")
                };
                checkpoints.push((boundary, ids, tail));
                if (n + r) % 3 == 0 {
                    tokio::task::yield_now().await;
                }
            }
            checkpoints
        }));
    }
    for w in writers {
        w.await.unwrap();
    }
    let all: BTreeSet<String> =
        sqlx::query_scalar::<_, Uuid>("select id from inputs where conversation_id=$1")
            .bind(id)
            .fetch_all(&f.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|id| id.to_string())
            .collect();
    assert_eq!(all.len(), 60);
    for r in readers {
        let checkpoints = r.await.unwrap();
        assert_eq!(checkpoints.len(), 25);
        for (boundary, ids, mut tail) in checkpoints {
            let after = tail.last().map_or(boundary, |e| e.scope_seq);
            let ReadAfter::Events(rest) =
                events::read_after(&f.pool, &Scope::Conversation { id }, after, 256)
                    .await
                    .unwrap()
            else {
                panic!("unexpected resync")
            };
            tail.extend(rest);
            let mut tail_ids = BTreeSet::new();
            for (next, e) in (boundary + 1..).zip(tail) {
                assert_eq!(e.scope_seq, next);
                if e.kind == "input_queued" {
                    tail_ids.insert(e.payload["input_id"].as_str().unwrap().to_owned());
                }
            }
            assert!(ids.is_disjoint(&tail_ids));
            assert_eq!(ids.union(&tail_ids).cloned().collect::<BTreeSet<_>>(), all);
        }
    }
    let after: i64 = sqlx::query_scalar(
        "select deadlocks from pg_stat_database where datname=current_database()",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(after, deadlocks);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn naive_global_sequence_boundary_is_detected_as_lossy(pool: PgPool) {
    let f = Fixture::new(pool).await;
    sqlx::query("create table boundary_probe(gid bigserial primary key)")
        .execute(&f.pool)
        .await
        .unwrap();
    let mut a = f.pool.begin().await.unwrap();
    let gid1: i64 = sqlx::query_scalar("insert into boundary_probe default values returning gid")
        .fetch_one(&mut *a)
        .await
        .unwrap();
    let gid2: i64 = sqlx::query_scalar("insert into boundary_probe default values returning gid")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!((gid1, gid2), (1, 2));
    let seen: Vec<i64> = sqlx::query_scalar("select gid from boundary_probe order by gid")
        .fetch_all(&f.pool)
        .await
        .unwrap();
    let boundary = *seen.last().unwrap();
    a.commit().await.unwrap();
    let tail: Vec<i64> = sqlx::query_scalar("select gid from boundary_probe where gid>$1")
        .bind(boundary)
        .fetch_all(&f.pool)
        .await
        .unwrap();
    let all: Vec<i64> = sqlx::query_scalar("select gid from boundary_probe order by gid")
        .fetch_all(&f.pool)
        .await
        .unwrap();
    let lost: Vec<_> = all
        .into_iter()
        .filter(|g| !seen.contains(g) && !tail.contains(g))
        .collect();
    assert_eq!(lost, vec![1]);
    let scope = Scope::Conversation {
        id: f.seed.conversation_id,
    };
    let mut a = f.pool.begin().await.unwrap();
    let first = events::append(
        &mut a,
        NewEvent {
            scope,
            kind: "journal_closed",
            conversation_id: Some(f.seed.conversation_id),
            execution_id: None,
            payload: json!({}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    let p = f.pool.clone();
    let (ready, rx) = tokio::sync::oneshot::channel();
    let mut b = tokio::spawn(async move {
        let mut tx = p.begin().await.unwrap();
        ready.send(()).unwrap();
        let e = events::append(
            &mut tx,
            NewEvent {
                scope,
                kind: "journal_closed",
                conversation_id: None,
                execution_id: None,
                payload: json!({}),
                correlation_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        e.scope_seq
    });
    rx.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut b)
            .await
            .is_err()
    );
    let s = snapshot::conversation(&f.pool, f.seed.owner_id, f.seed.conversation_id, 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.cursor, "0");
    a.commit().await.unwrap();
    assert_eq!(b.await.unwrap(), first.scope_seq + 1);
    let ReadAfter::Events(tail) =
        events::read_after(&f.pool, &scope, s.cursor.parse().unwrap(), 256)
            .await
            .unwrap()
    else {
        panic!("unexpected resync")
    };
    assert_eq!(
        tail.iter().map(|e| e.scope_seq).collect::<Vec<_>>(),
        vec![1, 2]
    );
    let mut rollback = f.pool.begin().await.unwrap();
    let reserved = events::append(
        &mut rollback,
        NewEvent {
            scope,
            kind: "journal_closed",
            conversation_id: None,
            execution_id: None,
            payload: json!({}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap()
    .scope_seq;
    rollback.rollback().await.unwrap();
    assert_eq!(
        emit(&f.pool, scope, "journal_closed", json!({}))
            .await
            .scope_seq,
        reserved
    );
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn snapshot_is_one_repeatable_read_transaction(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let mut tx = snapshot::begin(&f.pool).await.unwrap();
    let iso: String = sqlx::query_scalar("show transaction_isolation")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let ro: String = sqlx::query_scalar("show transaction_read_only")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(iso, "repeatable read");
    assert_eq!(ro, "on");
    let scope = Scope::Conversation {
        id: f.seed.conversation_id,
    };
    assert_eq!(events::boundary(&mut tx, &scope).await.unwrap(), 0);
    f.input("outside").await;
    let count: i64 = sqlx::query_scalar("select count(*) from inputs")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert_eq!(events::boundary(&mut tx, &scope).await.unwrap(), 0);
    tx.commit().await.unwrap();
    let s = snapshot::conversation(&f.pool, f.seed.owner_id, f.seed.conversation_id, 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.data.queue.queued.len(), 1);
    assert!(s.cursor.parse::<i64>().unwrap() > 0);
}
async fn range(f: &Fixture, after: i64, limit: i64) -> Value {
    f.get(&format!(
        "/api/v1/events?scope=conversation:{}&after={after}&limit={limit}",
        f.seed.conversation_id
    ))
    .await
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn stale_cursor_gets_resync_not_reconnect_loop(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for _ in 0..3 {
        f.emit("queue_state_changed", json!({})).await;
    }
    events::purge_before(
        &f.pool,
        &Scope::Conversation {
            id: f.seed.conversation_id,
        },
        3,
    )
    .await
    .unwrap();
    let r = range(&f, 0, 256).await;
    assert_eq!(
        r["resync_required"],
        json!({"reason":"cursor_too_old","oldest_available_seq":"3"})
    );
    let s = f.get(&f.path("/snapshot")).await;
    assert!(
        range(&f, seq(&s["cursor"]), 256).await["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cursor_ahead_of_head_requires_resync(pool: PgPool) {
    let f = Fixture::new(pool).await;
    assert_eq!(
        range(&f, 99, 256).await["resync_required"]["reason"],
        "cursor_ahead"
    );
    f.emit("queue_state_changed", empty()).await;
    assert_eq!(
        range(&f, 2, 256).await["resync_required"],
        json!({"reason":"cursor_ahead","oldest_available_seq":"1"})
    );
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn events_range_endpoint_returns_gap_range_or_resync(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for _ in 0..4 {
        f.emit("queue_state_changed", empty()).await;
    }
    let r = range(&f, 1, 2).await;
    assert_eq!(r["head"], "4");
    assert_eq!(r["events"][0]["scope_seq"], "2");
    assert_eq!(r["events"][1]["scope_seq"], "3");
    for q in [
        "after=-1",
        "after=x",
        "after=0&limit=257",
        "after=0&limit=0",
        "after=0&scope=owner:bad:conversations",
    ] {
        assert_eq!(
            f.client
                .send(
                    Method::GET,
                    &format!("/api/v1/events?scope=conversations&{q}"),
                    None,
                    None
                )
                .await
                .status
                .as_u16(),
            400
        );
    }
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn range_endpoint_never_skips_a_seq(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.emit("queue_state_changed", empty()).await;
    let first = f.emit("journal_closed", json!({"secret":"internal"})).await;
    f.emit("execution_fact_rejected", json!({"secret":"internal"}))
        .await;
    f.emit("queue_state_changed", empty()).await;
    let r = range(&f, 0, 256).await;
    let es = r["events"].as_array().unwrap();
    assert_eq!(es.len(), 3);
    let mut c = 0;
    for e in es {
        assert_eq!(seq(&e["scope_seq_from"]), c + 1);
        c = seq(&e["scope_seq"]);
    }
    assert_eq!(c, 4);
    assert_eq!(es[1]["kind"], "hidden");
    assert_eq!(es[1]["payload"], empty());
    assert!(es[1]["conversation_id"].is_null());
    assert!(es[1]["execution_id"].is_null());
    assert_eq!(es[1]["id"], first.id.to_string());
    assert_eq!(es[1]["correlation_id"], first.correlation_id.to_string());
    assert_eq!(es[1]["scope_seq_from"], "2");
    assert_eq!(es[1]["scope_seq"], "3");
}
fn fold(item: &mut Value, e: &Value) {
    let p = &e["payload"];
    match e["kind"].as_str().unwrap() {
        "conversation_updated" => {
            for (k, v) in p.as_object().unwrap() {
                if k != "counts" {
                    item[k] = v.clone();
                }
            }
        }
        "derived_changed" => {
            for k in [
                "activity",
                "unread_count",
                "queue",
                "last_activity_at",
                "crunch",
            ] {
                item[k] = p[k].clone();
            }
            for k in ["model", "effort", "permission_mode"] {
                item[k] = p["settings"][k].clone();
            }
        }
        _ => {}
    }
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn list_snapshot_then_tail_folds_to_the_database_state(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let owner = f.seed.owner_id;
    let id = f.seed.conversation_id;
    let barrier = Arc::new(Barrier::new(5));
    let mut jobs = Vec::new();
    for w in 0..4 {
        let pool = f.pool.clone();
        let b = barrier.clone();
        let machine = f.seed.machine_id;
        jobs.push(tokio::spawn(async move {
            b.wait().await;
            for n in 0..12 {
                match w {
                    0 => {
                        conversations::update_attributes(
                            &pool,
                            owner,
                            id,
                            &format!("attr{n}"),
                            Attributes {
                                title: Some(format!("title{n}")),
                                pinned: Some(n % 2 == 0),
                                ..Default::default()
                            },
                            Uuid::now_v7(),
                        )
                        .await
                        .unwrap();
                    }
                    1 => {
                        conversations::set_crunch(
                            &pool,
                            owner,
                            id,
                            &format!("crunch{n}"),
                            conversations::CrunchSetting {
                                enabled: n % 2 == 0,
                                resume_text: None,
                            },
                            &Default::default(),
                            Uuid::now_v7(),
                        )
                        .await
                        .unwrap();
                    }
                    2 => {
                        let submitted = input(&pool, owner, id, &format!("input{n}")).await;
                        conversations::change_settings(
                            &pool,
                            owner,
                            id,
                            &format!("settings{n}"),
                            conversations::SettingsPatch {
                                model: Some(if n % 2 == 0 {"synthetic-small"} else {"synthetic-large"}.into()),
                                effort: Some(if n % 2 == 0 { "high" } else { "low" }.into()),
                                permission_mode: None,
                                instruction_profile: None,
                            },
                            Uuid::now_v7(),
                        )
                        .await
                        .unwrap();
                        let mut tx=pool.begin().await.unwrap();
                        sqlx::query("update commands set payload=payload||jsonb_build_object('settings',(select pending_settings from conversations where id=$2)) where id=$1")
                            .bind(submitted.command_id).bind(id).execute(&mut *tx).await.unwrap();
                        conversations::promote_settings_in_tx(&mut tx,submitted.command_id,Uuid::now_v7()).await.unwrap();
                        tx.commit().await.unwrap();
                    }
                    _ => {
                        let mut tx=pool.begin().await.unwrap();
                        let availability=if n%2==0 {"available"} else {"unavailable"};
                        sqlx::query("update machines set availability=$2,availability_at=clock_timestamp() where id=$1").bind(machine).bind(availability).execute(&mut *tx).await.unwrap();
                        events::append(&mut tx,NewEvent{scope:Scope::Machines{owner_id:owner},kind:"machine_availability_changed",conversation_id:None,execution_id:None,payload:json!({"machine_id":machine,"availability":availability}),correlation_id:Uuid::now_v7()}).await.unwrap();
                        tx.commit().await.unwrap();
                    }
                }
                activity::refresh(&pool, id, Utc::now(), &thresholds(), Uuid::now_v7())
                    .await
                    .unwrap();
            }
        }));
    }
    let mut readers = Vec::new();
    for _ in 0..8 {
        let pool = f.pool.clone();
        readers.push(tokio::spawn(async move {
            let mut snapshots = Vec::new();
            for _ in 0..25 {
                snapshots.push(
                    serde_json::to_value(snapshot::conversations(&pool, owner).await.unwrap())
                        .unwrap(),
                );
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            snapshots
        }));
    }
    barrier.wait().await;
    for j in jobs {
        j.await.unwrap();
    }
    activity::refresh(&f.pool, id, Utc::now(), &thresholds(), Uuid::now_v7())
        .await
        .unwrap();
    let final_page = f.get("/api/v1/conversations").await;
    let expected = &final_page["items"][0];
    let mut snapshots = Vec::new();
    for reader in readers {
        let points = reader.await.unwrap();
        assert_eq!(points.len(), 25);
        snapshots.extend(points);
    }
    let kinds: Vec<String> = sqlx::query_scalar("select distinct kind from events")
        .fetch_all(&f.pool)
        .await
        .unwrap();
    for kind in [
        "conversation_settings_changed",
        "crunch_changed",
        "input_queued",
        "conversation_updated",
        "machine_availability_changed",
        "derived_changed",
    ] {
        assert!(kinds.iter().any(|k| k == kind));
    }
    for s in snapshots {
        assert!(s["data"].get("machines").is_none());
        let mut item = s["data"]["items"][0].clone();
        let mut counts = s["data"]["counts"].clone();
        let mut cursor = seq(&s["cursor"]);
        loop {
            let p = f
                .get(&format!(
                    "/api/v1/events?scope=conversations&after={cursor}&limit=256"
                ))
                .await;
            let es = p["events"].as_array().unwrap();
            for e in es {
                assert_eq!(seq(&e["scope_seq_from"]), cursor + 1);
                cursor = seq(&e["scope_seq"]);
                fold(&mut item, e);
                if let Some(c) = e["payload"].get("counts") {
                    counts = c.clone();
                }
            }
            if cursor == seq(&p["head"]) {
                break;
            }
            assert!(!es.is_empty());
        }
        assert_eq!(&item, expected);
        assert_eq!(counts, final_page["counts"]);
    }
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn snapshot_cursor_and_seq_fields_are_decimal_strings_with_wire_aliases(pool: PgPool) {
    let f = Fixture::new(pool).await;
    f.input("seq").await;
    for (p, scope) in [
        (
            "/api/v1/snapshot/conversations".into(),
            "conversations".into(),
        ),
        ("/api/v1/snapshot/machines".into(), "machines".into()),
        (
            f.path("/snapshot"),
            format!("conversation:{}", f.seed.conversation_id),
        ),
    ] {
        let s = f.get(&p).await;
        assert_eq!(s["scope"], scope);
        assert!(
            s["cursor"]
                .as_str()
                .unwrap()
                .bytes()
                .all(|b| b.is_ascii_digit())
        );
        assert!(s["data"].get("limits").is_none() || scope == "conversations");
    }
    let r = range(&f, 0, 256).await;
    for e in r["events"].as_array().unwrap() {
        assert_eq!(
            e["scope"],
            format!("conversation:{}", f.seed.conversation_id)
        );
        for k in ["scope_seq", "scope_seq_from"] {
            assert!(e[k].is_string());
        }
    }
    let s = f.get(&f.path("/snapshot")).await;
    assert!(s["data"]["queue"]["queued"][0]["created_seq"].is_string());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn list_snapshot_is_bounded_and_includes_archived_and_legacy_rows(pool: PgPool) {
    let f = Fixture::new(pool).await;
    sqlx::query("insert into conversations(id,owner_id,machine_id,harness,workdir,archived_at) select gen_random_uuid(),$1,$2,'synthetic','/tmp/w',case when n%2=0 then now() end from generate_series(1,500) n").bind(f.seed.owner_id).bind(f.seed.machine_id).execute(&f.pool).await.unwrap();
    let s = f.get("/api/v1/snapshot/conversations").await;
    assert_eq!(s["data"]["items"].as_array().unwrap().len(), 500);
    assert_eq!(s["data"]["truncated"], true);
    assert_eq!(s["data"]["counts"]["all"], 251);
    assert_eq!(s["data"]["counts"]["archived"], 250);
    assert!(
        s["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["archived"] == true)
    );
}
