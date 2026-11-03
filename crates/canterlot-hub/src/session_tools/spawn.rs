//! `session_spawn`: новая сессия из белого списка или карточка владельцу (D14).
//!
//! Разрешённый запуск выполняют два задания: `spawn` создаёт сессию, держа только её очередь,
//! `spawn_close` закрывает запрос под очередью родителя. Так две очереди не блокируются вместе.

use std::fmt;

use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

use super::{
    Caller, ToolContext, ToolError, ToolErrorCode, access, origin, parse_arguments, send, subs,
    whitelist,
};
use crate::{
    config::LimitsConfig,
    conversations::{
        self, CreateError, CrunchSetting, NewConversation, ParentLink, WorktreeSource,
    },
    crunch::JobOutcome,
    delivery::{
        ContentBlock, SubmitError, SubmitInput, submit_forwarded_message_input_in_tx,
        submit_input_in_tx,
    },
    events::{self, NewEvent, Scope},
    presets::PresetRef,
    requests::{self, RequestKind, RequestState},
    scheduler::ClaimedJob,
    session::{CAPABILITY_SESSION_TOOLS_V1, Registry},
};

const MAX_BRIEF_BYTES: usize = 64 * 1024;
const MAX_TITLE_CHARS: usize = 256;
const MAX_BASE_BYTES: usize = 256;
const DEFAULT_PRESET: &str = "clean";
const DEFAULT_EFFORT: &str = "medium";
const DEFAULT_BASE: &str = "HEAD";
const OWNER_MODE: &str = "owner";
const MODES: [&str; 4] = ["read_only", "ask", "bypass", OWNER_MODE];
const REASON_APPLIED: &str = "applied";
const WIDEST_RANK: u8 = 2;

const HINT_STARTED: &str = "Session created. You are subscribed to its completion; the summary arrives in your queue. Do not poll.";
const HINT_NOT_SUBSCRIBED: &str =
    "Session created. You are not subscribed: no [canterlot] message will tell you the result.";
const HINT_PENDING: &str = "This harness or model is outside the owner's list. The owner decides in the app; the outcome arrives in your queue. Carry on with other work.";

fn subscribe_by_default() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    harness: String,
    brief: String,
    model: Option<String>,
    effort: Option<String>,
    title: Option<String>,
    workdir: Option<String>,
    #[serde(default)]
    isolated: bool,
    base: Option<String>,
    preset: Option<String>,
    permission_mode: Option<String>,
    #[serde(default = "subscribe_by_default")]
    subscribe: bool,
    #[serde(default)]
    attachment_ids: Vec<Uuid>,
}

/// Проверенный запуск с уже выбранным режимом: `request_payload` карточки.
#[derive(Serialize, Deserialize)]
pub struct SpawnSpec {
    harness: String,
    model: String,
    effort: String,
    title: Option<String>,
    workdir: String,
    isolated: bool,
    base: Option<String>,
    preset: String,
    permission_mode: String,
    brief: String,
    subscribe: bool,
    #[serde(default)]
    attachment_ids: Vec<Uuid>,
    /// Глубина хода родителя: с неё подписка и бриф считают цепочку.
    chain_depth: i32,
    /// Режим у CLI потомка сильнее, чем у родителя: запуск только через карточку.
    #[serde(default)]
    wider_than_parent: bool,
}

impl fmt::Debug for SpawnSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpawnSpec")
            .field("harness", &self.harness)
            .field("model", &self.model)
            .field("isolated", &self.isolated)
            .field("permission_mode", &self.permission_mode)
            .field("brief_bytes", &self.brief.len())
            .finish_non_exhaustive()
    }
}

fn rank(mode: &str) -> Option<u8> {
    match mode {
        "read_only" => Some(0),
        "ask" => Some(1),
        "bypass" => Some(WIDEST_RANK),
        _ => None,
    }
}

/// Сила режима у конкретного CLI: что агент делает без вопроса владельцу.
/// У Codex `ask` идёт без песочницы, и команды он почти никогда не спрашивает
/// (`approvalPolicy: on-request` при `dangerFullAccess`), поэтому он равен `bypass`.
/// `owner` неизвестен заранее: сравнивается как `ask`.
fn power(harness: &str, mode: &str) -> u8 {
    match (harness, rank(mode)) {
        ("codex", Some(1)) => WIDEST_RANK,
        (_, Some(rank)) => rank,
        (_, None) => 1,
    }
}

