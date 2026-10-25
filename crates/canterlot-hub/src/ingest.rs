use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use canterlot_proto::{
    limits::check_record_batch,
    worker::v1::{JournalRecords, record_body::Body},
};
use prost::Message;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    delivery::receipts::{
        InputReceipt, apply_input_receipt_in_tx, apply_run_finished_in_tx, sanitize_reason,
    },
    events::{self, NewEvent, Scope},
    executions::{self, ExecutionError},
    model::ExecutionState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalKind {
    Cj,
    Sj,
}

impl JournalKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cj => "cj",
            Self::Sj => "sj",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    Committed { up_to_seq: u64 },
    Gap { committed_seq: u64, got_seq: u64 },
}

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("unknown_record")]
    UnknownRecord,
    #[error("foreign_journal")]
    ForeignJournal,
    #[error("journal_kind_mismatch")]
    JournalKindMismatch,
    #[error("batch_limit")]
    BatchLimit,
    #[error("invalid_record")]
    InvalidRecord,
    #[error("execution")]
    Execution,
    #[error("database")]
    Database,
}

impl From<sqlx::Error> for IngestError {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}

impl From<ExecutionError> for IngestError {
    fn from(error: ExecutionError) -> Self {
        match error {
            ExecutionError::Db(_) => Self::Database,
            _ => Self::Execution,
        }
    }
}

#[derive(Clone)]
pub struct Ingest {
    pool: PgPool,
    dispatcher: Option<crate::dispatcher::Dispatcher>,
    ignored: Arc<AtomicU64>,
    #[cfg(feature = "fault-injection")]
    pub faults: crate::fault_injection::Faults,
}

impl Ingest {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            dispatcher: None,
            ignored: Arc::new(AtomicU64::new(0)),
            #[cfg(feature = "fault-injection")]
            faults: Default::default(),
        }
    }

    pub fn ignored_record_count(&self) -> u64 {
        self.ignored.load(Ordering::Relaxed)
    }

    pub fn with_dispatcher(mut self, dispatcher: crate::dispatcher::Dispatcher) -> Self {
        self.dispatcher = Some(dispatcher);
        self
    }

    pub async fn apply_batch(
        &self,
        machine_id: Uuid,
        kind: JournalKind,
        batch: &JournalRecords,
    ) -> Result<IngestOutcome, IngestError> {
        check_record_batch(batch).map_err(|_| IngestError::BatchLimit)?;
        let journal_id = parse_id(&batch.journal_id)?;
        if batch.records.iter().any(|record| record.seq == 0) {
            return Err(IngestError::InvalidRecord);
        }
        let rows: Vec<CursorRow> = sqlx::query_as(
            "select machine_id, kind, committed_seq, last_record_digest from journal_cursors where journal_id = $1",
        )
        .bind(journal_id)
        .fetch_all(&self.pool)
        .await?;
        let (mut up_to_seq, mut last_digest) = checked_cursor(machine_id, kind, rows)?;
        for record in &batch.records {
            // Записи старше снимка не требуют сравнения с последним digest.
            if record.seq < up_to_seq {
                continue;
            }
            let digest = record
                .body
                .as_ref()
                .map(|body| Sha256::digest(body.encode_to_vec()).to_vec());
            if record.seq == up_to_seq && (last_digest.is_none() || last_digest == digest) {
                continue;
            }
            let mut tx = self.pool.begin().await?;
            sqlx::query("insert into journal_cursors (machine_id, journal_id, kind, committed_seq) values ($1, $2, $3, 0) on conflict do nothing")
                .bind(machine_id).bind(journal_id).bind(kind.as_str()).execute(&mut *tx).await?;
            let rows: Vec<CursorRow> = sqlx::query_as(
                "select machine_id, kind, committed_seq, last_record_digest from journal_cursors where journal_id = $1 for update",
            ).bind(journal_id).fetch_all(&mut *tx).await?;
            (up_to_seq, last_digest) = checked_cursor(machine_id, kind, rows)?;
            if record.seq <= up_to_seq {
                if record.seq == up_to_seq && last_digest.is_some() && last_digest != digest {
                    let owner_id =
                        sqlx::query_scalar("select owner_id from machines where id = $1")
                            .bind(machine_id)
                            .fetch_one(&mut *tx)
                            .await?;
                    events::append(
                        &mut tx,
                        NewEvent {
                            scope: Scope::Conversations { owner_id },
                            kind: "journal_record_mismatch",
                            conversation_id: None,
                            execution_id: None,
                            payload: json!({"journal_id": journal_id, "seq": record.seq}),
                            correlation_id: Uuid::now_v7(),
                        },
                    )
                    .await?;
                }
                tx.commit().await?;
                continue;
            }
            if record.seq > up_to_seq + 1 {
                return Ok(IngestOutcome::Gap {
                    committed_seq: up_to_seq,
                    got_seq: record.seq,
                });
            }
            let seq = i64::try_from(record.seq).map_err(|_| IngestError::InvalidRecord)?;
            let body = record
                .body
                .as_ref()
                .and_then(|body| body.body.as_ref())
                .ok_or(IngestError::UnknownRecord)?;
            let ignored = apply_body(&mut tx, machine_id, body, Uuid::now_v7()).await?;
            sqlx::query("update journal_cursors set committed_seq = $3, last_record_digest = $4, updated_at = clock_timestamp() where machine_id = $1 and journal_id = $2")
                .bind(machine_id).bind(journal_id).bind(seq).bind(&digest).execute(&mut *tx).await?;
            tx.commit().await?;
            up_to_seq = record.seq;
            last_digest = digest;
            if ignored {
                self.ignored.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Some(dispatcher) = &self.dispatcher {
            for conversation in
                crate::dispatcher::queued_conversations(&self.pool, Some(machine_id)).await?
            {
                dispatcher.trigger(conversation);
            }
        }
        Ok(IngestOutcome::Committed { up_to_seq })
    }
}

type CursorRow = (Uuid, String, i64, Option<Vec<u8>>);

fn checked_cursor(
    machine_id: Uuid,
    kind: JournalKind,
    rows: Vec<CursorRow>,
) -> Result<(u64, Option<Vec<u8>>), IngestError> {
    let mut committed = 0;
    let mut digest = None;
    for (owner, stored_kind, seq, last_digest) in rows {
        if owner != machine_id {
            return Err(IngestError::ForeignJournal);
        }
        if stored_kind != kind.as_str() {
            return Err(IngestError::JournalKindMismatch);
        }
        committed = u64::try_from(seq).map_err(|_| IngestError::Database)?;
        digest = last_digest;
    }
    Ok((committed, digest))
}

fn parse_id(value: &str) -> Result<Uuid, IngestError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or(IngestError::InvalidRecord)
}

