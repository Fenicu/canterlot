//! Спасение ходов, зависших в генерации, на настоящем хабе с синтетическим
//! исполнением. Время задаётся явно через `now`, реальных пауз в тестах нет.
//! Все имена нейтральные — репозиторий публичный.

use std::time::Duration;

use canterlot_hub::{
    activity,
    delivery::{ContentBlock, SubmitInput, submit_input},
    testkit::{self, Seed},
};
use chrono::{TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn thresholds() -> activity::Thresholds {
    activity::Thresholds {
        stalled_generating: Duration::from_secs(1800),
        stalled_tool: Duration::from_secs(1800),
        hung_generating: Duration::from_secs(600),
        idle_quiet: Duration::from_secs(5),
        idle_background_max: Duration::from_secs(900),
    }
}

struct Scene {
    seed: Seed,
    execution: Uuid,
    now: chrono::DateTime<chrono::Utc>,
}

async fn hung_scene(pool: &PgPool, harness: &str, parent: bool) -> Scene {
    let seed = testkit::seed(pool).await;
    sqlx::query(
        "insert into push_settings(singleton,waiting,turn_finished,failures)
         values(true,true,true,true) on conflict(singleton) do nothing",
    )
    .execute(pool)
    .await
    .unwrap();
    let now = Utc::now();
    let execution = Uuid::now_v7();
    let ago = now - TimeDelta::minutes(11);
    if parent {
        let parent_seed = testkit::seed(pool).await;
        sqlx::query("update conversations set harness=$2,parent_conversation_id=$3 where id=$1")
            .bind(seed.conversation_id)
            .bind(harness)
            .bind(parent_seed.conversation_id)
            .execute(pool)
            .await
            .unwrap();
    } else {
        sqlx::query("update conversations set harness=$2 where id=$1")
            .bind(seed.conversation_id)
            .bind(harness)
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query("update machines set capabilities=array['pilot_v1','send_now_v1'],availability='available' where id=$1")
        .bind(seed.machine_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state,phase,phase_at,last_event_at)
         values($1,$2,1,'running','generating',$3,$3)",
    )
    .bind(execution)
    .bind(seed.conversation_id)
    .bind(ago)
    .execute(pool)
    .await
    .unwrap();
    let running_input = submit_input(
        pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: format!("running-{}", Uuid::now_v7()),
            content: vec![ContentBlock::Text {
                text: "neutral running turn".into(),
            }],
            received_at: ago,
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap()
    .input_id;
    sqlx::query("update inputs set state='dispatched' where id=$1")
        .bind(running_input)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2,
           run_slot_since=$3,hold_reasons='[]' where conversation_id=$1",
    )
    .bind(seed.conversation_id)
    .bind(running_input)
    .bind(ago)
    .execute(pool)
    .await
    .unwrap();
    metrics(pool, &seed, now, execution, true, 0, 0).await;
    Scene {
        seed,
        execution,
        now,
    }
}

async fn metrics(
    pool: &PgPool,
    seed: &Seed,
    at: chrono::DateTime<chrono::Utc>,
    execution: Uuid,
    alive: bool,
    cpu: i64,
    descendants: i64,
) {
    sqlx::query(
        "insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,
           mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions)
         values($1,$2,1,1,100,100,50,0,0,0,'[]',$3)
         on conflict(machine_id) do update set observed_at=excluded.observed_at,
           machine_time_ms=excluded.machine_time_ms,executions=excluded.executions",
    )
    .bind(seed.machine_id)
    .bind(at)
    .bind(json!([{
        "execution_id": execution,
        "alive": alive,
        "cpu_permille": cpu,
        "descendants": descendants
    }]))
    .execute(pool)
    .await
    .unwrap();
}

