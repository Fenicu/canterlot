//! Итоги тестовых прогонов из вывода команд хода: nextest, Playwright и vitest.
//!
//! Сводка считается, когда ход закрывается, и лежит рядом с его git-фактами: итог
//! подписчику, `session_inspect` и PWA читают одно и то же.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

/// Длина короткого хеша в отметке «до коммита».
pub const SHORT_SHA: usize = 8;

/// Сводка одного прогона. Нулевые счётчики, кроме `passed`, в текст не попадают.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TestSummary {
    /// `nextest`, `playwright` или `vitest`.
    pub tool: String,
    pub passed: u32,
    pub failed: u32,
    pub flaky: u32,
    pub skipped: u32,
    pub did_not_run: u32,
    pub timed_out: u32,
    pub duration_ms: Option<i64>,
    /// Время блока результата, в котором нашлась сводка.
    #[serde(with = "crate::model::utc_time")]
    pub ran_at: DateTime<Utc>,
    /// Короткий хеш первого коммита хода после прогона; `None`, если такого нет.
    pub stale_before: Option<String>,
}

/// Разобранные счётчики без привязки к ходу и коммитам.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Counts {
    passed: u32,
    failed: u32,
    flaky: u32,
    skipped: u32,
    did_not_run: u32,
    timed_out: u32,
    duration_ms: Option<i64>,
}

/// Коммит хода с временем: форма `runs.git.commits`.
#[derive(Deserialize)]
struct CommitTime {
    sha: String,
    #[serde(default)]
    committed_at_ms: i64,
}

#[derive(Deserialize)]
struct StoredGit {
    #[serde(default)]
    commits: Vec<CommitTime>,
}

const TOOLS: [&str; 3] = ["nextest", "playwright", "vitest"];
const SHELLS: [&str; 4] = ["bash", "shell", "exec_command", "exec"];

/// Имя инструмента строки результата, если это оболочка любого харнесса.
fn shell_name(raw: &str) -> bool {
    SHELLS.contains(&raw.trim().to_ascii_lowercase().as_str())
}

/// Команды могли прийти от старой версии воркера: отметки о времени коммита нет.
fn first_code_commit_after(ran_at: DateTime<Utc>, commits: &[CommitTime]) -> Option<String> {
    let ran_at = ran_at.timestamp_millis();
    commits
        .iter()
        .filter(|commit| commit.committed_at_ms > ran_at && !commit.sha.is_empty())
        .min_by_key(|commit| commit.committed_at_ms)
        .map(|commit| commit.sha.chars().take(SHORT_SHA).collect())
}

/// Снимает CSI-последовательности ANSI: отчёты печатают цвета даже без терминала.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for ch in chars.by_ref() {
                if ('\x40'..='\x7e').contains(&ch) {
                    break;
                }
            }
            continue;
        }
        // OSC: ESC ] ... BEL или ST.
        if chars.peek() == Some(&']') {
            chars.next();
            while let Some(ch) = chars.next() {
                if ch == '\x07' {
                    break;
                }
                if ch == '\x1b' && chars.peek() == Some(&'\\') {
                    chars.next();
                    break;
                }
            }
        }
    }
    out
}

fn seconds_ms(raw: &str) -> Option<i64> {
    let number = raw.trim().strip_suffix('s')?;
    let seconds: f64 = number.trim().parse().ok()?;
    Some((seconds * 1000.0).round() as i64)
}

fn duration_ms(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    for (suffix, factor) in [
        ("ms", 1.0),
        ("s", 1000.0),
        ("m", 60_000.0),
        ("h", 3_600_000.0),
    ] {
        if let Some(number) = raw.strip_suffix(suffix) {
            let value: f64 = number.trim().parse().ok()?;
            return Some((value * factor).round() as i64);
        }
    }
    None
}

/// Делит строку по запятым верхнего уровня: запятые внутри скобок не считаются.
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(text[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim());
    parts
}

fn leading_count(text: &str) -> Option<(u32, &str)> {
    let digits = text
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .last()?;
    let end = digits.0 + digits.1.len_utf8();
    let count = text[..end].parse().ok()?;
    Some((count, text[end..].trim()))
}

