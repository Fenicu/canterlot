use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::{borrow::Cow, collections::BTreeSet};

use canterlot_proto::{
    limits::{
        BLOCK_FINAL_INLINE_MAX_BYTES, MAX_BLOCK_DELTA_BYTES, MAX_SJ_BATCH_RECORDS,
        check_record_batch,
    },
    worker::v1::{
        DecisionApplied, DecisionNotApplied, DecisionTransmissionStarted, DecisionUnconfirmed,
        JournalRecords, ReconcileReport, RequestClosed, RequestOpened, record_body::Body,
    },
};
use prost::Message;
use serde_json::{Value, json};
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
    model::{ExecutionState, HoldReason, PauseReason, RunSlot},
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
    registry: Option<crate::session::Registry>,
    limits: crate::config::LimitsConfig,
    ignored: Arc<AtomicU64>,
    #[cfg(feature = "fault-injection")]
    pub faults: crate::fault_injection::Faults,
}

// Итог SJ-транзакции: смены лимитов, прогресс, счётчик, отложенные ошибка и разрыв.
type SjBatchOutcome = (
    BTreeSet<(String, crate::limits::Window)>,
    u64,
    u64,
    Option<IngestError>,
    Option<u64>,
);

impl Ingest {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            dispatcher: None,
            registry: None,
            limits: Default::default(),
            ignored: Arc::new(AtomicU64::new(0)),
            #[cfg(feature = "fault-injection")]
            faults: Default::default(),
        }
    }

    pub fn with_limits(mut self, config: crate::config::LimitsConfig) -> Self {
        self.limits = config;
        self
    }
    pub fn with_registry(mut self, registry: crate::session::Registry) -> Self {
        self.registry = Some(registry);
        self
    }
    fn hub_time(
        &self,
        machine: Uuid,
        ms: u64,
        now: chrono::DateTime<chrono::Utc>,
    ) -> chrono::DateTime<chrono::Utc> {
        Self::hub_time_for(&self.registry, machine, ms, now)
    }

    fn hub_time_for(
        registry: &Option<crate::session::Registry>,
        machine: Uuid,
        ms: u64,
        now: chrono::DateTime<chrono::Utc>,
    ) -> chrono::DateTime<chrono::Utc> {
        if ms == 0 {
            return now;
        }
        registry
            .as_ref()
            .and_then(|r| r.to_hub_time(machine, ms))
            .or_else(|| {
                i64::try_from(ms)
                    .ok()
                    .and_then(chrono::DateTime::from_timestamp_millis)
            })
            .unwrap_or(now)
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
                    observation.clone(),
                ))
            })
            .collect::<Result<Vec<_>, IngestError>>()?;
        let pool = self.pool.clone();
        crate::db::in_tx::<(), IngestError, _>(&pool, move |conn| {
            Box::pin(async move {
        let owner_id: Uuid = sqlx::query_scalar("select owner_id from machines where id = $1")
            .bind(machine_id)
            .fetch_one(&mut *conn)
            .await?;
        sqlx::query(
            "select q.conversation_id from queue_state q join conversations c on c.id = q.conversation_id
             where c.machine_id = $1 order by q.conversation_id for update of q",
        ).bind(machine_id).fetch_all(&mut *conn).await?;
        let executions: Vec<(Uuid, Uuid, i64, String)> = sqlx::query_as(
            "select e.id, e.conversation_id, e.generation, e.state from executions e
             join conversations c on c.id = e.conversation_id where c.machine_id = $1",
        )
        .bind(machine_id)
        .fetch_all(&mut *conn)
        .await?;
        for (id, generation, worker_state, observation) in &observations {
            let known = executions
                .iter()
                .find(|(execution_id, _, _, _)| execution_id == id);
            let Some((_, _, hub_generation, hub_state)) = known else {
                events::append(&mut *conn, NewEvent {
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
                        .bind(id).bind(journal_id).fetch_one(&mut *conn).await?;
                    if !closed {
                        continue;
                    }
                }
                apply_body(
                    &mut *conn,
                    machine_id,
                    &Body::ExecutionState(canterlot_proto::worker::v1::ExecutionState {
                        execution_id: id.to_string(),
                        generation: observation.generation,
                        state: observation.state.clone(),
                        outcome: bounded_text(&observation.outcome, 1024),
                        ..Default::default()
                    }),
                    Uuid::now_v7(),
                )
                .await?;
            } else {
                sqlx::query("update executions set observed_at = now() where id = $1")
                    .bind(id)
                    .execute(&mut *conn)
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
                    &mut *conn,
                    execution_id,
                    ExecutionState::OwnerLost,
                    None,
                    correlation_id,
                )
                .await?;
                complete_unsuccessful_start(&mut *conn, execution_id, machine_id, "owner_lost")
                    .await?;
                events::append(
                    &mut *conn,
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
                Ok(())
            })
        })
        .await?;
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
            if let Err(error) = require_active(&mut tx, machine_id, credential).await {
                let _ = tx.rollback().await;
                return Err(error);
            }
            if let Err(error) = sqlx::query("insert into journal_cursors (machine_id, journal_id, kind, committed_seq) values ($1, $2, $3, 0) on conflict do nothing")
                .bind(machine_id).bind(journal_id).bind(kind.as_str()).execute(&mut *tx).await
            {
                let _ = tx.rollback().await;
                return Err(error.into());
            }
            let rows = match sqlx::query_as(
                "select machine_id, kind, committed_seq, last_record_digest from journal_cursors where journal_id = $1 for update",
            ).bind(journal_id).fetch_all(&mut *tx).await {
                Ok(rows) => rows,
                Err(error) => {
                    let _ = tx.rollback().await;
                    return Err(error.into());
                }
            };
            let rows: Vec<CursorRow> = rows;
            match checked_cursor(machine_id, kind, rows) {
                Ok((committed, cursor_digest)) => {
                    up_to_seq = committed;
                    last_digest = cursor_digest;
                }
                Err(error) => {
                    let _ = tx.rollback().await;
                    return Err(error);
                }
            }
            if record.seq <= up_to_seq {
                if record.seq == up_to_seq && last_digest.is_some() && last_digest != digest {
                    let owner_id =
                        match sqlx::query_scalar("select owner_id from machines where id = $1")
                            .bind(machine_id)
                            .fetch_one(&mut *tx)
                            .await
                        {
                            Ok(owner_id) => owner_id,
                            Err(error) => {
                                let _ = tx.rollback().await;
                                return Err(error.into());
                            }
                        };
                    let owner_id: Uuid = owner_id;
                    if let Err(error) = events::append(
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
                    .await
                    {
                        let _ = tx.rollback().await;
                        return Err(error.into());
                    }
                }
                tx.commit().await?;
                continue;
            }
            if record.seq > up_to_seq + 1 {
                let _ = tx.rollback().await;
                return Ok(IngestOutcome::Gap {
                    committed_seq: up_to_seq,
                    got_seq: record.seq,
                });
            }
            let seq = match i64::try_from(record.seq).map_err(|_| IngestError::InvalidRecord) {
                Ok(seq) => seq,
                Err(error) => {
                    let _ = tx.rollback().await;
                    return Err(error);
                }
            };
            let body = match record
                .body
                .as_ref()
                .and_then(|body| body.body.as_ref())
                .ok_or(IngestError::UnknownRecord)
            {
                Ok(body) => body,
                Err(error) => {
                    let _ = tx.rollback().await;
                    return Err(error);
                }
            };
            let body = bounded_metadata(body);
            let body = body.as_ref();
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
            .await
            {
                Ok(Some(ignored)) => ignored,
                Ok(None) => match apply_body(&mut tx, machine_id, body, correlation_id).await {
                    Ok(ignored) => ignored,
                    Err(error) => {
                        let _ = tx.rollback().await;
                        return Err(error);
                    }
                },
                Err(error) => {
                    let _ = tx.rollback().await;
                    return Err(error);
                }
            };
            if let Err(error) = sqlx::query("update journal_cursors set committed_seq = $3, last_record_digest = $4, updated_at = clock_timestamp() where machine_id = $1 and journal_id = $2")
                        .bind(machine_id).bind(journal_id).bind(seq).bind(&digest).execute(&mut *tx).await
                    {
                        let _ = tx.rollback().await;
                        return Err(error.into());
                    }
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
        if batch.records.is_empty() {
            return Ok(IngestOutcome::Committed { up_to_seq });
        }
        let limit_records = batch.records.iter().any(|record| {
            matches!(
                record.body.as_ref().and_then(|b| b.body.as_ref()),
                Some(Body::LimitObserved(_) | Body::LimitReached(_))
            )
        });
        let entry_seq = up_to_seq;
        // Всё заимствованное — в owned: замыкание in_tx требует 'static.
        let pool = self.pool.clone();
        let registry = self.registry.clone();
        let limits_cfg = self.limits.clone();
        let credential = credential.to_owned();
        let pending: Vec<(canterlot_proto::worker::v1::JournalRecord, Option<Vec<u8>>)> = batch
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
                Some((record.clone(), digest))
            })
            .collect();
        if pending.is_empty() {
            return Ok(IngestOutcome::Committed { up_to_seq });
        }
        let (changed_limits, up_to_seq, ignored_count, failure, gap): SjBatchOutcome =
            crate::db::in_tx::<SjBatchOutcome, IngestError, _>(&pool, move |conn| {
            Box::pin(async move {
        let mut locked = false;
        let mut initial_seq = up_to_seq;
        let mut ignored_count = 0;
        let mut failure = None;
        let mut gap = None;
        let mut changed_limits = BTreeSet::new();
        let mut touched = BTreeSet::new();
        let mut diagnostics: Vec<(&'static str, serde_json::Value)> = Vec::new();
        // Порядок SJ: credentials → cursor → providers → queue → события.
        // Отзыв/ротация: machines NO KEY UPDATE → credentials; FK KEY SHARE курсора с этим совместим.
        require_active(&mut *conn, machine_id, &credential).await?;
        let owner_id: Uuid = sqlx::query_scalar("select owner_id from machines where id=$1")
            .bind(machine_id)
            .fetch_one(&mut *conn)
            .await?;
        for (record, digest) in &pending {
            sqlx::query("savepoint sj_record").execute(&mut *conn).await?;
            if !locked {
                sqlx::query("insert into journal_cursors (machine_id, journal_id, kind, committed_seq) values ($1, $2, 'sj', 0) on conflict do nothing")
                    .bind(machine_id).bind(journal_id).execute(&mut *conn).await?;
                let rows = sqlx::query_as("select machine_id, kind, committed_seq, last_record_digest from journal_cursors where journal_id = $1 for update")
                    .bind(journal_id).fetch_all(&mut *conn).await?;
                (up_to_seq, last_digest) = checked_cursor(machine_id, JournalKind::Sj, rows)?;
                let providers: BTreeSet<_> = pending
                    .iter()
                    .filter_map(
                        |(record, _)| match record.body.as_ref().and_then(|b| b.body.as_ref()) {
                            Some(Body::LimitObserved(f)) => Some(f.provider.as_str()),
                            Some(Body::LimitReached(f)) => Some(f.provider.as_str()),
                            _ => None,
                        },
                    )
                    .filter(|provider| crate::limits::known_provider(provider))
                    .collect();
                for provider in providers {
                    crate::limits::lock_provider(&mut *conn, provider).await?;
                }
                let opened = pending.iter().find_map(|(record, _)| {
                    match record.body.as_ref().and_then(|body| body.body.as_ref()) {
                        Some(Body::JournalOpened(fact)) if record.seq == 1 => {
                            Uuid::parse_str(&fact.execution_id).ok()
                        }
                        _ => None,
                    }
                });
                sqlx::query("select q.conversation_id from queue_state q join executions e on e.conversation_id = q.conversation_id join conversations c on c.id = q.conversation_id where e.id = coalesce((select execution_id from journal_cursors where journal_id = $1), $2) and c.machine_id = $3 for update of q")
                    .bind(journal_id).bind(opened).bind(machine_id).fetch_optional(&mut *conn).await?;
                initial_seq = up_to_seq;
                locked = true;
            }
            if record.seq <= up_to_seq {
                if record.seq == up_to_seq && last_digest.is_some() && last_digest != *digest {
                    diagnostics.push((
                        "journal_record_mismatch",
                        json!({"journal_id": journal_id, "seq": record.seq}),
                    ));
                }
                sqlx::query("release savepoint sj_record")
                    .execute(&mut *conn)
                    .await?;
                continue;
            }
            if record.seq > up_to_seq + 1 {
                sqlx::query("rollback to savepoint sj_record")
                    .execute(&mut *conn)
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
                let body = bounded_metadata(body);
                let body = body.as_ref();
                let correlation_id = Uuid::now_v7();
                match apply_journal_body(
                    &mut *conn,
                    machine_id,
                    JournalKind::Sj,
                    journal_id,
                    record.seq,
                    body,
                    correlation_id,
                )
                .await?
                {
                    Some(ignored) => Ok((ignored, None, false)),
                    None => {
                        Self::apply_pilot_body(&mut *conn, &registry, &limits_cfg, machine_id, journal_id, body, correlation_id)
                            .await
                    }
                }
            }
            .await;
            match result {
                Ok((ignored, change, unmapped)) => {
                    if let Some(change) = change {
                        changed_limits.insert(change);
                    }
                    if unmapped {
                        diagnostics.push((
                            "limit_observation_unmapped",
                            json!({"journal_id":journal_id,"seq":record.seq,"reason":"clock_offset_unknown"}),
                        ));
                    }
                    if (!ignored || unmapped)
                        && let Some(body) = record.body.as_ref().and_then(|b| b.body.as_ref())
                        && touches_activity(body)
                    {
                        let id: Option<Uuid> = sqlx::query_scalar(
                            "select execution_id from journal_cursors where journal_id=$1",
                        )
                        .bind(journal_id)
                        .fetch_one(&mut *conn)
                        .await?;
                        if let Some(id) = id {
                            touched.insert(id);
                        }
                    }
                    sqlx::query("release savepoint sj_record")
                        .execute(&mut *conn)
                        .await?;
                    up_to_seq = record.seq;
                    last_digest = digest.clone();
                    ignored_count += u64::from(ignored);
                }
                Err(error) => {
                    sqlx::query("rollback to savepoint sj_record")
                        .execute(&mut *conn)
                        .await?;
                    failure = Some(error);
                    break;
                }
            }
        }
        if up_to_seq > initial_seq {
            sqlx::query("update journal_cursors set committed_seq = $2, last_record_digest = $3, updated_at = clock_timestamp() where journal_id = $1")
                .bind(journal_id).bind(up_to_seq as i64).bind(last_digest).execute(&mut *conn).await?;
        }
        for execution in touched {
            sqlx::query("update executions set last_event_at=clock_timestamp() where id=$1")
                .bind(execution)
                .execute(&mut *conn)
                .await?;
        }
        for (provider, window) in &changed_limits {
            crate::limits::emit_changed(&mut *conn, owner_id, provider, *window, Uuid::now_v7())
                .await?;
        }
        for (kind, diagnostic) in diagnostics {
            events::append(
                &mut *conn,
                NewEvent {
                    scope: Scope::Conversations { owner_id },
                    kind,
                    conversation_id: None,
                    execution_id: None,
                    payload: diagnostic,
                    correlation_id: Uuid::now_v7(),
                },
            )
            .await?;
        }
            Ok((changed_limits, up_to_seq, ignored_count, failure, gap))
            })
        })
        .await?;
        if limit_records {
            // Факт лимита мог поставить пробуждение или правило резерва.
            crate::scheduler::notify_pool(&self.pool);
        }
        if let Some(registry) = &self.registry {
            let providers: BTreeSet<_> = changed_limits.iter().map(|(p, _)| p).collect();
            for provider in providers {
                if crate::limits::push_limits_snapshot(&self.pool, registry, provider)
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        kind = "limits_snapshot_push_failed",
                        "limits snapshot push failed"
                    );
                }
            }
        }
        self.ignored.fetch_add(ignored_count, Ordering::Relaxed);
        if let Some(error) = failure {
            if up_to_seq > entry_seq {
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

fn touches_activity(body: &Body) -> bool {
    matches!(
        body,
        Body::PhaseChanged(_)
            | Body::BlockStarted(_)
            | Body::BlockDelta(_)
            | Body::BlockFinal(_)
            | Body::BlockIncomplete(_)
            | Body::RunFinished(_)
            | Body::InputAccepted(_)
            | Body::LimitObserved(_)
            | Body::LimitReached(_)
            | Body::RequestOpened(_)
            | Body::RequestClosed(_)
            | Body::DecisionApplied(_)
            | Body::DecisionNotApplied(_)
    )
}

impl Ingest {
    async fn apply_pilot_body(
        conn: &mut PgConnection,
        registry: &Option<crate::session::Registry>,
        limits: &crate::config::LimitsConfig,
        machine: Uuid,
        journal: Uuid,
        body: &Body,
        correlation: Uuid,
    ) -> Result<(bool, Option<(String, crate::limits::Window)>, bool), IngestError> {
        use crate::limits::{self, Observation, ObservationSource, Window};
        let now = chrono::Utc::now();
        if matches!(body, Body::LimitObserved(_) | Body::LimitReached(_)) {
            let execution: Option<Uuid> =
                sqlx::query_scalar("select execution_id from journal_cursors where journal_id=$1")
                    .bind(journal)
                    .fetch_one(&mut *conn)
                    .await?;
            let run = owned_execution(
                conn,
                machine,
                execution.ok_or(IngestError::InvalidRecord)?,
                None,
            )
            .await?
            .ok_or(IngestError::ForeignExecution)?;
            let (owner, harness): (Uuid, String) =
                sqlx::query_as("select owner_id,harness from conversations where id=$1")
                    .bind(run.conversation_id)
                    .fetch_one(&mut *conn)
                    .await?;
            let provider = match body {
                Body::LimitObserved(f) => &f.provider,
                Body::LimitReached(f) => &f.provider,
                _ => unreachable!(),
            };
            if canterlot_proto::harness::provider_of(&harness) != Some(provider.as_str()) {
                return Err(IngestError::InvalidRecord);
            }
            match body {
                Body::LimitObserved(f) => {
                    let at = registry
                        .as_ref()
                        .and_then(|r| r.to_hub_time(machine, f.observed_machine_ms));
                    let observation = Observation {
                        provider: f.provider.clone(),
                        window: Window::from_proto(f.window)
                            .map_err(|_| IngestError::InvalidRecord)?,
                        used_permille: u16::try_from(f.used_permille)
                            .map_err(|_| IngestError::InvalidRecord)?,
                        resets_at: limits::timestamp(f.resets_at_ms)
                            .map_err(|_| IngestError::InvalidRecord)?,
                        observed_at: at.unwrap_or_else(|| {
                            Self::hub_time_for(registry, machine, f.observed_machine_ms, now)
                        }),
                        source: ObservationSource::Session,
                        machine_id: machine,
                        execution_id: Some(run.execution_id),
                    };
                    let result =
                        limits::record_in_tx(conn, &observation, now, limits, at.is_some())
                            .await
                            .map_err(pilot_error)?;
                    Ok((
                        at.is_none(),
                        result
                            .provider_changed
                            .then_some((observation.provider, observation.window)),
                        at.is_none(),
                    ))
                }
                Body::LimitReached(f) => {
                    if parse_id(&f.execution_id)? != run.execution_id {
                        return Err(IngestError::ForeignExecution);
                    }
                    if !f.input_command_id.is_empty() {
                        let target = command(conn, machine, parse_id(&f.input_command_id)?).await?;
                        if !target.is_some_and(|c| {
                            c.kind == "deliver_input" && c.execution_id == Some(run.execution_id)
                        }) {
                            return Err(IngestError::ForeignExecution);
                        }
                    }
                    limits::on_limit_reached_in_tx(conn, owner, &run, f, limits, now, correlation)
                        .await
                        .map_err(pilot_error)?;
                    Ok((false, None, false))
                }
                _ => unreachable!(),
            }
        } else {
            let ignored = apply_body(conn, machine, body, correlation).await?;
            match body {
                Body::InputAccepted(f) => {
                    let cmd = parse_id(&f.command_id)?;
                    let target:Option<(Uuid,Uuid)>=sqlx::query_as("select c.execution_id,c.conversation_id from commands c join inputs i on i.command_id=c.id where c.id=$1 and c.machine_id=$2 and c.kind='deliver_input' and c.dispatch_attempt=$3 and i.state='accepted'").bind(cmd).bind(machine).bind(attempt(f.dispatch_attempt)?).fetch_optional(&mut *conn).await?;
                    if let Some((execution_id, conversation_id)) = target {
                        crate::runs::start_in_tx(
                            conn,
                            cmd,
                            &ExecutionRef {
                                execution_id,
                                conversation_id,
                                machine_id: machine,
                            },
                            Self::hub_time_for(registry, machine, f.machine_time_ms, now),
                            now,
                        )
                        .await
                        .map_err(pilot_error)?;
                    }
                }
                Body::RunFinished(f) => {
                    if let Some(run) =
                        owned_execution(conn, machine, parse_id(&f.execution_id)?, None).await?
                    {
                        crate::runs::finish_in_tx(
                            conn,
                            f,
                            &run,
                            Self::hub_time_for(registry, machine, f.machine_time_ms, now),
                            now,
                        )
                        .await
                        .map_err(pilot_error)?;
                    }
                }
                _ => {}
            }
            Ok((ignored, None, false))
        }
    }
}
fn pilot_error(error: sqlx::Error) -> IngestError {
    match error {
        sqlx::Error::InvalidArgument(_) => IngestError::InvalidRecord,
        _ => IngestError::Database,
    }
}

fn bounded_text(value: &str, limit: usize) -> String {
    let mut value = value.replace('\0', "�");
    if let Some((end, ch)) = value.char_indices().nth(limit - 1)
        && end + ch.len_utf8() < value.len()
    {
        value.truncate(end);
        value.push('…');
    }
    value
}

fn bounded_metadata(body: &Body) -> Cow<'_, Body> {
    let mut bounded = match body {
        Body::BlockStarted(_)
        | Body::BlockFinal(_)
        | Body::BlockIncomplete(_)
        | Body::PhaseChanged(_)
        | Body::JournalClosed(_)
        | Body::AdapterDiagnostic(_)
        | Body::ExecutionState(_)
        | Body::RunFinished(_) => body.clone(),
        _ => return Cow::Borrowed(body),
    };
    match &mut bounded {
        Body::BlockStarted(f) => {
            f.agent = bounded_text(&f.agent, 128);
            f.model = bounded_text(&f.model, 128);
            f.tool_name = bounded_text(&f.tool_name, 128);
            f.summary = bounded_text(&f.summary, 1024);
        }
        Body::BlockFinal(f) => {
            f.summary = bounded_text(&f.summary, 1024);
        }
        Body::BlockIncomplete(f) => f.reason = bounded_text(&f.reason, 1024),
        Body::JournalClosed(f) => f.reason = bounded_text(&f.reason, 1024),
        Body::PhaseChanged(f) => {
            f.tool_name = bounded_text(&f.tool_name, 128);
            f.summary = bounded_text(&f.summary, 1024);
        }
        Body::AdapterDiagnostic(f) => {
            f.kind = bounded_text(&f.kind, 128);
            f.event_type = bounded_text(&f.event_type, 128);
        }
        Body::ExecutionState(f) => f.outcome = bounded_text(&f.outcome, 1024),
        Body::RunFinished(f) => {
            for usage in &mut f.usage {
                usage.agent = bounded_text(&usage.agent, 128);
                usage.model = bounded_text(&usage.model, 128);
            }
        }
        _ => unreachable!(),
    }
    Cow::Owned(bounded)
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
        Body::RequestOpened(_)
        | Body::RequestClosed(_)
        | Body::DecisionTransmissionStarted(_)
        | Body::DecisionApplied(_)
        | Body::DecisionNotApplied(_)
        | Body::DecisionUnconfirmed(_)
        | Body::LimitObserved(_)
        | Body::LimitReached(_)
        | Body::AttachmentReady(_)
        | Body::AttachmentFailed(_)
        | Body::JournalClosed(_)
        | Body::BlockStarted(_)
        | Body::BlockDelta(_)
        | Body::BlockFinal(_)
        | Body::BlockIncomplete(_)
        | Body::BlockMedia(_)
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

    async fn machine_report(
        &self,
        machine_id: Uuid,
        report: canterlot_proto::worker::v1::MachineReport,
    ) -> Result<(), tonic::Status> {
        let owner: Uuid = sqlx::query_scalar("select owner_id from machines where id=$1")
            .bind(machine_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| tonic::Status::internal("machine_report_database"))?;
        crate::machine_info::apply_report(&self.pool, owner, machine_id, &report, Uuid::now_v7())
            .await
            .map_err(|_| tonic::Status::internal("machine_report_database"))?;
        if let Some(registry) = &self.registry {
            crate::limits::push_machine_snapshot(&self.pool, registry, machine_id)
                .await
                .map_err(|_| tonic::Status::internal("limits_snapshot_database"))?;
        }
        Ok(())
    }
    async fn machine_metrics(
        &self,
        machine_id: Uuid,
        metrics: canterlot_proto::worker::v1::MachineMetrics,
    ) -> Result<(), tonic::Status> {
        let owner: Uuid = sqlx::query_scalar("select owner_id from machines where id=$1")
            .bind(machine_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| tonic::Status::internal("machine_metrics_database"))?;
        crate::machine_info::apply_metrics(&self.pool, owner, machine_id, &metrics, Uuid::now_v7())
            .await
            .map_err(|_| tonic::Status::internal("machine_metrics_database"))?;
        Ok(())
    }
    async fn limits_report(
        &self,
        machine_id: Uuid,
        report: canterlot_proto::worker::v1::LimitsReport,
    ) -> Result<(), tonic::Status> {
        use crate::limits::{self, Observation, ObservationSource, Window};
        let mut observations = Vec::new();
        let now = chrono::Utc::now();
        for f in report.observations {
            let Ok(window) = Window::from_proto(f.window) else {
                continue;
            };
            let Ok(used) = u16::try_from(f.used_permille) else {
                continue;
            };
            if used > 1000 {
                continue;
            }
            let Ok(reset) = limits::timestamp(f.resets_at_ms) else {
                continue;
            };
            let at = self
                .registry
                .as_ref()
                .and_then(|r| r.to_hub_time(machine_id, f.observed_machine_ms));
            observations.push((
                Observation {
                    provider: f.provider,
                    window,
                    used_permille: used,
                    resets_at: reset,
                    observed_at: at
                        .unwrap_or_else(|| self.hub_time(machine_id, f.observed_machine_ms, now)),
                    source: ObservationSource::Poll,
                    machine_id,
                    execution_id: None,
                },
                at.is_some(),
            ));
        }
        observations.sort_by(|(a, _), (b, _)| a.provider.cmp(&b.provider));
        let pool = self.pool.clone();
        let limits = self.limits.clone();
        let outcome = crate::db::in_tx(&pool, move |conn| {
            Box::pin(async move {
                let owner: Uuid = sqlx::query_scalar("select owner_id from machines where id=$1")
                    .bind(machine_id)
                    .fetch_one(&mut *conn)
                    .await?;
                let providers: BTreeSet<_> = observations
                    .iter()
                    .filter(|(o, _)| limits::known_provider(&o.provider))
                    .map(|(o, _)| o.provider.as_str())
                    .collect();
                for provider in providers {
                    limits::lock_provider(&mut *conn, provider).await?;
                }
                let mut changed = BTreeSet::new();
                let mut unmapped = 0_u64;
                for (o, mapped) in observations {
                    if !limits::known_provider(&o.provider) {
                        continue;
                    }
                    let recorded =
                        limits::record_in_tx(&mut *conn, &o, now, &limits, mapped).await?;
                    if recorded.provider_changed {
                        changed.insert((o.provider, o.window));
                    }
                    if !mapped {
                        unmapped += 1;
                    }
                }
                for (provider, window) in &changed {
                    limits::emit_changed(&mut *conn, owner, provider, *window, Uuid::now_v7())
                        .await?;
                }
                if unmapped > 0 {
                    events::append(
                        &mut *conn,
                        NewEvent {
                            scope: Scope::Conversations { owner_id: owner },
                            kind: "limit_observation_unmapped",
                            conversation_id: None,
                            execution_id: None,
                            payload: json!({"machine_id":machine_id,"count":unmapped}),
                            correlation_id: Uuid::now_v7(),
                        },
                    )
                    .await?;
                }
                Ok::<_, sqlx::Error>((changed, unmapped))
            })
        })
        .await;
        let (changed, unmapped) =
            outcome.map_err(|_| tonic::Status::internal("limits_report_database"))?;
        crate::scheduler::notify_pool(&self.pool);
        self.ignored.fetch_add(unmapped, Ordering::Relaxed);
        if let Some(registry) = &self.registry {
            let providers: BTreeSet<_> = changed.iter().map(|(p, _)| p).collect();
            for provider in providers {
                if limits::push_limits_snapshot(&self.pool, registry, provider)
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        kind = "limits_snapshot_push_failed",
                        "limits snapshot push failed"
                    );
                }
            }
        }
        Ok(())
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
    let attachment = match body {
        Body::AttachmentReady(fact) => Some((
            fact.command_id.as_str(),
            fact.dispatch_attempt,
            fact.attachment_id.as_str(),
            "ready",
            None,
            None,
        )),
        Body::AttachmentFailed(fact) => Some((
            fact.command_id.as_str(),
            fact.dispatch_attempt,
            fact.attachment_id.as_str(),
            if fact.retryable { "pending" } else { "failed" },
            Some(sanitize_reason(&fact.reason)),
            Some(fact.retryable),
        )),
        _ => None,
    };
    if let Some((command, dispatch_attempt, attachment, state, reason, retryable)) = attachment {
        if kind != JournalKind::Cj {
            return Err(IngestError::InvalidRecord);
        }
        let command = parse_id(command)?;
        let attachment = parse_id(attachment)?;
        let dispatch_attempt = attempt(dispatch_attempt)?;
        let conversation: Option<Uuid> = sqlx::query_scalar(
            "select q.conversation_id from queue_state q
             join conversations conv on conv.id=q.conversation_id
             join commands c on c.conversation_id=q.conversation_id
             where c.id=$1 and c.machine_id=$2 and conv.machine_id=$2
               and c.kind='deliver_input' for update of q",
        )
        .bind(command)
        .bind(machine_id)
        .fetch_optional(&mut *conn)
        .await?;
        let Some(conversation_id) = conversation else {
            return Ok(Some(true));
        };
        let target: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
            "select i.id,c.execution_id from inputs i
             join commands c on c.id=i.command_id
             join input_attachments ia on ia.input_id=i.id
             where c.id=$1 and c.dispatch_attempt=$2 and ia.attachment_id=$3
               and i.state not in ('cancelled','rejected')",
        )
        .bind(command)
        .bind(dispatch_attempt)
        .bind(attachment)
        .fetch_optional(&mut *conn)
        .await?;
        let Some((input_id, execution_id)) = target else {
            return Ok(Some(true));
        };
        let changed = sqlx::query(
            "update input_attachments set machine_state=$3,machine_reason=$4,retryable=$5,
                updated_at=clock_timestamp()
             where input_id=$1 and attachment_id=$2
               and (machine_state,machine_reason,retryable) is distinct from ($3,$4,$5)",
        )
        .bind(input_id)
        .bind(attachment)
        .bind(state)
        .bind(&reason)
        .bind(retryable)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if changed > 0 {
            events::append(
                conn,
                NewEvent {
                    scope: Scope::Conversation {
                        id: conversation_id,
                    },
                    kind: "attachment_state_changed",
                    conversation_id: Some(conversation_id),
                    execution_id,
                    payload: json!({"input_id":input_id,"attachment_id":attachment,
                    "machine_state":state,"machine_reason":reason}),
                    correlation_id,
                },
            )
            .await?;
        }
        return Ok(Some(false));
    }
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
            | Body::BlockMedia(_)
            | Body::PhaseChanged(_)
            | Body::JournalClosed(_)
            | Body::RequestOpened(_)
            | Body::RequestClosed(_)
            | Body::DecisionTransmissionStarted(_)
            | Body::DecisionApplied(_)
            | Body::DecisionNotApplied(_)
            | Body::DecisionUnconfirmed(_)
    ) {
        return Ok(None);
    }
    if kind == JournalKind::Cj {
        // Управляющая служба пишет в CJ отказ по решению, когда исполнение его уже не примет.
        let decision = match body {
            Body::DecisionApplied(fact) => Some((&fact.request_id, &fact.decision_id)),
            Body::DecisionNotApplied(fact) => Some((&fact.request_id, &fact.decision_id)),
            Body::DecisionUnconfirmed(fact) => Some((&fact.request_id, &fact.decision_id)),
            _ => None,
        };
        if let Some((request_id, decision_id)) = decision {
            let Some(run) = decision_execution(conn, machine_id, request_id, decision_id).await?
            else {
                return Ok(Some(true));
            };
            sqlx::query(
                "select conversation_id from queue_state where conversation_id = $1 for update",
            )
            .bind(run.conversation_id)
            .execute(&mut *conn)
            .await?;
            let ignored = match body {
                Body::DecisionApplied(fact) => {
                    apply_decision_applied(conn, &run, fact, correlation_id).await?
                }
                Body::DecisionNotApplied(fact) => {
                    apply_decision_not_applied(conn, &run, fact, correlation_id).await?
                }
                Body::DecisionUnconfirmed(fact) => {
                    apply_decision_unconfirmed(conn, &run, fact, correlation_id).await?
                }
                _ => unreachable!("decision fact"),
            };
            return Ok(Some(ignored));
        }
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
    if let Body::BlockMedia(fact) = body {
        return crate::media::attach_in_tx(conn, &run, fact, correlation_id)
            .await
            .map(Some)
            .map_err(|error| match error {
                sqlx::Error::InvalidArgument(_) => IngestError::InvalidRecord,
                _ => IngestError::Database,
            });
    }
    match body {
        Body::RequestOpened(fact) => {
            return apply_request_opened(conn, &run, fact, correlation_id)
                .await
                .map(Some);
        }
        Body::RequestClosed(fact) => {
            return apply_request_closed(conn, &run, fact, correlation_id)
                .await
                .map(Some);
        }
        Body::DecisionTransmissionStarted(fact) => {
            return apply_decision_transmission_started(conn, &run, fact, correlation_id)
                .await
                .map(Some);
        }
        Body::DecisionApplied(fact) => {
            return apply_decision_applied(conn, &run, fact, correlation_id)
                .await
                .map(Some);
        }
        Body::DecisionNotApplied(fact) => {
            return apply_decision_not_applied(conn, &run, fact, correlation_id)
                .await
                .map(Some);
        }
        Body::DecisionUnconfirmed(fact) => {
            return apply_decision_unconfirmed(conn, &run, fact, correlation_id)
                .await
                .map(Some);
        }
        _ => {}
    }
    let outcome = match body {
        Body::BlockStarted(fact) => blocks::start_block(conn, &run, fact, correlation_id).await,
        Body::BlockDelta(fact) => blocks::apply_delta(conn, &run, fact, correlation_id).await,
        Body::BlockFinal(fact) => blocks::finalize(conn, &run, fact, correlation_id).await,
        Body::BlockIncomplete(fact) => blocks::mark_incomplete(conn, &run, fact, correlation_id).await,
        Body::PhaseChanged(fact) => {
            if parse_id(&fact.execution_id)? != run.execution_id { return Err(IngestError::ForeignExecution); }
            sqlx::query("select conversation_id from queue_state where conversation_id = $1 for update")
                .bind(run.conversation_id).execute(&mut *conn).await?;
            let changed = sqlx::query("update executions set phase = $2, phase_at = clock_timestamp(), phase_detail=$3 where id = $1 and state not in ('stopped', 'failed')")
                .bind(run.execution_id).bind(&fact.phase).bind(json!({"phase":fact.phase,"tool_name":crate::blocks::non_empty(&fact.tool_name),"summary":crate::blocks::non_empty(&fact.summary)})).execute(&mut *conn).await?.rows_affected();
            if changed == 0 { return Ok(Some(true)); }
            execution_event(conn, &run, "phase_changed", json!({"phase": fact.phase,"tool_name":crate::blocks::non_empty(&fact.tool_name),"summary":crate::blocks::non_empty(&fact.summary)}), correlation_id).await?;
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

/// Исполнение решения из CJ: запрос машины с тем же решением-победителем.
async fn decision_execution(
    conn: &mut PgConnection,
    machine_id: Uuid,
    request_id: &str,
    decision_id: &str,
) -> Result<Option<ExecutionRef>, IngestError> {
    let Ok(decision_id) = Uuid::parse_str(decision_id) else {
        return Ok(None);
    };
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select r.execution_id, r.conversation_id from requests r
         join conversations c on c.id = r.conversation_id
         where (r.id = $1 or r.native_request_id = $2) and r.winner_decision_id = $3
           and c.machine_id = $4",
    )
    .bind(Uuid::parse_str(request_id).ok())
    .bind(request_id)
    .bind(decision_id)
    .bind(machine_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|(execution_id, conversation_id)| ExecutionRef {
        execution_id,
        conversation_id,
        machine_id,
    }))
}

type QueueRowTuple = (
    sqlx::types::Json<Vec<PauseReason>>,
    sqlx::types::Json<Vec<HoldReason>>,
    String,
    Option<Uuid>,
    Option<i32>,
);

async fn apply_request_opened(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &RequestOpened,
    correlation_id: Uuid,
) -> Result<bool, IngestError> {
    if !fact.execution_id.is_empty() && parse_id(&fact.execution_id)? != run.execution_id {
        return Err(IngestError::ForeignExecution);
    }
    // Запрос завершённого исполнения уже не получит решения: карточку не открываем.
    let ended: bool = sqlx::query_scalar(
        "select state in ('stopped', 'failed', 'owner_lost') from executions where id = $1",
    )
    .bind(run.execution_id)
    .fetch_one(&mut *conn)
    .await?;
    if ended {
        return Ok(true);
    }
    let req_uuid = Uuid::parse_str(&fact.request_id).ok();
    let existing: Option<Uuid> = sqlx::query_scalar(
        "select id from requests where (execution_id = $1 and native_request_id = $2) or ($3 is not null and id = $3)",
    )
    .bind(run.execution_id)
    .bind(&fact.request_id)
    .bind(req_uuid)
    .fetch_optional(&mut *conn)
    .await?;

    if existing.is_some() {
        return Ok(true);
    }

    let id = req_uuid.unwrap_or_else(Uuid::now_v7);
    let kind = fact
        .kind
        .parse::<crate::requests::RequestKind>()
        .map_err(|_| IngestError::InvalidRecord)?;
    let payload_val: Value = if fact.payload_json.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&fact.payload_json).map_err(|_| IngestError::InvalidRecord)?
    };
    let generation = i64::try_from(fact.generation).map_err(|_| IngestError::InvalidRecord)?;

    sqlx::query("select id from conversations where id = $1 for no key update")
        .bind(run.conversation_id)
        .fetch_one(&mut *conn)
        .await?;

    sqlx::query(
        "insert into requests (
            id, conversation_id, execution_id, generation,
            kind, native_request_id, state, request_payload,
            created_at, updated_at
        ) values (
            $1, $2, $3, $4,
            $5, $6, 'open', $7,
            clock_timestamp(), clock_timestamp()
        )",
    )
    .bind(id)
    .bind(run.conversation_id)
    .bind(run.execution_id)
    .bind(generation)
    .bind(kind)
    .bind(&fact.request_id)
    .bind(&payload_val)
    .execute(&mut *conn)
    .await?;

    let queue_row: Option<QueueRowTuple> = sqlx::query_as(
        "select pause_reasons, hold_reasons, run_slot, run_slot_input_id, run_slot_attempt
         from queue_state where conversation_id = $1 for update",
    )
    .bind(run.conversation_id)
    .fetch_optional(&mut *conn)
    .await?;

    if let Some((pauses, mut holds, slot_str, slot_input, slot_attempt)) = queue_row
        && !holds.0.contains(&HoldReason::AwaitingUserRequest)
    {
        holds.0.push(HoldReason::AwaitingUserRequest);
        sqlx::query("update queue_state set hold_reasons = $2 where conversation_id = $1")
            .bind(run.conversation_id)
            .bind(&holds)
            .execute(&mut *conn)
            .await?;

        let slot = RunSlot::from_columns(&slot_str, slot_input, slot_attempt)
            .map_err(|_| IngestError::Database)?;
        events::append(
            conn,
            NewEvent {
                scope: Scope::Conversation {
                    id: run.conversation_id,
                },
                kind: "queue_state_changed",
                conversation_id: Some(run.conversation_id),
                execution_id: Some(run.execution_id),
                payload: json!({
                    "pause_reasons": pauses.0,
                    "hold_reasons": holds.0,
                    "run_slot": slot,
                }),
                correlation_id,
            },
        )
        .await?;
    }

    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: run.conversation_id,
            },
            kind: "request_opened",
            conversation_id: Some(run.conversation_id),
            execution_id: Some(run.execution_id),
            payload: json!({
                "request_id": id,
                "kind": fact.kind,
                "payload": payload_val,
            }),
            correlation_id,
        },
    )
    .await?;

    Ok(false)
}

