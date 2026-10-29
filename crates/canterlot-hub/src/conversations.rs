use crate::{
    activity,
    config::LimitsConfig,
    events::{self, NewEvent, Scope},
    executions::{self, StartedExecution},
    idempotency,
    model::{HoldReason, HoldView},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Row, postgres::PgRow};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CrunchSetting {
    pub enabled: bool,
    pub resume_text: Option<String>,
}
#[derive(Serialize)]
pub struct NewConversation {
    pub owner_id: Uuid,
    pub machine_id: Uuid,
    pub harness: String,
    pub workdir: String,
    pub model: String,
    pub effort: String,
    pub permission_mode: Option<String>,
    pub instruction_profile: Option<String>,
    pub title: Option<String>,
    pub color: Option<String>,
    pub group_ids: Vec<Uuid>,
    pub crunch: CrunchSetting,
    pub start: bool,
}
#[derive(Serialize, Deserialize, ToSchema)]
pub struct Created {
    pub conversation_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<StartedExecution>,
}
#[derive(thiserror::Error)]
pub enum CreateError {
    #[error("machine not found")]
    MachineNotFound,
    #[error("machine unavailable")]
    MachineUnavailable,
    #[error("capability missing")]
    CapabilityMissing,
    #[error("harness unknown")]
    HarnessUnknown,
    #[error("model unknown")]
    ModelUnknown,
    #[error("effort not allowed")]
    EffortNotAllowed,
    #[error("permission mode unknown")]
    PermissionModeUnknown,
    #[error("profile unknown")]
    ProfileUnknown,
    #[error("workdir not allowed")]
    WorkdirNotAllowed,
    #[error("group not found")]
    GroupNotFound,
    #[error("invalid request")]
    Invalid(&'static str),
    #[error("idempotency conflict")]
    IdempotencyConflict,
    #[error("database error")]
    Db(#[from] sqlx::Error),
}
#[derive(thiserror::Error)]
pub enum UpdateError {
    #[error("not found")]
    NotFound,
    #[error("group not found")]
    GroupNotFound,
    #[error("invalid request")]
    Invalid(&'static str),
    #[error("archive active")]
    ArchiveActive(Vec<&'static str>),
    #[error("idempotency conflict")]
    IdempotencyConflict,
    #[error("database error")]
    Db(#[from] sqlx::Error),
}
impl std::fmt::Debug for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
impl std::fmt::Debug for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
impl From<idempotency::IdempotencyConflict> for CreateError {
    fn from(_: idempotency::IdempotencyConflict) -> Self {
        Self::IdempotencyConflict
    }
}
impl From<idempotency::IdempotencyConflict> for UpdateError {
    fn from(_: idempotency::IdempotencyConflict) -> Self {
        Self::IdempotencyConflict
    }
}

#[derive(Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Attributes {
    pub title: Option<String>,
    #[serde(
        default,
        deserialize_with = "nullable_color",
        skip_serializing_if = "Option::is_none"
    )]
    pub color: Option<Option<String>>,
    pub pinned: Option<bool>,
    pub archived: Option<bool>,
    #[serde(default)]
    pub force: bool,
}
fn nullable_color<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct MachineRef {
    pub id: Uuid,
    pub name: String,
}
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct HarnessRef {
    pub id: String,
    pub title: String,
    pub provider: String,
}
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct AttributeView {
    pub id: Uuid,
    pub title: String,
    pub title_source: String,
    pub color: Option<String>,
    pub pinned: bool,
    pub archived: bool,
    pub groups: Vec<Uuid>,
    pub machine: MachineRef,
    pub harness: HarnessRef,
    pub workdir: String,
    pub created_at: String,
}
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct ActivityView {
    #[schema(value_type=activity::ActivityState)]
    pub state: String,
    #[schema(value_type=activity::Evidence)]
    pub evidence: Value,
    pub since: String,
}
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct QueueView {
    pub queued: i32,
    #[schema(value_type=Vec<crate::model::PauseReason>)]
    pub pause_reasons: Value,
    #[schema(value_type=Vec<HoldView>)]
    pub hold_reasons: Value,
}
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct CrunchView {
    pub enabled: bool,
}
#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct ListItem {
    #[serde(flatten)]
    pub attributes: AttributeView,
    pub model: String,
    pub effort: String,
    pub permission_mode: String,
    pub activity: ActivityView,
    pub unread_count: i32,
    pub queue: QueueView,
    pub crunch: CrunchView,
    pub last_activity_at: Option<String>,
}
#[derive(Serialize, Deserialize, ToSchema)]
pub struct EffectiveSettings {
    pub model: String,
    pub effort: String,
    pub permission_mode: String,
    pub instruction_profile: String,
}
#[derive(Serialize, Deserialize, ToSchema)]
pub struct SettingsView {
    pub effective: EffectiveSettings,
    pub pending: Value,
    pub rev: i64,
}
#[derive(Serialize, Deserialize, ToSchema)]
pub struct CrunchDetail {
    pub enabled: bool,
    pub resume_text: Option<String>,
    pub reserve_override_until: Option<String>,
    pub resume_pending: bool,
}
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ConversationDetail {
    #[serde(flatten)]
    pub attributes: AttributeView,
    pub model: String,
    pub effort: String,
    pub permission_mode: String,
    pub activity: ActivityView,
    pub unread_count: i32,
    pub queue: QueueView,
    pub last_activity_at: Option<String>,
    pub settings: SettingsView,
    pub crunch: CrunchDetail,
    #[schema(value_type=Option<LiveExecutionView>)]
    pub execution: Option<Value>,
    pub holds: Vec<HoldView>,
}
#[derive(Serialize, Deserialize, ToSchema)]
pub struct GroupCount {
    pub id: Uuid,
    pub count: i64,
}
#[derive(Serialize, Deserialize, ToSchema)]
pub struct Counts {
    pub all: i64,
    pub pinned: i64,
    pub archived: i64,
    pub groups: Vec<GroupCount>,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ListFilter {
    All,
    Pinned,
    Group(Uuid),
    Archived,
}
#[derive(Serialize, Deserialize)]
pub struct ListCursor {
    pub pinned: bool,
    pub pinned_at: Option<DateTime<Utc>>,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub id: Uuid,
    pub seen: Vec<Uuid>,
    pub filter: ListFilter,
    pub q: Option<String>,
    pub owner_id: Uuid,
}
impl ListCursor {
    pub fn decode(value: &str) -> Option<Self> {
        if value.len() > 512 * 1024 {
            return None;
        }
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(value).ok()?).ok()
    }
    fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).expect("cursor JSON"))
    }
}
pub struct ListQuery {
    pub owner_id: Uuid,
    pub filter: ListFilter,
    pub q: Option<String>,
    pub cursor: Option<ListCursor>,
    pub limit: i64,
}
#[derive(Serialize, Deserialize, ToSchema)]
pub struct ListPage {
    pub items: Vec<ListItem>,
    pub next_cursor: Option<String>,
    pub counts: Counts,
}
pub(crate) fn time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}
fn optional_time(row: &PgRow, col: &str) -> Result<Option<String>, sqlx::Error> {
    Ok(row.try_get::<Option<DateTime<Utc>>, _>(col)?.map(time))
}