/// Более узкий из действующего и ожидающего (D11). `owner` с рангами не сравним:
/// ограниченный режим считается уже него, `bypass` ничего не сужает.
fn parent_mode<'a>(effective: &'a str, pending: Option<&'a str>) -> &'a str {
    let effective = if effective.is_empty() {
        OWNER_MODE
    } else {
        effective
    };
    let Some(pending) = pending.filter(|mode| !mode.is_empty()) else {
        return effective;
    };
    match (rank(effective), rank(pending)) {
        (Some(a), Some(b)) if b < a => pending,
        (Some(_), Some(_)) => effective,
        (None, Some(b)) if b < WIDEST_RANK => pending,
        (Some(WIDEST_RANK), None) => pending,
        _ => effective,
    }
}

struct ChildMode {
    mode: String,
    /// Режим по названию не шире родителя, но у CLI потомка сильнее: нужна карточка.
    wider_than_parent: bool,
}

/// Режим потомка: по названию не шире родителя, по силе у его CLI не сильнее без карточки.
/// `owner` значит разное у разных CLI, поэтому через границу харнесса он становится `ask`.
/// Без явного режима берётся самый широкий из не сильнее родителя: Codex от родителя
/// в `ask` или `owner` получает `read_only`.
fn resolve_mode(
    parent_harness: &str,
    parent: &str,
    child_harness: &str,
    requested: Option<&str>,
) -> Result<ChildMode, ToolError> {
    let same_harness = parent_harness == child_harness;
    let ceiling = if parent == OWNER_MODE && !same_harness {
        "ask"
    } else {
        parent
    };
    let limit = power(parent_harness, parent);
    let Some(requested) = requested else {
        if ceiling == OWNER_MODE {
            return Ok(ChildMode {
                mode: OWNER_MODE.to_owned(),
                wider_than_parent: false,
            });
        }
        let ceiling_rank = rank(ceiling).unwrap_or(0);
        let mode = ["bypass", "ask", "read_only"]
            .into_iter()
            .find(|mode| {
                rank(mode).is_some_and(|rank| rank <= ceiling_rank)
                    && power(child_harness, mode) <= limit
            })
            .unwrap_or("read_only");
        return Ok(ChildMode {
            mode: mode.to_owned(),
            wider_than_parent: false,
        });
    };
    let fits = requested == ceiling
        || match (rank(requested), rank(ceiling)) {
            (Some(asked), Some(limit)) => asked <= limit,
            (Some(asked), None) => asked < WIDEST_RANK,
            _ => false,
        };
    if !fits {
        return Err(ToolError::new(ToolErrorCode::PermissionModeTooWide));
    }
    let wider_than_parent =
        !(same_harness && requested == parent) && power(child_harness, requested) > limit;
    Ok(ChildMode {
        mode: requested.to_owned(),
        wider_than_parent,
    })
}

fn invalid() -> ToolError {
    ToolError::new(ToolErrorCode::InvalidArguments)
}

fn check_arguments(args: &SpawnArgs) -> Result<(), ToolError> {
    if args.brief.len() > MAX_BRIEF_BYTES {
        return Err(ToolError::new(ToolErrorCode::TextTooLarge));
    }
    let bad_base = args.base.as_deref().is_some_and(|base| {
        !args.isolated
            || base.is_empty()
            || base.len() > MAX_BASE_BYTES
            || base.starts_with('-')
            || base.chars().any(char::is_control)
    });
    let bad_title = args
        .title
        .as_deref()
        .is_some_and(|title| title.contains('\0') || title.chars().count() > MAX_TITLE_CHARS);
    let bad_mode = args
        .permission_mode
        .as_deref()
        .is_some_and(|mode| !MODES.contains(&mode));
    if args.brief.trim().is_empty()
        || args.brief.contains('\0')
        || bad_base
        || bad_title
        || bad_mode
    {
        return Err(invalid());
    }
    Ok(())
}

fn create_error(error: CreateError) -> ToolError {
    let code = match error {
        CreateError::MachineNotFound | CreateError::MachineUnavailable => {
            ToolErrorCode::MachineUnavailable
        }
        CreateError::CapabilityMissing => ToolErrorCode::CapabilityMissing,
        CreateError::HarnessUnknown => ToolErrorCode::HarnessUnknown,
        CreateError::ModelUnknown => ToolErrorCode::ModelUnknown,
        CreateError::EffortNotAllowed => ToolErrorCode::EffortNotAllowed,
        CreateError::ProfileUnknown => ToolErrorCode::ProfileUnknown,
        CreateError::WorkdirNotAllowed => ToolErrorCode::WorkdirNotAllowed,
        CreateError::PermissionModeUnknown
        | CreateError::GroupNotFound
        | CreateError::Invalid(_)
        | CreateError::IdempotencyConflict => ToolErrorCode::InvalidArguments,
        CreateError::Db(error) => return error.into(),
    };
    ToolError::new(code)
}

