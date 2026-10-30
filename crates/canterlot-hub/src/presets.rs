//! Пресеты инструкций агента (план 2c, задача 2).
//!
//! Пресет — именованный текст инструкций, хранящийся в хабе. Встроенные пресеты
//! `clean` («Без инструкций») и `owner` («Как у владельца») — это код, а не строки:
//! у них нет текста, ревизии и объёма. Пользовательские пресеты хранят каждую версию
//! текста в `instruction_preset_revisions`; номер версии всегда называет один текст,
//! поэтому правки никогда не меняют идущие сессии. Текст пресета — данные уровня
//! сообщения: он есть только в таблице версий, в командных payload, в ответе
//! `GET /api/v1/presets/{id}` и в ответе чтения агента; в журналах, `Debug`,
//! событиях, снимках и списках его нет.

use crate::{conversations, idempotency};
use canterlot_proto::limits::MAX_PRESET_TEXT_BYTES;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Row};
use utoipa::ToSchema;
use uuid::Uuid;

/// Максимальная длина названия пресета в символах.
pub const MAX_PRESET_TITLE_CHARS: usize = 80;
/// Максимальная длина описания пресета в символах.
pub const MAX_PRESET_DESCRIPTION_CHARS: usize = 500;
/// Максимум пользовательских пресетов одного владельца (без удалённых).
pub const MAX_PRESETS_PER_OWNER: i64 = 100;

/// Встроенный пресет «Без инструкций».
pub const BUILTIN_CLEAN_ID: &str = "clean";
/// Встроенный пресет «Как у владельца».
pub const BUILTIN_OWNER_ID: &str = "owner";
/// Название встроенного пресета «Без инструкций».
pub const BUILTIN_CLEAN_TITLE: &str = "Без инструкций";
/// Название встроенного пресета «Как у владельца».
pub const BUILTIN_OWNER_TITLE: &str = "Как у владельца";
/// Описание встроенного пресета «Без инструкций».
pub const BUILTIN_CLEAN_DESCRIPTION: &str = "Агент запускается без личных инструкций владельца: без его глобальных CLAUDE.md и AGENTS.md и без автопамяти. Инструкции самого репозитория, вход, MCP-серверы и плагины владельца остаются.";
/// Описание встроенного пресета «Как у владельца».
pub const BUILTIN_OWNER_DESCRIPTION: &str =
    "Конфигурация агента владельца без изменений: его CLAUDE.md, AGENTS.md и автопамять.";

/// Ссылка на пресет: встроенный идентификатор или UUID пользовательского.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PresetRef {
    Clean,
    Owner,
    Custom(Uuid),
}

impl PresetRef {
    /// Разбирает `clean`, `owner` или UUID; неизвестное значение — `NotFound`.
    pub fn parse(value: &str) -> Result<Self, PresetError> {
        match value {
            BUILTIN_CLEAN_ID => Ok(Self::Clean),
            BUILTIN_OWNER_ID => Ok(Self::Owner),
            _ => value
                .parse()
                .map(Self::Custom)
                .map_err(|_| PresetError::NotFound),
        }
    }

    pub fn as_str(&self) -> String {
        match self {
            Self::Clean => BUILTIN_CLEAN_ID.to_owned(),
            Self::Owner => BUILTIN_OWNER_ID.to_owned(),
            Self::Custom(id) => id.to_string(),
        }
    }
}

/// Вид пресета в списках и событиях.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PresetKind {
    Clean,
    Owner,
    Custom,
}

/// Кто создал пресет или внёс последнюю правку; текст правки не несёт.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PresetOrigin {
    /// `builtin`, `owner` или `agent`.
    pub by: String,
    pub conversation_id: Option<Uuid>,
    pub conversation_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// Строка списка, события и снимка: текста пресета здесь нет.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PresetSummary {
    /// `clean`, `owner` или UUID пользовательского пресета.
    pub id: String,
    pub kind: PresetKind,
    pub title: String,
    pub description: String,
    pub revision: Option<i64>,
    pub text_bytes: Option<i64>,
    pub created: PresetOrigin,
    pub last_edit: Option<PresetOrigin>,
    pub used_by: i64,
}

