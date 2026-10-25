use sqlx::{PgPool, types::Json};
use uuid::Uuid;

use crate::model::HoldReason;

pub struct Seed {
    pub owner_id: Uuid,
    pub machine_id: Uuid,
    pub conversation_id: Uuid,
}

pub async fn seed(pool: &PgPool) -> Seed {
    let owner_id = Uuid::now_v7();
    let machine_id = Uuid::now_v7();
    sqlx::query("insert into owners (id) values ($1)")
        .bind(owner_id)
        .execute(pool)
        .await
        .expect("seed owner");
    sqlx::query("insert into machines (id, owner_id, name) values ($1, $2, 'synthetic')")
        .bind(machine_id)
        .bind(owner_id)
        .execute(pool)
        .await
        .expect("seed machine");
    let conversation_id = seed_conversation(pool, owner_id, machine_id).await;
    Seed {
        owner_id,
        machine_id,
        conversation_id,
    }
}

pub async fn seed_conversation(pool: &PgPool, owner_id: Uuid, machine_id: Uuid) -> Uuid {
    let conversation_id = Uuid::now_v7();
    let mut transaction = pool.begin().await.expect("begin seed conversation");
    sqlx::query(
        "insert into conversations (id, owner_id, machine_id, harness, workdir)
         values ($1, $2, $3, 'synthetic', '/tmp/w')",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .bind(machine_id)
    .execute(&mut *transaction)
    .await
    .expect("seed conversation");
    sqlx::query("insert into queue_state (conversation_id, hold_reasons) values ($1, $2)")
        .bind(conversation_id)
        .bind(Json([HoldReason::NoLiveExecution]))
        .execute(&mut *transaction)
        .await
        .expect("seed queue state");
    transaction
        .commit()
        .await
        .expect("commit seed conversation");
    conversation_id
}
