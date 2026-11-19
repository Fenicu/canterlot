//! Хранилище памяти. Весь SQL к таблицам памяти живёт здесь и в `memory::mod`.
//!
//! Каждый запрос берёт `MemoryScope`. Область строится только из строки вызывающего
//! разговора (домен и проект читаются в той же транзакции) либо для владельца из явного
//! домена. Домен и проект входят в `where` до ранжирования, а чужие и несуществующие
//! идентификаторы отвечают одинаково: `not_found`.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::MemoryDomain;
use super::secrets::{self, SecretClass};
use crate::config::MemoryConfig;

pub const SUBJECT_MAX_CHARS: usize = 160;
pub const BODY_MAX_BYTES: usize = 8 * 1024;
pub const EXCERPT_CHARS: i32 = 200;
pub const REVISIONS_KEPT: i64 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    User,
    Feedback,
    Project,
    Reference,
    Decision,
    Gotcha,
    Environment,
    Incident,
}

impl Kind {
    pub const ALL: [Self; 8] = [
        Self::User,
        Self::Feedback,
        Self::Project,
        Self::Reference,
        Self::Decision,
        Self::Gotcha,
        Self::Environment,
        Self::Incident,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Feedback => "feedback",
            Self::Project => "project",
            Self::Reference => "reference",
            Self::Decision => "decision",
            Self::Gotcha => "gotcha",
            Self::Environment => "environment",
            Self::Incident => "incident",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Project,
    Domain,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Domain => "domain",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "project" => Some(Self::Project),
            "domain" => Some(Self::Domain),
            _ => None,
        }
    }

    fn of(project: Option<&str>) -> Self {
        if project.is_some() {
            Self::Project
        } else {
            Self::Domain
        }
    }
}

/// Область вызова. Поля закрыты: собрать её можно только из строки разговора
/// (`of_caller`) или от имени владельца (`owner`).
#[derive(Clone, Debug)]
pub struct MemoryScope {
    domain: MemoryDomain,
    project: Option<String>,
    owner_id: Option<Uuid>,
    is_owner: bool,
}

impl MemoryScope {
    /// Область разговора. `None`, если у него нет домена: памяти нет.
    pub async fn of_caller(
        conn: &mut PgConnection,
        conversation_id: Uuid,
    ) -> Result<Option<Self>, sqlx::Error> {
        let row: Option<(Uuid, Option<String>, Option<String>)> = sqlx::query_as(
            "select owner_id, memory_domain, memory_project from conversations where id = $1",
        )
        .bind(conversation_id)
        .fetch_optional(&mut *conn)
        .await?;
        let Some((owner_id, Some(domain), project)) = row else {
            return Ok(None);
        };
        let Some(domain) = MemoryDomain::parse(&domain) else {
            return Ok(None);
        };
        Ok(Some(Self {
            domain,
            project,
            owner_id: Some(owner_id),
            is_owner: false,
        }))
    }

    /// Область владельца: явный домен, при необходимости проект для записей уровня проекта.
    pub fn owner(domain: MemoryDomain, project: Option<String>) -> Self {
        Self {
            domain,
            project,
            owner_id: None,
            is_owner: true,
        }
    }

    pub fn domain(&self) -> MemoryDomain {
        self.domain
    }
}

/// Кто пишет. Идёт в происхождение записи и в журнал.
#[derive(Clone, Debug)]
pub enum Actor {
    Agent {
        conversation_id: Uuid,
        harness: String,
        model: String,
    },
    Owner,
    Import {
        bundle_id: String,
    },
}

impl Actor {
    fn json(&self) -> Value {
        match self {
            Self::Agent {
                conversation_id,
                harness,
                model,
            } => json!({
                "kind": "agent",
                "conversation_id": conversation_id,
                "harness": harness,
                "model": model,
            }),
            Self::Owner => json!({"kind": "owner"}),
            Self::Import { bundle_id } => json!({"kind": "import", "bundle_id": bundle_id}),
        }
    }

