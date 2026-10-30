//! Вызовы инструментов агента (план 2c, задача 4).
//!
//! Агент действует только на свою сессию: разговор и владелец выводятся
//! из живого исполнения и поколения на вызвавшей машине, а не из аргументов.
//! Текст пресетов — данные уровня сообщения: в журнал попадают только
//! идентификатор вызова, инструмент, исполнение и код итога.

use canterlot_proto::{limits::MAX_AGENT_CALL_BYTES, worker::v1::*};
use serde::Deserialize;
use sqlx::Row;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use super::{HubGrpc, auth};

/// Аргументы `presets.list`: без `id` — сводки, с `id` — пресет с текстом.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    id: Option<String>,
}

/// Аргументы `presets.save`: без `id` — создание, с `id` — правка
/// (`base_revision` обязателен).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveArgs {
    id: Option<String>,
    title: String,
    description: Option<String>,
    text: String,
    base_revision: Option<i64>,
}

/// Аргументы `presets.apply`: ссылка на пресет своей сессии.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyArgs {
    id: String,
}

fn ok(result: serde_json::Value) -> AgentCallResponse {
    AgentCallResponse {
        result_json: result.to_string(),
        error_code: String::new(),
        error_message: String::new(),
    }
}

fn err(code: &str, message: String) -> AgentCallResponse {
    AgentCallResponse {
        result_json: String::new(),
        error_code: code.to_owned(),
        error_message: message,
    }
}

fn preset_error(error: crate::presets::PresetError) -> Result<AgentCallResponse, Status> {
    use crate::presets::PresetError as E;
    Ok(match error {
        E::NotFound => err("preset_not_found", "Пресет не найден".into()),
        E::Builtin => err(
            "preset_builtin",
            "Встроенный пресет нельзя изменить или удалить".into(),
        ),
        E::RevisionConflict { current } => err(
            "preset_revision_conflict",
            format!("Пресет уже изменён (текущая версия {current})"),
        ),
        E::TitleTaken => err(
            "preset_title_taken",
            "Пресет с таким названием уже есть".into(),
        ),
        E::LimitReached => err("preset_limit_reached", "Слишком много пресетов".into()),
        E::Invalid { field } => err("preset_invalid", format!("Некорректный пресет ({field})")),
        E::IdempotencyConflict => err(
            "idempotency_conflict",
            "Ключ идемпотентности уже использован с другим запросом".into(),
        ),
        E::Db(_) => return Err(Status::internal("database")),
    })
}

/// Вызов инструмента агента: авторизация по машине, исполнение обязано быть
/// живым на вызвавшей машине; доменные ошибки — кодами в ответе, а не статусом.
pub async fn agent_call(
    grpc: &HubGrpc,
    mut request: Request<AgentCallRequest>,
) -> Result<Response<AgentCallResponse>, Status> {
    auth::intercept(&grpc.pool, &mut request).await?;
    let machine_id = request
        .extensions()
        .get::<auth::AuthenticatedMachine>()
        .expect("authenticated machine")
        .machine_id;
    let body = request.into_inner();
    let outcome = handle(grpc, machine_id, &body).await?;
    Ok(Response::new(outcome))
}

