//! «Без уведомлений»: флаг сессии гасит пуши её дерева, а включение обратно
//! возвращает следующий пуш и отменяет уже поставленные.
use crate::support::task7 as http_support;

use axum::http::Method;
use canterlot_hub::{
    activity::Evidence,
    notify,
    session_tools::settle::{SettleFacts, Settled},
    testkit,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn now() -> DateTime<Utc> {
    Utc::now()
}

async fn enable(pool: &PgPool) {
    sqlx::query(
        "insert into push_settings(singleton,waiting,turn_finished,failures,timezone)
         values(true,true,true,true,'UTC')",
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn mute(pool: &PgPool, id: Uuid, muted: bool) {
    sqlx::query("update conversations set notifications_muted=$2 where id=$1")
        .bind(id)
        .bind(muted)
        .execute(pool)
        .await
        .unwrap();
}

async fn push_count(pool: &PgPool, id: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from owner_pushes where conversation_id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_muted_session_gets_no_push_for_any_conversation_event() {
    let pool = canterlot_hub::testkit::db().await;
    enable(&pool).await;
    let s = testkit::seed(&pool).await;
    mute(&pool, s.conversation_id, true).await;
    let at = now();
    let facts = SettleFacts {
        now: at,
        since: at - TimeDelta::seconds(5),
        last_turn_at: Some(at - TimeDelta::seconds(1)),
        last_event_at: Some(at),
        phase_since: None,
    };
    let idle = Evidence::Idle {
        last_turn_at: Some(at - TimeDelta::seconds(1)),
        background_work: None,
        process_alive: Some(false),
    };
    let failed = Evidence::Failed {
        reason: "context_exhausted".into(),
    };
    let mut tx = pool.begin().await.unwrap();
    notify::on_request_opened_in_tx(&mut tx, s.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    notify::on_settled_in_tx(
        &mut tx,
        s.conversation_id,
        Settled::Idle,
        "idle-1",
        &facts,
        &idle,
    )
    .await
    .unwrap();
    notify::on_settled_in_tx(
        &mut tx,
        s.conversation_id,
        Settled::Failed,
        "fail-1",
        &facts,
        &failed,
    )
    .await
    .unwrap();
    notify::on_routine_report_in_tx(&mut tx, s.conversation_id, Uuid::now_v7(), "Утро")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        push_count(&pool, s.conversation_id).await,
        0,
        "ни один пуш выключенной сессии не ставится"
    );
}

#[tokio::test]
async fn a_muted_parent_silences_its_delegate_and_unmuting_lets_the_next_request_through() {
    let pool = canterlot_hub::testkit::db().await;
    enable(&pool).await;
    let s = testkit::seed(&pool).await;
    let delegate = testkit::seed_conversation(&pool, s.owner_id, s.machine_id).await;
    sqlx::query("update conversations set parent_conversation_id=$2,spawn_depth=1 where id=$1")
        .bind(delegate)
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();

    mute(&pool, s.conversation_id, true).await;
    let mut tx = pool.begin().await.unwrap();
    notify::on_request_opened_in_tx(&mut tx, delegate, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        push_count(&pool, delegate).await,
        0,
        "выключенный оркестратор гасит делегата"
    );

    mute(&pool, s.conversation_id, false).await;
    let mut tx = pool.begin().await.unwrap();
    notify::on_request_opened_in_tx(&mut tx, delegate, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        push_count(&pool, delegate).await,
        1,
        "включение возвращает пуш следующего запроса"
    );

    // Делегат выключается и сам, не трогая пуши оркестратора.
    mute(&pool, delegate, true).await;
    let mut tx = pool.begin().await.unwrap();
    notify::on_request_opened_in_tx(&mut tx, s.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    notify::on_request_opened_in_tx(&mut tx, delegate, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        push_count(&pool, s.conversation_id).await,
        1,
        "флаг делегата не гасит родителя"
    );
    assert_eq!(
        push_count(&pool, delegate).await,
        1,
        "второй запрос выключенного делегата молчит"
    );
}

#[tokio::test]
async fn muting_cancels_pending_pushes_of_the_session_and_its_delegates() {
    let pool = canterlot_hub::testkit::db().await;
    let f = http_support::Fixture::new(pool.clone()).await;
    let child = testkit::seed_conversation(&pool, f.seed.owner_id, f.seed.machine_id).await;
    sqlx::query("update conversations set parent_conversation_id=$2,spawn_depth=1 where id=$1")
        .bind(child)
        .bind(f.seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    for (id, key) in [
        (f.seed.conversation_id, "pending-parent"),
        (child, "pending-child"),
    ] {
        sqlx::query(
            "insert into owner_pushes(id,conversation_id,kind,dedupe_key,payload,run_at)
             values($1,$2,'waiting',$3,'{}',clock_timestamp())",
        )
        .bind(Uuid::now_v7())
        .bind(id)
        .bind(key)
        .execute(&pool)
        .await
        .unwrap();
    }

    let response = f
        .client
        .send(
            Method::PUT,
            &f.path("/notifications"),
            Some("mute-a"),
            Some(json!({"muted":true})),
        )
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json::<Value>()["notifications_muted"], true);
    let muted: bool =
        sqlx::query_scalar("select notifications_muted from conversations where id=$1")
            .bind(f.seed.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(muted);
    let listed = f.get("/api/v1/conversations").await;
    assert_eq!(listed["items"][0]["notifications_muted"], true);
    let states: Vec<(String, String)> =
        sqlx::query_as("select dedupe_key,state from owner_pushes order by dedupe_key")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        states,
        vec![
            ("pending-child".into(), "cancelled".into()),
            ("pending-parent".into(), "cancelled".into()),
        ],
        "отмена накрывает и делегата"
    );

    // Повтор с тем же ключом ничего не меняет, а выключение флага возвращает маршрут.
    let replay = f
        .client
        .send(
            Method::PUT,
            &f.path("/notifications"),
            Some("mute-a"),
            Some(json!({"muted":true})),
        )
        .await;
    assert_eq!(replay.status, 200);
    let unmute = f
        .client
        .send(
            Method::PUT,
            &f.path("/notifications"),
            Some("mute-b"),
            Some(json!({"muted":false})),
        )
        .await;
    assert_eq!(unmute.status, 200);
    assert_eq!(unmute.json::<Value>()["notifications_muted"], false);
}
