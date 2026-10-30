use std::collections::{HashMap, HashSet};

use canterlot_proto::{
    limits::{MAX_MESSAGE_BYTES, check_deliver_input},
    worker::v1::{
        CancelInput, Command, ControlDown, DeliverDecision, DeliverInput, Instructions, Interrupt,
        SettingsOverride, StartExecution, StopExecution, WithdrawInput, command, control_down,
    },
};
use prost::Message;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgListener};
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinSet,
    time::Instant,
};
use uuid::Uuid;

use crate::{
    config::HubTimers,
    delivery::{
        receipts::{ApplyOutcome, InputReceipt, apply_input_receipt_in_tx},
        submit,
    },
    dispatcher::wait_for_shutdown,
    session::{Availability, Registry},
};

#[derive(sqlx::FromRow)]
pub struct CommandRow {
    pub id: Uuid,
    pub machine_id: Uuid,
    pub conversation_id: Option<Uuid>,
    pub execution_id: Option<Uuid>,
    pub execution_generation: Option<i64>,
    pub kind: String,
    pub payload: Value,
    pub dispatch_attempt: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error("invalid_command")]
    InvalidCommand,
    #[error("resource_limit")]
    ResourceLimit,
    #[error("capability_missing")]
    CapabilityMissing,
    #[error("session_changed")]
    SessionChanged,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputPayload {
    input_id: Uuid,
    conversation_id: Uuid,
    content: Vec<submit::ContentBlock>,
    /// Те же вложения, что и блоки `attachment` в `content`; поле служит для проверок в SQL.
    #[serde(default)]
    #[allow(dead_code)]
    attachment_ids: Vec<Uuid>,
    settings: Option<SettingsPayload>,
    settings_rev: Option<i64>,
}
#[derive(Deserialize)]
struct SettingsPayload {
    model: Option<String>,
    effort: Option<String>,
    permission_mode: Option<String>,
    /// Снимок инструкций пресета (задача 3 кладёт сюда объект `instructions`).
    #[serde(default)]
    instructions: Option<InstructionPayload>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstructionPayload {
    mode: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    preset_id: String,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    title: String,
}

fn instructions_proto(payload: &Option<InstructionPayload>) -> Option<Instructions> {
    payload.as_ref().map(|preset| Instructions {
        mode: preset.mode.clone(),
        text: preset.text.clone(),
        preset_id: preset.preset_id.clone(),
        revision: preset.revision,
        title: preset.title.clone(),
    })
}

fn text(payload: &Value, key: &str) -> Result<String, OutboxError> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(OutboxError::InvalidCommand)
}

fn target(payload: &Value) -> Result<(String, u32), OutboxError> {
    let id = text(payload, "target_command_id")?;
    Uuid::parse_str(&id).map_err(|_| OutboxError::InvalidCommand)?;
    let attempt = payload
        .get("target_dispatch_attempt")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(OutboxError::InvalidCommand)?;
    Ok((id, attempt))
}

pub fn to_proto_for_session(row: &CommandRow, registry: &Registry) -> Result<Command, OutboxError> {
    to_proto_with_capability(row, &registry.capabilities(row.machine_id))
}

fn to_proto_for_connection(
    row: &CommandRow,
    registry: &Registry,
    connection: Uuid,
) -> Result<Command, OutboxError> {
    let capabilities = registry
        .capabilities_for_connection(row.machine_id, connection)
        .ok_or(OutboxError::SessionChanged)?;
    to_proto_with_capability(row, &capabilities)
}

