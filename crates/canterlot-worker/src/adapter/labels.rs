//! Подписи вызовов инструментов: строка видна без нажатия и на узком экране,
//! поэтому показывает то, что различает вызовы, и не несёт тело команды.

/// Первая непустая строка команды без тела heredoc.
pub fn command_label(command: &str) -> &str {
    let line = command
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    line.split_once("<<")
        .map_or(line, |(head, _)| head.trim_end())
}

const MARK: &str = "…";

/// Начало текста не длиннее `limit` байт; срезанное отмечается многоточием в пределах длины.
pub fn cut_end(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let end = text.floor_char_boundary(limit.saturating_sub(MARK.len()));
    format!("{}{MARK}", &text[..end])
}

/// Конец пути: имя файла различает строки, а начало у них общее.
pub fn path_tail(path: &str, limit: usize) -> String {
    if path.len() <= limit {
        return path.to_owned();
    }
    let mut start = path.len() - limit.saturating_sub(MARK.len());
    while !path.is_char_boundary(start) {
        start += 1;
    }
    format!("{MARK}{}", &path[start..])
}
