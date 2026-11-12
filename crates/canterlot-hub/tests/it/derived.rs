use crate::support::task5 as support;
use canterlot_hub::{
    activity::{self, Refresh},
    bus::EventBus,
    config::{ActivityConfig, MachinesConfig},
    events::{self, NewEvent, Scope},
    testkit,
};
use chrono::{DateTime, TimeDelta, Timelike, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgListener};
use std::time::Duration;
use support::*;
use tokio::sync::watch;
use uuid::Uuid;

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
    for migration in canterlot_hub::db::MIGRATOR
        .iter()
        .filter(|m| m.version >= 10)
    {
        sqlx::raw_sql(migration.sql.as_ref())
            .execute(&pool)
            .await
            .unwrap();
    }
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

#[sqlx::test(migrations = "./migrations")]
async fn a_new_turn_in_the_same_phase_rebases_the_activity_clocks(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    // Прошлый ход закончился, но исполнение живо и стоит в прежней фазе:
    // отметка фазы осталась от него, а не от нового хода.
    let execution = Uuid::now_v7();
    sqlx::query("insert into executions(id,conversation_id,generation,state,phase,phase_at,last_event_at) values($1,$2,1,'running','awaiting_input',clock_timestamp()-interval '6 hours',clock_timestamp()-interval '6 hours')")
        .bind(execution)
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Changed);
    assert_eq!(
        row(&pool, s.conversation_id).await["activity_state"],
        "idle"
    );

    // Владелец пишет: слот хода занят только что, а фаза в исполнении ещё прежняя.
    sqlx::query("update queue_state set run_slot='running',run_slot_input_id=$2,run_slot_since=clock_timestamp(),hold_reasons='[]' where conversation_id=$1")
        .bind(s.conversation_id)
        .bind(Uuid::now_v7())
        .execute(&pool)
        .await
        .unwrap();
    let turn_start: DateTime<Utc> =
        sqlx::query_scalar("select run_slot_since from queue_state where conversation_id=$1")
            .bind(s.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Changed);
    let derived = row(&pool, s.conversation_id).await;
    assert_eq!(derived["activity_state"], "working");
    let at = |value: &Value| {
        DateTime::parse_from_rfc3339(value.as_str().unwrap())
            .unwrap()
            .with_timezone(&Utc)
    };
    // В событие время уходит с миллисекундами: допускаем их потерю.
    let phase_since = at(&derived["activity_evidence"]["phase_since"]);
    assert!(
        phase_since >= turn_start - TimeDelta::milliseconds(1),
        "phase_since {phase_since} is older than the turn start {turn_start}"
    );
    let since = at(&derived["activity_since"]);
    assert!(
        since >= turn_start,
        "activity_since {since} is older than the turn start {turn_start}"
    );

    // Состояние работы уже шло, когда начался ход: часы переезжают на начало хода.
    sqlx::query("update conversation_derived set activity_since = clock_timestamp() - interval '6 hours' where conversation_id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Changed);
    let since = at(&row(&pool, s.conversation_id).await["activity_since"]);
    assert!(
        since >= turn_start,
        "activity_since {since} kept counting from the previous turn"
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
        true,
        rx,
    ));
    (stop, task, bus)
}

