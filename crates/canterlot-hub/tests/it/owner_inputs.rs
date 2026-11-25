use crate::support::task7 as support;
use serde_json::{Value, json};
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

#[tokio::test]
async fn owner_messages_are_chronological_paged_and_skip_undelivered() {
    let pool = canterlot_hub::testkit::db().await;
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
