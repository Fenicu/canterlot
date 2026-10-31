//! Фоновые команды: прерывание их не снимает, а `DELETE` разбудил бы модель, поэтому они
//! остаются работать и показываются владельцу.

use serde_json::Value;

const MAX_SHOWN: usize = 20;
const COMMAND_BYTES: usize = 200;

/// Работающие команды своих сессий из `GET /api/shell`.
pub fn running(list: &Value, owns: impl Fn(&str) -> bool) -> Vec<String> {
    list.get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|shell| shell.get("status").and_then(Value::as_str) == Some("running"))
        .filter(|shell| {
            shell
                .pointer("/metadata/sessionID")
                .and_then(Value::as_str)
                .is_some_and(&owns)
        })
        .filter_map(|shell| shell.get("command")?.as_str().map(str::to_owned))
        .collect()
}

/// Текст блока для владельца.
pub fn notice(commands: &[String]) -> String {
    let mut text = "Фоновые команды продолжают работать:".to_owned();
    for command in commands.iter().take(MAX_SHOWN) {
        let cut = command.floor_char_boundary(COMMAND_BYTES);
        text.push_str("\n- ");
        text.push_str(&command[..cut]);
        if cut < command.len() {
            text.push('…');
        }
    }
    if commands.len() > MAX_SHOWN {
        text.push_str(&format!("\n- и ещё {}", commands.len() - MAX_SHOWN));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn long_cyrillic_command_is_cut_on_a_letter() {
        let text = notice(&["щ".repeat(300)]);
        assert!(text.ends_with('…'));
        assert!(text.len() < 300);
    }

    #[test]
    fn list_is_bounded() {
        let commands: Vec<String> = (0..25).map(|index| format!("sleep {index}")).collect();
        let text = notice(&commands);
        assert!(text.contains("sleep 19"));
        assert!(!text.contains("sleep 20"));
        assert!(text.ends_with("- и ещё 5"));
    }

    #[test]
    fn shell_without_a_session_is_not_ours() {
        let list = json!({"data": [{"status": "running", "command": "x", "metadata": {}}]});
        assert!(running(&list, |_| true).is_empty());
    }
}
