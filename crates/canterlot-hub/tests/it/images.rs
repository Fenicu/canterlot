//! Все картинки сессии одним списком: общий просмотрщик PWA листает их без привязки к сообщению.

use crate::support::task7 as support;
use serde_json::Value;
use support::*;
use uuid::Uuid;

async fn feed(f: &Fixture, input_id: Uuid, seq: i64) {
    sqlx::query("update inputs set feed_seq=$2, created_seq=$2, state='accepted' where id=$1")
        .bind(input_id)
        .bind(seq)
        .execute(&f.pool)
        .await
        .unwrap();
}

async fn attachment(
    f: &Fixture,
    conversation_id: Uuid,
    name: &str,
    mime: &str,
    state: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into attachments(id,owner_id,conversation_id,name,mime,size_bytes,sha256,state,ready_at)
         values($1,$2,$3,$4,$5,4096,$6,$7,clock_timestamp())",
    )
    .bind(id)
    .bind(f.seed.owner_id)
    .bind(conversation_id)
    .bind(name)
    .bind(mime)
    .bind("0".repeat(64))
    .bind(state)
    .execute(&f.pool)
    .await
    .unwrap();
    id
}

async fn owner_image(f: &Fixture, input_id: Uuid, position: i32, name: &str, mime: &str) -> Uuid {
    let id = attachment(f, f.seed.conversation_id, name, mime, "ready").await;
    sqlx::query("insert into input_attachments(input_id,attachment_id,position) values($1,$2,$3)")
        .bind(input_id)
        .bind(id)
        .bind(position)
        .execute(&f.pool)
        .await
        .unwrap();
    id
}

async fn block_image(f: &Fixture, execution: Uuid, seq: i64) -> Uuid {
    let media = Uuid::now_v7();
    let block = f.block(execution, seq - 1, "tool_call", "").await;
    sqlx::query(
        "insert into block_media(id,execution_id,conversation_id,block_id,mime,size_bytes,sha256,width,height,attached_seq,attached_at)
         values($1,$2,$3,$4,'image/png',2048,$5,800,600,$6,clock_timestamp())",
    )
    .bind(media)
    .bind(execution)
    .bind(f.seed.conversation_id)
    .bind(block)
    .bind("1".repeat(64))
    .bind(seq)
    .execute(&f.pool)
    .await
    .unwrap();
    media
}

async fn block_file_image(f: &Fixture, execution: Uuid, seq: i64, name: &str) -> Uuid {
    let id = attachment(f, f.seed.conversation_id, name, "image/webp", "ready").await;
    let block = f.block(execution, seq - 1, "text", "").await;
    sqlx::query(
        "insert into block_files(id,execution_id,conversation_id,block_id,attachment_id,name,mime,size_bytes,sha256,title,attached_seq,attached_at)
         values($1,$2,$3,$4,$1,$5,'image/webp',4096,$6,'',$7,clock_timestamp())",
    )
    .bind(id)
    .bind(execution)
    .bind(f.seed.conversation_id)
    .bind(block)
    .bind(name)
    .bind("2".repeat(64))
    .bind(seq)
    .execute(&f.pool)
    .await
    .unwrap();
    id
}

fn ids(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn images_are_listed_across_inputs_blocks_and_files_in_timeline_order() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    let i1 = f.input("i1").await;
    feed(&f, i1.input_id, 10).await;
    let first = owner_image(&f, i1.input_id, 0, "one.png", "image/png").await;
    let second = owner_image(&f, i1.input_id, 1, "two.jpg", "image/jpeg").await;
    let media = block_image(&f, e, 20).await;
    let file = block_file_image(&f, e, 30, "desk.webp").await;

    let page = f.get(&f.path("/images")).await;
    assert_eq!(
        ids(&page),
        vec![
            format!("input_attachment:{first}"),
            format!("input_attachment:{second}"),
            format!("block_media:{media}"),
            format!("block_file:{file}"),
        ]
    );
    let items = page["items"].as_array().unwrap();
    assert_eq!(items[0]["kind"], "input_attachment");
    assert_eq!(items[0]["seq"], "10");
    assert_eq!(items[0]["input_id"], i1.input_id.to_string());
    assert_eq!(items[0]["name"], "one.png");
    assert_eq!(items[0]["mime"], "image/png");
    assert_eq!(items[0]["size"], 4096);
    assert_eq!(items[1]["attachment_id"], second.to_string());
    assert_eq!(items[2]["kind"], "block_media");
    assert_eq!(items[2]["media_id"], media.to_string());
    assert_eq!(items[2]["width"], 800);
    assert_eq!(items[2]["height"], 600);
    // Переход к блоку идёт по месту блока в ленте, а не по номеру события вложения.
    assert_eq!(items[2]["seq"], "19");
    assert!(items[2]["block_id"].is_string());
    assert_eq!(items[3]["kind"], "block_file");
    assert_eq!(items[3]["name"], "desk.webp");
    assert_eq!(page["total"], 4);
    assert_eq!(page["offset"], 0);
    assert!(page["next_before"].is_null());
    assert!(page["next_after"].is_null());
}