    fn conversation_id(&self) -> Option<Uuid> {
        match self {
            Self::Agent {
                conversation_id, ..
            } => Some(*conversation_id),
            _ => None,
        }
    }

    /// Детектор секретов и лимит записей действуют на агентов и импорт, не на владельца:
    /// владельцу PWA показывает предупреждение, и он вправе сохранить поверх него.
    fn is_screened(&self) -> bool {
        !matches!(self, Self::Owner)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("looks like a secret")]
    LooksLikeSecret { class: SecretClass },
    #[error("too large")]
    TooLarge { limit: usize },
    #[error("subject exists")]
    SubjectExists { id: Uuid, version: i32 },
    #[error("version conflict")]
    VersionConflict { current_version: i32 },
    #[error("not found")]
    NotFound,
    #[error("rate limited")]
    RateLimited { retry_after_s: u64 },
    #[error("domain full")]
    DomainFull { limit: u32 },
    #[error("bad level")]
    BadLevel,
    #[error("invalid")]
    Invalid { field: &'static str },
    #[error("memory unavailable")]
    MemoryUnavailable,
    #[error("database error")]
    Db(#[from] sqlx::Error),
}

impl MemoryError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::LooksLikeSecret { .. } => "looks_like_secret",
            Self::TooLarge { .. } => "too_large",
            Self::SubjectExists { .. } => "subject_exists",
            Self::VersionConflict { .. } => "version_conflict",
            Self::NotFound => "not_found",
            Self::RateLimited { .. } => "rate_limited",
            Self::DomainFull { .. } => "domain_full",
            Self::BadLevel => "bad_level",
            Self::Invalid { .. } => "invalid",
            Self::MemoryUnavailable => "memory_unavailable",
            Self::Db(_) => "internal_error",
        }
    }

    /// Метаданные отказа: поля, которых ждёт вызывающий. Значение секрета сюда не попадает.
    pub fn metadata(&self) -> Value {
        match self {
            Self::LooksLikeSecret { class } => json!({"class": class.as_str()}),
            Self::TooLarge { limit } => json!({"limit": limit}),
            Self::SubjectExists { id, version } => json!({"id": id, "version": version}),
            Self::VersionConflict { current_version } => {
                json!({"current_version": current_version})
            }
            Self::RateLimited { retry_after_s } => json!({"retry_after_s": retry_after_s}),
            Self::DomainFull { limit } => json!({"limit": limit}),
            Self::Invalid { field } => json!({"field": field}),
            _ => json!({}),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewEntry {
    pub kind: Kind,
    pub subject: String,
    pub body: String,
    pub level: Level,
    pub valid_until: Option<DateTime<Utc>>,
}

/// Частичное изменение: пустые поля остаются как были. `valid_until: Some(None)` снимает срок.
#[derive(Debug, Clone, Default)]
pub struct Patch {
    pub kind: Option<Kind>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub level: Option<Level>,
    pub valid_until: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Clone)]
pub struct Written {
    pub id: Uuid,
    pub version: i32,
    pub level: Level,
    pub domain: MemoryDomain,
    pub project_label: Option<String>,
}

impl Written {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "version": self.version,
            "level": self.level.as_str(),
            "domain": self.domain.as_str(),
            "project_label": self.project_label,
        })
    }
}

#[derive(Debug, Clone)]
pub struct SearchRow {
    pub id: Uuid,
    pub kind: String,
    pub level: Level,
    pub project_label: Option<String>,
    pub subject: String,
    pub excerpt: String,
    pub updated_at: DateTime<Utc>,
    pub expired: bool,
}

impl SearchRow {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "level": self.level.as_str(),
            "project_label": self.project_label,
            "subject": self.subject,
            "excerpt": self.excerpt,
            "updated_at": self.updated_at,
            "expired": self.expired,
        })
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Entry {
    pub id: Uuid,
    pub domain: String,
    pub project: Option<String>,
    pub kind: String,
    pub subject: String,
    pub body: String,
    pub status: String,
    pub valid_until: Option<DateTime<Utc>>,
    pub version: i32,
    pub created_by: Value,
    pub created_at: DateTime<Utc>,
    pub updated_by: Value,
    pub updated_at: DateTime<Utc>,
    pub import_source: Option<Value>,
    pub expired: bool,
}