/// Каталог харнесса на машине родителя.
struct Catalog {
    models: Vec<Value>,
    modes: Vec<Value>,
}

impl Catalog {
    async fn load(
        conn: &mut PgConnection,
        machine: Uuid,
        harness: &str,
    ) -> Result<Self, ToolError> {
        let row: Option<(Value, Value)> = sqlx::query_as(
            "select models,permission_modes from machine_harnesses
             where machine_id=$1 and harness=$2",
        )
        .bind(machine)
        .bind(harness)
        .fetch_optional(conn)
        .await?;
        let (models, modes) = row.ok_or_else(|| ToolError::new(ToolErrorCode::HarnessUnknown))?;
        Ok(Self {
            models: models.as_array().cloned().unwrap_or_default(),
            modes: modes.as_array().cloned().unwrap_or_default(),
        })
    }

    fn first_model(&self) -> Option<&str> {
        self.models.first().and_then(|model| model["id"].as_str())
    }

    fn resolve_model(&self, requested: Option<&str>) -> Result<&Value, ToolError> {
        let id = requested
            .filter(|model| *model != whitelist::DEFAULT_MODEL)
            .or(self.first_model());
        self.models
            .iter()
            .find(|model| id.is_some() && model["id"].as_str() == id)
            .ok_or_else(|| ToolError::new(ToolErrorCode::ModelUnknown))
    }

    fn has_mode(&self, mode: &str) -> bool {
        self.modes.iter().any(|known| known == mode)
    }
}

/// `medium`, если он есть у модели, иначе её первое усилие (D23).
fn resolve_effort(model: &Value, requested: Option<&str>) -> Result<String, ToolError> {
    let efforts: Vec<&str> = model["efforts"]
        .as_array()
        .map(|efforts| efforts.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let chosen = match requested {
        Some(effort) => efforts.iter().find(|known| **known == effort),
        None => efforts
            .iter()
            .find(|known| **known == DEFAULT_EFFORT)
            .or(efforts.first()),
    };
    chosen
        .map(|effort| (*effort).to_owned())
        .ok_or_else(|| ToolError::new(ToolErrorCode::EffortNotAllowed))
}

async fn resolve_preset(
    conn: &mut PgConnection,
    owner: Uuid,
    requested: Option<&str>,
) -> Result<String, ToolError> {
    let unknown = || ToolError::new(ToolErrorCode::ProfileUnknown);
    let reference = PresetRef::parse(requested.unwrap_or(DEFAULT_PRESET)).map_err(|_| unknown())?;
    // Пресет owner включает память владельца, а потомок всегда делегирован.
    if reference == PresetRef::Owner {
        return Err(ToolError::with_message(
            ToolErrorCode::ProfileUnknown,
            "The owner preset is not available to delegated sessions",
        ));
    }
    if let PresetRef::Custom(id) = &reference {
        let alive: bool = sqlx::query_scalar(
            "select exists(select 1 from instruction_presets
                           where owner_id=$1 and id=$2 and deleted_at is null)",
        )
        .bind(owner)
        .bind(id)
        .fetch_one(conn)
        .await?;
        if !alive {
            return Err(unknown());
        }
    }
    Ok(reference.as_str().to_owned())
}

/// Где работает сессия-родитель: сама в рабочей копии или в обычном каталоге.
struct ParentPlace {
    workdir: String,
    worktree: Option<Value>,
    allowed_roots: Value,
    worktree_root: Option<String>,
}

async fn parent_place(
    conn: &mut PgConnection,
    parent: Uuid,
    machine: Uuid,
) -> Result<ParentPlace, ToolError> {
    let (workdir, worktree, allowed_roots, worktree_root) = sqlx::query_as(
        "select c.workdir,c.worktree,m.allowed_roots,m.worktree_root
         from conversations c join machines m on m.id=c.machine_id
         where c.id=$1 and m.id=$2",
    )
    .bind(parent)
    .bind(machine)
    .fetch_one(conn)
    .await?;
    Ok(ParentPlace {
        workdir,
        worktree,
        allowed_roots,
        worktree_root,
    })
}

/// Рабочая копия родителя вне разрешённых корней: дочерняя копия строится от его исходного
/// репозитория и его ветки, а без изоляции каталог надо назвать явно.
fn resolve_workdir(
    args: &SpawnArgs,
    place: &ParentPlace,
) -> Result<(String, Option<String>), ToolError> {
    let source = place.worktree.as_ref().and_then(|worktree| {
        Some((
            worktree["source_workdir"].as_str()?,
            worktree["branch"].as_str()?,
        ))
    });
    let (workdir, parent_branch) = match (&args.workdir, source) {
        (Some(workdir), _) => (workdir.clone(), None),
        (None, None) => (place.workdir.clone(), None),
        (None, Some((source, branch))) if args.isolated => {
            (source.to_owned(), Some(branch.to_owned()))
        }
        (None, Some(_)) => {
            return Err(ToolError::with_message(
                ToolErrorCode::WorkdirNotAllowed,
                "This session works in a git worktree: pass isolated or name a workdir",
            ));
        }
    };
    if !conversations::workdir_allowed(&workdir, &place.allowed_roots) {
        return Err(ToolError::new(ToolErrorCode::WorkdirNotAllowed));
    }
    Ok((workdir, parent_branch))
}

