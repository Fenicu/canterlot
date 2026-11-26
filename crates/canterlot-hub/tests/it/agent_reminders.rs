//! Напоминания агентам служебной строкой: настройка владельца, устаревшие задачи плана
//! оркестратора и забытый список задач агента.

use canterlot_hub::{
    reminders::{self, AgentReminderSettings},
    testkit,
};
use serde_json::json;
use sqlx::PgPool;

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

/// Напоминания: настройка, план, список задач.
/// Перед каждым случаем база возвращается к снимку пустой базы.
#[tokio::test]
async fn agent_reminder_cases() {
    let pool = canterlot_hub::testkit::db().await;
    Snapshot::take(&pool).await;
    run_cases!(pool.clone(), before = restore;
        settings_default_on_and_round_trip,
    );
}
