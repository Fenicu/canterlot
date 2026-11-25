//! Спасение ходов, зависших в генерации, на настоящем хабе с синтетическим
//! исполнением. Время задаётся явно через `now`, реальных пауз в тестах нет.
//! Все имена нейтральные — репозиторий публичный.

use std::time::Duration;

use canterlot_hub::{
    activity,
    config::HungRescue,
    delivery::{ContentBlock, SubmitInput, submit_input},
    testkit::{self, Seed},
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn thresholds() -> activity::Thresholds {
    activity::Thresholds {
        stalled_generating: Duration::from_secs(1800),
        stalled_tool: Duration::from_secs(1800),
        hung_generating: Duration::from_secs(600),
        hung_rescue: canterlot_hub::config::HungRescue::Auto,
        idle_quiet: Duration::from_secs(5),
        idle_background_max: Duration::from_secs(900),
    }
}

struct Scene {
    seed: Seed,
    execution: Uuid,
    /// `command_id` ввода, который идёт в слоте: имя хода.
    turn: Uuid,
    base: DateTime<Utc>,
}

async fn hung_scene(pool: &PgPool, harness: &str, with_parent: bool) -> Scene {
    let seed = testkit::seed(pool).await;
    hung_scene_for(pool, harness, with_parent, seed).await
}

async fn hung_scene_for(pool: &PgPool, harness: &str, with_parent: bool, seed: Seed) -> Scene {
    sqlx::query(
        "insert into push_settings(singleton,waiting,turn_finished,failures)
         values(true,true,true,true) on conflict(singleton) do nothing",
    )
    .execute(pool)
    .await
    .unwrap();
    let base = Utc::now();
    let execution = Uuid::now_v7();
    let ago = base - TimeDelta::minutes(11);
    if with_parent {
        let parent = testkit::seed_conversation(pool, seed.owner_id, seed.machine_id).await;
        sqlx::query("update conversations set parent_conversation_id=$2,spawn_depth=1 where id=$1")
            .bind(seed.conversation_id)
            .bind(parent)
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query("update conversations set harness=$2 where id=$1")
        .bind(seed.conversation_id)
        .bind(harness)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "update machines set capabilities=array['pilot_v1','send_now_v1','guarded_interrupt_v1'],
           availability='available',availability_at=$2 where id=$1",
    )
    .bind(seed.machine_id)
    .bind(base - TimeDelta::hours(1))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state,phase,phase_at,last_event_at,created_at)
         values($1,$2,1,'running','generating',$3,$3,$4)",
    )
    .bind(execution)
    .bind(seed.conversation_id)
    .bind(ago)
    .bind(base - TimeDelta::hours(1))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into journal_cursors(machine_id,journal_id,kind,committed_seq,execution_id)
         values($1,$2,'sj',41,$3)",
    )
    .bind(seed.machine_id)
    .bind(Uuid::now_v7())
    .bind(execution)
    .execute(pool)
    .await
    .unwrap();
    let running = submit_input(
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
    .unwrap();
    sqlx::query("update inputs set state='dispatched' where id=$1")
        .bind(running.input_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2,
           run_slot_since=$3,hold_reasons='[]' where conversation_id=$1",
    )
    .bind(seed.conversation_id)
    .bind(running.input_id)
    .bind(ago)
    .execute(pool)
    .await
    .unwrap();
    Scene {
        seed,
        execution,
        turn: running.command_id,
        base,
    }
}

/// Выборка метрик машины в момент `at`; `valid` — у CLI есть прошлый замер CPU.
async fn sample(pool: &PgPool, scene: &Scene, at: DateTime<Utc>, valid: bool, cpu: i64) {
    sqlx::query(
        "insert into machine_metrics(machine_id,observed_at,machine_time_ms,cpu_cores,cpu_permille,
           mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions)
         values($1,$2,1,1,100,100,50,0,0,0,'[]',$3)
         on conflict(machine_id) do update set observed_at=excluded.observed_at,
           machine_time_ms=excluded.machine_time_ms,executions=excluded.executions",
    )
    .bind(scene.seed.machine_id)
    .bind(at)
    .bind(json!([{
        "execution_id": scene.execution,
        "alive": true,
        "cpu_permille": cpu,
        "descendants": 0,
        "cpu_valid": valid,
        "cli_alive": true
    }]))
    .execute(pool)
    .await
    .unwrap();
}

