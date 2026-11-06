//! Единое понятие основного агента для блоков и расхода.
//!
//! Claude Code и Codex подписывают главный поток `main`, OpenCode — `orchestrator`.
//! Пустое имя осталось в старых записях и синтетике. Потомки всегда названы своим
//! агентом (`general`, `explore`, `subagent`, имя подагента Claude), поэтому в это
//! множество не попадают.

/// Имена основного агента; список один для Rust-фильтров и SQL-предиката.
pub const MAIN_AGENTS: &[&str] = &["", "main", "orchestrator"];

pub fn is_main_agent(agent: &str) -> bool {
    MAIN_AGENTS.contains(&agent)
}

/// SQL-предикат для колонки с именем агента: `agent in ('', 'main', 'orchestrator')`.
///
/// Значения — константы модуля, поэтому имя колонки подставляется только вызывающим
/// кодом, а не данными запроса.
pub fn main_agent_filter(column: &str) -> String {
    let list = MAIN_AGENTS
        .iter()
        .map(|agent| format!("'{agent}'"))
        .collect::<Vec<_>>()
        .join(",");
    format!("{column} in ({list})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sql_predicate_is_the_same_set_as_the_rust_predicate() {
        let predicate = main_agent_filter("agent");
        assert_eq!(predicate, "agent in ('','main','orchestrator')");
        for agent in ["", "main", "orchestrator"] {
            assert!(is_main_agent(agent), "{agent:?}");
        }
        for agent in ["general", "explore", "subagent", "Explore", "helper"] {
            assert!(!is_main_agent(agent), "{agent:?}");
        }
    }
}