impl Entry {
    pub fn level(&self) -> Level {
        Level::of(self.project.as_deref())
    }

    pub fn project_label(&self) -> Option<String> {
        self.project.as_deref().map(label_of)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "domain": self.domain,
            "level": self.level().as_str(),
            "project_label": self.project_label(),
            "kind": self.kind,
            "subject": self.subject,
            "body": self.body,
            "status": self.status,
            "valid_until": self.valid_until,
            "expired": self.expired,
            "version": self.version,
            "created_by": self.created_by,
            "created_at": self.created_at,
            "updated_by": self.updated_by,
            "updated_at": self.updated_at,
            "import_source": self.import_source,
        })
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Revision {
    pub id: i64,
    pub version: i32,
    pub action: String,
    pub project: Option<String>,
    pub kind: String,
    pub subject: String,
    pub body: String,
    pub status: String,
    pub valid_until: Option<DateTime<Utc>>,
    pub actor: Value,
    pub at: DateTime<Utc>,
}

/// Последний компонент пути проекта: по нему агент отличает проекты, не видя путей.
fn label_of(project: &str) -> String {
    std::path::Path::new(project)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(project)
        .to_owned()
}

/// `$1` id, `$2` домен, `$3` проект, `$4` шире ли проекта, `$5` с архивными.
const GET_SQL: &str = "select id, domain, project, kind, subject, body, status, valid_until, \
    version, created_by, created_at, updated_by, updated_at, import_source, \
    (valid_until is not null and valid_until < now()) as expired \
    from memory_entries \
    where id = $1 and domain = $2 \
    and ($4 or project is null or project is not distinct from $3) \
    and ($5 or status = 'active')";

/// Тот же отбор с блокировкой строки.
const LOCK_SQL: &str = "select id, domain, project, kind, subject, body, status, valid_until, \
    version, created_by, created_at, updated_by, updated_at, import_source, \
    (valid_until is not null and valid_until < now()) as expired \
    from memory_entries \
    where id = $1 and domain = $2 \
    and ($4 or project is null or project is not distinct from $3) \
    and ($5 or status = 'active') for update";

impl MemoryScope {
    fn widen_everywhere(&self) -> bool {
        self.is_owner
    }
}

fn check_subject(subject: &str) -> Result<String, MemoryError> {
    let subject = subject.trim();
    let chars = subject.chars().count();
    if chars == 0 || chars > SUBJECT_MAX_CHARS || subject.contains(['\n', '\r']) {
        return Err(MemoryError::Invalid { field: "subject" });
    }
    Ok(subject.to_owned())
}

fn check_body(body: &str) -> Result<(), MemoryError> {
    if body.trim().is_empty() {
        return Err(MemoryError::Invalid { field: "body" });
    }
    if body.len() > BODY_MAX_BYTES {
        return Err(MemoryError::TooLarge {
            limit: BODY_MAX_BYTES,
        });
    }
    Ok(())
}

fn screen(actor: &Actor, texts: &[&str]) -> Result<(), MemoryError> {
    if !actor.is_screened() {
        return Ok(());
    }
    for text in texts {
        if let Some(class) = secrets::scan(text) {
            return Err(MemoryError::LooksLikeSecret { class });
        }
    }
    Ok(())
}

