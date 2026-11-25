//! Рутины: отчёты по расписанию из контекста существующей сессии через скрытую боковую копию.

use crate::support::task7::Fixture;

use axum::http::Method;
use canterlot_hub::{
    session::CAPABILITY_SIDE_CHAT_V1,
    testkit::{self, FakeSession},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn caps() -> Vec<&'static str> {
    vec![
        "pilot_v1",
        "instructions_v1",
        "session_tools_v1",
        "schedule_precheck_v1",
        CAPABILITY_SIDE_CHAT_V1,
        "run_outcome_v1",
    ]
}

fn session(f: &Fixture) -> FakeSession {
    testkit::fake_session(&f.state.registry, f.seed.machine_id, &caps(), true)
}

fn report_target(conversation_id: Uuid) -> Value {
    json!({"kind": "session_report", "conversation_id": conversation_id})
}

/// Родитель с нативной записью: есть что форкать.
async fn with_native(f: &Fixture) {
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state,native_conversation_id)
         values($1,$2,1,'stopped','parent-native')",
    )
    .bind(Uuid::now_v7())
    .bind(f.seed.conversation_id)
    .execute(&f.pool)
    .await
    .unwrap();
}

async fn post_schedule(f: &Fixture, target: Value) -> (u16, Value) {
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/schedules",
            Some(&format!("routine-{}", Uuid::now_v7())),
            Some(json!({
                "name": "Стендап",
                "cron": "0 9 * * 1-5",
                "target": target,
                "prompt": "Вчера сделано / Сегодня в планах / Блокеры",
            })),
        )
        .await;
    (r.status.as_u16(), r.json())
}

#[tokio::test]
async fn session_report_target_is_created_for_an_owned_session() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool).await;
    let _session = session(&f);
    let (status, body) = post_schedule(&f, report_target(f.seed.conversation_id)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["target"]["kind"], "session_report");
    assert_eq!(
        body["target"]["conversation_id"],
        f.seed.conversation_id.to_string()
    );
}

// ---------------------------------------------------------------------------
// Запуск отчёта: заявка тика, копия-читатель, карточка в ленте родителя.
// ---------------------------------------------------------------------------

use canterlot_hub::scheduler::Scheduler;
use chrono::{DateTime, TimeDelta, Utc};

const PROMPT: &str = "Вчера сделано / Сегодня в планах / Блокеры";

fn scheduler(f: &Fixture) -> Scheduler {
    Scheduler::new(
        f.pool.clone(),
        f.state.dispatcher.clone(),
        Default::default(),
    )
    .with_limits(f.state.registry.clone(), Default::default())
}