fn attempt(value: u32) -> Result<i32, IngestError> {
    i32::try_from(value).map_err(|_| IngestError::InvalidRecord)
}

#[derive(sqlx::FromRow)]
struct CommandTarget {
    kind: String,
    execution_id: Option<Uuid>,
    dispatch_attempt: i32,
    delivery_state: String,
}

async fn command(
    conn: &mut PgConnection,
    machine_id: Uuid,
    command_id: Uuid,
) -> Result<Option<CommandTarget>, IngestError> {
    Ok(sqlx::query_as("select kind, execution_id, dispatch_attempt, delivery_state from commands where id = $1 and machine_id = $2")
        .bind(command_id).bind(machine_id).fetch_optional(conn).await?)
}

async fn receipt(
    conn: &mut PgConnection,
    machine_id: Uuid,
    command_id: &str,
    dispatch_attempt: u32,
    receipt: InputReceipt,
    correlation_id: Uuid,
) -> Result<(), IngestError> {
    let command_id = parse_id(command_id)?;
    let dispatch_attempt = attempt(dispatch_attempt)?;
    if command(conn, machine_id, command_id).await?.is_some() {
        apply_input_receipt_in_tx(conn, command_id, dispatch_attempt, receipt, correlation_id)
            .await?;
    }
    Ok(())
}