/// Детальный ответ: текст только у пользовательских пресетов.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Preset {
    #[serde(flatten)]
    pub summary: PresetSummary,
    pub text: Option<String>,
}

impl std::fmt::Debug for Preset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preset")
            .field("summary", &self.summary)
            .field("text_bytes", &self.text.as_ref().map(|t| t.len()))
            .finish()
    }
}

/// Тело создания пресета.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewPreset {
    pub title: String,
    pub description: Option<String>,
    pub text: String,
}

/// Тело правки пресета: `base_revision` обязателен для оптимистичной блокировки.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub text: Option<String>,
    pub base_revision: i64,
}

/// Кто выполняет изменение: владелец из PWA или агент из своей сессии.
#[derive(Clone, Copy)]
pub enum Actor {
    Owner,
    Agent { conversation_id: Uuid },
}

impl Actor {
    fn origin(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Agent { .. } => "agent",
        }
    }

    fn conversation_id(&self) -> Option<Uuid> {
        match self {
            Self::Owner => None,
            Self::Agent { conversation_id } => Some(*conversation_id),
        }
    }
}

/// Применённый пресет в эффективных настройках: ссылка, название и версии.
/// Текста здесь нет — только снимок запуска везёт текст.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AppliedPreset {
    pub id: String,
    pub kind: PresetKind,
    pub title: String,
    pub revision: Option<i64>,
    pub current_revision: Option<i64>,
    pub deleted: bool,
}

/// Разрешённая ссылка на пресет для снимка запуска (задача 3); текст — данные сообщения.
pub struct Resolved {
    pub mode: &'static str,
    pub preset_id: Option<Uuid>,
    pub revision: Option<i64>,
    pub title: String,
    pub text: Option<String>,
}

impl std::fmt::Debug for Resolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolved")
            .field("mode", &self.mode)
            .field("preset_id", &self.preset_id)
            .field("revision", &self.revision)
            .field("title_bytes", &self.title.len())
            .field("text_bytes", &self.text.as_ref().map(|t| t.len()))
            .finish()
    }
}

#[derive(thiserror::Error)]
pub enum PresetError {
    #[error("not found")]
    NotFound,
    #[error("preset is builtin")]
    Builtin,
    #[error("preset revision conflict")]
    RevisionConflict { current: i64 },
    #[error("preset title taken")]
    TitleTaken,
    #[error("preset limit reached")]
    LimitReached,
    #[error("invalid preset")]
    Invalid { field: &'static str },
    #[error("idempotency conflict")]
    IdempotencyConflict,
    #[error("database error")]
    Db(#[from] sqlx::Error),
}

impl std::fmt::Debug for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl From<idempotency::IdempotencyConflict> for PresetError {
    fn from(_: idempotency::IdempotencyConflict) -> Self {
        Self::IdempotencyConflict
    }
}

/// Стартовый пресет исполнителя (решение владельца 29.10, late): обычный
/// пользовательский пресет, редактируемый и удаляемый; не default для новых сессий.
pub struct SeedPreset {
    pub title: &'static str,
    pub description: &'static str,
    pub text: &'static str,
}

pub const SEED_EXECUTOR_EN: SeedPreset = SeedPreset {
    title: "Исполнитель (английский)",
    description: "Общение, рассуждения и отчёты на английском; язык коммитов, интерфейса и документации задаёт задача.",
    text: "Communicate, reason and write reports in English. Artefacts whose language the task fixes (commit messages, UI strings, documentation) follow the task.",
};

fn clean_builtin(used_by: i64) -> PresetSummary {
    PresetSummary {
        id: BUILTIN_CLEAN_ID.to_owned(),
        kind: PresetKind::Clean,
        title: BUILTIN_CLEAN_TITLE.to_owned(),
        description: BUILTIN_CLEAN_DESCRIPTION.to_owned(),
        revision: None,
        text_bytes: None,
        created: PresetOrigin {
            by: "builtin".to_owned(),
            conversation_id: None,
            conversation_title: None,
            at: None,
        },
        last_edit: None,
        used_by,
    }
}

fn owner_builtin(used_by: i64) -> PresetSummary {
    PresetSummary {
        id: BUILTIN_OWNER_ID.to_owned(),
        kind: PresetKind::Owner,
        title: BUILTIN_OWNER_TITLE.to_owned(),
        description: BUILTIN_OWNER_DESCRIPTION.to_owned(),
        revision: None,
        text_bytes: None,
        created: PresetOrigin {
            by: "builtin".to_owned(),
            conversation_id: None,
            conversation_title: None,
            at: None,
        },
        last_edit: None,
        used_by,
    }
}

fn valid_title(title: &str) -> bool {
    let count = title.chars().count();
    (1..=MAX_PRESET_TITLE_CHARS).contains(&count) && !title.chars().any(|c| c.is_control())
}

fn is_builtin_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    lower == BUILTIN_CLEAN_TITLE.to_lowercase() || lower == BUILTIN_OWNER_TITLE.to_lowercase()
}

async fn used_by(
    conn: &mut PgConnection,
    owner: Uuid,
    reference: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*) from conversations where owner_id=$1 and archived_at is null
         and (instruction_profile=$2 or pending_settings->>'instruction_profile'=$2)",
    )
    .bind(owner)
    .bind(reference)
    .fetch_one(conn)
    .await
}

