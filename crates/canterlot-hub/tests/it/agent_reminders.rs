//! Напоминания агентам служебной строкой: настройка владельца, устаревшие задачи плана
//! оркестратора и забытый список задач агента.

use canterlot_hub::{
    delivery::{
        self, ContentBlock, SubmitInput,
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
    },
    executions,
    model::ExecutionState,
    reminders::{self, AgentReminderSettings},
    testkit::{self, Seed},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::cases::{Snapshot, restore, run_cases};

async fn settings_default_on_and_round_trip(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        reminders::settings(&mut conn, seed.owner_id).await.unwrap(),
        AgentReminderSettings {
            plan_enabled: true,
            todo_enabled: true,
        },
        "строки нет — оба включены"
    );
    let next = AgentReminderSettings {
        plan_enabled: false,
        todo_enabled: true,
    };
    reminders::replace_settings_in_tx(&mut conn, seed.owner_id, next)
        .await
        .unwrap();
    assert_eq!(
        reminders::settings(&mut conn, seed.owner_id).await.unwrap(),
        next
    );
    assert_eq!(
        serde_json::to_value(next).unwrap(),
        json!({"plan_enabled": false, "todo_enabled": true})
    );
    assert!(
        serde_json::from_value::<AgentReminderSettings>(json!({"plan_enabled": true})).is_err(),
        "оба поля обязательны"
    );
}

async fn orchestrator(pool: &PgPool) -> Seed {
    let seed = testkit::seed(pool).await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(seed.machine_id)
        .execute(pool)
        .await
        .unwrap();
    let execution = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    seed
}

async fn child(pool: &PgPool, seed: &Seed) -> Uuid {
    testkit::seed_conversation(pool, seed.owner_id, seed.machine_id).await
}

/// Задача плана, последний раз правленная `age` назад (интервал PostgreSQL).
async fn task(
    pool: &PgPool,
    seed: &Seed,
    number: i32,
    state: &str,
    executor: Option<Uuid>,
    age: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into plan_tasks (id, owner_id, orchestrator_conversation_id, number, title,
                                 state, executor_conversation_id, updated_at)
         values ($1, $2, $3, $4, $5, $6, $7, clock_timestamp() - $8::interval)",
    )
    .bind(id)
    .bind(seed.owner_id)
    .bind(seed.conversation_id)
    .bind(number)
    .bind(format!("Задача {number}"))
    .bind(state)
    .bind(executor)
    .bind(age)
    .execute(pool)
    .await
    .unwrap();
    id
}

/// Законченные ходы разговора: каждый со своей командой ввода.
async fn finished_runs(pool: &PgPool, conversation_id: Uuid, count: usize) {
    for _ in 0..count {
        let (machine_id, execution_id, generation): (Uuid, Uuid, i64) = sqlx::query_as(
            "select c.machine_id, e.id, e.generation from conversations c
             join executions e on e.conversation_id = c.id
             where c.id = $1 order by e.generation desc limit 1",
        )
        .bind(conversation_id)
        .fetch_one(pool)
        .await
        .unwrap();
        let command_id = Uuid::now_v7();
        sqlx::query(
            "insert into commands (id, machine_id, conversation_id, execution_id,
                                   execution_generation, kind, payload, delivery_state)
             values ($1, $2, $3, $4, $5, 'deliver_input', '{}', 'completed')",
        )
        .bind(command_id)
        .bind(machine_id)
        .bind(conversation_id)
        .bind(execution_id)
        .bind(generation)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into runs (input_command_id, execution_id, conversation_id, started_at,
                               finished_at)
             values ($1, $2, $3, clock_timestamp(), clock_timestamp())",
        )
        .bind(command_id)
        .bind(execution_id)
        .bind(conversation_id)
        .execute(pool)
        .await
        .unwrap();
    }
}

