use crate::{
    conversations::{self, Counts},
    events::Scope,
    idempotency,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Row};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Serialize, Deserialize, ToSchema)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub position: i32,
    /// Значок группы; `None` — папка по умолчанию.
    pub icon: Option<GroupIcon>,
    pub created_at: String,
}
/// Значок группы из постоянного набора; тот же список проверяет база.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum GroupIcon {
    Folder,
    Briefcase,
    Home,
    Star,
    Code,
    Rocket,
    Book,
    Flask,
    Wrench,
    Server,
    Globe,
    Heart,
    User,
    Users,
    Gamepad,
    Music,
}
impl GroupIcon {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Folder => "folder",
            Self::Briefcase => "briefcase",
            Self::Home => "home",
            Self::Star => "star",
            Self::Code => "code",
            Self::Rocket => "rocket",
            Self::Book => "book",
            Self::Flask => "flask",
            Self::Wrench => "wrench",
            Self::Server => "server",
            Self::Globe => "globe",
            Self::Heart => "heart",
            Self::User => "user",
            Self::Users => "users",
            Self::Gamepad => "gamepad",
            Self::Music => "music",
        }
    }
}
#[derive(thiserror::Error)]
pub enum GroupError {
    #[error("not found")]
    NotFound,
    #[error("group name taken")]
    NameTaken,
    #[error("invalid request")]
    Invalid,
    #[error("idempotency conflict")]
    IdempotencyConflict,
    #[error("database error")]
    Db(sqlx::Error),
}
impl std::fmt::Debug for GroupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
impl From<sqlx::Error> for GroupError {
    fn from(e: sqlx::Error) -> Self {
        let info = crate::dberr::describe(&e);
        if info.constraint.as_deref() == Some("conversation_groups_name") {
            Self::NameTaken
        } else {
            Self::Db(e)
        }
    }
}
impl From<idempotency::IdempotencyConflict> for GroupError {
    fn from(_: idempotency::IdempotencyConflict) -> Self {
        Self::IdempotencyConflict
    }
}
fn group(r: sqlx::postgres::PgRow) -> Result<Group, sqlx::Error> {
    Ok(Group {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        position: r.try_get("position")?,
        icon: r
            .try_get::<Option<String>, _>("icon")?
            .map(|icon| serde_json::from_value(Value::String(icon)))
            .transpose()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        created_at: conversations::time(r.try_get("created_at")?),
    })
}
pub async fn list(conn: &mut PgConnection, owner: Uuid) -> Result<Vec<Group>, sqlx::Error> {
    sqlx::query("select * from conversation_groups where owner_id=$1 order by position,id")
        .bind(owner)
        .fetch_all(conn)
        .await?
        .into_iter()
        .map(group)
        .collect()
}
async fn get(conn: &mut PgConnection, owner: Uuid, id: Uuid) -> Result<Group, GroupError> {
    group(
        sqlx::query("select * from conversation_groups where owner_id=$1 and id=$2")
            .bind(owner)
            .bind(id)
            .fetch_optional(conn)
            .await?
            .ok_or(GroupError::NotFound)?,
    )
    .map_err(Into::into)
}
fn valid(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 60 && !name.contains('\0')
}
async fn emit(
    conn: &mut PgConnection,
    owner: Uuid,
    group: Value,
    correlation: Uuid,
) -> Result<(), sqlx::Error> {
    let counts: Counts = conversations::counts(conn, owner).await?;
    let groups = list(conn, owner).await?;
    conversations::append(
        conn,
        Scope::Conversations { owner_id: owner },
        "group_changed",
        None,
        json!({"group":group,"groups":groups,"counts":counts}),
        correlation,
    )
    .await
}
pub async fn create_group(
    pool: &PgPool,
    owner_id: Uuid,
    key: &str,
    name: &str,
    icon: Option<GroupIcon>,
    correlation_id: Uuid,
) -> Result<Group, GroupError> {
    let request = json!({"name":name,"icon":icon});
    let name = name.to_owned();
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "create_group",
        &request,
        None,
        move |conn| {
            Box::pin(create_group_in_tx(
                conn,
                owner_id,
                name,
                icon,
                correlation_id,
            ))
        },
    )
    .await?;
    Ok(outcome.value)
}
/// Правка группы: `None` в поле оставляет его прежним; `icon: Some(None)` — папка по умолчанию.
#[derive(Clone, Default)]
pub struct GroupChange {
    pub name: Option<String>,
    pub position: Option<i32>,
    pub icon: Option<Option<GroupIcon>>,
}
pub async fn update_group(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    change: GroupChange,
    correlation_id: Uuid,
) -> Result<Group, GroupError> {
    let request = json!({"id":id,"name":change.name,"position":change.position,"icon":change.icon});
    let outcome = idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "update_group",
        &request,
        None,
        move |conn| {
            Box::pin(update_group_in_tx(
                conn,
                owner_id,
                id,
                change.clone(),
                correlation_id,
            ))
        },
    )
    .await?;
    Ok(outcome.value)
}
pub async fn rename_group(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    name: &str,
    correlation_id: Uuid,
) -> Result<Group, GroupError> {
    let change = GroupChange {
        name: Some(name.to_owned()),
        ..GroupChange::default()
    };
    update_group(pool, owner_id, id, key, change, correlation_id).await
}
pub async fn reorder_groups(
    pool: &PgPool,
    owner_id: Uuid,
    key: &str,
    ids: Vec<Uuid>,
    correlation_id: Uuid,
) -> Result<Vec<Group>, GroupError> {
    let request = json!({"ids":ids});
    Ok(idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "reorder_groups",
        &request,
        None,
        move |conn| {
            Box::pin(async move {
                conversations::lock_owner(conn, owner_id).await?;
                let mut actual: Vec<_> = list(conn, owner_id)
                    .await?
                    .into_iter()
                    .map(|g| g.id)
                    .collect();
                actual.sort_unstable();
                let mut given = ids.clone();
                given.sort_unstable();
                if actual != given {
                    return Err(GroupError::Invalid);
                }
                for (position, id) in ids.into_iter().enumerate() {
                    sqlx::query("update conversation_groups set position=$2 where id=$1")
                        .bind(id)
                        .bind(position as i32)
                        .execute(&mut *conn)
                        .await?;
                }
                emit(conn, owner_id, Value::Null, correlation_id).await?;
                Ok(list(conn, owner_id).await?)
            })
        },
    )
    .await?
    .value)
}
pub async fn delete_group(
    pool: &PgPool,
    owner_id: Uuid,
    id: Uuid,
    key: &str,
    correlation_id: Uuid,
) -> Result<(), GroupError> {
    let request = json!({"id":id});
    idempotency::run_in_tx(
        pool,
        owner_id,
        key,
        "delete_group",
        &request,
        None,
        move |conn| Box::pin(delete_group_in_tx(conn, owner_id, id, correlation_id)),
    )
    .await?;
    Ok(())
}

