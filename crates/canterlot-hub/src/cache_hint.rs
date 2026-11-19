//! Подсказка «почему кеш пересобран» под строкой расхода хода. Строится только из того, что
//! хаб уже хранит: расход ходов по моделям, время их начала и конца и записи о сжатии.
//! Подсказка всегда «вероятно»: хаб не видит самого кеша, а только его следы в токенах.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

/// Чтение кеша ниже этой доли входной стороны прошлого хода считается обвалом.
pub const READ_DROP_RATIO_PERCENT: u64 = 50;
/// Запись в кеш не меньше этого числа токенов считается крупной: мелкая запись — обычный ход.
pub const LARGE_WRITE_TOKENS: u64 = 4_000;
/// Простой не меньше этого срока объясняет пропажу кеша: у Anthropic он живёт около пяти минут.
pub const IDLE_EXPIRY_MINUTES: i64 = 5;

/// Расход главной модели хода, нужный для сравнения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    pub model: String,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Side {
    fn input_side(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CacheHintKind {
    /// Простой дольше срока жизни кеша.
    Expired,
    /// Модель сменилась: у неё свой кеш.
    ModelChanged,
    /// В ходе или перед ним сжали контекст.
    Compaction,
    /// Другого объяснения нет.
    Rebuilt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
pub struct CacheHint {
    pub kind: CacheHintKind,
    /// Простой между ходами в минутах; только у `expired`.
    pub idle_minutes: Option<i64>,
}

/// Подсказка или `None`: первый ход, нет расхода с одной из сторон, здоровое чтение кеша.
pub fn hint(
    previous: Option<&Side>,
    current: Option<&Side>,
    idle_minutes: Option<i64>,
    compacted: bool,
) -> Option<CacheHint> {
    let (previous, current) = (previous?, current?);
    let before = previous.input_side();
    if before == 0 || current.cache_write < LARGE_WRITE_TOKENS {
        return None;
    }
    // cache_read < before * 50%, без дробей.
    if current.cache_read.saturating_mul(100) >= before.saturating_mul(READ_DROP_RATIO_PERCENT) {
        return None;
    }
    let kind = if idle_minutes.is_some_and(|minutes| minutes >= IDLE_EXPIRY_MINUTES) {
        CacheHintKind::Expired
    } else if previous.model != current.model {
        CacheHintKind::ModelChanged
    } else if compacted {
        CacheHintKind::Compaction
    } else {
        CacheHintKind::Rebuilt
    };
    Some(CacheHint {
        kind,
        idle_minutes: (kind == CacheHintKind::Expired)
            .then_some(idle_minutes)
            .flatten(),
    })
}

fn is_main(agent: &str) -> bool {
    matches!(agent, "orchestrator" | "main")
}

/// Главная модель хода: строка основного агента, а без неё самая крупная.
fn main_side(rows: &[(String, String, i64, i64, i64, i64)]) -> Option<Side> {
    let weight = |row: &&(String, String, i64, i64, i64, i64)| row.2 + row.3 + row.4 + row.5;
    let pick = rows
        .iter()
        .filter(|row| is_main(&row.1))
        .max_by_key(weight)
        .or_else(|| rows.iter().max_by_key(weight))?;
    let model = &pick.0;
    let sum = |field: fn(&(String, String, i64, i64, i64, i64)) -> i64| {
        rows.iter()
            .filter(|row| row.0 == *model)
            .map(field)
            .sum::<i64>()
            .max(0) as u64
    };
    Some(Side {
        model: model.clone(),
        input: sum(|row| row.2),
        cache_read: sum(|row| row.4),
        cache_write: sum(|row| row.5),
    })
}

type UsageRows = Vec<(String, String, i64, i64, i64, i64)>;

async fn usage_rows(conn: &mut PgConnection, run: Uuid) -> Result<UsageRows, sqlx::Error> {
    sqlx::query_as(
        "select model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens
           from run_model_usage where input_command_id=$1",
    )
    .bind(run)
    .fetch_all(conn)
    .await
}

/// Подсказка для одного хода разговора по прошлому законченному ходу и записям о сжатии.
pub async fn for_run(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    run: Uuid,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
) -> Result<Option<CacheHint>, sqlx::Error> {
    let Some(finished_at) = finished_at else {
        return Ok(None);
    };
    let previous: Option<(Uuid, DateTime<Utc>)> = sqlx::query_as(
        "select input_command_id,finished_at from runs
          where conversation_id=$1 and input_command_id<>$2 and finished_at is not null
            and started_at < $3
          order by started_at desc,input_command_id desc limit 1",
    )
    .bind(conversation_id)
    .bind(run)
    .bind(started_at)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((previous_run, previous_finished)) = previous else {
        return Ok(None);
    };
    let current = main_side(&usage_rows(conn, run).await?);
    let before = main_side(&usage_rows(conn, previous_run).await?);
    let idle = (started_at - previous_finished).num_minutes().max(0);
    let compacted: bool = sqlx::query_scalar(
        "select exists(select 1 from compactions
          where conversation_id=$1 and ok is not false
            and created_at > $2 and created_at <= $3)",
    )
    .bind(conversation_id)
    .bind(previous_finished)
    .bind(finished_at)
    .fetch_one(&mut *conn)
    .await?;
    Ok(hint(
        before.as_ref(),
        current.as_ref(),
        Some(idle),
        compacted,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(model: &str, input: u64, read: u64, write: u64) -> Side {
        Side {
            model: model.into(),
            input,
            cache_read: read,
            cache_write: write,
        }
    }

    fn kind(h: Option<CacheHint>) -> Option<CacheHintKind> {
        h.map(|h| h.kind)
    }

    #[test]
    fn idle_gap_means_the_cache_likely_expired() {
        let before = side("opus", 1_000, 40_000, 0);
        let now = side("opus", 500, 2_000, 38_000);
        let h = hint(Some(&before), Some(&now), Some(12), false).unwrap();
        assert_eq!(h.kind, CacheHintKind::Expired);
        assert_eq!(h.idle_minutes, Some(12));
    }

    #[test]
    fn gap_threshold_is_inclusive_at_five_minutes() {
        let before = side("opus", 1_000, 40_000, 0);
        let now = side("opus", 500, 2_000, 38_000);
        assert_eq!(
            kind(hint(
                Some(&before),
                Some(&now),
                Some(IDLE_EXPIRY_MINUTES),
                false
            )),
            Some(CacheHintKind::Expired)
        );
        assert_eq!(
            kind(hint(
                Some(&before),
                Some(&now),
                Some(IDLE_EXPIRY_MINUTES - 1),
                false
            )),
            Some(CacheHintKind::Rebuilt)
        );
    }

    #[test]
    fn model_change_has_its_own_hint() {
        let before = side("opus", 1_000, 40_000, 0);
        let now = side("sonnet", 500, 0, 38_000);
        assert_eq!(
            kind(hint(Some(&before), Some(&now), Some(1), false)),
            Some(CacheHintKind::ModelChanged)
        );
    }

    #[test]
    fn compaction_explains_a_rebuild_on_the_same_model() {
        let before = side("opus", 1_000, 40_000, 0);
        let now = side("opus", 500, 0, 9_000);
        assert_eq!(
            kind(hint(Some(&before), Some(&now), Some(1), true)),
            Some(CacheHintKind::Compaction)
        );
    }

    #[test]
    fn otherwise_the_cache_was_rebuilt() {
        let before = side("opus", 1_000, 40_000, 0);
        let now = side("opus", 500, 0, 9_000);
        let h = hint(Some(&before), Some(&now), Some(1), false).unwrap();
        assert_eq!(h.kind, CacheHintKind::Rebuilt);
        assert_eq!(h.idle_minutes, None);
    }

    #[test]
    fn idle_wins_over_model_change_and_compaction() {
        let before = side("opus", 1_000, 40_000, 0);
        let now = side("sonnet", 500, 0, 9_000);
        assert_eq!(
            kind(hint(Some(&before), Some(&now), Some(30), true)),
            Some(CacheHintKind::Expired)
        );
    }

    #[test]
    fn no_hint_on_the_first_turn_or_without_usage() {
        let now = side("opus", 500, 0, 9_000);
        assert_eq!(hint(None, Some(&now), Some(30), false), None);
        assert_eq!(hint(Some(&now), None, Some(30), false), None);
        assert_eq!(
            hint(Some(&side("opus", 0, 0, 0)), Some(&now), Some(30), false),
            None
        );
    }

    #[test]
    fn no_hint_when_cache_reads_are_healthy() {
        let before = side("opus", 1_000, 40_000, 0);
        // Ровно половина входной стороны прошлого хода ещё не обвал.
        let half = side("opus", 500, before.input_side() / 2, 9_000);
        assert_eq!(hint(Some(&before), Some(&half), Some(30), false), None);
        let fine = side("opus", 500, 39_000, 9_000);
        assert_eq!(hint(Some(&before), Some(&fine), Some(30), false), None);
    }

    #[test]
    fn no_hint_when_the_cache_write_is_small() {
        let before = side("opus", 1_000, 40_000, 0);
        let now = side("opus", 500, 0, LARGE_WRITE_TOKENS - 1);
        assert_eq!(hint(Some(&before), Some(&now), Some(30), false), None);
        let big = side("opus", 500, 0, LARGE_WRITE_TOKENS);
        assert!(hint(Some(&before), Some(&big), Some(30), false).is_some());
    }

    #[test]
    fn harness_without_cache_writes_never_gets_a_hint() {
        // Codex: кеш чтения есть, записи нет.
        let before = side("gpt", 1_000, 40_000, 0);
        let now = side("gpt", 30_000, 100, 0);
        assert_eq!(hint(Some(&before), Some(&now), Some(30), false), None);
    }
}
