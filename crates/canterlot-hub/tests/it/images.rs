//! Все картинки сессии одним списком: общий просмотрщик PWA листает их без привязки к сообщению.

use crate::support::task7 as support;
use axum::http::Method;
use canterlot_hub::{http, testkit::TestClient};
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

#[tokio::test]
async fn images_page_both_ways_and_around_an_item() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    let mut wanted = Vec::new();
    for n in 1..=5 {
        let input = f.input(&format!("i{n}")).await;
        feed(&f, input.input_id, n * 10).await;
        wanted.push(format!(
            "input_attachment:{}",
            owner_image(&f, input.input_id, 0, &format!("p{n}.png"), "image/png").await
        ));
        wanted.push(format!(
            "block_media:{}",
            block_image(&f, e, n * 10 + 1).await
        ));
    }

    let mut before = None;
    let mut back = Vec::new();
    loop {
        let page = f
            .get(&f.path(&format!(
                "/images?limit=3{}",
                before
                    .as_ref()
                    .map(|cursor: &String| format!("&before={cursor}"))
                    .unwrap_or_default()
            )))
            .await;
        let mut got = ids(&page);
        got.extend(back);
        back = got;
        before = page["next_before"].as_str().map(str::to_owned);
        if before.is_none() {
            break;
        }
    }
    assert_eq!(back, wanted);

    let mut after = "0:0:00000000-0000-0000-0000-000000000000".to_owned();
    let mut forward = Vec::new();
    loop {
        let page = f
            .get(&f.path(&format!("/images?limit=3&after={after}")))
            .await;
        forward.extend(ids(&page));
        let Some(cursor) = page["next_after"].as_str() else {
            break;
        };
        after = cursor.to_owned();
    }
    assert_eq!(forward, wanted);

    let middle = wanted[4].clone(); // ввод с картинкой на месте 30: пятая из десяти
    let page = f
        .get(&f.path(&format!("/images?limit=4&around={middle}")))
        .await;
    let got = ids(&page);
    assert_eq!(got.len(), 4);
    assert_eq!(page["total"], 10);
    assert_eq!(page["offset"], 4);
    assert_eq!(got[0], middle);
    assert_eq!(got, wanted[4..8].to_vec());
}

#[tokio::test]
async fn images_exclude_another_conversation_non_images_and_unready_files() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    let i1 = f.input("i1").await;
    feed(&f, i1.input_id, 10).await;
    let kept = owner_image(&f, i1.input_id, 0, "shot.png", "image/png").await;
    // Тип не картинка, картинка без имени-расширения с общим типом, неготовое вложение.
    owner_image(&f, i1.input_id, 1, "doc.pdf", "application/pdf").await;
    let generic = owner_image(&f, i1.input_id, 2, "scan.JPG", "application/octet-stream").await;
    owner_image(&f, i1.input_id, 3, "later.png", "image/png").await;
    sqlx::query("update attachments set state='uploading' where name='later.png'")
        .execute(&f.pool)
        .await
        .unwrap();
    let media = block_image(&f, e, 20).await;

    // Чужой разговор того же владельца.
    let other =
        canterlot_hub::testkit::seed_conversation(&f.pool, f.seed.owner_id, f.seed.machine_id)
            .await;
    let foreign = attachment(&f, other, "foreign.png", "image/png", "ready").await;

    let page = f.get(&f.path("/images")).await;
    let got = ids(&page);
    assert_eq!(
        got,
        vec![
            format!("input_attachment:{kept}"),
            format!("input_attachment:{generic}"),
            format!("block_media:{media}"),
        ]
    );
    assert!(!got.iter().any(|id| id.contains(&foreign.to_string())));
    assert_eq!(page["total"], 3);
}

#[tokio::test]
async fn images_reject_bad_cursors_and_unknown_items() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    for suffix in [
        "/images?before=nope",
        "/images?before=1:2",
        "/images?before=1:0:not-a-uuid",
        "/images?before=1:0:00000000-0000-0000-0000-000000000000&after=2:0:00000000-0000-0000-0000-000000000000",
        "/images?limit=0",
        "/images?limit=201",
    ] {
        let response = f
            .client
            .send(Method::GET, &f.path(suffix), None, None)
            .await;
        assert_eq!(response.status.as_u16(), 400, "{suffix}");
    }
    let unknown = f
        .client
        .send(
            Method::GET,
            &f.path("/images?around=block_media:00000000-0000-0000-0000-000000000001"),
            None,
            None,
        )
        .await;
    assert_eq!(unknown.status.as_u16(), 404);
}

#[tokio::test]
async fn images_are_owner_only() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    let anonymous = TestClient::in_process(http::router(f.state.clone()));
    let response = anonymous
        .send(Method::GET, &f.path("/images"), None, None)
        .await;
    assert_eq!(response.status.as_u16(), 401);
    assert_eq!(response.json::<Value>()["error"]["code"], "unauthenticated");
}

#[tokio::test]
async fn images_have_conversation_seq_indexes_for_paging() {
    let pool = canterlot_hub::testkit::db().await;
    let indexes: Vec<String> = sqlx::query_scalar(
        "select indexname from pg_indexes where indexname in
         ('block_media_conversation_seq','block_files_conversation_seq') order by indexname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        indexes,
        [
            "block_files_conversation_seq",
            "block_media_conversation_seq"
        ]
    );
}
