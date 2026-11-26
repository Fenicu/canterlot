//! Сводка устаревших задач плана для оркестратора.
//!
//! Задача движется сама только при связанном делегате; остальное держится на памяти агента,
//! а она теряется при сжатии контекста. Перед выдачей ввода оркестратору хаб смотрит на его
//! открытые задачи и, если какие-то выглядят забытыми, кладёт одну служебную строку.
//!
//! Повторов нет: каждая задача называется один раз на пару «причина и ревизия» (`noted`),
//! а сводки идут не чаще раза в три часа. Строка плана в этом пути только читается,
//! поэтому порядок блокировок плана (`session_tools::plan`) не задет: берётся лишь строка
//! `plan_reminders` этого оркестратора под уже взятой очередью разговора.

use std::{collections::BTreeMap, fmt::Write as _};

use serde_json::{Value, json};
use sqlx::{PgConnection, types::Json};
use uuid::Uuid;

/// Сколько задача ждёт приёма на проверке у простаивающего исполнителя.
pub const REVIEW_IDLE_SECS: i64 = 2 * 3600;
/// Задача в работе, а исполнитель остановлен, упал, принят или в архиве.
pub const STOPPED_EXECUTOR_SECS: i64 = 30 * 60;
/// Задача в работе без исполнителя: оркестратор может делать её сам, поэтому срок длинный.
pub const NO_EXECUTOR_SECS: i64 = 6 * 3600;
/// Открытая задача не менялась сутки…
pub const UNCHANGED_SECS: i64 = 24 * 3600;
/// …пока оркестратор закончил столько ходов.
pub const UNCHANGED_RUNS: i64 = 5;
/// Не чаще одной сводки за этот срок; слитая ветка идёт вне очереди.
pub const DIGEST_INTERVAL_SECS: i64 = 3 * 3600;
/// Строк задач в одной сводке.
pub const MAX_LINES: usize = 5;
/// Название задачи в строке сводки.
const TITLE_CHARS: usize = 60;

/// Почему задача попала в сводку. Порядок вариантов — порядок проверки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// На проверке, а исполнителя уже нет: принят, в архиве, в корзине или удалён.
    ReviewOrphan,
    /// На проверке дольше [`REVIEW_IDLE_SECS`] у простаивающего исполнителя.
    ReviewIdle,
    /// В работе, а исполнитель не живёт.
    ExecutorStopped,
    /// В работе без исполнителя дольше [`NO_EXECUTOR_SECS`].
    NoExecutor,
    /// Не менялась сутки за [`UNCHANGED_RUNS`] ходов оркестратора.
    Unchanged,
}

impl Reason {
    fn key(self) -> &'static str {
        match self {
            Self::ReviewOrphan => "review_orphan",
            Self::ReviewIdle => "review_idle",
            Self::ExecutorStopped | Self::NoExecutor => "no_executor",
            Self::Unchanged => "unchanged",
        }
    }
}

/// Факты одной открытой задачи, собранные одним запросом.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TaskFacts {
    pub id: Uuid,
    pub number: i32,
    pub title: String,
    pub state: String,
    pub revision: i64,
    pub executor_conversation_id: Option<Uuid>,
    /// Секунд с последней правки задачи.
    pub age_secs: i64,
    /// Исполнитель принят, в архиве или в корзине.
    pub executor_gone: bool,
    /// У исполнителя есть исполнение, которое не остановлено и не упало.
    pub executor_live: bool,
    /// У исполнителя идёт ход.
    pub executor_busy: bool,
    /// Ходов оркестратора, закончившихся после правки задачи.
    pub orchestrator_runs: i64,
}

/// Причина или `None`, если задача выглядит живой.
pub fn classify(task: &TaskFacts) -> Option<Reason> {
    match task.state.as_str() {
        "in_review" => {
            if task.executor_conversation_id.is_none() || task.executor_gone {
                Some(Reason::ReviewOrphan)
            } else if !task.executor_busy && task.age_secs >= REVIEW_IDLE_SECS {
                Some(Reason::ReviewIdle)
            } else {
                None
            }
        }
        "in_progress" => {
            if task.executor_conversation_id.is_none() {
                return (task.age_secs >= NO_EXECUTOR_SECS).then_some(Reason::NoExecutor);
            }
            if (task.executor_gone || !task.executor_live) && task.age_secs >= STOPPED_EXECUTOR_SECS
            {
                return Some(Reason::ExecutorStopped);
            }
            unchanged(task)
        }
        "blocked" => unchanged(task),
        _ => None,
    }
}

fn unchanged(task: &TaskFacts) -> Option<Reason> {
    (!task.executor_busy
        && task.age_secs >= UNCHANGED_SECS
        && task.orchestrator_runs >= UNCHANGED_RUNS)
        .then_some(Reason::Unchanged)
}

/// «26h» до двух суток, дальше «3d».
fn age(secs: i64) -> String {
    let hours = secs / 3600;
    if hours < 48 {
        format!("{}h", hours.max(1))
    } else {
        format!("{}d", hours / 24)
    }
}