struct LockedRequest {
    id: Uuid,
    conversation_id: Uuid,
    state: String,
    winner: Option<Uuid>,
}

/// Строка запроса исполнения под блокировкой; очередь разговора уже заблокирована вызывающим.
async fn locked_request(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    request_id: &str,
) -> Result<Option<LockedRequest>, IngestError> {
    let row: Option<(Uuid, Uuid, String, Option<Uuid>)> = sqlx::query_as(
        "select id, conversation_id, state, winner_decision_id from requests
         where (id = $1 or native_request_id = $2) and execution_id = $3 for update",
    )
    .bind(Uuid::parse_str(request_id).ok())
    .bind(request_id)
    .bind(run.execution_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(
        row.map(|(id, conversation_id, state, winner)| LockedRequest {
            id,
            conversation_id,
            state,
            winner,
        }),
    )
}

impl LockedRequest {
    /// Факт относится к решению-победителю этого запроса.
    fn won_by(&self, decision_id: &str) -> bool {
        self.winner.is_some() && Uuid::parse_str(decision_id).ok() == self.winner
    }
}

/// Гасит команду доставки решения только этого запроса.
async fn complete_decision_command(
    conn: &mut PgConnection,
    request: &LockedRequest,
    result: Value,
) -> Result<(), IngestError> {
    if let Some(winner) = request.winner {
        crate::requests::complete_decision_command_in_tx(
            conn,
            request.conversation_id,
            request.id,
            winner,
            result,
        )
        .await?;
    }
    Ok(())
}

async fn set_request_state(
    conn: &mut PgConnection,
    request: &LockedRequest,
    state: &str,
) -> Result<(), IngestError> {
    sqlx::query("update requests set state = $2, updated_at = clock_timestamp() where id = $1")
        .bind(request.id)
        .bind(state)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn request_closed_event(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    request: &LockedRequest,
    reason: &str,
    correlation_id: Uuid,
) -> Result<(), IngestError> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: request.conversation_id,
            },
            kind: "request_closed",
            conversation_id: Some(request.conversation_id),
            execution_id: Some(run.execution_id),
            payload: json!({
                "request_id": request.id,
                "reason": reason,
            }),
            correlation_id,
        },
    )
    .await?;
    clear_awaiting_hold_if_no_open_requests(
        conn,
        request.conversation_id,
        run.execution_id,
        correlation_id,
    )
    .await
}

