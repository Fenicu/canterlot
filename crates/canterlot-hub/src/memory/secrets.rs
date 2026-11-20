//! Детектор секретов для записей памяти и импорта: возвращает только класс находки,
//! само совпадение наружу не идёт ни в ошибку, ни в журнал.

/// Класс найденного секрета.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretClass {
    PrivateKey,
    TokenPrefix,
    Assignment,
    HighEntropy,
}

impl SecretClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrivateKey => "private_key",
            Self::TokenPrefix => "token_prefix",
            Self::Assignment => "assignment",
            Self::HighEntropy => "high_entropy",
        }
    }
}

/// Первый найденный класс или `None`. Пути, имена хостов, UUID и хэши коммитов проходят.
pub fn scan(text: &str) -> Option<SecretClass> {
    if has_private_key(text) {
        return Some(SecretClass::PrivateKey);
    }
    if text
        .split(|c: char| !is_token_char(c))
        .any(is_prefixed_token)
    {
        return Some(SecretClass::TokenPrefix);
    }
    if text.lines().any(line_has_assignment) {
        return Some(SecretClass::Assignment);
    }
    if text
        .split(|c: char| !is_entropy_char(c))
        .any(is_high_entropy)
    {
        return Some(SecretClass::HighEntropy);
    }
    None
}

fn has_private_key(text: &str) -> bool {
    text.match_indices("-----BEGIN ").any(|(at, _)| {
        let line = text[at..].lines().next().unwrap_or("");
        line.contains("PRIVATE KEY-----")
    })
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

fn tail_len(token: &str, prefix: &str) -> usize {
    token[prefix.len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .count()
}

fn is_prefixed_token(token: &str) -> bool {
    let token = token.trim_matches('.');
    for (prefix, min) in [
        ("sk-", 16),
        ("ghp_", 20),
        ("gho_", 20),
        ("github_pat_", 20),
        ("glpat-", 16),
        ("xoxa-", 10),
        ("xoxb-", 10),
        ("xoxp-", 10),
    ] {
        if token.starts_with(prefix) && tail_len(token, prefix) >= min {
            return true;
        }
    }
    if let Some(rest) = token.strip_prefix("AKIA")
        && rest.len() == 16
        && rest
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return true;
    }
    is_jwt(token)
}

fn is_jwt(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    parts.len() == 3
        && parts[0].starts_with("eyJ")
        && parts.iter().all(|part| {
            part.len() >= 8
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        })
}

const KEYWORDS: [&str; 10] = [
    "password",
    "passwd",
    "pwd",
    "token",
    "secret",
    "api_key",
    "api-key",
    "пароль",
    "Пароль",
    "ПАРОЛЬ",
];

const PLACEHOLDERS: [&str; 14] = [
    "changeme",
    "change_me",
    "example",
    "placeholder",
    "redacted",
    "your_",
    "your-",
    "xxx",
    "secret_request",
    "password123",
    "undefined",
    "nothing",
    "unknown",
    "required",
];

fn line_has_assignment(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    for keyword in KEYWORDS {
        for (at, _) in lower.match_indices(keyword) {
            let mut rest = &line[at + keyword.len()..];
            // Хвост имени: `api_key_prod`, `tokens`, `password_hash`.
            rest = rest.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_');
            rest = rest.trim_start_matches(['"', '\'', '`', '*', ' ', '\t']);
            let Some(rest) = rest.strip_prefix(['=', ':']) else {
                continue;
            };
            let rest = rest.trim_start_matches(['=', '>', '"', '\'', '`', ' ', '\t']);
            let value: String = rest
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '`' | ',' | ';'))
                .collect();
            let after = rest[value.len()..].trim_start();
            if looks_like_secret_value(&value, after) {
                return true;
            }
        }
    }
    false
}

/// `value` — слово после `=`/`:`, `after` — что идёт за ним в строке. Явное присвоение
/// пароля с значением от 8 знаков секрет при любом наборе знаков; исключения узкие:
/// заглушки, значения вида `<…>`, `${…}`, `%…%`, и проза, где за словом идут ещё слова
/// («password: see the manager»).
fn looks_like_secret_value(value: &str, after: &str) -> bool {
    if value.chars().count() < 8 {
        return false;
    }
    if value.starts_with(['<', '$', '{', '%', '*', '/', '~', '.', '[', '(']) {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    if PLACEHOLDERS.iter().any(|p| lower.contains(p)) {
        return false;
    }
    let has_digit = value.chars().any(|c| c.is_ascii_digit());
    let has_upper = value.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = value.chars().any(|c| c.is_ascii_lowercase());
    let has_symbol = value.chars().any(|c| c.is_ascii_punctuation());
    if has_digit || (has_upper && has_lower) || has_symbol {
        return true;
    }
    // Одни буквы: секрет, если за словом нет продолжения фразой.
    let prose = after.chars().next().is_some_and(char::is_alphanumeric);
    !prose
}

fn is_entropy_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '_' | '-' | '=')
}