fn check_isolation(
    registry: &Registry,
    machine: Uuid,
    place: &ParentPlace,
) -> Result<(), ToolError> {
    if place.worktree_root.is_none()
        || !registry.has_capability(machine, CAPABILITY_SESSION_TOOLS_V1)
    {
        return Err(ToolError::new(ToolErrorCode::CapabilityMissing));
    }
    Ok(())
}

async fn validate(
    conn: &mut PgConnection,
    context: &ToolContext,
    caller: &Caller,
    parent: &access::Session,
    args: SpawnArgs,
    chain_depth: i32,
) -> Result<(SpawnSpec, Catalog), ToolError> {
    let catalog = Catalog::load(conn, caller.machine_id, &args.harness).await?;
    let model = catalog.resolve_model(args.model.as_deref())?;
    let effort = resolve_effort(model, args.effort.as_deref())?;
    let model = model["id"].as_str().unwrap_or_default().to_owned();
    let ChildMode {
        mode,
        wider_than_parent,
    } = resolve_mode(
        &parent.harness,
        parent_mode(&parent.permission_mode, parent.pending_mode.as_deref()),
        &args.harness,
        args.permission_mode.as_deref(),
    )?;
    if !catalog.has_mode(&mode) {
        return Err(invalid());
    }
    let preset = resolve_preset(conn, caller.owner_id, args.preset.as_deref()).await?;
    let place = parent_place(conn, parent.id, caller.machine_id).await?;
    let (workdir, parent_branch) = resolve_workdir(&args, &place)?;
    if args.isolated {
        check_isolation(&context.registry, caller.machine_id, &place)?;
    }
    let base = args.isolated.then(|| args.base.or(parent_branch)).flatten();
    Ok((
        SpawnSpec {
            harness: args.harness,
            model,
            effort,
            title: args.title,
            workdir,
            isolated: args.isolated,
            base,
            preset,
            permission_mode: mode,
            brief: args.brief,
            subscribe: args.subscribe,
            attachment_ids: args.attachment_ids,
            chain_depth,
            wider_than_parent,
        },
        catalog,
    ))
}

struct Started {
    session_id: Uuid,
    title: String,
    worktree: Option<Value>,
}

/// Всё для создания сессии: и у инструмента, и у задания после решения владельца.
struct Launch<'a> {
    registry: &'a Registry,
    owner: Uuid,
    machine: Uuid,
    parent: &'a access::Session,
    spec: &'a SpawnSpec,
    key: String,
    correlation_id: Uuid,
}

async fn start_child(conn: &mut PgConnection, launch: Launch<'_>) -> Result<Started, ToolError> {
    let Launch {
        registry,
        owner,
        machine,
        parent,
        spec,
        key,
        correlation_id,
    } = launch;
    let brief = vec![ContentBlock::Text {
        text: spec.brief.clone(),
    }];
    let created = conversations::create_in_tx(
        conn,
        NewConversation {
            owner_id: owner,
            machine_id: machine,
            harness: spec.harness.clone(),
            workdir: spec.workdir.clone(),
            model: spec.model.clone(),
            effort: spec.effort.clone(),
            permission_mode: Some(spec.permission_mode.clone()),
            instruction_profile: Some(spec.preset.clone()),
            title: spec.title.clone(),
            color: None,
            group_ids: Vec::new(),
            crunch: CrunchSetting::default(),
            start: true,
            parent: Some(ParentLink {
                id: parent.id,
                spawn_depth: parent.spawn_depth + 1,
            }),
            worktree_source: spec.isolated.then(|| WorktreeSource {
                base: spec.base.clone().unwrap_or_else(|| DEFAULT_BASE.to_owned()),
            }),
        },
        LimitsConfig::default(),
        correlation_id,
        registry,
    )
    .await
    .map_err(create_error)?;
    let child = created.conversation_id;
    if conversations::auto_title_in_tx(conn, owner, child, &brief).await? {
        conversations::emit_attributes(conn, owner, child, correlation_id).await?;
    }
    let (title, worktree): (String, Option<Value>) =
        sqlx::query_as("select title,worktree from conversations where id=$1")
            .bind(child)
            .fetch_one(&mut *conn)
            .await?;
    let branch = worktree
        .as_ref()
        .and_then(|worktree| worktree["branch"].as_str());
    let from = parent.sender();
    let request = SubmitInput {
        owner_id: owner,
        conversation_id: child,
        idempotency_key: key,
        content: vec![ContentBlock::Text {
            text: origin::spawn_text(&from, branch, &spec.brief),
        }],
        received_at: Utc::now(),
        correlation_id,
        origin: Some(origin::spawn_origin(&from, spec.chain_depth + 1)),
    };
    if spec.attachment_ids.is_empty() {
        submit_input_in_tx(conn, &request).await
    } else {
        submit_forwarded_message_input_in_tx(conn, &request, &spec.attachment_ids, None, parent.id)
            .await
    }
    .map_err(send::submit_error)?;
    if spec.subscribe {
        subs::subscribe_in_tx(conn, owner, parent.id, child, spec.chain_depth).await?;
    }
    Ok(Started {
        session_id: child,
        title,
        worktree,
    })
}

