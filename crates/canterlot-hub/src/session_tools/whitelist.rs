//! Пары «харнесс, модель», которые агент запускает без вопроса владельцу. Менять список
//! может только владелец через HTTP: у инструментов агента пути сюда на запись нет.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

/// Модель записи, значащая первую модель каталога на момент запуска.
pub const DEFAULT_MODEL: &str = "default";
const MAX_ENTRIES: usize = 64;
const MAX_MODEL_CHARS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WhitelistEntry {
    pub harness: String,
    pub model: String,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct WhitelistView {
    pub entries: Vec<WhitelistEntry>,
    pub is_default: bool,
    pub default_entries: Vec<WhitelistEntry>,
}

pub fn default_entries() -> Vec<WhitelistEntry> {
    [
        ("claude_code", "sonnet[1m]"),
        ("claude_code", "haiku"),
        ("codex", DEFAULT_MODEL),
        ("opencode", "opencode-go/muse-spark-1.3-contributor"),
    ]
    .map(|(harness, model)| WhitelistEntry {
        harness: harness.to_owned(),
        model: model.to_owned(),
    })
    .to_vec()
}

/// Модели с каталогом не сверяются: машина может быть не в сети.
pub fn valid(entries: &[WhitelistEntry]) -> bool {
    entries.len() <= MAX_ENTRIES
        && entries.iter().enumerate().all(|(n, entry)| {
            canterlot_proto::harness::provider_of(&entry.harness).is_some()
                && !entry.model.is_empty()
                && !entry.model.contains('\0')
                && entry.model.chars().count() <= MAX_MODEL_CHARS
                && !entries[..n].contains(entry)
        })
}

async fn stored(conn: &mut PgConnection, owner_id: Uuid) -> Result<Option<Value>, sqlx::Error> {
    sqlx::query_scalar("select spawn_whitelist from owners where id=$1")
        .bind(owner_id)
        .fetch_one(conn)
        .await
}

pub async fn view(conn: &mut PgConnection, owner_id: Uuid) -> Result<WhitelistView, sqlx::Error> {
    let defaults = default_entries();
    let (entries, is_default) = match stored(conn, owner_id).await? {
        Some(entries) => (
            serde_json::from_value(entries)
                .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
            false,
        ),
        None => (defaults.clone(), true),
    };
    Ok(WhitelistView {
        entries,
        is_default,
        default_entries: defaults,
    })
}

/// `None` возвращает встроенный список.
pub async fn replace(
    conn: &mut PgConnection,
    owner_id: Uuid,
    entries: Option<&[WhitelistEntry]>,
) -> Result<(), sqlx::Error> {
    sqlx::query("update owners set spawn_whitelist=$2 where id=$1")
        .bind(owner_id)
        .bind(entries.map(sqlx::types::Json))
        .execute(conn)
        .await?;
    Ok(())
}

/// `first_model` нужна для записей `default`.
pub fn allows(entries: &[WhitelistEntry], harness: &str, model: &str, first_model: &str) -> bool {
    entries.iter().any(|entry| {
        entry.harness == harness
            && (entry.model == model || (entry.model == DEFAULT_MODEL && model == first_model))
    })
}
