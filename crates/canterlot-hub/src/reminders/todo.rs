//! Напоминание о списке задач основного агента, который перестал меняться.
//!
//! Пункт `in_progress` после хода, который его закончил, — частая забывчивость; но и
//! оркестратор, ждущий делегатов, держит пункт в работе много ходов законно, а каждое
//! сообщение `[canterlot]` начинает ход. Поэтому порог в ходах, одна заметка на ревизию
//! списка и не чаще раза в три часа. Строка дорожки берётся под очередью разговора, как
//! в пакете журнала (`todos::apply`).

use sqlx::{PgConnection, types::Json};
use uuid::Uuid;

use crate::todos::{TodoItemView, TodoStatus};

/// Ходов без правки списка с пунктом в работе.
pub const IN_PROGRESS_TURNS: i64 = 3;
/// Ходов без правки списка с любым незакрытым пунктом.
pub const OPEN_TURNS: i64 = 10;
/// Не чаще одной заметки за этот срок.
pub const INTERVAL_SECS: i64 = 3 * 3600;
const ITEM_CHARS: usize = 60;

/// Текст заметки или `None`: ходов `turns` с последней правки списка `items`.
pub fn note(items: &[TodoItemView], turns: i64) -> Option<String> {
    let in_progress: Vec<&TodoItemView> = items
        .iter()
        .filter(|item| item.status == TodoStatus::InProgress)
        .collect();
    if !in_progress.is_empty() && turns >= IN_PROGRESS_TURNS {
        let more = if in_progress.len() > 1 { ", …" } else { "" };
        return Some(format!(
            "[canterlot] Your todo list still shows {} in progress (\"{}\"{more}) and has not changed for {turns} turns. Update it if that no longer matches your work.",
            in_progress.len(),
            short(&in_progress[0].text),
        ));
    }
    let open = items
        .iter()
        .filter(|item| matches!(item.status, TodoStatus::Pending | TodoStatus::InProgress))
        .count();
    (open > 0 && turns >= OPEN_TURNS).then(|| {
        format!(
            "[canterlot] Your todo list has not changed for {turns} turns ({open} open). Update it if it no longer matches your work."
        )
    })
}

fn short(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= ITEM_CHARS {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(ITEM_CHARS - 1).collect();
    cut.push('…');
    cut
}

#[derive(sqlx::FromRow)]
struct Lane {
    revision: i64,
    items: Json<Vec<TodoItemView>>,
    reminded_revision: Option<i64>,
    due: bool,
    turns: i64,
}

/// Заметка для разговора, если она нужна сейчас: `(текст, ревизия списка)`.
pub async fn take_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Option<(String, i64)>, sqlx::Error> {
    let lane: Option<Lane> = sqlx::query_as(
        "select t.revision, t.items, t.reminded_revision,
                t.reminded_at is null
                  or t.reminded_at <= clock_timestamp() - make_interval(secs => $2) as due,
                (select count(*) from runs r where r.conversation_id = t.conversation_id
                   and r.started_at > t.updated_at and r.finished_at is not null) as turns
         from agent_todos t
         where t.conversation_id = $1 and t.lane_block_id is null
         for update of t",
    )
    .bind(conversation_id)
    .bind(INTERVAL_SECS as f64)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(lane) = lane else {
        return Ok(None);
    };
    if !lane.due || lane.reminded_revision == Some(lane.revision) {
        return Ok(None);
    }
    let Some(text) = note(&lane.items.0, lane.turns) else {
        return Ok(None);
    };
    sqlx::query(
        "update agent_todos set reminded_revision = $2, reminded_at = clock_timestamp()
         where conversation_id = $1 and lane_block_id is null",
    )
    .bind(conversation_id)
    .bind(lane.revision)
    .execute(conn)
    .await?;
    Ok(Some((text, lane.revision)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str, status: TodoStatus) -> TodoItemView {
        TodoItemView {
            text: text.into(),
            status,
        }
    }

    #[test]
    fn thresholds_and_texts() {
        let working = [item("Fix X", TodoStatus::InProgress)];
        assert_eq!(note(&working, IN_PROGRESS_TURNS - 1), None);
        assert!(note(&working, IN_PROGRESS_TURNS).is_some());
        let pending = [
            item("A", TodoStatus::Pending),
            item("B", TodoStatus::Cancelled),
        ];
        assert_eq!(note(&pending, OPEN_TURNS - 1), None);
        assert_eq!(
            note(&pending, OPEN_TURNS).as_deref(),
            Some(
                "[canterlot] Your todo list has not changed for 10 turns (1 open). Update it if it no longer matches your work."
            )
        );
        let done = [
            item("A", TodoStatus::Completed),
            item("B", TodoStatus::Cancelled),
        ];
        assert_eq!(note(&done, 100), None);
        assert_eq!(note(&[], 100), None);
        let long = [item(&"ж".repeat(80), TodoStatus::InProgress)];
        assert!(
            note(&long, 3)
                .unwrap()
                .contains(&format!("{}…", "ж".repeat(59)))
        );
    }
}