async fn lock(conn: &mut PgConnection, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Лимит записей агента в час. Счёт идёт по журналу под замком разговора, который держится
/// до конца транзакции записи: параллельные записи одного разговора становятся в очередь.
async fn check_rate(
    conn: &mut PgConnection,
    actor: &Actor,
    config: &MemoryConfig,
) -> Result<(), MemoryError> {
    let Some(conversation_id) = actor.conversation_id() else {
        return Ok(());
    };
    lock(conn, &format!("memory_writes:{conversation_id}")).await?;
    let (count, retry): (i64, Option<i64>) = sqlx::query_as(
        "select count(*),
                ceil(extract(epoch from (min(at) + interval '1 hour' - now())))::bigint
         from memory_events
         where conversation_id = $1 and at > now() - interval '1 hour'
           and action in ('create', 'update', 'archive')",
    )
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    if count >= i64::from(config.writes_per_hour) {
        return Err(MemoryError::RateLimited {
            retry_after_s: u64::try_from(retry.unwrap_or(1).max(1)).unwrap_or(1),
        });
    }
    Ok(())
}

async fn owner_id(conn: &mut PgConnection, scope: &MemoryScope) -> Result<Uuid, MemoryError> {
    if let Some(id) = scope.owner_id {
        return Ok(id);
    }
    // Владелец один: область владельца не несёт его идентификатор.
    let id: Option<Uuid> = sqlx::query_scalar("select id from owners limit 1")
        .fetch_optional(&mut *conn)
        .await?;
    id.ok_or(MemoryError::NotFound)
}

async fn active_count(conn: &mut PgConnection, domain: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*) from memory_entries where domain = $1 and status = 'active'",
    )
    .bind(domain)
    .fetch_one(&mut *conn)
    .await
}

async fn subject_holder(
    conn: &mut PgConnection,
    domain: &str,
    project: Option<&str>,
    subject: &str,
    except: Option<Uuid>,
) -> Result<Option<(Uuid, i32)>, sqlx::Error> {
    sqlx::query_as(
        "select id, version from memory_entries
         where domain = $1 and coalesce(project, '') = coalesce($2, '')
           and lower(subject) = lower($3) and status = 'active'
           and ($4::uuid is null or id <> $4)",
    )
    .bind(domain)
    .bind(project)
    .bind(subject)
    .bind(except)
    .fetch_optional(&mut *conn)
    .await
}

async fn record_event(
    conn: &mut PgConnection,
    entry_id: Uuid,
    domain: &str,
    action: &str,
    actor: &Actor,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into memory_events (entry_id, domain, action, actor, conversation_id)
         values ($1, $2, $3, $4, $5)",
    )
    .bind(entry_id)
    .bind(domain)
    .bind(action)
    .bind(actor.json())
    .bind(actor.conversation_id())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Сохраняет прежнее состояние и держит последние 20.
