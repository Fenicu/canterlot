//! Страж машины перед запуском сессии (§5.5): свежие метрики машины родителя против
//! порогов `[orchestrator.guards]`. Устаревшие метрики страж не включают: решение
//! помечается «machine metrics stale», запуск идёт.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::config::GuardsConfig;

const BYTES_PER_GB: f64 = 1e9;
const CLEANUP_MIN_AGE_MINUTES: i64 = 60;
const CLEANUP_LIMIT: i64 = 5;

/// Измерения машины, по которым решает страж.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measurements {
    pub free_disk_gb: Option<f64>,
    pub free_disk_percent: Option<f64>,
    pub load_per_core: Option<f64>,
    pub mem_available_gb: Option<f64>,
    pub observed_at: Option<DateTime<Utc>>,
}

/// Кандидат на уборку: делегированная сессия владельца с неубранной рабочей копией.
#[derive(Clone, Debug, PartialEq)]
pub struct CleanupCandidate {
    pub session_id: Uuid,
    pub title: String,
    pub state: String,
    pub hours: i64,
}

/// Вердикт стража: измерения, нарушившие пороги, и строка для `detail`.
#[derive(Clone, Debug, Default, Serialize, PartialEq, utoipa::ToSchema)]
pub struct GuardVerdict {
    /// Метрики старше `metrics_max_age`: страж не включается.
    pub stale: bool,
    /// Хотя бы один порог нарушен (старение отменяет блокировку).
    pub blocked: bool,
    /// Человеческие имена нарушенных порогов (пусто — машина свободна).
    pub violations: Vec<String>,
    /// Одна английская строка для ответа и элемента очереди.
    pub detail: Option<String>,
}

impl GuardVerdict {
    pub fn blocked(&self) -> bool {
        self.blocked
    }

    pub fn to_value(&self) -> Value {
        json!({
            "stale": self.stale,
            "blocked": self.blocked,
            "detail": self.detail,
            "violations": self.violations,
        })
    }
}

fn gb(value: f64) -> String {
    if (value - value.round()).abs() < 0.05 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.1}")
    }
}

fn percent(value: f64) -> String {
    if (value - value.round()).abs() < 0.05 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.1}")
    }
}

/// Чистая проверка измерений против порогов: старение отменяет страж целиком.
pub fn evaluate(config: &GuardsConfig, measurements: &Measurements, stale: bool) -> GuardVerdict {
    if stale {
        return GuardVerdict {
            stale: true,
            ..GuardVerdict::default()
        };
    }
    let mut violations = Vec::new();
    if let Some(free) = measurements.free_disk_gb
        && free < config.min_free_disk_gb
    {
        violations.push(format!(
            "disk {} GB free < {} GB",
            gb(free),
            gb(config.min_free_disk_gb)
        ));
    }
    if let Some(free) = measurements.free_disk_percent
        && free < config.min_free_disk_percent
    {
        violations.push(format!(
            "disk {}% free < {}%",
            percent(free),
            percent(config.min_free_disk_percent)
        ));
    }
    if let Some(load) = measurements.load_per_core
        && load > config.max_load_per_core
    {
        violations.push(format!(
            "load {} > {} per core",
            percent(load),
            percent(config.max_load_per_core)
        ));
    }
    if let Some(memory) = measurements.mem_available_gb
        && memory < config.min_mem_available_gb
    {
        violations.push(format!(
            "memory {} GB available < {} GB",
            gb(memory),
            gb(config.min_mem_available_gb)
        ));
    }
    let detail = (!violations.is_empty()).then(|| {
        let mut text = violations.join("; ");
        text.push('.');
        text
    });
    let blocked = !violations.is_empty();
    GuardVerdict {
        stale: false,
        blocked,
        violations,
        detail,
    }
}

