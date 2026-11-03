//! Скорость ответа модели по уже сохранённым блокам хода.
//!
//! Генерация — выходные токены модели, делённые только на время, когда модель писала:
//! от первого кадра запроса до конца его вывода. Работа инструментов, ожидание разрешений
//! и время до первого кадра в знаменатель не входят. Запрос начинается с начала хода или
//! с конца предыдущей пачки инструментов.
//!
//! Claude Code передаёт поток каждого блока, включая вход вызова инструмента: блок открывается
//! первым кадром и закрывается последним. OpenCode передаёт поток текста и рассуждений, а
//! вызов показывает, когда его вход уже собран; шаг без видимого первого кадра (только вызов)
//! не даёт честного времени, и такой ход остаётся без скорости. Codex прячет рассуждения и
//! генерацию входа инструментов, поэтому для него скорость не считается вовсе.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

/// Короче этого время генерации тонет в задержке доставки кадров до хаба.
const MIN_GENERATION_MS: i64 = 250;

#[derive(Clone, Debug)]
pub struct TimedBlock {
    pub block_id: Uuid,
    pub kind: String,
    pub agent: String,
    pub model: String,
    pub parent_block_id: Option<Uuid>,
    pub approved_by: Option<String>,
    pub created_seq: i64,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct RunTiming {
    pub provider: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Начало и конец хода дошли до хаба вовремя: блоки шли вживую, а не пачкой после разрыва.
    pub exact: bool,
    /// Во время хода сжимался контекст: его вызов тратит токены без видимых кадров.
    pub compacted: bool,
}

#[derive(Clone, Debug)]
pub struct OutputUsage {
    pub model: String,
    pub agent: String,
    pub output_tokens: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, utoipa::ToSchema)]