const SELECT:&str="select c.*,m.name machine_name,m.capabilities,m.availability,
 h.title harness_title,h.provider,
 coalesce(d.effective_model,'') effective_model,coalesce(d.effective_effort,'') effective_effort,
 coalesce(d.effective_permission_mode,'') effective_permission_mode,coalesce(d.crunch_enabled,false) derived_crunch,
 coalesce(d.activity_state,'idle') activity_state,coalesce(nullif(d.activity_evidence,'{}'),'{\"kind\":\"idle\",\"last_turn_at\":null,\"background_work\":null}') activity_evidence,
 coalesce(d.activity_since,c.created_at) activity_since,coalesce(d.unread_count,0) unread_count,
 coalesce(d.queue_queued,0) queue_queued,coalesce(d.pause_reasons,'[]') derived_pauses,coalesce(d.hold_reasons,'[]') derived_holds,
 d.last_activity_at derived_last_activity,
 array(select gm.group_id from conversation_group_members gm where gm.conversation_id=c.id order by gm.group_id) groups
 from conversations c join machines m on m.id=c.machine_id
 left join machine_harnesses h on h.machine_id=c.machine_id and h.harness=c.harness
 left join conversation_derived d on d.conversation_id=c.id";
fn item(row: &PgRow) -> Result<ListItem, sqlx::Error> {
    Ok(ListItem {
        attributes: AttributeView {
            id: row.try_get("id")?,
            title: row.try_get("title")?,
            title_source: row.try_get("title_source")?,
            color: row.try_get("color")?,
            pinned: row.try_get("pinned")?,
            archived: row
                .try_get::<Option<DateTime<Utc>>, _>("archived_at")?
                .is_some(),
            groups: row.try_get("groups")?,
            machine: MachineRef {
                id: row.try_get("machine_id")?,
                name: row.try_get("machine_name")?,
            },
            harness: HarnessRef {
                id: row.try_get("harness")?,
                title: row
                    .try_get::<Option<String>, _>("harness_title")?
                    .unwrap_or_else(|| "Неизвестный харнесс".into()),
                provider: row
                    .try_get::<Option<String>, _>("provider")?
                    .unwrap_or_default(),
            },
            workdir: row.try_get("workdir")?,
            created_at: time(row.try_get("created_at")?),
        },
        model: row.try_get("effective_model")?,
        effort: row.try_get("effective_effort")?,
        permission_mode: row.try_get("effective_permission_mode")?,
        activity: ActivityView {
            state: row.try_get("activity_state")?,
            evidence: row.try_get("activity_evidence")?,
            since: time(row.try_get("activity_since")?),
        },
        unread_count: row.try_get("unread_count")?,
        queue: QueueView {
            queued: row.try_get("queue_queued")?,
            pause_reasons: row.try_get("derived_pauses")?,
            hold_reasons: row.try_get("derived_holds")?,
        },
        crunch: CrunchView {
            enabled: row.try_get("derived_crunch")?,
        },
        last_activity_at: optional_time(row, "derived_last_activity")?,
    })
}
pub async fn counts(conn: &mut PgConnection, owner_id: Uuid) -> Result<Counts, sqlx::Error> {
    let (all,pinned,archived)=sqlx::query_as("select count(*) filter(where archived_at is null),count(*) filter(where archived_at is null and pinned),count(*) filter(where archived_at is not null) from conversations where owner_id=$1").bind(owner_id).fetch_one(&mut *conn).await?;
    let groups=sqlx::query_as::<_,(Uuid,i64)>("select g.id,count(c.id) from conversation_groups g left join conversation_group_members gm on gm.group_id=g.id left join conversations c on c.id=gm.conversation_id and c.owner_id=$1 and c.archived_at is null where g.owner_id=$1 group by g.id order by g.position,g.id").bind(owner_id).fetch_all(conn).await?.into_iter().map(|(id,count)|GroupCount{id,count}).collect();
    Ok(Counts {
        all,
        pinned,
        archived,
        groups,
    })
}
pub async fn list(conn: &mut PgConnection, q: &ListQuery) -> Result<ListPage, sqlx::Error> {
    let invalid = || sqlx::Error::InvalidArgument("invalid list query".into());
    if !(1..=200).contains(&q.limit) || q.q.as_ref().is_some_and(|s| s.len() > 4096) {
        return Err(invalid());
    }
    if let Some(c) = &q.cursor
        && (c.owner_id != q.owner_id || c.filter != q.filter || c.q != q.q || c.seen.len() > 10000)
    {
        return Err(invalid());
    }
    let mut sql = sqlx::QueryBuilder::new(SELECT);
    sql.push(" where c.owner_id=").push_bind(q.owner_id);
    match q.filter {
        ListFilter::All => {
            sql.push(" and c.archived_at is null");
        }
        ListFilter::Pinned => {
            sql.push(" and c.archived_at is null and c.pinned");
        }
        ListFilter::Archived => {
            sql.push(" and c.archived_at is not null");
        }
        ListFilter::Group(id) => {
            sql.push(" and c.archived_at is null and exists(select 1 from conversation_group_members gm where gm.conversation_id=c.id and gm.group_id=").push_bind(id).push(")");
        }
    }
    if let Some(text) = &q.q {
        let pattern = format!(
            "%{}%",
            text.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        sql.push(" and lower(c.title collate \"und-x-icu\") like lower(")
            .push_bind(pattern)
            .push(" collate \"und-x-icu\") escape E'\\\\'");
    }
    if let Some(c) = &q.cursor {
        sql.push(" and not(c.id=any(").push_bind(c.seen.clone()).push(")) and (c.pinned < ").push_bind(c.pinned)
            .push(" or (c.pinned = ").push_bind(c.pinned).push(" and (coalesce(c.pinned_at,'-infinity') < coalesce(").push_bind(c.pinned_at).push("::timestamptz,'-infinity') or (coalesce(c.pinned_at,'-infinity')=coalesce(").push_bind(c.pinned_at)
            .push("::timestamptz,'-infinity') and (coalesce(c.last_activity_at,'-infinity') < coalesce(").push_bind(c.last_activity_at).push("::timestamptz,'-infinity') or (coalesce(c.last_activity_at,'-infinity')=coalesce(").push_bind(c.last_activity_at).push("::timestamptz,'-infinity') and c.id > ").push_bind(c.id).push("))))))");
    }
    sql.push(" order by c.pinned desc,c.pinned_at desc nulls last,c.last_activity_at desc nulls last,c.id limit ").push_bind(q.limit+1);
    let mut rows = sql.build().fetch_all(&mut *conn).await?;
    let more = rows.len() > q.limit as usize;
    rows.truncate(q.limit as usize);
    let next_cursor = if more {
        let row = rows.last().expect("nonempty page");
        let mut seen = q
            .cursor
            .as_ref()
            .map(|c| c.seen.clone())
            .unwrap_or_default();
        for row in &rows {
            seen.push(row.try_get("id")?);
        }
        Some(
            ListCursor {
                pinned: row.try_get("pinned")?,
                pinned_at: row.try_get("pinned_at")?,
                last_activity_at: row.try_get("last_activity_at")?,
                id: row.try_get("id")?,
                seen,
                filter: q.filter.clone(),
                q: q.q.clone(),
                owner_id: q.owner_id,
            }
            .encode(),
        )
    } else {
        None
    };
    let items = rows.iter().map(item).collect::<Result<_, _>>()?;
    Ok(ListPage {
        items,
        next_cursor,
        counts: counts(conn, q.owner_id).await?,
    })
}
pub(crate) async fn snapshot_items(
    conn: &mut PgConnection,
    owner_id: Uuid,
) -> Result<Vec<ListItem>, sqlx::Error> {
    let mut q = sqlx::QueryBuilder::new(SELECT);
    q.push(" where c.owner_id=").push_bind(owner_id)
        .push(" order by c.pinned desc,c.pinned_at desc nulls last,d.last_activity_at desc nulls last,c.id limit 501");
    q.build().fetch_all(conn).await?.iter().map(item).collect()
}
/// Голова очереди, команда которой несёт настройки или вложения: без `pilot_v1` в сеансе она не уйдёт.
pub async fn capability_hold_input(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select i.id from inputs i join commands cmd on cmd.id=i.command_id
         where i.conversation_id=$1 and i.state in ('queued','dispatched')
           and ((i.state='queued' and (select pending_settings <> '{}'::jsonb from conversations where id=i.conversation_id))
                or (i.state='dispatched' and cmd.payload ? 'settings')
                or jsonb_array_length(coalesce(cmd.payload->'attachment_ids','[]')) > 0)
         order by i.input_seq limit 1",
    )
    .bind(id)
    .fetch_optional(conn)
    .await
}
/// Вычисляемое удержание: ввод выдан машине, но не все его файлы опубликованы у агента.
/// Причина берётся из `input_attachments.machine_reason` и нигде не хранится как удержание.
pub async fn attachments_pending(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Vec<(Uuid, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        "select ia.input_id, (array_agg(ia.machine_reason order by ia.position)
                filter (where ia.machine_reason is not null))[1]
         from input_attachments ia join inputs i on i.id = ia.input_id
         where i.conversation_id = $1 and i.state in ('dispatched','on_machine')
           and ia.machine_state <> 'ready'
         group by ia.input_id order by min(i.input_seq)",
    )
    .bind(id)
    .fetch_all(conn)
    .await
}
pub async fn detail(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Option<ConversationDetail>, sqlx::Error> {
    let Some(row) = load_row(conn, owner_id, id).await? else {
        return Ok(None);
    };
    let mut i = item(&row)?;
    let queue = sqlx::query("select hold_reasons from queue_state where conversation_id=$1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    let mut holds: Vec<HoldView> = queue
        .map(|r| r.try_get::<sqlx::types::Json<Vec<HoldReason>>, _>("hold_reasons"))
        .transpose()?
        .map(|v| v.0.into_iter().map(HoldView::from).collect())
        .unwrap_or_default();
    let caps: Vec<String> = row.try_get("capabilities")?;
    if !caps.iter().any(|c| c == "pilot_v1")
        && let Some(input_id) = capability_hold_input(conn, id).await?
    {
        holds.push(HoldView::CapabilityMissing { input_id });
        if let Some(reasons) = i.queue.hold_reasons.as_array_mut() {
            reasons.push(json!({"reason":"capability_missing","input_id":input_id}));
        }
    }
    if let Some(reasons) = i.queue.hold_reasons.as_array_mut() {
        reasons.retain(|hold| hold["reason"] != "attachments_pending");
    }
    for (input_id, reason) in attachments_pending(conn, id).await? {
        let hold = HoldView::AttachmentsPending { input_id, reason };
        if let Some(reasons) = i.queue.hold_reasons.as_array_mut() {
            reasons
                .push(serde_json::to_value(&hold).map_err(|e| sqlx::Error::Encode(Box::new(e)))?);
        }
        holds.push(hold);
    }
    let execution=sqlx::query("select id,generation,state,outcome,native_conversation_id,created_at from executions where conversation_id=$1 and state not in ('stopped','failed')").bind(id).fetch_optional(&mut *conn).await?.map(|r|->Result<Value,sqlx::Error>{Ok(json!({"execution_id":r.try_get::<Uuid,_>("id")?,"generation":r.try_get::<i64,_>("generation")?,"state":r.try_get::<String,_>("state")?,"outcome":r.try_get::<Option<String>,_>("outcome")?,"native_conversation_id":r.try_get::<Option<String>,_>("native_conversation_id")?,"created_at":time(r.try_get("created_at")?)}))}).transpose()?;
    let resume_pending: bool = row.try_get("resume_pending")?;
    Ok(Some(ConversationDetail {
        attributes: i.attributes,
        model: i.model,
        effort: i.effort,
        permission_mode: i.permission_mode,
        activity: i.activity,
        unread_count: i.unread_count,
        queue: i.queue,
        last_activity_at: i.last_activity_at,
        settings: SettingsView {
            effective: EffectiveSettings {
                model: row.try_get("model")?,
                effort: row.try_get("effort")?,
                permission_mode: row.try_get("permission_mode")?,
                instruction_profile: row.try_get("instruction_profile")?,
            },
            pending: row.try_get("pending_settings")?,
            rev: row.try_get("settings_rev")?,
        },
        crunch: CrunchDetail {
            enabled: row.try_get("crunch_enabled")?,
            resume_text: row.try_get("crunch_resume_text")?,
            reserve_override_until: optional_time(&row, "reserve_override_until")?,
            resume_pending,
        },
        execution,
        holds,
    }))
}
pub async fn recent_workdirs(
    conn: &mut PgConnection,
    owner_id: Uuid,
    machine_id: Uuid,
    limit: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("select workdir from conversations where owner_id=$1 and machine_id=$2 group by workdir order by max(created_at) desc,workdir limit $3").bind(owner_id).bind(machine_id).bind(limit.clamp(0,20)).fetch_all(conn).await
}

pub(crate) async fn lock_owner(conn: &mut PgConnection, owner: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "select pg_advisory_xact_lock(hashtextextended('canterlot:attributes:'||$1::text,0))",
    )
    .bind(owner)
    .execute(conn)
    .await?;
    Ok(())
}
pub(crate) async fn lock_conversation(
    conn: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
) -> Result<(), UpdateError> {
    sqlx::query("select q.conversation_id from queue_state q join conversations c on c.id=q.conversation_id where c.owner_id=$1 and c.id=$2 for update of q").bind(owner).bind(id).fetch_optional(&mut *conn).await?.ok_or(UpdateError::NotFound)?;
    sqlx::query("select id from conversations where id=$1 and owner_id=$2 for no key update")
        .bind(id)
        .bind(owner)
        .fetch_optional(conn)
        .await?
        .ok_or(UpdateError::NotFound)?;
    Ok(())
}
fn valid_color(color: &str) -> bool {
    !color.is_empty()
        && color.len() <= 24
        && color
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
fn valid_text(s: &str, max: usize) -> bool {
    !s.contains('\0') && s.chars().count() <= max
}
fn workdir_allowed(workdir: &str, roots: &Value) -> bool {
    use std::path::{Component, Path};
    let path = Path::new(workdir);
    path.is_absolute()
        && !workdir.contains('\0')
        && !path.components().any(|c| c == Component::ParentDir)
        && roots.as_array().is_some_and(|rs| {
            rs.iter()
                .filter_map(Value::as_str)
                .any(|root| Path::new(root).is_absolute() && path.starts_with(root))
        })
}
async fn groups_valid(
    conn: &mut PgConnection,
    owner: Uuid,
    groups: &[Uuid],
) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        "select count(*) from conversation_groups where owner_id=$1 and id=any($2)",
    )
    .bind(owner)
    .bind(groups)
    .fetch_one(conn)
    .await?;
    let mut unique = groups.to_vec();
    unique.sort_unstable();
    unique.dedup();
    Ok(n == unique.len() as i64)
}
async fn replace_groups(
    conn: &mut PgConnection,
    id: Uuid,
    groups: &[Uuid],
) -> Result<(), sqlx::Error> {
    sqlx::query("delete from conversation_group_members where conversation_id=$1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    for group in groups {
        sqlx::query("insert into conversation_group_members(group_id,conversation_id) values($1,$2) on conflict do nothing").bind(group).bind(id).execute(&mut *conn).await?;
    }
    Ok(())
}
fn validate_crunch(setting: &CrunchSetting) -> bool {
    setting
        .resume_text
        .as_ref()
        .is_none_or(|s| valid_text(s, 262144) && s.len() <= 262144)
}
pub async fn create(
    pool: &PgPool,
    key: &str,
    req: NewConversation,
    limits: &LimitsConfig,
    correlation_id: Uuid,
) -> Result<Created, CreateError> {
    let request = serde_json::to_value(&req).expect("create request");
    let limits = limits.clone();
    let outcome = idempotency::run_in_tx(
        pool,
        req.owner_id,
        key,
        "create_conversation",
        &request,
        None,
        move |conn| Box::pin(create_in_tx(conn, req, limits, correlation_id)),
    )
    .await?;
    Ok(outcome.value)
}
pub(crate) async fn append(
    conn: &mut PgConnection,
    scope: Scope,
    kind: &'static str,
    id: Option<Uuid>,
    payload: Value,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope,
            kind,
            conversation_id: id,
            execution_id: None,
            payload,
            correlation_id,
        },
    )
    .await?;
    Ok(())
}
pub(crate) async fn attribute_payload(
    conn: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
) -> Result<Value, sqlx::Error> {
    let row = load_row(conn, owner, id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    let mut p = serde_json::to_value(item(&row)?.attributes).expect("attributes");
    p["counts"] = serde_json::to_value(counts(conn, owner).await?).expect("counts");
    Ok(p)
}
pub(crate) async fn emit_attributes(
    conn: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
    correlation: Uuid,
) -> Result<(), sqlx::Error> {
    let mut p = attribute_payload(conn, owner, id).await?;
    append(
        conn,
        Scope::Conversation { id },
        "conversation_updated",
        Some(id),
        p.clone(),
        correlation,
    )
    .await?;
    let scope = Scope::Conversations { owner_id: owner }.key();
    sqlx::query("insert into scope_counters(scope,last_seq) values($1,0) on conflict do nothing")
        .bind(&scope)
        .execute(&mut *conn)
        .await?;
    sqlx::query("select scope from scope_counters where scope=$1 for update")
        .bind(scope)
        .execute(&mut *conn)
        .await?;
    p["counts"] = serde_json::to_value(counts(conn, owner).await?).expect("counts");
    append(
        conn,
        Scope::Conversations { owner_id: owner },
        "conversation_updated",
        Some(id),
        p,
        correlation,
    )
    .await
}
pub async fn update_attributes(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    attrs: Attributes,
    correlation_id: Uuid,
) -> Result<ConversationDetail, UpdateError> {
    let request = json!({"id":id,"attributes":attrs});
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "update_attributes",
        &request,
        Some(id),
        move |conn| {
            Box::pin(update_attributes_in_tx(
                conn,
                owner_id,
                id,
                attrs,
                correlation_id,
            ))
        },
    )
    .await?;
    Ok(outcome.value)
}
pub async fn set_groups(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    groups: Vec<Uuid>,
    correlation_id: Uuid,
) -> Result<ConversationDetail, UpdateError> {
    let request = json!({"id":id,"group_ids":groups});
    Ok(idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "set_groups",
        &request,
        Some(id),
        move |conn| {
            Box::pin(async move {
                lock_owner(conn, owner_id).await?;
                lock_conversation(conn, owner_id, id).await?;
                if !groups_valid(conn, owner_id, &groups).await? {
                    return Err(UpdateError::GroupNotFound);
                }
                replace_groups(conn, id, &groups).await?;
                emit_attributes(conn, owner_id, id, correlation_id).await?;
                detail(conn, owner_id, id)
                    .await?
                    .ok_or(UpdateError::NotFound)
            })
        },
    )
    .await?
    .value)
}
pub async fn set_crunch(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    setting: CrunchSetting,
    limits: &LimitsConfig,
    correlation_id: Uuid,
) -> Result<ConversationDetail, UpdateError> {
    let request = json!({"id":id,"crunch":setting});
    let limits = limits.clone();
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "set_crunch",
        &request,
        Some(id),
        move |conn| {
            Box::pin(set_crunch_in_tx(
                conn,
                owner_id,
                id,
                setting,
                limits,
                correlation_id,
            ))
        },
    )
    .await?;
    Ok(outcome.value)
}
pub async fn mark_read(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    seq: i64,
    thresholds: &activity::Thresholds,
    metrics_stale_after: std::time::Duration,
) -> Result<(), UpdateError> {
    if seq < 0 {
        return Err(UpdateError::Invalid("seq"));
    }
    let mut tx = pool.begin().await?;
    lock_conversation(&mut tx, owner_id, id).await?;
    sqlx::query("update conversations set last_read_seq=greatest(last_read_seq,$2) where id=$1")
        .bind(id)
        .bind(seq)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    activity::refresh_with_metrics(
        pool,
        id,
        Utc::now(),
        thresholds,
        metrics_stale_after,
        Uuid::now_v7(),
    )
    .await?;
    Ok(())
}

