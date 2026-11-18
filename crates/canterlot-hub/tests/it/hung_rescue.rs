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
use sha2::Digest as _;
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
    parent: Option<Uuid>,
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
    let parent = if with_parent {
        let parent = testkit::seed_conversation(pool, seed.owner_id, seed.machine_id).await;
        sqlx::query("update conversations set parent_conversation_id=$2,spawn_depth=1 where id=$1")
            .bind(seed.conversation_id)
            .bind(parent)
            .execute(pool)
            .await
            .unwrap();
        Some(parent)
    } else {
        None
    };
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
        parent,
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

/// Первая выборка окна: сама по себе verdict не даёт.
/// Выборка и пересчёт в заданном режиме.
async fn tick_mode(pool: &PgPool, scene: &Scene, at: DateTime<Utc>, mode: HungRescue) {
    sample(pool, scene, at, true, 0).await;
    refresh_with(pool, scene, at, mode).await;
}

/// Новый прогресс потока: событие исполнения в момент `at`.
async fn progress(pool: &PgPool, scene: &Scene, at: DateTime<Utc>) {
    sqlx::query("update executions set last_event_at=$2 where id=$1")
        .bind(scene.execution)
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
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

async fn parent_jobs(pool: &PgPool, scene: &Scene) -> Vec<String> {
    let Some(parent) = scene.parent else {
        return Vec::new();
    };
    sqlx::query_scalar(
        "select payload->>'text' from scheduled_jobs where conversation_id=$1
         and payload->>'action'='note' order by created_at",
    )
    .bind(parent)
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn claude_and_codex_queued_input_gets_a_guarded_interrupt_for_this_turn(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn opencode_and_alert_mode_only_show_a_card_and_push(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_machine_without_the_guarded_interrupt_gets_a_card_not_an_interrupt(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", true).await;
    sqlx::query("update machines set capabilities=array['pilot_v1'] where id=$1")
        .bind(scene.seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    warm(&pool, &scene).await;
    tick(&pool, &scene, scene.base).await;
    assert_eq!(harmful(&pool, &scene).await, 0);
    assert_eq!(notes(&pool, &scene).await.len(), 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn waits_and_holds_are_never_rescued(pool: PgPool) {
    let waiting = hung_scene(&pool, "claude_code", true).await;
    sqlx::query("update executions set phase='awaiting_permission' where id=$1")
        .bind(waiting.execution)
        .execute(&pool)
        .await
        .unwrap();
    warm(&pool, &waiting).await;
    tick(&pool, &waiting, waiting.base).await;
    assert_eq!(
        harmful(&pool, &waiting).await,
        0,
        "a permission wait is not a hang"
    );

    let held = hung_scene(&pool, "claude_code", true).await;
    sqlx::query(r#"update queue_state set hold_reasons='[{"reason":"no_live_execution"}]' where conversation_id=$1"#)
        .bind(held.seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    warm(&pool, &held).await;
    tick(&pool, &held, held.base).await;
    assert_eq!(harmful(&pool, &held).await, 0);
    assert!(notes(&pool, &held).await.is_empty());

    let idle = hung_scene(&pool, "claude_code", true).await;
    sqlx::query(
        "update queue_state set run_slot='free',run_slot_input_id=null where conversation_id=$1",
    )
    .bind(idle.seed.conversation_id)
    .execute(&pool)
    .await
    .unwrap();
    warm(&pool, &idle).await;
    tick(&pool, &idle, idle.base).await;
    assert_eq!(
        harmful(&pool, &idle).await,
        0,
        "an idle delegate gets nothing"
    );
    assert!(parent_jobs(&pool, &idle).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_verdict_waits_for_two_valid_samples_after_a_reconnect(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", true).await;
    let at = |seconds: i64| scene.base + TimeDelta::seconds(seconds);
    // Выборка без прошлого замера CPU ничего не доказывает, сколько бы их ни было.
    for seconds in [-30, -20, -10] {
        sample(&pool, &scene, at(seconds), false, 0).await;
        refresh_with(&pool, &scene, at(seconds), HungRescue::Auto).await;
    }
    assert_eq!(harmful(&pool, &scene).await, 0);
    tick(&pool, &scene, at(0)).await;
    assert_eq!(
        harmful(&pool, &scene).await,
        0,
        "one valid sample is not a window"
    );
    // Переподключение воркера: связь вернулась позже выборок, окно начинается заново.
    sqlx::query("update machines set availability_at=$2 where id=$1")
        .bind(scene.seed.machine_id)
        .bind(at(5))
        .execute(&pool)
        .await
        .unwrap();
    tick(&pool, &scene, at(10)).await;
    assert_eq!(
        harmful(&pool, &scene).await,
        0,
        "the first sample after the reconnect"
    );
    tick(&pool, &scene, at(20)).await;
    assert_eq!(
        harmful(&pool, &scene).await,
        1,
        "two consecutive valid samples"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn interrupt_and_nudge_keys_belong_to_the_turn(pool: PgPool) {
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
    let interrupts = commands_of(&pool, &scene, "interrupt").await;
    assert_eq!(interrupts.len(), 1);
    let nudge_keys: Vec<String> = sqlx::query_scalar(
        "select idempotency_key from inputs where conversation_id=$1 and origin->>'kind'='nudge'",
    )
    .bind(scene.seed.conversation_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(nudge_keys.len(), 1);
    assert!(
        nudge_keys[0].contains(&scene.turn.to_string()),
        "{nudge_keys:?}"
    );
    // Следующий ход: новый ввод в слоте и новая тишина.
    confirm_interrupt(&pool, interrupts[0].0, "completed").await;
    // Первый толчок дошёл до агента и продолжил ход; следующий ход идёт без очереди.
    sqlx::query(
        "update inputs set state='accepted' where conversation_id=$1 and origin->>'kind'='nudge'",
    )
    .bind(scene.seed.conversation_id)
    .execute(&pool)
    .await
    .unwrap();
    let next = queued_input(&pool, &scene).await;
    let next_command: Uuid = sqlx::query_scalar("select command_id from inputs where id=$1")
        .bind(next)
        .fetch_one(&pool)
        .await
        .unwrap();
    let later = scene.base + TimeDelta::hours(1);
    sqlx::query("update inputs set state='dispatched' where id=$1")
        .bind(next)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2,run_slot_since=$3
         where conversation_id=$1",
    )
    .bind(scene.seed.conversation_id)
    .bind(next)
    .bind(scene.base + TimeDelta::minutes(1))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("update executions set last_event_at=$2,phase_at=$2 where id=$1")
        .bind(scene.execution)
        .bind(scene.base + TimeDelta::minutes(1))
        .execute(&pool)
        .await
        .unwrap();
    tick(&pool, &scene, later - TimeDelta::seconds(10)).await;
    tick(&pool, &scene, later).await;
    let interrupts = commands_of(&pool, &scene, "interrupt").await;
    assert_eq!(
        interrupts.len(),
        2,
        "the second turn gets its own interrupt"
    );
    assert_eq!(
        interrupts[1].1["expected_turn_id"],
        next_command.to_string()
    );
    let nudges: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and origin->>'kind'='nudge'",
    )
    .bind(scene.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(nudges, 2, "a nudge per turn, not a replay of the old key");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_exhausted_nudge_rule_is_not_bypassed_and_nothing_is_half_done(pool: PgPool) {
    let scene = hung_scene(&pool, "codex", true).await;
    sqlx::query(
        "insert into nudge_rules(conversation_id,owner_id,set_by,state,consecutive,total,max_consecutive)
         values($1,$2,'owner','watching',3,3,3)",
    )
    .bind(scene.seed.conversation_id)
    .bind(scene.seed.owner_id)
    .execute(&pool)
    .await
    .unwrap();
    warm(&pool, &scene).await;
    tick(&pool, &scene, scene.base).await;
    assert_eq!(
        harmful(&pool, &scene).await,
        0,
        "no interrupt without the continuation"
    );
    let nudges: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and origin->>'kind'='nudge'",
    )
    .bind(scene.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(nudges, 0);
    let (state, total): (String, i32) =
        sqlx::query_as("select state,total from nudge_rules where conversation_id=$1")
            .bind(scene.seed.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((state.as_str(), total), ("exhausted", 3));
    assert_eq!(
        notes(&pool, &scene).await.len(),
        1,
        "the owner gets the card"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_nudge_commits_interrupt_input_and_counters_together(pool: PgPool) {
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
    assert_eq!(commands_of(&pool, &scene, "interrupt").await.len(), 1);
    let (consecutive, total): (i32, i32) =
        sqlx::query_as("select consecutive,total from nudge_rules where conversation_id=$1")
            .bind(scene.seed.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((consecutive, total), (1, 1));
    let events: i64 = sqlx::query_scalar(
        "select count(*) from nudge_events where conversation_id=$1 and outcome='nudged'",
    )
    .bind(scene.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 1);
    // Живого исполнения нет: прерывание невозможно, и ввод с счётчиками откатываются вместе с ним.
    let broken = hung_scene(&pool, "codex", true).await;
    sqlx::query(
        "insert into nudge_rules(conversation_id,owner_id,set_by,state,consecutive,total)
         values($1,$2,'owner','watching',0,0)",
    )
    .bind(broken.seed.conversation_id)
    .bind(broken.seed.owner_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("update conversations set trashed_at=now() where id=$1")
        .bind(broken.seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    warm(&pool, &broken).await;
    tick(&pool, &broken, broken.base).await;
    let (consecutive, total): (i32, i32) =
        sqlx::query_as("select consecutive,total from nudge_rules where conversation_id=$1")
            .bind(broken.seed.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (consecutive, total),
        (0, 0),
        "counters are not left half-committed"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn only_newer_progress_closes_an_episode_and_a_cpu_spike_does_not(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", false).await;
    warm(&pool, &scene).await;
    tick(&pool, &scene, scene.base).await;
    assert_eq!(pushes(&pool, &scene).await.len(), 1, "the first hang");
    // Всплеск CPU и провал связи без нового события потока эпизода не закрывают.
    sample(
        &pool,
        &scene,
        scene.base + TimeDelta::seconds(10),
        true,
        400,
    )
    .await;
    refresh_with(
        &pool,
        &scene,
        scene.base + TimeDelta::seconds(10),
        HungRescue::Auto,
    )
    .await;
    sqlx::query("update machines set availability='degraded' where id=$1")
        .bind(scene.seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    refresh_with(
        &pool,
        &scene,
        scene.base + TimeDelta::seconds(20),
        HungRescue::Auto,
    )
    .await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(scene.seed.machine_id)
        .execute(&pool)
        .await
        .unwrap();
    tick(&pool, &scene, scene.base + TimeDelta::seconds(30)).await;
    tick(&pool, &scene, scene.base + TimeDelta::seconds(40)).await;
    assert_eq!(
        pushes(&pool, &scene).await.len(),
        1,
        "no invented second hang"
    );
    // Новый прогресс закрывает эпизод; следующая тишина — второе зависание хода.
    let resumed = scene.base + TimeDelta::minutes(1);
    sqlx::query("update executions set last_event_at=$2 where id=$1")
        .bind(scene.execution)
        .bind(resumed)
        .execute(&pool)
        .await
        .unwrap();
    tick(&pool, &scene, resumed).await;
    let second = resumed + TimeDelta::minutes(11);
    tick(&pool, &scene, second - TimeDelta::seconds(10)).await;
    tick(&pool, &scene, second).await;
    let all = pushes(&pool, &scene).await;
    assert_eq!(all.len(), 2, "{all:?}");
    assert!(all[1].0.ends_with(":second"));
    assert_eq!(all[1].1["evidence"]["second"], true);
    // Без новых событий потока третьего уведомления нет.
    tick(&pool, &scene, second + TimeDelta::seconds(30)).await;
    assert_eq!(pushes(&pool, &scene).await.len(), 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_hang_has_one_push_with_hang_text_scoped_to_the_turn(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", false).await;
    warm(&pool, &scene).await;
    tick(&pool, &scene, scene.base).await;
    let all = pushes(&pool, &scene).await;
    assert_eq!(
        all.len(),
        1,
        "no second push from the stalled path: {all:?}"
    );
    assert!(all[0].0.starts_with("hung:"), "{}", all[0].0);
    assert!(all[0].0.contains(&scene.turn.to_string()));
    assert_eq!(all[0].1["state"], "hung");
    let text = canterlot_hub::notify::failure_text(
        all[0].1["state"].as_str().unwrap(),
        &all[0].1["evidence"],
        None,
    );
    assert_eq!(text, "Ход завис в генерации");
    assert!(!text.contains("упала"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_rescue_is_confirmed_by_the_worker_receipt_and_a_refusal_is_visible(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", true).await;
    warm(&pool, &scene).await;
    tick(&pool, &scene, scene.base).await;
    let command = commands_of(&pool, &scene, "interrupt").await[0].0;
    let (pending, kind): (Option<Uuid>, Option<String>) = sqlx::query_as(
        "select pending_command_id,pending_kind from hung_rescues where conversation_id=$1",
    )
    .bind(scene.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (pending, kind.as_deref()),
        (Some(command), Some("interrupt"))
    );
    assert!(
        parent_jobs(&pool, &scene).await.is_empty(),
        "nothing before the receipt"
    );
    confirm_interrupt(&pool, command, "turn_mismatch").await;
    let all = notes(&pool, &scene).await;
    assert!(
        all.iter()
            .any(|(text, _)| text.contains("прерывание не принято")),
        "{all:?}"
    );
    assert!(
        all.iter()
            .all(|(text, _)| !text.contains("прерван и продолжен")),
        "a refusal is not a rescue"
    );
    assert!(parent_jobs(&pool, &scene).await.is_empty());
    // Отказ уведомляет владельца один раз и не открывает второй эпизод в той же тишине.
    tick(&pool, &scene, scene.base + TimeDelta::seconds(10)).await;
    assert_eq!(commands_of(&pool, &scene, "interrupt").await.len(), 1);
    assert_eq!(pushes(&pool, &scene).await.len(), 1);
    assert!(pushes(&pool, &scene).await[0].0.ends_with(":first"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_parent_gets_a_recovery_note_only_when_nothing_continues_the_session(pool: PgPool) {
    // Без ввода и без правила: родитель получает заметку после квитанции.
    let bare = hung_scene(&pool, "claude_code", true).await;
    warm(&pool, &bare).await;
    tick(&pool, &bare, bare.base).await;
    let command = commands_of(&pool, &bare, "interrupt").await[0].0;
    assert!(parent_jobs(&pool, &bare).await.is_empty());
    confirm_interrupt(&pool, command, "completed").await;
    refresh_with(
        &pool,
        &bare,
        bare.base + TimeDelta::seconds(5),
        HungRescue::Auto,
    )
    .await;
    let jobs = parent_jobs(&pool, &bare).await;
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert!(jobs[0].contains("continue it from where it stopped"));
    // С ожидающим вводом продолжение уже есть.
    let queued = hung_scene(&pool, "claude_code", true).await;
    let _ = queued_input(&pool, &queued).await;
    warm(&pool, &queued).await;
    tick(&pool, &queued, queued.base).await;
    let command = commands_of(&pool, &queued, "interrupt").await[0].0;
    confirm_interrupt(&pool, command, "completed").await;
    refresh_with(
        &pool,
        &queued,
        queued.base + TimeDelta::seconds(5),
        HungRescue::Auto,
    )
    .await;
    assert!(parent_jobs(&pool, &queued).await.is_empty());
    // Второе зависание говорит, что ход всё ещё завис.
    let resumed = bare.base + TimeDelta::minutes(1);
    sqlx::query("update executions set last_event_at=$2 where id=$1")
        .bind(bare.execution)
        .bind(resumed)
        .execute(&pool)
        .await
        .unwrap();
    tick(&pool, &bare, resumed).await;
    let second = resumed + TimeDelta::minutes(11);
    tick(&pool, &bare, second - TimeDelta::seconds(10)).await;
    tick(&pool, &bare, second).await;
    let jobs = parent_jobs(&pool, &bare).await;
    assert_eq!(jobs.len(), 2, "{jobs:?}");
    assert!(jobs[1].contains("still hung"), "{}", jobs[1]);
}

/// Удаление родителя держит его строку и отвязывает детей; спасение ребёнка не должно
/// ждать эту строку под своей блокировкой, иначе две транзакции ждут друг друга.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn rescue_does_not_deadlock_with_a_concurrent_parent_delete(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", true).await;
    let parent = scene.parent.unwrap();
    warm(&pool, &scene).await;
    let mut deleting = pool.begin().await.unwrap();
    sqlx::query("select id from conversations where id=$1 for update")
        .bind(parent)
        .fetch_one(&mut *deleting)
        .await
        .unwrap();
    let rescue = {
        let pool = pool.clone();
        let base = scene.base;
        let conversation = scene.seed.conversation_id;
        tokio::spawn(async move {
            activity::refresh_with_metrics(
                &pool,
                conversation,
                base,
                &thresholds(),
                Duration::from_secs(3600),
                true,
                Uuid::now_v7(),
            )
            .await
        })
    };
    sample(&pool, &scene, scene.base, true, 0).await;
    // Спасение либо уже закончилось, либо ждёт чужую блокировку: тогда в `pg_stat_activity`
    // виден ожидающий бэкенд. Удаление, как в `deletion.rs`, отвязывает ребёнка только
    // после этого.
    let mut rescue = rescue;
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "select count(*) from pg_stat_activity where wait_event_type='Lock'
               and datname=current_database()",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        if waiting > 0 || rescue.is_finished() {
            break;
        }
        tokio::task::yield_now().await;
    }
    sqlx::query("update conversations set parent_conversation_id=null where id=$1")
        .bind(scene.seed.conversation_id)
        .execute(&mut *deleting)
        .await
        .expect("the delete is not deadlocked by the rescue");
    deleting.commit().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), &mut rescue)
        .await
        .expect("the rescue finishes")
        .unwrap();
    result.expect("the rescue is not a deadlock victim");
}

/// Родитель занят удалением: заметка остаётся записью и уходит, когда строка свободна.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_locked_parent_defers_the_note_instead_of_waiting(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", true).await;
    let parent = scene.parent.unwrap();
    warm(&pool, &scene).await;
    tick(&pool, &scene, scene.base).await;
    let command = commands_of(&pool, &scene, "interrupt").await[0].0;
    let mut deleting = pool.begin().await.unwrap();
    sqlx::query("select id from conversations where id=$1 for update")
        .bind(parent)
        .fetch_one(&mut *deleting)
        .await
        .unwrap();
    confirm_interrupt(&pool, command, "completed").await;
    let flushed = tokio::time::timeout(
        Duration::from_secs(5),
        canterlot_hub::rescue::flush_parent_note(&pool, scene.seed.conversation_id),
    )
    .await
    .expect("the flush does not wait for the parent row")
    .unwrap();
    assert!(!flushed);
    assert!(parent_jobs(&pool, &scene).await.is_empty());
    deleting.rollback().await.unwrap();
    assert!(
        canterlot_hub::rescue::flush_parent_note(&pool, scene.seed.conversation_id)
            .await
            .unwrap()
    );
    assert_eq!(parent_jobs(&pool, &scene).await.len(), 1);
}

async fn http_scene(
    pool: &PgPool,
) -> (
    testkit::TestHub,
    testkit::TestClient,
    Scene,
    canterlot_hub::session::TestLease,
) {
    http_scene_with(pool, &["pilot_v1", "send_now_v1", "guarded_interrupt_v1"]).await
}

/// Сцена с HTTP и живым воркером с заданным набором возможностей.
async fn http_scene_with(
    pool: &PgPool,
    capabilities: &[&str],
) -> (
    testkit::TestHub,
    testkit::TestClient,
    Scene,
    canterlot_hub::session::TestLease,
) {
    // Фоновый проектор хаба в этих тестах только наблюдает (`off`): решения о зависании
    // принимают явные пересчёты теста с заданным моментом, а не гонка с часами проектора.
    let hub = testkit::spawn_test_hub_with(
        pool.clone(),
        testkit::TestHubOptions {
            http: Some(testkit::test_http_config()),
            activity: canterlot_hub::config::ActivityConfig {
                hung_rescue: HungRescue::Off,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    let owner = hub.installation.owner_id;
    let machine = canterlot_hub::machines::add(pool, owner, "synthetic")
        .await
        .unwrap();
    // Машина подключена по-настоящему (действующий токен), и фоновая проекция доступности
    // записывает `available` до сцены (явным вызовом проекции): после этого она ничего не меняет, а значит,
    // не сдвигает начало связи и не сбрасывает окно наблюдения посреди проверки.
    let credential = sha2::Sha256::digest(machine.token.as_bytes()).to_vec();
    let lease = hub.registry.connect_for_tests_with_credential(
        machine.machine_id,
        &capabilities
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect::<Vec<_>>(),
        credential,
    );
    canterlot_hub::machine_info::project_availability(pool, &hub.registry, Uuid::now_v7())
        .await
        .unwrap();
    let settled: String = sqlx::query_scalar("select availability from machines where id=$1")
        .bind(machine.machine_id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(settled, "available");
    let conversation = testkit::seed_conversation(pool, owner, machine.machine_id).await;
    let seed = Seed {
        owner_id: owner,
        machine_id: machine.machine_id,
        conversation_id: conversation,
    };
    let scene = hung_scene_for(pool, "opencode", false, seed).await;
    let client = testkit::TestClient::over_socket(hub.http_addr())
        .await
        .as_owner();
    (hub, client, scene, lease)
}

/// Две доказательные выборки после старта фонового проектора хаба, по часам теста.
/// Машина сцены подключена по-настоящему (см. [`http_scene_with`]), поэтому фоновая
/// проекция доступности ей ничего не меняет и окно наблюдения не сбрасывает.
async fn two_ticks_after_projector_start(pool: &PgPool, scene: &Scene) {
    crate::support::projector_running(pool).await;
    for _ in 0..2 {
        tick(pool, scene, Utc::now()).await;
    }
}

async fn episode_of(pool: &PgPool, scene: &Scene) -> Uuid {
    sqlx::query_scalar("select episode_id from hung_rescues where conversation_id=$1")
        .bind(scene.seed.conversation_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Кнопки карточки привязаны к эпизоду: пока он открыт, действуют; закрытая карточка
/// (действие выполнено, ход сменился) отвечает отказом и ничего не прерывает.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn card_actions_are_bound_to_their_episode(pool: PgPool) {
    use axum::http::{Method, StatusCode};
    let (hub, client, scene, _lease) = http_scene(&pool).await;
    // Отметки сразу после старта фонового проектора, но уже в прошлом его часов.
    two_ticks_after_projector_start(&pool, &scene).await;
    let episode = episode_of(&pool, &scene).await;
    let base = format!(
        "/api/v1/conversations/{}/hung-episodes/{episode}",
        scene.seed.conversation_id
    );
    let state: Value = client.send(Method::GET, &base, None, None).await.json();
    assert_eq!(state["state"], "open");
    // Чужой или выдуманный эпизод не действует.
    let wrong = format!(
        "/api/v1/conversations/{}/hung-episodes/{}/interrupt",
        scene.seed.conversation_id,
        Uuid::now_v7()
    );
    let refused = client
        .send(Method::POST, &wrong, Some("wrong-1"), None)
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(harmful(&pool, &scene).await, 0);
    // Действие по открытой карточке.
    let done = client
        .send(
            Method::POST,
            &format!("{base}/interrupt"),
            Some("card-1"),
            None,
        )
        .await;
    assert_eq!(done.status, StatusCode::OK, "{:?}", done.body);
    assert_eq!(commands_of(&pool, &scene, "interrupt").await.len(), 1);
    let state: Value = client.send(Method::GET, &base, None, None).await.json();
    assert_eq!(state["state"], "owner_interrupt");
    let again = client
        .send(
            Method::POST,
            &format!("{base}/interrupt"),
            Some("card-2"),
            None,
        )
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    let body: Value = again.json();
    assert_eq!(
        body["error"]["details"]["reason"], "hung_episode_resolved",
        "{body}"
    );
    assert_eq!(commands_of(&pool, &scene, "interrupt").await.len(), 1);
    drop(hub);
}

/// Вчерашняя карточка не прервёт сегодняшний ход.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_old_card_cannot_stop_a_later_turn(pool: PgPool) {
    use axum::http::{Method, StatusCode};
    let (hub, client, scene, _lease) = http_scene(&pool).await;
    // Отметки сразу после старта фонового проектора, но уже в прошлом его часов.
    two_ticks_after_projector_start(&pool, &scene).await;
    let episode = episode_of(&pool, &scene).await;
    // Ход закончился, пошёл другой.
    let next = queued_input(&pool, &scene).await;
    sqlx::query("update inputs set state='dispatched' where id=$1")
        .bind(next)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2 where conversation_id=$1",
    )
    .bind(scene.seed.conversation_id)
    .bind(next)
    .execute(&pool)
    .await
    .unwrap();
    let path = format!(
        "/api/v1/conversations/{}/hung-episodes/{episode}",
        scene.seed.conversation_id
    );
    let state: Value = client.send(Method::GET, &path, None, None).await.json();
    // Закрыла ли эпизод сама проверка запроса или фоновый пересчёт, зависит от порядка;
    // важно, что открытым он не остаётся.
    assert!(
        matches!(state["state"].as_str(), Some("turn_ended" | "progress")),
        "{state}"
    );
    let response = client
        .send(
            Method::POST,
            &format!("{path}/interrupt"),
            Some("old-card"),
            None,
        )
        .await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    let response = client
        .send(
            Method::POST,
            &format!("{path}/steer"),
            Some("old-card-steer"),
            None,
        )
        .await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(harmful(&pool, &scene).await, 0);
    drop(hub);
}

/// После старта хаба молчание нельзя оценивать по выборкам до него: нужны две новые.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn samples_from_before_the_hub_started_do_not_count(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", true).await;
    activity::mark_projector_started(
        &activity::database_of(&pool),
        scene.base + TimeDelta::seconds(5),
    );
    tick(&pool, &scene, scene.base - TimeDelta::seconds(10)).await;
    tick(&pool, &scene, scene.base).await;
    assert_eq!(
        harmful(&pool, &scene).await,
        0,
        "both samples predate the hub start"
    );
    tick(&pool, &scene, scene.base + TimeDelta::seconds(10)).await;
    assert_eq!(
        harmful(&pool, &scene).await,
        0,
        "one fresh sample is not a window"
    );
    tick(&pool, &scene, scene.base + TimeDelta::seconds(20)).await;
    assert_eq!(harmful(&pool, &scene).await, 1);
}

/// Режим `alert` ни при каких обстоятельствах сам не создаёт ввод ни в одной сессии:
/// ни заметки родителю, ни толчка, ни вливания, ни прерывания. Второе зависание того
/// же хода (после прогресса) тоже только карточка и пуш.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn alert_mode_never_creates_an_input_for_any_session(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", true).await;
    let parent = scene.parent.unwrap();
    // Родитель простаивает: заметка разбудила бы его новым ходом.
    let inputs_before: i64 =
        sqlx::query_scalar("select count(*) from inputs where conversation_id=$1")
            .bind(parent)
            .fetch_one(&pool)
            .await
            .unwrap();
    tick_mode(
        &pool,
        &scene,
        scene.base - TimeDelta::seconds(15),
        HungRescue::Alert,
    )
    .await;
    tick_mode(&pool, &scene, scene.base, HungRescue::Alert).await;
    assert_eq!(pushes(&pool, &scene).await.len(), 1, "the first alert");
    // Ребёнок продолжил и завис снова.
    let resumed = scene.base + TimeDelta::minutes(1);
    progress(&pool, &scene, resumed).await;
    tick_mode(&pool, &scene, resumed, HungRescue::Alert).await;
    let second = resumed + TimeDelta::minutes(11);
    tick_mode(
        &pool,
        &scene,
        second - TimeDelta::seconds(10),
        HungRescue::Alert,
    )
    .await;
    tick_mode(&pool, &scene, second, HungRescue::Alert).await;
    let all = pushes(&pool, &scene).await;
    assert_eq!(
        all.len(),
        2,
        "the second hang still alerts the owner: {all:?}"
    );
    assert_eq!(harmful(&pool, &scene).await, 0);
    assert!(
        parent_jobs(&pool, &scene).await.is_empty(),
        "no parent note job"
    );
    let pending_note: i64 = sqlx::query_scalar(
        "select count(*) from hung_rescues where conversation_id=$1 and parent_note is not null",
    )
    .bind(scene.seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending_note, 0);
    let inputs_after: i64 =
        sqlx::query_scalar("select count(*) from inputs where conversation_id=$1")
            .bind(parent)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(inputs_after, inputs_before, "an idle parent is not woken");
    let rescued: bool =
        sqlx::query_scalar("select rescued from hung_rescues where conversation_id=$1")
            .bind(scene.seed.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!rescued, "an alert is not an automatic rescue");
}

/// Один непрерывный зависший ход шлёт владельцу один пуш: обрыв связи в той же тишине
/// и обычный `stalled` после переподключения второго пуша не дают.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn one_continuous_hang_sends_one_push_across_a_reconnect(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", false).await;
    tick_mode(
        &pool,
        &scene,
        scene.base - TimeDelta::seconds(15),
        HungRescue::Alert,
    )
    .await;
    tick_mode(&pool, &scene, scene.base, HungRescue::Alert).await;
    assert_eq!(pushes(&pool, &scene).await.len(), 1);
    // Связь пропала на полчаса той же тишины и вернулась: окно наблюдения греется заново.
    sqlx::query("update machines set availability='unavailable',availability_at=$2 where id=$1")
        .bind(scene.seed.machine_id)
        .bind(scene.base + TimeDelta::minutes(5))
        .execute(&pool)
        .await
        .unwrap();
    refresh_with(
        &pool,
        &scene,
        scene.base + TimeDelta::minutes(30),
        HungRescue::Alert,
    )
    .await;
    sqlx::query("update machines set availability='available',availability_at=$2 where id=$1")
        .bind(scene.seed.machine_id)
        .bind(scene.base + TimeDelta::minutes(31))
        .execute(&pool)
        .await
        .unwrap();
    tick_mode(
        &pool,
        &scene,
        scene.base + TimeDelta::minutes(32),
        HungRescue::Alert,
    )
    .await;
    let all = pushes(&pool, &scene).await;
    assert_eq!(
        all.len(),
        1,
        "no generic stalled push for the same silence: {all:?}"
    );
    tick_mode(
        &pool,
        &scene,
        scene.base + TimeDelta::minutes(33),
        HungRescue::Alert,
    )
    .await;
    assert_eq!(pushes(&pool, &scene).await.len(), 1);
}

/// Отказ воркера не открывает второй эпизод без нового прогресса.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_refusal_does_not_open_a_second_episode_without_progress(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", true).await;
    warm(&pool, &scene).await;
    tick(&pool, &scene, scene.base).await;
    let command = commands_of(&pool, &scene, "interrupt").await[0].0;
    confirm_interrupt(&pool, command, "turn_mismatch").await;
    let episode = episode_of(&pool, &scene).await;
    for step in [10, 20, 30] {
        tick(&pool, &scene, scene.base + TimeDelta::seconds(step)).await;
    }
    assert_eq!(episode_of(&pool, &scene).await, episode, "same episode");
    assert_eq!(commands_of(&pool, &scene, "interrupt").await.len(), 1);
    let all = pushes(&pool, &scene).await;
    assert_eq!(all.len(), 1, "one push for the whole silence: {all:?}");
    // Новый прогресс закрывает эпизод; следующая тишина — новая.
    let resumed = scene.base + TimeDelta::minutes(2);
    progress(&pool, &scene, resumed).await;
    tick(&pool, &scene, resumed).await;
    let second = resumed + TimeDelta::minutes(11);
    tick(&pool, &scene, second - TimeDelta::seconds(10)).await;
    tick(&pool, &scene, second).await;
    assert_ne!(episode_of(&pool, &scene).await, episode);
    assert_eq!(pushes(&pool, &scene).await.len(), 2);
}

/// Между записью прогресса в журнал и закрытием эпизода проектором карточка ещё «открыта»:
/// кнопки отказывают по сохранённой отметке эпизода и не читают новый курсор журнала.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_card_of_an_advanced_episode_is_refused_before_the_projector_closes_it(pool: PgPool) {
    use axum::http::{Method, StatusCode};
    let (hub, client, scene, _lease) = http_scene(&pool).await;
    let queued = queued_input(&pool, &scene).await;
    two_ticks_after_projector_start(&pool, &scene).await;
    let episode = episode_of(&pool, &scene).await;
    let base = format!(
        "/api/v1/conversations/{}/hung-episodes/{episode}",
        scene.seed.conversation_id
    );
    // Журнал ушёл вперёд (ход ожил), проектор эпизод ещё не закрыл.
    sqlx::query("update journal_cursors set committed_seq=committed_seq+3 where execution_id=$1")
        .bind(scene.execution)
        .execute(&pool)
        .await
        .unwrap();
    let state: Value = client.send(Method::GET, &base, None, None).await.json();
    assert_eq!(state["state"], "open", "the projector has not run yet");
    for (path, key) in [("interrupt", "adv-int"), ("steer", "adv-steer")] {
        let response = client
            .send(Method::POST, &format!("{base}/{path}"), Some(key), None)
            .await;
        assert_eq!(response.status, StatusCode::CONFLICT, "{path}");
        let body: Value = response.json();
        assert_eq!(
            body["error"]["details"]["reason"], "hung_episode_resolved",
            "{path}: {body}"
        );
    }
    assert_eq!(harmful(&pool, &scene).await, 0);
    let state: String = sqlx::query_scalar("select state from inputs where id=$1")
        .bind(queued)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "queued");
    drop(hub);
}

/// «Влить» привязана к вводу карточки: если его отменили, новый ввод в голове очереди
/// не подменяет его.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn steer_is_bound_to_the_cards_input_and_never_picks_a_new_head(pool: PgPool) {
    use axum::http::{Method, StatusCode};
    let (hub, client, scene, _lease) = http_scene(&pool).await;
    let original = queued_input(&pool, &scene).await;
    two_ticks_after_projector_start(&pool, &scene).await;
    let episode = episode_of(&pool, &scene).await;
    let path = format!(
        "/api/v1/conversations/{}/hung-episodes/{episode}/steer",
        scene.seed.conversation_id
    );
    sqlx::query("update inputs set state='cancelled' where id=$1")
        .bind(original)
        .execute(&pool)
        .await
        .unwrap();
    let newer = queued_input(&pool, &scene).await;
    let response = client.send(Method::POST, &path, Some("gone-1"), None).await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    let body: Value = response.json();
    assert_eq!(
        body["error"]["details"]["reason"], "input_not_queued",
        "{body}"
    );
    assert_eq!(
        harmful(&pool, &scene).await,
        0,
        "the new head was not steered"
    );
    let state: String = sqlx::query_scalar("select state from inputs where id=$1")
        .bind(newer)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "queued");
    drop(hub);
}

/// Воркер без охранного interrupt (или сопровождение, не понимающее охрану, — оно
/// отвечает отказом на стороне воркера): карточка не прерывает вслепую.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_card_never_interrupts_through_a_worker_without_the_guard(pool: PgPool) {
    use axum::http::{Method, StatusCode};
    let (hub, client, scene, _lease) = http_scene_with(&pool, &["pilot_v1", "send_now_v1"]).await;
    two_ticks_after_projector_start(&pool, &scene).await;
    let episode = episode_of(&pool, &scene).await;
    let response = client
        .send(
            Method::POST,
            &format!(
                "/api/v1/conversations/{}/hung-episodes/{episode}/interrupt",
                scene.seed.conversation_id
            ),
            Some("legacy-1"),
            None,
        )
        .await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    let body: Value = response.json();
    assert_eq!(
        body["error"]["details"]["reason"], "capability_missing",
        "{body}"
    );
    assert_eq!(harmful(&pool, &scene).await, 0);
    // Карточка осталась открытой: обычное «Прервать» в чате у владельца есть всегда.
    let state: Value = client
        .send(
            Method::GET,
            &format!(
                "/api/v1/conversations/{}/hung-episodes/{episode}",
                scene.seed.conversation_id
            ),
            None,
            None,
        )
        .await
        .json();
    assert_eq!(state["state"], "open");
    drop(hub);
}

/// Пересчёт, чьи часы отстали от уже записанной выборки (он взял `now` до ожидания блокировки
/// очереди, а за это время пришла новая выборка), не видит её и не должен стирать окно
/// наблюдения: это отсутствие данных у самого пересчёта, а не доказательство тишины процесса.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_refresh_with_a_stale_clock_does_not_erase_the_observation_window(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", true).await;
    let samples = |pool: PgPool, scene: Uuid| async move {
        sqlx::query_scalar::<_, i32>(
            "select obs_samples from hung_rescues where conversation_id=$1",
        )
        .bind(scene)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let conversation = scene.seed.conversation_id;
    tick(&pool, &scene, scene.base - TimeDelta::seconds(15)).await;
    assert_eq!(samples(pool.clone(), conversation).await, 1);
    // Новая выборка уже в базе, а пересчёт считает по часам, которые её ещё не достигли.
    sample(&pool, &scene, scene.base, true, 0).await;
    refresh_with(
        &pool,
        &scene,
        scene.base - TimeDelta::seconds(5),
        HungRescue::Auto,
    )
    .await;
    assert_eq!(
        samples(pool.clone(), conversation).await,
        1,
        "the stale refresh kept the window"
    );
    refresh_with(&pool, &scene, scene.base, HungRescue::Auto).await;
    assert_eq!(samples(pool.clone(), conversation).await, 2);
    assert_eq!(harmful(&pool, &scene).await, 1, "the window completed");
}

/// Карточка «Ход завис» в режиме alert нужна только владельцу: она видна в ленте, но
/// следующий выданный ввод агента её текст не несёт.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_alert_card_never_reaches_the_agents_next_prompt(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", false).await;
    tick_mode(
        &pool,
        &scene,
        scene.base - TimeDelta::seconds(15),
        HungRescue::Alert,
    )
    .await;
    tick_mode(&pool, &scene, scene.base, HungRescue::Alert).await;
    let card = notes(&pool, &scene).await;
    assert_eq!(card.len(), 1, "the card is on the timeline");
    let hang_text = card[0].0.clone();
    assert!(!hang_text.is_empty());

    // Ход закончился, владелец пишет следующее сообщение.
    sqlx::query("update inputs set state='accepted' where conversation_id=$1 and not service_note")
        .bind(scene.seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "update queue_state set run_slot='free',run_slot_input_id=null,run_slot_since=now()
         where conversation_id=$1",
    )
    .bind(scene.seed.conversation_id)
    .execute(&pool)
    .await
    .unwrap();
    submit_input(
        &pool,
        SubmitInput {
            owner_id: scene.seed.owner_id,
            conversation_id: scene.seed.conversation_id,
            idempotency_key: "owner:after-card".to_owned(),
            content: vec![ContentBlock::Text {
                text: "neutral owner message".into(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    let outcome = canterlot_hub::delivery::dispatch::dispatch_next(
        &pool,
        scene.seed.conversation_id,
        canterlot_hub::delivery::dispatch::GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: Vec::new(),
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let canterlot_hub::delivery::dispatch::DispatchOutcome::Dispatched { command_id, .. } = outcome
    else {
        panic!("the owner message did not dispatch: {outcome:?}");
    };
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let content = payload["content"].to_string();
    assert!(content.contains("neutral owner message"), "{content}");
    assert!(
        !content.contains(&hang_text),
        "the card text must not reach the agent: {content}"
    );
    assert_eq!(payload["content"].as_array().unwrap().len(), 1, "{content}");
    assert_eq!(
        notes(&pool, &scene).await.len(),
        1,
        "the card stays visible"
    );
}

/// Тишина длиннее порога общего `stalled`: ход молчит `minutes` минут к моменту `base`.
async fn silent_for(pool: &PgPool, scene: &Scene, minutes: i64) {
    let since = scene.base - TimeDelta::minutes(minutes);
    sqlx::query("update executions set phase_at=$2,last_event_at=$2 where id=$1")
        .bind(scene.execution)
        .bind(since)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("update queue_state set run_slot_since=$2 where conversation_id=$1")
        .bind(scene.seed.conversation_id)
        .bind(since)
        .execute(pool)
        .await
        .unwrap();
}

/// Общий `stalled` пришёл раньше (окно наблюдения ещё грелось), потом вынесен verdict
/// «завис»: на одну непрерывную тишину один пуш, карточка при этом появляется.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_generic_stalled_push_first_suppresses_the_hang_push_of_the_same_silence(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", false).await;
    silent_for(&pool, &scene, 40).await;
    tick_mode(
        &pool,
        &scene,
        scene.base - TimeDelta::seconds(15),
        HungRescue::Alert,
    )
    .await;
    let first = pushes(&pool, &scene).await;
    assert_eq!(first.len(), 1, "the generic push goes first: {first:?}");
    assert!(!first[0].0.starts_with("hung:"), "{}", first[0].0);
    tick_mode(&pool, &scene, scene.base, HungRescue::Alert).await;
    let all = pushes(&pool, &scene).await;
    assert_eq!(all.len(), 1, "no second push for one silence: {all:?}");
    assert_eq!(
        notes(&pool, &scene).await.len(),
        1,
        "the card still appears"
    );
    tick_mode(
        &pool,
        &scene,
        scene.base + TimeDelta::minutes(1),
        HungRescue::Alert,
    )
    .await;
    assert_eq!(pushes(&pool, &scene).await.len(), 1);

    // Ход ожил и замолчал снова: это уже другая тишина и свой пуш.
    let resumed = scene.base + TimeDelta::minutes(2);
    progress(&pool, &scene, resumed).await;
    tick_mode(&pool, &scene, resumed, HungRescue::Alert).await;
    let later = resumed + TimeDelta::minutes(11);
    tick_mode(
        &pool,
        &scene,
        later - TimeDelta::seconds(10),
        HungRescue::Alert,
    )
    .await;
    tick_mode(&pool, &scene, later, HungRescue::Alert).await;
    assert_eq!(
        pushes(&pool, &scene).await.len(),
        2,
        "a new silence after progress alerts again"
    );
}

/// Verdict «завис» пришёл раньше, потом связь пропала и общий `stalled` перешёл порог
/// той же тишины: второго пуша нет.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_hang_push_first_suppresses_the_generic_stalled_push_of_the_same_silence(pool: PgPool) {
    let scene = hung_scene(&pool, "claude_code", false).await;
    silent_for(&pool, &scene, 25).await;
    tick_mode(
        &pool,
        &scene,
        scene.base - TimeDelta::seconds(15),
        HungRescue::Alert,
    )
    .await;
    tick_mode(&pool, &scene, scene.base, HungRescue::Alert).await;
    let first = pushes(&pool, &scene).await;
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0].0.starts_with("hung:"), "{}", first[0].0);
    sqlx::query("update machines set availability='unavailable',availability_at=$2 where id=$1")
        .bind(scene.seed.machine_id)
        .bind(scene.base + TimeDelta::minutes(1))
        .execute(&pool)
        .await
        .unwrap();
    refresh_with(
        &pool,
        &scene,
        scene.base + TimeDelta::minutes(10),
        HungRescue::Alert,
    )
    .await;
    sqlx::query("update machines set availability='available',availability_at=$2 where id=$1")
        .bind(scene.seed.machine_id)
        .bind(scene.base + TimeDelta::minutes(11))
        .execute(&pool)
        .await
        .unwrap();
    tick_mode(
        &pool,
        &scene,
        scene.base + TimeDelta::minutes(12),
        HungRescue::Alert,
    )
    .await;
    assert_eq!(
        pushes(&pool, &scene).await.len(),
        1,
        "silence crossed the generic threshold"
    );
}
