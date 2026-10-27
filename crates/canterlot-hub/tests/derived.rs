use canterlot_hub::{
    activity::{self, Refresh},
    bus::EventBus,
    config::{ActivityConfig, MachinesConfig},
    events::{self, NewEvent, Scope},
    testkit,
};
use chrono::{TimeDelta, Timelike, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgListener};
use std::time::Duration;
use tokio::sync::watch;
use uuid::Uuid;
#[path = "support/task5.rs"]
mod support;
use support::*;

#[sqlx::test(migrations = false)]
async fn migration_and_startup_project_old_terminal_conversations_without_events(pool: PgPool) {
    for migration in canterlot_hub::db::MIGRATOR
        .iter()
        .filter(|m| m.version < 10)
    {
        sqlx::raw_sql(migration.sql.as_ref())
            .execute(&pool)
            .await
            .unwrap();
    }
    let s = testkit::seed(&pool).await;
    let execution = Uuid::now_v7();
    sqlx::query("insert into executions(id,conversation_id,generation,state,outcome) values($1,$2,1,'stopped','user_stop')").bind(execution).bind(s.conversation_id).execute(&pool).await.unwrap();
    sqlx::query("insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_seq) values($1,$2,$3,'text','final',1,10)").bind(Uuid::now_v7()).bind(execution).bind(s.conversation_id).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!("../migrations/0010_derived_and_scheduler.sql"))
        .execute(&pool)
        .await
        .unwrap();
    let (stop, task, _) = projector(
        &pool,
        ActivityConfig {
            tick: Duration::from_millis(20),
            ..Default::default()
        },
    )
    .await;
    until_state(&pool, s.conversation_id, "stopped").await;
    assert_eq!(row(&pool, s.conversation_id).await["unread_count"], 1);
    let copied:bool=sqlx::query_scalar("select d.last_activity_at=c.last_activity_at from conversations c join conversation_derived d on d.conversation_id=c.id where c.id=$1").bind(s.conversation_id).fetch_one(&pool).await.unwrap();
    assert!(copied);
    stop.send(true).unwrap();
    task.await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn refresh_writes_row_and_emits_one_event_per_change_in_both_scopes(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Changed);
    let ev: Vec<(String, Value)> = sqlx::query_as(
        "select scope,payload from events where kind='derived_changed' order by committed_at,id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(ev.len(), 2);
    assert_eq!(ev[0].0, format!("conversation:{}", s.conversation_id));
    assert_eq!(ev[1].0, format!("owner:{}:conversations", s.owner_id));
    assert_eq!(ev[0].1, ev[1].1);
    let since = row(&pool, s.conversation_id).await["activity_since"].clone();
    input(&pool, &s, "one").await;
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Changed);
    assert_eq!(row(&pool, s.conversation_id).await["activity_since"], since);
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='derived_changed'"
        )
        .await,
        4
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn refresh_without_change_emits_nothing(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    refresh(&pool, s.conversation_id).await;
    let before = row(&pool, s.conversation_id).await;
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Unchanged);
    assert_eq!(row(&pool, s.conversation_id).await, before);
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='derived_changed'"
        )
        .await,
        2
    );
}

async fn projector(
    pool: &PgPool,
    config: ActivityConfig,
) -> (watch::Sender<bool>, tokio::task::JoinHandle<()>, EventBus) {
    let (stop, rx) = watch::channel(false);
    let bus = EventBus::spawn(pool.clone(), false, rx.clone());
    let task = tokio::spawn(activity::run(
        pool.clone(),
        bus.clone(),
        config,
        MachinesConfig::default(),
        rx,
    ));
    (stop, task, bus)
}