/// Пересчёт напоминаний, как перед выдачей; возвращает тексты новых заметок вида `kind`.
async fn prepare(pool: &PgPool, conversation_id: Uuid, kind: &str) -> Vec<String> {
    let before: i64 = sqlx::query_scalar(
        "select coalesce(max(input_seq), 0) from inputs where conversation_id = $1",
    )
    .bind(conversation_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    reminders::prepare_in_tx(&mut tx, conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query_scalar(
        "select content->0->>'text' from inputs
         where conversation_id = $1 and service_note and input_seq > $2
           and origin#>>'{event,kind}' = $3
         order by input_seq",
    )
    .bind(conversation_id)
    .bind(before)
    .bind(kind)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn rewind_digest(pool: &PgPool, seed: &Seed, by: &str) {
    sqlx::query(
        "update plan_reminders set last_digest_at = last_digest_at - $2::interval
         where orchestrator_conversation_id = $1",
    )
    .bind(seed.conversation_id)
    .bind(by)
    .execute(pool)
    .await
    .unwrap();
}

async fn owner_input(pool: &PgPool, seed: &Seed, text: &str) -> delivery::SubmittedInput {
    delivery::submit_input(
        pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: format!("owner:{}", Uuid::now_v7()),
            content: vec![ContentBlock::Text { text: text.into() }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap()
}

async fn dispatched_content(pool: &PgPool, seed: &Seed) -> Value {
    let outcome = dispatch_next(
        pool,
        seed.conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: Vec::new(),
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let DispatchOutcome::Dispatched { command_id, .. } = outcome else {
        panic!("input did not dispatch: {outcome:?}");
    };
    sqlx::query_scalar("select payload->'content' from commands where id=$1")
        .bind(command_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn plan_digest_rides_the_next_dispatched_input(pool: PgPool) {
    let seed = orchestrator(&pool).await;
    let executor = child(&pool, &seed).await;
    task(&pool, &seed, 1, "in_review", Some(executor), "3 hours").await;
    owner_input(&pool, &seed, "что дальше").await;
    let content = dispatched_content(&pool, &seed).await;
    let content = content.as_array().unwrap();
    assert_eq!(content.len(), 2, "{content:?}");
    let note = content[0]["text"].as_str().unwrap();
    assert!(note.starts_with("[canterlot] Plan check:"), "{note}");
    assert!(
        note.contains(&format!(
            "#1 \"Задача 1\" has waited in_review for 3h with an idle executor: session_accept {executor}"
        )),
        "{note}"
    );
    assert_eq!(content[1]["text"], "что дальше");
    let origin: Value =
        sqlx::query_scalar("select origin from inputs where conversation_id=$1 and service_note")
            .bind(seed.conversation_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        origin["event"],
        json!({"kind": "plan_reminder", "plan_tasks": [1]})
    );
}

async fn plan_digest_is_not_repeated_and_waits_three_hours(pool: PgPool) {
    let seed = orchestrator(&pool).await;
    let executor = child(&pool, &seed).await;
    let first = task(&pool, &seed, 1, "in_review", Some(executor), "1 hour").await;
    assert!(
        prepare(&pool, seed.conversation_id, "plan_reminder")
            .await
            .is_empty()
    );
    sqlx::query("update plan_tasks set updated_at = updated_at - interval '2 hours' where id=$1")
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
    let notes = prepare(&pool, seed.conversation_id, "plan_reminder").await;
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("#1 "), "{}", notes[0]);
    assert!(
        prepare(&pool, seed.conversation_id, "plan_reminder")
            .await
            .is_empty(),
        "an unchanged digest is not repeated"
    );

    task(&pool, &seed, 2, "blocked", None, "25 hours").await;
    finished_runs(&pool, seed.conversation_id, 5).await;
    task(&pool, &seed, 3, "in_progress", None, "5 hours").await;
    task(&pool, &seed, 4, "in_progress", Some(executor), "40 minutes").await;
    assert!(
        prepare(&pool, seed.conversation_id, "plan_reminder")
            .await
            .is_empty(),
        "three hours have not passed"
    );
    rewind_digest(&pool, &seed, "4 hours").await;
    let notes = prepare(&pool, seed.conversation_id, "plan_reminder").await;
    assert_eq!(notes.len(), 1);
    let note = &notes[0];
    assert!(!note.contains("#1 "), "already told: {note}");
    assert!(
        note.contains("#2 \"Задача 2\" is blocked and unchanged for 25h"),
        "{note}"
    );
    assert!(!note.contains("#3 "), "alone for under six hours: {note}");
    assert!(
        note.contains("#4 \"Задача 4\" is in_progress, but its executor is not running"),
        "{note}"
    );

    // Новая ревизия — новый факт: задача называется снова, когда подойдёт срок.
    sqlx::query("update plan_tasks set revision = revision + 1 where id=$1")
        .bind(first)
        .execute(&pool)
        .await
        .unwrap();
    rewind_digest(&pool, &seed, "4 hours").await;
    let notes = prepare(&pool, seed.conversation_id, "plan_reminder").await;
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].contains("#1 ") && !notes[0].contains("#2 "),
        "{}",
        notes[0]
    );
}

async fn orphaned_reviews_are_capped_and_the_setting_turns_them_off(pool: PgPool) {
    let seed = orchestrator(&pool).await;
    let executor = child(&pool, &seed).await;
    sqlx::query("update conversations set accepted_at = clock_timestamp() where id=$1")
        .bind(executor)
        .execute(&pool)
        .await
        .unwrap();
    for number in 1..=7 {
        task(
            &pool,
            &seed,
            number,
            "in_review",
            Some(executor),
            "1 minute",
        )
        .await;
    }
    let mut conn = pool.acquire().await.unwrap();
    reminders::replace_settings_in_tx(
        &mut conn,
        seed.owner_id,
        AgentReminderSettings {
            plan_enabled: false,
            todo_enabled: true,
        },
    )
    .await
    .unwrap();
    drop(conn);
    assert!(
        prepare(&pool, seed.conversation_id, "plan_reminder")
            .await
            .is_empty()
    );
    let mut conn = pool.acquire().await.unwrap();
    reminders::replace_settings_in_tx(&mut conn, seed.owner_id, AgentReminderSettings::default())
        .await
        .unwrap();
    drop(conn);
    let notes = prepare(&pool, seed.conversation_id, "plan_reminder").await;
    assert_eq!(notes.len(), 1);
    let note = &notes[0];
    assert!(
        note.contains(
            "#1 \"Задача 1\" is in_review, but its executor is accepted, archived or deleted"
        ),
        "{note}"
    );
    assert_eq!(note.lines().count(), 1 + 5 + 1, "{note}");
    assert!(
        note.ends_with("…and 2 more (plan_list shows all)."),
        "{note}"
    );
}

/// Основная дорожка списка задач с пунктами `(текст, состояние)`, обновлённая `age` назад.
async fn todo_list(pool: &PgPool, conversation_id: Uuid, items: &[(&str, &str)], age: &str) {
    let items: Vec<Value> = items
        .iter()
        .map(|(text, status)| json!({"text": text, "status": status}))
        .collect();
    sqlx::query(
        "insert into agent_todos as t (conversation_id, lane_block_id, generation,
                                       source_revision, revision, items, omitted, updated_at)
         values ($1, null, 1, 1, 1, $2, 0, clock_timestamp() - $3::interval)
         on conflict (conversation_id, lane_block_id) do update
         set revision = t.revision + 1, source_revision = t.source_revision + 1,
             items = excluded.items, updated_at = excluded.updated_at",
    )
    .bind(conversation_id)
    .bind(Value::Array(items))
    .bind(age)
    .execute(pool)
    .await
    .unwrap();
}

async fn rewind_todo_note(pool: &PgPool, conversation_id: Uuid, by: &str) {
    sqlx::query(
        "update agent_todos set reminded_at = reminded_at - $2::interval
         where conversation_id = $1 and lane_block_id is null",
    )
    .bind(conversation_id)
    .bind(by)
    .execute(pool)
    .await
    .unwrap();
}

async fn todo_in_progress_after_three_quiet_turns(pool: PgPool) {
    let seed = orchestrator(&pool).await;
    let id = seed.conversation_id;
    todo_list(
        &pool,
        id,
        &[
            ("Fix X", "in_progress"),
            ("Ship Y", "in_progress"),
            ("Z", "completed"),
        ],
        "1 hour",
    )
    .await;
    finished_runs(&pool, id, 2).await;
    assert!(
        prepare(&pool, id, "todo_reminder").await.is_empty(),
        "two turns are normal"
    );
    finished_runs(&pool, id, 1).await;
    let notes = prepare(&pool, id, "todo_reminder").await;
    assert_eq!(
        notes,
        [
            "[canterlot] Your todo list still shows 2 in progress (\"Fix X\", …) and has not changed for 3 turns. Update it if that no longer matches your work."
        ]
    );
    let origin: Value =
        sqlx::query_scalar("select origin from inputs where conversation_id=$1 and service_note")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        origin["event"],
        json!({"kind": "todo_reminder", "todo_revision": 1})
    );
    finished_runs(&pool, id, 10).await;
    rewind_todo_note(&pool, id, "4 hours").await;
    assert!(
        prepare(&pool, id, "todo_reminder").await.is_empty(),
        "one note per list revision"
    );

    // Новая ревизия, но с прошлой заметки не прошло трёх часов.
    rewind_todo_note(&pool, id, "-4 hours").await;
    todo_list(&pool, id, &[("Fix X", "in_progress")], "1 hour").await;
    finished_runs(&pool, id, 3).await;
    assert!(
        prepare(&pool, id, "todo_reminder").await.is_empty(),
        "three hours"
    );
    rewind_todo_note(&pool, id, "4 hours").await;
    let notes = prepare(&pool, id, "todo_reminder").await;
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].contains("1 in progress (\"Fix X\")"),
        "{}",
        notes[0]
    );
}

async fn todo_pending_only_after_ten_turns(pool: PgPool) {
    let seed = orchestrator(&pool).await;
    let id = seed.conversation_id;
    todo_list(&pool, id, &[("A", "pending"), ("B", "completed")], "1 hour").await;
    finished_runs(&pool, id, 9).await;
    assert!(prepare(&pool, id, "todo_reminder").await.is_empty());
    finished_runs(&pool, id, 1).await;
    assert_eq!(
        prepare(&pool, id, "todo_reminder").await,
        [
            "[canterlot] Your todo list has not changed for 10 turns (1 open). Update it if it no longer matches your work."
        ]
    );
}

async fn todo_reminder_skips_finished_lists_side_chats_and_the_setting(pool: PgPool) {
    let seed = orchestrator(&pool).await;
    let id = seed.conversation_id;
    todo_list(
        &pool,
        id,
        &[("A", "completed"), ("B", "cancelled")],
        "1 hour",
    )
    .await;
    finished_runs(&pool, id, 12).await;
    assert!(
        prepare(&pool, id, "todo_reminder").await.is_empty(),
        "finished list"
    );

    todo_list(&pool, id, &[("A", "in_progress")], "1 hour").await;
    finished_runs(&pool, id, 3).await;
    let mut conn = pool.acquire().await.unwrap();
    reminders::replace_settings_in_tx(
        &mut conn,
        seed.owner_id,
        AgentReminderSettings {
            plan_enabled: true,
            todo_enabled: false,
        },
    )
    .await
    .unwrap();
    drop(conn);
    assert!(
        prepare(&pool, id, "todo_reminder").await.is_empty(),
        "setting off"
    );

    let mut conn = pool.acquire().await.unwrap();
    reminders::replace_settings_in_tx(&mut conn, seed.owner_id, AgentReminderSettings::default())
        .await
        .unwrap();
    drop(conn);
    let parent = child(&pool, &seed).await;
    sqlx::query("update conversations set side_of = $2 where id = $1")
        .bind(id)
        .bind(parent)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        prepare(&pool, id, "todo_reminder").await.is_empty(),
        "side chat"
    );

    // Дорожка субагента не считается.
    let other = child(&pool, &seed).await;
    sqlx::query(
        "insert into agent_todos (conversation_id, lane_block_id, generation, source_revision,
                                  revision, items, omitted, updated_at)
         values ($1, $2, 1, 1, 1, '[{\"text\":\"A\",\"status\":\"in_progress\"}]', 0,
                 clock_timestamp() - interval '1 hour')",
    )
    .bind(other)
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        prepare(&pool, other, "todo_reminder").await.is_empty(),
        "subagent lane"
    );
}

/// Напоминания: настройка, план, список задач.
/// Перед каждым случаем база возвращается к снимку пустой базы.
#[tokio::test]
async fn agent_reminder_cases() {
    let pool = canterlot_hub::testkit::db().await;
    Snapshot::take(&pool).await;
    run_cases!(pool.clone(), before = restore;
        settings_default_on_and_round_trip,
        plan_digest_rides_the_next_dispatched_input,
        plan_digest_is_not_repeated_and_waits_three_hours,
        orphaned_reviews_are_capped_and_the_setting_turns_them_off,
        todo_in_progress_after_three_quiet_turns,
        todo_pending_only_after_ten_turns,
        todo_reminder_skips_finished_lists_side_chats_and_the_setting,
    );
}
