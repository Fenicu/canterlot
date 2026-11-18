//! Лента обновлений и моделей машин. События рождаются из отчёта машины в той же
//! транзакции, где сохраняется сам отчёт: повтор и перезапуск хаба не теряют и не
//! удваивают записи. Хаб только показывает изменения, обновления он не выполняет.

use std::{cmp::Ordering, collections::HashMap};

use canterlot_proto::worker::v1::MachineReport;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    conversations,
    events::{self, NewEvent, Scope},
};

pub const VERSION_AVAILABLE: &str = "version_available";
pub const BELOW_MINIMUM: &str = "below_minimum";
pub const CLI_UPDATED: &str = "cli_updated";
pub const MODELS_ADDED: &str = "models_added";
pub const MODELS_REMOVED: &str = "models_removed";
pub const ALL_KINDS: &[&str] = &[
    VERSION_AVAILABLE,
    BELOW_MINIMUM,
    CLI_UPDATED,
    MODELS_ADDED,
    MODELS_REMOVED,
];
/// Живое уведомление об новой записи ленты; область — машины владельца.
pub const LIVE_KIND: &str = "machine_update";
/// Сколько записей ленты хранится после уборки.
pub const RETAIN_LATEST: i64 = 500;

struct Semver<'a> {
    core: (u64, u64, u64),
    pre: Vec<&'a str>,
}

fn parse_semver(text: &str) -> Option<Semver<'_>> {
    let text = text.split_once('+').map_or(text, |(core, _)| core);
    let (core, pre) = match text.split_once('-') {
        Some((core, pre)) => (core, pre),
        None => (text, ""),
    };
    let mut parts = core.split('.');
    let core = (
        parts.next()?.parse::<u64>().ok()?,
        parts.next()?.parse::<u64>().ok()?,
        parts.next()?.parse::<u64>().ok()?,
    );
    if parts.next().is_some() {
        return None;
    }
    let pre = if pre.is_empty() {
        Vec::new()
    } else {
        pre.split('.').collect()
    };
    Some(Semver { core, pre })
}

/// Сравнение semver с предварительными сборками: `1.2.3-rc.2 < 1.2.3`.
/// Зеркало правила воркера из `cli_versions::compare` (#196).
pub fn compare(a: &str, b: &str) -> Ordering {
    let (Some(left), Some(right)) = (parse_semver(a), parse_semver(b)) else {
        return a.cmp(b);
    };
    left.core
        .cmp(&right.core)
        .then_with(|| match (left.pre.is_empty(), right.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => {
                for (x, y) in left.pre.iter().zip(&right.pre) {
                    let ordering = match (x.parse::<u64>(), y.parse::<u64>()) {
                        (Ok(x), Ok(y)) => x.cmp(&y),
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => x.cmp(y),
                    };
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                }
                left.pre.len().cmp(&right.pre.len())
            }
        })
}

fn dedup_key(kind: &str, machine_id: Uuid, harness: &str, parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update(kind.as_bytes());
    hash.update([0]);
    hash.update(machine_id.as_bytes());
    hash.update([0]);
    hash.update(harness.as_bytes());
    for part in parts {
        hash.update([0]);
        hash.update(part.as_bytes());
    }
    let hex: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{kind}:{hex}")
}

fn model_ids(models: &Value) -> Vec<String> {
    models
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn sorted_unique(ids: &[String]) -> Vec<String> {
    let mut sorted = ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sorted
}

fn joined(ids: &[String]) -> String {
    sorted_unique(ids).join("\u{1f}")
}

pub struct Previous {
    pub installed: String,
    pub models: Vec<String>,
}

struct Draft {
    kind: &'static str,
    harness: String,
    key: String,
    payload: Value,
}

/// Читает состояние до применения отчёта: для первого отчёта харнесса нет строки —
/// он становится базовой точкой и событий моделей не порождает.
async fn previous_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
) -> Result<HashMap<String, Previous>, sqlx::Error> {
    let rows: Vec<(String, Value, String)> = sqlx::query_as(
        "select harness,models,installed_version from machine_harnesses where machine_id=$1",
    )
    .bind(machine_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(harness, models, installed)| {
            (
                harness,
                Previous {
                    installed,
                    models: model_ids(&models),
                },
            )
        })
        .collect())
}

fn draft(
    kind: &'static str,
    harness: &str,
    machine_id: Uuid,
    parts: &[&str],
    payload: Value,
) -> Draft {
    Draft {
        kind,
        harness: harness.to_owned(),
        key: dedup_key(kind, machine_id, harness, parts),
        payload,
    }
}