async fn record_revision(
    conn: &mut PgConnection,
    old: &Entry,
    action: &str,
    actor: &Actor,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into memory_revisions
           (entry_id, version, action, project, kind, subject, body, status, valid_until, actor)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(old.id)
    .bind(old.version)
    .bind(action)
    .bind(&old.project)
    .bind(&old.kind)
    .bind(&old.subject)
    .bind(&old.body)
    .bind(&old.status)
    .bind(old.valid_until)
    .bind(actor.json())
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "delete from memory_revisions
         where entry_id = $1
           and id not in (select id from memory_revisions where entry_id = $1
                          order by id desc limit $2)",
    )
    .bind(old.id)
    .bind(REVISIONS_KEPT)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn search(
    conn: &mut PgConnection,
    scope: &MemoryScope,
    query: &str,
    kind: Option<Kind>,
    include_other_projects: bool,
    limit: u32,
    config: &MemoryConfig,
) -> Result<Vec<SearchRow>, MemoryError> {
    let query: String = query.trim().chars().take(200).collect();
    let limit = i64::from(limit.clamp(1, config.search_limit.max(1)));
    // Сначала не истёкшие; среди них грубый ранг двух конфигураций, затем свой проект,
    // записи домена и чужие проекты, затем новизна. Пустой запрос даёт самые новые.
    let rows: Vec<SearchRaw> =
        sqlx::query_as(
            "with q as (
               select pg_catalog.websearch_to_tsquery('pg_catalog.russian'::pg_catalog.regconfig, $5) as ru,
                      pg_catalog.websearch_to_tsquery('pg_catalog.simple'::pg_catalog.regconfig, $5) as si
             )
             select e.id, e.kind, e.project, e.subject,
                    left(regexp_replace(e.body, '\\s+', ' ', 'g'), $7) as excerpt,
                    e.updated_at,
                    (e.valid_until is not null and e.valid_until < now()) as expired
             from memory_entries e, q
             where e.domain = $1 and e.status = 'active'
               and ($3 or e.project is null or e.project is not distinct from $2)
               and ($4::text is null or e.kind = $4)
               and ($5 = '' or e.tsv @@ q.ru or e.tsv @@ q.si)
             order by (e.valid_until is not null and e.valid_until < now()),
                      case when $5 = '' then 0
                           else round((pg_catalog.ts_rank_cd(e.tsv, q.ru, 32)
                                     + pg_catalog.ts_rank_cd(e.tsv, q.si, 32))::numeric, 1)
                      end desc,
                      case when $5 = '' then 0
                           when e.project is not null and e.project is not distinct from $2 then 0
                           when e.project is null then 1
                           else 2 end,
                      e.updated_at desc, e.id
             limit $6",
        )
        .bind(scope.domain.as_str())
        .bind(&scope.project)
        .bind(include_other_projects || scope.widen_everywhere())
        .bind(kind.map(Kind::as_str))
        .bind(&query)
        .bind(limit)
        .bind(EXCERPT_CHARS)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| SearchRow {
            id: row.id,
            kind: row.kind,
            level: Level::of(row.project.as_deref()),
            project_label: row.project.as_deref().map(label_of),
            subject: row.subject,
            excerpt: row.excerpt,
            updated_at: row.updated_at,
            expired: row.expired,
        })
        .collect())
}

#[derive(sqlx::FromRow)]
struct SearchRaw {
    id: Uuid,
    kind: String,
    project: Option<String>,
    subject: String,
    excerpt: String,
    updated_at: DateTime<Utc>,
    expired: bool,
}

/// Запись по идентификатору. Агент видит только активные записи своего домена; с `widen`
/// это и чужие проекты домена. Чужой домен и несуществующий идентификатор неотличимы.
pub async fn get(
    conn: &mut PgConnection,
    scope: &MemoryScope,
    id: Uuid,
    widen: bool,
) -> Result<Option<Entry>, sqlx::Error> {
    sqlx::query_as(GET_SQL)
        .bind(id)
        .bind(scope.domain.as_str())
        .bind(&scope.project)
        .bind(widen || scope.widen_everywhere())
        .bind(scope.is_owner)
        .fetch_optional(&mut *conn)
        .await
}

pub async fn write(
    conn: &mut PgConnection,
    scope: &MemoryScope,
    actor: Actor,
    new: NewEntry,
    config: &MemoryConfig,
) -> Result<Written, MemoryError> {
    let subject = check_subject(&new.subject)?;
    check_body(&new.body)?;
    screen(&actor, &[&subject, &new.body])?;
    let project = match new.level {
        Level::Domain => None,
        Level::Project => Some(scope.project.clone().ok_or(MemoryError::BadLevel)?),
    };
    let domain = scope.domain.as_str();
    check_rate(conn, &actor, config).await?;
    lock(conn, &format!("memory_domain:{domain}")).await?;
    if active_count(conn, domain).await? >= i64::from(config.max_active_per_domain) {
        return Err(MemoryError::DomainFull {
            limit: config.max_active_per_domain,
        });
    }
    if let Some((id, version)) =
        subject_holder(conn, domain, project.as_deref(), &subject, None).await?
    {
        return Err(MemoryError::SubjectExists { id, version });
    }
    let id = Uuid::now_v7();
    let owner = owner_id(conn, scope).await?;
    sqlx::query(
        "insert into memory_entries
           (id, owner_id, domain, project, kind, subject, body, valid_until, created_by, updated_by)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)",
    )
    .bind(id)
    .bind(owner)
    .bind(domain)
    .bind(&project)
    .bind(new.kind.as_str())
    .bind(&subject)
    .bind(&new.body)
    .bind(new.valid_until)
    .bind(actor.json())
    .execute(&mut *conn)
    .await?;
    record_event(conn, id, domain, "create", &actor).await?;
    Ok(Written {
        id,
        version: 1,
        level: Level::of(project.as_deref()),
        domain: scope.domain,
        project_label: project.as_deref().map(label_of),
    })
}