async fn delete_group_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    correlation_id: Uuid,
) -> Result<(), GroupError> {
    conversations::lock_owner(conn, owner_id).await?;
    get(conn, owner_id, id).await?;
    let ids:Vec<Uuid>=sqlx::query_scalar("select c.id from conversations c join conversation_group_members gm on gm.conversation_id=c.id where gm.group_id=$1 and c.owner_id=$2 order by c.id").bind(id).bind(owner_id).fetch_all(&mut *conn).await?;
    sqlx::query("select conversation_id from queue_state where conversation_id=any($1) order by conversation_id for update").bind(&ids).fetch_all(&mut *conn).await?;
    sqlx::query("select id from conversations where id=any($1) order by id for no key update")
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?;
    sqlx::query("delete from conversation_groups where owner_id=$1 and id=$2")
        .bind(owner_id)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    let mut payloads = Vec::new();
    for &chat in &ids {
        let p = conversations::attribute_payload(conn, owner_id, chat).await?;
        conversations::append(
            conn,
            Scope::Conversation { id: chat },
            "conversation_updated",
            Some(chat),
            p.clone(),
            correlation_id,
        )
        .await?;
        payloads.push((chat, p));
    }
    for (chat, p) in payloads {
        conversations::append(
            conn,
            Scope::Conversations { owner_id },
            "conversation_updated",
            Some(chat),
            p,
            correlation_id,
        )
        .await?;
    }
    emit(conn, owner_id, Value::Null, correlation_id).await?;
    Ok::<(), GroupError>(())
}

async fn create_group_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    name: String,
    icon: Option<GroupIcon>,
    correlation_id: Uuid,
) -> Result<Group, GroupError> {
    conversations::lock_owner(conn, owner_id).await?;
    if !valid(&name) {
        return Err(GroupError::Invalid);
    }
    let id = Uuid::now_v7();
    sqlx::query("insert into conversation_groups(id,owner_id,name,position,icon) values($1,$2,$3,(select coalesce(max(position),-1)+1 from conversation_groups where owner_id=$2),$4)").bind(id).bind(owner_id).bind(name).bind(icon.map(GroupIcon::as_str)).execute(&mut *conn).await?;
    let g = get(conn, owner_id, id).await?;
    emit(conn, owner_id, json!(g), correlation_id).await?;
    Ok(g)
}

async fn update_group_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
    change: GroupChange,
    correlation_id: Uuid,
) -> Result<Group, GroupError> {
    let GroupChange {
        name,
        position,
        icon,
    } = change;
    conversations::lock_owner(conn, owner_id).await?;
    get(conn, owner_id, id).await?;
    if name.as_ref().is_some_and(|n| !valid(n)) || position.is_some_and(|p| p < 0) {
        return Err(GroupError::Invalid);
    }
    // `icon`: нет поля — значок прежний, `null` — папка по умолчанию.
    sqlx::query("update conversation_groups set name=coalesce($3,name),position=coalesce($4,position),icon=case when $5 then $6 else icon end where owner_id=$1 and id=$2").bind(owner_id).bind(id).bind(name).bind(position).bind(icon.is_some()).bind(icon.flatten().map(GroupIcon::as_str)).execute(&mut *conn).await?;
    let g = get(conn, owner_id, id).await?;
    emit(conn, owner_id, json!(g), correlation_id).await?;
    Ok(g)
}