async fn make_schedule(f: &Fixture, name: &str) -> Uuid {
    let r = f
        .client
        .send(
            Method::POST,
            "/api/v1/schedules",
            Some(&format!("routine-{}", Uuid::now_v7())),
            Some(json!({
                "name": name,
                "cron": "0 9 * * 1-5",
                "target": report_target(f.seed.conversation_id),
                "prompt": PROMPT,
            })),
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    r.json::<Value>()["id"].as_str().unwrap().parse().unwrap()
}

async fn make_due(pool: &PgPool, schedule: Uuid, at: DateTime<Utc>) {
    sqlx::query("update schedules set next_fire_at=$2 where id=$1")
        .bind(schedule)
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
}

async fn latest_run(pool: &PgPool, schedule: Uuid) -> Uuid {
    sqlx::query_scalar(
        "select id from schedule_runs where schedule_id=$1 order by created_at desc limit 1",
    )
    .bind(schedule)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn run_state(pool: &PgPool, run: Uuid) -> (String, Value) {
    sqlx::query_as("select state,detail from schedule_runs where id=$1")
        .bind(run)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn wait_run(pool: &PgPool, run: Uuid, expected: &str) -> Value {
    for _ in 0..20_000 {
        let (state, detail) = run_state(pool, run).await;
        if state == expected {
            return detail;
        }
        tokio::task::yield_now().await;
    }
    let (state, detail) = run_state(pool, run).await;
    panic!("run never reached {expected}: {state}, {detail}");
}

/// Срабатывание расписания: тик заявляет запуск, приёмка доводит его до `started`.
async fn fire(f: &Fixture, schedule: Uuid) -> Uuid {
    let now = Utc::now();
    make_due(&f.pool, schedule, now - TimeDelta::seconds(30)).await;
    scheduler(f).tick(now).await.unwrap();
    let run = latest_run(&f.pool, schedule).await;
    wait_run(&f.pool, run, "started").await;
    run
}

async fn copies(pool: &PgPool, parent: Uuid) -> Vec<(Uuid, String)> {
    sqlx::query_as("select id, side_kind from conversations where side_of=$1 order by id")
        .bind(parent)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[derive(sqlx::FromRow)]
struct Card {
    id: Uuid,
    state: String,
    title: String,
    body: Option<String>,
    dismiss_reason: Option<String>,
    copy_id: Option<Uuid>,
}

async fn cards(pool: &PgPool, schedule: Uuid) -> Vec<Card> {
    sqlx::query_as(
        "select id,state,title,body,dismiss_reason,copy_id
           from routine_reports where schedule_id=$1 order by created_at,id",
    )
    .bind(schedule)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn open_cards(pool: &PgPool, schedule: Uuid) -> Vec<Card> {
    cards(pool, schedule)
        .await
        .into_iter()
        .filter(|card| card.dismiss_reason.is_none())
        .collect()
}

/// Запись хода копии в `runs`: `finished` — ход закончился штатно (`run_finished`),
/// `forced` — исполнение умерло посреди хода, и строку закрыл переход исполнения.
async fn record_copy_run(pool: &PgPool, copy: Uuid, finished: bool, forced: bool) {
    let execution: Uuid = sqlx::query_scalar(
        "select id from executions where conversation_id=$1 order by generation desc limit 1",
    )
    .bind(copy)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,provider,finished_at,
                          duration_ms,forced_close,outcome,last_block_seq)
         select i.command_id,$2,$1,'claude',case when $3 then clock_timestamp() end,
                case when $3 then 1000 end,$4,case when $3 and not $4 then 'completed' end,
                case when $3 then coalesce((select max(created_seq) from blocks
                                             where conversation_id=$1),0) end
           from inputs i where i.conversation_id=$1 and i.command_id is not null
         on conflict do nothing",
    )
    .bind(copy)
    .bind(execution)
    .bind(finished)
    .bind(forced)
    .execute(pool)
    .await
    .unwrap();
}

/// Копия отчёта в тесте: ход «закончился» ответом `answer` (как после `run_finished`).
async fn finish_copy_turn(pool: &PgPool, copy: Uuid, answer: &str) {
    let execution: Uuid = match sqlx::query_scalar(
        "select id from executions where conversation_id=$1 order by generation desc limit 1",
    )
    .bind(copy)
    .fetch_optional(pool)
    .await
    .unwrap()
    {
        Some(id) => id,
        None => {
            let id = Uuid::now_v7();
            sqlx::query("insert into executions(id,conversation_id,generation,state) values($1,$2,1,'running')")
                .bind(id)
                .bind(copy)
                .execute(pool)
                .await
                .unwrap();
            id
        }
    };
    sqlx::query("update inputs set state='accepted' where conversation_id=$1")
        .bind(copy)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "update queue_state set run_slot='free',run_slot_input_id=null,run_slot_attempt=null
          where conversation_id=$1",
    )
    .bind(copy)
    .execute(pool)
    .await
    .unwrap();
    // Промежуточная реплика и итоговый ответ: отчётом становится последний текст.
    for (seq, text) in [(1_i64, "Смотрю журнал…"), (2, answer)] {
        sqlx::query(
            "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,content,created_seq)
             values($1,$2,$3,'text','final',1,$4,$5)",
        )
        .bind(Uuid::now_v7())
        .bind(execution)
        .bind(copy)
        .bind(text)
        .bind(1000 + seq)
        .execute(pool)
        .await
        .unwrap();
    }
    record_copy_run(pool, copy, true, false).await;
}

async fn sweep(f: &Fixture) {
    scheduler(f).tick(Utc::now()).await.unwrap();
}

/// Воркер подтвердил остановку копии: исполнения закончились, строки можно убирать.
async fn worker_stops_copies(pool: &PgPool) {
    sqlx::query(
        "update executions set state='stopped'
          where state in ('created','starting','running','stopping')
            and conversation_id in (select id from conversations where side_kind='report')",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn run_makes_a_pending_card_then_the_report_then_deletes_the_copy() {
    let pool = canterlot_hub::testkit::db().await;
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    let run = fire(&f, schedule).await;

    // Идёт сбор: карточка уже в ленте, копия работает.
    let pending = open_cards(&pool, schedule).await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, "pending");
    assert!(
        pending[0].title.starts_with("Стендап · "),
        "{}",
        pending[0].title
    );
    let made = copies(&pool, f.seed.conversation_id).await;
    assert_eq!(made.len(), 1);
    assert_eq!(made[0].1, "report");
    let copy = made[0].0;
    let (_, detail) = run_state(&pool, run).await;
    assert_eq!(detail["report"]["state"], "pending", "{detail}");

    // Ход копии кончился: отчёт — её последний ответ.
    finish_copy_turn(&pool, copy, "Вчера: релиз. Сегодня: ревью. Блокеров нет.").await;
    sweep(&f).await;
    let ready = open_cards(&pool, schedule).await;
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id, pending[0].id);
    assert_eq!(ready[0].state, "ready");
    assert_eq!(
        ready[0].body.as_deref(),
        Some("Вчера: релиз. Сегодня: ревью. Блокеров нет.")
    );
    let (state, detail) = run_state(&pool, run).await;
    assert_eq!(state, "started");
    assert_eq!(detail["report"]["state"], "ready");
    assert_eq!(
        detail["report"]["text"],
        "Вчера: релиз. Сегодня: ревью. Блокеров нет."
    );

    // Копии заказана остановка; когда воркер её подтверждает, следующий проход удаляет строки.
    let stops: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id=$1 and kind='stop_execution'",
    )
    .bind(copy)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(stops >= 1, "copy stop was not ordered");
    worker_stops_copies(&pool).await;
    sweep(&f).await;
    assert!(copies(&pool, f.seed.conversation_id).await.is_empty());
    assert!(cards(&pool, schedule).await[0].copy_id.is_none());
}

// ---------------------------------------------------------------------------
// «Завершить»: идемпотентно, событием, текст остаётся в истории запусков.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Пуш «{имя} готов»: один на отчёт, по настройкам владельца, без пуша за ожидание.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Отказы: упавшая копия, нет нативной записи, пределы времени. Карточка с ошибкой
// и запуск `failed` с причиной; копия убирается.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Допуск: перекрытие и лимиты пропускают запуск, не оставляя ни карточки, ни копии.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Уборка: текст отчёта живёт в истории запусков 30 дней.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Агент родителя карточки не видит: она не ввод и не блок, в доставку не попадает.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Контракт событий: настоящие события карточки укладываются в events.schema.json.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Уборка копий: порядок замков, отказ по одной копии, потерянные исполнения.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Удаление родителя и пересечение запусков.
// ---------------------------------------------------------------------------