fn started_answer(id: Uuid, mode: &str, worktree: Option<&Value>, subscribed: bool) -> Value {
    let mut answer = json!({
        "status": "started",
        "session_id": id,
        "permission_mode": mode,
        "subscribed": subscribed,
        "hint": if subscribed { HINT_STARTED } else { HINT_NOT_SUBSCRIBED },
    });
    if let Some(worktree) = worktree {
        answer["worktree"] = json!({"branch": worktree["branch"], "path": worktree["path"]});
    }
    answer
}

fn pending_answer(request_id: Uuid) -> Value {
    json!({"status": "pending_approval", "request_id": request_id, "hint": HINT_PENDING})
}

fn input_key(caller: &Caller) -> String {
    format!("agent:{}", caller.call_id)
}

fn native_request_id(caller: &Caller) -> String {
    format!("spawn:{}", caller.call_id)
}

/// Этот вызов уже прошёл: повтор транспорта получает тот же ответ, вторая сессия не создаётся.
async fn replayed(conn: &mut PgConnection, caller: &Caller) -> Result<Option<Value>, sqlx::Error> {
    let child: Option<(Uuid, String, Option<Value>, bool)> = sqlx::query_as(
        "select c.id,c.permission_mode,c.worktree,exists(
           select 1 from session_subscriptions s
           where s.subscriber_id=$3 and s.target_id=c.id and s.closed_at is null)
         from inputs i join conversations c on c.id=i.conversation_id
         where i.owner_id=$1 and i.idempotency_key=$2 and c.parent_conversation_id=$3",
    )
    .bind(caller.owner_id)
    .bind(input_key(caller))
    .bind(caller.conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((id, mode, worktree, subscribed)) = child {
        return Ok(Some(started_answer(
            id,
            &mode,
            worktree.as_ref(),
            subscribed,
        )));
    }
    let request: Option<Uuid> = sqlx::query_scalar(
        "select id from requests where conversation_id=$1 and native_request_id=$2",
    )
    .bind(caller.conversation_id)
    .bind(native_request_id(caller))
    .fetch_optional(conn)
    .await?;
    Ok(request.map(pending_answer))
}

/// Как `request_opened` из журнала, но без удержания очереди: ход родителя идёт дальше (D14).
async fn open_card(
    conn: &mut PgConnection,
    caller: &Caller,
    spec: &SpawnSpec,
) -> Result<Uuid, ToolError> {
    send::lock_queue(conn, caller.conversation_id).await?;
    // Под очередью: закрытие запросов закончившегося исполнения этот запрос не пропустит.
    let generation: Option<i64> = sqlx::query_scalar(
        "select generation from executions
         where id=$1 and conversation_id=$2 and state not in ('stopped','failed','owner_lost')",
    )
    .bind(caller.execution_id)
    .bind(caller.conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let generation = generation.ok_or_else(|| ToolError::new(ToolErrorCode::ConversationClosed))?;
    let id = Uuid::now_v7();
    let payload =
        serde_json::to_value(spec).map_err(|_| ToolError::new(ToolErrorCode::Internal))?;
    requests::create_request_in_tx(
        conn,
        &requests::NewRequest {
            id,
            conversation_id: caller.conversation_id,
            execution_id: caller.execution_id,
            generation,
            kind: RequestKind::SpawnApproval,
            native_request_id: native_request_id(caller),
            request_payload: payload.clone(),
        },
    )
    .await?;
    let event = events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: caller.conversation_id,
            },
            kind: "request_opened",
            conversation_id: Some(caller.conversation_id),
            execution_id: Some(caller.execution_id),
            payload: json!({
                "request_id": id,
                "kind": RequestKind::SpawnApproval.as_str(),
                "payload": payload,
            }),
            correlation_id: caller.call_id,
        },
    )
    .await?;
    sqlx::query("update requests set created_seq=$2 where id=$1")
        .bind(id)
        .bind(event.scope_seq)
        .execute(conn)
        .await?;
    Ok(id)
}

