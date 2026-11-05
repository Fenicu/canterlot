//! Кто кого видит и когда агент должен остановиться: видимость (D12), запись чужими руками
//! (D13), частота (D15) и глубина цепочки (D16).

use chrono::{DateTime, TimeDelta, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{ToolError, ToolErrorCode, origin::Sender};
use crate::config::SessionToolsConfig;

const READ_ONLY: &str = "read_only";

/// Сессия с точки зрения инструментов.
#[derive(sqlx::FromRow)]
pub struct Session {
    pub id: Uuid,
    pub title: String,
    pub harness: String,
    pub model: String,
    pub permission_mode: String,
    pub pending_mode: Option<String>,
    pub spawn_depth: i32,
    pub archived: bool,
}

impl Session {
    pub fn sender(&self) -> Sender {
        Sender {
            id: self.id,
            title: self.title.clone(),
            harness: self.harness.clone(),
            model: self.model.clone(),
        }
    }

    /// Отправитель ограничен, если read_only уже действует или вступит со следующего хода.
    fn sends_read_only(&self) -> bool {
        self.permission_mode == READ_ONLY || self.pending_mode.as_deref() == Some(READ_ONLY)
    }

    /// Цель безопасна, только если и следующий её ход пойдёт в read_only.
    fn runs_read_only(&self) -> bool {
        self.pending_mode
            .as_deref()
            .unwrap_or(&self.permission_mode)
            == READ_ONLY
    }
}

/// Строка держится `for key share`: удаление разговора ждёт конца транзакции,
/// поэтому ввод с его названием не появится после чистки в `deletion.rs`.
/// Строки разговоров берутся по возрастанию ID до очереди и подписок:
/// docs/design/delivery.md, «Порядок блокировок инструментов сессии».
pub async fn session_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Option<Session>, sqlx::Error> {
    sqlx::query_as(
        "select id,title,harness,model,permission_mode,
                pending_settings->>'permission_mode' pending_mode,spawn_depth,
                archived_at is not null archived
         from conversations where id=$1 and owner_id=$2 and trashed_at is null for key share",
    )
    .bind(id)
    .bind(owner_id)
    .fetch_optional(conn)
    .await
}

/// Вызывающий проверен живым исполнением; пропасть он может только гонкой с удалением.
pub async fn caller_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    id: Uuid,
) -> Result<Session, ToolError> {
    session_in_tx(conn, owner_id, id)
        .await?
        .ok_or_else(|| ToolError::new(ToolErrorCode::SessionNotFound))
}

/// Идентификаторы семьи делегированной сессии; `None` у корневой: ей видно всё у владельца.
/// Корень определяет глубина, а не пустой родитель: потомок удалённого родителя остаётся
/// делегированным и шире не становится.
pub async fn family_in_tx(
    conn: &mut PgConnection,
    config: &SessionToolsConfig,
    caller: &Session,
    owner_id: Uuid,
) -> Result<Option<Vec<Uuid>>, sqlx::Error> {
    if caller.spawn_depth == 0 {
        return Ok(None);
    }
    let bound = caller.spawn_depth.max(config.max_spawn_depth as i32);
    sqlx::query_scalar(
        "with recursive
           up(id,parent,step) as (
             select id,parent_conversation_id,0 from conversations where id=$1 and owner_id=$2
             union all
             select c.id,c.parent_conversation_id,u.step+1 from conversations c
               join up u on c.id=u.parent where u.step<$3 and c.owner_id=$2),
           down(id,step) as (
             select id,0 from conversations where id=$1 and owner_id=$2
             union all
             select c.id,d.step+1 from conversations c
               join down d on c.parent_conversation_id=d.id where d.step<$3 and c.owner_id=$2)
         select id from up union select id from down",
    )
    .bind(caller.id)
    .bind(owner_id)
    .bind(bound)
    .fetch_all(conn)
    .await
    .map(Some)
}

/// Чужая и несуществующая сессия неразличимы: один и тот же код и текст.
pub async fn target_in_tx(
    conn: &mut PgConnection,
    config: &SessionToolsConfig,
    caller: &Session,
    owner_id: Uuid,
    target: Uuid,
) -> Result<Session, ToolError> {
    let missing = || ToolError::new(ToolErrorCode::SessionNotFound);
    if let Some(family) = family_in_tx(conn, config, caller, owner_id).await?
        && !family.contains(&target)
    {
        return Err(missing());
    }
    session_in_tx(conn, owner_id, target)
        .await?
        .ok_or_else(missing)
}