async fn conversation_title(
    conn: &mut PgConnection,
    id: Option<Uuid>,
) -> Result<Option<String>, sqlx::Error> {
    let Some(id) = id else { return Ok(None) };
    sqlx::query_scalar("select title from conversations where id=$1")
        .bind(id)
        .fetch_optional(conn)
        .await
}

async fn summarize(
    conn: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
) -> Result<Option<PresetSummary>, sqlx::Error> {
    let row = sqlx::query(
        "select id, title, description, revision, origin, origin_conversation_id, created_at
         from instruction_presets where owner_id=$1 and id=$2 and deleted_at is null",
    )
    .bind(owner)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let revision: i64 = row.try_get("revision")?;
    let rev: (String, Option<Uuid>, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "select edited_by, edited_conversation_id, created_at from instruction_preset_revisions
         where preset_id=$1 and revision=$2",
    )
    .bind(id)
    .bind(revision)
    .fetch_one(&mut *conn)
    .await?;
    let text_bytes: i64 = sqlx::query_scalar(
        "select octet_length(text)::bigint from instruction_preset_revisions where preset_id=$1 and revision=$2",
    )
    .bind(id)
    .bind(revision)
    .fetch_one(&mut *conn)
    .await?;
    let origin: String = row.try_get("origin")?;
    let origin_conversation: Option<Uuid> = row.try_get("origin_conversation_id")?;
    let created_at: chrono::DateTime<chrono::Utc> = row.try_get("created_at")?;
    let last_edit = if revision > 1 {
        Some(PresetOrigin {
            by: rev.0,
            conversation_id: rev.1,
            conversation_title: conversation_title(conn, rev.1).await?,
            at: Some(conversations::time(rev.2)),
        })
    } else {
        None
    };
    let reference = id.to_string();
    Ok(Some(PresetSummary {
        id: reference.clone(),
        kind: PresetKind::Custom,
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        revision: Some(revision),
        text_bytes: Some(text_bytes),
        created: PresetOrigin {
            by: origin,
            conversation_id: origin_conversation,
            conversation_title: conversation_title(conn, origin_conversation).await?,
            at: Some(conversations::time(created_at)),
        },
        last_edit,
        used_by: used_by(conn, owner, &reference).await?,
    }))
}

async fn current_text(
    conn: &mut PgConnection,
    id: Uuid,
    revision: i64,
) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "select text from instruction_preset_revisions where preset_id=$1 and revision=$2",
    )
    .bind(id)
    .bind(revision)
    .fetch_one(conn)
    .await
}

fn emit_payload(summary: &PresetSummary, deleted: bool) -> Value {
    json!({"preset": summary, "deleted": deleted})
}

async fn emit(
    conn: &mut PgConnection,
    owner: Uuid,
    summary: &PresetSummary,
    deleted: bool,
    correlation: Uuid,
) -> Result<(), sqlx::Error> {
    conversations::append(
        conn,
        crate::events::Scope::Conversations { owner_id: owner },
        "instruction_preset_changed",
        None,
        emit_payload(summary, deleted),
        correlation,
    )
    .await
}

