//! «Без уведомлений»: флаг сессии гасит пуши её дерева, а включение обратно
//! возвращает следующий пуш и отменяет уже поставленные.

use canterlot_hub::{
    activity::Evidence,
    notify,
    session_tools::settle::{SettleFacts, Settled},
    testkit,
};
use chrono::{DateTime, TimeDelta, Utc};
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
