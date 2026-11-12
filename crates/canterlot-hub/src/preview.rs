//! Превью последнего ответа агента для строки списка: начало текста без разметки Markdown.

use sqlx::PgConnection;
use uuid::Uuid;

/// Сколько символов превью хранит строка списка.
pub const PREVIEW_CHARS: usize = 160;
/// Сколько исходного текста читается из блока: превью короче, а разметка съедает часть.
const SOURCE_CHARS: i32 = 4000;

/// Превью ответа последнего завершённого хода: финальный текст основного агента, созданный
/// не позже конца хода. Рассуждения, вызовы инструментов, их вывод и тексты подагентов
/// сюда не попадают; `None` — ответа ещё нет.
pub async fn last_answer(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    turn_finished_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<String>, sqlx::Error> {
    let query = format!(
        "select left(content,$3::int) from blocks
         where conversation_id=$1 and kind='text' and parent_block_id is null and state='final'
           and btrim(content) <> '' and created_at <= $2 and {}
         order by created_at desc,created_seq desc limit 1",
        crate::agents::main_agent_filter("agent")
    );
    let text: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .bind(conversation_id)
        .bind(turn_finished_at)
        .bind(SOURCE_CHARS)
        .fetch_optional(conn)
        .await?;
    Ok(text.and_then(|text| plain(&text, PREVIEW_CHARS)))
}

/// Текст без разметки Markdown в одну строку, не длиннее `max` символов; `None` — пусто.
pub fn plain(markdown: &str, max: usize) -> Option<String> {
    let mut lines = Vec::new();
    let mut fence: Option<&str> = None;
    for raw in markdown.lines() {
        let line = raw.trim();
        // Блок кода в превью не попадает: строка списка не место для листинга.
        if let Some(open) = fence {
            if line.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            fence = Some(&line[..3]);
            continue;
        }
        if is_rule(line) || is_table_divider(line) {
            continue;
        }
        let line = block_prefix(line);
        let line = if line.starts_with('|') {
            line.trim_matches('|').replace('|', " ")
        } else {
            line.to_owned()
        };
        lines.push(inline(&line));
    }
    let flat = lines
        .join(" ")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.is_empty() {
        return None;
    }
    if flat.chars().count() <= max {
        return Some(flat);
    }
    let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    Some(cut)
}

fn is_rule(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3
        && ['-', '*', '_']
            .iter()
            .any(|mark| compact.chars().all(|c| c == *mark))
}

fn is_table_divider(line: &str) -> bool {
    line.contains('-')
        && line.contains('|')
        && line
            .chars()
            .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

/// Снимает заголовок, цитату и маркер списка в начале строки.
fn block_prefix(mut line: &str) -> &str {
    loop {
        let before = line;
        line = line.trim_start();
        if let Some(rest) = line.strip_prefix('>') {
            line = rest;
        }
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && line[hashes..].starts_with([' ', '\t']) {
            line = &line[hashes..];
        }
        for marker in ["- [ ] ", "- [x] ", "- [X] ", "- ", "* ", "+ "] {
            if let Some(rest) = line.strip_prefix(marker) {
                line = rest;
                break;
            }
        }
        let digits = line.chars().take_while(char::is_ascii_digit).count();
        if (1..=9).contains(&digits)
            && (line[digits..].starts_with(". ") || line[digits..].starts_with(") "))
        {
            line = &line[digits + 2..];
        }
        if line == before {
            return line;
        }
    }
}

/// Снимает строчную разметку: ссылки, картинки, код, выделение, теги и экранирование.
fn inline(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() => {
                out.push(chars[i + 1]);
                i += 2;
            }
            '!' if chars.get(i + 1) == Some(&'[') => {
                i += 1;
            }
            '[' => {
                // Ссылка `[текст](адрес)` и `[текст][метка]` оставляют текст.
                if let Some(close) = find(&chars, i + 1, ']') {
                    let after = chars.get(close + 1);
                    let end = match after {
                        Some('(') => find(&chars, close + 2, ')'),
                        Some('[') => find(&chars, close + 2, ']'),
                        _ => None,
                    };
                    if let Some(end) = end {
                        out.push_str(&inline(&chars[i + 1..close].iter().collect::<String>()));
                        i = end + 1;
                        continue;
                    }
                }
                out.push(c);
                i += 1;
            }
            '<' => {
                // Тег HTML или автоссылка: тег пропадает, адрес остаётся.
                if let Some(close) = find(&chars, i + 1, '>') {
                    let body: String = chars[i + 1..close].iter().collect();
                    if body.contains("://") || body.contains('@') && !body.contains(' ') {
                        out.push_str(&body);
                        i = close + 1;
                        continue;
                    }
                    let first = body.chars().next();
                    if first.is_some_and(|f| f.is_ascii_alphabetic() || f == '/' || f == '!') {
                        out.push(' ');
                        i = close + 1;
                        continue;
                    }
                }
                out.push(c);
                i += 1;
            }
            '`' => {
                i += 1;
            }
            '*' | '~' => {
                // Одинокая звёздочка между пробелами — знак умножения, а не выделение.
                let spaced =
                    i > 0 && chars[i - 1] == ' ' && chars.get(i + 1).is_none_or(|n| *n == ' ');
                if c == '*' && spaced {
                    out.push(c);
                }
                i += 1;
            }
            '_' => {
                // Подчёркивание внутри слова (`snake_case`) — часть имени.
                let inside = i > 0
                    && chars[i - 1].is_alphanumeric()
                    && chars.get(i + 1).is_some_and(|n| n.is_alphanumeric());
                if inside {
                    out.push(c);
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn find(chars: &[char], from: usize, target: char) -> Option<usize> {
    chars
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, c)| **c == target)
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_inline_markup() {
        assert_eq!(
            plain("**Готово**: тесты `cargo test` _зелёные_, ~~старое~~ [коммит](https://x/a3f) ![схема](a.png)", 160).as_deref(),
            Some("Готово: тесты cargo test зелёные, старое коммит схема")
        );
    }

    #[test]
    fn strips_block_markup_and_collapses_whitespace() {
        let text = "## Итог\n\n> Важно\n\n- первый\n- второй\n1. шаг\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |";
        assert_eq!(
            plain(text, 160).as_deref(),
            Some("Итог Важно первый второй шаг a b 1 2")
        );
    }

    #[test]
    fn drops_code_fences_with_their_contents() {
        let text = "Вот правка:\n```rust\nfn main() {}\n```\nЗапусти тесты.";
        assert_eq!(
            plain(text, 160).as_deref(),
            Some("Вот правка: Запусти тесты.")
        );
    }

    #[test]
    fn keeps_identifiers_and_arithmetic() {
        assert_eq!(
            plain("Поле snake_case_name и 2 * 3", 160).as_deref(),
            Some("Поле snake_case_name и 2 * 3")
        );
        assert_eq!(
            plain("\\*не выделение\\*", 160).as_deref(),
            Some("*не выделение*")
        );
        assert_eq!(
            plain("<b>жирный</b> <https://a.b>", 160).as_deref(),
            Some("жирный https://a.b")
        );
    }

    #[test]
    fn clips_to_the_limit_with_an_ellipsis() {
        let text = "слово ".repeat(100);
        let cut = plain(&text, 160).unwrap();
        assert_eq!(cut.chars().count(), 160);
        assert!(cut.ends_with('…'));
        assert!(!cut.contains("  "));
    }

    #[test]
    fn empty_or_markup_only_text_is_none() {
        assert_eq!(plain("", 160), None);
        assert_eq!(plain("```\ncode\n```\n---", 160), None);
        assert_eq!(plain("  \n\t ", 160), None);
    }
}
