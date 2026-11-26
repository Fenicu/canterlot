use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::LockedQueue;
use crate::{
    events::{self, NewEvent, Scope},
    executions,
    model::{ExecutionState, HoldReason, PauseReason, RunSlot},
    session::CAPABILITY_INSTRUCTIONS_V1,
};

#[derive(Debug, Clone)]
pub struct GateInputs {
    pub machine_available: bool,
    pub machine_reconciled: bool,
    pub session_capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    Closed,
    Paused(Vec<PauseReason>),
    Held(Vec<HoldReason>),
    SlotBusy,
    NoLiveRunningExecution,
    MachineUnavailable,
    MachineNotReconciled,
    CapabilityMissing { input_id: Uuid },
    SteerInFlight { input_id: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    Dispatched {
        input_id: Uuid,
        command_id: Uuid,
        attempt: i32,
        machine_id: Uuid,
        execution_id: Uuid,
        generation: i64,
    },
    Blocked(Vec<BlockReason>),
    Empty,
}

/// Флаг резерва читается без блокировки: между commit наблюдения и правилом ход не начнётся.
async fn derived_reserve(
    conn: &mut sqlx::PgConnection,
    conversation_id: Uuid,
) -> Result<Option<HoldReason>, sqlx::Error> {
    let (harness, machine_id, overridden): (String, Uuid, bool) = sqlx::query_as(
        "select harness, machine_id, coalesce(reserve_override_until > clock_timestamp(), false) from conversations where id = $1",
    )
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    let Some(provider) = canterlot_proto::harness::provider_of(&harness).filter(|_| !overridden)
    else {
        return Ok(None);
    };
    // Резерв считается по учётной записи харнесса на машине разговора: чужая подписка
    // этот разговор не держит.
    let account = crate::limits::harness_account(&mut *conn, machine_id, &harness).await?;
    let reserve: Option<Option<chrono::DateTime<chrono::Utc>>> = sqlx::query_scalar(
        "select coalesce(weekly_reserve_resets_at, weekly_resets_at) from provider_limits
         where provider = $1 and account = $2 and weekly_reserve_active",
    )
    .bind(provider)
    .bind(&account)
    .fetch_optional(conn)
    .await?;
    Ok(reserve.map(|resets_at| HoldReason::WeeklyReserve {
        provider: provider.into(),
        account,
        resets_at,
    }))
}

pub async fn dispatch_next(
    pool: &PgPool,
    conversation_id: Uuid,
    gate: GateInputs,
    correlation_id: Uuid,
) -> Result<DispatchOutcome, sqlx::Error> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
    let mut queue: LockedQueue =
        sqlx::query_as("select * from queue_state where conversation_id = $1 for update")
            .bind(conversation_id)
            .fetch_one(&mut *conn)
            .await?;
    let slot = queue.slot()?;
    let live = executions::live(&mut *conn, conversation_id).await?;
    let mut reasons = Vec::new();
    if queue.closed {
        reasons.push(BlockReason::Closed);
    }
    if !queue.pause_reasons.0.is_empty() {
        reasons.push(BlockReason::Paused(queue.pause_reasons.0.clone()));
    }
    let open_requests_count: i64 = sqlx::query_scalar(crate::requests::LIVE_OPEN_REQUESTS)
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await?;