fn is_high_entropy(token: &str) -> bool {
    if token.len() < 32 {
        return false;
    }
    // Хэши коммитов и дайджесты: только шестнадцатеричные.
    if token.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let has_letter = token.chars().any(|c| c.is_ascii_alphabetic());
    if !has_digit || !has_letter {
        return false;
    }
    entropy(token) >= 4.0
}

fn entropy(token: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    for c in token.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    let total = token.chars().count() as f64;
    counts
        .values()
        .map(|&n| {
            let p = n as f64 / total;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_key_block() {
        let text =
            "ключ:\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3Bl\n-----END OPENSSH PRIVATE KEY-----";
        assert_eq!(scan(text), Some(SecretClass::PrivateKey));
        assert_eq!(
            scan("-----BEGIN RSA PRIVATE KEY-----"),
            Some(SecretClass::PrivateKey)
        );
    }

    #[test]
    fn token_prefixes() {
        for text in [
            "используй sk-abcdefghijklmnop1234 для вызова",
            "ghp_abcdefghijklmnopqrstuvwx",
            "gho_abcdefghijklmnopqrstuvwx",
            "github_pat_11ABCDEFG0abcdefghijkl_mnop",
            "xoxb-123456789012-abcdef",
            "xoxp-123456789012-abcdef",
            "ключ AKIAABCDEFGHIJKLMNOP в конфиге",
            "glpat-abcdefghij0123456789",
            "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        ] {
            assert_eq!(scan(text), Some(SecretClass::TokenPrefix), "{text}");
        }
    }

    #[test]
    fn assignments_with_real_looking_values() {
        for text in [
            "password = Hunter2Hunter2",
            "DB_PASSWORD=s3cr3tvalue",
            "api_key: \"k9f8d7s6a5\"",
            "token='abc-123-def-456'",
            "export GITHUB_TOKEN=ab12cd34ef56",
            "secret: Zq!pLm8#vx",
        ] {
            assert_eq!(scan(text), Some(SecretClass::Assignment), "{text}");
        }
    }

    #[test]
    fn lowercase_literal_passwords_are_secrets() {
        for text in [
            "password = hunterhunter",
            "пароль: correcthorsebattery",
            "Пароль: correcthorsebattery",
            "token: abcdefghij",
            "passwd=qwertyuiop",
            "pwd: lowercasevalue",
            "secret = abcdefghijkl.",
            "- **password**: hunterhunter",
        ] {
            assert_eq!(scan(text), Some(SecretClass::Assignment), "{text}");
        }
    }

    #[test]
    fn placeholders_and_prose_after_a_keyword_pass() {
        for text in [
            "password: <ask the owner>",
            "password = ${DB_PASSWORD}",
            "password: see the password manager",
            "password: см. менеджер паролей",
            "пароль: хранится в менеджере паролей",
            "token: refreshed when expired",
            "password: %PASSWORD%",
            "pwd = {{ vault.pwd }}",
            "password: hunterhunter is not stored here",
        ] {
            assert_eq!(scan(text), None, "{text}");
        }
    }

    #[test]
    fn long_high_entropy_strings() {
        assert_eq!(
            scan("значение Zm9vYmFyMTIzNDU2Nzg5MEFCQ0RFRkdISUpLTE1OT1BRUlNUVVY= дальше"),
            Some(SecretClass::HighEntropy)
        );
    }

    #[test]
    fn paths_hosts_uuids_and_shas_pass() {
        for text in [
            "каталог /h/work/service/src/very/long/path/to/some/module_with_long_name.rs",
            "хост build-runner-01.internal.example.net порт 8443",
            "id 0192e5a4-7c3b-7d2a-9f10-3b5c8d1e4a67",
            "коммит 68cfa990d3c1b5e7a2f4c6d8e0a1b3c5d7e9f012",
            "sha256 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "ERR_NETWORK_CHANGED_WHILE_LOADING_THE_REMOTE_RESOURCE",
            "token: refreshed when expired",
            "password = <your password>",
            "password = ${DB_PASSWORD}",
            "api_key: changeme-please",
            "token_file = /h/.cache/token",
            "секреты просим через secret_request",
            "",
        ] {
            assert_eq!(scan(text), None, "{text}");
        }
    }

    #[test]
    fn class_names_are_stable() {
        assert_eq!(SecretClass::PrivateKey.as_str(), "private_key");
        assert_eq!(SecretClass::TokenPrefix.as_str(), "token_prefix");
        assert_eq!(SecretClass::Assignment.as_str(), "assignment");
        assert_eq!(SecretClass::HighEntropy.as_str(), "high_entropy");
    }
}
