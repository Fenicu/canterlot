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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn session_report_target_is_created_for_an_owned_session(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn session_report_target_validation(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);

    // Несуществующий разговор.
    let (status, body) = post_schedule(&f, report_target(Uuid::now_v7())).await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(body["error"]["code"], "schedule_invalid");

    // Разговор в корзине считается несуществующим.
    let trashed = testkit::seed_conversation(&pool, f.seed.owner_id, f.seed.machine_id).await;
    sqlx::query("update conversations set trashed_at=now() where id=$1")
        .bind(trashed)
        .execute(&pool)
        .await
        .unwrap();
    let (status, _) = post_schedule(&f, report_target(trashed)).await;
    assert_eq!(status, 422);

    // Чужой разговор.
    let other_owner = Uuid::now_v7();
    sqlx::query("insert into owners(id) values($1)")
        .bind(other_owner)
        .execute(&pool)
        .await
        .unwrap();
    let foreign = testkit::seed_conversation(&pool, other_owner, f.seed.machine_id).await;
    let (status, _) = post_schedule(&f, report_target(foreign)).await;
    assert_eq!(status, 422);

    // Сама боковая копия не может быть источником.
    let copy = testkit::seed_conversation(&pool, f.seed.owner_id, f.seed.machine_id).await;
    sqlx::query("update conversations set side_of=$2 where id=$1")
        .bind(copy)
        .bind(f.seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    let (status, _) = post_schedule(&f, report_target(copy)).await;
    assert_eq!(status, 422);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn session_report_target_requires_side_chat_capability(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let _session = testkit::fake_session(
        &f.state.registry,
        f.seed.machine_id,
        &["pilot_v1", "instructions_v1"],
        true,
    );
    let (status, body) = post_schedule(&f, report_target(f.seed.conversation_id)).await;
    assert_eq!(status, 422, "{body}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn patch_to_session_report_target_is_validated_too(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let _session = session(&f);
    let (status, body) = post_schedule(&f, report_target(f.seed.conversation_id)).await;
    assert_eq!(status, 200);
    let id = body["id"].as_str().unwrap();
    let revision = body["revision"].as_i64().unwrap();
    let r = f
        .client
        .send(
            Method::PATCH,
            &format!("/api/v1/schedules/{id}"),
            Some(&format!("routine-patch-{}", Uuid::now_v7())),
            Some(json!({"base_revision": revision, "target": report_target(Uuid::now_v7())})),
        )
        .await;
    assert_eq!(r.status.as_u16(), 422);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn report_copy_and_interactive_side_coexist_under_one_parent(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    // Интерактивный боковой чат владельца.
    let r = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/side", f.seed.conversation_id),
            None,
            None,
        )
        .await;
    assert_eq!(r.status.as_u16(), 201);
    let chat: Uuid = r.json::<Value>()["conversation_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    // Копия отчёта живёт рядом: у неё своя метка, и одного бокового чата на родителя это не нарушает.
    let copy = testkit::seed_conversation(&pool, f.seed.owner_id, f.seed.machine_id).await;
    sqlx::query("update conversations set side_of=$2, side_kind='report' where id=$1")
        .bind(copy)
        .bind(f.seed.conversation_id)
        .execute(&pool)
        .await
        .unwrap();
    // Повтор создания возвращает интерактивный чат, а не копию отчёта.
    let r = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{}/side", f.seed.conversation_id),
            None,
            None,
        )
        .await;
    assert_eq!(r.status.as_u16(), 200);
    assert_eq!(
        r.json::<Value>()["conversation_id"].as_str().unwrap(),
        chat.to_string()
    );
    // Удаление интерактивного чата не трогает копию отчёта.
    let r = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{}/side", f.seed.conversation_id),
            None,
            None,
        )
        .await;
    assert_eq!(r.status.as_u16(), 204);
    let alive: bool = sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
        .bind(copy)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        alive,
        "interactive side removal must not delete the report copy"
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

/// Запуск вручную («Запустить сейчас»): в обход проверки пересечения.
async fn run_now(f: &Fixture, schedule: Uuid) -> Uuid {
    let r = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/schedules/{schedule}/run"),
            Some(&format!("routine-run-{}", Uuid::now_v7())),
            Some(json!({})),
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
    error: Option<String>,
    dismiss_reason: Option<String>,
    copy_id: Option<Uuid>,
}

async fn cards(pool: &PgPool, schedule: Uuid) -> Vec<Card> {
    sqlx::query_as(
        "select id,state,title,body,error,dismiss_reason,copy_id
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
                          duration_ms,forced_close,outcome)
         select i.command_id,$2,$1,'claude',case when $3 then clock_timestamp() end,
                case when $3 then 1000 end,$4,case when $3 and not $4 then 'completed' end
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_makes_a_pending_card_then_the_report_then_deletes_the_copy(pool: PgPool) {
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

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_copy_is_a_read_only_forked_report_copy_with_the_prompt(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    fire(&f, schedule).await;
    let (copy, kind) = copies(&pool, f.seed.conversation_id).await.remove(0);
    assert_eq!(kind, "report");
    let (mode, parent_conversation): (String, Option<Uuid>) = sqlx::query_as(
        "select permission_mode,parent_conversation_id from conversations where id=$1",
    )
    .bind(copy)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mode, "read_only");
    assert!(parent_conversation.is_none());
    // Промпт расписания — единственный ввод копии.
    let texts: Vec<Value> =
        sqlx::query_scalar("select content from inputs where conversation_id=$1")
            .bind(copy)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(texts.len(), 1);
    assert_eq!(texts[0][0]["text"], PROMPT);
    // Старт исполнения — форк от нативной записи родителя через боковой MCP.
    let payload: Value = sqlx::query_scalar(
        "select payload from commands where conversation_id=$1 and kind='start_execution'",
    )
    .bind(copy)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(payload["fork_from_native"], "parent-native");
    assert_eq!(payload["canterlot_mcp"], "side");
    assert_eq!(payload["isolate_mcp"], true);
    // Копия не видна в списках и счётчиках.
    let counts: Value = f.get("/api/v1/snapshot/conversations").await["data"]["counts"].clone();
    assert_eq!(counts["all"], 1, "{counts}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_parent_is_not_touched_by_a_report_run(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    async fn fingerprint(pool: &PgPool, parent: Uuid) -> Value {
        sqlx::query_scalar(
            "select jsonb_build_object(
               'inputs',(select count(*) from inputs where conversation_id=$1),
               'blocks',(select count(*) from blocks where conversation_id=$1),
               'commands',(select count(*) from commands where conversation_id=$1),
               'executions',(select count(*) from executions where conversation_id=$1),
               'jobs',(select count(*) from scheduled_jobs where conversation_id=$1),
               'queue',(select to_jsonb(q) - 'updated_at' from queue_state q where conversation_id=$1),
               'derived',(select to_jsonb(d) from conversation_derived d where conversation_id=$1),
               'conversation',(select to_jsonb(c) - 'updated_at' from conversations c where id=$1))",
        )
        .bind(parent)
        .fetch_one(pool)
        .await
        .unwrap()
    }
    let before = fingerprint(&pool, parent).await;
    let schedule = make_schedule(&f, "Стендап").await;
    fire(&f, schedule).await;
    let copy = copies(&pool, parent).await.remove(0).0;
    finish_copy_turn(&pool, copy, "Готово").await;
    sweep(&f).await;
    worker_stops_copies(&pool).await;
    sweep(&f).await;
    assert_eq!(fingerprint(&pool, parent).await, before);
    // Событий разговора родителя, кроме карточки отчёта, нет.
    let kinds: Vec<String> =
        sqlx::query_scalar("select kind from events where scope = $1 order by scope_seq")
            .bind(format!("conversation:{parent}"))
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        kinds.iter().all(|kind| kind.starts_with("routine_report_")),
        "{kinds:?}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_interactive_side_chat_is_not_reused_replaced_or_disturbed(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    let r = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/conversations/{parent}/side"),
            None,
            None,
        )
        .await;
    assert_eq!(r.status.as_u16(), 201);
    let chat: Uuid = r.json::<Value>()["conversation_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let chat_before: Value =
        sqlx::query_scalar("select to_jsonb(c) - 'updated_at' from conversations c where id=$1")
            .bind(chat)
            .fetch_one(&pool)
            .await
            .unwrap();

    let schedule = make_schedule(&f, "Стендап").await;
    fire(&f, schedule).await;
    let all = copies(&pool, parent).await;
    assert_eq!(all.len(), 2);
    let report = all.iter().find(|(_, kind)| kind == "report").unwrap().0;
    assert_ne!(report, chat);
    // В чате владельца нет ни ввода расписания, ни запуска.
    let inputs: i64 = sqlx::query_scalar("select count(*) from inputs where conversation_id=$1")
        .bind(chat)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(inputs, 0);
    finish_copy_turn(&pool, report, "Отчёт").await;
    sweep(&f).await;
    worker_stops_copies(&pool).await;
    sweep(&f).await;
    // Копия отчёта ушла, чат владельца на месте и не изменился.
    let left = copies(&pool, parent).await;
    assert_eq!(left, vec![(chat, "chat".to_owned())]);
    let chat_after: Value =
        sqlx::query_scalar("select to_jsonb(c) - 'updated_at' from conversations c where id=$1")
            .bind(chat)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(chat_after, chat_before);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_new_report_replaces_the_undismissed_card_of_the_same_schedule(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    let first = fire(&f, schedule).await;
    let first_copy = copies(&pool, f.seed.conversation_id).await.remove(0).0;
    finish_copy_turn(&pool, first_copy, "Первый отчёт").await;
    sweep(&f).await;
    worker_stops_copies(&pool).await;
    sweep(&f).await;
    assert_eq!(open_cards(&pool, schedule).await[0].state, "ready");

    let second = run_now(&f, schedule).await;
    assert_ne!(first, second);
    wait_run(&pool, second, "started").await;
    let all = cards(&pool, schedule).await;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].dismiss_reason.as_deref(), Some("replaced"));
    assert_eq!(all[1].dismiss_reason, None);
    assert_eq!(all[1].state, "pending");
    // Текст прежнего отчёта остался в истории запусков.
    let (_, detail) = run_state(&pool, first).await;
    assert_eq!(detail["report"]["text"], "Первый отчёт");
    // На каждый момент у расписания не больше одной открытой карточки.
    assert_eq!(open_cards(&pool, schedule).await.len(), 1);
}

// ---------------------------------------------------------------------------
// «Завершить»: идемпотентно, событием, текст остаётся в истории запусков.
// ---------------------------------------------------------------------------

fn dismiss_path(parent: Uuid, report: Uuid) -> String {
    format!("/api/v1/conversations/{parent}/routine-reports/{report}/dismiss")
}

async fn card_events(pool: &PgPool, parent: Uuid, kind: &str) -> Vec<Value> {
    sqlx::query_scalar("select payload from events where scope=$1 and kind=$2 order by scope_seq")
        .bind(format!("conversation:{parent}"))
        .bind(kind)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Готовая карточка: запуск, ответ копии, один проход сбора.
async fn ready_card(f: &Fixture, schedule: Uuid, answer: &str) -> (Uuid, Uuid) {
    let run = fire(f, schedule).await;
    let copy = copies(&f.pool, f.seed.conversation_id).await.remove(0).0;
    finish_copy_turn(&f.pool, copy, answer).await;
    sweep(f).await;
    (run, open_cards(&f.pool, schedule).await.remove(0).id)
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn snapshot_carries_the_open_card_and_events_follow_its_life(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    let schedule = make_schedule(&f, "Стендап").await;
    let run = fire(&f, schedule).await;
    let snapshot = f
        .get(&format!("/api/v1/conversations/{parent}/snapshot"))
        .await;
    let items = snapshot["data"]["routine_reports"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{snapshot}");
    assert_eq!(items[0]["state"], "pending");
    assert_eq!(items[0]["schedule_id"], schedule.to_string());
    assert_eq!(items[0]["run_id"], run.to_string());
    assert!(items[0]["body"].is_null());
    assert!(items[0]["created_at"].is_string());

    let copy = copies(&pool, parent).await.remove(0).0;
    finish_copy_turn(&pool, copy, "**Готово**").await;
    sweep(&f).await;
    let snapshot = f
        .get(&format!("/api/v1/conversations/{parent}/snapshot"))
        .await;
    let items = snapshot["data"]["routine_reports"].as_array().unwrap();
    assert_eq!(items[0]["state"], "ready");
    assert_eq!(items[0]["body"], "**Готово**");
    // Событие на каждое изменение: ожидание, затем готовый отчёт.
    let changed = card_events(&pool, parent, "routine_report_changed").await;
    let states: Vec<&str> = changed
        .iter()
        .map(|e| e["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["pending", "ready"]);
    assert_eq!(changed[1]["body"], "**Готово**");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dismiss_is_idempotent_evented_and_keeps_the_text_in_history(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    let schedule = make_schedule(&f, "Стендап").await;
    let (run, card) = ready_card(&f, schedule, "Отчёт дня").await;

    for _ in 0..2 {
        let r = f
            .client
            .send(Method::POST, &dismiss_path(parent, card), None, None)
            .await;
        assert_eq!(r.status.as_u16(), 204);
    }
    let dismissed = card_events(&pool, parent, "routine_report_dismissed").await;
    assert_eq!(
        dismissed.len(),
        1,
        "a repeat must not publish a second event"
    );
    assert_eq!(dismissed[0]["report_id"], card.to_string());
    assert_eq!(dismissed[0]["reason"], "owner");
    let snapshot = f
        .get(&format!("/api/v1/conversations/{parent}/snapshot"))
        .await;
    assert!(
        snapshot["data"]["routine_reports"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Чужая карточка и неизвестная — 404.
    let r = f
        .client
        .send(
            Method::POST,
            &dismiss_path(parent, Uuid::now_v7()),
            None,
            None,
        )
        .await;
    assert_eq!(r.status.as_u16(), 404);
    let other = testkit::seed_conversation(&pool, f.seed.owner_id, f.seed.machine_id).await;
    let r = f
        .client
        .send(Method::POST, &dismiss_path(other, card), None, None)
        .await;
    assert_eq!(r.status.as_u16(), 404);

    // История запуска отдаёт текст через API деталей запуска.
    let r = f
        .client
        .send(
            Method::GET,
            &format!("/api/v1/schedules/{schedule}/runs"),
            None,
            None,
        )
        .await;
    let page: Value = r.json();
    let item = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == run.to_string())
        .unwrap();
    assert_eq!(item["detail"]["report"]["text"], "Отчёт дня");
    assert_eq!(item["detail"]["report"]["state"], "ready");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_dismissed_pending_card_does_not_come_back_or_push(pool: PgPool) {
    use canterlot_hub::notify::{self, PushSettings};
    notify::configure(
        &pool,
        Some(&PushSettings {
            waiting: true,
            turn_finished: true,
            failures: true,
            timezone: "UTC".into(),
        }),
    )
    .await
    .unwrap();
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    let schedule = make_schedule(&f, "Стендап").await;
    let run = fire(&f, schedule).await;
    let card = open_cards(&pool, schedule).await.remove(0).id;
    let r = f
        .client
        .send(Method::POST, &dismiss_path(parent, card), None, None)
        .await;
    assert_eq!(r.status.as_u16(), 204);
    let copy = copies(&pool, parent).await.remove(0).0;
    finish_copy_turn(&pool, copy, "Поздний отчёт").await;
    sweep(&f).await;
    // Карточка осталась закрытой, текст всё же ушёл в историю, пуша нет.
    assert!(open_cards(&pool, schedule).await.is_empty());
    assert!(
        card_events(&pool, parent, "routine_report_changed")
            .await
            .iter()
            .all(|e| e["state"] != "ready")
    );
    let (_, detail) = run_state(&pool, run).await;
    assert_eq!(detail["report"]["text"], "Поздний отчёт");
    let pushes: i64 =
        sqlx::query_scalar("select count(*) from owner_pushes where kind='routine_report'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pushes, 0);
}

// ---------------------------------------------------------------------------
// Пуш «{имя} готов»: один на отчёт, по настройкам владельца, без пуша за ожидание.
// ---------------------------------------------------------------------------

async fn push_settings(pool: &PgPool, turn_finished: bool) {
    use canterlot_hub::notify::{self, PushSettings};
    notify::configure(
        pool,
        Some(&PushSettings {
            waiting: true,
            turn_finished,
            failures: true,
            timezone: "UTC".into(),
        }),
    )
    .await
    .unwrap();
}

async fn routine_pushes(pool: &PgPool) -> Vec<(Uuid, Value)> {
    sqlx::query_as(
        "select conversation_id,payload from owner_pushes where kind='routine_report' order by created_at",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn one_push_when_the_report_is_ready_and_none_while_pending(pool: PgPool) {
    push_settings(&pool, true).await;
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    fire(&f, schedule).await;
    sweep(&f).await;
    assert!(
        routine_pushes(&pool).await.is_empty(),
        "no push for pending"
    );
    let copy = copies(&pool, f.seed.conversation_id).await.remove(0).0;
    finish_copy_turn(&pool, copy, "Отчёт").await;
    sweep(&f).await;
    worker_stops_copies(&pool).await;
    sweep(&f).await;
    sweep(&f).await;
    let pushes = routine_pushes(&pool).await;
    assert_eq!(pushes.len(), 1, "exactly one push per report");
    assert_eq!(pushes[0].0, f.seed.conversation_id);
    assert_eq!(pushes[0].1["schedule_name"], "Стендап");
    let card = open_cards(&pool, schedule).await.remove(0).id;
    assert_eq!(pushes[0].1["report_id"], card.to_string());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_owners_push_settings_are_respected(pool: PgPool) {
    push_settings(&pool, false).await;
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    ready_card(&f, schedule, "Отчёт").await;
    assert!(routine_pushes(&pool).await.is_empty());
}

// ---------------------------------------------------------------------------
// Отказы: упавшая копия, нет нативной записи, пределы времени. Карточка с ошибкой
// и запуск `failed` с причиной; копия убирается.
// ---------------------------------------------------------------------------

async fn failed_run_and_card(f: &Fixture, schedule: Uuid, run: Uuid, reason: &str) {
    let (state, detail) = run_state(&f.pool, run).await;
    assert_eq!(state, "failed", "{detail}");
    assert_eq!(detail["reason"], reason, "{detail}");
    let open = open_cards(&f.pool, schedule).await;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].state, "failed");
    assert_eq!(open[0].error.as_deref(), Some(reason));
    assert!(open[0].body.is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_crashed_copy_fails_the_run_with_an_error_card_and_a_failure_push(pool: PgPool) {
    push_settings(&pool, true).await;
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    let run = fire(&f, schedule).await;
    let copy = copies(&pool, f.seed.conversation_id).await.remove(0).0;
    sqlx::query("update executions set state='failed' where conversation_id=$1")
        .bind(copy)
        .execute(&pool)
        .await
        .unwrap();
    sweep(&f).await;
    failed_run_and_card(&f, schedule, run, "copy_failed").await;
    // Копия убрана, отчётного пуша нет, а обычный пуш о сбое расписания ушёл.
    sweep(&f).await;
    assert!(copies(&pool, f.seed.conversation_id).await.is_empty());
    assert!(routine_pushes(&pool).await.is_empty());
    let failures: i64 = sqlx::query_scalar(
        "select count(*) from owner_pushes where kind='schedule_failed' and payload->>'schedule_id'=$1",
    )
    .bind(schedule.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(failures, 1);
    // Карточка показывает ошибку и всё, что нужно для «Повторить».
    let snapshot = f
        .get(&format!(
            "/api/v1/conversations/{}/snapshot",
            f.seed.conversation_id
        ))
        .await;
    let card = &snapshot["data"]["routine_reports"][0];
    assert_eq!(card["state"], "failed");
    assert_eq!(card["error"]["reason"], "copy_failed");
    assert!(card["error"]["message"].as_str().unwrap().len() > 5);
    assert_eq!(card["schedule_id"], schedule.to_string());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_parent_without_a_native_record_fails_the_run_with_an_error_card(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    let schedule = make_schedule(&f, "Стендап").await;
    let now = Utc::now();
    make_due(&pool, schedule, now - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(now).await.unwrap();
    let run = latest_run(&pool, schedule).await;
    wait_run(&pool, run, "failed").await;
    failed_run_and_card(&f, schedule, run, "no_native_session").await;
    // Копии нет: форкать было нечего.
    assert!(copies(&pool, f.seed.conversation_id).await.is_empty());
    // Повтор («Запустить сейчас») после появления записи заменяет карточку ошибки.
    with_native(&f).await;
    let retry = run_now(&f, schedule).await;
    wait_run(&pool, retry, "started").await;
    let all = cards(&pool, schedule).await;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].dismiss_reason.as_deref(), Some("replaced"));
    assert_eq!(all[1].state, "pending");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_turn_longer_than_sixty_minutes_fails_the_run(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    let run = fire(&f, schedule).await;
    let copy = copies(&pool, f.seed.conversation_id).await.remove(0).0;
    // Ход идёт: ввод передан агенту, исполнение живо.
    sqlx::query("update inputs set state='transmitting' where conversation_id=$1")
        .bind(copy)
        .execute(&pool)
        .await
        .unwrap();
    sweep(&f).await;
    assert_eq!(open_cards(&pool, schedule).await[0].state, "pending");
    // Через 59 минут ещё ждём, через 61 — отказ.
    sqlx::query("update routine_reports set turn_started_at=now()-interval '59 minutes' where schedule_id=$1")
        .bind(schedule)
        .execute(&pool)
        .await
        .unwrap();
    sweep(&f).await;
    assert_eq!(open_cards(&pool, schedule).await[0].state, "pending");
    sqlx::query("update routine_reports set turn_started_at=now()-interval '61 minutes' where schedule_id=$1")
        .bind(schedule)
        .execute(&pool)
        .await
        .unwrap();
    sweep(&f).await;
    failed_run_and_card(&f, schedule, run, "turn_timeout").await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_opencode_parent_busy_for_two_hours_fails_the_run(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    sqlx::query("update conversations set harness='opencode' where id=$1")
        .bind(parent)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "insert into conversation_derived(conversation_id,activity_state,activity_evidence)
         values($1,'working','{\"kind\":\"working\"}')",
    )
    .bind(parent)
    .execute(&pool)
    .await
    .unwrap();
    let schedule = make_schedule(&f, "Стендап").await;
    let run = fire(&f, schedule).await;
    let copy = copies(&pool, parent).await.remove(0).0;
    // Копия ждёт простоя родителя: исполнения нет, ввод держится.
    let executions: i64 =
        sqlx::query_scalar("select count(*) from executions where conversation_id=$1")
            .bind(copy)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(executions, 0);
    sweep(&f).await;
    assert_eq!(open_cards(&pool, schedule).await[0].state, "pending");
    sqlx::query(
        "update routine_reports set created_at=now()-interval '119 minutes' where schedule_id=$1",
    )
    .bind(schedule)
    .execute(&pool)
    .await
    .unwrap();
    sweep(&f).await;
    assert_eq!(open_cards(&pool, schedule).await[0].state, "pending");
    sqlx::query(
        "update routine_reports set created_at=now()-interval '121 minutes' where schedule_id=$1",
    )
    .bind(schedule)
    .execute(&pool)
    .await
    .unwrap();
    sweep(&f).await;
    failed_run_and_card(&f, schedule, run, "parent_busy_timeout").await;
    sweep(&f).await;
    assert!(copies(&pool, parent).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_finished_turn_without_text_fails_the_run(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    let run = fire(&f, schedule).await;
    let copy = copies(&pool, f.seed.conversation_id).await.remove(0).0;
    sqlx::query("update inputs set state='accepted' where conversation_id=$1")
        .bind(copy)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update queue_state set run_slot='free',run_slot_input_id=null,run_slot_attempt=null where conversation_id=$1")
        .bind(copy)
        .execute(&pool)
        .await
        .unwrap();
    record_copy_run(&pool, copy, true, false).await;
    sweep(&f).await;
    failed_run_and_card(&f, schedule, run, "empty_report").await;
}

/// Гонка сборщика: слот ещё свободен, ввод уже принят, а ход только начинается. Раньше
/// отсутствие текста объявлялось `empty_report`, и здоровую копию останавливали.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_turn_that_is_just_starting_is_not_an_empty_report(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    let run = fire(&f, schedule).await;
    let copy = copies(&pool, f.seed.conversation_id).await.remove(0).0;
    sqlx::query("update inputs set state='accepted' where conversation_id=$1")
        .bind(copy)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update queue_state set run_slot='free',run_slot_input_id=null,run_slot_attempt=null where conversation_id=$1")
        .bind(copy)
        .execute(&pool)
        .await
        .unwrap();
    // Запись хода открыта (`run_started`), конца ещё нет.
    record_copy_run(&pool, copy, false, false).await;
    sweep(&f).await;
    assert_eq!(run_state(&pool, run).await.0, "started");
    let open = open_cards(&pool, schedule).await;
    assert_eq!(open[0].state, "pending", "{:?}", open[0].error);
    // Ход закончился с ответом: отчёт собран.
    finish_copy_turn(&pool, copy, "Всё спокойно.").await;
    sqlx::query(
        "update runs set finished_at=clock_timestamp(),duration_ms=1,outcome='completed' where conversation_id=$1",
    )
    .bind(copy)
    .execute(&pool)
    .await
    .unwrap();
    sweep(&f).await;
    assert_eq!(open_cards(&pool, schedule).await[0].state, "ready");
}

/// Копия упала после приёма ввода: слот освободил переход исполнения, а раньше сказанная
/// реплика не итог хода. Отчётом она не становится.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_aborted_turn_does_not_publish_an_earlier_text_block(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    let run = fire(&f, schedule).await;
    let copy = copies(&pool, f.seed.conversation_id).await.remove(0).0;
    finish_copy_turn(&pool, copy, "Полуготовый отчёт").await;
    // Ход оборван: запись хода закрыта принудительно, исполнение упало.
    sqlx::query("update runs set finished_at=clock_timestamp(),duration_ms=1,forced_close=true where conversation_id=$1")
        .bind(copy)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update executions set state='failed' where conversation_id=$1")
        .bind(copy)
        .execute(&pool)
        .await
        .unwrap();
    sweep(&f).await;
    failed_run_and_card(&f, schedule, run, "copy_failed").await;
    assert!(routine_pushes(&pool).await.is_empty());
    let (_, detail) = run_state(&pool, run).await;
    assert!(detail["report"].get("text").is_none(), "{detail}");
}

// ---------------------------------------------------------------------------
// Допуск: перекрытие и лимиты пропускают запуск, не оставляя ни карточки, ни копии.
// ---------------------------------------------------------------------------

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_running_previous_report_skips_the_next_run(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    fire(&f, schedule).await;
    let now = Utc::now();
    make_due(&pool, schedule, now - TimeDelta::seconds(20)).await;
    scheduler(&f).tick(now).await.unwrap();
    let second = latest_run(&pool, schedule).await;
    wait_run(&pool, second, "skipped_overlap").await;
    assert_eq!(copies(&pool, f.seed.conversation_id).await.len(), 1);
    assert_eq!(cards(&pool, schedule).await.len(), 1);
    // Когда первый отчёт собран, следующий запуск проходит.
    let copy = copies(&pool, f.seed.conversation_id).await.remove(0).0;
    finish_copy_turn(&pool, copy, "Отчёт").await;
    sweep(&f).await;
    let later = Utc::now() + TimeDelta::seconds(2);
    make_due(&pool, schedule, later - TimeDelta::seconds(10)).await;
    scheduler(&f).tick(later).await.unwrap();
    let third = latest_run(&pool, schedule).await;
    wait_run(&pool, third, "started").await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_busy_parent_alone_does_not_count_as_a_running_report(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    sqlx::query(
        "insert into conversation_derived(conversation_id,activity_state,activity_evidence)
         values($1,'working','{\"kind\":\"working\"}')",
    )
    .bind(f.seed.conversation_id)
    .execute(&pool)
    .await
    .unwrap();
    let schedule = make_schedule(&f, "Стендап").await;
    let first = fire(&f, schedule).await;
    let copy = copies(&pool, f.seed.conversation_id).await.remove(0).0;
    finish_copy_turn(&pool, copy, "Отчёт").await;
    sweep(&f).await;
    let later = Utc::now() + TimeDelta::seconds(2);
    make_due(&pool, schedule, later - TimeDelta::seconds(10)).await;
    scheduler(&f).tick(later).await.unwrap();
    let second = latest_run(&pool, schedule).await;
    assert_ne!(first, second);
    // Родитель занят, но прошлый отчёт собран: запуск не пропускается как перекрытие.
    wait_run(&pool, second, "started").await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_red_limit_window_of_the_parents_model_skips_the_run(pool: PgPool) {
    use canterlot_hub::limits::{
        DataConfidence, Observation, ObservationSource, Window, record_observation_in_tx,
    };
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let now = Utc::now();
    let mut tx = pool.begin().await.unwrap();
    record_observation_in_tx(
        &mut tx,
        f.seed.owner_id,
        &Observation {
            provider: "claude".into(),
            window: Window::Weekly,
            used_permille: 990,
            resets_at: Some(now + TimeDelta::days(2)),
            observed_at: now,
            source: ObservationSource::Session,
            data_confidence: DataConfidence::Exact,
            machine_id: f.seed.machine_id,
            execution_id: None,
        },
        now,
        &Default::default(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let schedule = make_schedule(&f, "Стендап").await;
    make_due(&pool, schedule, now - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(now).await.unwrap();
    let run = latest_run(&pool, schedule).await;
    let detail = wait_run(&pool, run, "skipped_limits").await;
    assert_eq!(detail["window"], "weekly", "{detail}");
    assert!(copies(&pool, f.seed.conversation_id).await.is_empty());
    assert!(cards(&pool, schedule).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_offline_machine_skips_the_run(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    with_native(&f).await;
    let schedule = make_schedule_without_session(&f, "Стендап").await;
    let now = Utc::now();
    make_due(&pool, schedule, now - TimeDelta::seconds(30)).await;
    scheduler(&f).tick(now).await.unwrap();
    let run = latest_run(&pool, schedule).await;
    wait_run(&pool, run, "skipped_machine_offline").await;
    assert!(cards(&pool, schedule).await.is_empty());
}

/// Расписание создаётся, пока воркер на связи (иначе проверка цели откажет), потом связь рвётся.
async fn make_schedule_without_session(f: &Fixture, name: &str) -> Uuid {
    let session = session(f);
    let id = make_schedule(f, name).await;
    drop(session);
    id
}

// ---------------------------------------------------------------------------
// Уборка: текст отчёта живёт в истории запусков 30 дней.
// ---------------------------------------------------------------------------

/// Отчёт завершился `days` дней назад (срок текста считается от завершения, не от запуска).
async fn age_report(pool: &PgPool, run: Uuid, days: i64) {
    sqlx::query(
        "update schedule_runs
            set detail=jsonb_set(detail,'{report,completed_at}',
                to_jsonb(to_char((clock_timestamp()-make_interval(days=>$2)) at time zone 'UTC',
                                 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')))
          where id=$1",
    )
    .bind(run)
    .bind(days as i32)
    .execute(pool)
    .await
    .unwrap();
}

/// Двести пять завершённых запусков-заполнителей новее заданного.
async fn newer_runs(f: &Fixture, schedule: Uuid, count: i32) {
    for n in 0..count {
        sqlx::query(
            "insert into schedule_runs(id,schedule_id,owner_id,scheduled_at,claimed_revision,state,manual,idempotency_key)
             values($1,$2,$3,now()+make_interval(secs=>$4),1,'skipped_overlap',false,$5)",
        )
        .bind(Uuid::now_v7())
        .bind(schedule)
        .bind(f.seed.owner_id)
        .bind(f64::from(n + 10))
        .bind(format!("filler-{n}"))
        .execute(&f.pool)
        .await
        .unwrap();
    }
}

async fn run_exists(pool: &PgPool, run: Uuid) -> bool {
    sqlx::query_scalar("select exists(select 1 from schedule_runs where id=$1)")
        .bind(run)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn report_text_is_pruned_from_history_after_thirty_days(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    let (run, _) = ready_card(&f, schedule, "Отчёт").await;
    let mut conn = pool.acquire().await.unwrap();
    // Свежий отчёт уборка не трогает; запуск 29 дней назад — тоже.
    canterlot_hub::routines::prune(&mut conn).await.unwrap();
    assert_eq!(run_state(&pool, run).await.1["report"]["text"], "Отчёт");
    age_report(&pool, run, 29).await;
    canterlot_hub::routines::prune(&mut conn).await.unwrap();
    assert_eq!(run_state(&pool, run).await.1["report"]["text"], "Отчёт");
    // На 31 день после завершения текст уходит, запуск и его метаданные остаются.
    age_report(&pool, run, 31).await;
    canterlot_hub::routines::prune(&mut conn).await.unwrap();
    let (state, detail) = run_state(&pool, run).await;
    assert_eq!(state, "started");
    assert!(detail["report"].get("text").is_none(), "{detail}");
    assert_eq!(detail["report"]["state"], "ready");
}

// ---------------------------------------------------------------------------
// Агент родителя карточки не видит: она не ввод и не блок, в доставку не попадает.
// ---------------------------------------------------------------------------

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_card_never_reaches_the_agents_prompt(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    let schedule = make_schedule(&f, "Стендап").await;
    ready_card(&f, schedule, "СЕКРЕТНЫЙ-ТЕКСТ-ОТЧЁТА").await;
    worker_stops_copies(&pool).await;
    sweep(&f).await;

    // Ни одной строки с текстом или заголовком в том, из чего собирается содержимое доставки.
    for table in ["inputs", "blocks", "commands"] {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "select count(*) from {table} t where t.conversation_id=$1
               and (to_jsonb(t)::text like '%СЕКРЕТНЫЙ-ТЕКСТ-ОТЧЁТА%'
                    or to_jsonb(t)::text like '%Стендап · %')"
        )))
        .bind(parent)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0, "{table} must not carry the report card");
    }
    // Настоящее сообщение владельца уходит агенту без карточки.
    let input = f.input("after-report").await;
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(input.command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let text = payload.to_string();
    assert!(!text.contains("СЕКРЕТНЫЙ-ТЕКСТ-ОТЧЁТА"), "{text}");
    assert!(!text.contains("Стендап · "), "{text}");
}

// ---------------------------------------------------------------------------
// Контракт событий: настоящие события карточки укладываются в events.schema.json.
// ---------------------------------------------------------------------------

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn card_events_fit_the_published_schema(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    let schedule = make_schedule(&f, "Стендап").await;
    let (_, card) = ready_card(&f, schedule, "Отчёт").await;
    // Замена прежней карточки и закрытие владельцем дают оба вида событий.
    let second = run_now(&f, schedule).await;
    wait_run(&pool, second, "started").await;
    let open = open_cards(&pool, schedule).await.remove(0).id;
    let r = f
        .client
        .send(Method::POST, &dismiss_path(parent, open), None, None)
        .await;
    assert_eq!(r.status.as_u16(), 204);
    assert_ne!(card, open);
    // Удаление расписания снимает ещё одну карточку с причиной `schedule_deleted`.
    let third = run_now(&f, schedule).await;
    wait_run(&pool, third, "started").await;
    assert_eq!(delete_schedule(&f, schedule).await, 200);
    let reasons: Vec<String> = card_events(&pool, parent, "routine_report_dismissed")
        .await
        .iter()
        .map(|payload| payload["reason"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        reasons.contains(&"schedule_deleted".to_owned()),
        "{reasons:?}"
    );

    let mut document: Value =
        serde_json::from_str(include_str!("../../../../docs/api/events.schema.json")).unwrap();
    document.as_object_mut().unwrap().remove("oneOf");
    let rows: Vec<canterlot_hub::events::StoredEvent> =
        sqlx::query_as("select * from events where scope=$1 order by scope_seq")
            .bind(format!("conversation:{parent}"))
            .fetch_all(&pool)
            .await
            .unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for event in canterlot_hub::events::project_for_client(rows) {
        if !event.kind.starts_with("routine_report_") {
            continue;
        }
        let mut schema = document.clone();
        schema["$ref"] = json!(format!("#/$defs/{}", event.kind));
        let validator = jsonschema::validator_for(&schema).unwrap();
        let value = serde_json::to_value(&event).unwrap();
        assert!(
            validator.is_valid(&value),
            "{} does not fit: {value}",
            event.kind
        );
        seen.insert(event.kind.clone());
    }
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        ["routine_report_changed", "routine_report_dismissed"]
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn trashing_the_parent_removes_report_copies_and_keeps_the_interactive_rules(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    let schedule = make_schedule(&f, "Стендап").await;
    fire(&f, schedule).await;
    // Копия ещё собирает отчёт: удаление родителя заказывает остановку и ждёт её.
    let r = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{parent}"),
            Some("trash-with-report"),
            None,
        )
        .await;
    assert!(
        r.status.as_u16() == 204 || r.status.as_u16() == 409,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    worker_stops_copies(&pool).await;
    sweep(&f).await;
    sweep(&f).await;
    // Копий нет, а сломанная карточка не мешает следующим запускам и не падает в тике.
    let left: i64 = sqlx::query_scalar(
        "select count(*) from conversations where side_of=$1 and trashed_at is null",
    )
    .bind(parent)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, 0);
}

// ---------------------------------------------------------------------------
// Уборка копий: порядок замков, отказ по одной копии, потерянные исполнения.
// ---------------------------------------------------------------------------

/// Карточка с копией, которую пора убрать: копия — скрытая боковая копия отчёта.
async fn stray_copy(f: &Fixture, schedule: Uuid, exec_state: Option<&str>) -> (Uuid, Uuid) {
    let copy = testkit::seed_conversation(&f.pool, f.seed.owner_id, f.seed.machine_id).await;
    sqlx::query("update conversations set side_of=$2, side_kind='report' where id=$1")
        .bind(copy)
        .bind(f.seed.conversation_id)
        .execute(&f.pool)
        .await
        .unwrap();
    if let Some(state) = exec_state {
        sqlx::query(
            "insert into executions(id,conversation_id,generation,state) values($1,$2,1,$3)",
        )
        .bind(Uuid::now_v7())
        .bind(copy)
        .bind(state)
        .execute(&f.pool)
        .await
        .unwrap();
    }
    let card = Uuid::now_v7();
    sqlx::query(
        "insert into routine_reports(id,owner_id,conversation_id,schedule_id,copy_id,title,state,
            body,completed_at,dismissed_at,dismiss_reason)
         values($1,$2,$3,$4,$5,'t','ready','x',clock_timestamp(),clock_timestamp(),'replaced')",
    )
    .bind(card)
    .bind(f.seed.owner_id)
    .bind(f.seed.conversation_id)
    .bind(schedule)
    .bind(copy)
    .execute(&f.pool)
    .await
    .unwrap();
    (card, copy)
}

async fn card_copy(pool: &PgPool, card: Uuid) -> Option<Uuid> {
    sqlx::query_scalar("select copy_id from routine_reports where id=$1")
        .bind(card)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Порядок замков удаления: владелец, затем строки копий. Уборка, берущая копию раньше
/// владельца, замыкалась в цикл с переносом родителя в корзину.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cleanup_takes_the_owner_lock_before_the_copy_row(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let schedule = make_schedule_without_session(&f, "Стендап").await;
    let (_card, copy) = stray_copy(&f, schedule, None).await;
    let mut holder = canterlot_hub::db::begin(&pool).await.unwrap();
    sqlx::query(
        "select pg_advisory_xact_lock(hashtextextended('canterlot:attributes:'||$1::text,0))",
    )
    .bind(f.seed.owner_id)
    .execute(&mut *holder)
    .await
    .unwrap();
    let sweeper = {
        let pool = pool.clone();
        tokio::spawn(async move { canterlot_hub::routines::sweep(&pool, Utc::now()).await })
    };
    // Уборка дошла до замка владельца и стоит на нём.
    let mut waiting = false;
    for _ in 0..20_000 {
        waiting = sqlx::query_scalar(
            "select exists(select 1 from pg_locks where locktype='advisory' and not granted
               and database=(select oid from pg_database where datname=current_database()))",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        if waiting {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(waiting, "cleanup never reached the owner lock");
    let locked_by_cleanup =
        sqlx::query("select id from conversations where id=$1 for update nowait")
            .bind(copy)
            .execute(&mut *holder)
            .await
            .is_err();
    assert!(
        !locked_by_cleanup,
        "cleanup locked the copy row before the owner lock"
    );
    holder.rollback().await.unwrap();
    sweeper.await.unwrap().unwrap();
}

/// Сбой уборки одной копии не роняет ни проход, ни тик планировщика, ни уборку других копий.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_cleanup_failure_is_per_copy_and_never_aborts_the_tick(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let schedule = make_schedule_without_session(&f, "Стендап").await;
    let (broken_card, broken) = stray_copy(&f, schedule, None).await;
    let (good_card, _good) = stray_copy(&f, schedule, None).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "create function fail_copy_delete() returns trigger language plpgsql as $$
         begin
           if old.id = '{broken}' then raise exception 'boom'; end if;
           return old;
         end $$"
    )))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "create trigger fail_copy_delete before delete on conversations
         for each row execute function fail_copy_delete()",
    )
    .execute(&pool)
    .await
    .unwrap();
    scheduler(&f).tick(Utc::now()).await.unwrap();
    assert!(card_copy(&pool, good_card).await.is_none());
    assert_eq!(card_copy(&pool, broken_card).await, Some(broken));
}

/// Потерянные исполнения копий получают запрос разрешения и уходят в конец очереди уборки:
/// пятьдесят таких не вытесняют из пачки обычную копию.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lost_copies_are_recovered_and_cannot_fill_the_cleanup_batch(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let schedule = make_schedule_without_session(&f, "Стендап").await;
    let mut lost = Vec::new();
    for _ in 0..55 {
        lost.push(stray_copy(&f, schedule, Some("owner_lost")).await);
    }
    let (normal_card, _) = stray_copy(&f, schedule, None).await;
    canterlot_hub::routines::sweep(&pool, Utc::now())
        .await
        .unwrap();
    canterlot_hub::routines::sweep(&pool, Utc::now())
        .await
        .unwrap();
    assert!(
        card_copy(&pool, normal_card).await.is_none(),
        "lost copies starved the cleanup batch"
    );
    let resolves: i64 = sqlx::query_scalar(
        "select count(distinct conversation_id) from commands where kind='resolve_owner_lost'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(resolves, 55, "every lost copy gets a recovery request");
}

// ---------------------------------------------------------------------------
// Удаление родителя и пересечение запусков.
// ---------------------------------------------------------------------------

/// Окончательное удаление родителя с недособранным отчётом: карточка уходит каскадом, а
/// запуск не остаётся «собирается» навсегда.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn purging_the_parent_finalizes_its_pending_runs(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    let schedule = make_schedule(&f, "Стендап").await;
    let run = fire(&f, schedule).await;
    let r = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{parent}"),
            Some("trash-before-purge"),
            None,
        )
        .await;
    assert!(r.status.as_u16() == 204 || r.status.as_u16() == 409);
    worker_stops_copies(&pool).await;
    // Окончательное удаление раньше прохода сборщика.
    let r = f
        .client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{parent}/trash"),
            Some("purge-before-sweep"),
            None,
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        204,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let (state, detail) = run_state(&pool, run).await;
    assert_eq!(state, "failed", "{detail}");
    assert_eq!(detail["reason"], "conversation_not_found", "{detail}");
    assert_eq!(detail["report"]["state"], "failed", "{detail}");
}

/// Пересечение считается по живому состоянию отчёта: запуск, чья карточка исчезла
/// (каскад), следующий не блокирует, даже если в его журнале записано «собирается».
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn overlap_follows_the_live_report_not_the_run_journal(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    let first = fire(&f, schedule).await;
    assert_eq!(
        run_state(&pool, first).await.1["report"]["state"],
        "pending"
    );
    sqlx::query("delete from routine_reports where schedule_id=$1")
        .bind(schedule)
        .execute(&pool)
        .await
        .unwrap();
    let second = fire(&f, schedule).await;
    assert_ne!(first, second);
}

async fn delete_schedule(f: &Fixture, schedule: Uuid) -> u16 {
    let revision: i64 = sqlx::query_scalar("select revision from schedules where id=$1")
        .bind(schedule)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    f.client
        .send(
            Method::DELETE,
            &format!("/api/v1/schedules/{schedule}"),
            Some(&format!("routine-delete-{}", Uuid::now_v7())),
            Some(json!({"base_revision": revision})),
        )
        .await
        .status
        .as_u16()
}

/// Удаление расписания с собирающимся отчётом: каскад не уносит запись об уборке, копии
/// заказана остановка, открытая лента получает снятие карточки в той же транзакции.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn deleting_a_schedule_stops_its_copy_and_dismisses_the_open_card(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    let schedule = make_schedule(&f, "Стендап").await;
    fire(&f, schedule).await;
    let copy = copies(&pool, parent).await.remove(0).0;
    let card: Uuid = sqlx::query_scalar("select id from routine_reports where schedule_id=$1")
        .bind(schedule)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(delete_schedule(&f, schedule).await, 200);

    // Лента узнаёт о снятии сразу, не дожидаясь прохода.
    let dismissed = card_events(&pool, parent, "routine_report_dismissed").await;
    assert_eq!(dismissed.len(), 1, "{dismissed:?}");
    assert_eq!(dismissed[0]["report_id"], card.to_string());
    // Строка уборки пережила каскад, копии заказана остановка.
    assert_eq!(card_copy(&pool, card).await, Some(copy));
    let stops: i64 = sqlx::query_scalar(
        "select count(*) from commands where conversation_id=$1 and kind='stop_execution'",
    )
    .bind(copy)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        stops >= 1,
        "the copy keeps running after its schedule is gone"
    );
    // Остановка подтверждена: копия и запись об уборке уходят, ничего не публикуется.
    worker_stops_copies(&pool).await;
    sweep(&f).await;
    assert!(copies(&pool, parent).await.is_empty());
    assert!(card_copy(&pool, card).await.is_none());
    assert!(routine_pushes(&pool).await.is_empty());
}

/// Карточка со старой ошибкой после смены цели расписания: «Повторить» не может отправить
/// сообщение в настоящую сессию. Хаб сверяет цель с разговором карточки сам.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_stale_retry_is_refused_when_the_schedule_no_longer_reports_this_session(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    let parent = f.seed.conversation_id;
    let schedule = make_schedule(&f, "Стендап").await;
    let revision: i64 = sqlx::query_scalar("select revision from schedules where id=$1")
        .bind(schedule)
        .fetch_one(&pool)
        .await
        .unwrap();
    let r = f
        .client
        .send(
            Method::PATCH,
            &format!("/api/v1/schedules/{schedule}"),
            Some(&format!("retarget-{}", Uuid::now_v7())),
            Some(json!({
                "base_revision": revision,
                "target": {"kind": "existing_session", "conversation_id": parent},
            })),
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let refused = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/schedules/{schedule}/run"),
            Some(&format!("stale-retry-{}", Uuid::now_v7())),
            Some(json!({"run_anyway": false, "report_conversation_id": parent})),
        )
        .await;
    assert_eq!(
        refused.status.as_u16(),
        422,
        "{}",
        String::from_utf8_lossy(&refused.body)
    );
    let body = refused.json::<Value>();
    assert_eq!(body["error"]["code"], "schedule_invalid", "{body}");
    assert_eq!(body["error"]["details"]["field"], "target", "{body}");
    let runs: i64 = sqlx::query_scalar("select count(*) from schedule_runs where schedule_id=$1")
        .bind(schedule)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 0, "a refused retry must not start a run");
}

/// Предел в 200 запусков не стирает отчёты: собирающийся запуск живёт до финала, а готовый
/// отчёт — 30 дней от завершения, как бы часто ни шли запуски.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_history_cap_keeps_pending_reports_and_fresh_report_text(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    let (ready_run, _) = ready_card(&f, schedule, "Отчёт").await;
    worker_stops_copies(&pool).await;
    sweep(&f).await;
    let pending_run = run_now(&f, schedule).await;
    wait_run(&pool, pending_run, "started").await;
    // Оба запуска старше всех заполнителей, то есть первыми попадают под предел.
    sqlx::query("update schedule_runs set scheduled_at=now()-interval '2 days' where id=any($1)")
        .bind(vec![ready_run, pending_run])
        .execute(&pool)
        .await
        .unwrap();
    newer_runs(&f, schedule, 205).await;
    let mut conn = pool.acquire().await.unwrap();
    canterlot_hub::schedules::prune_runs(&mut conn)
        .await
        .unwrap();
    assert!(
        run_exists(&pool, pending_run).await,
        "a pending report run was purged"
    );
    assert!(
        run_exists(&pool, ready_run).await,
        "fresh report text was purged by the cap"
    );
    assert_eq!(
        run_state(&pool, ready_run).await.1["report"]["text"],
        "Отчёт"
    );
    // Через 30 дней после завершения текст уходит, и запуск снова подчиняется пределу.
    age_report(&pool, ready_run, 31).await;
    canterlot_hub::routines::prune(&mut conn).await.unwrap();
    canterlot_hub::schedules::prune_runs(&mut conn)
        .await
        .unwrap();
    assert!(!run_exists(&pool, ready_run).await);
    assert!(run_exists(&pool, pending_run).await);
}

/// Потерянный ответ Gotify не повод слать пуш «отчёт готов» второй раз: неоднозначная
/// доставка не повторяется, как и у пуша о сбое расписания.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_lost_gotify_response_does_not_resend_the_report_push(pool: PgPool) {
    use std::sync::atomic::Ordering;
    crate::auth_pushes::enable(&pool).await;
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    ready_card(&f, schedule, "Отчёт").await;
    assert_eq!(routine_pushes(&pool).await.len(), 1);
    let (hits, url) = crate::schedule_failure_push::dropping_gotify().await;
    let (_dir, sender) = crate::auth_pushes::sender(&url);
    for _ in 0..4 {
        sender.deliver_due(&pool).await.unwrap();
        sqlx::query("update owner_pushes set run_at=clock_timestamp() where state='pending'")
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "ambiguous delivery was retried"
    );
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from owner_pushes where kind='routine_report' and state='pending'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, 0, "the row is settled");
}

async fn assert_not_published(outcome: Option<&str>, pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    let run = fire(&f, schedule).await;
    let copy = copies(&pool, f.seed.conversation_id).await.remove(0).0;
    finish_copy_turn(&pool, copy, "Промежуточная реплика, не итог").await;
    sqlx::query("update runs set outcome=$2 where conversation_id=$1")
        .bind(copy)
        .bind(outcome)
        .execute(&pool)
        .await
        .unwrap();
    sweep(&f).await;
    failed_run_and_card(&f, schedule, run, "copy_failed").await;
    assert!(routine_pushes(&pool).await.is_empty());
    assert!(
        run_state(&pool, run).await.1["report"]
            .get("text")
            .is_none()
    );
}

/// Codex прерывает ход, OpenCode роняет его: `run_finished` приходит без принудительного
/// закрытия, а блок с текстом уже `final`. Отчётом он быть не должен.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_interrupted_finish_never_publishes_the_earlier_text(pool: PgPool) {
    assert_not_published(Some("interrupted"), pool).await;
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failed_finish_never_publishes_the_earlier_text(pool: PgPool) {
    assert_not_published(Some("failed"), pool).await;
}

/// Старый воркер исхода не присылает: успех не доказан, отчёт не публикуется.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_finish_without_a_recorded_outcome_never_publishes(pool: PgPool) {
    assert_not_published(None, pool).await;
}

async fn retry_with(f: &Fixture, schedule: Uuid, key: &str, conversation: Uuid) -> (u16, Value) {
    let r = f
        .client
        .send(
            Method::POST,
            &format!("/api/v1/schedules/{schedule}/run"),
            Some(key),
            Some(json!({"run_anyway": false, "report_conversation_id": conversation})),
        )
        .await;
    (r.status.as_u16(), r.json())
}

async fn retarget_to_live_session(f: &Fixture, schedule: Uuid, conversation: Uuid) {
    let revision: i64 = sqlx::query_scalar("select revision from schedules where id=$1")
        .bind(schedule)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    let r = f
        .client
        .send(
            Method::PATCH,
            &format!("/api/v1/schedules/{schedule}"),
            Some(&format!("retarget-{}", Uuid::now_v7())),
            Some(json!({
                "base_revision": revision,
                "target": {"kind": "existing_session", "conversation_id": conversation},
            })),
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
}

/// Принятый повтор после правки расписания отвечает тем же ответом: повтор по ключу ищется
/// раньше проверки цели.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_accepted_retry_replays_after_a_later_edit_of_the_schedule(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    let schedule = make_schedule(&f, "Стендап").await;
    let (status, first) = retry_with(&f, schedule, "retry-replay-key", parent).await;
    assert_eq!(status, 200, "{first}");
    retarget_to_live_session(&f, schedule, parent).await;
    let (status, again) = retry_with(&f, schedule, "retry-replay-key", parent).await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["id"], first["id"]);
}

/// Разговор карточки входит в личность запроса: тот же ключ с другим разговором не получает
/// чужой ответ.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn the_retry_conversation_is_part_of_the_idempotency_identity(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let schedule = make_schedule(&f, "Стендап").await;
    let (status, first) = retry_with(&f, schedule, "identity-key", f.seed.conversation_id).await;
    assert_eq!(status, 200, "{first}");
    let (status, other) = retry_with(&f, schedule, "identity-key", Uuid::now_v7()).await;
    assert_eq!(status, 409, "{other}");
}

/// Правка расписания между заявкой повтора и приёмкой: заявка записала версию, которую
/// проверила, поэтому приёмка по новой версии не проходит и промпт отчёта в живую сессию не
/// уходит.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_edit_between_the_retry_reservation_and_admission_never_reaches_the_live_session(
    pool: PgPool,
) {
    use canterlot_hub::schedules::admission::{Reservation, reserve_manual_for};
    let f = Fixture::new(pool.clone()).await;
    let _session = session(&f);
    with_native(&f).await;
    let parent = f.seed.conversation_id;
    let schedule = make_schedule(&f, "Стендап").await;
    let Reservation::Fresh { run, .. } = reserve_manual_for(
        &pool,
        f.seed.owner_id,
        schedule,
        "race-retry",
        false,
        Some(parent),
    )
    .await
    .unwrap() else {
        panic!("fresh");
    };
    retarget_to_live_session(&f, schedule, parent).await;
    sqlx::query("update schedule_runs set lease_at=now()-interval '2 hours',updated_at=now()-interval '2 hours' where id=$1")
        .bind(run.id)
        .execute(&pool)
        .await
        .unwrap();
    sweep(&f).await;
    for _ in 0..20_000 {
        if run_state(&pool, run.id).await.0 != "claimed" {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_ne!(run_state(&pool, run.id).await.0, "started");
    let sent: i64 = sqlx::query_scalar("select count(*) from inputs where content->0->>'text'=$1")
        .bind(PROMPT)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sent, 0, "the report prompt went into the live session");
}

/// Ждёт, пока в базе этого теста кто-то встанет на advisory-замок.
async fn wait_for_advisory_waiter(pool: &PgPool) -> bool {
    for _ in 0..20_000 {
        let waiting: bool = sqlx::query_scalar(
            "select exists(select 1 from pg_locks where locktype='advisory' and not granted
               and database=(select oid from pg_database where datname=current_database()))",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting {
            return true;
        }
        tokio::task::yield_now().await;
    }
    false
}

/// Приёмка берёт строку расписания, затем замок владельца. Удаление расписания идёт тем же
/// порядком: обратный порядок давал взаимную блокировку при удалении во время срабатывания.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn deleting_a_schedule_takes_the_schedule_row_before_the_owner_lock(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let schedule = make_schedule_without_session(&f, "Стендап").await;
    let mut holder = canterlot_hub::db::begin(&pool).await.unwrap();
    sqlx::query(
        "select pg_advisory_xact_lock(hashtextextended('canterlot:attributes:'||$1::text,0))",
    )
    .bind(f.seed.owner_id)
    .execute(&mut *holder)
    .await
    .unwrap();
    let deleter = {
        let client = f.client.clone();
        let revision: i64 = sqlx::query_scalar("select revision from schedules where id=$1")
            .bind(schedule)
            .fetch_one(&pool)
            .await
            .unwrap();
        tokio::spawn(async move {
            client
                .send(
                    Method::DELETE,
                    &format!("/api/v1/schedules/{schedule}"),
                    Some("delete-order"),
                    Some(json!({"base_revision": revision})),
                )
                .await
                .status
                .as_u16()
        })
    };
    assert!(
        wait_for_advisory_waiter(&pool).await,
        "delete never reached the owner lock"
    );
    let held_by_delete = sqlx::query("select id from schedules where id=$1 for update nowait")
        .bind(schedule)
        .execute(&mut *holder)
        .await
        .is_err();
    assert!(
        held_by_delete,
        "delete waits on the owner lock without holding the schedule row"
    );
    holder.rollback().await.unwrap();
    assert_eq!(deleter.await.unwrap(), 200);
}

/// Очередь копии берётся раньше строки разговора, как в удалении и в `release_waiting`:
/// обратный порядок замыкал уборку на отпускание ввода при простое родителя.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn cleanup_takes_the_copy_queue_before_the_copy_row(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let schedule = make_schedule_without_session(&f, "Стендап").await;
    let (_card, copy) = stray_copy(&f, schedule, None).await;
    let mut holder = canterlot_hub::db::begin(&pool).await.unwrap();
    sqlx::query("select 1 from queue_state where conversation_id=$1 for update")
        .bind(copy)
        .execute(&mut *holder)
        .await
        .unwrap();
    let sweeper = {
        let pool = pool.clone();
        tokio::spawn(async move { canterlot_hub::routines::sweep(&pool, Utc::now()).await })
    };
    // Уборка встала на очереди копии: ждём, пока в базе появится ожидающий замок строки.
    let mut waiting = false;
    for _ in 0..20_000 {
        waiting = sqlx::query_scalar(
            "select exists(select 1 from pg_locks l join pg_stat_activity a on a.pid=l.pid
                            where not l.granted and l.locktype in ('transactionid','tuple')
                              and a.datname=current_database())",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        if waiting {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(waiting, "cleanup never waited on the copy queue");
    let row_taken_first = sqlx::query("select id from conversations where id=$1 for update nowait")
        .bind(copy)
        .execute(&mut *holder)
        .await
        .is_err();
    assert!(
        !row_taken_first,
        "cleanup took the copy row before its queue"
    );
    holder.rollback().await.unwrap();
    sweeper.await.unwrap().unwrap();
}

/// Сбой уборки одного осиротевшего бокового чата не роняет `side::sweep`, поэтому тик доходит
/// до отчётов и остальных копий.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_failing_orphan_side_chat_does_not_stop_the_tick(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let schedule = make_schedule_without_session(&f, "Стендап").await;
    let (good_card, _good) = stray_copy(&f, schedule, None).await;
    let other_parent = testkit::seed_conversation(&pool, f.seed.owner_id, f.seed.machine_id).await;
    sqlx::query("update conversations set trashed_at=now() where id=$1")
        .bind(other_parent)
        .execute(&pool)
        .await
        .unwrap();
    let orphan = testkit::seed_conversation(&pool, f.seed.owner_id, f.seed.machine_id).await;
    sqlx::query("update conversations set side_of=$2, side_kind='chat' where id=$1")
        .bind(orphan)
        .bind(other_parent)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "create function fail_orphan_delete() returns trigger language plpgsql as $$
         begin
           if old.id = '{orphan}' then raise exception 'boom'; end if;
           return old;
         end $$"
    )))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "create trigger fail_orphan_delete before delete on conversations
         for each row execute function fail_orphan_delete()",
    )
    .execute(&pool)
    .await
    .unwrap();
    scheduler(&f).tick(Utc::now()).await.unwrap();
    assert!(card_copy(&pool, good_card).await.is_none());
}