/// `Summary [ 229.949s] 3870 tests run: 3870 passed (1 flaky), 54 skipped`
fn parse_nextest(text: &str) -> Option<Counts> {
    let text = strip_ansi(text);
    let mut last = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r').trim_start();
        let Some(rest) = line.strip_prefix("Summary [") else {
            continue;
        };
        let Some((seconds, rest)) = rest.split_once(']') else {
            continue;
        };
        let Some(duration_ms) = seconds_ms(seconds) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some((_, rest)) = leading_count(rest) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest
            .strip_prefix("tests run:")
            .or_else(|| rest.strip_prefix("test run:"))
        else {
            continue;
        };
        let mut counts = Counts {
            duration_ms: Some(duration_ms),
            ..Counts::default()
        };
        let mut known = false;
        for part in split_top_level(rest) {
            let Some((count, tail)) = leading_count(part) else {
                continue;
            };
            let tail_lower = tail.to_ascii_lowercase();
            let (name, paren) = match tail_lower.split_once('(') {
                Some((name, rest)) => (name.trim(), rest.trim_end_matches(')').trim()),
                None => (tail_lower.as_str(), ""),
            };
            match name {
                "passed" => counts.passed = count,
                "failed" => counts.failed = count,
                "skipped" => counts.skipped = count,
                "timed out" => counts.timed_out = count,
                _ => {}
            }
            if matches!(name, "passed" | "failed" | "skipped" | "timed out") {
                known = true;
            }
            // «passed (3 slow, 2 flaky)»: неустойчивые входят в число прошедших.
            for extra in split_top_level(paren) {
                let Some((extra_count, extra_tail)) = leading_count(extra) else {
                    continue;
                };
                if extra_tail.eq_ignore_ascii_case("flaky") {
                    counts.flaky = extra_count;
                }
            }
        }
        if known {
            last = Some(counts);
        }
    }
    last
}

fn playwright_line(line: &str) -> Option<(u32, &'static str, Option<i64>)> {
    let (body, duration) = match line.rsplit_once(" (") {
        Some((body, tail)) if tail.ends_with(')') => {
            (body.trim(), duration_ms(&tail[..tail.len() - 1]))
        }
        _ => (line, None),
    };
    let (count, name) = body.split_once(' ')?;
    let count: u32 = count.parse().ok()?;
    let name = match name.trim() {
        "passed" => "passed",
        "failed" => "failed",
        "flaky" => "flaky",
        "skipped" => "skipped",
        "did not run" => "did_not_run",
        _ => return None,
    };
    Some((count, name, duration))
}

/// Хвост list-репортёра: строки счётчиков в любом порядке, длительность на одной из них.
fn parse_playwright(text: &str) -> Option<Counts> {
    let text = strip_ansi(text);
    let mut counts = Counts::default();
    let mut seen = false;
    for line in text.lines().rev() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            if seen {
                break;
            }
            continue;
        }
        let Some((count, name, duration)) = playwright_line(line) else {
            if seen {
                break;
            }
            continue;
        };
        seen = true;
        match name {
            "passed" => counts.passed = count,
            "failed" => counts.failed = count,
            "flaky" => counts.flaky = count,
            "skipped" => counts.skipped = count,
            "did_not_run" => counts.did_not_run = count,
            _ => {}
        }
        if counts.duration_ms.is_none() {
            counts.duration_ms = duration;
        }
    }
    (seen && (counts.passed > 0 || counts.failed > 0)).then_some(counts)
}

fn vitest_tests(rest: &str) -> Option<Counts> {
    let mut counts = Counts::default();
    let mut known = false;
    let rest = match rest.rsplit_once('(') {
        Some((body, _)) => body.trim(),
        None => rest,
    };
    for part in rest.split('|') {
        let Some((count, name)) = leading_count(part.trim()) else {
            continue;
        };
        match name.to_ascii_lowercase().as_str() {
            "passed" => counts.passed = count,
            "failed" => counts.failed = count,
            "skipped" => counts.skipped = count,
            _ => continue,
        }
        known = true;
    }
    known.then_some(counts)
}

/// `Tests  3 failed | 1504 passed (1507)` и `Duration  12.34s (transform …)`.
fn parse_vitest(text: &str) -> Option<Counts> {
    let text = strip_ansi(text);
    let mut candidates: Vec<Counts> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r').trim_start();
        let Some(rest) = line.strip_prefix("Tests") else {
            if let Some(duration) = line.strip_prefix("Duration")
                && let Some(last) = candidates.last_mut()
                && let Some(token) = duration.split_whitespace().next()
            {
                last.duration_ms = duration_ms(token);
            }
            continue;
        };
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        if let Some(counts) = vitest_tests(rest.trim()) {
            candidates.push(counts);
        }
    }
    candidates.pop()
}

fn parsed(text: &str) -> Vec<(&'static str, Counts)> {
    let mut found = Vec::new();
    if let Some(counts) = parse_nextest(text) {
        found.push(("nextest", counts));
    }
    if let Some(counts) = parse_playwright(text) {
        found.push(("playwright", counts));
    }
    if let Some(counts) = parse_vitest(text) {
        found.push(("vitest", counts));
    }
    found
}