fn non_empty_reason(reason: &str, fallback: &str) -> String {
    if reason.is_empty() {
        fallback.to_owned()
    } else {
        sanitize_reason(reason)
    }
}

async fn apply_decision_transmission_started(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &DecisionTransmissionStarted,
    correlation_id: Uuid,
) -> Result<bool, IngestError> {
    let Some(request) = locked_request(conn, run, &fact.request_id).await? else {
        return Ok(true);
    };
    if request.state != "decision_saved" || !request.won_by(&fact.decision_id) {
        return Ok(true);
    }
    set_request_state(conn, &request, "decision_transmitting").await?;
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: request.conversation_id,
            },
            kind: "request_transmitting",
            conversation_id: Some(request.conversation_id),
            execution_id: Some(run.execution_id),
            payload: json!({ "request_id": request.id }),
            correlation_id,
        },
    )
    .await?;
    Ok(false)
}

async fn apply_decision_applied(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &DecisionApplied,
    correlation_id: Uuid,
) -> Result<bool, IngestError> {
    let Some(request) = locked_request(conn, run, &fact.request_id).await? else {
        return Ok(true);
    };
    if !matches!(
        request.state.as_str(),
        "decision_saved" | "decision_transmitting" | "decision_unconfirmed"
    ) || !request.won_by(&fact.decision_id)
    {
        return Ok(true);
    }
    set_request_state(conn, &request, "decision_applied").await?;
    complete_decision_command(conn, &request, json!({"applied": true})).await?;
    request_closed_event(conn, run, &request, "applied", correlation_id).await?;
    Ok(false)
}

