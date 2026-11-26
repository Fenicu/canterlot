//! Журнал событий: курсор чтения, время фиксации, соответствие публичной схеме,
//! счётчики списка разговоров и скрытие содержимого в отладочном выводе.

use crate::support::task5 as support;

use canterlot_hub::{
    conversations,
    delivery::{
        control,
        submit::{ContentBlock, SubmitInput},
    },
    events::{self, NewEvent, ReadAfter, Scope},
    groups, idempotency,
    media::MediaError,
    testkit::{self, Seed},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use support::*;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn request(seed: &Seed, text: &str) -> SubmitInput {
    SubmitInput {
        owner_id: seed.owner_id,
        conversation_id: seed.conversation_id,
        idempotency_key: Uuid::now_v7().to_string(),
        content: vec![ContentBlock::Text { text: text.into() }],
        received_at: Utc::now(),
        correlation_id: Uuid::now_v7(),
        origin: None,
    }
}

async fn blocked_by(pool: &PgPool, pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar("select exists(select 1 from pg_stat_activity where datname=current_database() and $1=any(pg_blocking_pids(pid)))")
                .bind(pid).fetch_one(pool).await.unwrap();
            if blocked { break; }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }).await.unwrap();
}

#[tokio::test]
async fn cursor_ahead_of_head_requires_resync() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let scope = Scope::Conversation { id: Uuid::now_v7() };
    assert!(matches!(
        events::read_after(&pool, &scope, 1, 10).await?,
        ReadAfter::ResyncRequired {
            oldest_available_seq: 1
        }
    ));
    let mut tx = pool.begin().await?;
    events::append(
        &mut tx,
        NewEvent {
            scope,
            kind: "test",
            conversation_id: None,
            execution_id: None,
            payload: json!({}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await?;
    tx.commit().await?;
    assert!(matches!(
        events::read_after(&pool, &scope, 2, 10).await?,
        ReadAfter::ResyncRequired {
            oldest_available_seq: 1
        }
    ));
    events::purge_before(&pool, &scope, 2).await?;
    assert!(matches!(
        events::read_after(&pool, &scope, 2, 10).await?,
        ReadAfter::ResyncRequired {
            oldest_available_seq: 2
        }
    ));
    Ok(())
}
#[tokio::test]
async fn committed_at_defaults_use_wall_clock() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let defaults: Vec<(String, String)> = sqlx::query_as("select table_name, column_default from information_schema.columns where table_schema = 'public' and table_name in ('inputs', 'events') and column_name = 'committed_at' order by table_name").fetch_all(&pool).await?;
    assert_eq!(
        defaults,
        [
            ("events".into(), "clock_timestamp()".into()),
            ("inputs".into(), "clock_timestamp()".into())
        ]
    );
    Ok(())
}
#[tokio::test]
async fn debug_redacts_content_and_event_payloads() -> TestResult {
    let pool = canterlot_hub::testkit::db().await;
    let seed = testkit::seed(&pool).await;
    let marker = "PRIVATE_MARKER_TEXT_123";
    let req = request(&seed, marker);
    for debug in [format!("{:?}", req.content[0]), format!("{req:?}")] {
        assert!(!debug.contains(marker));
        assert!(debug.contains(&marker.len().to_string()));
    }
    let mut tx = pool.begin().await?;
    let event = NewEvent {
        scope: Scope::Conversation {
            id: seed.conversation_id,
        },
        kind: "input_queued",
        conversation_id: Some(seed.conversation_id),
        execution_id: None,
        payload: json!({"content": req.content, "nested": {"text": marker}, "append": marker}),
        correlation_id: Uuid::now_v7(),
    };
    assert!(!format!("{event:?}").contains(marker));
    let stored = events::append(&mut tx, event).await?;
    assert!(!format!("{stored:?}").contains(marker));
    tx.commit().await?;
    let ReadAfter::Events(events) = events::read_after(
        &pool,
        &Scope::Conversation {
            id: seed.conversation_id,
        },
        0,
        10,
    )
    .await?
    else {
        panic!()
    };
    assert!(!format!("{:?}", events[0]).contains(marker));
    Ok(())
}
#[tokio::test]
async fn group_delete_and_queue_resume_conform_to_public_schema() {
    let pool = canterlot_hub::testkit::db().await;
    let s = testkit::seed(&pool).await;
    let g = groups::create_group(&pool, s.owner_id, "group", "g", None, Uuid::now_v7())
        .await
        .unwrap();
    groups::delete_group(&pool, s.owner_id, g.id, "delete-group", Uuid::now_v7())
        .await
        .unwrap();
    control::request_stop(&pool, s.owner_id, s.conversation_id, "stop", Uuid::now_v7())
        .await
        .unwrap();
    control::continue_queue(
        &pool,
        s.owner_id,
        s.conversation_id,
        "continue",
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../../../../docs/api/events.schema.json")).unwrap();
    let rows: Vec<events::StoredEvent> = sqlx::query_as(
        "select * from events where kind in ('group_changed','queue_resumed') order by scope_seq",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for e in rows {
        assert!(events::is_client_visible(&e.kind), "{} is hidden", e.kind);
        let mut validator = schema.clone();
        validator["$ref"] = json!(format!("#/$defs/{}", e.kind));
        validator.as_object_mut().unwrap().remove("oneOf");
        let validator = jsonschema::validator_for(&validator).unwrap();
        let value = serde_json::to_value(events::project_for_client(vec![e]).remove(0)).unwrap();
        assert!(validator.is_valid(&value));
        let mut server = schema.clone();
        server["$ref"] = json!("#/$defs/ServerOp");
        server.as_object_mut().unwrap().remove("oneOf");
        let server = jsonschema::validator_for(&server).unwrap();
        assert!(server.is_valid(&json!({"op":"event","id":"s","event":value})));
    }
}
#[tokio::test]
async fn auto_title_counts_follow_prior_list_commit() {
    let pool = canterlot_hub::testkit::db().await;
    let s = testkit::seed(&pool).await;
    let other = testkit::seed_conversation(&pool, s.owner_id, s.machine_id).await;
    sqlx::query("update conversations set title_source='auto' where id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut prior = pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("select pg_backend_pid()")
        .fetch_one(&mut *prior)
        .await
        .unwrap();
    sqlx::query("update conversations set archived_at=clock_timestamp() where id=$1")
        .bind(other)
        .execute(&mut *prior)
        .await
        .unwrap();
    events::append(
        &mut prior,
        NewEvent {
            scope: Scope::Conversations {
                owner_id: s.owner_id,
            },
            kind: "conversation_updated",
            conversation_id: Some(other),
            execution_id: None,
            payload: json!({}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    let p = pool.clone();
    let writer = tokio::spawn(async move {
        conversations::submit_message(
            &p,
            conversations::SubmitMessage {
                owner_id: s.owner_id,
                conversation_id: s.conversation_id,
                key: "auto-title".into(),
                content: content(),
                attachment_ids: vec![],
                retry_of: None,
                received_at: Utc::now(),
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    });
    blocked_by(&pool, pid).await;
    prior.commit().await.unwrap();
    writer.await.unwrap();
    let counts:Value=sqlx::query_scalar("select payload->'counts' from events where scope like 'owner:%:conversations' order by scope_seq desc limit 1").fetch_one(&pool).await.unwrap();
    assert_eq!(counts["archived"], 1);
}
#[tokio::test]
async fn sensitive_debug_redacts_stored_response_and_database_detail() {
    let pool = canterlot_hub::testkit::db().await;
    let marker = "private_regression_marker";
    let replay = idempotency::Begin::Replay(json!({"content":marker}));
    assert!(!format!("{replay:?}").contains(marker));
    let error=sqlx::raw_sql("do $$ begin raise exception using errcode='23514',message='rejected',detail='private_regression_marker'; end $$").execute(&pool).await.unwrap_err();
    let debug = format!("{:?}", MediaError::Db(error));
    assert!(!debug.contains(marker));
    assert!(debug.contains("23514"));
}