/// Последний прогон каждого инструмента среди результатов оболочки этого хода.
async fn collect(conn: &mut PgConnection, command: Uuid) -> Result<Vec<TestSummary>, sqlx::Error> {
    let rows: Vec<(Uuid, String, DateTime<Utc>)> = sqlx::query_as(
        "select r.block_id, lower(coalesce(c.tool_name, r.tool_name, '')),
                coalesce(r.finished_at, r.updated_at, r.created_at)
         from blocks r
         left join blocks c on c.block_id = r.parent_block_id and c.conversation_id = r.conversation_id
         where r.input_command_id = $1 and r.kind = 'tool_result'
           and lower(coalesce(c.tool_name, r.tool_name, '')) in ('bash', 'shell', 'exec_command', 'exec')
         order by coalesce(r.created_seq, 0), r.created_at, r.block_id",
    )
    .bind(command)
    .fetch_all(&mut *conn)
    .await?;
    let mut found: BTreeMap<&'static str, TestSummary> = BTreeMap::new();
    for (block_id, tool_name, ran_at) in rows {
        if !shell_name(&tool_name) {
            continue;
        }
        let text = crate::blocks::read_text(conn, block_id).await?;
        for (tool, counts) in parsed(&text) {
            found.insert(
                tool,
                TestSummary {
                    tool: tool.to_owned(),
                    passed: counts.passed,
                    failed: counts.failed,
                    flaky: counts.flaky,
                    skipped: counts.skipped,
                    did_not_run: counts.did_not_run,
                    timed_out: counts.timed_out,
                    duration_ms: counts.duration_ms,
                    ran_at,
                    stale_before: None,
                },
            );
        }
    }
    let git: Option<Value> = sqlx::query_scalar("select git from runs where input_command_id = $1")
        .bind(command)
        .fetch_optional(&mut *conn)
        .await?
        .flatten();
    let commits = git
        .and_then(|value| serde_json::from_value::<StoredGit>(value).ok())
        .map(|git| git.commits)
        .unwrap_or_default();
    let mut out: Vec<TestSummary> = Vec::new();
    for tool in TOOLS {
        let Some(mut summary) = found.remove(tool) else {
            continue;
        };
        summary.stale_before = first_code_commit_after(summary.ran_at, &commits);
        out.push(summary);
    }
    Ok(out)
}

