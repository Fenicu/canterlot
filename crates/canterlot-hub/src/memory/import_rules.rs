//! Правила имён файлов, которые не импортируются никогда. Один исходный файл на двоих:
//! воркер подключает его через `#[path]` при сборе пакета, хаб применяет при приёме
//! пакета. Редактируемая пометка пропуска в манифесте их не отменяет.

pub const SKIP_CREDENTIAL_NAME: &str = "credential_name";
pub const SKIP_INDEX_FILE: &str = "index_file";

/// Последний компонент пути: имя файла.
pub fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Имя файла по правилам обязательного пропуска или `None`, если файл допустим.
pub fn forbidden_name(name: &str) -> Option<&'static str> {
    if name.eq_ignore_ascii_case("MEMORY.md") {
        Some(SKIP_INDEX_FILE)
    } else if credential_name(name) {
        Some(SKIP_CREDENTIAL_NAME)
    } else {
        None
    }
}

pub fn credential_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower == "reference_all.md"
        || ["credential", "secret", "token"]
            .iter()
            .any(|word| lower.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_credential_names_are_forbidden() {
        assert_eq!(forbidden_name("MEMORY.md"), Some(SKIP_INDEX_FILE));
        assert_eq!(forbidden_name("memory.md"), Some(SKIP_INDEX_FILE));
        assert_eq!(
            forbidden_name("reference_all.md"),
            Some(SKIP_CREDENTIAL_NAME)
        );
        assert_eq!(forbidden_name("My_Tokens.md"), Some(SKIP_CREDENTIAL_NAME));
        assert_eq!(forbidden_name("notes.md"), None);
        assert_eq!(basename("-h-x/memory/MEMORY.md"), "MEMORY.md");
        assert_eq!(basename("plain.md"), "plain.md");
    }
}