    let is_awaiting_phase = sqlx::query_scalar::<_, Option<bool>>(
        "select phase in ('awaiting_permission', 'awaiting_answer') from executions where conversation_id = $1 and state = 'running' order by generation desc limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?
    .flatten()
    .unwrap_or(false);

    if open_requests_count > 0 || is_awaiting_phase {
        if !queue
            .hold_reasons
            .0
            .contains(&HoldReason::AwaitingUserRequest)
        {
            queue.hold_reasons.0.push(HoldReason::AwaitingUserRequest);
            queue
                .save(&mut *conn, &slot, live.map(|(id, _, _)| id), correlation_id)
                .await?;
        }
    } else if queue
        .hold_reasons
        .0
        .contains(&HoldReason::AwaitingUserRequest)
    {
        queue
            .hold_reasons
            .0
            .retain(|r| *r != HoldReason::AwaitingUserRequest);
        queue
            .save(&mut *conn, &slot, live.map(|(id, _, _)| id), correlation_id)
            .await?;
    }
    let mut holds: Vec<_> = queue
        .hold_reasons
        .0
        .iter()
        .filter(|reason| {
            !matches!(
                reason,
                HoldReason::RunInProgress
                    | HoldReason::WorkdirBusy { .. }
                    | HoldReason::AttachmentsPending { .. }
            )
        })
        .cloned()
        .collect();
    if let Some(reserve) = derived_reserve(&mut *conn, conversation_id).await?
        && !holds.iter().any(|h| h.same_reserve(&reserve))
    {
        holds.push(reserve);
    }
    if !holds.is_empty() {
        reasons.push(BlockReason::Held(holds));
    }
    if slot != RunSlot::Free {
        reasons.push(BlockReason::SlotBusy);
    }
    if !matches!(live, Some((_, _, ExecutionState::Running))) {
        reasons.push(BlockReason::NoLiveRunningExecution);
    }
    if !gate.machine_available {
        reasons.push(BlockReason::MachineUnavailable);
    } else if !gate.machine_reconciled {
        reasons.push(BlockReason::MachineNotReconciled);
    }
    if !reasons.is_empty() {
        return Ok(DispatchOutcome::Blocked(reasons));
    }
    // Голова с вливанием в пути ждёт его итога: позже поставленный ввод не
    // обгоняет её, а обычная выдача не дублирует вливание.
    let head: Option<(Uuid, Uuid, bool)> = sqlx::query_as(
        "select id, command_id, exists (select 1 from commands where kind = 'steer_input'
                         and target_command_id = inputs.command_id
                         and delivery_state <> 'completed')
         from inputs where conversation_id = $1 and state = 'queued'
         order by input_seq limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((input_id, command_id, steering)) = head else {
        return Ok(DispatchOutcome::Empty);
    };
    if steering {
        return Ok(DispatchOutcome::Blocked(vec![BlockReason::SteerInFlight {
            input_id,
        }]));
    }
    let (mut pending, rev) =
        crate::conversations::revalidate_pending_in_tx(&mut *conn, conversation_id, correlation_id)
            .await?;
    // Снимок инструкций рядом со ссылкой: текст текущей версии пресета.
    if let Some(profile) = pending
        .get("instruction_profile")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
    {
        let owner_id: uuid::Uuid =
            sqlx::query_scalar("select owner_id from conversations where id=$1")
                .bind(conversation_id)
                .fetch_one(&mut *conn)
                .await?;
        let resolved = match crate::presets::PresetRef::parse(&profile) {
            Ok(reference) => {
                match crate::presets::resolve_in_tx(&mut *conn, owner_id, &reference, None).await {
                    Ok(resolved) => Some(resolved),
                    Err(crate::presets::PresetError::Db(error)) => return Err(error),
                    Err(_) => None,
                }
            }
            Err(_) => None,
        };
        if let Some(resolved) = resolved {
            pending["instructions"] = serde_json::json!({
                "mode": resolved.mode,
                "preset_id": resolved.preset_id.map(|id| id.to_string()).unwrap_or_default(),
                "revision": resolved.revision.unwrap_or(0),
                "title": resolved.title,
                "text": resolved.text.unwrap_or_default(),
            });
        } else {
            // Пресет пропал после повторной проверки: ссылка отбрасывается так же, как
            // в ней, а ввод уходит без смены инструкций вместо вечного простоя очереди.
            if let Some(settings) = pending.as_object_mut() {
                settings.remove("instruction_profile");
            }
            crate::conversations::reject_pending_in_tx(
                &mut *conn,
                owner_id,
                conversation_id,
                &pending,
                &["instruction_profile"],
                correlation_id,
            )
            .await?;
        }
    }
    // Ссылки на вложения и настройки понимает только сеанс с `pilot_v1`.
    let carries_refs: bool = sqlx::query_scalar(
        "select jsonb_array_length(coalesce(payload->'attachment_ids','[]'::jsonb)) > 0 from commands where id = $1",
    )
    .bind(command_id)
    .fetch_one(&mut *conn)
    .await?;
    if (carries_refs || pending.as_object().is_some_and(|v| !v.is_empty()))
        && !gate.session_capabilities.iter().any(|v| v == "pilot_v1")
    {
        return Ok(DispatchOutcome::Blocked(vec![
            BlockReason::CapabilityMissing { input_id },
        ]));
    }
    // Не-владельческий пресет, включая «Без инструкций», понимает только сеанс с instructions_v1.
    if pending
        .get("instruction_profile")
        .and_then(Value::as_str)
        .is_some_and(|profile| profile != "owner")
        && !gate
            .session_capabilities
            .iter()
            .any(|v| v == CAPABILITY_INSTRUCTIONS_V1)
    {
        return Ok(DispatchOutcome::Blocked(vec![
            BlockReason::CapabilityMissing { input_id },
        ]));
    }
    sqlx::query("update commands set payload=case when $2::jsonb='{}'::jsonb then payload-'settings'-'settings_rev' else (payload-'settings'-'settings_rev') || jsonb_build_object('settings',$2::jsonb,'settings_rev',$3::bigint) end where id=$1")
        .bind(command_id).bind(&pending).bind(rev).execute(&mut *conn).await?;
    // Сводка о родителе прикладывается к первой выдаче ввода и остаётся в команде:
    // повтор после обрыва отправит тот же текст, а не соберёт новую картину.
    let side: Option<Uuid> = sqlx::query_scalar(
        "select side_of from conversations where id=$1 and side_of is not null",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    if side.is_some() {
        let missing: bool = sqlx::query_scalar("select not(payload ? 'side_preface') from commands where id=$1")
            .bind(command_id)
            .fetch_one(&mut *conn)
            .await?;
        if missing
            && let Some(text) = crate::side::compose_preface(&mut *conn, conversation_id).await?
        {
            sqlx::query("update commands set payload = payload || jsonb_build_object('side_preface',$2) where id=$1")
                .bind(command_id)
                .bind(text)
                .execute(&mut *conn)
                .await?;
        }
    }
    // Напоминания складываются заметками прямо перед сборкой приставки и едут с этим вводом;
    // откат выдачи откатывает и их, и отметку о том, что они сказаны.
    crate::reminders::prepare_in_tx(&mut *conn, conversation_id, correlation_id).await?;
    let noted = with_service_notes(&mut *conn, conversation_id, input_id).await?;
    if !noted.notes.is_empty() {
        sqlx::query(
            "update commands set payload=jsonb_set(payload,'{content}',$2::jsonb) where id=$1",
        )
        .bind(command_id)
        .bind(&noted.content)
        .execute(&mut *conn)
        .await?;
    }
    let (execution_id, generation, _) = live.ok_or(sqlx::Error::RowNotFound)?;
    let (attempt, machine_id): (i32, Uuid) = sqlx::query_as(
        "update commands set delivery_state = 'pending', dispatch_attempt = dispatch_attempt + 1,
                execution_id = $2, execution_generation = $3, last_status = null,
                last_status_rank = 0, result = null, stored_at = null, machine_stored_at_ms = null,
                completed_at = null, dispatch_claimed_at = null
         where id = $1 returning dispatch_attempt, machine_id",
    )
    .bind(command_id)
    .bind(execution_id)
    .bind(generation)
    .fetch_one(&mut *conn)
    .await?;
    let reset: Vec<Uuid> = sqlx::query_scalar(
        "update input_attachments set machine_state='pending', machine_reason=null,
                retryable=null, updated_at=clock_timestamp()
         where input_id=$1 and (machine_state,machine_reason,retryable)
             is distinct from ('pending',null,null) returning attachment_id",
    )
    .bind(input_id)
    .fetch_all(&mut *conn)
    .await?;
    for attachment_id in reset {
        events::append(
            &mut *conn,
            NewEvent {
                scope: Scope::Conversation {
                    id: conversation_id,
                },
                kind: "attachment_state_changed",
                conversation_id: Some(conversation_id),
                execution_id: Some(execution_id),
                payload: json!({"input_id":input_id,"attachment_id":attachment_id,
                    "machine_state":"pending","machine_reason":null}),
                correlation_id,
            },
        )
        .await?;
    }
    queue
        .save(
            &mut *conn,
            &RunSlot::Dispatching { input_id, attempt },
            Some(execution_id),
            correlation_id,
        )
        .await?;
    sqlx::query("update inputs set state = 'dispatched' where id = $1")
        .bind(input_id)
        .execute(&mut *conn)
        .await?;
    events::append(
        &mut *conn,
        NewEvent {
            scope: Scope::Conversation {
                id: conversation_id,
            },
            kind: "input_state_changed",
            conversation_id: Some(conversation_id),
            execution_id: Some(execution_id),
            payload: json!({"input_id": input_id, "state": "dispatched", "attempt": attempt}),
            correlation_id,
        },
    )
    .await?;
    sqlx::query("select pg_notify('canterlot_outbox', $1::text)")
        .bind(machine_id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(DispatchOutcome::Dispatched {
        input_id,
        command_id,
        attempt,
        machine_id,
        execution_id,
        generation,
    })
        })
    })
    .await
}

