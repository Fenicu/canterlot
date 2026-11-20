//! Разовый импорт памяти Claude: применение пакета, собранного воркером.
//!
//! Пакет приходит из `canterlot-worker memory import-bundle`, владелец уже просмотрел
//! манифест. Здесь нет доверия к пакету: каждая запись идёт через `store::write` с
//! автором `Import`, то есть через тот же детектор секретов, что и запись агента.
//! Всё применение идёт в одной транзакции; сухой прогон выполняет те же записи и
//! откатывает транзакцию, поэтому его отчёт совпадает с настоящим.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::store::{
    self, Actor, Kind, Level, MemoryError, MemoryScope, NewEntry, SUBJECT_MAX_CHARS,
};
use super::{MemoryDomain, import_rules};
use crate::config::MemoryConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub bundle_id: String,
    pub created_at: DateTime<Utc>,
    pub entries: Vec<BundleEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleEntry {
    pub domain: String,
    /// Корень проекта; пусто или `null` — запись всего домена.
    #[serde(default)]
    pub project: Option<String>,
    pub kind: String,
    pub subject: String,
    pub body: String,
    /// Каталог источника и относительный путь файла; вместе с `sha256` ключ идемпотентности.
    pub source_path: String,
    pub sha256: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    pub created: u32,
    pub skipped_existing: u32,
    pub renamed_collisions: u32,
    /// Записи, которые детектор отклонил, хотя воркер их в пакет не включил.
    pub refused_secrets: u32,
    /// Записи с запрещённым именем источника (`MEMORY.md`, имена с учётными данными):
    /// воркер их в пакет не кладёт, значит пакет собран вручную.
    pub refused_names: u32,
    /// Записи, у которых вид вне восьми допустимых заменён на `reference`.
    pub kind_remapped: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("entry {source_path}: invalid {field}")]
    Invalid {
        source_path: String,
        field: &'static str,
    },
    #[error("entry {source_path}: refused with {code}")]
    Refused {
        source_path: String,
        code: &'static str,
    },
    #[error("database error")]
    Db(#[from] sqlx::Error),
}

/// Применить пакет одной транзакцией. `dry_run` откатывает её в конце.
pub async fn apply(
    pool: &PgPool,
    bundle: &Bundle,
    dry_run: bool,
    config: &MemoryConfig,
) -> Result<ImportReport, ImportError> {
    let mut tx = crate::db::begin(pool).await?;
    // Применения идут по одному: проверка «уже импортировано» и запись не должны
    // разойтись между двумя параллельными транзакциями.
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('canterlot:memory:import', 0))")
        .execute(&mut *tx)
        .await?;
    let report = match apply_entries(&mut tx, bundle, config).await {
        Ok(report) => report,
        Err(error) => {
            let _ = tx.rollback().await;
            return Err(error);
        }
    };
    if dry_run {
        tx.rollback().await?;
    } else {
        tx.commit().await?;
    }
    Ok(report)
}