/// Список: встроенные первыми, затем пользовательские по `lower(title)`.
pub async fn list(conn: &mut PgConnection, owner: Uuid) -> Result<Vec<PresetSummary>, sqlx::Error> {
    let mut out = vec![
        clean_builtin(used_by(conn, owner, BUILTIN_CLEAN_ID).await?),
        owner_builtin(used_by(conn, owner, BUILTIN_OWNER_ID).await?),
    ];
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select id from instruction_presets where owner_id=$1 and deleted_at is null order by lower(title), id",
    )
    .bind(owner)
    .fetch_all(&mut *conn)
    .await?;
    for id in ids {
        if let Some(summary) = summarize(conn, owner, id).await? {
            out.push(summary);
        }
    }
    Ok(out)
}

/// Детальный ответ; удалённый или чужой пресет — `NotFound`.
pub async fn get(
    conn: &mut PgConnection,
    owner: Uuid,
    reference: &PresetRef,
) -> Result<Preset, PresetError> {
    match reference {
        PresetRef::Clean => Ok(Preset {
            summary: clean_builtin(used_by(conn, owner, BUILTIN_CLEAN_ID).await?),
            text: None,
        }),
        PresetRef::Owner => Ok(Preset {
            summary: owner_builtin(used_by(conn, owner, BUILTIN_OWNER_ID).await?),
            text: None,
        }),
        PresetRef::Custom(id) => {
            let summary = summarize(conn, owner, *id)
                .await?
                .ok_or(PresetError::NotFound)?;
            let text = current_text(conn, *id, summary.revision.unwrap_or(1)).await?;
            Ok(Preset {
                summary,
                text: Some(text),
            })
        }
    }
}

fn validate_new(
    title: &str,
    description: &str,
    text: &str,
) -> Result<(String, String), PresetError> {
    let title = title.trim().to_owned();
    if !valid_title(&title) {
        return Err(PresetError::Invalid { field: "title" });
    }
    if is_builtin_title(&title) {
        return Err(PresetError::TitleTaken);
    }
    if description.chars().count() > MAX_PRESET_DESCRIPTION_CHARS || description.contains('\0') {
        return Err(PresetError::Invalid {
            field: "description",
        });
    }
    if text.trim().is_empty() || text.contains('\0') {
        return Err(PresetError::Invalid { field: "text" });
    }
    if text.len() > MAX_PRESET_TEXT_BYTES {
        return Err(PresetError::Invalid { field: "text" });
    }
    Ok((title, description.to_owned()))
}

async fn title_taken(
    conn: &mut PgConnection,
    owner: Uuid,
    title: &str,
    except: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    if is_builtin_title(title) {
        return Ok(true);
    }
    let count: i64 = sqlx::query_scalar(
        "select count(*) from instruction_presets
         where owner_id=$1 and deleted_at is null and lower(title)=lower($2)
           and ($3::uuid is null or id<>$3)",
    )
    .bind(owner)
    .bind(title)
    .bind(except)
    .fetch_one(conn)
    .await?;
    Ok(count > 0)
}

async fn create_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    body: NewPreset,
    actor: Actor,
    correlation: Uuid,
) -> Result<Preset, PresetError> {
    conversations::lock_owner(conn, owner).await?;
    let (title, description) = validate_new(
        &body.title,
        body.description.as_deref().unwrap_or(""),
        &body.text,
    )?;
    if title_taken(conn, owner, &title, None).await? {
        return Err(PresetError::TitleTaken);
    }
    let count: i64 = sqlx::query_scalar(
        "select count(*) from instruction_presets where owner_id=$1 and deleted_at is null",
    )
    .bind(owner)
    .fetch_one(&mut *conn)
    .await?;
    if count >= MAX_PRESETS_PER_OWNER {
        return Err(PresetError::LimitReached);
    }
    let id = Uuid::now_v7();
    let origin = actor.origin();
    let origin_conversation = actor.conversation_id();
    sqlx::query(
        "insert into instruction_presets(id,owner_id,title,description,revision,origin,origin_conversation_id)
         values($1,$2,$3,$4,1,$5,$6)",
    )
    .bind(id)
    .bind(owner)
    .bind(&title)
    .bind(&description)
    .bind(origin)
    .bind(origin_conversation)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "insert into instruction_preset_revisions(preset_id,revision,text,edited_by,edited_conversation_id)
         values($1,1,$2,$3,$4)",
    )
    .bind(id)
    .bind(&body.text)
    .bind(origin)
    .bind(origin_conversation)
    .execute(&mut *conn)
    .await?;
    let summary = summarize(conn, owner, id)
        .await?
        .ok_or(PresetError::NotFound)?;
    emit(conn, owner, &summary, false, correlation).await?;
    Ok(Preset {
        summary,
        text: Some(body.text),
    })
}