fn to_proto_with_capability(
    row: &CommandRow,
    capabilities: &[String],
) -> Result<Command, OutboxError> {
    let has = |wanted: &str| capabilities.iter().any(|capability| capability == wanted);
    let pilot = has(crate::session::CAPABILITY_PILOT_V1);
    // Старый воркер проигнорировал бы снимок и запустил конфигурацию владельца.
    if row.kind == "start_execution"
        && row
            .payload
            .pointer("/instructions/mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode != "owner")
        && !has(crate::session::CAPABILITY_INSTRUCTIONS_V1)
    {
        return Err(OutboxError::CapabilityMissing);
    }
    if row.kind == "deliver_input"
        && !pilot
        && (row.payload.get("settings").is_some()
            || row
                .payload
                .get("attachment_ids")
                .and_then(Value::as_array)
                .is_some_and(|ids| !ids.is_empty()))
    {
        return Err(OutboxError::CapabilityMissing);
    }
    let mut command = to_proto(row)?;
    if pilot && let Some(command::Kind::StartExecution(start)) = &mut command.kind {
        start.capabilities = vec![crate::session::CAPABILITY_PILOT_V1.into()];
    }
    if command.encoded_len() > MAX_MESSAGE_BYTES {
        return Err(OutboxError::ResourceLimit);
    }
    Ok(command)
}

pub fn to_proto(row: &CommandRow) -> Result<Command, OutboxError> {
    let kind = match row.kind.as_str() {
        "deliver_input" => {
            let payload: InputPayload = serde_json::from_value(row.payload.clone())
                .map_err(|_| OutboxError::InvalidCommand)?;
            let _revision = payload.settings_rev;
            command::Kind::DeliverInput(DeliverInput {
                input_id: payload.input_id.to_string(),
                conversation_id: payload.conversation_id.to_string(),
                content: payload
                    .content
                    .iter()
                    .map(submit::ContentBlock::to_proto)
                    .collect(),
                settings: payload.settings.map(|s| SettingsOverride {
                    model: s.model.unwrap_or_default(),
                    effort: s.effort.unwrap_or_default(),
                    permission_mode: s.permission_mode.unwrap_or_default(),
                    instructions: instructions_proto(&s.instructions),
                }),
            })
        }
        "start_execution" => command::Kind::StartExecution(StartExecution {
            conversation_id: row
                .conversation_id
                .ok_or(OutboxError::InvalidCommand)?
                .to_string(),
            harness: text(&row.payload, "harness")?,
            workdir: text(&row.payload, "workdir")?,
            native_conversation_id: row
                .payload
                .get("native_conversation_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            model: row
                .payload
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            effort: row
                .payload
                .get("effort")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            permission_mode: row
                .payload
                .get("permission_mode")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            instructions: row
                .payload
                .get("instructions")
                .map(|value| {
                    serde_json::from_value::<InstructionPayload>(value.clone())
                        .map(|preset| Instructions {
                            mode: preset.mode,
                            text: preset.text,
                            preset_id: preset.preset_id,
                            revision: preset.revision,
                            title: preset.title,
                        })
                        .map_err(|_| OutboxError::InvalidCommand)
                })
                .transpose()?,
            capabilities: vec![],
        }),
        "cancel_input" => {
            let (target_command_id, target_dispatch_attempt) = target(&row.payload)?;
            command::Kind::CancelInput(CancelInput {
                target_command_id,
                target_dispatch_attempt,
            })
        }
        "withdraw_input" => {
            let (target_command_id, target_dispatch_attempt) = target(&row.payload)?;
            command::Kind::WithdrawInput(WithdrawInput {
                target_command_id,
                target_dispatch_attempt,
            })
        }
        "deliver_decision" => {
            // Сопровождение ищет запрос по нативному идентификатору из своего SJ.
            let request_id = match row.payload.get("native_request_id") {
                Some(value) => value
                    .as_str()
                    .ok_or(OutboxError::InvalidCommand)?
                    .to_owned(),
                None => text(&row.payload, "request_id")?,
            };
            let decision_id = text(&row.payload, "decision_id")?;
            Uuid::parse_str(&decision_id).map_err(|_| OutboxError::InvalidCommand)?;
            let decision = row
                .payload
                .get("decision_payload")
                .filter(|value| value.is_object())
                .ok_or(OutboxError::InvalidCommand)?;
            command::Kind::DeliverDecision(DeliverDecision {
                request_id,
                decision_id,
                generation: row
                    .payload
                    .get("generation")
                    .and_then(Value::as_u64)
                    .ok_or(OutboxError::InvalidCommand)?,
                kind: text(&row.payload, "kind")?,
                decision_payload_json: serde_json::to_vec(decision)
                    .map_err(|_| OutboxError::InvalidCommand)?,
            })
        }
        "interrupt" => command::Kind::Interrupt(Interrupt {}),
        "stop_execution" => command::Kind::StopExecution(StopExecution {
            force: match row.payload.get("force") {
                None => false,
                Some(value) => value.as_bool().ok_or(OutboxError::InvalidCommand)?,
            },
        }),
        _ => return Err(OutboxError::InvalidCommand),
    };
    let command = Command {
        command_id: row.id.to_string(),
        dispatch_attempt: row
            .dispatch_attempt
            .try_into()
            .map_err(|_| OutboxError::InvalidCommand)?,
        execution_id: row
            .execution_id
            .ok_or(OutboxError::InvalidCommand)?
            .to_string(),
        execution_generation: row
            .execution_generation
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(OutboxError::InvalidCommand)?,
        kind: Some(kind),
    };
    check_deliver_input(&command).map_err(|_| OutboxError::ResourceLimit)?;
    if envelope(command.clone()).encoded_len() > MAX_MESSAGE_BYTES {
        return Err(OutboxError::ResourceLimit);
    }
    Ok(command)
}

fn envelope(command: Command) -> ControlDown {
    ControlDown {
        msg: Some(control_down::Msg::Command(command)),
    }
}

pub async fn run(
    pool: PgPool,
    registry: Registry,
    timers: HubTimers,
    mut shutdown: watch::Receiver<bool>,
) {
    if *shutdown.borrow() {
        return;
    }
    let mut changes = registry.subscribe_changes();
    let context = SenderContext {
        pool: pool.clone(),
        registry,
        timers: timers.clone(),
        shutdown: shutdown.clone(),
    };
    let mut sweep = tokio::time::interval(timers.dispatch_sweep);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut listener: Option<PgListener> = None;
    let mut workers: HashMap<Uuid, mpsc::Sender<()>> = HashMap::new();
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => break,
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if result.is_err() { eprintln!("outbox_machine_task_error"); }
            },
            change = changes.recv() => match change {
                Ok((machine, Availability::Available)) => wake_machine(machine, &context, &mut workers, &mut tasks),
                Err(broadcast::error::RecvError::Lagged(_)) => { sweep.reset_immediately(); },
                _ => {},
            },
            notification = async { listener.as_mut().expect("listener").recv().await }, if listener.is_some() => {
                match notification {
                    Ok(notification) => if let Ok(machine) = Uuid::parse_str(notification.payload()) {
                        wake_machine(machine, &context, &mut workers, &mut tasks);
                    },
                    Err(_) => { listener = None; eprintln!("outbox_listener_error"); },
                }
            },
            _ = sweep.tick() => {
                tokio::select! {
                    biased;
                    _ = wait_for_shutdown(&mut shutdown) => break,
                    _ = async {
                        if listener.is_none() {
                            match listen(&pool).await {
                                Ok(connected) => listener = Some(connected),
                                Err(_) => eprintln!("outbox_listener_error"),
                            }
                        }
                        if let Ok(machines) = sqlx::query_scalar::<_, Uuid>("select distinct machine_id from commands where delivery_state = 'pending'").fetch_all(&pool).await {
                            for machine in machines {
                                wake_machine(machine, &context, &mut workers, &mut tasks);
                            }
                        }
                        workers.retain(|_, worker| !worker.is_closed());
                    } => {},
                }
            },
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

async fn listen(pool: &PgPool) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen("canterlot_outbox").await?;
    Ok(listener)
}

#[derive(Clone)]
struct SenderContext {
    pool: PgPool,
    registry: Registry,
    timers: HubTimers,
    shutdown: watch::Receiver<bool>,
}

fn wake_machine(
    machine: Uuid,
    context: &SenderContext,
    workers: &mut HashMap<Uuid, mpsc::Sender<()>>,
    tasks: &mut JoinSet<()>,
) {
    if workers.get(&machine).is_none_or(mpsc::Sender::is_closed) {
        let (tx, rx) = mpsc::channel(1);
        workers.insert(machine, tx);
        tasks.spawn(run_machine(context.clone(), machine, rx));
    }
    let _ = workers[&machine].try_send(());
}

struct Sent {
    attempt: i32,
    deadline: Instant,
    resent: bool,
}

async fn run_machine(context: SenderContext, machine: Uuid, mut wake: mpsc::Receiver<()>) {
    let SenderContext {
        pool,
        registry,
        timers,
        mut shutdown,
    } = context;
    if *shutdown.borrow() {
        return;
    }
    let mut sent: HashMap<Uuid, Sent> = HashMap::new();
    let mut session: Option<mpsc::Sender<ControlDown>> = None;
    let mut closing = false;
    loop {
        let deadline = sent
            .values()
            .map(|item| item.deadline)
            .filter(|deadline| *deadline > Instant::now())
            .min()
            .unwrap_or_else(|| Instant::now() + timers.dispatch_sweep);
        tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => break,
            _ = wake.recv() => {},
            _ = tokio::time::sleep_until(deadline) => {},
        }
        let Some((sender, connection, credential)) = registry.command_session(machine) else {
            break;
        };
        let current = Some(sender);
        let changed = match (&session, &current) {
            (Some(previous), Some(current)) => !previous.same_channel(current),
            (None, None) => false,
            _ => true,
        };
        if changed {
            sent.clear();
            session = current.clone();
            closing = false;
        }
        if closing {
            continue;
        }
        let active = async {
            crate::machines::credential_active(&mut *pool.acquire().await?, machine, &credential)
                .await
        }
        .await;
        match active {
            Ok(true) => {}
            Ok(false) => {
                registry.revoke(machine, connection);
                break;
            }
            Err(_) => {
                tracing::warn!(kind = "outbox_database", machine_id = %machine, "credential check failed");
                tokio::time::sleep(timers.dispatch_sweep).await;
                continue;
            }
        }
        let rows = tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => break,
            rows = sqlx::query_as::<_, CommandRow>("select * from commands where machine_id = $1 and delivery_state = 'pending' order by created_at, id")
                .bind(machine).fetch_all(&pool) => rows,
        };
        let Ok(rows) = rows else {
            eprintln!("outbox_database_error");
            tokio::select! {
                _ = wait_for_shutdown(&mut shutdown) => break,
                _ = tokio::time::sleep(timers.dispatch_sweep) => {},
            }
            continue;
        };
        if rows.is_empty() {
            break;
        }
        let pending: HashSet<_> = rows
            .iter()
            .map(|row| (row.id, row.dispatch_attempt))
            .collect();
        sent.retain(|id, item| pending.contains(&(*id, item.attempt)));
        let Some(sender) = current else {
            continue;
        };
        for row in rows {
            if let Some(item) = sent.get(&row.id) {
                if item.deadline > Instant::now() {
                    continue;
                }
                if item.resent {
                    tracing::warn!(kind = "outbox_restart", command_id = %row.id, machine_id = %machine, "command receipt timeout");
                    // Пустое сообщение закрывает сеанс внутри gRPC и не попадает в сеть.
                    tokio::select! {
                        biased;
                        _ = wait_for_shutdown(&mut shutdown) => return,
                        _ = sender.send(ControlDown { msg: None }) => {},
                    }
                    sent.clear();
                    closing = true;
                    break;
                }
            } else if registry.availability(machine) != Availability::Available {
                continue;
            }
            let command = match to_proto_for_connection(&row, &registry, connection) {
                Ok(command) => command,
                Err(OutboxError::CapabilityMissing | OutboxError::SessionChanged) => continue,
                Err(error) => {
                    let reason = match error {
                        OutboxError::ResourceLimit => "resource_limit",
                        OutboxError::InvalidCommand => "unsupported_command",
                        OutboxError::CapabilityMissing | OutboxError::SessionChanged => {
                            unreachable!()
                        }
                    };
                    tokio::select! {
                        biased;
                        _ = wait_for_shutdown(&mut shutdown) => return,
                        result = reject(&pool, &row, reason) => if result.is_err() { eprintln!("outbox_rejection_error"); },
                    }
                    continue;
                }
            };
            match sender.try_send(envelope(command)) {
                Ok(()) => {
                    let resent = sent.contains_key(&row.id);
                    if resent {
                        tracing::info!(kind = "outbox_resend", command_id = %row.id, machine_id = %machine, "command resent");
                    }
                    sent.insert(
                        row.id,
                        Sent {
                            attempt: row.dispatch_attempt,
                            deadline: Instant::now() + timers.command_stored_receipt,
                            resent,
                        },
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => break,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    if let Some(item) = sent.get_mut(&row.id) {
                        item.deadline = Instant::now() + timers.dispatch_sweep;
                    }
                    break;
                }
            }
        }
    }
}

async fn reject(pool: &PgPool, row: &CommandRow, reason: &str) -> Result<(), sqlx::Error> {
    let (kind, id, attempt) = (row.kind.clone(), row.id, row.dispatch_attempt);
    let reason = reason.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            if kind == "deliver_input" {
                let outcome = apply_input_receipt_in_tx(
                    &mut *conn,
                    id,
                    attempt,
                    InputReceipt::RejectedBeforeTransmission {
                        reason: reason.clone(),
                    },
                    Uuid::now_v7(),
                )
                .await?;
                if matches!(outcome, ApplyOutcome::Ignored(_)) {
                    return Ok(());
                }
            }
            sqlx::query("update commands set delivery_state = 'completed', completed_at = coalesce(completed_at, now()), result = $2 where id = $1 and dispatch_attempt = $3 and (delivery_state = 'pending' or (kind = 'deliver_input' and last_status = 'rejected_before_transmission'))")
                .bind(id).bind(json!({"rejected": reason})).bind(attempt).execute(&mut *conn).await?;
            Ok(())
        })
    })
    .await
}