async fn load_row(
    conn: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
) -> Result<Option<PgRow>, sqlx::Error> {
    sqlx::QueryBuilder::new(SELECT)
        .push(" where c.id=")
        .push_bind(id)
        .push(" and c.owner_id=")
        .push_bind(owner)
        .build()
        .fetch_optional(conn)
        .await
}

#[derive(ToSchema)]
pub struct LiveExecutionView {
    pub execution_id: Uuid,
    pub generation: i64,
    pub state: String,
    pub outcome: Option<String>,
    pub native_conversation_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

async fn create_in_tx(
    conn: &mut PgConnection,
    req: NewConversation,
    limits: LimitsConfig,
    correlation_id: Uuid,
) -> Result<Created, CreateError> {
    lock_owner(conn, req.owner_id).await?;
    let m = sqlx::query("select * from machines where id=$1 and owner_id=$2 for no key update")
        .bind(req.machine_id)
        .bind(req.owner_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(CreateError::MachineNotFound)?;
    if !m
        .try_get::<Vec<String>, _>("capabilities")?
        .iter()
        .any(|c| c == "pilot_v1")
    {
        return Err(CreateError::CapabilityMissing);
    }
    let h = sqlx::query("select * from machine_harnesses where machine_id=$1 and harness=$2")
        .bind(req.machine_id)
        .bind(&req.harness)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(CreateError::HarnessUnknown)?;
    let models: Value = h.try_get("models")?;
    let model = models
        .as_array()
        .and_then(|ms| ms.iter().find(|m| m["id"] == req.model))
        .ok_or(CreateError::ModelUnknown)?;
    if !model["efforts"]
        .as_array()
        .is_some_and(|es| es.iter().any(|e| e == &req.effort))
    {
        return Err(CreateError::EffortNotAllowed);
    }
    let permission = req.permission_mode.as_deref().unwrap_or("");
    if !permission.is_empty()
        && !h
            .try_get::<Value, _>("permission_modes")?
            .as_array()
            .is_some_and(|ps| ps.iter().any(|p| p == permission))
    {
        return Err(CreateError::PermissionModeUnknown);
    }
    let profile = req.instruction_profile.as_deref().unwrap_or("owner");
    if profile != "owner" {
        let valid:bool=sqlx::query_scalar("select exists(select 1 from machine_profiles where machine_id=$1 and name=$2 and (harnesses='[]' or harnesses ? $3))").bind(req.machine_id).bind(profile).bind(&req.harness).fetch_one(&mut *conn).await?;
        if !valid {
            return Err(CreateError::ProfileUnknown);
        }
    }
    if !workdir_allowed(&req.workdir, &m.try_get("allowed_roots")?) {
        return Err(CreateError::WorkdirNotAllowed);
    }
    if !groups_valid(conn, req.owner_id, &req.group_ids).await? {
        return Err(CreateError::GroupNotFound);
    }
    if req.start && m.try_get::<String, _>("availability")? == "unavailable" {
        return Err(CreateError::MachineUnavailable);
    }
    if req.title.as_ref().is_some_and(|t| !valid_text(t, 256))
        || req.color.as_ref().is_some_and(|c| !valid_color(c))
        || !validate_crunch(&req.crunch)
    {
        return Err(CreateError::Invalid("attributes"));
    }
    // Строка провайдера `for share` раньше очереди: правило резерва видит новый разговор или наоборот.
    let mut holds = vec![HoldReason::NoLiveExecution];
    if let Some(provider) = canterlot_proto::harness::provider_of(&req.harness) {
        crate::limits::share_provider(conn, provider).await?;
        holds.extend(crate::crunch::initial_holds_in_tx(conn, provider).await?);
    }
    let id = Uuid::now_v7();
    let title = req.title.as_deref().unwrap_or("");
    let user = !title.trim().is_empty();
    let resume = if req.crunch.enabled {
        Some(
            req.crunch
                .resume_text
                .as_deref()
                .unwrap_or(&limits.default_resume_text),
        )
    } else {
        req.crunch.resume_text.as_deref()
    };
    sqlx::query("insert into conversations(id,owner_id,machine_id,harness,workdir,title,title_source,color,model,effort,permission_mode,instruction_profile,crunch_enabled,crunch_resume_text) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)")
            .bind(id).bind(req.owner_id).bind(req.machine_id).bind(&req.harness).bind(&req.workdir).bind(if user{title}else{"Новая сессия"}).bind(if user{"user"}else{"auto"}).bind(&req.color).bind(&req.model).bind(&req.effort).bind(permission).bind(profile).bind(req.crunch.enabled).bind(resume).execute(&mut *conn).await?;
    sqlx::query("insert into queue_state(conversation_id,hold_reasons) values($1,$2)")
        .bind(id)
        .bind(sqlx::types::Json(&holds))
        .execute(&mut *conn)
        .await?;
    replace_groups(conn, id, &req.group_ids).await?;
    let execution = if req.start {
        Some(
            executions::start_in_tx(
                conn,
                req.owner_id,
                id,
                &executions::StartOptions { resume: false },
                correlation_id,
            )
            .await
            .map_err(|e| match e {
                executions::ExecutionError::Db(e) => CreateError::Db(e),
                _ => CreateError::Invalid("start"),
            })?,
        )
    } else {
        None
    };
    sqlx::query("insert into conversation_derived(conversation_id,effective_model,effective_effort,effective_permission_mode,crunch_enabled,hold_reasons,activity_evidence) select id,model,effort,permission_mode,crunch_enabled,q.hold_reasons,'{\"kind\":\"idle\",\"last_turn_at\":null,\"background_work\":null}' from conversations c join queue_state q on q.conversation_id=c.id where c.id=$1").bind(id).execute(&mut *conn).await?;
    let row = load_row(conn, req.owner_id, id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    let mut payload = serde_json::to_value(item(&row)?).expect("list item");
    payload["counts"] = serde_json::to_value(counts(conn, req.owner_id).await?).expect("counts");
    append(
        conn,
        Scope::Conversations {
            owner_id: req.owner_id,
        },
        "conversation_created",
        Some(id),
        payload,
        correlation_id,
    )
    .await?;
    Ok(Created {
        conversation_id: id,
        execution,
    })
}

async fn update_attributes_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    attrs: Attributes,
    correlation_id: Uuid,
) -> Result<ConversationDetail, UpdateError> {
    lock_owner(conn, owner_id).await?;
    lock_conversation(conn, owner_id, id).await?;
    if attrs.title.as_ref().is_some_and(|t| !valid_text(t, 256))
        || attrs
            .color
            .as_ref()
            .and_then(Option::as_ref)
            .is_some_and(|c| !valid_color(c))
        || attrs.force && attrs.archived != Some(true)
    {
        return Err(UpdateError::Invalid("attributes"));
    }
    if attrs.archived == Some(true) {
        let crunch: bool =
            sqlx::query_scalar("select crunch_enabled from conversations where id=$1")
                .bind(id)
                .fetch_one(&mut *conn)
                .await?;
        let jobs:Vec<Uuid>=sqlx::query_scalar("select id from scheduled_jobs where conversation_id=$1 and state='pending' and kind in ('message','wake')").bind(id).fetch_all(&mut *conn).await?;
        let mut reasons = Vec::new();
        if crunch {
            reasons.push("crunch");
        }
        if !jobs.is_empty() {
            reasons.push("scheduled");
        }
        if executions::live(conn, id).await?.is_some() {
            reasons.push("live_execution");
        }
        if !attrs.force && !reasons.is_empty() {
            return Err(UpdateError::ArchiveActive(reasons));
        }
        if attrs.force {
            let locked: Vec<Uuid> = sqlx::query_scalar(
                "select id from scheduled_jobs where id=any($1) for update skip locked",
            )
            .bind(&jobs)
            .fetch_all(&mut *conn)
            .await?;
            if locked.len() != jobs.len() {
                return Err(UpdateError::ArchiveActive(vec!["scheduled"]));
            }
            sqlx::query("update scheduled_jobs set state='cancelled',updated_at=clock_timestamp() where id=any($1)").bind(&locked).execute(&mut *conn).await?;
            crate::attachments::unlink_scheduled_in_tx(conn, &locked).await?;
            crate::scheduler::publish_archive_cancellations(conn, &locked, correlation_id).await?;
            sqlx::query("update conversations set crunch_enabled=false where id=$1")
                .bind(id)
                .execute(&mut *conn)
                .await?;
            if crunch {
                crate::crunch::disable_in_tx(conn, id, correlation_id).await?;
            }
        }
    }
    sqlx::query("update conversations set title=coalesce($2,title),title_source=case when $2::text is null then title_source when length(btrim($2))=0 then 'auto' else 'user' end,color=case when $3 then $4 else color end,pinned=coalesce($5,pinned),pinned_at=case when $5=true and not pinned then clock_timestamp() when $5=false then null else pinned_at end,archived_at=case when $6=true then coalesce(archived_at,clock_timestamp()) when $6=false then null else archived_at end,updated_at=clock_timestamp() where id=$1")
            .bind(id).bind(&attrs.title).bind(attrs.color.is_some()).bind(attrs.color.flatten()).bind(attrs.pinned).bind(attrs.archived).execute(&mut *conn).await?;
    emit_attributes(conn, owner_id, id, correlation_id).await?;
    detail(conn, owner_id, id)
        .await?
        .ok_or(UpdateError::NotFound)
}

async fn set_crunch_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    setting: CrunchSetting,
    limits: LimitsConfig,
    correlation_id: Uuid,
) -> Result<ConversationDetail, UpdateError> {
    lock_conversation(conn, owner_id, id).await?;
    if !validate_crunch(&setting) {
        return Err(UpdateError::Invalid("resume_text"));
    }
    sqlx::query("update conversations set crunch_enabled=$2,crunch_resume_text=case when $3::text is not null then $3 when $2 then $4 else crunch_resume_text end,updated_at=clock_timestamp() where id=$1").bind(id).bind(setting.enabled).bind(&setting.resume_text).bind(&limits.default_resume_text).execute(&mut *conn).await?;
    if !setting.enabled {
        crate::crunch::disable_in_tx(conn, id, correlation_id).await?;
    }
    let (resume_pending, until): (bool, Option<DateTime<Utc>>) = sqlx::query_as(
        "select resume_pending,reserve_override_until from conversations where id=$1",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    append(
        conn,
        Scope::Conversation { id },
        "crunch_changed",
        Some(id),
        json!({"enabled":setting.enabled,"resume_pending":resume_pending,"reserve_override_until":until.map(time)}),
        correlation_id,
    )
    .await?;
    detail(conn, owner_id, id)
        .await?
        .ok_or(UpdateError::NotFound)
}

#[derive(Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
}

#[derive(thiserror::Error)]
pub enum SettingsError {
    #[error("not found")]
    NotFound,
    #[error("model unknown")]
    ModelUnknown,
    #[error("capability missing")]
    CapabilityMissing,
    #[error("effort not allowed")]
    EffortNotAllowed,
    #[error("permission mode unknown")]
    PermissionModeUnknown,
    #[error("settings midsession unsupported")]
    MidsessionUnsupported,
    #[error("idempotency conflict")]
    IdempotencyConflict,
    #[error("database error")]
    Db(#[from] sqlx::Error),
}
impl std::fmt::Debug for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
impl From<idempotency::IdempotencyConflict> for SettingsError {
    fn from(_: idempotency::IdempotencyConflict) -> Self {
        Self::IdempotencyConflict
    }
}

pub async fn change_settings(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    patch: SettingsPatch,
    correlation_id: Uuid,
) -> Result<SettingsView, SettingsError> {
    let owned: bool = sqlx::query_scalar(
        "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_one(pool)
    .await?;
    if !owned {
        return Err(SettingsError::NotFound);
    }
    let request = json!({"id": id, "settings": patch});
    Ok(idempotency::run_in_tx(pool, owner_id, key, "change_settings", &request, Some(id), move |conn| Box::pin(async move {
        sqlx::query("select q.conversation_id from queue_state q join conversations c on c.id=q.conversation_id where c.id=$1 and c.owner_id=$2 for update of q").bind(id).bind(owner_id).fetch_optional(&mut *conn).await?.ok_or(SettingsError::NotFound)?;
                let row = sqlx::query("select c.*, m.capabilities, h.models, h.permission_modes, h.settings_midsession from conversations c join machines m on m.id=c.machine_id left join machine_harnesses h on h.machine_id=c.machine_id and h.harness=c.harness where c.id=$1 and c.owner_id=$2 for no key update of c").bind(id).bind(owner_id).fetch_optional(&mut *conn).await?.ok_or(SettingsError::NotFound)?;
                if !row.try_get::<Vec<String>, _>("capabilities")?.iter().any(|v| v == "pilot_v1") { return Err(SettingsError::CapabilityMissing); }
        let mut pending: Value = row.try_get("pending_settings")?;
        for (k, v) in [("model", patch.model), ("effort", patch.effort), ("permission_mode", patch.permission_mode)] { if let Some(v) = v { pending[k] = json!(v); } }
        let model: String = pending.get("model").and_then(Value::as_str).map(str::to_owned).unwrap_or(row.try_get("model")?);
        let effort: String = pending.get("effort").and_then(Value::as_str).map(str::to_owned).unwrap_or(row.try_get("effort")?);
        let permission: String = pending.get("permission_mode").and_then(Value::as_str).map(str::to_owned).unwrap_or(row.try_get("permission_mode")?);
        let models: Option<Value> = row.try_get("models")?;
        let m = models.as_ref().and_then(Value::as_array).and_then(|v| v.iter().find(|v| v["id"] == model)).ok_or(SettingsError::ModelUnknown)?;
        if !m["efforts"].as_array().is_some_and(|v| v.iter().any(|v| v == &effort)) { return Err(SettingsError::EffortNotAllowed); }
        let modes: Option<Value> = row.try_get("permission_modes")?;
        if (pending.get("permission_mode").is_some() || !permission.is_empty()) && !modes.as_ref().and_then(Value::as_array).is_some_and(|v| v.iter().any(|v| v == &permission)) { return Err(SettingsError::PermissionModeUnknown); }
        let live = executions::live(conn, id).await?.is_some();
        if live && !row.try_get::<Option<bool>, _>("settings_midsession")?.unwrap_or(false) { return Err(SettingsError::MidsessionUnsupported); }
        sqlx::query("update conversations set pending_settings=$2,settings_rev=settings_rev+1 where id=$1").bind(id).bind(&pending).execute(&mut *conn).await?;
        let view = detail(conn, owner_id, id).await?.ok_or(SettingsError::NotFound)?.settings;
        append(conn, Scope::Conversation { id }, "conversation_settings_changed", Some(id), serde_json::to_value(&view).map_err(|e| sqlx::Error::Encode(Box::new(e)))?, correlation_id).await?;
        Ok(view)
    })).await?.value)
}

pub async fn promote_settings_in_tx(
    conn: &mut PgConnection,
    accepted_command_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    apply_sent_settings(conn, accepted_command_id, correlation_id, true).await
}
pub async fn drop_rejected_settings_in_tx(
    conn: &mut PgConnection,
    rejected_command_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    apply_sent_settings(conn, rejected_command_id, correlation_id, false).await
}
async fn apply_sent_settings(
    conn: &mut PgConnection,
    command: Uuid,
    correlation: Uuid,
    promote: bool,
) -> Result<(), sqlx::Error> {
    let Some((id, sent)): Option<(Uuid, Value)> = sqlx::query_as("select conversation_id,payload->'settings' from commands where id=$1 and payload ? 'settings'").bind(command).fetch_optional(&mut *conn).await? else { return Ok(()); };
    sqlx::query("select conversation_id from queue_state where conversation_id=$1 for update")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    let row = sqlx::query("select owner_id,model,effort,permission_mode,pending_settings from conversations where id=$1 for no key update").bind(id).fetch_one(&mut *conn).await?;
    let owner: Uuid = row.try_get("owner_id")?;
    let mut pending: Value = row.try_get("pending_settings")?;
    let mut model: String = row.try_get("model")?;
    let mut effort: String = row.try_get("effort")?;
    let mut permission: String = row.try_get("permission_mode")?;
    let mut sent_keys = Vec::new();
    for (key, effective) in [
        ("model", &mut model),
        ("effort", &mut effort),
        ("permission_mode", &mut permission),
    ] {
        if let Some(value) = sent.get(key).and_then(Value::as_str) {
            sent_keys.push(key);
            if promote {
                *effective = value.to_owned();
            }
            if pending.get(key).and_then(Value::as_str) == Some(value) {
                pending
                    .as_object_mut()
                    .expect("settings object")
                    .remove(key);
            }
        }
    }
    sqlx::query("update conversations set model=$2,effort=$3,permission_mode=$4,pending_settings=$5 where id=$1").bind(id).bind(model).bind(effort).bind(permission).bind(pending).execute(&mut *conn).await?;
    let view = detail(conn, owner, id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?
        .settings;
    let mut payload = serde_json::to_value(view).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    if !promote {
        payload["rejected"] = json!({"keys":sent_keys,"reason":"settings_unavailable"});
    }
    append(
        conn,
        Scope::Conversation { id },
        "conversation_settings_changed",
        Some(id),
        payload,
        correlation,
    )
    .await
}

pub(crate) async fn revalidate_pending_in_tx(
    conn: &mut PgConnection,
    id: Uuid,
    correlation: Uuid,
) -> Result<(Value, i64), sqlx::Error> {
    let row = sqlx::query("select c.owner_id,c.model,c.effort,c.permission_mode,c.pending_settings,c.settings_rev,h.models,h.permission_modes from conversations c left join machine_harnesses h on h.machine_id=c.machine_id and h.harness=c.harness where c.id=$1 for no key update of c").bind(id).fetch_one(&mut *conn).await?;
    let mut pending: Value = row.try_get("pending_settings")?;
    let rev = row.try_get("settings_rev")?;
    if pending.as_object().is_none_or(|p| p.is_empty()) {
        return Ok((pending, rev));
    }
    let owner: Uuid = row.try_get("owner_id")?;
    let effective_model: String = row.try_get("model")?;
    let effective_effort: String = row.try_get("effort")?;
    let models: Option<Value> = row.try_get("models")?;
    let modes: Option<Value> = row.try_get("permission_modes")?;
    let mut rejected = Vec::new();
    if let Some(model) = pending.get("model").and_then(Value::as_str)
        && !models
            .as_ref()
            .and_then(Value::as_array)
            .is_some_and(|v| v.iter().any(|m| m["id"] == model))
    {
        pending.as_object_mut().expect("settings").remove("model");
        rejected.push("model");
    }
    let model = pending
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(&effective_model);
    let effort = pending
        .get("effort")
        .and_then(Value::as_str)
        .unwrap_or(&effective_effort);
    if !models
        .as_ref()
        .and_then(Value::as_array)
        .and_then(|v| v.iter().find(|m| m["id"] == model))
        .and_then(|m| m["efforts"].as_array())
        .is_some_and(|v| v.iter().any(|e| e == effort))
    {
        let key = if pending.get("effort").is_some() {
            "effort"
        } else {
            "model"
        };
        if pending
            .as_object_mut()
            .expect("settings")
            .remove(key)
            .is_some()
        {
            rejected.push(key);
        }
    }
    if let Some(model) = pending.get("model").and_then(Value::as_str) {
        let effort = pending
            .get("effort")
            .and_then(Value::as_str)
            .unwrap_or(&effective_effort);
        if !models
            .as_ref()
            .and_then(Value::as_array)
            .and_then(|v| v.iter().find(|m| m["id"] == model))
            .and_then(|m| m["efforts"].as_array())
            .is_some_and(|v| v.iter().any(|e| e == effort))
        {
            pending.as_object_mut().expect("settings").remove("model");
            rejected.push("model");
        }
    }
    if let Some(mode) = pending.get("permission_mode").and_then(Value::as_str)
        && !modes
            .as_ref()
            .and_then(Value::as_array)
            .is_some_and(|v| v.iter().any(|m| m == mode))
    {
        pending
            .as_object_mut()
            .expect("settings")
            .remove("permission_mode");
        rejected.push("permission_mode");
    }
    let model = pending
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(&effective_model);
    let effort = pending
        .get("effort")
        .and_then(Value::as_str)
        .unwrap_or(&effective_effort);
    let effective_permission: String = row.try_get("permission_mode")?;
    let permission = pending
        .get("permission_mode")
        .and_then(Value::as_str)
        .unwrap_or(&effective_permission);
    let valid_model = models
        .as_ref()
        .and_then(Value::as_array)
        .and_then(|v| v.iter().find(|m| m["id"] == model))
        .and_then(|m| m["efforts"].as_array())
        .is_some_and(|v| v.iter().any(|e| e == effort));
    let valid_permission = permission.is_empty()
        || modes
            .as_ref()
            .and_then(Value::as_array)
            .is_some_and(|v| v.iter().any(|m| m == permission));
    if !valid_model || !valid_permission {
        for key in ["model", "effort", "permission_mode"] {
            if pending
                .as_object_mut()
                .expect("settings")
                .remove(key)
                .is_some()
            {
                rejected.push(key);
            }
        }
    }
    if !rejected.is_empty() {
        sqlx::query("update conversations set pending_settings=$2 where id=$1")
            .bind(id)
            .bind(&pending)
            .execute(&mut *conn)
            .await?;
        let view = detail(conn, owner, id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?
            .settings;
        let mut payload =
            serde_json::to_value(view).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
        payload["rejected"] = json!({"keys":rejected,"reason":"settings_unavailable"});
        append(
            conn,
            Scope::Conversation { id },
            "conversation_settings_changed",
            Some(id),
            payload,
            correlation,
        )
        .await?;
    }
    Ok((pending, rev))
}

pub struct SubmitMessage {
    pub owner_id: Uuid,
    pub conversation_id: Uuid,
    pub key: String,
    pub content: Vec<crate::delivery::ContentBlock>,
    pub attachment_ids: Vec<Uuid>,
    pub retry_of: Option<Uuid>,
    pub received_at: DateTime<Utc>,
}

pub async fn submit_message(
    pool: &PgPool,
    req: SubmitMessage,
    correlation_id: Uuid,
) -> Result<crate::delivery::SubmittedInput, crate::delivery::SubmitError> {
    let mut tx = pool.begin().await?;
    match submit_message_in_tx(&mut tx, &req, correlation_id).await {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(crate::delivery::SubmitError::KeyRaceLost) => {
            let _ = tx.rollback().await;
            let mut tx = pool.begin().await?;
            match submit_message_in_tx(&mut tx, &req, correlation_id).await {
                Ok(value) => {
                    tx.commit().await?;
                    Ok(value)
                }
                Err(error) => {
                    let _ = tx.rollback().await;
                    Err(error)
                }
            }
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

pub async fn submit_message_in_tx(
    conn: &mut PgConnection,
    req: &SubmitMessage,
    correlation_id: Uuid,
) -> Result<crate::delivery::SubmittedInput, crate::delivery::SubmitError> {
    let owned: bool = sqlx::query_scalar(
        "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
    )
    .bind(req.conversation_id)
    .bind(req.owner_id)
    .fetch_one(&mut *conn)
    .await?;
    if !owned {
        return Err(crate::delivery::SubmitError::ConversationNotFound);
    }
    crate::delivery::submit::submit_message_input_in_tx(
        conn,
        &crate::delivery::SubmitInput {
            owner_id: req.owner_id,
            conversation_id: req.conversation_id,
            idempotency_key: req.key.clone(),
            content: req.content.clone(),
            received_at: req.received_at,
            correlation_id,
        },
        &req.attachment_ids,
        req.retry_of,
    )
    .await
}

pub(crate) async fn auto_title_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
    content: &[crate::delivery::ContentBlock],
) -> Result<bool, sqlx::Error> {
    let text = content
        .iter()
        .flat_map(|b| match b {
            crate::delivery::ContentBlock::Text { text } => text.split_whitespace(),
            crate::delivery::ContentBlock::Attachment { .. } => "".split_whitespace(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut title: String = text.chars().take(60).collect();
    if text.chars().nth(60).is_some_and(|c| !c.is_whitespace())
        && let Some(i) = title.rfind(char::is_whitespace)
    {
        title.truncate(i);
    }
    let title = if title.trim().is_empty() {
        "Новая сессия"
    } else {
        title.trim()
    };
    Ok(sqlx::query("update conversations set title=$3,updated_at=clock_timestamp() where id=$1 and owner_id=$2 and title_source='auto'").bind(id).bind(owner).bind(title).execute(conn).await?.rows_affected() == 1)
}
