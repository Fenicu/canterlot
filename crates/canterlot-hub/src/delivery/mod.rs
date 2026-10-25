pub mod dispatch;
pub mod receipts;
pub mod submit;

pub use submit::{
    ContentBlock, LimitKind, QUEUE_LIMIT, SubmitError, SubmitInput, SubmittedInput, submit_input,
    submit_input_in_tx,
};

use sqlx::{PgConnection, types::Json};
use uuid::Uuid;

use crate::model::{HoldReason, PauseReason, RunSlot};

#[derive(sqlx::FromRow)]
struct LockedQueue {
    conversation_id: Uuid,
    closed: bool,
    pause_reasons: Json<Vec<PauseReason>>,
    hold_reasons: Json<Vec<HoldReason>>,
    run_slot: String,
    run_slot_input_id: Option<Uuid>,
    run_slot_attempt: Option<i32>,
}

impl LockedQueue {
    fn slot(&self) -> Result<RunSlot, sqlx::Error> {
        RunSlot::from_columns(
            &self.run_slot,
            self.run_slot_input_id,
            self.run_slot_attempt,
        )
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))
    }

    async fn save(&mut self, conn: &mut PgConnection, slot: &RunSlot) -> Result<(), sqlx::Error> {
        if *slot == RunSlot::Free {
            self.hold_reasons
                .0
                .retain(|reason| *reason != HoldReason::RunInProgress);
        }
        let (name, input_id, attempt) = slot.to_columns();
        sqlx::query(
            "update queue_state set pause_reasons = $2, hold_reasons = $3,
                    run_slot = $4, run_slot_input_id = $5, run_slot_attempt = $6
             where conversation_id = $1",
        )
        .bind(self.conversation_id)
        .bind(&self.pause_reasons)
        .bind(&self.hold_reasons)
        .bind(name)
        .bind(input_id)
        .bind(attempt)
        .execute(conn)
        .await?;
        Ok(())
    }
}