async fn refresh_with(pool: &PgPool, scene: &Scene, at: DateTime<Utc>, mode: HungRescue) {
    let mut thresholds = thresholds();
    thresholds.hung_rescue = mode;
    activity::refresh_with_metrics(
        pool,
        scene.seed.conversation_id,
        at,
        &thresholds,
        Duration::from_secs(3600),
        true,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
}

/// Выборка с доказательным CPU и пересчёт в тот же момент.
async fn tick(pool: &PgPool, scene: &Scene, at: DateTime<Utc>) {
    sample(pool, scene, at, true, 0).await;
    refresh_with(pool, scene, at, HungRescue::Auto).await;
}

async fn warm(pool: &PgPool, scene: &Scene) {
    tick(pool, scene, scene.base - TimeDelta::seconds(15)).await;
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
            received_at: scene.base,
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap()
    .input_id
}

async fn commands_of(pool: &PgPool, scene: &Scene, kind: &str) -> Vec<(Uuid, Value)> {
    sqlx::query_as(
        "select id,payload from commands where conversation_id=$1 and kind=$2 order by created_at,id",
    )
    .bind(scene.seed.conversation_id)
    .bind(kind)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn harmful(pool: &PgPool, scene: &Scene) -> usize {
    commands_of(pool, scene, "interrupt").await.len()
        + commands_of(pool, scene, "steer_input").await.len()
}

async fn notes(pool: &PgPool, scene: &Scene) -> Vec<(String, Value)> {
    sqlx::query_as(
        "select content->0->>'text',origin from inputs where conversation_id=$1 and service_note
         order by input_seq",
    )
    .bind(scene.seed.conversation_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn pushes(pool: &PgPool, scene: &Scene) -> Vec<(String, Value)> {
    sqlx::query_as(
        "select dedupe_key,payload from owner_pushes where conversation_id=$1 order by created_at",
    )
    .bind(scene.seed.conversation_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn confirm_interrupt(pool: &PgPool, command: Uuid, status: &str) {
    let status = status.to_owned();
    canterlot_hub::db::in_tx::<_, sqlx::Error, _>(pool, move |conn| {
        Box::pin(async move {
            canterlot_hub::rescue::on_interrupt_status_in_tx(
                conn,
                command,
                &status,
                Utc::now(),
                Uuid::now_v7(),
            )
            .await
        })
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn claude_and_codex_queued_input_gets_a_guarded_interrupt_for_this_turn() {
    let pool = canterlot_hub::testkit::db().await;
    for harness in ["claude_code", "codex"] {
        let scene = hung_scene(&pool, harness, true).await;
        let input = queued_input(&pool, &scene).await;
        warm(&pool, &scene).await;
        assert_eq!(harmful(&pool, &scene).await, 0, "one sample is no verdict");
        tick(&pool, &scene, scene.base).await;
        let interrupts = commands_of(&pool, &scene, "interrupt").await;
        assert_eq!(interrupts.len(), 1, "{harness}");
        let payload = &interrupts[0].1;
        assert_eq!(payload["expected_turn_id"], scene.turn.to_string());
        assert_eq!(payload["progress_checkpoint"], 41);
        assert!(commands_of(&pool, &scene, "steer_input").await.is_empty());
        let state: String = sqlx::query_scalar("select state from inputs where id=$1")
            .bind(input)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "queued", "the input is delivered as the next turn");
        // Пока воркер не подтвердил, спасённым ход не считается: строки в ленте нет.
        assert!(
            notes(&pool, &scene)
                .await
                .iter()
                .all(|(text, _)| !text.contains("прерван и продолжен"))
        );
    }
}

#[tokio::test]
async fn opencode_and_alert_mode_only_show_a_card_and_push() {
    let pool = canterlot_hub::testkit::db().await;
    let opencode = hung_scene(&pool, "opencode", true).await;
    warm(&pool, &opencode).await;
    tick(&pool, &opencode, opencode.base).await;
    assert_eq!(harmful(&pool, &opencode).await, 0, "no reliable evidence");
    let notes_now = notes(&pool, &opencode).await;
    assert_eq!(notes_now.len(), 1);
    assert_eq!(notes_now[0].1["event"]["turn_hung_notice"], true);
    assert_eq!(pushes(&pool, &opencode).await.len(), 1);

    let claude = hung_scene(&pool, "claude_code", true).await;
    sample(
        &pool,
        &claude,
        claude.base - TimeDelta::seconds(15),
        true,
        0,
    )
    .await;
    refresh_with(
        &pool,
        &claude,
        claude.base - TimeDelta::seconds(15),
        HungRescue::Alert,
    )
    .await;
    sample(&pool, &claude, claude.base, true, 0).await;
    refresh_with(&pool, &claude, claude.base, HungRescue::Alert).await;
    assert_eq!(harmful(&pool, &claude).await, 0, "alert never interrupts");
    assert_eq!(pushes(&pool, &claude).await.len(), 1);

    let off = hung_scene(&pool, "claude_code", true).await;
    sample(&pool, &off, off.base - TimeDelta::seconds(15), true, 0).await;
    refresh_with(
        &pool,
        &off,
        off.base - TimeDelta::seconds(15),
        HungRescue::Off,
    )
    .await;
    sample(&pool, &off, off.base, true, 0).await;
    refresh_with(&pool, &off, off.base, HungRescue::Off).await;
    assert_eq!(harmful(&pool, &off).await, 0);
    assert!(notes(&pool, &off).await.is_empty());
}

/// План «прервать и подтолкнуть» ставит в очередь ввод «ход был прерван». Воркер в
/// прерывании отказал: ход ожил сам. Толчок обязан уйти из очереди — иначе агент после
/// хода получит неправду и сделает лишний ход, — а попытка возвращается правилу.
#[tokio::test]
async fn a_refused_rescue_interrupt_withdraws_the_queued_nudge() {
    let pool = canterlot_hub::testkit::db().await;
    let scene = hung_scene(&pool, "codex", true).await;
    sqlx::query(
        "insert into nudge_rules(conversation_id,owner_id,set_by,state,consecutive,total)
         values($1,$2,'owner','watching',0,0)",
    )
    .bind(scene.seed.conversation_id)
    .bind(scene.seed.owner_id)
    .execute(&pool)
    .await
    .unwrap();
    warm(&pool, &scene).await;
    tick(&pool, &scene, scene.base).await;
    let nudges = |pool: PgPool| async move {
        sqlx::query_as::<_, (Uuid, String)>(
            "select id, state from inputs
             where conversation_id=$1 and origin->>'kind'='nudge' order by input_seq",
        )
        .bind(scene.seed.conversation_id)
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    let counters = |pool: PgPool| async move {
        sqlx::query_as::<_, (i32, i32)>(
            "select consecutive,total from nudge_rules where conversation_id=$1",
        )
        .bind(scene.seed.conversation_id)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let queued = nudges(pool.clone()).await;
    assert_eq!(queued.len(), 1, "{queued:?}");
    assert_eq!(queued[0].1, "queued");
    assert_eq!(counters(pool.clone()).await, (1, 1));

    let command = commands_of(&pool, &scene, "interrupt").await[0].0;
    confirm_interrupt(&pool, command, "turn_mismatch").await;

    assert_eq!(
        nudges(pool.clone()).await,
        [(queued[0].0, "cancelled".to_owned())],
        "the nudge about an interrupt that never happened is withdrawn"
    );
    assert_eq!(
        counters(pool.clone()).await,
        (0, 0),
        "a withdrawn nudge is not counted against the rule"
    );
    let cancelled: i64 = sqlx::query_scalar(
        "select count(*) from events where conversation_id=$1 and kind='input_state_changed'
           and payload->>'input_id'=$2 and payload->>'state'='cancelled'",
    )
    .bind(scene.seed.conversation_id)
    .bind(queued[0].0.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cancelled, 1, "the feed learns that the input is gone");
}