async fn apply_body(
    conn: &mut PgConnection,
    machine_id: Uuid,
    body: &Body,
    correlation_id: Uuid,
) -> Result<bool, IngestError> {
    match body {
        Body::CommandStored(fact) => {
            let command_id = parse_id(&fact.command_id)?;
            let dispatch_attempt = attempt(fact.dispatch_attempt)?;
            if let Some(target) = command(conn, machine_id, command_id).await? {
                if target.kind == "deliver_input" {
                    apply_input_receipt_in_tx(
                        conn,
                        command_id,
                        dispatch_attempt,
                        InputReceipt::Stored,
                        correlation_id,
                    )
                    .await?;
                } else {
                    sqlx::query("update commands set delivery_state = 'stored_on_machine', stored_at = coalesce(stored_at, now()) where id = $1 and dispatch_attempt = $2 and delivery_state = 'pending'")
                        .bind(command_id).bind(dispatch_attempt).execute(&mut *conn).await?;
                }
            }
        }
        Body::CommandRejected(fact) => {
            let command_id = parse_id(&fact.command_id)?;
            let dispatch_attempt = attempt(fact.dispatch_attempt)?;
            if let Some(target) = command(conn, machine_id, command_id).await? {
                if target.kind == "deliver_input" {
                    apply_input_receipt_in_tx(
                        conn,
                        command_id,
                        dispatch_attempt,
                        InputReceipt::RejectedBeforeTransmission {
                            reason: fact.reason.clone(),
                        },
                        correlation_id,
                    )
                    .await?;
                } else if target.kind == "start_execution" {
                    reject_start(
                        conn,
                        machine_id,
                        command_id,
                        dispatch_attempt,
                        &fact.reason,
                        correlation_id,
                    )
                    .await?;
                } else {
                    complete_rejected(conn, command_id, dispatch_attempt, &fact.reason).await?;
                }
            }
        }
        Body::DispatchDeferred(fact) => {
            receipt(
                conn,
                machine_id,
                &fact.command_id,
                fact.dispatch_attempt,
                InputReceipt::DispatchDeferred {
                    reason: fact.reason.clone(),
                },
                correlation_id,
            )
            .await?
        }
        Body::TransmissionStarted(fact) => {
            receipt(
                conn,
                machine_id,
                &fact.command_id,
                fact.dispatch_attempt,
                InputReceipt::TransmissionStarted,
                correlation_id,
            )
            .await?
        }
        Body::InputAccepted(fact) => {
            receipt(
                conn,
                machine_id,
                &fact.command_id,
                fact.dispatch_attempt,
                InputReceipt::Accepted,
                correlation_id,
            )
            .await?
        }
        Body::AcceptanceUnconfirmed(fact) => {
            receipt(
                conn,
                machine_id,
                &fact.command_id,
                fact.dispatch_attempt,
                InputReceipt::AcceptanceUndetermined,
                correlation_id,
            )
            .await?
        }
        Body::CancelWon(fact) => {
            receipt(
                conn,
                machine_id,
                &fact.command_id,
                fact.dispatch_attempt,
                InputReceipt::CancelWon,
                correlation_id,
            )
            .await?
        }
        Body::CancelLost(fact) => {
            receipt(
                conn,
                machine_id,
                &fact.command_id,
                fact.dispatch_attempt,
                InputReceipt::CancelLost,
                correlation_id,
            )
            .await?
        }
        Body::InputWithdrawn(fact) => {
            receipt(
                conn,
                machine_id,
                &fact.command_id,
                fact.dispatch_attempt,
                InputReceipt::Withdrawn,
                correlation_id,
            )
            .await?
        }
        Body::InputRejected(fact) => {
            receipt(
                conn,
                machine_id,
                &fact.command_id,
                fact.dispatch_attempt,
                InputReceipt::ExplicitRejection {
                    reason: fact.reason.clone(),
                },
                correlation_id,
            )
            .await?
        }
        Body::ExecutionState(fact) => {
            let execution_id = parse_id(&fact.execution_id)?;
            let generation =
                i64::try_from(fact.generation).map_err(|_| IngestError::InvalidRecord)?;
            let to = fact.state.parse().map_err(|_| IngestError::InvalidRecord)?;
            let owned: Option<Uuid> = sqlx::query_scalar(
                "select e.id from executions e join conversations c on c.id = e.conversation_id where e.id = $1 and e.generation = $2 and c.machine_id = $3",
            ).bind(execution_id).bind(generation).bind(machine_id).fetch_optional(&mut *conn).await?;
            if owned.is_some() {
                let outcome = (!fact.outcome.is_empty()).then_some(fact.outcome.as_str());
                apply_execution_fact(conn, execution_id, to, outcome, correlation_id).await?;
            }
        }
        Body::RunFinished(fact) => {
            let execution_id = parse_id(&fact.execution_id)?;
            let command_id = parse_id(&fact.input_command_id)?;
            if command(conn, machine_id, command_id).await?.is_some() {
                apply_run_finished_in_tx(conn, execution_id, command_id, correlation_id).await?;
            }
        }
        Body::JournalClosed(_)
        | Body::BlockStarted(_)
        | Body::BlockDelta(_)
        | Body::BlockFinal(_)
        | Body::BlockIncomplete(_)
        | Body::PhaseChanged(_) => return Ok(true),
    }
    Ok(false)
}

