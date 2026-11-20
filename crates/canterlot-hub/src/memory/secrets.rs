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

/// Ключевые слова в нижнем регистре: строка перед поиском приводится к нему.
const KEYWORDS: [&str; 11] = [
    "password",
    "passwd",
    "pwd",
    "token",
    "secret",
    "api_key",
    "api-key",
    "api key",
    "пароль",
    "токен",
    "секрет",
];

/// Нижний регистр с сохранением байтовых смещений: символ, у которого длина в UTF-8
/// после приведения меняется, остаётся как был.
fn lower_same_len(line: &str) -> String {
    line.chars()
        .map(|c| {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(l), None) if l.len_utf8() == c.len_utf8() => l,
                _ => c,
            }
        })
        .collect()
}

const FILE_EXTENSIONS: [&str; 10] = [
    ".env", ".md", ".toml", ".json", ".yaml", ".yml", ".conf", ".pem", ".key", ".crt",
];

/// Имя, а не значение: переменная окружения, snake_case-идентификатор или имя файла.
/// Только для литерала в кавычках без `:`/`=` после ключевого слова.
fn looks_like_a_name(value: &str) -> bool {
    let is_env = value.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && value
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if is_env {
        return true;
    }
    let mut parts = value.split('_');
    let first_ok = parts.next().is_some_and(|p| {
        p.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && p.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    });
    let mut rest = 0;
    let rest_ok = parts.all(|p| {
        rest += 1;
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    });
    if first_ok && rest_ok && rest > 0 {
        return true;
    }
    let lower = value.to_ascii_lowercase();
    FILE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

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

/// `sshpass -p ЗНАЧЕНИЕ`: пароль в командной строке, значение любой длины.
fn has_sshpass_password(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    for (at, _) in lower.match_indices("sshpass") {
        // Разбираются только собственные опции sshpass, до первой команды: `-p` у ssh
        // или sudo дальше по строке паролем не считается.
        let mut tokens = line[at + "sshpass".len()..].split_whitespace();
        while let Some(token) = tokens.next() {
            let Some(option) = token.strip_prefix('-') else {
                break;
            };
            let value = match option {
                "p" => tokens.next().unwrap_or(""),
                "f" | "d" | "P" => {
                    tokens.next();
                    continue;
                }
                _ => match option.strip_prefix('p') {
                    Some(glued) => glued,
                    None => continue,
                },
            };
            let value = value.trim_matches(['"', '\'', '`']);
            if !value.is_empty() && !value.starts_with(['<', '$', '{']) {
                return true;
            }
        }
    }
    false
}

fn line_has_assignment(line: &str) -> bool {
    if has_sshpass_password(line) {
        return true;
    }
    let lower = lower_same_len(line);
    for keyword in KEYWORDS {
        for (at, _) in lower.match_indices(keyword) {
            let mut rest = &line[at + keyword.len()..];
            // Хвост имени: `api_key_prod`, `tokens`, `password_hash`.
            rest = rest.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '_');
            // Литерал в кавычках сразу после слова, без `:`/`=`: «пароль `значение`».
            // Кавычка вплотную к слову закрывает кодовый фрагмент (`CI_JOB_TOKEN`), а не
            // открывает литерал: нужен хотя бы один пробел или табуляция.
            if let Some(literal) = rest
                .starts_with([' ', '\t'])
                .then(|| {
                    rest.trim_start_matches([' ', '\t'])
                        .strip_prefix(['"', '\'', '`'])
                })
                .flatten()
            {
                let quote = rest
                    .trim_start_matches([' ', '\t'])
                    .chars()
                    .next()
                    .unwrap_or('`');
                let literal = literal.split(quote).next().unwrap_or(literal).trim();
                if literal.chars().count() >= 5
                    && !looks_like_a_name(literal)
                    && looks_like_secret_value(literal, "", 5)
                {
                    return true;
                }
            }
            rest = rest.trim_start_matches(['"', '\'', '`', '*', ' ', '\t']);
            let Some(rest) = rest.strip_prefix(['=', ':']) else {
                continue;
            };
            let rest = rest.trim_start_matches(['=', '>', ' ', '\t']);
            // Значение в кавычках берётся целиком, со всеми словами: проза-исключение
            // действует только для значения без кавычек.
            if let Some(quote) = rest
                .chars()
                .next()
                .filter(|c| matches!(c, '"' | '\'' | '`'))
            {
                let literal = &rest[quote.len_utf8()..];
                let literal = literal.split(quote).next().unwrap_or(literal);
                if looks_like_secret_value(literal.trim(), "", 8) {
                    return true;
                }
                continue;
            }
            let rest = rest.trim_start_matches(['=', '>', '"', '\'', '`', ' ', '\t']);
            let value: String = rest
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '`' | ',' | ';'))
                .collect();
            let after = rest[value.len()..].trim_start();
            if looks_like_secret_value(&value, after, 8) {
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
fn looks_like_secret_value(value: &str, after: &str, min_len: usize) -> bool {
    if value.chars().count() < min_len {
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
    if is_slug(token) {
        return false;
    }
    entropy(token) >= 4.0
}

/// Слаг с датой («autonomy-lease-watchdog-2026-11-08»): от трёх сегментов по `-`/`_`,
/// каждый только из букв, только из цифр или номер версии вида `v2`. У base64 сегменты
/// смешивают буквы и цифры, и такая строка остаётся находкой.
fn is_slug(token: &str) -> bool {
    if token.contains(['+', '=']) {
        return false;
    }
    let segments: Vec<&str> = token.split(['-', '_']).filter(|s| !s.is_empty()).collect();
    segments.len() >= 3
        && segments.iter().all(|s| {
            s.chars().all(|c| c.is_ascii_alphabetic())
                || s.chars().all(|c| c.is_ascii_digit())
                || s.strip_prefix('v')
                    .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
        })
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
    fn quoted_multiword_literals_are_secrets() {
        for text in [
            "password: \"correct horse battery\"",
            "token = 'abc def ghi jkl'",
            "пароль: \"тихий серый кот\"",
        ] {
            assert_eq!(scan(text), Some(SecretClass::Assignment), "{text}");
        }
    }

    #[test]
    fn quoted_placeholders_pass() {
        for text in [
            "password: \"<ask the owner>\"",
            "password = \"${DB_PASSWORD}\"",
            "token: \"changeme please\"",
        ] {
            assert_eq!(scan(text), None, "{text}");
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
    fn quoted_literal_after_a_keyword_without_separator_is_a_secret() {
        for text in [
            "пароль `administrator`",
            "password `Xk9#mQ2v`",
            "secret `4f9KxP2mQ7vL8nR3tY6wZ1bC5dE0aH2j`",
            "token `abcdEFGH1234ijkl`",
            "Токен: abcd1234efgh",
            "ПАРОЛЬ `qwertyuiop`",
            "API key: `Zq8Wv3Lp0Xn5Rt7Ys2Kd9Mf4Hb6Jc1Ga`",
            "Ёж, Пароль `qwertyuiop`",
            "ЁЖ Ёё ТОКЕН: abcd1234efgh",
        ] {
            assert_eq!(scan(text), Some(SecretClass::Assignment), "{text}");
        }
    }

    #[test]
    fn sshpass_value_is_a_secret() {
        for text in [
            "sshpass -p 1234 ssh root@h",
            "sshpass -p 'r00t!' scp f h:/",
            "sshpass -v -p 'r00t!' scp f h:/",
            "sshpass -pHunter2x ssh h",
        ] {
            assert_eq!(scan(text), Some(SecretClass::Assignment), "{text}");
        }
    }

    #[test]
    fn names_and_placeholders_after_a_keyword_pass() {
        for text in [
            "токен `BOT_TOKEN` в `.env`",
            "password `secrets.env`",
            "token `api_key_file`",
            "sshpass -p '<pass>' ssh root@<IP>",
            "sshpass -p \"$PASS\" ssh h",
            "sshpass -p \"$P\" ssh -J h -p 830 root@h",
            "sshpass -e ssh -o StrictHostKeyChecking=no h; echo x | sudo -S -p \"\" true",
            "sshpass -f /tmp/pw ssh -p 22 h",
            "пароль в ~/.config/x/secrets.env",
            "пароль `app`",
            "set `CI_JOB_TOKEN` then pass `--secret build-id` and done",
            "`secret_request` delivers it to `/run/user/1000/x`",
        ] {
            assert_eq!(scan(text), None, "{text}");
        }
    }

    #[test]
    fn dated_slugs_are_not_high_entropy() {
        for text in [
            "autonomy-lease-watchdog-2026-11-08",
            "project_inference_model_ab_2026_11",
            "docs/superpowers/specs/2026-07-15-shop-license-v2-plan-design.md",
            "[[project_garden_august_watering_wave_2026]]",
        ] {
            assert_eq!(scan(text), None, "{text}");
        }
    }

    #[test]
    fn base64url_with_separators_stays_high_entropy() {
        assert_eq!(
            scan("значение Zq8Wv3-Lp0Xn5_Rt7Ys2Kd9-Mf4Hb6Jc1Ga3Pe5 дальше"),
            Some(SecretClass::HighEntropy)
        );
    }

    #[test]
    fn class_names_are_stable() {
        assert_eq!(SecretClass::PrivateKey.as_str(), "private_key");
        assert_eq!(SecretClass::TokenPrefix.as_str(), "token_prefix");
        assert_eq!(SecretClass::Assignment.as_str(), "assignment");
        assert_eq!(SecretClass::HighEntropy.as_str(), "high_entropy");
    }
}