pub fn check_read_only(sender: &Session, target: &Session) -> Result<(), ToolError> {
    if sender.sends_read_only() && !target.runs_read_only() {
        return Err(ToolError::new(ToolErrorCode::ReadOnlySession));
    }
    Ok(())
}

/// Приём останавливает цель и потому подчиняется тому же правилу, что и запись. `force` ещё и
/// выбрасывает незакоммиченную работу, а она могла появиться до того, как цель сузили.
pub fn check_accept(sender: &Session, target: &Session, force: bool) -> Result<(), ToolError> {
    if force && sender.sends_read_only() {
        return Err(ToolError::new(ToolErrorCode::ReadOnlySession));
    }
    check_read_only(sender, target)
}

/// Глубина хода, из которого пришёл вызов. Ввод в слоте учитывается и без квитанции о приёме:
/// вызов инструмента может обогнать её. Без отметки о происхождении ввод владельца: ноль.
pub async fn current_depth(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<i32, sqlx::Error> {
    let depth: Option<i32> = sqlx::query_scalar(
        "select coalesce((i.origin->>'chain_depth')::int,0) from inputs i
         where i.conversation_id=$1
           and (i.state in ('accepted','acceptance_unconfirmed')
                or i.id=(select run_slot_input_id from queue_state where conversation_id=$1))
         order by i.input_seq desc limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(conn)
    .await?;
    Ok(depth.unwrap_or(0))
}

pub fn check_depth(config: &SessionToolsConfig, depth: i32) -> Result<(), ToolError> {
    if depth > config.max_chain_depth as i32 {
        return Err(ToolError::new(ToolErrorCode::ChainDepthExceeded));
    }
    Ok(())
}

fn window_start(now: DateTime<Utc>, window: std::time::Duration) -> DateTime<Utc> {
    now - TimeDelta::from_std(window).unwrap_or_else(|_| TimeDelta::days(365))
}

/// Запуски родителя за окно: созданные сессии и карточки, которые сессию не создали.
pub async fn check_spawn_rate(
    conn: &mut PgConnection,
    config: &SessionToolsConfig,
    parent: Uuid,
    now: DateTime<Utc>,
) -> Result<(), ToolError> {
    let since = window_start(now, config.spawn_window);
    let spawns: i64 = sqlx::query_scalar(
        "select (select count(*) from conversations
                  where parent_conversation_id=$1 and created_at>$2)
              + (select count(*) from requests
                  where conversation_id=$1 and kind='spawn_approval' and created_at>$2
                    and not (state='decision_applied' and decision_payload->>'behavior'='allow'))",
    )
    .bind(parent)
    .bind(since)
    .fetch_one(conn)
    .await?;
    if spawns >= i64::from(config.parent_spawns) {
        return Err(ToolError::new(ToolErrorCode::RateLimited));
    }
    Ok(())
}

/// События подписки не считаются: иначе последовательный оркестратор упёрся бы в свои же итоги.
pub async fn check_rate(
    conn: &mut PgConnection,
    config: &SessionToolsConfig,
    sender: Uuid,
    target: Uuid,
    now: DateTime<Utc>,
) -> Result<(), ToolError> {
    let (pair, all): (i64, i64) = sqlx::query_as(
        "select count(*) filter(where conversation_id=$2 and received_at>$3),
                count(*) filter(where received_at>$4)
         from inputs
         where origin->>'from'=$1 and received_at>least($3,$4)
           and origin is not null and origin->>'kind'<>$5",
    )
    .bind(sender.to_string())
    .bind(target)
    .bind(window_start(now, config.pair_window))
    .bind(window_start(now, config.sender_window))
    .bind(super::origin::KIND_EVENT)
    .fetch_one(conn)
    .await?;
    if pair >= i64::from(config.pair_sends) || all >= i64::from(config.sender_sends) {
        return Err(ToolError::new(ToolErrorCode::RateLimited));
    }
    Ok(())
}