pub async fn spawn(
    context: &ToolContext,
    caller: &Caller,
    arguments: Value,
) -> Result<Value, ToolError> {
    let args: SpawnArgs = parse_arguments(arguments)?;
    check_arguments(&args)?;
    let mut tx = context.pool.begin().await?;
    send::lock_sender(&mut tx, caller.conversation_id).await?;
    if let Some(answer) = replayed(&mut tx, caller).await? {
        return Ok(answer);
    }
    // Владелец раньше строки разговора: тот же порядок у удаления и создания.
    conversations::lock_owner(&mut tx, caller.owner_id).await?;
    let parent = access::caller_in_tx(&mut tx, caller.owner_id, caller.conversation_id).await?;
    if parent.spawn_depth >= context.config.max_spawn_depth as i32 {
        return Err(ToolError::new(ToolErrorCode::SpawnDepthExceeded));
    }
    let depth = access::current_depth(&mut tx, parent.id).await?;
    access::check_depth(&context.config, depth + 1)?;
    access::check_spawn_rate(&mut tx, &context.config, parent.id, Utc::now()).await?;
    let (spec, catalog) = validate(&mut tx, context, caller, &parent, args, depth).await?;
    let list = whitelist::view(&mut tx, caller.owner_id).await?;
    let first_model = catalog.first_model().unwrap_or_default();
    if spec.wider_than_parent
        || !whitelist::allows(&list.entries, &spec.harness, &spec.model, first_model)
    {
        let request = open_card(&mut tx, caller, &spec).await?;
        tx.commit().await?;
        return Ok(pending_answer(request));
    }
    let started = start_child(
        &mut tx,
        Launch {
            registry: &context.registry,
            owner: caller.owner_id,
            machine: caller.machine_id,
            parent: &parent,
            spec: &spec,
            key: input_key(caller),
            correlation_id: caller.call_id,
        },
    )
    .await?;
    tx.commit().await?;
    if let Some(dispatcher) = &context.dispatcher {
        dispatcher.trigger(started.session_id);
    }
    Ok(started_answer(
        started.session_id,
        &spec.permission_mode,
        started.worktree.as_ref(),
        spec.subscribe,
    ))
}

/// Вместо команды машине: решение выполнит задание хаба.
pub(crate) async fn queue_decision_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    parent: Uuid,
    request: Uuid,
) -> Result<Uuid, sqlx::Error> {
    subs::insert_job(
        conn,
        owner,
        parent,
        Utc::now(),
        &json!({"action": "spawn", "request_id": request}),
    )
    .await
}

pub(super) fn note_job(text: String, origin: Value, event: origin::HubEvent) -> Value {
    json!({"action": "note", "text": text, "origin": origin::with_event(origin, event)})
}

/// Вызывается под очередью родителя; сообщение дойдёт отдельным заданием.
pub(crate) async fn queue_lapsed_note_in_tx(
    conn: &mut PgConnection,
    parent: Uuid,
    request: Uuid,
    payload: &Value,
) -> Result<(), sqlx::Error> {
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(parent)
        .fetch_one(&mut *conn)
        .await?;
    let depth = payload["chain_depth"].as_i64().unwrap_or(0) as i32;
    let text = format!(
        "[canterlot] Your session_spawn request {request} lapsed: your execution ended before the owner decided. Nothing was started. Call session_spawn again if you still need it."
    );
    subs::insert_job(
        conn,
        owner,
        parent,
        Utc::now(),
        &note_job(
            text,
            origin::note_origin(depth),
            origin::HubEvent::approval("lapsed", request, None, None),
        ),
    )
    .await?;
    Ok(())
}

#[derive(Deserialize)]
struct SpawnJob {
    request_id: Uuid,
}

/// Исход решения: закрытие запроса и сообщение родителю идут своими заданиями.
struct Resolution {
    state: RequestState,
    reason: String,
    text: String,
    origin: Value,
    event: origin::HubEvent,
}

fn denied(request: Uuid, depth: i32, message: Option<&str>) -> Resolution {
    let remark = message
        .filter(|text| !text.trim().is_empty())
        .map_or(String::new(), |text| format!(" The owner said: {text}"));
    Resolution {
        state: RequestState::DecisionApplied,
        reason: REASON_APPLIED.to_owned(),
        text: format!(
            "[canterlot] The owner denied your session_spawn request {request}. Nothing was started. Do not retry it with different arguments.{remark}"
        ),
        origin: origin::note_origin(depth),
        event: origin::HubEvent::approval("denied", request, None, None),
    }
}