/// Текст ввода для агента и заметки, которые в нём едут.
#[derive(Debug, Clone)]
pub(crate) struct NotedContent {
    pub content: Value,
    /// Заметки в приставке; пусто — `content` равен собственному тексту ввода.
    pub notes: Vec<Uuid>,
}

/// Служебные строки, накопившиеся до этого ввода, идут агенту одним коротким блоком
/// перед текстом владельца или событием. Сами они хода не начинали; приставка
/// собирается здесь, при каждой выдаче и каждом вливании («Отправить сейчас»), заново:
/// из заметок и собственного текста ввода, и заметки привязываются к вводу.
/// Выдача может не состояться (отзыв, отложенная выдача, отказ вливания), и ввод
/// вернётся в очередь с уже привязанными заметками. Поэтому берутся и свободные заметки,
/// и привязанные к этому вводу (правка между выдачами заменяет текст команды и стёрла бы
/// приставку), и привязанные к вводу, который до агента так и не дошёл: отменён или
/// отклонён. Карточка «Ход завис» (`turn_hung_notice`) адресована только владельцу: она
/// остаётся в ленте и к агенту не идёт никогда.
pub(crate) async fn with_service_notes(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    input_id: Uuid,
) -> Result<NotedContent, sqlx::Error> {
    // Основа — текст самого ввода, а не прежнее содержимое команды: в команде после
    // несостоявшейся выдачи уже стоит приставка, и вторая легла бы поверх первой.
    let own: Value = sqlx::query_scalar("select content from inputs where id=$1")
        .bind(input_id)
        .fetch_one(&mut *conn)
        .await?;
    let plain = |own: Value| NotedContent {
        content: own,
        notes: Vec::new(),
    };
    let notes: Vec<(Uuid, Value)> = sqlx::query_as(
        "select n.id,n.content from inputs n
         where n.conversation_id=$1 and n.service_note
           and (n.note_attached_to is null
                or n.note_attached_to=$2
                or exists(select 1 from inputs carrier where carrier.id=n.note_attached_to
                          and carrier.state in ('cancelled','rejected')))
           and coalesce(n.origin#>>'{event,turn_hung_notice}','') <> 'true'
         order by n.input_seq for update of n",
    )
    .bind(conversation_id)
    .bind(input_id)
    .fetch_all(&mut *conn)
    .await?;
    if notes.is_empty() {
        return Ok(plain(own));
    }
    let text = notes
        .iter()
        .filter_map(|(_, content)| {
            content
                .as_array()
                .and_then(|blocks| blocks.first())
                .and_then(|block| block.get("text"))
                .and_then(Value::as_str)
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.is_empty() {
        return Ok(plain(own));
    }
    let mut content = own.as_array().cloned().unwrap_or_default();
    content.insert(0, json!({"type":"text","text":text}));
    let ids: Vec<Uuid> = notes.iter().map(|(id, _)| *id).collect();
    sqlx::query(
        "update inputs set note_attached_to=$2
         where id=any($1) and note_attached_to is distinct from $2",
    )
    .bind(&ids)
    .bind(input_id)
    .execute(&mut *conn)
    .await?;
    Ok(NotedContent {
        content: json!(content),
        notes: ids,
    })
}