/// Создание с ключом идемпотентности; повтор возвращает сохранённый ответ.
pub async fn create(
    pool: &PgPool,
    owner: Uuid,
    key: &str,
    body: NewPreset,
    actor: Actor,
    correlation: Uuid,
) -> Result<Preset, PresetError> {
    let request = json!({"title": body.title, "description": body.description, "text_sha256": text_digest(&body.text)});
    let actor_conversation = actor.conversation_id();
    let outcome = idempotency::run_in_tx(
        pool,
        owner,
        key,
        "create_preset",
        &request,
        actor_conversation,
        move |conn| {
            let body = NewPreset {
                title: body.title.clone(),
                description: body.description.clone(),
                text: body.text.clone(),
            };
            Box::pin(async move {
                without_text(create_in_tx(conn, owner, body, actor, correlation).await)
            })
        },
    )
    .await?;
    with_text(pool, outcome.value).await
}

/// Отпечаток текста для ключа идемпотентности: длины мало, чтобы отличить правку.
fn text_digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Сохранённый ответ ключа идемпотентности не держит текст пресета (хранится 7 дней
/// и пережил бы удаление пресета); текст берётся из строки версии.
fn without_text(result: Result<Preset, PresetError>) -> Result<Preset, PresetError> {
    result.map(|preset| Preset {
        text: None,
        ..preset
    })
}

async fn with_text(pool: &PgPool, mut preset: Preset) -> Result<Preset, PresetError> {
    let id = Uuid::parse_str(&preset.summary.id).map_err(|_| PresetError::NotFound)?;
    let revision = preset.summary.revision.ok_or(PresetError::NotFound)?;
    preset.text = Some(current_text(&mut *pool.acquire().await?, id, revision).await?);
    Ok(preset)
}

async fn update_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
    patch: PresetPatch,
    actor: Actor,
    correlation: Uuid,
) -> Result<Preset, PresetError> {
    conversations::lock_owner(conn, owner).await?;
    let row = sqlx::query(
        "select revision, title, description from instruction_presets
         where owner_id=$1 and id=$2 and deleted_at is null",
    )
    .bind(owner)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(PresetError::NotFound)?;
    let current: i64 = row.try_get("revision")?;
    if patch.base_revision != current {
        return Err(PresetError::RevisionConflict { current });
    }
    let current_text_value = current_text(conn, id, current).await?;
    let title: String = row.try_get("title")?;
    let description: String = row.try_get("description")?;
    let new_title = patch.title.as_deref().unwrap_or(&title).trim().to_owned();
    if !valid_title(&new_title) {
        return Err(PresetError::Invalid { field: "title" });
    }
    if title_taken(conn, owner, &new_title, Some(id)).await? {
        return Err(PresetError::TitleTaken);
    }
    let new_description = patch
        .description
        .as_deref()
        .unwrap_or(&description)
        .to_owned();
    if new_description.chars().count() > MAX_PRESET_DESCRIPTION_CHARS
        || new_description.contains('\0')
    {
        return Err(PresetError::Invalid {
            field: "description",
        });
    }
    let new_text = patch
        .text
        .as_deref()
        .unwrap_or(&current_text_value)
        .to_owned();
    if patch.text.as_ref().is_some_and(|t| t.trim().is_empty())
        || new_text.trim().is_empty()
        || new_text.contains('\0')
    {
        return Err(PresetError::Invalid { field: "text" });
    }
    if new_text.len() > MAX_PRESET_TEXT_BYTES {
        return Err(PresetError::Invalid { field: "text" });
    }
    let next = current + 1;
    sqlx::query(
        "update instruction_presets set title=$3, description=$4, revision=$5, updated_at=clock_timestamp()
         where owner_id=$1 and id=$2",
    )
    .bind(owner)
    .bind(id)
    .bind(&new_title)
    .bind(&new_description)
    .bind(next)
    .execute(&mut *conn)
    .await?;
    // Номер версии всегда называет один текст: строка версии пишется и при правке
    // только названия или описания.
    sqlx::query(
        "insert into instruction_preset_revisions(preset_id,revision,text,edited_by,edited_conversation_id)
         values($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(next)
    .bind(&new_text)
    .bind(actor.origin())
    .bind(actor.conversation_id())
    .execute(&mut *conn)
    .await?;
    let summary = summarize(conn, owner, id)
        .await?
        .ok_or(PresetError::NotFound)?;
    emit(conn, owner, &summary, false, correlation).await?;
    Ok(Preset {
        summary,
        text: Some(new_text),
    })
}

/// Правка с ключом идемпотентности; `base_revision` обязан совпадать с текущей версией.
pub async fn update(
    pool: &PgPool,
    owner: Uuid,
    id: Uuid,
    key: &str,
    patch: PresetPatch,
    actor: Actor,
    correlation: Uuid,
) -> Result<Preset, PresetError> {
    let request = json!({"id": id, "title": patch.title, "description": patch.description,
        "text_sha256": patch.text.as_deref().map(text_digest), "base_revision": patch.base_revision});
    let actor_conversation = actor.conversation_id();
    let outcome = idempotency::run_in_tx(
        pool,
        owner,
        key,
        "update_preset",
        &request,
        actor_conversation,
        move |conn| {
            let patch = PresetPatch {
                title: patch.title.clone(),
                description: patch.description.clone(),
                text: patch.text.clone(),
                base_revision: patch.base_revision,
            };
            Box::pin(async move {
                without_text(update_in_tx(conn, owner, id, patch, actor, correlation).await)
            })
        },
    )
    .await?;
    with_text(pool, outcome.value).await
}

/// Мягкое удаление: строки и версии остаются, идущие сессии держат свою копию.
pub async fn delete(
    pool: &PgPool,
    owner: Uuid,
    id: Uuid,
    key: &str,
    correlation: Uuid,
) -> Result<(), PresetError> {
    let request = json!({"id": id});
    idempotency::run_in_tx(
        pool,
        owner,
        key,
        "delete_preset",
        &request,
        None,
        move |conn| Box::pin(delete_in_tx(conn, owner, id, correlation)),
    )
    .await?;
    Ok(())
}

async fn delete_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    id: Uuid,
    correlation: Uuid,
) -> Result<(), PresetError> {
    conversations::lock_owner(conn, owner).await?;
    let exists: bool = sqlx::query_scalar(
        "select exists(select 1 from instruction_presets where owner_id=$1 and id=$2 and deleted_at is null)",
    )
    .bind(owner)
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    if !exists {
        return Err(PresetError::NotFound);
    }
    sqlx::query(
        "update instruction_presets set deleted_at=clock_timestamp() where owner_id=$1 and id=$2",
    )
    .bind(owner)
    .bind(id)
    .execute(&mut *conn)
    .await?;
    let row = sqlx::query(
        "select title, description, revision, origin, origin_conversation_id, created_at
         from instruction_presets where owner_id=$1 and id=$2",
    )
    .bind(owner)
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    clear_pending_references_in_tx(conn, owner, &id.to_string(), correlation).await?;
    let revision: i64 = row.try_get("revision")?;
    let rev: (String, Option<Uuid>, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "select edited_by, edited_conversation_id, created_at from instruction_preset_revisions
         where preset_id=$1 and revision=$2",
    )
    .bind(id)
    .bind(revision)
    .fetch_one(&mut *conn)
    .await?;
    let reference = id.to_string();
    let summary = PresetSummary {
        id: reference.clone(),
        kind: PresetKind::Custom,
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        revision: Some(revision),
        text_bytes: None,
        created: PresetOrigin {
            by: row.try_get("origin")?,
            conversation_id: row.try_get("origin_conversation_id")?,
            conversation_title: None,
            at: Some(conversations::time(row.try_get("created_at")?)),
        },
        last_edit: Some(PresetOrigin {
            by: rev.0,
            conversation_id: rev.1,
            conversation_title: None,
            at: Some(conversations::time(rev.2)),
        }),
        used_by: used_by(conn, owner, &reference).await?,
    };
    // Событие списка последним: счётчик owner:{id}:conversations идёт после разговоров.
    emit(conn, owner, &summary, true, correlation).await?;
    Ok(())
}

/// Удаление пресета снимает висячие ссылки в pending_settings сессий владельца;
/// сами сессии продолжают работать со своей закреплённой копией.
async fn clear_pending_references_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    reference: &str,
    correlation: Uuid,
) -> Result<(), PresetError> {
    // Порядок хаба: очереди, затем строки разговоров по возрастанию ID, затем события.
    let candidates: Vec<Uuid> = sqlx::query_scalar(
        "select id from conversations where owner_id=$1 and pending_settings->>'instruction_profile'=$2 order by id",
    )
    .bind(owner)
    .bind(reference)
    .fetch_all(&mut *conn)
    .await?;
    sqlx::query(
        "select conversation_id from queue_state where conversation_id = any($1) order by conversation_id for update",
    )
    .bind(&candidates)
    .execute(&mut *conn)
    .await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select id from conversations where id = any($1) and pending_settings->>'instruction_profile'=$2 order by id for no key update",
    )
    .bind(&candidates)
    .bind(reference)
    .fetch_all(&mut *conn)
    .await?;
    for id in ids {
        sqlx::query("update conversations set pending_settings=pending_settings-'instruction_profile' where id=$1")
            .bind(id)
            .execute(&mut *conn)
            .await?;
        let view = crate::conversations::detail(conn, owner, id)
            .await?
            .ok_or(PresetError::NotFound)?
            .settings;
        let mut payload =
            serde_json::to_value(&view).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
        payload["rejected"] =
            serde_json::json!({"keys":["instruction_profile"],"reason":"preset_deleted"});
        crate::conversations::append(
            conn,
            crate::events::Scope::Conversation { id },
            "conversation_settings_changed",
            Some(id),
            payload,
            correlation,
        )
        .await?;
    }
    Ok(())
}