/// Запись для изменения: агент меняет только активные записи своего проекта и домена,
/// владелец любые записи домена.
async fn lock_for_change(
    conn: &mut PgConnection,
    scope: &MemoryScope,
    id: Uuid,
) -> Result<Entry, MemoryError> {
    sqlx::query_as(LOCK_SQL)
        .bind(id)
        .bind(scope.domain.as_str())
        .bind(&scope.project)
        .bind(scope.widen_everywhere())
        .bind(scope.is_owner)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(MemoryError::NotFound)
}

pub async fn update(
    conn: &mut PgConnection,
    scope: &MemoryScope,
    actor: Actor,
    id: Uuid,
    expected_version: i32,
    patch: Patch,
    config: &MemoryConfig,
) -> Result<Written, MemoryError> {
    if patch.kind.is_none()
        && patch.subject.is_none()
        && patch.body.is_none()
        && patch.level.is_none()
        && patch.valid_until.is_none()
    {
        return Err(MemoryError::Invalid { field: "patch" });
    }
    let subject = patch.subject.as_deref().map(check_subject).transpose()?;
    if let Some(body) = &patch.body {
        check_body(body)?;
    }
    let mut texts: Vec<&str> = Vec::new();
    texts.extend(subject.as_deref());
    texts.extend(patch.body.as_deref());
    screen(&actor, &texts)?;
    check_rate(conn, &actor, config).await?;
    let domain = scope.domain.as_str();
    // Замок домена охраняет уникальность темы: её меняют и новая тема, и новый уровень.
    lock(conn, &format!("memory_domain:{domain}")).await?;
    let old = lock_for_change(conn, scope, id).await?;
    if old.version != expected_version {
        return Err(MemoryError::VersionConflict {
            current_version: old.version,
        });
    }
    let project = match patch.level {
        None => old.project.clone(),
        Some(Level::Domain) => None,
        Some(Level::Project) => Some(
            old.project
                .clone()
                .or_else(|| scope.project.clone())
                .ok_or(MemoryError::BadLevel)?,
        ),
    };
    let new_subject = subject.unwrap_or_else(|| old.subject.clone());
    if old.status == "active"
        && let Some((other, version)) =
            subject_holder(conn, domain, project.as_deref(), &new_subject, Some(id)).await?
    {
        return Err(MemoryError::SubjectExists { id: other, version });
    }
    record_revision(conn, &old, "update", &actor).await?;
    let valid_until = match patch.valid_until {
        Some(value) => value,
        None => old.valid_until,
    };
    let version: i32 = sqlx::query_scalar(
        "update memory_entries
         set project = $3, kind = $4, subject = $5, body = $6, valid_until = $7,
             version = version + 1, updated_by = $8, updated_at = now()
         where id = $1 and version = $2
         returning version",
    )
    .bind(id)
    .bind(expected_version)
    .bind(&project)
    .bind(
        patch
            .kind
            .map_or_else(|| old.kind.clone(), |kind| kind.as_str().to_owned()),
    )
    .bind(&new_subject)
    .bind(patch.body.as_deref().unwrap_or(&old.body))
    .bind(valid_until)
    .bind(actor.json())
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(MemoryError::VersionConflict {
        current_version: old.version,
    })?;
    record_event(conn, id, domain, "update", &actor).await?;
    Ok(Written {
        id,
        version,
        level: Level::of(project.as_deref()),
        domain: scope.domain,
        project_label: project.as_deref().map(label_of),
    })
}