fn failed(request: Uuid, depth: i32, code: &str) -> Resolution {
    Resolution {
        state: RequestState::DecisionNotApplied,
        reason: code.to_owned(),
        text: format!(
            "[canterlot] The owner allowed your session_spawn request {request}, but the session could not start: {code}."
        ),
        origin: origin::note_origin(depth),
        event: origin::HubEvent::approval("failed", request, None, Some(code)),
    }
}

fn started(request: Uuid, spec: &SpawnSpec, child: &Started) -> Resolution {
    let sender = origin::Sender {
        id: child.session_id,
        title: child.title.clone(),
        harness: spec.harness.clone(),
        model: spec.model.clone(),
    };
    let follow_up = if spec.subscribe {
        "You are subscribed to its completion; the summary arrives in your queue. Do not poll."
    } else {
        "You are not subscribed to it."
    };
    Resolution {
        state: RequestState::DecisionApplied,
        reason: REASON_APPLIED.to_owned(),
        text: format!(
            "[canterlot] The owner allowed your session_spawn request {request}. Session {} started ({}, model {}, permission mode {}). {follow_up}",
            child.session_id, spec.harness, spec.model, spec.permission_mode,
        ),
        origin: origin::started_note_origin(&sender, spec.chain_depth),
        event: origin::HubEvent::approval("allowed", request, Some(child.session_id), None),
    }
}

/// Разрешение проходит те же правила, кроме списка: за время ожидания родитель мог сузиться,
/// а каталог измениться. Отказ откатывает только эту попытку: закрыть запрос ещё нужно.
async fn allow(conn: &mut PgConnection, launch: Launch<'_>) -> Result<Started, ToolError> {
    let mode = resolve_mode(
        &launch.parent.harness,
        parent_mode(
            &launch.parent.permission_mode,
            launch.parent.pending_mode.as_deref(),
        ),
        &launch.spec.harness,
        Some(&launch.spec.permission_mode),
    )?;
    // Владелец одобрял карточку без пометки: стать сильнее родителя запуск не может.
    if mode.wider_than_parent && !launch.spec.wider_than_parent {
        return Err(ToolError::new(ToolErrorCode::PermissionModeTooWide));
    }
    let mut attempt = conn.begin().await?;
    match start_child(&mut attempt, launch).await {
        Ok(child) => {
            attempt.commit().await?;
            Ok(child)
        }
        Err(error) => {
            attempt.rollback().await?;
            Err(error)
        }
    }
}