/// Разрешение ссылки для снимка запуска (задача 3); работает и для удалённых пресетов.
pub async fn resolve_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    reference: &PresetRef,
    revision: Option<i64>,
) -> Result<Resolved, PresetError> {
    match reference {
        PresetRef::Clean => Ok(Resolved {
            mode: "clean",
            preset_id: None,
            revision: None,
            title: BUILTIN_CLEAN_TITLE.to_owned(),
            text: None,
        }),
        PresetRef::Owner => Ok(Resolved {
            mode: "owner",
            preset_id: None,
            revision: None,
            title: BUILTIN_OWNER_TITLE.to_owned(),
            text: None,
        }),
        PresetRef::Custom(id) => {
            let row = sqlx::query(
                "select title, revision from instruction_presets where owner_id=$1 and id=$2",
            )
            .bind(owner)
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(PresetError::NotFound)?;
            let current: i64 = row.try_get("revision")?;
            let wanted = revision.unwrap_or(current);
            let text: String = sqlx::query_scalar(
                "select text from instruction_preset_revisions where preset_id=$1 and revision=$2",
            )
            .bind(id)
            .bind(wanted)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(PresetError::NotFound)?;
            Ok(Resolved {
                mode: "preset",
                preset_id: Some(*id),
                revision: Some(wanted),
                title: row.try_get("title")?,
                text: Some(text),
            })
        }
    }
}