async fn apply_decision_not_applied(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &DecisionNotApplied,
    correlation_id: Uuid,
) -> Result<bool, IngestError> {
    let Some(request) = locked_request(conn, run, &fact.request_id).await? else {
        return Ok(true);
    };
    if !matches!(
        request.state.as_str(),
        "decision_saved" | "decision_transmitting"
    ) || !request.won_by(&fact.decision_id)
    {
        return Ok(true);
    }
    let reason = non_empty_reason(&fact.reason, "not_applied");
    set_request_state(conn, &request, "decision_not_applied").await?;
    complete_decision_command(conn, &request, json!({"applied": false, "reason": reason})).await?;
    request_closed_event(conn, run, &request, &reason, correlation_id).await?;
    Ok(false)
}

async fn apply_request_closed(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &RequestClosed,
    correlation_id: Uuid,
) -> Result<bool, IngestError> {
    let Some(request) = locked_request(conn, run, &fact.request_id).await? else {
        return Ok(true);
    };
    let reason = non_empty_reason(&fact.reason, "closed");
    match request.state.as_str() {
        "open" => set_request_state(conn, &request, "closed_without_decision").await?,
        // Запрос закрылся на машине раньше, чем решение было передано агенту.
        "decision_saved" => {
            set_request_state(conn, &request, "decision_not_applied").await?;
            complete_decision_command(conn, &request, json!({"applied": false, "reason": reason}))
                .await?;
        }
        _ => return Ok(true),
    }
    request_closed_event(conn, run, &request, &reason, correlation_id).await?;
    Ok(false)
}