/// Строка `detail` с кандидатами на уборку: «disk 12 GB free < 30 GB. T3 idle 3 h,
/// T4 stopped 5 h: session_accept frees their worktrees and target dirs».
pub fn with_cleanup(verdict: &GuardVerdict, cleanup: &[CleanupCandidate]) -> GuardVerdict {
    let Some(detail) = verdict.detail.clone() else {
        return verdict.clone();
    };
    if cleanup.is_empty() {
        return verdict.clone();
    }
    let names: Vec<String> = cleanup
        .iter()
        .map(|candidate| {
            format!(
                "{} {} {} h",
                crate::session_tools::origin::clean_title(&candidate.title),
                candidate.state,
                candidate.hours
            )
        })
        .collect();
    let mut next = verdict.clone();
    next.detail = Some(format!(
        "{detail} {}: session_accept frees their worktrees and target dirs",
        names.join(", ")
    ));
    next
}

fn measurements(row: (i32, i32, i64, Value, DateTime<Utc>)) -> Measurements {
    let (cores, load1_centi, mem_available_bytes, disks, observed_at) = row;
    let mut free_disk_gb: Option<f64> = None;
    let mut free_disk_percent: Option<f64> = None;
    if let Some(items) = disks.as_array() {
        for disk in items {
            let Some(free) = disk["free_bytes"].as_u64() else {
                continue;
            };
            let total = disk["total_bytes"].as_u64().unwrap_or(0);
            let gb = free as f64 / BYTES_PER_GB;
            free_disk_gb = Some(free_disk_gb.map_or(gb, |known| known.min(gb)));
            if total > 0 {
                let percent = free as f64 / total as f64 * 100.0;
                free_disk_percent =
                    Some(free_disk_percent.map_or(percent, |known| known.min(percent)));
            }
        }
    }
    Measurements {
        free_disk_gb,
        free_disk_percent,
        load_per_core: (cores > 0).then(|| f64::from(load1_centi) / 100.0 / f64::from(cores)),
        mem_available_gb: Some(mem_available_bytes as f64 / BYTES_PER_GB),
        observed_at: Some(observed_at),
    }
}

/// Последние метрики машины и вердикт стража. Устаревшие метрики не загружают
/// кандидатов на уборку: они ни на что не влияют.
pub async fn check_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    machine: Uuid,
    config: &GuardsConfig,
    now: DateTime<Utc>,
) -> Result<GuardVerdict, sqlx::Error> {
    let row: Option<(i32, i32, i64, Value, DateTime<Utc>)> = sqlx::query_as(
        "select cpu_cores,load1_centi,mem_available_bytes,disks,observed_at
          from machine_metrics where machine_id=$1",
    )
    .bind(machine)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(GuardVerdict {
            stale: true,
            ..GuardVerdict::default()
        });
    };
    let measurements = measurements(row);
    let stale = measurements.observed_at.is_some_and(|observed| {
        now.signed_duration_since(observed)
            .to_std()
            .is_ok_and(|age| age > config.metrics_max_age)
    });
    let verdict = evaluate(config, &measurements, stale);
    if stale || !verdict.blocked() {
        return Ok(verdict);
    }
    let cleanup = cleanup_candidates_in_tx(conn, owner, machine, now).await?;
    Ok(with_cleanup(&verdict, &cleanup))
}