#[sqlx::test(migrations = "./migrations")]
async fn stalled_is_detected_by_the_tick_without_any_event(pool: PgPool) {
    let (s, e) = running(&pool).await;
    let now = Utc::now();
    sqlx::query("update executions set phase_at=$2,last_event_at=$2 where id=$1")
        .bind(e)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
    let config = ActivityConfig {
        stalled_generating: Duration::from_millis(600),
        tick: Duration::from_millis(20),
        ..Default::default()
    };
    let (stop, task, _) = projector(&pool, config).await;
    until_state(&pool, s.conversation_id, "working").await;
    until_state(&pool, s.conversation_id, "stalled").await;
    stop.send(true).unwrap();
    task.await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn stalled_never_changes_execution_queue_or_commands(pool: PgPool) {
    let (s, _) = running(&pool).await;
    let snapshot = "select jsonb_build_object('executions',(select jsonb_agg(to_jsonb(e)) from executions e),'queue',(select jsonb_agg(to_jsonb(q)) from queue_state q),'commands',(select jsonb_agg(to_jsonb(c)) from commands c))";
    let before: Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
    refresh(&pool, s.conversation_id).await;
    assert_eq!(
        row(&pool, s.conversation_id).await["activity_state"],
        "stalled"
    );
    let after: Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
    assert_eq!(before, after);
}

#[sqlx::test(migrations = "./migrations")]
async fn projector_ignores_deltas_and_its_own_events(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    refresh(&pool, s.conversation_id).await;
    let config = ActivityConfig {
        tick: Duration::from_secs(30),
        debounce: Duration::from_millis(10),
        ..Default::default()
    };
    let (stop, task, _) = projector(&pool, config).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    sqlx::query("update queue_state set pause_reasons='[{\"reason\":\"user_stop\"}]' where conversation_id=$1").bind(s.conversation_id).execute(&pool).await.unwrap();
    for kind in ["block_delta", "derived_changed", "scheduled_changed"] {
        notice(&pool, s.conversation_id, kind).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        row(&pool, s.conversation_id).await["pause_reasons"],
        json!([])
    );
    notice(&pool, s.conversation_id, "queue_state_changed").await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if row(&pool, s.conversation_id).await["pause_reasons"]
                == json!([{"reason":"user_stop"}])
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    task.await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn unread_counts_text_warning_and_error_blocks_after_the_read_marker(pool: PgPool) {
    let (s, e) = running(&pool).await;
    for (n, kind, state) in [
        (1, "text", "final"),
        (2, "warning", "streaming"),
        (3, "error", "final"),
        (4, "reasoning", "final"),
        (5, "tool_call", "streaming"),
        (6, "text", "incomplete"),
    ] {
        sqlx::query("insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_seq) values($1,$2,$3,$4,$5,1,$6)").bind(Uuid::now_v7()).bind(e).bind(s.conversation_id).bind(kind).bind(state).bind(n as i64).execute(&pool).await.unwrap();
    }
    refresh(&pool, s.conversation_id).await;
    assert_eq!(row(&pool, s.conversation_id).await["unread_count"], 3);
    sqlx::query("update conversations set last_read_seq=2 where id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    refresh(&pool, s.conversation_id).await;
    assert_eq!(row(&pool, s.conversation_id).await["unread_count"], 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn process_evidence_comes_only_from_fresh_metrics(pool: PgPool) {
    let (s, e) = running(&pool).await;
    let now = Utc::now();
    sqlx::query("insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions) values($1,$2,1,1,100,100,50,0,0,0,'[]',$3)").bind(s.machine_id).bind(now).bind(json!([{"execution_id":e,"alive":true,"cpu_permille":50}])).execute(&pool).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    for (age, expected) in [(0, Some(true)), (91, None)] {
        let i = activity::load_inputs(
            &mut conn,
            s.conversation_id,
            now + TimeDelta::seconds(age),
            Duration::from_secs(90),
        )
        .await
        .unwrap();
        let a = activity::compute(&i, &thresholds());
        assert!(
            matches!(a.evidence,activity::Evidence::Stalled { process_alive,cpu_busy,.. } if process_alive==expected && cpu_busy==expected)
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn event_append_publishes_the_notice_in_process_and_pg_notify_only_in_multi_process_mode(
    pool: PgPool,
) {
    let s = testkit::seed(&pool).await;
    let (stop, rx) = watch::channel(false);
    let mut listener = PgListener::connect_with(&pool).await.unwrap();
    listener.listen("canterlot_events").await.unwrap();
    for multi in [false, true] {
        let bus = EventBus::spawn(pool.clone(), multi, rx.clone());
        let mut notices = bus.subscribe();
        let mut tx = pool.begin().await.unwrap();
        let ev = events::append(
            &mut tx,
            NewEvent {
                scope: Scope::Conversation {
                    id: s.conversation_id,
                },
                kind: "phase_changed",
                conversation_id: Some(s.conversation_id),
                execution_id: None,
                payload: json!({}),
                correlation_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
        let n = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let n = notices.recv().await.unwrap();
                if n.scope == ev.scope && n.scope_seq == ev.scope_seq {
                    break n;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(n.scope, ev.scope);
        assert_eq!(n.kind, "phase_changed");
        assert_eq!(n.scope_seq, ev.scope_seq);
        assert_eq!(
            count(&pool, "select count(*) from events").await,
            if multi { 1 } else { 0 }
        );
        tx.commit().await.unwrap();
        let notification = tokio::time::timeout(Duration::from_millis(150), listener.recv()).await;
        if multi {
            assert_eq!(
                notification.unwrap().unwrap().payload(),
                format!("conversation:{}|phase_changed", s.conversation_id)
            );
        } else {
            assert!(notification.is_err());
        }
    }
    stop.send(true).unwrap();
}

#[cfg(feature = "fault-injection")]
#[sqlx::test(migrations = "./migrations")]
async fn missed_notice_is_repaired_by_the_lag_sweep(pool: PgPool) {
    let (s, e) = running(&pool).await;
    let (stop, task, bus) = projector(
        &pool,
        ActivityConfig {
            tick: Duration::from_millis(100),
            ..Default::default()
        },
    )
    .await;
    until_state(&pool, s.conversation_id, "stalled").await;
    bus.drop_next_notice(&format!("conversation:{}", s.conversation_id));
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("update executions set state='stopped',outcome='user_stop' where id=$1")
        .bind(e)
        .execute(&mut *tx)
        .await
        .unwrap();
    events::append(
        &mut tx,
        NewEvent {
            scope: Scope::Conversation {
                id: s.conversation_id,
            },
            kind: "execution_state_changed",
            conversation_id: Some(s.conversation_id),
            execution_id: Some(e),
            payload: json!({"state":"stopped"}),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    tokio::time::timeout(
        Duration::from_millis(200),
        until_state(&pool, s.conversation_id, "stopped"),
    )
    .await
    .unwrap();
    stop.send(true).unwrap();
    task.await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn refresh_upserts_the_derived_row_for_conversations_that_predate_the_migration(
    pool: PgPool,
) {
    let s = testkit::seed(&pool).await;
    sqlx::query("delete from conversation_derived where conversation_id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Changed);
    assert_eq!(
        row(&pool, s.conversation_id).await["activity_state"],
        "idle"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn derived_changed_carries_queue_summary_and_last_activity_and_copies_it_to_conversations(
    pool: PgPool,
) {
    let s = testkit::seed(&pool).await;
    input(&pool, &s, "one").await;
    refresh(&pool, s.conversation_id).await;
    let payload: Value =
        sqlx::query_scalar("select payload from events where kind='derived_changed' limit 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        payload["queue"],
        json!({"queued":1,"pause_reasons":[],"hold_reasons":[{"reason":"no_live_execution"}]})
    );
    let at: chrono::DateTime<Utc> =
        sqlx::query_scalar("select last_activity_at from conversations where id=$1")
            .bind(s.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(at.second(), 0);
    assert_eq!(at.nanosecond(), 0);
    assert_eq!(
        payload["last_activity_at"],
        at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );
    assert_eq!(
        payload["settings"],
        json!({"model":null,"effort":null,"permission_mode":null})
    );
    assert_eq!(payload["crunch"], json!({"enabled":false}));
}

#[sqlx::test(migrations = "./migrations")]
async fn settings_crunch_or_queue_change_alone_emits_derived_changed_in_both_scopes(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    refresh(&pool, s.conversation_id).await;
    sqlx::query("update queue_state set pause_reasons='[{\"reason\":\"user_stop\"}]' where conversation_id=$1").bind(s.conversation_id).execute(&pool).await.unwrap();
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Changed);
    let ev: Vec<(String, Value)> = sqlx::query_as(
        "select scope,payload from events where kind='derived_changed' order by committed_at,id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(ev.len(), 4);
    assert_eq!(ev[2].1, ev[3].1);
    assert_eq!(ev[2].1["activity"]["state"], "idle");
    assert_eq!(
        ev[2].1["queue"]["pause_reasons"],
        json!([{"reason":"user_stop"}])
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn last_activity_is_truncated_to_the_minute_and_streaming_emits_no_list_event_for_it(
    pool: PgPool,
) {
    let (s, e) = running(&pool).await;
    let block = Uuid::now_v7();
    let at = Utc::now()
        .with_second(1)
        .unwrap()
        .with_nanosecond(123_000_000)
        .unwrap();
    sqlx::query("insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_seq,created_at) values($1,$2,$3,'text','streaming',1,1,$4)").bind(block).bind(e).bind(s.conversation_id).bind(at).execute(&pool).await.unwrap();
    refresh(&pool, s.conversation_id).await;
    let initial = row(&pool, s.conversation_id).await;
    sqlx::query("update blocks set content='stream update',rev=2,updated_at=$2 where block_id=$1")
        .bind(block)
        .bind(at + TimeDelta::minutes(10))
        .execute(&pool)
        .await
        .unwrap();
    notice(&pool, s.conversation_id, "block_delta").await;
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Unchanged);
    assert_eq!(row(&pool, s.conversation_id).await, initial);
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where scope like 'owner:%' and kind='derived_changed'"
        )
        .await,
        1
    );
}