/// Применённый пресет для эффективных настроек: название и версии без текста.
pub async fn applied_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    reference: &str,
    pinned: Option<i64>,
) -> Result<AppliedPreset, PresetError> {
    let parsed = PresetRef::parse(reference)?;
    match parsed {
        PresetRef::Clean => Ok(AppliedPreset {
            id: BUILTIN_CLEAN_ID.to_owned(),
            kind: PresetKind::Clean,
            title: BUILTIN_CLEAN_TITLE.to_owned(),
            revision: None,
            current_revision: None,
            deleted: false,
        }),
        PresetRef::Owner => Ok(AppliedPreset {
            id: BUILTIN_OWNER_ID.to_owned(),
            kind: PresetKind::Owner,
            title: BUILTIN_OWNER_TITLE.to_owned(),
            revision: None,
            current_revision: None,
            deleted: false,
        }),
        PresetRef::Custom(id) => {
            let row = sqlx::query(
                "select title, revision, deleted_at is not null as gone from instruction_presets where owner_id=$1 and id=$2",
            )
            .bind(owner)
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
            match row {
                None => Ok(AppliedPreset {
                    id: reference.to_owned(),
                    kind: PresetKind::Custom,
                    title: reference.to_owned(),
                    revision: pinned,
                    current_revision: None,
                    deleted: true,
                }),
                Some(row) => {
                    let gone: bool = row.try_get("gone")?;
                    Ok(AppliedPreset {
                        id: reference.to_owned(),
                        kind: PresetKind::Custom,
                        title: row.try_get("title")?,
                        revision: pinned,
                        current_revision: (!gone).then(|| row.try_get("revision")).transpose()?,
                        deleted: gone,
                    })
                }
            }
        }
    }
}