pub struct ModelSpeed {
    pub model: String,
    pub output_tokens: i64,
    /// Сумма времени генерации по запросам хода; нет, если его нельзя выделить честно.
    pub generation_ms: Option<i64>,
    pub tokens_per_second: Option<f64>,
    /// Медиана по запросам хода: от начала запроса до первого кадра ответа.
    pub first_token_ms: Option<i64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rules {
    /// Все блоки идут потоком; вызов инструмента — тоже генерация.
    Streamed,
    /// Поток только у текста и рассуждений; вызов виден, когда его вход собран.
    CallReady,
}

fn rules(provider: Option<&str>) -> Option<Rules> {
    match provider {
        Some("claude") => Some(Rules::Streamed),
        Some("opencode") => Some(Rules::CallReady),
        _ => None,
    }
}

pub fn is_main_agent(agent: &str) -> bool {
    matches!(agent, "" | "main" | "orchestrator")
}

fn ms(from: DateTime<Utc>, to: DateTime<Utc>) -> i64 {
    (to - from).num_milliseconds()
}

struct Tool {
    ready: DateTime<Utc>,
    done: DateTime<Utc>,
    trusted: bool,
    model: String,
}

struct Cluster {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    last_ready: DateTime<Utc>,
    trusted: bool,
    model: String,
}

struct Interval {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    model: String,
}

struct Step {
    model: String,
    generation_ms: Option<i64>,
    first_token_ms: Option<i64>,
}

fn steps(
    rules: Rules,
    run: &RunTiming,
    finished: DateTime<Utc>,
    blocks: &[TimedBlock],
) -> Vec<Step> {
    let mut main: Vec<&TimedBlock> = blocks.iter().filter(|b| is_main_agent(&b.agent)).collect();
    main.sort_by_key(|b| (b.created_seq, b.created_at));
    // Блок без своего конца (пустое рассуждение, обрыв) кончается там, где начался следующий.
    let end_of = |index: usize| -> DateTime<Utc> {
        let block = main[index];
        let mut end = finished;
        if let Some(at) = block.finished_at {
            end = end.min(at);
        }
        if let Some(next) = main.get(index + 1) {
            end = end.min(next.created_at);
        }
        end.max(block.created_at)
    };
    let mut generated = Vec::new();
    let mut tools = Vec::new();
    for (index, block) in main.iter().enumerate() {
        match (block.kind.as_str(), rules) {
            ("text" | "reasoning", _) | ("tool_call", Rules::Streamed) => {
                generated.push(Interval {
                    start: block.created_at,
                    end: end_of(index),
                    model: block.model.clone(),
                })
            }
            _ => {}
        }
        if block.kind != "tool_call" {
            continue;
        }
        let result = main
            .iter()
            .find(|b| b.kind == "tool_result" && b.parent_block_id == Some(block.block_id));
        tools.push(match rules {
            Rules::Streamed => {
                let ready = end_of(index);
                Tool {
                    ready,
                    done: result.map_or(ready, |r| r.created_at).max(ready),
                    trusted: true,
                    model: block.model.clone(),
                }
            }
            // Удержанный до решения вызов («Разрешено правилами CLI») приходит позже готовности.
            Rules::CallReady => Tool {
                ready: block.created_at,
                done: block
                    .finished_at
                    .or(result.map(|r| r.created_at))
                    .unwrap_or(finished)
                    .max(block.created_at),
                trusted: block.approved_by.is_none(),
                model: block.model.clone(),
            },
        });
    }
    tools.sort_by_key(|t| t.ready);
    let mut clusters: Vec<Cluster> = Vec::new();
    for tool in tools {
        match clusters.last_mut() {
            Some(last) if tool.ready <= last.end => {
                last.end = last.end.max(tool.done);
                last.last_ready = last.last_ready.max(tool.ready);
                last.trusted &= tool.trusted;
            }
            _ => clusters.push(Cluster {
                start: tool.ready,
                end: tool.done,
                last_ready: tool.ready,
                trusted: tool.trusted,
                model: tool.model,
            }),
        }
    }
    let mut out = Vec::new();
    let mut previous_end: Option<DateTime<Utc>> = None;
    for k in 0..=clusters.len() {
        let cluster = clusters.get(k);
        let in_step: Vec<&Interval> = generated
            .iter()
            .filter(|g| {
                previous_end.is_none_or(|p| g.start > p) && cluster.is_none_or(|c| g.start <= c.end)
            })
            .collect();
        let request_start = previous_end.unwrap_or(run.started_at);
        previous_end = cluster.map(|c| c.end);
        let first = in_step.iter().min_by_key(|g| g.start);
        let Some(first) = first.filter(|f| cluster.is_none_or(|c| f.start <= c.start)) else {
            // Шаг из одного вызова: первый кадр не виден, генерацию не отделить от ожидания.
            if let Some(cluster) = cluster {
                out.push(Step {
                    model: cluster.model.clone(),
                    generation_ms: None,
                    first_token_ms: None,
                });
            }
            continue;
        };
        let mut end = in_step.iter().map(|g| g.end).max().unwrap_or(first.end);
        if let Some(cluster) = cluster
            && rules == Rules::CallReady
        {
            end = end.max(cluster.last_ready);
        }
        let first_token = ms(request_start, first.start);
        out.push(Step {
            model: first.model.clone(),
            generation_ms: cluster
                .is_none_or(|c| c.trusted)
                .then(|| ms(first.start, end)),
            first_token_ms: (first_token >= 0).then_some(first_token),
        });
    }
    out
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    })
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// Скорость хода по моделям основного агента; пусто, если ход нельзя мерить.
pub fn compute(run: &RunTiming, blocks: &[TimedBlock], usage: &[OutputUsage]) -> Vec<ModelSpeed> {
    let Some(rules) = rules(run.provider.as_deref()) else {
        return Vec::new();
    };
    let Some(finished) = run.finished_at else {
        return Vec::new();
    };
    if !run.exact || run.compacted {
        return Vec::new();
    }
    let steps = steps(rules, run, finished, blocks);
    let mut models: Vec<&str> = Vec::new();
    for step in &steps {
        if !models.contains(&step.model.as_str()) {
            models.push(&step.model);
        }
    }
    let main_usage: Vec<&OutputUsage> = usage.iter().filter(|u| is_main_agent(&u.agent)).collect();
    let mut out: Vec<ModelSpeed> = models
        .iter()
        .map(|&model| {
            let own: Vec<&Step> = steps.iter().filter(|s| s.model == model).collect();
            let named: Vec<&&OutputUsage> =
                main_usage.iter().filter(|u| u.model == model).collect();
            // Имя модели в блоке и в итоге хода может разойтись; единственную пару сводим.
            let output = if !named.is_empty() {
                Some(named.iter().map(|u| u.output_tokens).sum::<i64>())
            } else if models.len() == 1 && main_usage.len() == 1 {
                Some(main_usage[0].output_tokens)
            } else {
                None
            };
            // Claude Code отдаёт расход по модели целиком, вместе с субагентами той же модели.
            let shared = rules == Rules::Streamed
                && blocks
                    .iter()
                    .any(|b| !is_main_agent(&b.agent) && b.model == model);
            let generation = own
                .iter()
                .map(|s| s.generation_ms)
                .sum::<Option<i64>>()
                .filter(|ms| *ms >= MIN_GENERATION_MS && !shared);
            let tokens_per_second = generation
                .zip(output.filter(|o| *o > 0))
                .map(|(ms, output)| round1(output as f64 * 1000.0 / ms as f64));
            let first_token_ms = median(
                own.iter()
                    .filter_map(|s| s.first_token_ms.map(|v| v as f64))
                    .collect(),
            )
            .map(|v| v.round() as i64);
            ModelSpeed {
                model: model.to_owned(),
                output_tokens: output.unwrap_or(0),
                generation_ms: generation.filter(|_| tokens_per_second.is_some()),
                tokens_per_second,
                first_token_ms,
            }
        })
        .filter(|s| s.tokens_per_second.is_some() || s.first_token_ms.is_some())
        .collect();
    out.sort_by(|a, b| {
        b.output_tokens
            .cmp(&a.output_tokens)
            .then_with(|| a.model.cmp(&b.model))
    });
    out
}

