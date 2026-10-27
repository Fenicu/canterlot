pub use crate::http::limits::ProviderView as ProviderLimits;
use crate::http::machines::MachineView;
use crate::{
    conversations::{self, ConversationDetail, Counts, ListItem},
    events::{self, Scope},
    groups::{self, Group},
    history::{self, HistoryCursor, HistoryPage, InputView},
    model::{HoldView, PauseReason, RunSlot},
    scheduler::{self, ScheduledMessage},
    session::Registry,
};
use serde::Serialize;
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
    pub limits: Vec<ProviderLimits>,
}
#[derive(Serialize, ToSchema)]
pub struct ConversationSnapshot {
    pub detail: ConversationDetail,
    pub queue: QueueView,
    pub history: HistoryPage,
    pub scheduled: Vec<ScheduledMessage>,
}
#[derive(Serialize, ToSchema)]
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
    let mut tx = begin(pool).await?;
    let scope = Scope::Conversations { owner_id };
    let cursor = events::boundary(&mut tx, &scope).await?.to_string();
    let mut items = conversations::snapshot_items(&mut tx, owner_id).await?;
    let truncated = items.len() > 500;
    items.truncate(500);
    let data = ListSnapshot {
        items,
        truncated,
        counts: conversations::counts(&mut tx, owner_id).await?,
        groups: groups::list(&mut tx, owner_id).await?,
        limits: crate::limits::current(&mut tx)
            .await?
            .into_iter()
            .map(ProviderLimits::from)
            .collect(),
    };
    tx.commit().await?;
    Ok(Snapshot {
        scope: events::wire_scope(&scope),
        cursor,
        data,
    })
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
    let mut tx = begin(pool).await?;
    let scope = Scope::Conversation { id };
    let cursor = events::boundary(&mut tx, &scope).await?.to_string();
    let Some(detail) = conversations::detail(&mut tx, owner_id, id).await? else {
        return Ok(None);
    };
    let row = sqlx::query("select * from queue_state where conversation_id=$1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let queued=sqlx::query("select * from inputs where conversation_id=$1 and feed_seq is null and state in ('queued','dispatched','on_machine') order by input_seq limit 100").bind(id).fetch_all(&mut *tx).await?.into_iter().map(history::input_view).collect::<Result<Vec<_>,_>>()?;
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
        && !registry.is_some_and(|r| r.is_reconciled(machine))
    {
        holds.push(HoldView::MachineNotReconciled);
    }
    if registry.is_some_and(|r| {
        r.session_info(machine).is_some() && !r.has_capability(machine, "pilot_v1")
    }) && let Some(input_id) = conversations::capability_hold_input(&mut tx, id).await?
        && !holds
            .iter()
            .any(|h| matches!(h, HoldView::CapabilityMissing { .. }))
    {
        holds.push(HoldView::CapabilityMissing { input_id });
    }
    let history = history::page(&mut tx, owner_id, id, HistoryCursor::Tail, history_limit)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    let scheduled = scheduler::list_messages(&mut tx, owner_id, id).await?;
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
    };
    tx.commit().await?;
    Ok(Some(Snapshot {
        scope: events::wire_scope(&scope),
        cursor,
        data,
    }))
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
    let mut tx = begin(pool).await?;
    let scope = Scope::Machines { owner_id };
    let cursor = events::boundary(&mut tx, &scope).await?.to_string();
    let ids: Vec<Uuid> =
        sqlx::query_scalar("select id from machines where owner_id=$1 order by name,id")
            .bind(owner_id)
            .fetch_all(&mut *tx)
            .await?;
    let mut items = Vec::with_capacity(ids.len());
    for id in ids {
        items.push(crate::http::machines::load_stored(&mut tx, config, owner_id, id).await?);
    }
    tx.commit().await?;
    Ok(Snapshot {
        scope: events::wire_scope(&scope),
        cursor,
        data: MachinesSnapshot { items },
    })
}