async fn reject_start(
    conn: &mut PgConnection,
    machine_id: Uuid,
    command_id: Uuid,
    dispatch_attempt: i32,
    reason: &str,
    correlation_id: Uuid,
) -> Result<(), IngestError> {
    // Очередь блокируется раньше чтения команды: тот же порядок, что у квитанций и диспетчера.
    sqlx::query(
        "select q.conversation_id from queue_state q join commands c on c.conversation_id = q.conversation_id where c.id = $1 for update of q",
    )
    .bind(command_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(target) = command(conn, machine_id, command_id).await? else {
        return Ok(());
    };
    if target.delivery_state == "completed" || target.dispatch_attempt != dispatch_attempt {
        return Ok(());
    }
    if let Some(execution_id) = target.execution_id {
        apply_execution_fact(
            conn,
            execution_id,
            ExecutionState::Failed,
            Some("start_rejected"),
            correlation_id,
        )
        .await?;
    }
    complete_rejected(conn, command_id, dispatch_attempt, reason).await
}

async fn complete_rejected(
    conn: &mut PgConnection,
    command_id: Uuid,
    dispatch_attempt: i32,
    reason: &str,
) -> Result<(), IngestError> {
    sqlx::query("update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, now()), result = $3 where id = $1 and dispatch_attempt = $2 and delivery_state <> 'completed'")
        .bind(command_id).bind(dispatch_attempt).bind(json!({"rejected": sanitize_reason(reason)})).execute(&mut *conn).await?;
    Ok(())
}

async fn apply_execution_fact(
    conn: &mut PgConnection,
    execution_id: Uuid,
    to: ExecutionState,
    outcome: Option<&str>,
    correlation_id: Uuid,
) -> Result<(), IngestError> {
    let current: Option<(Uuid, String, i64)> =
        sqlx::query_as("select conversation_id, state, generation from executions where id = $1")
            .bind(execution_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some((conversation_id, state, generation)) = current else {
        return Ok(());
    };
    if state == to.as_str() {
        return Ok(());
    }
    match executions::set_state_in_tx(conn, execution_id, to, outcome, correlation_id).await {
        Ok(()) => Ok(()),
        Err(ExecutionError::InvalidTransition { from, to }) => {
            events::append(
                conn,
                NewEvent {
                    scope: Scope::Conversation {
                        id: conversation_id,
                    },
                    kind: "execution_fact_rejected",
                    conversation_id: Some(conversation_id),
                    execution_id: Some(execution_id),
                    payload: json!({"from": from, "to": to, "generation": generation}),
                    correlation_id,
                },
            )
            .await?;
            Ok(())
        }
        Err(ExecutionError::Db(sqlx::Error::RowNotFound)) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

impl Ingest {
    async fn range_ack(
        &self,
        machine_id: Uuid,
        kind: JournalKind,
        batch: JournalRecords,
    ) -> Result<canterlot_proto::worker::v1::JournalRangeAck, tonic::Status> {
        let outcome = self
            .apply_batch(machine_id, kind, &batch)
            .await
            .map_err(|error| {
                let message = error.to_string();
                match error {
                    IngestError::BatchLimit => tonic::Status::resource_exhausted(message),
                    IngestError::ForeignJournal => tonic::Status::permission_denied(message),
                    IngestError::Database | IngestError::Execution => {
                        tonic::Status::internal(message)
                    }
                    IngestError::UnknownRecord
                    | IngestError::InvalidRecord
                    | IngestError::JournalKindMismatch => tonic::Status::invalid_argument(message),
                }
            })?;
        match outcome {
            IngestOutcome::Committed { up_to_seq } => {
                Ok(canterlot_proto::worker::v1::JournalRangeAck {
                    journal_id: batch.journal_id,
                    up_to_seq,
                })
            }
            IngestOutcome::Gap { .. } => Err(tonic::Status::failed_precondition("journal_gap")),
        }
    }
}

#[tonic::async_trait]
impl crate::grpc::Ingest for Ingest {
    async fn cj_records(
        &self,
        machine_id: Uuid,
        records: JournalRecords,
    ) -> Result<canterlot_proto::worker::v1::JournalRangeAck, tonic::Status> {
        #[cfg(feature = "fault-injection")]
        {
            self.faults.batch(records.clone());
            self.faults
                .pause(crate::fault_injection::Point::BeforeCjCommit)
                .await;
        }
        self.range_ack(machine_id, JournalKind::Cj, records).await
    }

    async fn sj_records(
        &self,
        machine_id: Uuid,
        records: JournalRecords,
    ) -> Result<canterlot_proto::worker::v1::JournalRangeAck, tonic::Status> {
        self.range_ack(machine_id, JournalKind::Sj, records).await
    }
}