/// Делегированные сессии владельца на машине с неубранной рабочей копией,
/// устоявшиеся в `idle` или `stopped` дольше часа.
pub async fn cleanup_candidates_in_tx(
    conn: &mut PgConnection,
    owner: Uuid,
    machine: Uuid,
    now: DateTime<Utc>,
) -> Result<Vec<CleanupCandidate>, sqlx::Error> {
    let rows: Vec<(Uuid, String, String, i64)> = sqlx::query_as(
        "select c.id,c.title,coalesce(d.activity_state,'unknown'),
                greatest(0, floor(extract(epoch from ($3 - coalesce(d.activity_since,$3))) / 3600))::bigint
           from conversations c
           left join conversation_derived d on d.conversation_id=c.id
          where c.owner_id=$1 and c.machine_id=$2
            and c.parent_conversation_id is not null and c.trashed_at is null
            and c.worktree is not null and coalesce(c.worktree->>'state','') <> 'removed'
            and d.activity_state in ('idle','stopped')
            and d.activity_since <= $3 - interval '1 hour'
          order by d.activity_since
          limit $4",
    )
    .bind(owner)
    .bind(machine)
    .bind(now)
    .bind(CLEANUP_LIMIT)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(session_id, title, state, hours)| CleanupCandidate {
            session_id,
            title,
            state,
            hours: hours.max(CLEANUP_MIN_AGE_MINUTES / 60),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> GuardsConfig {
        GuardsConfig::default()
    }

    fn healthy() -> Measurements {
        Measurements {
            free_disk_gb: Some(120.0),
            free_disk_percent: Some(42.0),
            load_per_core: Some(0.4),
            mem_available_gb: Some(12.0),
            observed_at: Some(Utc::now()),
        }
    }

    #[test]
    fn healthy_machine_passes() {
        let verdict = evaluate(&config(), &healthy(), false);
        assert!(!verdict.blocked());
        assert!(!verdict.stale);
        assert_eq!(verdict.detail, None);
    }

    #[test]
    fn low_disk_in_gigabytes_blocks_and_names_the_measurement() {
        let mut measurements = healthy();
        measurements.free_disk_gb = Some(12.0);
        let verdict = evaluate(&config(), &measurements, false);
        assert!(verdict.blocked());
        assert_eq!(
            verdict.detail.as_deref(),
            Some("disk 12 GB free < 30 GB.")
        );
    }

    #[test]
    fn low_disk_percent_blocks() {
        let mut measurements = healthy();
        measurements.free_disk_percent = Some(4.0);
        let verdict = evaluate(&config(), &measurements, false);
        assert!(verdict.blocked());
        assert!(
            verdict
                .violations
                .iter()
                .any(|line| line.contains("4% free < 10%")),
            "{verdict:?}"
        );
    }

    #[test]
    fn high_load_blocks() {
        let mut measurements = healthy();
        measurements.load_per_core = Some(2.1);
        let verdict = evaluate(&config(), &measurements, false);
        assert!(verdict.blocked());
        assert!(
            verdict
                .violations
                .iter()
                .any(|line| line.contains("2.1 > 1.5 per core")),
            "{verdict:?}"
        );
    }

    #[test]
    fn low_memory_blocks() {
        let mut measurements = healthy();
        measurements.mem_available_gb = Some(2.0);
        let verdict = evaluate(&config(), &measurements, false);
        assert!(verdict.blocked());
        assert!(
            verdict
                .violations
                .iter()
                .any(|line| line.contains("2 GB available < 4 GB")),
            "{verdict:?}"
        );
    }

    #[test]
    fn stale_metrics_never_block() {
        let mut measurements = healthy();
        measurements.free_disk_gb = Some(1.0);
        let verdict = evaluate(&config(), &measurements, true);
        assert!(!verdict.blocked());
        assert!(verdict.stale);
    }

    #[test]
    fn cleanup_candidates_join_the_detail() {
        let mut measurements = healthy();
        measurements.free_disk_gb = Some(12.0);
        let verdict = evaluate(&config(), &measurements, false);
        let cleanup = vec![
            CleanupCandidate {
                session_id: Uuid::now_v7(),
                title: "T3".to_owned(),
                state: "idle".to_owned(),
                hours: 3,
            },
            CleanupCandidate {
                session_id: Uuid::now_v7(),
                title: "T4".to_owned(),
                state: "stopped".to_owned(),
                hours: 5,
            },
        ];
        let verdict = with_cleanup(&verdict, &cleanup);
        assert_eq!(
            verdict.detail.as_deref(),
            Some(
                "disk 12 GB free < 30 GB. T3 idle 3 h, T4 stopped 5 h: \
                 session_accept frees their worktrees and target dirs"
            )
        );
    }

    #[test]
    fn no_cleanup_candidates_leave_the_detail_alone() {
        let mut measurements = healthy();
        measurements.mem_available_gb = Some(1.0);
        let verdict = evaluate(&config(), &measurements, false);
        let with = with_cleanup(&verdict, &[]);
        assert_eq!(with.detail, verdict.detail);
    }
}