/// Скорость ходов из списка; ходы без данных в ответе отсутствуют.
pub async fn for_runs(
    conn: &mut PgConnection,
    run_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<ModelSpeed>>, sqlx::Error> {
    if run_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let runs = sqlx::query("select r.input_command_id,r.provider,r.started_at,r.finished_at,r.exact,exists(select 1 from compactions k where k.conversation_id=r.conversation_id and k.created_at>=r.started_at and k.created_at<=coalesce(r.finished_at,'infinity')) compacted from runs r where r.input_command_id=any($1) and r.provider in ('claude','opencode')")
        .bind(run_ids).fetch_all(&mut *conn).await?;
    if runs.is_empty() {
        return Ok(HashMap::new());
    }
    let mut blocks: HashMap<Uuid, Vec<TimedBlock>> = HashMap::new();
    for row in sqlx::query("select input_command_id,block_id,kind,agent,model,parent_block_id,approved_by,created_seq,created_at,finished_at from blocks where input_command_id=any($1)")
        .bind(run_ids).fetch_all(&mut *conn).await?
    {
        blocks.entry(row.try_get("input_command_id")?).or_default().push(TimedBlock {
            block_id: row.try_get("block_id")?,
            kind: row.try_get("kind")?,
            agent: row.try_get("agent")?,
            model: row.try_get("model")?,
            parent_block_id: row.try_get("parent_block_id")?,
            approved_by: row.try_get("approved_by")?,
            created_seq: row.try_get("created_seq")?,
            created_at: row.try_get("created_at")?,
            finished_at: row.try_get("finished_at")?,
        });
    }
    let mut usage: HashMap<Uuid, Vec<OutputUsage>> = HashMap::new();
    for row in sqlx::query("select input_command_id,model,agent,output_tokens from run_model_usage where input_command_id=any($1)")
        .bind(run_ids).fetch_all(&mut *conn).await?
    {
        usage.entry(row.try_get("input_command_id")?).or_default().push(OutputUsage {
            model: row.try_get("model")?,
            agent: row.try_get("agent")?,
            output_tokens: row.try_get("output_tokens")?,
        });
    }
    let mut out = HashMap::new();
    for row in runs {
        let id: Uuid = row.try_get("input_command_id")?;
        let timing = RunTiming {
            provider: row.try_get("provider")?,
            started_at: row.try_get("started_at")?,
            finished_at: row.try_get("finished_at")?,
            exact: row.try_get("exact")?,
            compacted: row.try_get("compacted")?,
        };
        let speed = compute(
            &timing,
            blocks.get(&id).map_or(&[][..], Vec::as_slice),
            usage.get(&id).map_or(&[][..], Vec::as_slice),
        );
        if !speed.is_empty() {
            out.insert(id, speed);
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, utoipa::ToSchema)]
pub struct SpeedStats {
    /// Ходы, по которым посчитана скорость генерации.
    pub runs: i64,
    /// Медиана по ходам.
    pub tokens_per_second: Option<f64>,
    /// Медиана по ходам.
    pub first_token_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, utoipa::ToSchema)]
pub struct ModelSpeedSummary {
    pub provider: String,
    pub model: String,
    /// Последний день диапазона в часовом поясе запроса.
    pub today: SpeedStats,
    /// Весь запрошенный диапазон.
    pub period: SpeedStats,
}

fn stats(values: &[&ModelSpeed]) -> SpeedStats {
    let speeds: Vec<f64> = values.iter().filter_map(|s| s.tokens_per_second).collect();
    SpeedStats {
        runs: speeds.len() as i64,
        tokens_per_second: median(speeds).map(round1),
        first_token_ms: median(
            values
                .iter()
                .filter_map(|s| s.first_token_ms.map(|v| v as f64))
                .collect(),
        )
        .map(|v| v.round() as i64),
    }
}

/// Медианы по моделям основного агента за диапазон и за его последний день.
/// Часовой пояс должен быть уже проверен вызывающим.
pub async fn summary(
    conn: &mut PgConnection,
    owner_id: Uuid,
    provider: Option<&str>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    tz: &str,
) -> Result<Vec<ModelSpeedSummary>, sqlx::Error> {
    let rows: Vec<(Uuid, String, NaiveDate, NaiveDate)> = sqlx::query_as("select r.input_command_id,r.provider,(r.started_at at time zone $5)::date,(($4::timestamptz - interval '1 microsecond') at time zone $5)::date from runs r join conversations c on c.id=r.conversation_id where c.owner_id=$1 and ($2::text is null or r.provider=$2) and r.started_at>=$3 and r.started_at<$4 and r.provider in ('claude','opencode') and r.exact and r.finished_at is not null")
        .bind(owner_id).bind(provider).bind(from).bind(to).bind(tz).fetch_all(&mut *conn).await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let speeds = for_runs(conn, &ids).await?;
    let mut grouped: HashMap<(String, String), (Vec<&ModelSpeed>, Vec<&ModelSpeed>)> =
        HashMap::new();
    for (id, provider, day, last_day) in &rows {
        for speed in speeds.get(id).into_iter().flatten() {
            let entry = grouped
                .entry((provider.clone(), speed.model.clone()))
                .or_default();
            entry.1.push(speed);
            if day == last_day {
                entry.0.push(speed);
            }
        }
    }
    let mut out: Vec<ModelSpeedSummary> = grouped
        .into_iter()
        .map(|((provider, model), (today, period))| ModelSpeedSummary {
            provider,
            model,
            today: stats(&today),
            period: stats(&period),
        })
        .collect();
    out.sort_by(|a, b| {
        a.provider
            .cmp(&b.provider)
            .then_with(|| a.model.cmp(&b.model))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(1_800_000_000_000 + ms).unwrap()
    }

    struct Builder {
        blocks: Vec<TimedBlock>,
    }

    impl Builder {
        fn new() -> Self {
            Self { blocks: Vec::new() }
        }
        fn add(&mut self, kind: &str, start: i64, end: Option<i64>) -> Uuid {
            self.add_full(kind, start, end, "orchestrator", "m", None, None)
        }
        #[allow(clippy::too_many_arguments)]
        fn add_full(
            &mut self,
            kind: &str,
            start: i64,
            end: Option<i64>,
            agent: &str,
            model: &str,
            parent: Option<Uuid>,
            approved_by: Option<&str>,
        ) -> Uuid {
            let id = Uuid::now_v7();
            self.blocks.push(TimedBlock {
                block_id: id,
                kind: kind.into(),
                agent: agent.into(),
                model: model.into(),
                parent_block_id: parent,
                approved_by: approved_by.map(str::to_owned),
                created_seq: self.blocks.len() as i64 + 1,
                created_at: at(start),
                finished_at: end.map(at),
            });
            id
        }
        fn result(&mut self, call: Uuid, at_ms: i64) {
            self.add_full(
                "tool_result",
                at_ms,
                Some(at_ms),
                "orchestrator",
                "m",
                Some(call),
                None,
            );
        }
    }

    fn run(provider: &str, end: i64) -> RunTiming {
        RunTiming {
            provider: Some(provider.into()),
            started_at: at(0),
            finished_at: Some(at(end)),
            exact: true,
            compacted: false,
        }
    }

    fn usage(output: i64) -> Vec<OutputUsage> {
        vec![OutputUsage {
            model: "m".into(),
            agent: "orchestrator".into(),
            output_tokens: output,
        }]
    }

    #[test]
    fn claude_tool_time_in_the_middle_does_not_lower_the_speed() {
        // Первый запрос: 1 с до кадра, 1 с текста и 1 с входа вызова; инструмент 60 с;
        // второй запрос: 0,5 с до кадра, 2 с ответа. Генерация 4 с, 400 токенов.
        let mut b = Builder::new();
        b.add("reasoning", 1_000, Some(2_000));
        let call = b.add("tool_call", 2_000, Some(3_000));
        b.result(call, 63_000);
        b.add("text", 63_500, Some(65_500));
        let speed = compute(&run("claude", 66_000), &b.blocks, &usage(400));
        assert_eq!(speed.len(), 1);
        assert_eq!(speed[0].generation_ms, Some(4_000));
        assert_eq!(speed[0].tokens_per_second, Some(100.0));
        assert_eq!(speed[0].first_token_ms, Some(750));

        // Тот же ход без инструмента в середине даёт ту же скорость.
        let mut flat = Builder::new();
        flat.add("reasoning", 1_000, Some(2_000));
        flat.add("text", 2_000, Some(5_000));
        let speed = compute(&run("claude", 6_000), &flat.blocks, &usage(400));
        assert_eq!(speed[0].tokens_per_second, Some(100.0));
    }

    #[test]
    fn claude_unfinished_reasoning_ends_where_the_next_block_starts() {
        let mut b = Builder::new();
        b.add("reasoning", 1_000, None);
        b.add("text", 3_000, Some(5_000));
        let speed = compute(&run("claude", 9_000), &b.blocks, &usage(400));
        assert_eq!(speed[0].generation_ms, Some(4_000));
    }

    #[test]
    fn claude_permission_wait_counts_as_tool_time() {
        let mut b = Builder::new();
        let call = b.add("tool_call", 500, Some(1_500));
        // Владелец отвечал пять минут: это между готовностью вызова и результатом.
        b.result(call, 301_500);
        b.add("text", 302_000, Some(303_000));
        let speed = compute(&run("claude", 303_100), &b.blocks, &usage(200));
        assert_eq!(speed[0].generation_ms, Some(2_000));
        assert_eq!(speed[0].tokens_per_second, Some(100.0));
    }

    #[test]
    fn claude_subagent_of_the_same_model_hides_the_speed_but_not_the_first_token() {
        let mut b = Builder::new();
        let call = b.add("tool_call", 1_000, Some(2_000));
        b.add_full("text", 3_000, Some(3_000), "Explore", "m", Some(call), None);
        b.result(call, 10_000);
        b.add("text", 11_000, Some(12_000));
        let speed = compute(&run("claude", 12_000), &b.blocks, &usage(200));
        assert_eq!(speed[0].tokens_per_second, None);
        assert_eq!(speed[0].first_token_ms, Some(1_000));
    }

    #[test]
    fn opencode_generation_runs_until_the_call_is_ready() {
        // Рассуждение 1–2 с, затем вход вызова до готовности на 3 с, инструмент до 30 с.
        let mut b = Builder::new();
        b.add("reasoning", 1_000, Some(2_000));
        let call = b.add_full(
            "tool_call",
            3_000,
            Some(30_000),
            "orchestrator",
            "m",
            None,
            None,
        );
        b.add_full(
            "tool_result",
            30_000,
            Some(30_000),
            "orchestrator",
            "m",
            Some(call),
            None,
        );
        b.add("text", 31_000, Some(33_000));
        let speed = compute(&run("opencode", 33_500), &b.blocks, &usage(400));
        assert_eq!(speed[0].generation_ms, Some(4_000));
        assert_eq!(speed[0].tokens_per_second, Some(100.0));
        assert_eq!(speed[0].first_token_ms, Some(1_000));
    }

    #[test]
    fn opencode_step_with_only_a_call_has_no_honest_speed() {
        let mut b = Builder::new();
        b.add("reasoning", 1_000, Some(2_000));
        let first = b.add("tool_call", 2_500, Some(5_000));
        b.result(first, 5_000);
        // Следующий шаг сразу вызов: его первый кадр не виден.
        let second = b.add("tool_call", 7_000, Some(9_000));
        b.result(second, 9_000);
        b.add("text", 9_500, Some(10_500));
        let speed = compute(&run("opencode", 11_000), &b.blocks, &usage(300));
        assert_eq!(speed[0].tokens_per_second, None);
        assert!(speed[0].first_token_ms.is_some());
    }

    #[test]
    fn opencode_call_held_for_a_decision_has_no_honest_ready_time() {
        let mut b = Builder::new();
        b.add("text", 1_000, Some(2_000));
        let call = b.add_full(
            "tool_call",
            20_000,
            Some(20_100),
            "orchestrator",
            "m",
            None,
            Some("cli_policy"),
        );
        b.result(call, 20_100);
        b.add("text", 21_000, Some(22_000));
        let speed = compute(&run("opencode", 22_500), &b.blocks, &usage(300));
        assert_eq!(speed[0].tokens_per_second, None);
    }

    #[test]
    fn opencode_parallel_calls_of_one_step_stay_one_step() {
        let mut b = Builder::new();
        b.add("text", 1_000, Some(2_000));
        let a = b.add("tool_call", 2_500, Some(9_000));
        let c = b.add("tool_call", 3_000, Some(8_000));
        b.result(c, 8_000);
        b.result(a, 9_000);
        b.add("text", 9_500, Some(11_500));
        let speed = compute(&run("opencode", 12_000), &b.blocks, &usage(400));
        assert_eq!(speed[0].generation_ms, Some(4_000));
    }

    #[test]
    fn codex_late_runs_and_compactions_give_nothing() {
        let mut b = Builder::new();
        b.add("text", 1_000, Some(2_000));
        assert!(compute(&run("codex", 3_000), &b.blocks, &usage(100)).is_empty());
        let mut late = run("claude", 3_000);
        late.exact = false;
        assert!(compute(&late, &b.blocks, &usage(100)).is_empty());
        let mut compacted = run("claude", 3_000);
        compacted.compacted = true;
        assert!(compute(&compacted, &b.blocks, &usage(100)).is_empty());
        let mut open = run("claude", 3_000);
        open.finished_at = None;
        assert!(compute(&open, &b.blocks, &usage(100)).is_empty());
    }

    #[test]
    fn too_short_generation_is_noise() {
        let mut b = Builder::new();
        b.add("text", 1_000, Some(1_100));
        let speed = compute(&run("claude", 1_200), &b.blocks, &usage(50));
        assert_eq!(speed[0].tokens_per_second, None);
        assert_eq!(speed[0].generation_ms, None);
        assert_eq!(speed[0].first_token_ms, Some(1_000));
    }

    #[test]
    fn single_usage_row_pairs_with_a_differently_named_block_model() {
        let mut b = Builder::new();
        b.add_full(
            "text",
            1_000,
            Some(2_000),
            "orchestrator",
            "claude-x",
            None,
            None,
        );
        let speed = compute(&run("claude", 2_100), &b.blocks, &usage(100));
        assert_eq!(speed[0].model, "claude-x");
        assert_eq!(speed[0].tokens_per_second, Some(100.0));
    }

    #[test]
    fn median_of_even_count_is_the_mean_of_the_middle() {
        assert_eq!(median(vec![4.0, 1.0, 3.0, 2.0]), Some(2.5));
        assert_eq!(median(vec![]), None);
    }
}