async fn apply_decision_unconfirmed(
    conn: &mut PgConnection,
    run: &ExecutionRef,
    fact: &DecisionUnconfirmed,
    correlation_id: Uuid,
) -> Result<bool, IngestError> {
    let Some(request) = locked_request(conn, run, &fact.request_id).await? else {
        return Ok(true);
    };
    if !matches!(
        request.state.as_str(),
        "decision_saved" | "decision_transmitting"
    ) || !request.won_by(&fact.decision_id)
    {
        return Ok(true);
    }
    // Повтор запрещён: команда гасится, поздний факт применения ещё переведёт запрос.
    set_request_state(conn, &request, "decision_unconfirmed").await?;
    complete_decision_command(
        conn,
        &request,
        json!({"applied": null, "reason": non_empty_reason(&fact.reason, "undetermined")}),
    )
    .await?;
    request_closed_event(conn, run, &request, "unconfirmed", correlation_id).await?;
    Ok(false)
}

async fn clear_awaiting_hold_if_no_open_requests(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    execution_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), IngestError> {
    let open_count: i64 = sqlx::query_scalar(crate::requests::LIVE_OPEN_REQUESTS)
        .bind(conversation_id)
        .fetch_one(&mut *conn)
        .await?;

    if open_count == 0 {
        let queue_opt: Option<QueueRowTuple> = sqlx::query_as(
            "select pause_reasons, hold_reasons, run_slot, run_slot_input_id, run_slot_attempt
             from queue_state where conversation_id = $1 for update",
        )
        .bind(conversation_id)
        .fetch_optional(&mut *conn)
        .await?;

        if let Some((pauses, mut holds, slot_str, slot_input, slot_attempt)) = queue_opt
            && holds.0.contains(&HoldReason::AwaitingUserRequest)
        {
            holds.0.retain(|r| *r != HoldReason::AwaitingUserRequest);
            sqlx::query("update queue_state set hold_reasons = $2 where conversation_id = $1")
                .bind(conversation_id)
                .bind(&holds)
                .execute(&mut *conn)
                .await?;

            let slot = RunSlot::from_columns(&slot_str, slot_input, slot_attempt)
                .map_err(|_| IngestError::Database)?;
            events::append(
                conn,
                NewEvent {
                    scope: Scope::Conversation {
                        id: conversation_id,
                    },
                    kind: "queue_state_changed",
                    conversation_id: Some(conversation_id),
                    execution_id: Some(execution_id),
                    payload: json!({
                        "pause_reasons": pauses.0,
                        "hold_reasons": holds.0,
                        "run_slot": slot,
                    }),
                    correlation_id,
                },
            )
            .await?;
        }
    }
    Ok(())
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