/// Архивирует активную запись. Для агента это и есть `memory_delete`.
pub async fn archive(
    conn: &mut PgConnection,
    scope: &MemoryScope,
    actor: Actor,
    id: Uuid,
    expected_version: i32,
    config: &MemoryConfig,
) -> Result<Written, MemoryError> {
    check_rate(conn, &actor, config).await?;
    let domain = scope.domain.as_str();
    let old = lock_for_change(conn, scope, id).await?;
    if old.status != "active" {
        return Err(MemoryError::NotFound);
    }
    if old.version != expected_version {
        return Err(MemoryError::VersionConflict {
            current_version: old.version,
        });
    }
    record_revision(conn, &old, "archive", &actor).await?;
    let version: i32 = sqlx::query_scalar(
        "update memory_entries
         set status = 'archived', version = version + 1, updated_by = $3, updated_at = now()
         where id = $1 and version = $2
         returning version",
    )
    .bind(id)
    .bind(expected_version)
    .bind(actor.json())
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(MemoryError::VersionConflict {
        current_version: old.version,
    })?;
    record_event(conn, id, domain, "archive", &actor).await?;
    Ok(Written {
        id,
        version,
        level: old.level(),
        domain: scope.domain,
        project_label: old.project_label(),
    })
}

/// Только владелец: возвращает архивную запись. Если её тема за это время стала активной
/// у другой записи, отказывает с `subject_exists`.
pub async fn restore(
    conn: &mut PgConnection,
    scope: &MemoryScope,
    actor: Actor,
    id: Uuid,
    config: &MemoryConfig,
) -> Result<Written, MemoryError> {
    if !scope.is_owner {
        return Err(MemoryError::NotFound);
    }
    let domain = scope.domain.as_str();
    lock(conn, &format!("memory_domain:{domain}")).await?;
    let old = lock_for_change(conn, scope, id).await?;
    if old.status != "archived" {
        return Err(MemoryError::NotFound);
    }
    if let Some((other, version)) =
        subject_holder(conn, domain, old.project.as_deref(), &old.subject, Some(id)).await?
    {
        return Err(MemoryError::SubjectExists { id: other, version });
    }
    if active_count(conn, domain).await? >= i64::from(config.max_active_per_domain) {
        return Err(MemoryError::DomainFull {
            limit: config.max_active_per_domain,
        });
    }
    record_revision(conn, &old, "restore", &actor).await?;
    let version: i32 = sqlx::query_scalar(
        "update memory_entries
         set status = 'active', version = version + 1, updated_by = $2, updated_at = now()
         where id = $1
         returning version",
    )
    .bind(id)
    .bind(actor.json())
    .fetch_one(&mut *conn)
    .await?;
    record_event(conn, id, domain, "restore", &actor).await?;
    Ok(Written {
        id,
        version,
        level: old.level(),
        domain: scope.domain,
        project_label: old.project_label(),
    })
}

/// Только владелец: убирает запись и её прошлые состояния. Журнал действий остаётся.
pub async fn hard_delete(
    conn: &mut PgConnection,
    scope: &MemoryScope,
    actor: Actor,
    id: Uuid,
) -> Result<(), MemoryError> {
    if !scope.is_owner {
        return Err(MemoryError::NotFound);
    }
    lock_for_change(conn, scope, id).await?;
    sqlx::query("delete from memory_entries where id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    record_event(conn, id, scope.domain.as_str(), "hard_delete", &actor).await?;
    Ok(())
}

/// Только владелец: прошлые состояния записи, новые первыми.
pub async fn revisions(
    conn: &mut PgConnection,
    scope: &MemoryScope,
    id: Uuid,
) -> Result<Vec<Revision>, MemoryError> {
    if !scope.is_owner {
        return Err(MemoryError::NotFound);
    }
    if get(conn, scope, id, true).await?.is_none() {
        return Err(MemoryError::NotFound);
    }
    Ok(sqlx::query_as(
        "select id, version, action, project, kind, subject, body, status, valid_until, actor, at
         from memory_revisions where entry_id = $1 order by id desc",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?)
}
