pub mod control;
pub mod dispatch;
pub mod receipts;
pub mod submit;

pub use submit::{
    ContentBlock, LimitKind, QUEUE_LIMIT, SubmitError, SubmitInput, SubmittedInput,
    submit_forwarded_message_input_in_tx, submit_input, submit_input_in_tx,
};

use sqlx::{PgConnection, types::Json};
use uuid::Uuid;

use crate::events::{self, NewEvent, Scope};
use crate::model::{HoldReason, PauseReason, RunSlot};
use serde_json::json;

#[derive(sqlx::FromRow)]
pub(crate) struct LockedQueue {
    pub(crate) conversation_id: Uuid,
    pub(crate) closed: bool,
    pub(crate) pause_reasons: Json<Vec<PauseReason>>,
    pub(crate) hold_reasons: Json<Vec<HoldReason>>,
    run_slot: String,
    run_slot_input_id: Option<Uuid>,
    run_slot_attempt: Option<i32>,
}

impl LockedQueue {
    pub(crate) fn slot(&self) -> Result<RunSlot, sqlx::Error> {
        RunSlot::from_columns(
            &self.run_slot,
            self.run_slot_input_id,
            self.run_slot_attempt,
        )
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))
    }

    pub(crate) async fn save(
        &mut self,
        conn: &mut PgConnection,
        slot: &RunSlot,
        execution_id: Option<Uuid>,
        correlation_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        if *slot == RunSlot::Free {
            self.hold_reasons
                .0
                .retain(|reason| *reason != HoldReason::RunInProgress);
        }
        let (name, input_id, attempt) = slot.to_columns();
        let changed = sqlx::query(
            "update queue_state set pause_reasons = $2, hold_reasons = $3,
                    run_slot = $4, run_slot_input_id = $5, run_slot_attempt = $6,
                    run_slot_since = case
                        when (run_slot, run_slot_input_id, run_slot_attempt)
                            is distinct from ($4::text, $5::uuid, $6::integer)
                        then clock_timestamp()
                        else run_slot_since
                    end
             where conversation_id = $1
               and (pause_reasons, hold_reasons, run_slot, run_slot_input_id, run_slot_attempt)
                   is distinct from ($2::jsonb, $3::jsonb, $4::text, $5::uuid, $6::integer)",
        )
        .bind(self.conversation_id)
        .bind(&self.pause_reasons)
        .bind(&self.hold_reasons)
        .bind(name)
        .bind(input_id)
        .bind(attempt)
        .execute(&mut *conn)
        .await?
        .rows_affected()
            != 0;
        if changed {
            events::append(
                conn,
                NewEvent {
                    scope: Scope::Conversation {
                        id: self.conversation_id,
                    },
                    kind: "queue_state_changed",
                    conversation_id: Some(self.conversation_id),
                    execution_id,
                    payload: json!({
                        "pause_reasons": self.pause_reasons.0,
                        "hold_reasons": self.hold_reasons.0,
                        "run_slot": slot,
                    }),
                    correlation_id,
                },
            )
            .await?;
        }
        Ok(())
    }
}