#[sqlx::test(migrations = "./migrations")]
async fn stalled_is_detected_by_the_tick_without_any_event(pool: PgPool) {
    let (s, e) = running(&pool).await;
    sqlx::query("insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions) values($1,clock_timestamp(),1,1,100,100,50,0,0,0,'[]',$2)")
        .bind(s.machine_id).bind(json!([{"execution_id":e,"alive":true,"cpu_permille":0,"descendants":0}]))
        .execute(&pool).await.unwrap();
    sqlx::query("update executions set phase_at=clock_timestamp(),last_event_at=clock_timestamp() where id=$1")
        .bind(e)
        .execute(&pool)
        .await
        .unwrap();
    let config = ActivityConfig {
        tick: Duration::from_millis(20),
        ..Default::default()
    };
    let (stop, task, _) = projector(&pool, config).await;
    until_state(&pool, s.conversation_id, "working").await;
    sqlx::query("update executions set phase_at=clock_timestamp()-interval '31 minutes',last_event_at=clock_timestamp()-interval '31 minutes' where id=$1")
        .bind(e)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update queue_state set run_slot_since=clock_timestamp()-interval '31 minutes' where conversation_id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    until_state(&pool, s.conversation_id, "stalled").await;
    stop.send(true).unwrap();
    task.await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn stalled_never_changes_execution_queue_or_commands(pool: PgPool) {
    let (s, e) = running(&pool).await;
    sqlx::query("insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions) values($1,clock_timestamp(),1,1,100,100,50,0,0,0,'[]',$2)")
        .bind(s.machine_id).bind(json!([{"execution_id":e,"alive":true,"cpu_permille":0,"descendants":0}]))
        .execute(&pool).await.unwrap();
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
    sqlx::query("insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions) values($1,$2,1,1,100,100,50,0,0,0,'[]',$3)").bind(s.machine_id).bind(now).bind(json!([{"execution_id":e,"alive":true,"cpu_permille":0,"descendants":0}])).execute(&pool).await.unwrap();
    let mut conn = pool.acquire().await.unwrap();
    for (age, fresh) in [(0, true), (91, false)] {
        let i = activity::load_inputs(
            &mut conn,
            s.conversation_id,
            now + TimeDelta::seconds(age),
            Duration::from_secs(90),
        )
        .await
        .unwrap();
        let a = activity::compute(&i, &thresholds());
        if fresh {
            assert!(
                matches!(a.evidence,activity::Evidence::Stalled { process_alive,cpu_busy,.. } if process_alive==Some(true) && cpu_busy==Some(false))
            );
        } else {
            assert_eq!(
                a.state,
                activity::ActivityState::Working,
                "без свежих метрик судить о зависании нельзя"
            );
        }
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn live_tool_descendants_keep_a_silent_tool_working(pool: PgPool) {
    let (s, e) = running(&pool).await;
    sqlx::query("update executions set phase='running_tool',phase_at=clock_timestamp()-interval '2 hours',last_event_at=clock_timestamp()-interval '2 hours' where id=$1")
        .bind(e)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update queue_state set run_slot_since=clock_timestamp()-interval '2 hours' where conversation_id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let now = Utc::now();
    let set_metrics = |descendants: u32, at: chrono::DateTime<Utc>| {
        let pool = pool.clone();
        let machine = s.machine_id;
        let execution = e;
        async move {
            sqlx::query("insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions) values($1,$2,1,1,100,100,50,0,0,0,'[]',$3) on conflict(machine_id) do update set observed_at=excluded.observed_at,machine_time_ms=excluded.machine_time_ms,executions=excluded.executions").bind(machine).bind(at).bind(json!([{"execution_id":execution,"alive":true,"cpu_permille":0,"descendants":descendants}])).execute(&pool).await.unwrap();
        }
    };
    set_metrics(1, now).await;
    let mut conn = pool.acquire().await.unwrap();
    let inputs = activity::load_inputs(&mut conn, s.conversation_id, now, Duration::from_secs(90))
        .await
        .unwrap();
    assert_eq!(
        activity::compute(&inputs, &thresholds()).state,
        activity::ActivityState::Working,
        "живой потомок инструмента — доказательство работы"
    );
    set_metrics(0, now + TimeDelta::seconds(1)).await;
    let inputs = activity::load_inputs(
        &mut conn,
        s.conversation_id,
        now + TimeDelta::seconds(1),
        Duration::from_secs(90),
    )
    .await
    .unwrap();
    assert_eq!(
        activity::compute(&inputs, &thresholds()).state,
        activity::ActivityState::Stalled,
        "потомков нет и события молчат дольше порога"
    );
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
    sqlx::query("insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions) values($1,clock_timestamp(),1,1,100,100,50,0,0,0,'[]',$2)")
        .bind(s.machine_id).bind(json!([{"execution_id":e,"alive":true,"cpu_permille":0,"descendants":0}]))
        .execute(&pool).await.unwrap();
    sqlx::query("update executions set phase_at=clock_timestamp()-interval '31 minutes',last_event_at=clock_timestamp()-interval '31 minutes' where id=$1")
        .bind(e)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update queue_state set run_slot_since=clock_timestamp()-interval '31 minutes' where conversation_id=$1")
        .bind(s.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
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
    until_state(&pool, s.conversation_id, "stopped").await;
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
        json!({"model":"","effort":"","permission_mode":""})
    );
    assert_eq!(payload["crunch"], json!({"enabled":false}));
    assert_eq!(payload["working_ms"], json!(0));
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

async fn answer_block(
    pool: &PgPool,
    s: &testkit::Seed,
    execution: Uuid,
    kind: &str,
    agent: &str,
    content: &str,
    at: DateTime<Utc>,
) {
    sqlx::query("insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,agent,created_seq,created_at) values($1,$2,$3,$4,'final',1,$5,$6,(select coalesce(max(created_seq),0)+1 from blocks where conversation_id=$3),$7)")
        .bind(Uuid::now_v7()).bind(execution).bind(s.conversation_id).bind(kind).bind(content).bind(agent).bind(at)
        .execute(pool).await.unwrap();
}

async fn turn(
    pool: &PgPool,
    s: &testkit::Seed,
    execution: Uuid,
    started: DateTime<Utc>,
    finished: Option<DateTime<Utc>>,
) -> Uuid {
    let command = Uuid::now_v7();
    sqlx::query("insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state) values($1,$2,$3,$4,1,'deliver_input','{}','completed')")
        .bind(command).bind(s.machine_id).bind(s.conversation_id).bind(execution)
        .execute(pool).await.unwrap();
    sqlx::query("insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at) values($1,$2,$3,$4,$5)")
        .bind(command).bind(execution).bind(s.conversation_id).bind(started).bind(finished)
        .execute(pool).await.unwrap();
    command
}

async fn last_preview(pool: &PgPool, s: &testkit::Seed) -> Value {
    let payload: Value = sqlx::query_scalar(
        "select payload from events where kind='derived_changed' and scope=$1 order by scope_seq desc limit 1",
    )
    .bind(format!("owner:{}:conversations", s.owner_id))
    .fetch_one(pool)
    .await
    .unwrap();
    payload["preview"].clone()
}

async fn listed_preview(pool: &PgPool, s: &testkit::Seed) -> Option<String> {
    let mut conn = pool.acquire().await.unwrap();
    let page = canterlot_hub::conversations::list(
        &mut conn,
        &canterlot_hub::conversations::ListQuery {
            owner_id: s.owner_id,
            filter: canterlot_hub::conversations::ListFilter::All,
            q: None,
            cursor: None,
            limit: 50,
        },
    )
    .await
    .unwrap();
    page.items
        .into_iter()
        .find(|item| item.attributes.id == s.conversation_id)
        .unwrap()
        .preview
}

#[sqlx::test(migrations = "./migrations")]
async fn preview_is_the_plain_last_answer_of_the_finished_turn_in_event_and_list(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    refresh(&pool, s.conversation_id).await;
    assert_eq!(last_preview(&pool, &s).await, Value::Null);
    assert_eq!(listed_preview(&pool, &s).await, None);

    let execution = Uuid::now_v7();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'running')",
    )
    .bind(execution)
    .bind(s.conversation_id)
    .execute(&pool)
    .await
    .unwrap();
    let t0 = Utc::now() - TimeDelta::minutes(30);
    turn(&pool, &s, execution, t0, Some(t0 + TimeDelta::minutes(6))).await;
    answer_block(
        &pool,
        &s,
        execution,
        "text",
        "main",
        "Черновой ответ",
        t0 + TimeDelta::seconds(30),
    )
    .await;
    answer_block(
        &pool,
        &s,
        execution,
        "text",
        "main",
        "## Итог\n\n**Готово**: тесты `cargo test` [зелёные](https://ci/1)",
        t0 + TimeDelta::minutes(1),
    )
    .await;
    // Рассуждения, инструменты и тексты подагентов в превью не попадают.
    answer_block(
        &pool,
        &s,
        execution,
        "reasoning",
        "main",
        "скрытые рассуждения",
        t0 + TimeDelta::minutes(2),
    )
    .await;
    answer_block(
        &pool,
        &s,
        execution,
        "tool_call",
        "main",
        "{\"command\":\"cat secret\"}",
        t0 + TimeDelta::minutes(3),
    )
    .await;
    answer_block(
        &pool,
        &s,
        execution,
        "tool_result",
        "main",
        "вывод инструмента",
        t0 + TimeDelta::minutes(4),
    )
    .await;
    answer_block(
        &pool,
        &s,
        execution,
        "text",
        "general",
        "ответ подагента",
        t0 + TimeDelta::minutes(5),
    )
    .await;
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Changed);
    let expected = "Итог Готово: тесты cargo test зелёные";
    assert_eq!(last_preview(&pool, &s).await, json!(expected));
    assert_eq!(listed_preview(&pool, &s).await.as_deref(), Some(expected));
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Unchanged);

    // Идущий ход превью не трогает: строка показывает его состояние, а превью — прошлый ответ.
    let t1 = t0 + TimeDelta::minutes(10);
    let running = turn(&pool, &s, execution, t1, None).await;
    let long = "слово ".repeat(60);
    answer_block(
        &pool,
        &s,
        execution,
        "text",
        "main",
        &long,
        t1 + TimeDelta::minutes(1),
    )
    .await;
    refresh(&pool, s.conversation_id).await;
    assert_eq!(listed_preview(&pool, &s).await.as_deref(), Some(expected));

    sqlx::query("update runs set finished_at=$2 where input_command_id=$1")
        .bind(running)
        .bind(t1 + TimeDelta::minutes(2))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Changed);
    let fresh = last_preview(&pool, &s).await;
    let fresh = fresh.as_str().unwrap();
    assert_eq!(fresh.chars().count(), 160);
    assert!(fresh.starts_with("слово слово") && fresh.ends_with('…'));
    assert_eq!(listed_preview(&pool, &s).await.as_deref(), Some(fresh));
}

