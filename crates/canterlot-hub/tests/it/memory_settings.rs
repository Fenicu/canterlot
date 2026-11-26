use canterlot_hub::{
    executions,
    memory::auto_memory::{self, ClaudeAutoMemory, ClaudeAutoMemorySettings},
    outbox::{CommandRow, to_proto_for_session},
    session::Registry,
    testkit::{self, Seed},
};
use canterlot_proto::worker::v1::command;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::cases::{Snapshot, restore, run_cases};

async fn claude_seed(pool: &PgPool) -> Seed {
    let seed = testkit::seed(pool).await;
    sqlx::query("update conversations set harness = 'claude_code' where id = $1")
        .bind(seed.conversation_id)
        .execute(pool)
        .await
        .unwrap();
    seed
}

async fn put(pool: &PgPool, owner: Uuid, value: ClaudeAutoMemory) {
    let mut conn = pool.acquire().await.unwrap();
    auto_memory::replace_settings_in_tx(
        &mut conn,
        owner,
        ClaudeAutoMemorySettings {
            claude_auto_memory: value,
        },
    )
    .await
    .unwrap();
}

async fn start_command(pool: &PgPool, seed: &Seed) -> (executions::StartedExecution, Value) {
    let execution = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    let payload = sqlx::query_scalar("select payload from commands where id = $1")
        .bind(execution.command_id)
        .fetch_one(pool)
        .await
        .unwrap();
    (execution, payload)
}

async fn owner_toggle_round_trip(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        auto_memory::settings(&mut conn, seed.owner_id)
            .await
            .unwrap()
            .claude_auto_memory,
        ClaudeAutoMemory::On,
        "по умолчанию включена"
    );
    drop(conn);
    put(&pool, seed.owner_id, ClaudeAutoMemory::Off).await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        auto_memory::settings(&mut conn, seed.owner_id)
            .await
            .unwrap()
            .claude_auto_memory,
        ClaudeAutoMemory::Off
    );
    drop(conn);
    put(&pool, seed.owner_id, ClaudeAutoMemory::On).await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        auto_memory::settings(&mut conn, seed.owner_id)
            .await
            .unwrap()
            .claude_auto_memory,
        ClaudeAutoMemory::On
    );
    let wire = serde_json::to_value(ClaudeAutoMemorySettings {
        claude_auto_memory: ClaudeAutoMemory::Off,
    })
    .unwrap();
    assert_eq!(wire, serde_json::json!({"claude_auto_memory": "off"}));
    assert!(
        serde_json::from_value::<ClaudeAutoMemorySettings>(
            serde_json::json!({"claude_auto_memory": "maybe"})
        )
        .is_err()
    );
}

async fn start_execution_carries_flag_only_for_capable_workers(pool: PgPool) {
    let seed = claude_seed(&pool).await;
    put(&pool, seed.owner_id, ClaudeAutoMemory::Off).await;
    let (execution, payload) = start_command(&pool, &seed).await;
    assert_eq!(payload["disable_claude_auto_memory"], true);
    let row = CommandRow {
        id: execution.command_id,
        machine_id: Uuid::now_v7(),
        conversation_id: Some(seed.conversation_id),
        execution_id: Some(execution.execution_id),
        execution_generation: Some(execution.generation),
        kind: "start_execution".into(),
        payload,
        dispatch_attempt: 1,
    };
    for (capabilities, expected) in [
        (vec!["pilot_v1", "instructions_v1"], false),
        (vec!["pilot_v1", "instructions_v1", "memory_scope_v1"], true),
    ] {
        let registry = Registry::new(Default::default());
        let _session = registry.connect_for_tests(
            row.machine_id,
            &capabilities
                .iter()
                .map(|c| (*c).to_owned())
                .collect::<Vec<_>>(),
        );
        let command = to_proto_for_session(&row, &registry).unwrap();
        let Some(command::Kind::StartExecution(start)) = command.kind else {
            panic!("{command:?}");
        };
        assert_eq!(
            start.disable_claude_auto_memory, expected,
            "{capabilities:?}"
        );
    }
}

async fn default_setting_sends_no_flag(pool: PgPool) {
    let seed = claude_seed(&pool).await;
    let (_, payload) = start_command(&pool, &seed).await;
    assert!(payload.get("disable_claude_auto_memory").is_none());
}

async fn non_claude_harness_never_gets_the_flag(pool: PgPool) {
    // Синтетический харнес сида — не Claude Code.
    let seed = testkit::seed(&pool).await;
    put(&pool, seed.owner_id, ClaudeAutoMemory::Off).await;
    let (_, payload) = start_command(&pool, &seed).await;
    assert!(payload.get("disable_claude_auto_memory").is_none());
}

/// Переключатель памяти владельца и флаг запуска: только для способных воркеров и
/// только для Claude Code.
/// Перед каждым случаем база возвращается к снимку пустой базы.
#[tokio::test]
async fn memory_settings_cases() {
    let pool = canterlot_hub::testkit::db().await;
    Snapshot::take(&pool).await;
    run_cases!(pool.clone(), before = restore;
        owner_toggle_round_trip,
        start_execution_carries_flag_only_for_capable_workers,
        default_setting_sends_no_flag,
        non_claude_harness_never_gets_the_flag,
    );
}