async fn queued_input(pool: &PgPool, scene: &Scene) -> Uuid {
    submit_input(
        pool,
        SubmitInput {
            owner_id: scene.seed.owner_id,
            conversation_id: scene.seed.conversation_id,
            idempotency_key: format!("fixture-{}", Uuid::now_v7()),
            content: vec![ContentBlock::Text {
                text: "neutral continuation".into(),
            }],
            received_at: scene.now,
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap()
    .input_id
}

async fn refresh(pool: &PgPool, scene: &Scene, at: chrono::DateTime<chrono::Utc>) {
    activity::refresh_with_metrics(
        pool,
        scene.seed.conversation_id,
        at,
        &thresholds(),
        Duration::from_secs(3600),
        true,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
}

async fn commands(pool: &PgPool, scene: &Scene) -> Vec<String> {
    sqlx::query_scalar("select kind from commands where conversation_id=$1 order by created_at,id")
        .bind(scene.seed.conversation_id)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn notes(pool: &PgPool, scene: &Scene) -> Vec<Value> {
    sqlx::query_scalar(
        "select origin from inputs where conversation_id=$1 and service_note
         order by input_seq",
    )
    .bind(scene.seed.conversation_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn opencode_queued_input_is_steered_once(pool: PgPool) {
    let scene = hung_scene(&pool, "opencode", true).await;
    let _input = queued_input(&pool, &scene).await;
    refresh(&pool, &scene, scene.now).await;
    refresh(&pool, &scene, scene.now + TimeDelta::seconds(1)).await;
    let steers: Vec<Uuid> = sqlx::query_scalar(
        "select id from commands where conversation_id=$1 and kind='steer_input'",
    )
    .bind(scene.seed.conversation_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(steers.len(), 1, "one steer per hung episode");
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(steers[0])
        .fetch_one(&pool)
        .await
        .unwrap();
    // steerering подтверждает тот самый ожидающий ввод (UUID в payload — строка).
    let steered_input: Uuid = payload["input_id"].as_str().unwrap().parse().unwrap();
    let steered_state: String = sqlx::query_scalar("select state from inputs where id=$1")
        .bind(steered_input)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        steered_state, "queued",
        "steered input stays queued until receipt"
    );
    let note_text: String = sqlx::query_scalar(
        "select content->0->>'text' from inputs where conversation_id=$1 and service_note
         order by input_seq desc limit 1",
    )
    .bind(scene.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(note_text, "Ход завис в генерации, влито и продолжено");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn claude_and_codex_queued_input_waits_for_the_interrupt(pool: PgPool) {
    for harness in ["claude_code", "codex"] {
        let scene = hung_scene(&pool, harness, true).await;
        let input = queued_input(&pool, &scene).await;
        refresh(&pool, &scene, scene.now).await;
        let kinds = commands(&pool, &scene).await;
        assert_eq!(
            kinds.iter().filter(|kind| *kind == "interrupt").count(),
            1,
            "{harness}"
        );
        assert!(!kinds.iter().any(|kind| kind == "steer_input"), "{harness}");
        let state: String = sqlx::query_scalar("select state from inputs where id=$1")
            .bind(input)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "queued", "the input is delivered as the next turn");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_nudged_session_is_interrupted_and_counted(pool: PgPool) {
    let scene = hung_scene(&pool, "codex", true).await;
    sqlx::query(
        "insert into nudge_rules(conversation_id,owner_id,set_by,state,consecutive,total)
         values($1,$2,'agent','watching',0,0)",
    )
    .bind(scene.seed.conversation_id)
    .bind(scene.seed.owner_id)
    .execute(&pool)
    .await
    .unwrap();
    refresh(&pool, &scene, scene.now).await;
    assert_eq!(
        commands(&pool, &scene)
            .await
            .iter()
            .filter(|kind| *kind == "interrupt")
            .count(),
        1
    );
    let nudge: Option<(String, Value)> = sqlx::query_as(
        "select content->0->>'text',origin from inputs where conversation_id=$1
         and origin->>'kind'='nudge'",
    )
    .bind(scene.seed.conversation_id)
    .fetch_optional(&pool)
    .await
    .unwrap();
    let (text, origin) = nudge.expect("a nudge was delivered");
    assert_eq!(
        text,
        "Previous step hung in generation and was interrupted; continue from where you were"
    );
    assert_eq!(origin["event"]["kind"], "hung");
    let total: i32 = sqlx::query_scalar("select total from nudge_rules where conversation_id=$1")
        .bind(scene.seed.conversation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(total, 1, "the rescue counts as a nudge");
    let events: i64 = sqlx::query_scalar(
        "select count(*) from nudge_events where conversation_id=$1 and outcome='nudged'",
    )
    .bind(scene.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_root_without_a_rule_gets_a_notice_and_one_push(pool: PgPool) {
    let scene = hung_scene(&pool, "opencode", false).await;
    refresh(&pool, &scene, scene.now).await;
    let harmful: Vec<String> = commands(&pool, &scene)
        .await
        .into_iter()
        .filter(|kind| kind == "interrupt" || kind == "steer_input")
        .collect();
    assert!(harmful.is_empty(), "no automatic interrupt: {harmful:?}");
    let notes = notes(&pool, &scene).await;
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["event"]["kind"], "turn_hung");
    assert_eq!(notes[0]["event"]["notice"], true);
    let pushes: i64 = sqlx::query_scalar(
        "select count(*) from owner_pushes where conversation_id=$1 and dedupe_key like 'hung:%'",
    )
    .bind(scene.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pushes, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_second_hang_in_the_same_turn_only_notifies(pool: PgPool) {
    let scene = hung_scene(&pool, "opencode", true).await;
    let _ = queued_input(&pool, &scene).await;
    refresh(&pool, &scene, scene.now).await;
    // Новое потоковое событие закрывает эпизод, но не меняет ход.
    let resumed = scene.now + TimeDelta::minutes(1);
    sqlx::query("update executions set last_event_at=$2 where id=$1")
        .bind(scene.execution)
        .bind(resumed)
        .execute(&pool)
        .await
        .unwrap();
    refresh(&pool, &scene, resumed).await;
    let second = resumed + TimeDelta::minutes(11);
    refresh(&pool, &scene, second).await;
    refresh(&pool, &scene, second + TimeDelta::seconds(1)).await;
    assert_eq!(
        commands(&pool, &scene)
            .await
            .iter()
            .filter(|kind| *kind == "steer_input")
            .count(),
        1,
        "no second automatic rescue"
    );
    let second_push: i64 = sqlx::query_scalar(
        "select count(*) from owner_pushes where conversation_id=$1
         and dedupe_key like 'hung:%second'",
    )
    .bind(scene.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(second_push, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_running_tool_with_a_long_silence_is_not_rescued(pool: PgPool) {
    let scene = hung_scene(&pool, "opencode", true).await;
    sqlx::query("update executions set phase='running_tool' where id=$1")
        .bind(scene.execution)
        .execute(&pool)
        .await
        .unwrap();
    let late = scene.now + TimeDelta::hours(2);
    metrics(&pool, &scene.seed, late, scene.execution, true, 0, 0).await;
    refresh(&pool, &scene, late).await;
    let harmful: Vec<String> = commands(&pool, &scene)
        .await
        .into_iter()
        .filter(|kind| kind == "interrupt" || kind == "steer_input")
        .collect();
    assert!(harmful.is_empty());
    assert!(notes(&pool, &scene).await.is_empty());
    let evidence: Value = sqlx::query_scalar(
        "select activity_evidence from conversation_derived where conversation_id=$1",
    )
    .bind(scene.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(evidence["kind"], "stalled");
}
