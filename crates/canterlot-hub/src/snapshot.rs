pub use crate::http::limits::ProviderView as ProviderLimits;
use crate::http::machines::MachineView;
use crate::{
    conversations::{self, ConversationDetail, Counts, ListItem},
    events::{self, Scope},
    groups::{self, Group},
    history::{self, HistoryCursor, HistoryPage, InputView},
    model::{HoldView, PauseReason, RunSlot},
    presets::PresetSummary,
    scheduler::{self, ScheduledMessage},
    session::Registry,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Serialize, ToSchema)]
pub struct Snapshot<T> {
    pub scope: String,
    pub cursor: String,
    pub data: T,
}
#[derive(Serialize, ToSchema)]
pub struct ListSnapshot {
    pub items: Vec<ListItem>,
    pub truncated: bool,
    pub counts: Counts,
    pub groups: Vec<Group>,
    pub presets: Vec<PresetSummary>,
    pub limits: Vec<ProviderLimits>,
}
#[derive(Serialize, ToSchema)]
pub struct ConversationSnapshot {
    pub detail: ConversationDetail,
    pub queue: QueueView,
    pub history: HistoryPage,
    pub scheduled: Vec<ScheduledMessage>,
    /// Незавершённые запросы по времени создания: открытые, с сохранённым и с передаваемым агенту решением.
    pub open_requests: Vec<RequestView>,
    /// Списки задач агента по дорожкам, основной агент первым.
    pub todos: Vec<crate::todos::TodoListView>,
    pub background: Vec<crate::background_tasks::BackgroundTaskView>,
    /// Последний известный расход контекста; `None` — данных нет, индикатор скрыт.
    pub context: Option<crate::compactions::ContextSnapshot>,
    /// Взведённые и недавние завершившиеся будильники разговора.
    pub alarms: Vec<crate::session_tools::alarms::AlarmView>,
}