async fn handle(
    grpc: &HubGrpc,
    machine_id: Uuid,
    body: &AgentCallRequest,
) -> Result<AgentCallResponse, Status> {
    let call_id = match body.call_id.parse::<Uuid>() {
        Ok(id) => id,
        Err(_) => {
            tracing::info!(kind = "agent_call", tool = %body.tool, outcome = "invalid_arguments", "agent call rejected");
            return Ok(err("invalid_arguments", "Некорректный вызов".into()));
        }
    };
    if body.arguments_json.len() > MAX_AGENT_CALL_BYTES {
        tracing::info!(kind = "agent_call", tool = %body.tool, call_id = %call_id, outcome = "too_large", "agent call rejected");
        return Ok(err("too_large", "Превышен допустимый размер данных".into()));
    }
    let arguments: serde_json::Value =
        serde_json::from_str(&body.arguments_json).unwrap_or(serde_json::Value::Null);
    if !arguments.is_object() {
        tracing::info!(kind = "agent_call", tool = %body.tool, call_id = %call_id, outcome = "invalid_arguments", "agent call rejected");
        return Ok(err("invalid_arguments", "Некорректный вызов".into()));
    }
    let execution_id = body.execution_id.parse::<Uuid>().unwrap_or(Uuid::nil());
    let generation = i64::try_from(body.generation).unwrap_or(-1);
    let live: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select c.id, c.owner_id from executions e
         join conversations c on c.id = e.conversation_id
         where e.id = $1 and e.generation = $2
           and e.state not in ('stopped', 'failed') and c.machine_id = $3",
    )
    .bind(execution_id)
    .bind(generation)
    .bind(machine_id)
    .fetch_optional(&grpc.pool)
    .await
    .map_err(|_| Status::internal("database"))?;
    let Some((conversation_id, owner_id)) = live else {
        tracing::info!(kind = "agent_call", tool = %body.tool, call_id = %call_id, execution_id = %body.execution_id, outcome = "execution_not_live", "agent call rejected");
        return Ok(err(
            "execution_not_live",
            "Исполнение не запущено на этой машине".into(),
        ));
    };
    let response = match body.tool.as_str() {
        "presets.list" => list(&grpc.pool, owner_id, conversation_id, arguments).await?,
        "presets.save" => save(&grpc.pool, owner_id, conversation_id, &call_id, arguments).await?,
        "presets.apply" => {
            apply(&grpc.pool, owner_id, conversation_id, &call_id, arguments).await?
        }
        _ => err("unknown_tool", "Неизвестный инструмент".into()),
    };
    tracing::info!(kind = "agent_call", tool = %body.tool, call_id = %call_id, execution_id = %execution_id, outcome = if response.error_code.is_empty() { "ok" } else { response.error_code.as_str() }, "agent call answered");
    Ok(response)
}

async fn applied_and_pending(
    conn: &mut sqlx::PgConnection,
    owner: Uuid,
    conversation: Uuid,
) -> Result<(crate::presets::AppliedPreset, Option<String>), Status> {
    let row = sqlx::query(
        "select instruction_profile, instruction_preset_revision,
                pending_settings->>'instruction_profile' as pending
         from conversations where id = $1",
    )
    .bind(conversation)
    .fetch_one(&mut *conn)
    .await
    .map_err(|_| Status::internal("database"))?;
    let reference: String = row
        .try_get("instruction_profile")
        .map_err(|_| Status::internal("database"))?;
    let revision: Option<i64> = row
        .try_get("instruction_preset_revision")
        .map_err(|_| Status::internal("database"))?;
    let pending: Option<String> = row
        .try_get("pending")
        .map_err(|_| Status::internal("database"))?;
    let applied = crate::presets::applied_in_tx(&mut *conn, owner, &reference, revision)
        .await
        .map_err(|error| match error {
            crate::presets::PresetError::Db(_) => Status::internal("database"),
            _ => Status::internal("preset resolve failed"),
        })?;
    Ok((applied, pending))
}

async fn list(
    pool: &sqlx::PgPool,
    owner: Uuid,
    conversation: Uuid,
    arguments: serde_json::Value,
) -> Result<AgentCallResponse, Status> {
    let args: ListArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(_) => return Ok(err("invalid_arguments", "Некорректный вызов".into())),
    };
    let mut conn = pool
        .acquire()
        .await
        .map_err(|_| Status::internal("database"))?;
    let summaries = crate::presets::list(&mut conn, owner)
        .await
        .map_err(|_| Status::internal("database"))?;
    let (applied, pending) = applied_and_pending(&mut conn, owner, conversation).await?;
    if let Some(id) = args.id {
        let reference = match crate::presets::PresetRef::parse(&id) {
            Ok(reference) => reference,
            Err(_) => return Ok(err("preset_not_found", "Пресет не найден".into())),
        };
        let preset = match crate::presets::get(&mut conn, owner, &reference).await {
            Ok(preset) => preset,
            Err(error) => return preset_error(error),
        };
        return Ok(ok(serde_json::json!({
            "presets": summaries,
            "applied": applied,
            "pending": pending,
            "preset": preset,
        })));
    }
    Ok(ok(serde_json::json!({
        "presets": summaries,
        "applied": applied,
        "pending": pending,
    })))
}