#[sqlx::test(migrations = "./migrations")]
async fn sweep_fills_the_preview_of_conversations_finished_before_the_migration(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let execution = Uuid::now_v7();
    sqlx::query("insert into executions(id,conversation_id,generation,state,outcome) values($1,$2,1,'stopped','user_stop')")
        .bind(execution).bind(s.conversation_id).execute(&pool).await.unwrap();
    let t0 = Utc::now() - TimeDelta::hours(2);
    turn(&pool, &s, execution, t0, Some(t0 + TimeDelta::minutes(5))).await;
    answer_block(
        &pool,
        &s,
        execution,
        "text",
        "main",
        "Старый *ответ*",
        t0 + TimeDelta::minutes(1),
    )
    .await;
    refresh(&pool, s.conversation_id).await;
    // Строка до миграции: превью не посчитано, событий о сессии новее строки нет.
    sqlx::query("update conversation_derived set preview=null,preview_turn_at=null,updated_at=clock_timestamp()+interval '1 minute' where conversation_id=$1")
        .bind(s.conversation_id).execute(&pool).await.unwrap();
    let events_before = count(
        &pool,
        "select count(*) from events where kind='derived_changed'",
    )
    .await;
    let (stop, task, _) = projector(
        &pool,
        ActivityConfig {
            tick: Duration::from_millis(20),
            ..Default::default()
        },
    )
    .await;
    for _ in 0..200 {
        if row(&pool, s.conversation_id).await["preview"] == json!("Старый ответ") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        row(&pool, s.conversation_id).await["preview"],
        json!("Старый ответ")
    );
    stop.send(true).unwrap();
    task.await.unwrap();
    // Дозаливка тихая: событий о старой сессии не появилось.
    assert_eq!(
        count(
            &pool,
            "select count(*) from events where kind='derived_changed'"
        )
        .await,
        events_before
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn preview_backfill_goes_in_bounded_batches_without_events(pool: PgPool) {
    let s = testkit::seed(&pool).await;
    let other = testkit::Seed {
        owner_id: s.owner_id,
        machine_id: s.machine_id,
        conversation_id: testkit::seed_conversation(&pool, s.owner_id, s.machine_id).await,
    };
    let t0 = Utc::now() - TimeDelta::hours(2);
    for (seed, text) in [(&s, "Первый"), (&other, "Второй")] {
        let execution = Uuid::now_v7();
        sqlx::query("insert into executions(id,conversation_id,generation,state,outcome) values($1,$2,1,'stopped','user_stop')")
            .bind(execution).bind(seed.conversation_id).execute(&pool).await.unwrap();
        turn(&pool, seed, execution, t0, Some(t0 + TimeDelta::minutes(5))).await;
        answer_block(
            &pool,
            seed,
            execution,
            "text",
            "main",
            text,
            t0 + TimeDelta::minutes(1),
        )
        .await;
        refresh(&pool, seed.conversation_id).await;
    }
    sqlx::query("update conversation_derived set preview=null,preview_turn_at=null")
        .execute(&pool)
        .await
        .unwrap();
    let events_before = count(&pool, "select count(*) from events").await;
    assert_eq!(activity::BACKFILL_BATCH, 200);
    assert_eq!(activity::backfill_previews(&pool, 1).await.unwrap(), 1);
    assert_eq!(
        count(
            &pool,
            "select count(*) from conversation_derived where preview_turn_at is null"
        )
        .await,
        1
    );
    assert_eq!(activity::backfill_previews(&pool, 1).await.unwrap(), 1);
    assert_eq!(activity::backfill_previews(&pool, 1).await.unwrap(), 0);
    let previews: Vec<(Option<String>, Option<DateTime<Utc>>)> =
        sqlx::query_as("select preview,preview_turn_at from conversation_derived order by preview")
            .fetch_all(&pool)
            .await
            .unwrap();
    let turn_at = Some(t0 + TimeDelta::minutes(5));
    assert_eq!(previews.len(), 2);
    assert_eq!(previews[0].0.as_deref(), Some("Второй"));
    assert_eq!(previews[1].0.as_deref(), Some("Первый"));
    assert!(previews.iter().all(
        |(_, at)| at.map(|at| at.timestamp_micros()) == turn_at.map(|at| at.timestamp_micros())
    ));
    assert_eq!(
        count(&pool, "select count(*) from events").await,
        events_before,
        "backfill emits nothing"
    );
    // Посчитанное превью совпадает с тем, что посчитал бы пересчёт: он ничего не меняет.
    assert_eq!(refresh(&pool, s.conversation_id).await, Refresh::Unchanged);
}