fn title(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= TITLE_CHARS {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(TITLE_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// Строка задачи в сводке: английский текст для агента, с инструментом, который её чинит.
pub fn line(task: &TaskFacts, reason: Reason) -> String {
    let head = format!("#{} \"{}\"", task.number, title(&task.title));
    match reason {
        Reason::ReviewOrphan => format!(
            "{head} is in_review, but its executor is accepted, archived or deleted: set done or cancelled with plan_update."
        ),
        Reason::ReviewIdle => format!(
            "{head} has waited in_review for {} with an idle executor: session_accept {} or reopen it with plan_update.",
            age(task.age_secs),
            task.executor_conversation_id.unwrap_or_default()
        ),
        Reason::ExecutorStopped => format!(
            "{head} is in_progress, but its executor is not running: set its real state with plan_update."
        ),
        Reason::NoExecutor => format!(
            "{head} is in_progress for {} with no executor: set its real state with plan_update.",
            age(task.age_secs)
        ),
        Reason::Unchanged => format!(
            "{head} is {} and unchanged for {}: update or close it.",
            task.state,
            age(task.age_secs)
        ),
    }
}

/// Текст сводки по выбранным строкам; задач больше [`MAX_LINES`] — счётчик остальных.
pub fn digest(lines: &[String]) -> String {
    let mut text = String::from(
        "[canterlot] Plan check: these tasks look stale. Fix them with plan_update or session_accept.",
    );
    for line in lines.iter().take(MAX_LINES) {
        text.push('\n');
        text.push_str(line);
    }
    if lines.len() > MAX_LINES {
        let _ = write!(
            text,
            "\n…and {} more (plan_list shows all).",
            lines.len() - MAX_LINES
        );
    }
    text
}

const FACTS: &str = "
select t.id, t.number, t.title, t.state, t.revision, t.executor_conversation_id,
       extract(epoch from (clock_timestamp() - t.updated_at))::bigint as age_secs,
       coalesce(e.accepted_at is not null or e.trashed_at is not null
                or e.archived_at is not null, false) as executor_gone,
       exists(select 1 from executions x where x.conversation_id = t.executor_conversation_id
                and x.state not in ('stopped', 'failed', 'owner_lost')) as executor_live,
       exists(select 1 from executions x
                join queue_state q on q.conversation_id = x.conversation_id
              where x.conversation_id = t.executor_conversation_id
                and x.state not in ('stopped', 'failed', 'owner_lost')
                and (q.run_slot <> 'free'
                     or x.phase in ('generating', 'running_tool', 'context_maintenance',
                                    'awaiting_permission', 'awaiting_answer'))) as executor_busy,
       (select count(*) from runs r where r.conversation_id = t.orchestrator_conversation_id
          and r.finished_at > t.updated_at) as orchestrator_runs
from plan_tasks t
left join conversations e on e.id = t.executor_conversation_id
where t.orchestrator_conversation_id = $1 and t.archived_at is null
  and t.state in ('in_progress', 'in_review', 'blocked')
order by t.number";

/// Сводка для оркестратора, если она нужна сейчас: `(текст, номера задач)`.
/// Вызывающий держит очередь разговора; состояние напоминаний обновляется здесь же,
/// поэтому откат выдачи откатывает и его.
pub async fn take_in_tx(
    conn: &mut PgConnection,
    orchestrator: Uuid,
) -> Result<Option<(String, Value)>, sqlx::Error> {
    let open: bool = sqlx::query_scalar(
        "select exists(select 1 from plan_tasks where orchestrator_conversation_id = $1
           and archived_at is null and state in ('in_progress', 'in_review', 'blocked'))",
    )
    .bind(orchestrator)
    .fetch_one(&mut *conn)
    .await?;
    if !open {
        return Ok(None);
    }
    sqlx::query(
        "insert into plan_reminders (orchestrator_conversation_id) values ($1)
         on conflict do nothing",
    )
    .bind(orchestrator)
    .execute(&mut *conn)
    .await?;
    let (due, noted): (bool, Json<BTreeMap<Uuid, String>>) = sqlx::query_as(
        "select last_digest_at is null
                or last_digest_at <= clock_timestamp() - make_interval(secs => $2),
                noted
         from plan_reminders where orchestrator_conversation_id = $1 for update",
    )
    .bind(orchestrator)
    .bind(DIGEST_INTERVAL_SECS as f64)
    .fetch_one(&mut *conn)
    .await?;
    let tasks: Vec<TaskFacts> = sqlx::query_as(FACTS)
        .bind(orchestrator)
        .fetch_all(&mut *conn)
        .await?;
    let mut keep = BTreeMap::new();
    let mut picked = Vec::new();
    for task in &tasks {
        let Some(reason) = classify(task) else {
            if let Some(key) = noted.0.get(&task.id) {
                keep.insert(task.id, key.clone());
            }
            continue;
        };
        let key = format!("{}:{}", reason.key(), task.revision);
        if noted.0.get(&task.id) == Some(&key) {
            keep.insert(task.id, key);
            continue;
        }
        if due {
            picked.push((task, reason, key));
        } else if let Some(key) = noted.0.get(&task.id) {
            keep.insert(task.id, key.clone());
        }
    }
    if picked.is_empty() {
        // Закрытые и снятые с доски задачи уходят из `noted`, чтобы строка не росла.
        if keep.len() != noted.0.len() {
            store(conn, orchestrator, &keep, false).await?;
        }
        return Ok(None);
    }
    let lines: Vec<String> = picked
        .iter()
        .map(|(task, reason, _)| line(task, *reason))
        .collect();
    let numbers: Vec<i32> = picked.iter().map(|(task, _, _)| task.number).collect();
    for (task, _, key) in picked {
        keep.insert(task.id, key);
    }
    store(conn, orchestrator, &keep, due).await?;
    Ok(Some((digest(&lines), json!(numbers))))
}

async fn store(
    conn: &mut PgConnection,
    orchestrator: Uuid,
    noted: &BTreeMap<Uuid, String>,
    digested: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update plan_reminders set noted = $2,
           last_digest_at = case when $3 then clock_timestamp() else last_digest_at end
         where orchestrator_conversation_id = $1",
    )
    .bind(orchestrator)
    .bind(Json(noted))
    .bind(digested)
    .execute(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(state: &str) -> TaskFacts {
        TaskFacts {
            id: Uuid::nil(),
            number: 7,
            title: "Свести план".into(),
            state: state.into(),
            revision: 3,
            executor_conversation_id: Some(Uuid::nil()),
            age_secs: 0,
            executor_gone: false,
            executor_live: true,
            executor_busy: false,
            orchestrator_runs: 0,
        }
    }

    #[test]
    fn review_waits_two_hours_unless_the_executor_is_gone() {
        let mut t = task("in_review");
        t.age_secs = REVIEW_IDLE_SECS - 1;
        assert_eq!(classify(&t), None);
        t.age_secs = REVIEW_IDLE_SECS;
        assert_eq!(classify(&t), Some(Reason::ReviewIdle));
        t.executor_busy = true;
        assert_eq!(classify(&t), None, "a running executor is not idle");
        t.age_secs = 0;
        t.executor_gone = true;
        assert_eq!(classify(&t), Some(Reason::ReviewOrphan));
        t.executor_gone = false;
        t.executor_conversation_id = None;
        assert_eq!(classify(&t), Some(Reason::ReviewOrphan));
    }

    #[test]
    fn in_progress_needs_a_live_executor_or_six_hours_alone() {
        let mut t = task("in_progress");
        t.executor_live = false;
        t.age_secs = STOPPED_EXECUTOR_SECS - 1;
        assert_eq!(classify(&t), None);
        t.age_secs = STOPPED_EXECUTOR_SECS;
        assert_eq!(classify(&t), Some(Reason::ExecutorStopped));
        t.executor_live = true;
        assert_eq!(classify(&t), None);
        t.executor_gone = true;
        assert_eq!(classify(&t), Some(Reason::ExecutorStopped));
        t.executor_conversation_id = None;
        t.age_secs = NO_EXECUTOR_SECS - 1;
        assert_eq!(classify(&t), None);
        t.age_secs = NO_EXECUTOR_SECS;
        assert_eq!(classify(&t), Some(Reason::NoExecutor));
    }

    #[test]
    fn unchanged_needs_a_day_and_five_orchestrator_turns() {
        for state in ["in_progress", "blocked"] {
            let mut t = task(state);
            t.age_secs = UNCHANGED_SECS;
            t.orchestrator_runs = UNCHANGED_RUNS - 1;
            assert_eq!(classify(&t), None, "{state}");
            t.orchestrator_runs = UNCHANGED_RUNS;
            assert_eq!(classify(&t), Some(Reason::Unchanged), "{state}");
            t.executor_busy = true;
            assert_eq!(classify(&t), None, "{state}: the executor is working");
        }
        let mut waiting = task("waiting");
        waiting.age_secs = UNCHANGED_SECS * 10;
        waiting.orchestrator_runs = 100;
        assert_eq!(classify(&waiting), None, "waiting is backlog");
    }

    #[test]
    fn lines_name_the_fix_and_the_digest_caps_its_length() {
        let mut t = task("in_review");
        t.age_secs = 26 * 3600;
        assert_eq!(
            line(&t, Reason::ReviewIdle),
            format!(
                "#7 \"Свести план\" has waited in_review for 26h with an idle executor: session_accept {} or reopen it with plan_update.",
                Uuid::nil()
            )
        );
        t.state = "blocked".into();
        t.age_secs = 3 * 86400;
        assert_eq!(
            line(&t, Reason::Unchanged),
            "#7 \"Свести план\" is blocked and unchanged for 3d: update or close it."
        );
        t.title = "д".repeat(100);
        assert!(line(&t, Reason::Unchanged).contains(&format!("{}…", "д".repeat(59))));
        let lines: Vec<String> = (1..=7).map(|n| format!("#{n}")).collect();
        let text = digest(&lines);
        assert!(text.starts_with("[canterlot] Plan check:"));
        assert_eq!(text.lines().count(), 1 + MAX_LINES + 1);
        assert!(text.ends_with("…and 2 more (plan_list shows all)."));
    }
}
