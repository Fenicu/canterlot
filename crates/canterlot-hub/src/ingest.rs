use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use canterlot_proto::{
    limits::{
        BLOCK_FINAL_INLINE_MAX_BYTES, MAX_BLOCK_DELTA_BYTES, MAX_SJ_BATCH_RECORDS,
        check_record_batch,
    },
    worker::v1::{JournalRecords, ReconcileReport, record_body::Body},
};
use prost::Message;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    blocks::{self, BlockOutcome, ExecutionRef},
    delivery::receipts::{
        InputReceipt, apply_input_receipt_in_tx, apply_run_finished_in_tx, sanitize_reason,
        store_machine_time_in_tx,
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
    #[error("credential_revoked")]
    CredentialRevoked,
    #[error("unknown_record")]
    UnknownRecord,
    #[error("foreign_journal")]
    ForeignJournal,
    #[error("foreign_execution")]
    ForeignExecution,
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

    pub async fn reconcile(
        &self,
        machine_id: Uuid,
        report: &ReconcileReport,
    ) -> Result<(), IngestError> {
        let observations = report
            .executions
            .iter()
            .map(|observation| {
                Ok((
                    parse_id(&observation.execution_id)?,
                    i64::try_from(observation.generation)
                        .map_err(|_| IngestError::InvalidRecord)?,
                    observation
                        .state
                        .parse::<ExecutionState>()
                        .map_err(|_| IngestError::InvalidRecord)?,
                    observation,
                ))
            })
            .collect::<Result<Vec<_>, IngestError>>()?;
        let mut tx = self.pool.begin().await?;
        let owner_id: Uuid = sqlx::query_scalar("select owner_id from machines where id = $1")
            .bind(machine_id)
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query(
            "select q.conversation_id from queue_state q join conversations c on c.id = q.conversation_id
             where c.machine_id = $1 order by q.conversation_id for update of q",
        ).bind(machine_id).fetch_all(&mut *tx).await?;
        let executions: Vec<(Uuid, Uuid, i64, String)> = sqlx::query_as(
            "select e.id, e.conversation_id, e.generation, e.state from executions e
             join conversations c on c.id = e.conversation_id where c.machine_id = $1",
        )
        .bind(machine_id)
        .fetch_all(&mut *tx)
        .await?;
        for (id, generation, worker_state, observation) in &observations {
            let known = executions
                .iter()
                .find(|(execution_id, _, _, _)| execution_id == id);
            let Some((_, _, hub_generation, hub_state)) = known else {
                events::append(&mut tx, NewEvent {
                    scope: Scope::Conversations { owner_id }, kind: "state_divergence",
                    conversation_id: None, execution_id: None,
                    payload: json!({"machine_id": machine_id, "execution_id": id, "generation": generation, "reason": "unknown_execution"}),
                    correlation_id: Uuid::now_v7(),
                }).await?;
                continue;
            };
            if generation != hub_generation || matches!(hub_state.as_str(), "stopped" | "failed") {
                continue;
            }
            if matches!(
                worker_state,
                ExecutionState::Stopped | ExecutionState::Failed | ExecutionState::OwnerLost
            ) {
                if *worker_state != ExecutionState::OwnerLost {
                    let journal_id = Uuid::parse_str(&observation.sj_journal_id).ok();
                    let closed: bool = sqlx::query_scalar("select exists(select 1 from journal_cursors where execution_id = $1 and journal_id = $2 and closed)")
                        .bind(id).bind(journal_id).fetch_one(&mut *tx).await?;
                    if !closed {
                        continue;
                    }
                }
                apply_body(
                    &mut tx,
                    machine_id,
                    &Body::ExecutionState(canterlot_proto::worker::v1::ExecutionState {
                        execution_id: id.to_string(),
                        generation: observation.generation,
                        state: observation.state.clone(),
                        outcome: observation.outcome.clone(),
                        ..Default::default()
                    }),
                    Uuid::now_v7(),
                )
                .await?;
            } else {
                sqlx::query("update executions set observed_at = now() where id = $1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        for (execution_id, conversation_id, generation, state) in executions {
            if matches!(state.as_str(), "starting" | "running" | "stopping")
                && !observations.iter().any(|(id, observed_generation, _, _)| {
                    *id == execution_id && *observed_generation == generation
                })
            {
                let correlation_id = Uuid::now_v7();
                executions::set_state_in_tx(
                    &mut tx,
                    execution_id,
                    ExecutionState::OwnerLost,
                    None,
                    correlation_id,
                )
                .await?;
                complete_unsuccessful_start(&mut tx, execution_id, machine_id, "owner_lost")
                    .await?;
                events::append(
                    &mut tx,
                    NewEvent {
                        scope: Scope::Conversation {
                            id: conversation_id,
                        },
                        kind: "state_divergence",
                        conversation_id: Some(conversation_id),
                        execution_id: Some(execution_id),
                        payload: json!({"reason": "execution_unknown_to_worker"}),
                        correlation_id,
                    },
                )
                .await?;
            }
        }
        tx.commit().await?;
        if let Some(dispatcher) = &self.dispatcher {
            for conversation in
                crate::dispatcher::queued_conversations(&self.pool, Some(machine_id)).await?
            {
                dispatcher.trigger(conversation);
            }
        }
        Ok(())
    }

    pub async fn apply_batch(
        &self,
        machine_id: Uuid,
        credential: &[u8],
        kind: JournalKind,
        batch: &JournalRecords,
    ) -> Result<IngestOutcome, IngestError> {
        self.apply_batch_report(machine_id, credential, kind, batch)
            .await
            .0
    }

    async fn apply_batch_report(
        &self,
        machine_id: Uuid,
        credential: &[u8],
        kind: JournalKind,
        batch: &JournalRecords,
    ) -> (Result<IngestOutcome, IngestError>, Option<u64>) {
        let mut prefix = None;
        let result = self
            .apply_batch_inner(machine_id, credential, kind, batch, &mut prefix)
            .await;
        match &result {
            Err(error) => tracing::warn!(kind = %error, machine_id = %machine_id,
                journal_id = ?Uuid::parse_str(&batch.journal_id).ok(), "ingest rejected"),
            Ok(IngestOutcome::Gap { .. }) => {
                tracing::warn!(kind = "journal_gap", machine_id = %machine_id,
                journal_id = ?Uuid::parse_str(&batch.journal_id).ok(), "ingest gap")
            }
            _ => {}
        }
        (result, prefix)
    }

    async fn apply_batch_inner(
        &self,
        machine_id: Uuid,
        credential: &[u8],
        kind: JournalKind,
        batch: &JournalRecords,
        prefix: &mut Option<u64>,
    ) -> Result<IngestOutcome, IngestError> {
        require_active(&mut *self.pool.acquire().await?, machine_id, credential).await?;
        check_record_batch(batch).map_err(|_| IngestError::BatchLimit)?;
        if (kind == JournalKind::Sj && batch.records.len() > MAX_SJ_BATCH_RECORDS)
            || batch.records.iter().any(|record| {
                match record.body.as_ref().and_then(|body| body.body.as_ref()) {
                    Some(Body::BlockDelta(fact)) => fact.append.len() > MAX_BLOCK_DELTA_BYTES,
                    Some(Body::BlockFinal(fact)) => fact.text.len() > BLOCK_FINAL_INLINE_MAX_BYTES,
                    _ => false,
                }
            })
        {
            return Err(IngestError::BatchLimit);
        }
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
        if kind == JournalKind::Sj {
            let outcome = self
                .apply_sj_batch(
                    machine_id,
                    credential,
                    batch,
                    (up_to_seq, last_digest),
                    prefix,
                )
                .await?;
            self.trigger_dispatcher(machine_id).await?;
            return Ok(outcome);
        }
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
            require_active(&mut tx, machine_id, credential).await?;
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
            let correlation_id = Uuid::now_v7();
            let ignored = match apply_journal_body(
                &mut tx,
                machine_id,
                kind,
                journal_id,
                record.seq,
                body,
                correlation_id,
            )
            .await?
            {
                Some(ignored) => ignored,
                None => apply_body(&mut tx, machine_id, body, correlation_id).await?,
            };
            sqlx::query("update journal_cursors set committed_seq = $3, last_record_digest = $4, updated_at = clock_timestamp() where machine_id = $1 and journal_id = $2")
                .bind(machine_id).bind(journal_id).bind(seq).bind(&digest).execute(&mut *tx).await?;
            tx.commit().await?;
            up_to_seq = record.seq;
            last_digest = digest;
            if ignored {
                self.ignored.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.trigger_dispatcher(machine_id).await?;
        Ok(IngestOutcome::Committed { up_to_seq })
    }

    async fn apply_sj_batch(
        &self,
        machine_id: Uuid,
        credential: &[u8],
        batch: &JournalRecords,
        snapshot: (u64, Option<Vec<u8>>),
        prefix: &mut Option<u64>,
    ) -> Result<IngestOutcome, IngestError> {
        let journal_id = parse_id(&batch.journal_id)?;
        let (mut up_to_seq, mut last_digest) = snapshot;
        let pending: Vec<_> = batch
            .records
            .iter()
            .filter_map(|record| {
                if record.seq < up_to_seq {
                    return None;
                }
                let digest = record
                    .body
                    .as_ref()
                    .map(|body| Sha256::digest(body.encode_to_vec()).to_vec());
                if record.seq == up_to_seq && (last_digest.is_none() || last_digest == digest) {
                    return None;
                }
                Some((record, digest))
            })
            .collect();
        if pending.is_empty() {
            return Ok(IngestOutcome::Committed { up_to_seq });
        }
        let mut tx = self.pool.begin().await?;
        // Порядок SJ: credentials SHARE → journal_cursors → queue_state → scope_counters/events.
        // Отзыв/ротация: machines NO KEY UPDATE → credentials; FK KEY SHARE курсора с этим совместим.
        require_active(&mut tx, machine_id, credential).await?;
        let mut locked = false;
        let mut initial_seq = up_to_seq;
        let mut ignored_count = 0;
        let mut failure = None;
        let mut gap = None;
        for (record, digest) in pending {
            sqlx::query("savepoint sj_record").execute(&mut *tx).await?;
            if !locked {
                sqlx::query("insert into journal_cursors (machine_id, journal_id, kind, committed_seq) values ($1, $2, 'sj', 0) on conflict do nothing")
                    .bind(machine_id).bind(journal_id).execute(&mut *tx).await?;
                let rows = sqlx::query_as("select machine_id, kind, committed_seq, last_record_digest from journal_cursors where journal_id = $1 for update")
                    .bind(journal_id).fetch_all(&mut *tx).await?;
                (up_to_seq, last_digest) = checked_cursor(machine_id, JournalKind::Sj, rows)?;
                let opened = batch.records.iter().find_map(|record| {
                    match record.body.as_ref().and_then(|body| body.body.as_ref()) {
                        Some(Body::JournalOpened(fact)) if record.seq == 1 => {
                            Uuid::parse_str(&fact.execution_id).ok()
                        }
                        _ => None,
                    }
                });
                sqlx::query("select q.conversation_id from queue_state q join executions e on e.conversation_id = q.conversation_id join conversations c on c.id = q.conversation_id where e.id = coalesce((select execution_id from journal_cursors where journal_id = $1), $2) and c.machine_id = $3 for update of q")
                    .bind(journal_id).bind(opened).bind(machine_id).fetch_optional(&mut *tx).await?;
                initial_seq = up_to_seq;
                locked = true;
            }
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
                sqlx::query("release savepoint sj_record")
                    .execute(&mut *tx)
                    .await?;
                continue;
            }
            if record.seq > up_to_seq + 1 {
                sqlx::query("rollback to savepoint sj_record")
                    .execute(&mut *tx)
                    .await?;
                gap = Some(record.seq);
                break;
            }
            let result = async {
                i64::try_from(record.seq).map_err(|_| IngestError::InvalidRecord)?;
                let body = record
                    .body
                    .as_ref()
                    .and_then(|body| body.body.as_ref())
                    .ok_or(IngestError::UnknownRecord)?;
                let correlation_id = Uuid::now_v7();
                match apply_journal_body(
                    &mut tx,
                    machine_id,
                    JournalKind::Sj,
                    journal_id,
                    record.seq,
                    body,
                    correlation_id,
                )
                .await?
                {
                    Some(ignored) => Ok(ignored),
                    None => apply_body(&mut tx, machine_id, body, correlation_id).await,
                }
            }
            .await;
            match result {
                Ok(ignored) => {
                    sqlx::query("release savepoint sj_record")
                        .execute(&mut *tx)
                        .await?;
                    up_to_seq = record.seq;
                    last_digest = digest;
                    ignored_count += u64::from(ignored);
                }
                Err(error) => {
                    sqlx::query("rollback to savepoint sj_record")
                        .execute(&mut *tx)
                        .await?;
                    failure = Some(error);
                    break;
                }
            }
        }
        if up_to_seq > initial_seq {
            sqlx::query("update journal_cursors set committed_seq = $2, last_record_digest = $3, updated_at = clock_timestamp() where journal_id = $1")
                .bind(journal_id).bind(up_to_seq as i64).bind(last_digest).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.ignored.fetch_add(ignored_count, Ordering::Relaxed);
        if let Some(error) = failure {
            if up_to_seq > initial_seq {
                *prefix = Some(up_to_seq);
            }
            return Err(error);
        }
        if let Some(got_seq) = gap {
            return Ok(IngestOutcome::Gap {
                committed_seq: up_to_seq,
                got_seq,
            });
        }
        Ok(IngestOutcome::Committed { up_to_seq })
    }

    async fn trigger_dispatcher(&self, machine_id: Uuid) -> Result<(), IngestError> {
        if let Some(dispatcher) = &self.dispatcher {
            for conversation in
                crate::dispatcher::queued_conversations(&self.pool, Some(machine_id)).await?
            {
                dispatcher.trigger(conversation);
            }
        }
        Ok(())
    }
}

async fn require_active(
    conn: &mut PgConnection,
    machine_id: Uuid,
    credential: &[u8],
) -> Result<(), IngestError> {
    let active = sqlx::query("select machine_id from machine_credentials where machine_id = $1 and token_sha256 = $2 and revoked_at is null for share")
        .bind(machine_id).bind(credential).fetch_optional(conn).await?;
    if active.is_none() {
        return Err(IngestError::CredentialRevoked);
    }
    Ok(())
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
                    timed_receipt(
                        conn,
                        machine_id,
                        &fact.command_id,
                        fact.dispatch_attempt,
                        InputReceipt::Stored,
                        fact.machine_time_ms,
                        correlation_id,
                    )
                    .await?;
                } else {
                    let machine_time = i64::try_from(fact.machine_time_ms)
                        .map_err(|_| IngestError::InvalidRecord)?;
                    sqlx::query("update commands set delivery_state = 'stored_on_machine', stored_at = coalesce(stored_at, now()), machine_stored_at_ms = coalesce(machine_stored_at_ms, $3) where id = $1 and dispatch_attempt = $2 and delivery_state = 'pending'")
                        .bind(command_id).bind(dispatch_attempt).bind(machine_time).execute(&mut *conn).await?;
                    store_machine_time_in_tx(
                        conn,
                        command_id,
                        dispatch_attempt,
                        &InputReceipt::Stored,
                        machine_time,
                    )
                    .await?;
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
            timed_receipt(
                conn,
                machine_id,
                &fact.command_id,
                fact.dispatch_attempt,
                InputReceipt::TransmissionStarted,
                fact.machine_time_ms,
                correlation_id,
            )
            .await?
        }
        Body::InputAccepted(fact) => {
            timed_receipt(
                conn,
                machine_id,
                &fact.command_id,
                fact.dispatch_attempt,
                InputReceipt::Accepted,
                fact.machine_time_ms,
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
                let (state, stored_outcome): (String, Option<String>) =
                    sqlx::query_as("select state, outcome from executions where id = $1")
                        .bind(execution_id)
                        .fetch_one(&mut *conn)
                        .await?;
                if state == to.as_str() {
                    if to == ExecutionState::Running {
                        sqlx::query("update executions set native_conversation_id = coalesce(nullif($2, ''), native_conversation_id) where id = $1")
                            .bind(execution_id).bind(&fact.native_conversation_id).execute(&mut *conn).await?;
                        sqlx::query("update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, now()), result = '{\"started\":true}' where execution_id = $1 and machine_id = $2 and kind = 'start_execution' and delivery_state <> 'completed'")
                            .bind(execution_id).bind(machine_id).execute(&mut *conn).await?;
                    } else if matches!(
                        to,
                        ExecutionState::Stopped
                            | ExecutionState::Failed
                            | ExecutionState::OwnerLost
                    ) {
                        complete_unsuccessful_start(
                            conn,
                            execution_id,
                            machine_id,
                            stored_outcome.as_deref().unwrap_or("owner_lost"),
                        )
                        .await?;
                    }
                    if matches!(to, ExecutionState::Stopped | ExecutionState::Failed) {
                        sqlx::query("update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, now()), result = case when kind = 'stop_execution' then jsonb_build_object('outcome', $3::text) else jsonb_build_object('status', 'superseded') end, last_status = case when kind = 'interrupt' then 'superseded' else last_status end where execution_id = $1 and machine_id = $2 and kind in ('stop_execution', 'interrupt') and delivery_state <> 'completed'")
                            .bind(execution_id).bind(machine_id).bind(stored_outcome).execute(&mut *conn).await?;
                    }
                }
            }
        }
        Body::RunFinished(fact) => {
            let execution_id = parse_id(&fact.execution_id)?;
            let command_id = parse_id(&fact.input_command_id)?;
            if command(conn, machine_id, command_id).await?.is_some() {
                apply_run_finished_in_tx(conn, execution_id, command_id, correlation_id).await?;
            }
        }
        Body::InterruptStatus(fact) => {
            use canterlot_proto::worker::v1::InterruptState;
            let command_id = parse_id(&fact.command_id)?;
            let state =
                InterruptState::try_from(fact.state).map_err(|_| IngestError::InvalidRecord)?;
            let status = match state {
                InterruptState::SentToAgent => "sent_to_agent",
                InterruptState::Acknowledged => "acknowledged",
                InterruptState::Completed => "completed",
                InterruptState::NotApplicable => "not_applicable",
                InterruptState::Superseded => "superseded",
                InterruptState::RejectedStale => "rejected_stale",
                InterruptState::Unspecified => return Err(IngestError::InvalidRecord),
            };
            if let Some(target) = command(conn, machine_id, command_id).await?
                && target.kind == "interrupt"
                && target.dispatch_attempt == attempt(fact.dispatch_attempt)?
            {
                let terminal = !matches!(
                    state,
                    InterruptState::SentToAgent | InterruptState::Acknowledged
                );
                sqlx::query("update commands set last_status = $2, delivery_state = case when $3 then 'completed' else delivery_state end, completed_at = case when $3 then coalesce(completed_at, now()) else completed_at end, result = case when $3 then jsonb_build_object('status', $2::text) else result end where id = $1")
                    .bind(command_id).bind(status).bind(terminal).execute(&mut *conn).await?;
                if let Some(execution_id) = target.execution_id
                    && let Some(run) = owned_execution(conn, machine_id, execution_id, None).await?
                {
                    execution_event(
                        conn,
                        &run,
                        "interrupt_state_changed",
                        json!({"command_id": command_id, "status": status}),
                        correlation_id,
                    )
                    .await?;
                }
            }
        }
        Body::WithdrawRefused(fact) => {
            let command_id = parse_id(&fact.command_id)?;
            if let Some(target) = command(conn, machine_id, command_id).await?
                && target.kind == "withdraw_input"
                && target.dispatch_attempt == attempt(fact.dispatch_attempt)?
            {
                sqlx::query("update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, now()), result = $2 where id = $1 and delivery_state <> 'completed'")
                    .bind(command_id).bind(json!({"refused": sanitize_reason(&fact.reason)})).execute(&mut *conn).await?;
            }
        }
        Body::AdapterDiagnostic(fact) => {
            let execution_id = parse_id(&fact.execution_id)?;
            if let Some(run) = owned_execution(conn, machine_id, execution_id, None).await? {
                execution_event(
                    conn,
                    &run,
                    "adapter_diagnostic",
                    json!({"kind": fact.kind, "event_type": fact.event_type}),
                    correlation_id,
                )
                .await?;
            }
        }
        Body::JournalClosed(_)
        | Body::BlockStarted(_)
        | Body::BlockDelta(_)
        | Body::BlockFinal(_)
        | Body::BlockIncomplete(_)
        | Body::PhaseChanged(_)
        | Body::JournalOpened(_) => return Err(IngestError::InvalidRecord),
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
        credential: &[u8],
        kind: JournalKind,
        batch: JournalRecords,
    ) -> Result<canterlot_proto::worker::v1::JournalRangeAck, tonic::Status> {
        let (result, prefix) = self
            .apply_batch_report(machine_id, credential, kind, &batch)
            .await;
        if let Some(up_to_seq) = prefix {
            return Ok(canterlot_proto::worker::v1::JournalRangeAck {
                journal_id: batch.journal_id,
                up_to_seq,
            });
        }
        let outcome = result.map_err(|error| {
            let message = error.to_string();
            match error {
                IngestError::CredentialRevoked => tonic::Status::unauthenticated(message),
                IngestError::BatchLimit => tonic::Status::resource_exhausted(message),
                IngestError::ForeignJournal | IngestError::ForeignExecution => {
                    tonic::Status::permission_denied(message)
                }
                IngestError::Database | IngestError::Execution => tonic::Status::internal(message),
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
    async fn reconcile(
        &self,
        machine_id: Uuid,
        report: ReconcileReport,
    ) -> Result<(), tonic::Status> {
        Ingest::reconcile(self, machine_id, &report)
            .await
            .map_err(|error| match error {
                IngestError::InvalidRecord => tonic::Status::invalid_argument(error.to_string()),
                _ => tonic::Status::internal(error.to_string()),
            })
    }

    async fn cj_records(
        &self,
        machine_id: Uuid,
        credential: &[u8],
        records: JournalRecords,
    ) -> Result<canterlot_proto::worker::v1::JournalRangeAck, tonic::Status> {
        #[cfg(feature = "fault-injection")]
        {
            self.faults.batch(records.clone());
            self.faults
                .pause(crate::fault_injection::Point::BeforeCjCommit)
                .await;
        }
        self.range_ack(machine_id, credential, JournalKind::Cj, records)
            .await
    }

    async fn sj_records(
        &self,
        machine_id: Uuid,
        credential: &[u8],
        records: JournalRecords,
    ) -> Result<canterlot_proto::worker::v1::JournalRangeAck, tonic::Status> {
        #[cfg(feature = "fault-injection")]
        {
            self.faults.batch(records.clone());
            self.faults.pause_sj_batch().await;
        }
        self.range_ack(machine_id, credential, JournalKind::Sj, records)
            .await
    }
}

async fn owned_execution(
    conn: &mut PgConnection,
    machine_id: Uuid,
    execution_id: Uuid,
    generation: Option<u64>,
) -> Result<Option<ExecutionRef>, IngestError> {
    let generation = generation
        .map(i64::try_from)
        .transpose()
        .map_err(|_| IngestError::InvalidRecord)?;
    let conversation: Option<Uuid> = sqlx::query_scalar("select e.conversation_id from executions e join conversations c on c.id = e.conversation_id where e.id = $1 and c.machine_id = $2 and ($3::bigint is null or e.generation = $3)")
        .bind(execution_id).bind(machine_id).bind(generation).fetch_optional(conn).await?;
    Ok(conversation.map(|conversation_id| ExecutionRef {
        execution_id,
        conversation_id,
        machine_id,
    }))
}

async fn execution_event(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    kind: &'static str,
    payload: serde_json::Value,
    correlation_id: Uuid,
) -> Result<(), IngestError> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: run.conversation_id,
            },
            kind,
            conversation_id: Some(run.conversation_id),
            execution_id: Some(run.execution_id),
            payload,
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

async fn apply_journal_body(
    conn: &mut PgConnection,
    machine_id: Uuid,
    kind: JournalKind,
    journal_id: Uuid,
    seq: u64,
    body: &Body,
    correlation_id: Uuid,
) -> Result<Option<bool>, IngestError> {
    if let Body::JournalOpened(fact) = body {
        if kind != JournalKind::Sj || seq != 1 {
            return Err(IngestError::InvalidRecord);
        }
        let execution_id = parse_id(&fact.execution_id)?;
        owned_execution(conn, machine_id, execution_id, Some(fact.generation))
            .await?
            .ok_or(IngestError::ForeignExecution)?;
        sqlx::query("update journal_cursors set execution_id = $2 where journal_id = $1")
            .bind(journal_id)
            .bind(execution_id)
            .execute(conn)
            .await?;
        return Ok(Some(false));
    }
    if !matches!(
        body,
        Body::BlockStarted(_)
            | Body::BlockDelta(_)
            | Body::BlockFinal(_)
            | Body::BlockIncomplete(_)
            | Body::PhaseChanged(_)
            | Body::JournalClosed(_)
    ) {
        return Ok(None);
    }
    if kind != JournalKind::Sj {
        return Err(IngestError::InvalidRecord);
    }
    let execution_id: Option<Uuid> =
        sqlx::query_scalar("select execution_id from journal_cursors where journal_id = $1")
            .bind(journal_id)
            .fetch_one(&mut *conn)
            .await?;
    let execution_id = execution_id.ok_or(IngestError::InvalidRecord)?;
    let run = owned_execution(conn, machine_id, execution_id, None)
        .await?
        .ok_or(IngestError::ForeignExecution)?;
    let outcome = match body {
        Body::BlockStarted(fact) => blocks::start_block(conn, &run, fact, correlation_id).await,
        Body::BlockDelta(fact) => blocks::apply_delta(conn, &run, fact, correlation_id).await,
        Body::BlockFinal(fact) => blocks::finalize(conn, &run, fact, correlation_id).await,
        Body::BlockIncomplete(fact) => blocks::mark_incomplete(conn, &run, fact, correlation_id).await,
        Body::PhaseChanged(fact) => {
            if parse_id(&fact.execution_id)? != run.execution_id { return Err(IngestError::ForeignExecution); }
            sqlx::query("select conversation_id from queue_state where conversation_id = $1 for update")
                .bind(run.conversation_id).execute(&mut *conn).await?;
            let changed = sqlx::query("update executions set phase = $2, phase_at = clock_timestamp() where id = $1 and state not in ('stopped', 'failed')")
                .bind(run.execution_id).bind(&fact.phase).execute(&mut *conn).await?.rows_affected();
            if changed == 0 { return Ok(Some(true)); }
            execution_event(conn, &run, "phase_changed", json!({"phase": fact.phase}), correlation_id).await?;
            return Ok(Some(false));
        }
        Body::JournalClosed(fact) => {
            if parse_id(&fact.execution_id)? != run.execution_id || owned_execution(conn, machine_id, run.execution_id, Some(fact.generation)).await?.is_none() { return Err(IngestError::ForeignExecution); }
            blocks::close_journal(conn, &run, fact, correlation_id).await?;
            sqlx::query("update journal_cursors set closed = true where journal_id = $1").bind(journal_id).execute(&mut *conn).await?;
            execution_event(conn, &run, "journal_closed", json!({"reason": fact.reason, "generation": fact.generation, "journal_id": journal_id}), correlation_id).await?;
            return Ok(Some(false));
        }
        _ => return Err(IngestError::InvalidRecord),
    }.map_err(|error| match error { sqlx::Error::InvalidArgument(_) => IngestError::InvalidRecord, _ => IngestError::Database })?;
    Ok(Some(matches!(outcome, BlockOutcome::Ignored(_))))
}

async fn timed_receipt(
    conn: &mut PgConnection,
    machine_id: Uuid,
    command_id: &str,
    dispatch_attempt: u32,
    receipt: InputReceipt,
    machine_time_ms: u64,
    correlation_id: Uuid,
) -> Result<(), IngestError> {
    let command_id = parse_id(command_id)?;
    let dispatch_attempt = attempt(dispatch_attempt)?;
    if command(conn, machine_id, command_id).await?.is_some() {
        let machine_time_ms =
            i64::try_from(machine_time_ms).map_err(|_| IngestError::InvalidRecord)?;
        apply_input_receipt_in_tx(
            conn,
            command_id,
            dispatch_attempt,
            receipt.clone(),
            correlation_id,
        )
        .await?;
        store_machine_time_in_tx(
            conn,
            command_id,
            dispatch_attempt,
            &receipt,
            machine_time_ms,
        )
        .await?;
    }
    Ok(())
}

async fn complete_unsuccessful_start(
    conn: &mut PgConnection,
    execution_id: Uuid,
    machine_id: Uuid,
    outcome: &str,
) -> Result<(), IngestError> {
    sqlx::query("update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, now()), result = jsonb_build_object('started', false, 'outcome', $3::text) where execution_id = $1 and machine_id = $2 and kind = 'start_execution' and delivery_state <> 'completed'")
        .bind(execution_id).bind(machine_id).bind(outcome).execute(conn).await?;
    Ok(())
}