/// Создаёт сессию, держа только её очередь. Родитель закрывается следующим заданием.
pub(crate) async fn run_job(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    parent_id: Uuid,
    registry: Option<&Registry>,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<JobOutcome, SubmitError> {
    let Ok(SpawnJob { request_id }) = serde_json::from_value(job.payload.clone()) else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    conversations::lock_owner(conn, job.owner_id).await?;
    let row: Option<(RequestState, Value, Option<Value>)> = sqlx::query_as(
        "select state,request_payload,decision_payload from requests
         where id=$1 and conversation_id=$2 and kind='spawn_approval'",
    )
    .bind(request_id)
    .bind(parent_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((RequestState::DecisionSaved, payload, decision)) = row else {
        return Ok(JobOutcome::Done(json!({"skipped": true})));
    };
    let Some(parent) = access::session_in_tx(conn, job.owner_id, parent_id).await? else {
        return Ok(JobOutcome::Done(json!({"skipped": true})));
    };
    let machine: Uuid = sqlx::query_scalar("select machine_id from conversations where id=$1")
        .bind(parent_id)
        .fetch_one(&mut *conn)
        .await?;
    let decision = decision.and_then(|value| serde_json::from_value(value).ok());
    let spec = serde_json::from_value::<SpawnSpec>(payload).ok();
    let depth = spec.as_ref().map_or(0, |spec| spec.chain_depth);
    let resolution = match (decision, spec) {
        (
            Some(DecisionPayload::Permission {
                behavior: PermissionBehavior::Allow,
                ..
            }),
            Some(spec),
        ) => {
            let outcome = match registry {
                Some(registry) => {
                    allow(
                        conn,
                        Launch {
                            registry,
                            owner: job.owner_id,
                            machine,
                            parent: &parent,
                            spec: &spec,
                            key: format!("spawn:{request_id}"),
                            correlation_id,
                        },
                    )
                    .await
                }
                None => Err(ToolError::new(ToolErrorCode::MachineUnavailable)),
            };
            match outcome {
                Ok(child) => started(request_id, &spec, &child),
                Err(error) => failed(request_id, depth, error.code.as_str()),
            }
        }
        (
            Some(DecisionPayload::Permission {
                behavior: PermissionBehavior::Deny,
                message,
            }),
            _,
        ) => denied(request_id, depth, message.as_deref()),
        _ => failed(request_id, depth, "invalid_payload"),
    };
    let close = json!({
        "action": "spawn_close",
        "request_id": request_id,
        "state": resolution.state.as_str(),
        "reason": resolution.reason,
    });
    subs::insert_job(conn, job.owner_id, parent_id, now, &close).await?;
    let note = note_job(resolution.text, resolution.origin, resolution.event);
    subs::insert_job(conn, job.owner_id, parent_id, now, &note).await?;
    Ok(JobOutcome::Done(json!({"request_id": request_id})))
}

#[derive(Deserialize)]
struct CloseJob {
    request_id: Uuid,
    state: String,
    reason: String,
}

/// Закрывает запрос в порядке хаба: очередь родителя, строка запроса, события.
pub(crate) async fn close_job(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    parent_id: Uuid,
    correlation_id: Uuid,
) -> Result<JobOutcome, SubmitError> {
    let Ok(close) = serde_json::from_value::<CloseJob>(job.payload.clone()) else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    let closed = close_request_in_tx(
        conn,
        parent_id,
        close.request_id,
        &close.state,
        &close.reason,
        correlation_id,
    )
    .await?;
    Ok(JobOutcome::Done(if closed {
        json!({"request_id": close.request_id})
    } else {
        json!({"skipped": true})
    }))
}

/// Только сохранённое решение закрывается: открытый запрос и закрытый другим путём не трогаются.
async fn close_request_in_tx(
    conn: &mut PgConnection,
    parent_id: Uuid,
    request_id: Uuid,
    state: &str,
    reason: &str,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    send::lock_queue(conn, parent_id).await?;
    let row: Option<(RequestState, Uuid)> = sqlx::query_as(
        "select state,execution_id from requests
         where id=$1 and conversation_id=$2 for update",
    )
    .bind(request_id)
    .bind(parent_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((RequestState::DecisionSaved, execution_id)) = row else {
        return Ok(false);
    };
    sqlx::query("update requests set state=$2,updated_at=clock_timestamp() where id=$1")
        .bind(request_id)
        .bind(state)
        .execute(&mut *conn)
        .await?;
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation { id: parent_id },
            kind: "request_closed",
            conversation_id: Some(parent_id),
            execution_id: Some(execution_id),
            payload: json!({"request_id": request_id, "reason": reason}),
            correlation_id,
        },
    )
    .await?;
    Ok(true)
}

const REASON_JOB_FAILED: &str = "job_failed";

/// Задание запуска исчерпало попытки: решение владельца не выполнено, и запрос не должен
/// висеть в `decision_saved`. Закрывается он здесь же, родитель получает сообщение.
pub(crate) async fn abandon_spawn_in_tx(
    conn: &mut PgConnection,
    parent_id: Uuid,
    payload: &Value,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Ok(SpawnJob { request_id }) = serde_json::from_value(payload.clone()) else {
        return Ok(());
    };
    let closed = close_request_in_tx(
        conn,
        parent_id,
        request_id,
        RequestState::DecisionNotApplied.as_str(),
        REASON_JOB_FAILED,
        correlation_id,
    )
    .await?;
    if !closed {
        return Ok(());
    }
    let depth: Option<i32> = sqlx::query_scalar(
        "select (request_payload->>'chain_depth')::int from requests where id=$1",
    )
    .bind(request_id)
    .fetch_one(&mut *conn)
    .await?;
    let owner: Uuid = sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(parent_id)
        .fetch_one(&mut *conn)
        .await?;
    let resolution = failed(request_id, depth.unwrap_or(0), REASON_JOB_FAILED);
    subs::insert_job(
        conn,
        owner,
        parent_id,
        now,
        &note_job(resolution.text, resolution.origin, resolution.event),
    )
    .await?;
    Ok(())
}

/// Закрывающее задание исчерпало попытки: итог уже выбран заданием запуска, закрытие тот же.
pub(crate) async fn abandon_close_in_tx(
    conn: &mut PgConnection,
    parent_id: Uuid,
    payload: &Value,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let Ok(close) = serde_json::from_value::<CloseJob>(payload.clone()) else {
        return Ok(());
    };
    close_request_in_tx(
        conn,
        parent_id,
        close.request_id,
        &close.state,
        &close.reason,
        correlation_id,
    )
    .await?;
    Ok(())
}
