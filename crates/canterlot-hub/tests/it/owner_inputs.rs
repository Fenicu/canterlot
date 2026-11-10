use crate::support::task7 as support;
use axum::http::Method;
use canterlot_hub::testkit;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::*;
use uuid::Uuid;

async fn delivered(f: &Fixture, key: &str, feed: i64, text: &str, origin: Option<Value>) -> Uuid {
    let input = f.input(key).await;
    let content = json!([{ "type": "text", "text": text }]);
    sqlx::query("update inputs set feed_seq=$2,state='accepted',content=$3,origin=$4 where id=$1")
        .bind(input.input_id)
        .bind(feed)
        .bind(content)
        .bind(origin)
        .execute(&f.pool)
        .await
        .unwrap();
    input.input_id
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_messages_are_chronological_paged_and_skip_undelivered(pool: PgPool) {
    let f = Fixture::new(pool).await;
    delivered(&f, "m1", 10, "первое сообщение\nвторая строка", None).await;
    delivered(&f, "m2", 20, "второе", None).await;
    delivered(&f, "m3", 30, "третье", None).await;
    let queued = f.input("queued").await;

    let page = f.get(&f.path("/inputs?limit=2")).await;
    assert_eq!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["seq"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["20", "30"]
    );
    assert_eq!(page["next_before"], "20");
    assert_eq!(page["items"][0]["preview"], "второе");
    assert!(
        !page["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == queued.input_id.to_string())
    );

    let first = f.get(&f.path("/inputs?before=20&limit=2")).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert_eq!(first["items"][0]["seq"], "10");
    assert_eq!(
        first["items"][0]["preview"],
        "первое сообщение\nвторая строка"
    );
    assert!(first["next_before"].is_null());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_messages_truncate_the_preview_and_keep_origin_and_attachments(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let input = f.input("long").await;
    let text = "я".repeat(500);
    let content = json!([
        { "type": "text", "text": text },
        { "type": "attachment", "attachment_id": Uuid::now_v7(), "name": "a.png", "mime": "image/png", "size": 10, "sha256": "x", "image": true }
    ]);
    let origin =
        json!({"kind":"message","from":Uuid::now_v7(),"title":"Оркестратор","chain_depth":1});
    sqlx::query("update inputs set feed_seq=7,state='accepted',content=$2,origin=$3 where id=$1")
        .bind(input.input_id)
        .bind(content)
        .bind(&origin)
        .execute(&f.pool)
        .await
        .unwrap();

    let item = &f.get(&f.path("/inputs")).await["items"][0];
    let preview = item["preview"].as_str().unwrap();
    assert_eq!(preview.chars().count(), 200);
    assert!(preview.chars().all(|c| c == 'я'));
    assert_eq!(item["attachments_count"], 1);
    assert_eq!(item["origin"], origin);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_messages_skip_hub_events_and_alarms(pool: PgPool) {
    let f = Fixture::new(pool).await;
    delivered(&f, "own", 10, "моё", None).await;
    delivered(
        &f,
        "event",
        20,
        "[canterlot] Session finished",
        Some(json!({"kind":"event","chain_depth":0})),
    )
    .await;
    delivered(
        &f,
        "alarm",
        30,
        "будильник",
        Some(json!({"kind":"alarm","alarm_id":Uuid::now_v7(),"chain_depth":0})),
    )
    .await;
    delivered(
        &f,
        "nudge",
        25,
        "автоматический толчок",
        Some(json!({"kind":"nudge","rule_id":Uuid::now_v7(),"attempt":2,"max":3,"chain_depth":0})),
    )
    .await;
    delivered(
        &f,
        "message",
        40,
        "от сессии",
        Some(json!({"kind":"message","from":Uuid::now_v7(),"chain_depth":1})),
    )
    .await;
    delivered(
        &f,
        "spawn",
        50,
        "задание",
        Some(json!({"kind":"spawn","from":Uuid::now_v7(),"chain_depth":1})),
    )
    .await;

    let page = f.get(&f.path("/inputs")).await;
    assert_eq!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["seq"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["10", "40", "50"]
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_messages_of_a_foreign_conversation_are_not_found(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let other_owner = Uuid::now_v7();
    sqlx::query("insert into owners(id) values($1)")
        .bind(other_owner)
        .execute(&f.pool)
        .await
        .unwrap();
    let foreign = testkit::seed_conversation(&f.pool, other_owner, f.seed.machine_id).await;
    let response = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/conversations/{foreign}/inputs"),
            None,
            None,
        )
        .await;
    assert_eq!(response.status.as_u16(), 404);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn history_around_returns_a_window_with_the_target_and_both_sides(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    for seq in [10, 30] {
        f.block(e, seq, "text", &format!("блок {seq}")).await;
    }
    delivered(&f, "m20", 20, "цель", None).await;

    let page = f.get(&f.path("/history?around=20&limit=8")).await;
    let keys = keys(&page);
    assert!(keys.contains(&20), "target stays in the window: {keys:?}");
    assert_eq!(keys, {
        let mut sorted = keys.clone();
        sorted.sort();
        sorted
    });
    assert!(keys.iter().any(|seq| *seq < 20));
    assert!(keys.iter().any(|seq| *seq > 20));
    assert!(page["next_before"].is_null() || page["next_before"].is_string());
    assert!(page["next_after"].is_null() || page["next_after"].is_string());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn history_around_rejects_two_directions_at_once(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let response = f
        .client
        .send(
            Method::GET,
            &f.path("/history?around=5&before=6"),
            None,
            None,
        )
        .await;
    assert!(response.status.is_client_error());
}
