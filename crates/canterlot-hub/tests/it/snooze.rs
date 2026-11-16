//! «Отложить»: срок возврата сессии, ранний возврат ради владельца и тишина пушей об итогах.
use crate::support::{task5 as activity_support, task7 as http_support};

use axum::http::Method;
use canterlot_hub::{
    activity::{self, Evidence},
    notify,
    session_tools::settle::{SettleFacts, Settled},
    testkit,
};
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap()
}

fn wire(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

async fn snoozed(pool: &PgPool, id: Uuid) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("select snoozed_until from conversations where id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn snooze(pool: &PgPool, id: Uuid, until: DateTime<Utc>) {
    sqlx::query("update conversations set snoozed_until=$2 where id=$1")
        .bind(id)
        .bind(until)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_endpoint_sets_and_clears_an_absolute_snooze(pool: PgPool) {
    let f = http_support::Fixture::new(pool.clone()).await;
    let until = now() + TimeDelta::hours(3);
    let set = f
        .client
        .send(
            Method::PUT,
            &f.path("/snooze"),
            Some("snooze-a"),
            Some(json!({"until": wire(until)})),
        )
        .await;
    assert_eq!(set.status, 200);
    assert_eq!(snoozed(&pool, f.seed.conversation_id).await, Some(until));
    assert_eq!(set.json::<Value>()["snoozed_until"], wire(until));
    let listed = f.get("/api/v1/conversations").await;
    assert_eq!(listed["items"][0]["snoozed_until"], wire(until));
    let detail = f.get(&f.path("")).await;
    assert_eq!(detail["snoozed_until"], wire(until));

    let clear = f
        .client
        .send(Method::DELETE, &f.path("/snooze"), Some("snooze-b"), None)
        .await;
    assert_eq!(clear.status, 204);
    assert_eq!(snoozed(&pool, f.seed.conversation_id).await, None);
    assert_eq!(f.get(&f.path("")).await["snoozed_until"], Value::Null);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_snooze_time_must_be_in_the_future_and_within_thirty_days(pool: PgPool) {
    let f = http_support::Fixture::new(pool.clone()).await;
    for (key, until) in [
        ("past", now() - TimeDelta::minutes(1)),
        ("far", now() + TimeDelta::days(31)),
    ] {
        let response = f
            .client
            .send(
                Method::PUT,
                &f.path("/snooze"),
                Some(key),
                Some(json!({"until": wire(until)})),
            )
            .await;
        assert_eq!(response.status, 400, "{key}");
        assert_eq!(response.json::<Value>()["error"]["code"], "invalid_request");
        assert_eq!(snoozed(&pool, f.seed.conversation_id).await, None);
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_question_wakes_a_snoozed_session(pool: PgPool) {
    let (s, execution) = activity_support::running(&pool).await;
    snooze(&pool, s.conversation_id, now() + TimeDelta::hours(8)).await;
    assert_eq!(
        activity_support::refresh(&pool, s.conversation_id).await,
        activity::Refresh::Changed
    );
    assert!(snoozed(&pool, s.conversation_id).await.is_some());

    sqlx::query("update executions set phase='awaiting_answer' where id=$1")
        .bind(execution)
        .execute(&pool)
        .await
        .unwrap();
    activity_support::refresh(&pool, s.conversation_id).await;
    assert_eq!(
        snoozed(&pool, s.conversation_id).await,
        None,
        "вопрос владельцу возвращает сессию"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failure_wakes_a_snoozed_session(pool: PgPool) {
    let (s, execution) = activity_support::running(&pool).await;
    snooze(&pool, s.conversation_id, now() + TimeDelta::hours(8)).await;
    activity_support::refresh(&pool, s.conversation_id).await;
    assert!(snoozed(&pool, s.conversation_id).await.is_some());

    sqlx::query("update executions set state='failed',outcome='context_exhausted' where id=$1")
        .bind(execution)
        .execute(&pool)
        .await
        .unwrap();
    activity_support::refresh(&pool, s.conversation_id).await;
    assert_eq!(
        snoozed(&pool, s.conversation_id).await,
        None,
        "сбой возвращает сессию"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_stall_wakes_a_snoozed_session(pool: PgPool) {
    let (s, execution) = activity_support::running(&pool).await;
    snooze(&pool, s.conversation_id, now() + TimeDelta::hours(8)).await;
    activity_support::refresh(&pool, s.conversation_id).await;
    assert!(snoozed(&pool, s.conversation_id).await.is_some());

    sqlx::query("update executions set phase='running_tool',phase_at=clock_timestamp()-interval '2 hours',last_event_at=clock_timestamp()-interval '2 hours' where id=$1")
        .bind(execution)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update queue_state set run_slot_since=clock_timestamp()-interval '2 hours' where conversation_id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions) values($1,clock_timestamp(),1,1,100,100,50,0,0,0,'[]',$2)")
        .bind(s.machine_id)
        .bind(json!([{"execution_id":execution,"alive":true,"cpu_permille":0,"descendants":0}]))
        .execute(&pool)
        .await
        .unwrap();
    activity_support::refresh(&pool, s.conversation_id).await;
    assert_eq!(
        snoozed(&pool, s.conversation_id).await,
        None,
        "подвисание возвращает сессию"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_finished_turn_does_not_wake_a_snoozed_session(pool: PgPool) {
    let (s, execution) = activity_support::running(&pool).await;
    snooze(&pool, s.conversation_id, now() + TimeDelta::hours(8)).await;
    activity_support::refresh(&pool, s.conversation_id).await;

    sqlx::query(
        "update executions set phase='awaiting_input',phase_at=clock_timestamp() where id=$1",
    )
    .bind(execution)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("update queue_state set run_slot='free',run_slot_input_id=null,hold_reasons='[]' where conversation_id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        activity_support::refresh(&pool, s.conversation_id).await,
        activity::Refresh::Changed
    );
    assert_eq!(
        activity_support::row(&pool, s.conversation_id).await["activity_state"],
        "idle"
    );
    assert!(
        snoozed(&pool, s.conversation_id).await.is_some(),
        "законченный ход не будит отложенную сессию"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_owner_message_wakes_a_snoozed_session(pool: PgPool) {
    let f = http_support::Fixture::new(pool.clone()).await;
    snooze(&pool, f.seed.conversation_id, now() + TimeDelta::hours(8)).await;
    let response = f
        .client
        .send(
            Method::POST,
            &f.path("/inputs"),
            Some("owner-message"),
            Some(json!({"content":[{"type":"text","text":"продолжай"}]})),
        )
        .await;
    assert_eq!(response.status, 201);
    assert_eq!(
        snoozed(&pool, f.seed.conversation_id).await,
        None,
        "сообщение владельца возвращает сессию"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_snooze_silences_turn_finished_pushes_but_not_waiting_or_failure(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    sqlx::query("insert into push_settings(singleton,waiting,turn_finished,failures,timezone) values(true,true,true,true,'UTC')")
        .execute(&pool)
        .await
        .unwrap();
    let at = now();
    snooze(&pool, s.conversation_id, at + TimeDelta::hours(8)).await;
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
    let mut tx = pool.begin().await.unwrap();
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
    tx.commit().await.unwrap();
    let finished: i64 = sqlx::query_scalar(
        "select count(*) from owner_pushes where conversation_id=$1 and kind='turn_finished'",
    )
    .bind(s.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(finished, 0, "итог хода отложенной сессии не пушится");

    let mut tx = pool.begin().await.unwrap();
    notify::on_request_opened_in_tx(&mut tx, s.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    let failed = Evidence::Failed {
        reason: "context_exhausted".into(),
    };
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
    tx.commit().await.unwrap();
    let kinds: Vec<String> =
        sqlx::query_scalar("select kind from owner_pushes where conversation_id=$1 order by kind")
            .bind(s.conversation_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(kinds, vec!["failure".to_string(), "waiting".to_string()]);

    // Без срока ожидания итог хода снова пушится: подавление живёт ровно до возврата.
    sqlx::query("update conversations set snoozed_until=null where id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
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
    tx.commit().await.unwrap();
    let finished: i64 = sqlx::query_scalar(
        "select count(*) from owner_pushes where conversation_id=$1 and kind='turn_finished'",
    )
    .bind(s.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(finished, 1);
}
