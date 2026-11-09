use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use canterlot_proto::{
    limits::{MAX_MESSAGE_BYTES, check_deliver_input},
    worker::v1::{
        CancelInput, Command, CompactSession, ControlDown, DeliverDecision, DeliverInput,
        Instructions, Interrupt, RemoveWorktree, ResolveOwnerLost, SettingsOverride,
        StartExecution, SteerInput, StopExecution, WithdrawInput, WorktreeSpec, command,
        control_down,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorktreePayload {
    source_workdir: String,
    branch: String,
    base: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SteerPayload {
    input_id: Uuid,
    conversation_id: Uuid,
    content: Vec<submit::ContentBlock>,
    #[serde(default)]
    #[allow(dead_code)]
    attachment_ids: Vec<Uuid>,
    settings: Option<SettingsPayload>,
    #[allow(dead_code)]
    settings_rev: Option<i64>,
    #[serde(default)]
    expected_running_command_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveWorktreePayload {
    source_workdir: String,
    path: String,
    branch: String,
    #[serde(default)]
    force: bool,
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
    if row.kind == "compact_session" && !has(crate::session::CAPABILITY_COMPACT_V1) {
        return Err(OutboxError::CapabilityMissing);
    }
    // Старый воркер запустил бы сессию в исходном каталоге вместо рабочей копии.
    if ((row.kind == "start_execution" && row.payload.get("worktree").is_some())
        || row.kind == "remove_worktree")
        && !has(crate::session::CAPABILITY_SESSION_TOOLS_V1)
    {
        return Err(OutboxError::CapabilityMissing);
    }
    // Старый воркер не знает флага и загрузил бы MCP-серверы владельца в делегированную сессию.
    if row.kind == "start_execution"
        && isolate_mcp(&row.payload)
        && !has(crate::session::CAPABILITY_SESSION_TOOLS_V1)
    {
        return Err(OutboxError::CapabilityMissing);
    }
    if row.kind == "resolve_owner_lost" && !has(crate::session::CAPABILITY_OWNER_EXIT_V1) {
        return Err(OutboxError::CapabilityMissing);
    }
    // Старый воркер отклонил бы неизвестный вид команды чисто, но ввод при этом
    // остался бы выданным без исполнителя: не выпускаем команду из хаба.
    if row.kind == "steer_input" && !has(crate::session::CAPABILITY_SEND_NOW_V1) {
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
    if let Some(command::Kind::StartExecution(start)) = &mut command.kind {
        start.capabilities = [
            crate::session::CAPABILITY_PILOT_V1,
            crate::session::CAPABILITY_TODO_V1,
            crate::session::CAPABILITY_COMPACT_V1,
            crate::session::CAPABILITY_BACKGROUND_V1,
            crate::session::CAPABILITY_SESSION_TOOLS_V1,
            crate::session::CAPABILITY_SEND_NOW_V1,
        ]
        .into_iter()
        .filter(|capability| has(capability))
        .map(str::to_owned)
        .collect();
    }
    if command.encoded_len() > MAX_MESSAGE_BYTES {
        return Err(OutboxError::ResourceLimit);
    }
    Ok(command)
}

fn isolate_mcp(payload: &Value) -> bool {
    payload
        .get("isolate_mcp")
        .and_then(Value::as_bool)
        .unwrap_or(false)
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
            isolate_mcp: isolate_mcp(&row.payload),
            worktree: row
                .payload
                .get("worktree")
                .map(|value| {
                    serde_json::from_value::<WorktreePayload>(value.clone())
                        .map(|spec| WorktreeSpec {
                            source_workdir: spec.source_workdir,
                            branch: spec.branch,
                            base: spec.base,
                        })
                        .map_err(|_| OutboxError::InvalidCommand)
                })
                .transpose()?,
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
        "steer_input" => {
            let steer: SteerPayload = serde_json::from_value(row.payload.clone())
                .map_err(|_| OutboxError::InvalidCommand)?;
            command::Kind::SteerInput(SteerInput {
                input_id: steer.input_id.to_string(),
                conversation_id: steer.conversation_id.to_string(),
                content: steer
                    .content
                    .iter()
                    .map(submit::ContentBlock::to_proto)
                    .collect(),
                settings: steer.settings.map(|s| SettingsOverride {
                    model: s.model.unwrap_or_default(),
                    effort: s.effort.unwrap_or_default(),
                    permission_mode: s.permission_mode.unwrap_or_default(),
                    instructions: instructions_proto(&s.instructions),
                }),
                expected_running_command_id: steer.expected_running_command_id,
            })
        }
        "compact_session" => command::Kind::CompactSession(CompactSession {}),
        "remove_worktree" => {
            let remove: RemoveWorktreePayload = serde_json::from_value(row.payload.clone())
                .map_err(|_| OutboxError::InvalidCommand)?;
            command::Kind::RemoveWorktree(RemoveWorktree {
                source_workdir: remove.source_workdir,
                path: remove.path,
                branch: remove.branch,
                force: remove.force,
            })
        }
        "resolve_owner_lost" => command::Kind::ResolveOwnerLost(ResolveOwnerLost {}),
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

/// Захват команды для отправки: свежая строка и признак, что метку поставил
/// именно этот вызов. Метка, поставленная раньше (прошлым соединением,
/// прошлой задачей или прошлым процессом хаба), значит, что команда могла
/// уже уйти в транспорт: такой захват снимать нельзя.
#[derive(sqlx::FromRow)]
pub struct Claim {
    #[sqlx(flatten)]
    pub row: CommandRow,
    pub newly_claimed: bool,
}

/// Захватывает команду для отправки под замком очереди и возвращает её свежую
/// строку. После захвата правка ввода отклоняется, поэтому payload, прочитанный
/// здесь, уже не может разойтись с отправленным. Для ввода и вливания замок тот
/// же, что у правки, выдачи и квитанций.
pub async fn claim_for_send(pool: &PgPool, row: &CommandRow) -> Result<Option<Claim>, sqlx::Error> {
    let id = row.id;
    let attempt = row.dispatch_attempt;
    let conversation_id = row.conversation_id;
    let lock_queue = matches!(row.kind.as_str(), "deliver_input" | "steer_input");
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            if lock_queue && let Some(conversation_id) = conversation_id {
                sqlx::query(
                    "select conversation_id from queue_state where conversation_id = $1 for update",
                )
                .bind(conversation_id)
                .execute(&mut *conn)
                .await?;
            }
            sqlx::query_as::<_, Claim>(
                "with prev as (
                     select id, dispatch_claimed_at from commands
                     where id = $1 and dispatch_attempt = $2 and delivery_state = 'pending'
                     for update
                 )
                 update commands c set dispatch_claimed_at = coalesce(c.dispatch_claimed_at, now())
                 from prev where c.id = prev.id
                 returning c.id, c.machine_id, c.conversation_id, c.execution_id,
                           c.execution_generation, c.kind, c.payload, c.dispatch_attempt,
                           prev.dispatch_claimed_at is null as newly_claimed",
            )
            .bind(id)
            .bind(attempt)
            .fetch_optional(&mut *conn)
            .await
        })
    })
    .await
}

/// Снимает захват команды, которая точно не попала в транспорт. Замок тот же,
/// что у выдачи и правки: отмена не увидит команду захваченной.
async fn release_claim(pool: &PgPool, row: &CommandRow) {
    let id = row.id;
    let attempt = row.dispatch_attempt;
    let conversation_id = row.conversation_id;
    let lock_queue = matches!(row.kind.as_str(), "deliver_input" | "steer_input");
    let result: Result<(), sqlx::Error> = crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            if lock_queue && let Some(conversation_id) = conversation_id {
                sqlx::query(
                    "select conversation_id from queue_state where conversation_id = $1 for update",
                )
                .bind(conversation_id)
                .execute(&mut *conn)
                .await?;
            }
            sqlx::query(
                "update commands set dispatch_claimed_at = null
                 where id = $1 and dispatch_attempt = $2 and delivery_state = 'pending'",
            )
            .bind(id)
            .bind(attempt)
            .execute(&mut *conn)
            .await?;
            Ok(())
        })
    })
    .await;
    if result.is_err() {
        eprintln!("outbox_database_error");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandOff {
    Sent,
    Full,
    Closed,
}

/// Отдаёт захваченную команду транспорту. При отказе канала захват снимается,
/// только если его поставил этот же вызов `claim_for_send` (`release`):
/// команда не ушла и её можно отменить.
pub async fn hand_off(
    pool: &PgPool,
    row: &CommandRow,
    command: Command,
    sender: &mpsc::Sender<ControlDown>,
    release: bool,
) -> HandOff {
    match sender.try_send(envelope(command)) {
        Ok(()) => HandOff::Sent,
        Err(mpsc::error::TrySendError::Closed(_)) => {
            if release {
                release_claim(pool, row).await;
            }
            HandOff::Closed
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            if release {
                release_claim(pool, row).await;
            }
            HandOff::Full
        }
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
    let restarts = Arc::new(Mutex::new(HashMap::new()));
    let context = SenderContext {
        pool: pool.clone(),
        registry,
        timers: timers.clone(),
        shutdown: shutdown.clone(),
        restarts,
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
    let mut listener = PgListener::connect_with(&crate::db::listener_pool(pool)).await?;
    listener.listen("canterlot_outbox").await?;
    Ok(listener)
}

type RestartKey = (Uuid, Uuid, i32);

#[derive(Clone)]
struct SenderContext {
    pool: PgPool,
    registry: Registry,
    timers: HubTimers,
    shutdown: watch::Receiver<bool>,
    /// Сколько раз хабу пришлось перезапускать сеанс машины из-за одной
    /// попытки команды. Живёт дольше задачи машины: иначе повтор после
    /// переподключения выглядел бы первым, а новая попытка — повтором.
    restarts: Arc<Mutex<HashMap<RestartKey, u32>>>,
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
        restarts,
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
        restarts
            .lock()
            .expect("restart counter lock")
            .retain(|(owner, id, attempt), _| {
                *owner != machine
                    || rows
                        .iter()
                        .any(|row| row.id == *id && row.dispatch_attempt == *attempt)
            });
        let Some(sender) = current else {
            continue;
        };
        for row in rows {
            if let Some(item) = sent.get(&row.id) {
                if item.deadline > Instant::now() {
                    continue;
                }
                if item.resent {
                    let restarted = {
                        let mut counts = restarts.lock().expect("restart counter lock");
                        let count = counts
                            .entry((machine, row.id, row.dispatch_attempt))
                            .or_insert(0);
                        *count += 1;
                        *count
                    };
                    // Первый обрыв квитанции — транзиент: медленный, но живой
                    // воркер; рестарт сеанса уже восстановил доставку, поэтому
                    // не будим страж выката. Повтор той же попытки после
                    // рестарта — признак застрявшей доставки.
                    if restarted > 1 {
                        tracing::warn!(
                            kind = "outbox_restart",
                            command_id = %row.id,
                            machine_id = %machine,
                            restarts = restarted,
                            "command receipt timeout repeats"
                        );
                    } else {
                        tracing::info!(
                            kind = "outbox_restart",
                            command_id = %row.id,
                            machine_id = %machine,
                            "command receipt timeout"
                        );
                    }
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
            // Запуск ждёт применения отчёта сверки этого сеанса: отчёт снят воркером до
            // запуска, и сверка, увидев исполнение в `starting` без наблюдения, сочла бы
            // живой процесс потерянным (`owner_lost`). Применённая сверка будит outbox.
            if row.kind == "start_execution" && !registry.is_reconciled(machine) {
                continue;
            }
            let (row, fresh) = match claim_for_send(&pool, &row).await {
                Ok(Some(claim)) => (claim.row, claim.newly_claimed),
                Ok(None) => continue,
                Err(_) => {
                    eprintln!("outbox_database_error");
                    continue;
                }
            };
            let command = match to_proto_for_connection(&row, &registry, connection) {
                Ok(command) => command,
                Err(OutboxError::SessionChanged) => {
                    // Команда не попала в транспорт: захват не должен держать отмену.
                    if fresh {
                        release_claim(&pool, &row).await;
                    }
                    continue;
                }
                Err(OutboxError::CapabilityMissing) => {
                    // Сеанс команду не поймёт: она не должна висеть вечно.
                    tokio::select! {
                        biased;
                        _ = wait_for_shutdown(&mut shutdown) => return,
                        result = refuse_missing_capability(&pool, &row, fresh) => if result.is_err() { eprintln!("outbox_rejection_error"); },
                    }
                    continue;
                }
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
            match hand_off(&pool, &row, command, &sender, fresh).await {
                HandOff::Sent => {
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
                HandOff::Closed => break,
                HandOff::Full => {
                    if let Some(item) = sent.get_mut(&row.id) {
                        item.deadline = Instant::now() + timers.dispatch_sweep;
                    }
                    break;
                }
            }
        }
    }
}

/// Команда, которую сеанс не поймёт. Не уходившая с хаба (`fresh`) завершается
/// отказом `capability_missing`, как если бы его прислала машина. Ввод или
/// вливание, которые уже уходили прошлому сеансу, могли дойти до агента:
/// приём считается неподтверждённым, решает владелец, повтора нет. Прочие
/// команды прошлого сеанса ждут его итога.
async fn refuse_missing_capability(
    pool: &PgPool,
    row: &CommandRow,
    fresh: bool,
) -> Result<(), crate::ingest::IngestError> {
    let (kind, id, attempt, machine_id) = (
        row.kind.clone(),
        row.id,
        row.dispatch_attempt,
        row.machine_id,
    );
    if !fresh && !matches!(kind.as_str(), "deliver_input" | "steer_input") {
        return Ok(());
    }
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            if fresh {
                crate::ingest::apply_command_rejected_in_tx(
                    &mut *conn,
                    machine_id,
                    id,
                    attempt,
                    "capability_missing",
                    Uuid::now_v7(),
                )
                .await
            } else {
                apply_input_receipt_in_tx(
                    &mut *conn,
                    id,
                    attempt,
                    InputReceipt::AcceptanceUndetermined,
                    Uuid::now_v7(),
                )
                .await?;
                Ok(())
            }
        })
    })
    .await
}

async fn reject(pool: &PgPool, row: &CommandRow, reason: &str) -> Result<(), sqlx::Error> {
    let (kind, id, attempt) = (row.kind.clone(), row.id, row.dispatch_attempt);
    let reason = reason.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            if kind == "deliver_decision" {
                return crate::requests::fail_decision_command_in_tx(
                    &mut *conn,
                    id,
                    attempt,
                    &reason,
                    Uuid::now_v7(),
                )
                .await;
            }
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