pub type ConversationSnapshotView = ConversationSnapshot;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RequestView {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub execution_id: Uuid,
    pub generation: i64,
    pub kind: String,
    pub native_request_id: String,
    pub state: String,
    pub request_payload: serde_json::Value,
    pub decision_payload: Option<serde_json::Value>,
    pub winner_decision_id: Option<Uuid>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<crate::requests::RequestRow> for RequestView {
    fn from(row: crate::requests::RequestRow) -> Self {
        Self {
            id: row.id,
            conversation_id: row.conversation_id,
            execution_id: row.execution_id,
            generation: row.generation,
            kind: row.kind.as_str().to_string(),
            native_request_id: row.native_request_id,
            state: row.state.as_str().to_string(),
            request_payload: row.request_payload,
            decision_payload: row.decision_payload,
            winner_decision_id: row.winner_decision_id,
            created_at: crate::conversations::time(row.created_at),
            updated_at: crate::conversations::time(row.updated_at),
        }
    }
}
#[derive(Serialize, ToSchema)]
#[schema(as = SnapshotQueueView)]
pub struct QueueView {
    pub queued: Vec<InputView>,
    pub pause_reasons: Vec<PauseReason>,
    pub hold_reasons: Vec<HoldView>,
    pub run_slot: RunSlot,
}
#[derive(Serialize, ToSchema)]
pub struct MachinesSnapshot {
    pub items: Vec<MachineView>,
}

pub async fn begin(pool: &PgPool) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("set transaction isolation level repeatable read read only")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}
pub async fn conversations(
    pool: &PgPool,
    owner_id: Uuid,
) -> Result<Snapshot<ListSnapshot>, sqlx::Error> {
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            let scope = Scope::Conversations { owner_id };
            let cursor = events::boundary(&mut *conn, &scope).await?.to_string();
            let mut items = conversations::snapshot_items(&mut *conn, owner_id).await?;
            let truncated = items.len() > 500;
            items.truncate(500);
            let data = ListSnapshot {
                items,
                truncated,
                counts: conversations::counts(&mut *conn, owner_id).await?,
                groups: groups::list(&mut *conn, owner_id).await?,
                presets: crate::presets::list(&mut *conn, owner_id).await?,
                limits: crate::limits::current(&mut *conn)
                    .await?
                    .into_iter()
                    .map(ProviderLimits::from)
                    .collect(),
            };
            Ok(Snapshot {
                scope: events::wire_scope(&scope),
                cursor,
                data,
            })
        })
    })
    .await
}
pub async fn conversation(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    history_limit: i64,
) -> Result<Option<Snapshot<ConversationSnapshot>>, sqlx::Error> {
    conversation_with_registry(pool, owner_id, id, history_limit, None).await
}
pub async fn conversation_with_registry(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    history_limit: i64,
    registry: Option<&Registry>,
) -> Result<Option<Snapshot<ConversationSnapshot>>, sqlx::Error> {
    let registry = registry.cloned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
    sqlx::query("set transaction isolation level repeatable read read only")
        .execute(&mut *conn)
        .await?;
    let scope = Scope::Conversation { id };
    let cursor = events::boundary(&mut *conn, &scope).await?.to_string();
    let Some(detail) = conversations::detail(&mut *conn, owner_id, id).await? else {
        return Ok(None);
    };
    let row = sqlx::query("select * from queue_state where conversation_id=$1")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    let queued=sqlx::query("select * from inputs where conversation_id=$1 and feed_seq is null and state in ('queued','dispatched','on_machine') order by input_seq limit 100").bind(id).fetch_all(&mut *conn).await?.into_iter().map(history::input_view).collect::<Result<Vec<_>,_>>()?;
    let pauses: sqlx::types::Json<Vec<PauseReason>> = row.try_get("pause_reasons")?;
    let run_slot = RunSlot::from_columns(
        row.try_get("run_slot")?,
        row.try_get("run_slot_input_id")?,
        row.try_get("run_slot_attempt")?,
    )
    .map_err(|_| sqlx::Error::Protocol("invalid run slot".into()))?;
    let mut holds = detail.holds.clone();
    let machine = detail.attributes.machine.id;
    if !queued.is_empty()
        && detail.execution.is_some()
        && !registry.as_ref().is_some_and(|r| r.is_reconciled(machine))
    {
        holds.push(HoldView::MachineNotReconciled);
    }
    let live = registry
        .as_ref()
        .filter(|r| r.session_info(machine).is_some());
    let held = match live {
        Some(r) if !r.has_capability(machine, "pilot_v1") => {
            conversations::capability_hold_input(&mut *conn, id).await?
        }
        Some(r) if !r.has_capability(machine, crate::session::CAPABILITY_INSTRUCTIONS_V1) => {
            conversations::instructions_hold_input(&mut *conn, id).await?
        }
        _ => None,
    };
    if let Some(input_id) = held
        && !holds
            .iter()
            .any(|h| matches!(h, HoldView::CapabilityMissing { .. }))
    {
        holds.push(HoldView::CapabilityMissing { input_id });
    }
    let history = history::page(&mut *conn, owner_id, id, HistoryCursor::Tail, history_limit)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    let scheduled = scheduler::list_messages(&mut *conn, owner_id, id).await?;
    let open_requests_rows: Vec<crate::requests::RequestRow> = sqlx::query_as(
        "select r.* from requests r join executions e on e.id = r.execution_id
         where r.conversation_id = $1 and r.state in ('open', 'decision_saved', 'decision_transmitting')
           and e.state not in ('stopped', 'failed', 'owner_lost')
         order by r.created_at asc, r.id asc",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let open_requests = open_requests_rows
        .into_iter()
        .map(RequestView::from)
        .collect();
    let todos = crate::todos::list(&mut *conn, id).await?;
    let background = crate::background_tasks::list(&mut *conn, id).await?;
    let context = crate::compactions::latest_context(&mut *conn, id).await?;
    let alarm_rows = crate::session_tools::alarms::snapshot_rows(&mut *conn, id).await?;
    let data = ConversationSnapshot {
        detail,
        queue: QueueView {
            queued,
            pause_reasons: pauses.0,
            hold_reasons: holds,
            run_slot,
        },
        history,
        scheduled,
        open_requests,
        todos,
        background,
        context,
        alarms: crate::session_tools::alarms::snapshot_views(&alarm_rows),
    };
    Ok(Some(Snapshot {
        scope: events::wire_scope(&scope),
        cursor,
        data,
    }))
        })
    })
    .await
}
pub async fn machines(
    pool: &PgPool,
    owner_id: Uuid,
) -> Result<Snapshot<MachinesSnapshot>, sqlx::Error> {
    machines_with_config(pool, owner_id, &Default::default()).await
}
pub async fn machines_with_config(
    pool: &PgPool,
    owner_id: Uuid,
    config: &crate::config::MachinesConfig,
) -> Result<Snapshot<MachinesSnapshot>, sqlx::Error> {
    let config = config.clone();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            sqlx::query("set transaction isolation level repeatable read read only")
                .execute(&mut *conn)
                .await?;
            let scope = Scope::Machines { owner_id };
            let cursor = events::boundary(&mut *conn, &scope).await?.to_string();
            let ids: Vec<Uuid> =
                sqlx::query_scalar("select id from machines where owner_id=$1 order by name,id")
                    .bind(owner_id)
                    .fetch_all(&mut *conn)
                    .await?;
            let mut items = Vec::with_capacity(ids.len());
            for id in ids {
                items.push(
                    crate::http::machines::load_stored(&mut *conn, &config, owner_id, id).await?,
                );
            }
            Ok(Snapshot {
                scope: events::wire_scope(&scope),
                cursor,
                data: MachinesSnapshot { items },
            })
        })
    })
    .await
}