async fn save(
    pool: &sqlx::PgPool,
    owner: Uuid,
    conversation: Uuid,
    call_id: &Uuid,
    arguments: serde_json::Value,
) -> Result<AgentCallResponse, Status> {
    let args: SaveArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(_) => return Ok(err("invalid_arguments", "Некорректный вызов".into())),
    };
    let key = format!("agent:{call_id}");
    let actor = crate::presets::Actor::Agent {
        conversation_id: conversation,
    };
    if let Some(id) = args.id {
        match crate::presets::PresetRef::parse(&id) {
            Ok(crate::presets::PresetRef::Custom(uuid)) => {
                let Some(base_revision) = args.base_revision else {
                    return Ok(err("invalid_arguments", "Некорректный вызов".into()));
                };
                let preset = match crate::presets::update(
                    pool,
                    owner,
                    uuid,
                    &key,
                    crate::presets::PresetPatch {
                        title: Some(args.title),
                        description: args.description,
                        text: Some(args.text),
                        base_revision,
                    },
                    actor,
                    *call_id,
                )
                .await
                {
                    Ok(preset) => preset,
                    Err(error) => return preset_error(error),
                };
                return Ok(ok(serde_json::json!({
                    "id": preset.summary.id,
                    "revision": preset.summary.revision,
                })));
            }
            _ => {
                return Ok(err(
                    "preset_builtin",
                    "Встроенный пресет нельзя изменить или удалить".into(),
                ));
            }
        }
    }
    if args.base_revision.is_some() {
        return Ok(err("invalid_arguments", "Некорректный вызов".into()));
    }
    let preset = match crate::presets::create(
        pool,
        owner,
        &key,
        crate::presets::NewPreset {
            title: args.title,
            description: args.description,
            text: args.text,
        },
        actor,
        *call_id,
    )
    .await
    {
        Ok(preset) => preset,
        Err(error) => return preset_error(error),
    };
    Ok(ok(serde_json::json!({
        "id": preset.summary.id,
        "revision": preset.summary.revision,
    })))
}

async fn apply(
    pool: &sqlx::PgPool,
    owner: Uuid,
    conversation: Uuid,
    call_id: &Uuid,
    arguments: serde_json::Value,
) -> Result<AgentCallResponse, Status> {
    let args: ApplyArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(_) => return Ok(err("invalid_arguments", "Некорректный вызов".into())),
    };
    // Ссылка проверяется сменой настроек; висячая смена видна в PWA.
    let key = format!("agent:{call_id}");
    if let Err(error) = crate::conversations::change_settings(
        pool,
        owner,
        conversation,
        &key,
        crate::conversations::SettingsPatch {
            model: None,
            effort: None,
            permission_mode: None,
            instruction_profile: Some(args.id.clone()),
        },
        *call_id,
    )
    .await
    {
        use crate::conversations::SettingsError as E;
        return Ok(match error {
            E::PresetUnknown => err("profile_unknown", "Пресет инструкций не найден".into()),
            E::MidsessionUnsupported => err(
                "settings_midsession_unsupported",
                "Смена настроек во время исполнения не поддерживается".into(),
            ),
            E::CapabilityMissing => {
                err("capability_missing", "Требуется обновление воркера".into())
            }
            E::IdempotencyConflict => err(
                "idempotency_conflict",
                "Ключ идемпотентности уже использован с другим запросом".into(),
            ),
            E::NotFound | E::ModelUnknown | E::EffortNotAllowed | E::PermissionModeUnknown => {
                return Err(Status::internal("settings apply failed"));
            }
            E::Db(_) => return Err(Status::internal("database")),
        });
    }
    let mut conn = pool
        .acquire()
        .await
        .map_err(|_| Status::internal("database"))?;
    let reference = match crate::presets::PresetRef::parse(&args.id) {
        Ok(reference) => reference,
        Err(_) => return Ok(err("profile_unknown", "Пресет инструкций не найден".into())),
    };
    let revision = match reference {
        crate::presets::PresetRef::Custom(id) => {
            let current: Option<i64> = sqlx::query_scalar(
                "select revision from instruction_presets
                 where owner_id = $1 and id = $2 and deleted_at is null",
            )
            .bind(owner)
            .bind(id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|_| Status::internal("database"))?;
            current
        }
        _ => None,
    };
    Ok(ok(serde_json::json!({
        "id": args.id,
        "revision": revision,
        "applies": "next_turn",
    })))
}