async fn apply_entries(
    conn: &mut PgConnection,
    bundle: &Bundle,
    config: &MemoryConfig,
) -> Result<ImportReport, ImportError> {
    let mut report = ImportReport::default();
    for entry in &bundle.entries {
        let invalid = |field| ImportError::Invalid {
            source_path: entry.source_path.clone(),
            field,
        };
        let domain = MemoryDomain::parse(&entry.domain).ok_or_else(|| invalid("domain"))?;
        let project = entry
            .project
            .as_deref()
            .map(str::trim)
            .filter(|project| !project.is_empty())
            .map(str::to_owned);
        if entry.source_path.is_empty() || entry.sha256.is_empty() {
            return Err(invalid("source"));
        }
        if import_rules::forbidden_name(import_rules::basename(&entry.source_path)).is_some() {
            report.refused_names += 1;
            continue;
        }
        if already_imported(conn, &entry.source_path, &entry.sha256).await? {
            report.skipped_existing += 1;
            continue;
        }
        let kind = Kind::parse(&entry.kind).unwrap_or_else(|| {
            report.kind_remapped += 1;
            Kind::Reference
        });
        let subject = normalize_subject(&entry.subject);
        if subject.is_empty() {
            return Err(invalid("subject"));
        }
        let level = if project.is_some() {
            Level::Project
        } else {
            Level::Domain
        };
        let scope = MemoryScope::owner(domain, project);
        let actor = Actor::Import {
            bundle_id: bundle.bundle_id.clone(),
        };
        let mut attempt = 1u32;
        let written = loop {
            let candidate = with_suffix(&subject, attempt);
            let new = NewEntry {
                kind,
                subject: candidate,
                body: entry.body.clone(),
                level,
                valid_until: None,
            };
            match store::write(conn, &scope, actor.clone(), new, config).await {
                Ok(written) => break Some(written),
                Err(MemoryError::SubjectExists { .. }) if attempt < 1000 => attempt += 1,
                Err(MemoryError::LooksLikeSecret { .. }) => break None,
                Err(error) => {
                    return Err(match error {
                        MemoryError::Db(error) => ImportError::Db(error),
                        other => ImportError::Refused {
                            source_path: entry.source_path.clone(),
                            code: other.code(),
                        },
                    });
                }
            }
        };
        let Some(written) = written else {
            report.refused_secrets += 1;
            continue;
        };
        if attempt > 1 {
            report.renamed_collisions += 1;
        }
        mark_imported(conn, written.id, entry).await?;
        report.created += 1;
    }
    Ok(report)
}

async fn already_imported(
    conn: &mut PgConnection,
    source_path: &str,
    sha256: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists (select 1 from memory_entries
         where import_source->>'path' = $1 and import_source->>'sha256' = $2)",
    )
    .bind(source_path)
    .bind(sha256)
    .fetch_one(&mut *conn)
    .await
}

/// Ключ идемпотентности и время файла: запись создана как есть на момент изменения файла.
async fn mark_imported(
    conn: &mut PgConnection,
    id: Uuid,
    entry: &BundleEntry,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update memory_entries set import_source = $2, created_at = $3, updated_at = $3
         where id = $1",
    )
    .bind(id)
    .bind(json!({"path": entry.source_path, "sha256": entry.sha256}))
    .bind(entry.updated_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Одна строка без повторных пробелов, не длиннее предела, с обрезкой по границе слова.
fn normalize_subject(subject: &str) -> String {
    let line = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_words(&line, SUBJECT_MAX_CHARS)
}

fn truncate_words(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max_chars).collect();
    // Если обрезка попала внутрь слова, слово отбрасывается целиком.
    let boundary = text.chars().nth(max_chars).is_some_and(char::is_whitespace);
    let cut = if boundary {
        cut.as_str()
    } else {
        cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head)
    };
    cut.trim_end().to_owned()
}

/// Тема с суффиксом ` (n)`; первая попытка без суффикса.
fn with_suffix(subject: &str, attempt: u32) -> String {
    if attempt <= 1 {
        return subject.to_owned();
    }
    let suffix = format!(" ({attempt})");
    let room = SUBJECT_MAX_CHARS - suffix.chars().count();
    format!("{}{suffix}", truncate_words(subject, room))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_is_one_line_and_cut_on_a_word_boundary() {
        let long = "слово ".repeat(60);
        let cut = normalize_subject(&long);
        assert!(cut.chars().count() <= SUBJECT_MAX_CHARS);
        assert!(cut.ends_with("слово"), "{cut}");
        assert_eq!(normalize_subject("a\n  b\tc"), "a b c");
    }

    #[test]
    fn suffix_keeps_the_subject_within_the_limit() {
        let long = "слово ".repeat(60);
        let subject = normalize_subject(&long);
        let renamed = with_suffix(&subject, 3);
        assert!(renamed.chars().count() <= SUBJECT_MAX_CHARS);
        assert!(renamed.ends_with(" (3)"));
        assert_eq!(with_suffix("тема", 2), "тема (2)");
    }
}