/// Посев стартового пресета исполнителя: один обычный пресет на владельца.
/// Повторный вызов и вызов после удаления или переименования посева — no-op,
/// но метка `presets_seeded_at` выставляется всегда.
pub async fn seed_in_tx(conn: &mut PgConnection, owner: Uuid) -> Result<(), PresetError> {
    // Путь обновления: init может выполняться на схеме старше пресетов
    // (миграции накатываются позже при старте serve); тогда посев пропускается
    // и выполняется через seed_all_missing после миграций.
    let ready: bool = sqlx::query_scalar(
        "select exists(select 1 from information_schema.tables
          where table_schema='public' and table_name='instruction_presets')
         and exists(select 1 from information_schema.columns
          where table_schema='public' and table_name='owners' and column_name='presets_seeded_at')",
    )
    .fetch_one(&mut *conn)
    .await?;
    if !ready {
        return Ok(());
    }
    let seeded: bool =
        sqlx::query_scalar("select presets_seeded_at is not null from owners where id=$1")
            .bind(owner)
            .fetch_one(&mut *conn)
            .await?;
    if seeded {
        return Ok(());
    }
    let clash: bool = sqlx::query_scalar(
        "select exists(select 1 from instruction_presets
         where owner_id=$1 and deleted_at is null and lower(title)=lower($2))",
    )
    .bind(owner)
    .bind(SEED_EXECUTOR_EN.title)
    .fetch_one(&mut *conn)
    .await?;
    if !clash {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into instruction_presets(id,owner_id,title,description,revision,origin)
             values($1,$2,$3,$4,1,'owner')",
        )
        .bind(id)
        .bind(owner)
        .bind(SEED_EXECUTOR_EN.title)
        .bind(SEED_EXECUTOR_EN.description)
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "insert into instruction_preset_revisions(preset_id,revision,text,edited_by)
             values($1,1,$2,'owner')",
        )
        .bind(id)
        .bind(SEED_EXECUTOR_EN.text)
        .execute(&mut *conn)
        .await?;
    }
    sqlx::query("update owners set presets_seeded_at=clock_timestamp() where id=$1")
        .bind(owner)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Посев для владельцев, созданных до появления пресетов (путь запуска хаба).
pub async fn seed_all_missing(pool: &PgPool) -> Result<(), PresetError> {
    let owners: Vec<Uuid> =
        sqlx::query_scalar("select id from owners where presets_seeded_at is null order by id")
            .fetch_all(pool)
            .await?;
    for owner in owners {
        crate::db::in_tx(pool, move |conn| {
            Box::pin(async move {
                conversations::lock_owner(conn, owner).await?;
                seed_in_tx(conn, owner).await
            })
        })
        .await?;
    }
    Ok(())
}