/// Сравнивает отчёт с сохранённым состоянием и вставляет новые записи ленты.
/// Вызывается внутри транзакции применения отчёта, до или после перезаписи
/// `machine_harnesses` — состояние передаётся отдельно, прочитанное до неё.
pub async fn generate_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    machine_id: Uuid,
    previous: &HashMap<String, Previous>,
    report: &MachineReport,
    versions_enabled: bool,
    correlation_id: Uuid,
) -> Result<usize, sqlx::Error> {
    let machine_name: String =
        sqlx::query_scalar("select name from machines where id=$1 and owner_id=$2")
            .bind(machine_id)
            .bind(owner_id)
            .fetch_one(&mut *conn)
            .await?;
    let mut drafts = Vec::new();
    for harness in &report.harnesses {
        let name = harness.harness.as_str();
        let installed = harness.installed_version.as_str();
        let latest = harness.latest_version.as_str();
        let minimum = harness.min_version.as_str();
        let command = harness.update_command.as_str();
        if versions_enabled {
            if !latest.is_empty()
                && !installed.is_empty()
                && compare(latest, installed) == Ordering::Greater
            {
                drafts.push(draft(
                    VERSION_AVAILABLE,
                    name,
                    machine_id,
                    &[latest],
                    json!({"installed_version":installed,"latest_version":latest,"min_version":minimum,"update_command":command}),
                ));
            }
            if !minimum.is_empty()
                && !installed.is_empty()
                && compare(installed, minimum) == Ordering::Less
            {
                drafts.push(draft(
                    BELOW_MINIMUM,
                    name,
                    machine_id,
                    &[installed],
                    json!({"installed_version":installed,"latest_version":latest,"min_version":minimum,"update_command":command}),
                ));
            }
            if let Some(old) = previous.get(name)
                && !old.installed.is_empty()
                && !installed.is_empty()
                && old.installed != installed
            {
                drafts.push(draft(
                    CLI_UPDATED,
                    name,
                    machine_id,
                    &[&old.installed, installed],
                    json!({"from":old.installed,"to":installed,"update_command":command}),
                ));
            }
        }
        if let Some(old) = previous.get(name) {
            let now: Vec<String> = harness.models.iter().map(|m| m.id.clone()).collect();
            let old_key = joined(&old.models);
            let new_key = joined(&now);
            let added: Vec<String> = now
                .iter()
                .filter(|id| !old.models.contains(id))
                .cloned()
                .collect();
            let removed: Vec<String> = old
                .models
                .iter()
                .filter(|id| !now.contains(id))
                .cloned()
                .collect();
            if !added.is_empty() {
                drafts.push(draft(
                    MODELS_ADDED,
                    name,
                    machine_id,
                    &[&old_key, &new_key],
                    json!({"models":sorted_unique(&added)}),
                ));
            }
            if !removed.is_empty() {
                drafts.push(draft(
                    MODELS_REMOVED,
                    name,
                    machine_id,
                    &[&old_key, &new_key],
                    json!({"models":sorted_unique(&removed)}),
                ));
            }
        }
    }
    let mut inserted = 0;
    for item in drafts {
        let stored: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(
            "insert into machine_updates(id,owner_id,machine_id,kind,harness,dedup_key,payload)
             values($1,$2,$3,$4,$5,$6,$7)
             on conflict(dedup_key) do nothing returning id,created_at",
        )
        .bind(Uuid::now_v7())
        .bind(owner_id)
        .bind(machine_id)
        .bind(item.kind)
        .bind(&item.harness)
        .bind(&item.key)
        .bind(&item.payload)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some((id, created_at)) = stored {
            inserted += 1;
            events::append(
                conn,
                NewEvent {
                    scope: Scope::Machines { owner_id },
                    kind: LIVE_KIND,
                    conversation_id: None,
                    execution_id: None,
                    payload: json!({
                        "id":id,
                        "kind":item.kind,
                        "machine":machine_name,
                        "harness":item.harness,
                        "created_at":conversations::time(created_at),
                        "payload":item.payload,
                    }),
                    correlation_id,
                },
            )
            .await?;
        }
    }
    Ok(inserted)
}

/// Читает предыдущее состояние харнессов; вызывается до удаления строк каталога.
pub async fn previous_for_report(
    conn: &mut PgConnection,
    machine_id: Uuid,
) -> Result<HashMap<String, Previous>, sqlx::Error> {
    previous_in_tx(conn, machine_id).await
}

/// Держим последние 500 записей и не старше 90 дней.
pub async fn purge(pool: &PgPool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "with ranked as (
            select id,row_number() over (partition by owner_id order by created_at desc,id desc) as position
            from machine_updates
         )
         delete from machine_updates u using ranked r
         where u.id=r.id
           and (r.position>$1 or u.created_at<clock_timestamp()-interval '90 days')",
    )
    .bind(RETAIN_LATEST)
    .execute(pool)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_compares_cores_first_and_prereleases_below_the_release() {
        assert_eq!(compare("2.1.311", "2.1.312"), Ordering::Less);
        assert_eq!(compare("2.1.311", "2.2.0"), Ordering::Less);
        assert_eq!(compare("0.167.0", "0.164.0"), Ordering::Greater);
        assert_eq!(compare("2.1.311", "2.1.311"), Ordering::Equal);
        assert_eq!(compare("2.1.311-rc.1", "2.1.311"), Ordering::Less);
        assert_eq!(compare("0.164.0-alpha.2", "0.164.0"), Ordering::Less);
        assert_eq!(
            compare("0.164.0-alpha.2", "0.164.0-alpha.10"),
            Ordering::Less
        );
        assert_eq!(compare("2.2.6-beta.1", "2.2.6-beta.2"), Ordering::Less);
        assert_eq!(compare("2.1.311+Build.7", "2.1.311"), Ordering::Equal);
    }
}
