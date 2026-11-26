//! Группа `it_rest`: удержание и вложения, тревоги и напоминания, исходящие и карточки плана,
//! долгие сценарии и прочее, что не попало в доменные группы.
//!
//! Модули остаются на своих местах в `tests/it/`, корень только объявляет их долю. Так правка
//! одного модуля пересобирает эту группу, а не общий стоблочный бинарник `it`.

#![forbid(unsafe_code)]

// Поддержка общая для всех бинарников: в этой группе часть её не нужна.
#[allow(dead_code)]
#[path = "it/support/mod.rs"]
mod support;

#[path = "it/accepted_retention.rs"]
mod accepted_retention;
#[path = "it/accepted_retention_recheck.rs"]
mod accepted_retention_recheck;
#[path = "it/activity_refresh.rs"]
mod activity_refresh;
#[path = "it/agent_calls.rs"]
mod agent_calls;
#[path = "it/agent_facts.rs"]
mod agent_facts;
#[path = "it/agent_reminders.rs"]
mod agent_reminders;
#[path = "it/alarms.rs"]
mod alarms;
#[path = "it/attachments.rs"]
mod attachments;
#[path = "it/attachments_gc.rs"]
mod attachments_gc;
#[path = "it/attachments_gc_drafts.rs"]
mod attachments_gc_drafts;
#[path = "it/background_tasks.rs"]
mod background_tasks;
#[path = "it/crunch.rs"]
mod crunch;
#[path = "it/crunch_http.rs"]
mod crunch_http;
#[path = "it/db_tx.rs"]
mod db_tx;
#[path = "it/dictation.rs"]
mod dictation;
#[path = "it/event_log.rs"]
mod event_log;
#[path = "it/file_probe.rs"]
mod file_probe;
#[path = "it/git_facts.rs"]
mod git_facts;
#[path = "it/held_delivery.rs"]
mod held_delivery;
#[path = "it/hibernate.rs"]
mod hibernate;
#[path = "it/hub_cli.rs"]
mod hub_cli;
#[path = "it/hung_rescue.rs"]
mod hung_rescue;
#[path = "it/nudge.rs"]
mod nudge;
#[path = "it/nudge_http.rs"]
mod nudge_http;
#[path = "it/outbox.rs"]
mod outbox;
#[path = "it/plan_board.rs"]
mod plan_board;
#[path = "it/poison_records.rs"]
mod poison_records;
#[path = "it/probe.rs"]
mod probe;
#[path = "it/queue_controls.rs"]
mod queue_controls;
#[path = "it/receipts.rs"]
mod receipts;
#[path = "it/resume_at_reset.rs"]
mod resume_at_reset;
#[path = "it/retention_keyset.rs"]
mod retention_keyset;
#[path = "it/scheduled_messages.rs"]
mod scheduled_messages;
#[path = "it/send_now.rs"]
mod send_now;
#[path = "it/singletons.rs"]
mod singletons;
#[path = "it/sj_ingest.rs"]
mod sj_ingest;
#[path = "it/snooze.rs"]
mod snooze;
#[path = "it/submit_validation.rs"]
mod submit_validation;

/// Число кругов нагрузочного теста: полное в профиле `stress`, короткое в обычном
/// гейте. `CANTERLOT_STRESS_ROUNDS` перекрывает оба значения.
fn stress_rounds(full: u32) -> u32 {
    if let Some(rounds) = std::env::var("CANTERLOT_STRESS_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        return rounds;
    }
    if std::env::var("NEXTEST_PROFILE").as_deref() == Ok("stress") {
        full
    } else {
        10
    }
}