/// Пересчитывает сводки хода после того, как его git-факты записаны.
pub async fn store_in_tx(conn: &mut PgConnection, command: Uuid) -> Result<(), sqlx::Error> {
    let summaries = collect(conn, command).await?;
    let value =
        serde_json::to_value(&summaries).map_err(|error| sqlx::Error::Encode(Box::new(error)))?;
    sqlx::query("update runs set tests = $2 where input_command_id = $1")
        .bind(command)
        .bind(value)
        .execute(conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ran(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(ms).unwrap()
    }

    fn commit(sha: &str, at_ms: i64) -> CommitTime {
        CommitTime {
            sha: sha.to_owned(),
            committed_at_ms: at_ms,
        }
    }

    #[test]
    fn nextest_reads_the_last_summary() {
        let text = "\x1b[1m   Compiling\x1b[0m\n\
                    \tSummary [ 229.949s] 3870 tests run: 3870 passed (1 flaky), 54 skipped\n\
                    \tSummary [ 195.532s] 3946 tests run: 3944 passed (3 slow, 2 flaky), 2 failed, 54 skipped\n";
        assert_eq!(
            parse_nextest(text),
            Some(Counts {
                passed: 3944,
                failed: 2,
                flaky: 2,
                skipped: 54,
                duration_ms: Some(195_532),
                ..Counts::default()
            })
        );
    }

    #[test]
    fn nextest_counts_a_timeout_and_ignores_incomplete_lines() {
        let text = "Summary [ 12.5s] 3 tests run: 2 passed, 1 timed out\n\
                    Summary [ ??? ] 1 test run:\n";
        assert_eq!(
            parse_nextest(text),
            Some(Counts {
                passed: 2,
                timed_out: 1,
                duration_ms: Some(12_500),
                ..Counts::default()
            })
        );
    }

    #[test]
    fn nextest_keeps_the_tail_of_a_head_truncated_output() {
        let text = "…run: 9 passed, 1 failed, 2 skipped\n\
                    Summary [ 1.2s] 3 tests run: 3 passed\n";
        assert_eq!(
            parse_nextest(text),
            Some(Counts {
                passed: 3,
                duration_ms: Some(1_200),
                ..Counts::default()
            })
        );
    }

    #[test]
    fn playwright_reads_counters_in_any_order() {
        let text = "Running 32 tests using 2 workers\n\
                    \x1b[31m  4 failed\x1b[0m\n\
                    \x1b[33m  2 flaky\x1b[0m\n\
                      92 skipped\n\
                      2 did not run\n\
                      2172 passed (8.0m)\n";
        assert_eq!(
            parse_playwright(text),
            Some(Counts {
                passed: 2172,
                failed: 4,
                flaky: 2,
                skipped: 92,
                did_not_run: 2,
                duration_ms: Some(480_000),
                ..Counts::default()
            })
        );
    }

    #[test]
    fn playwright_reads_a_single_passed_run() {
        assert_eq!(
            parse_playwright("  1 passed (1.2s)\n"),
            Some(Counts {
                passed: 1,
                duration_ms: Some(1_200),
                ..Counts::default()
            })
        );
    }

    #[test]
    fn playwright_reads_a_run_where_everything_failed() {
        assert_eq!(
            parse_playwright("  Error: boom\n  3 failed (2.5s)\n"),
            Some(Counts {
                failed: 3,
                duration_ms: Some(2_500),
                ..Counts::default()
            })
        );
    }

    #[test]
    fn playwright_takes_only_the_last_summary() {
        let text = "  1 passed (1.0s)\n  1 failed\n\
                    \n\
                      9 passed (38s)\n";
        assert_eq!(
            parse_playwright(text),
            Some(Counts {
                passed: 9,
                duration_ms: Some(38_000),
                ..Counts::default()
            })
        );
    }

    #[test]
    fn playwright_ignores_a_bare_test_title() {
        assert_eq!(parse_playwright("  4 failed attempts in the log\n"), None);
    }

    #[test]
    fn vitest_reads_the_last_tests_line_and_duration() {
        let text = " Test Files  176 passed (176)\n\
                    \x1b[32m      Tests  1507 passed (1507)\x1b[0m\n\
                    \x1b[32m   Duration  12.34s (transform 1.2s, tests 5.0s)\x1b[0m\n";
        assert_eq!(
            parse_vitest(text),
            Some(Counts {
                passed: 1507,
                duration_ms: Some(12_340),
                ..Counts::default()
            })
        );
    }

    #[test]
    fn vitest_reads_failures_and_skips() {
        let text = "      Tests  3 failed | 2 skipped | 1502 passed (1507)\n\
                    \x1b[2m   Duration  1.5m\x1b[0m\n";
        assert_eq!(
            parse_vitest(text),
            Some(Counts {
                passed: 1502,
                failed: 3,
                skipped: 2,
                duration_ms: Some(90_000),
                ..Counts::default()
            })
        );
    }

    #[test]
    fn vitest_takes_the_last_complete_run() {
        let text = "      Tests  5 passed (5)\n   Duration  1.0s\n\
                    \x1b[90mstdout | test | done\x1b[0m\n\
                          Tests  7 passed (7)\n   Duration  2.0s\n";
        assert_eq!(
            parse_vitest(text),
            Some(Counts {
                passed: 7,
                duration_ms: Some(2_000),
                ..Counts::default()
            })
        );
    }

    #[test]
    fn a_tool_result_can_hold_two_tools() {
        let text = "      Tests  12 passed (12)\n   Duration  3.0s\n\
                      32 passed (38s)\n";
        let found = parsed(text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "playwright");
        assert_eq!(found[1].0, "vitest");
    }

    #[test]
    fn stale_marker_is_the_first_code_commit_after_the_run() {
        let commits = [
            commit("cccccccccccccccc", 13_000),
            commit("bbbbbbbbbbbbbbbb", 11_000),
            commit("aaaaaaaaaaaaaaaa", 9_000),
        ];
        assert_eq!(
            first_code_commit_after(ran(10_000), &commits).as_deref(),
            Some("bbbbbbbb")
        );
    }

    #[test]
    fn a_run_after_the_last_commit_has_no_marker() {
        let commits = [commit("aaaaaaaaaaaaaaaa", 9_000)];
        assert_eq!(first_code_commit_after(ran(10_000), &commits), None);
    }

    #[test]
    fn a_commit_in_the_same_millisecond_is_not_after_the_run() {
        let commits = [commit("aaaaaaaaaaaaaaaa", 10_000)];
        assert_eq!(first_code_commit_after(ran(10_000), &commits), None);
    }

    #[test]
    fn a_turn_without_commits_has_no_marker() {
        assert_eq!(first_code_commit_after(ran(10_000), &[]), None);
    }

    #[test]
    fn a_commit_without_a_time_does_not_claim_to_be_later() {
        let commits = [commit("aaaaaaaaaaaaaaaa", 0)];
        assert_eq!(first_code_commit_after(ran(10_000), &commits), None);
    }

    #[test]
    fn shell_names_cover_every_harness() {
        for name in ["Bash", "bash", "shell", "exec_command", "Exec"] {
            assert!(shell_name(name), "{name}");
        }
        assert!(!shell_name("read"));
        assert!(!shell_name("BashOutput"));
    }
}
