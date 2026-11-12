//! Подпись устройства для списка сеансов, пушей и имени ключа доступа (спецификация §8).

use serde_json::Value;

/// Длина одной части подписи: подсказки клиента ничем не ограничены, кроме размера.
const PART_CHARS: usize = 32;
const APP_MARK: &str = "приложение";
const SEPARATOR: &str = " · ";

/// «Firefox 159 · Linux», «Chrome 142 · Android 16 · Pixel 9», «Safari 19 · iPhone»;
/// установленное приложение добавляет « · приложение». Подсказки клиента главнее user agent.
/// Части, которые не удалось узнать, пропускаются; ничего не узнано — `None`.
pub fn label(user_agent: Option<&str>, hints: Option<&Value>) -> Option<String> {
    let mut parts = device_parts(user_agent, hints);
    if parts.is_empty() {
        return None;
    }
    if hints
        .and_then(|h| h.get("standalone"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        parts.push(APP_MARK.to_owned());
    }
    Some(parts.join(SEPARATOR))
}

/// Имя нового ключа доступа по устройству, на котором его добавили.
pub fn passkey_name(user_agent: Option<&str>, hints: Option<&Value>) -> String {
    let parts = device_parts(user_agent, hints);
    if parts.is_empty() {
        return "Ключ доступа".to_owned();
    }
    parts
        .join(SEPARATOR)
        .chars()
        .take(super::sessions::NAME_CHARS)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

fn device_parts(user_agent: Option<&str>, hints: Option<&Value>) -> Vec<String> {
    let agent = user_agent.map(str::trim).unwrap_or_default();
    let hint = |key: &str| {
        hints
            .and_then(|h| h.get(key))
            .and_then(Value::as_str)
            .map(clean)
            .filter(|value| !value.is_empty())
    };
    let browser = hints
        .and_then(|h| h.get("fullVersionList"))
        .and_then(hinted_browser)
        .or_else(|| agent_browser(agent));
    let system = hint("platform")
        .map(|platform| hinted_system(&platform, hint("platformVersion").as_deref()))
        .or_else(|| agent_system(agent));
    let model = hint("model");
    [browser, system, model].into_iter().flatten().collect()
}

/// Без управляющих символов, без лишних пробелов, не длиннее части подписи.
fn clean(value: &str) -> String {
    let value: String = value
        .chars()
        .filter(|c| !c.is_control())
        .take(PART_CHARS)
        .collect();
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn major(version: &str) -> Option<u32> {
    version.split(['.', '_']).next()?.parse().ok()
}

fn hinted_browser(list: &Value) -> Option<String> {
    const KNOWN: [(&str, &str); 4] = [
        ("Microsoft Edge", "Edge"),
        ("Google Chrome", "Chrome"),
        ("Firefox", "Firefox"),
        ("Chromium", "Chromium"),
    ];
    let brands = list.as_array()?;
    KNOWN.iter().find_map(|(brand, name)| {
        let version = brands
            .iter()
            .find(|entry| entry.get("brand").and_then(Value::as_str) == Some(*brand))?
            .get("version")
            .and_then(Value::as_str)
            .and_then(major)?;
        Some(format!("{name} {version}"))
    })
}

fn hinted_system(platform: &str, version: Option<&str>) -> String {
    let major = version.and_then(major).filter(|major| *major > 0);
    match (platform, major) {
        // Windows 11 сообщает platformVersion от 13, Windows 10 — от 1 до 12.
        ("Windows", Some(major)) if major >= 13 => "Windows 11".into(),
        ("Windows", Some(_)) => "Windows 10".into(),
        ("macOS" | "Android", Some(major)) => format!("{platform} {major}"),
        _ => platform.to_owned(),
    }
}

/// Версия после `token/` в user agent: старшая часть.
fn agent_version(agent: &str, token: &str) -> Option<u32> {
    let start = agent.find(token)? + token.len();
    major(agent[start..].split([' ', ';', ')']).next()?)
}

fn agent_browser(agent: &str) -> Option<String> {
    // Порядок важен: Edge и браузеры iOS пишут в user agent и Chrome, и Safari.
    const TOKENS: [(&str, &str); 8] = [
        ("Edg/", "Edge"),
        ("EdgA/", "Edge"),
        ("EdgiOS/", "Edge"),
        ("FxiOS/", "Firefox"),
        ("CriOS/", "Chrome"),
        ("Firefox/", "Firefox"),
        ("Chrome/", "Chrome"),
        ("Version/", "Safari"),
    ];
    TOKENS.iter().find_map(|(token, name)| {
        if *name == "Safari" && !agent.contains("Safari/") {
            return None;
        }
        agent_version(agent, token).map(|version| format!("{name} {version}"))
    })
}

fn agent_system(agent: &str) -> Option<String> {
    if agent.contains("iPhone") {
        return Some("iPhone".into());
    }
    if agent.contains("iPad") {
        return Some("iPad".into());
    }
    if agent.contains("Android") {
        // Урезанный user agent Chrome: «Android 10; K» при любой настоящей версии.
        if agent.contains("Android 10; K)") {
            return Some("Android".into());
        }
        return Some(match agent_version(agent, "Android ") {
            Some(version) => format!("Android {version}"),
            None => "Android".into(),
        });
    }
    if agent.contains("Windows") {
        return Some("Windows".into());
    }
    if agent.contains("Macintosh") || agent.contains("Mac OS X") {
        return Some("macOS".into());
    }
    if agent.contains("Linux") || agent.contains("X11") {
        return Some("Linux".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const FIREFOX_LINUX: &str =
        "Mozilla/5.0 (X11; Linux x86_64; rv:159.0) Gecko/20100101 Firefox/159.0";
    const FIREFOX_WINDOWS: &str =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:159.0) Gecko/20100101 Firefox/159.0";
    const FIREFOX_ANDROID: &str =
        "Mozilla/5.0 (Android 15; Mobile; rv:159.0) Gecko/159.0 Firefox/159.0";
    const CHROME_ANDROID: &str = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 \
        (KHTML, like Gecko) Chrome/142.0.0.0 Mobile Safari/537.36";
    const CHROME_LINUX: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
        (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
    const CHROME_MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
        AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
    const EDGE_WINDOWS: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
        (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 Edg/142.0.3595.53";
    const SAFARI_IPHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 19_0 like Mac OS X) \
        AppleWebKit/605.1.15 (KHTML, like Gecko) Version/19.0 Mobile/15E148 Safari/604.1";
    const SAFARI_IPAD: &str = "Mozilla/5.0 (iPad; CPU OS 19_0 like Mac OS X) \
        AppleWebKit/605.1.15 (KHTML, like Gecko) Version/19.0 Mobile/15E148 Safari/604.1";
    const SAFARI_MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
        AppleWebKit/605.1.15 (KHTML, like Gecko) Version/19.0 Safari/605.1.15";
    const CHROME_IPHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 19_0 like Mac OS X) \
        AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/142.0.7444.10 Mobile/15E148 Safari/604.1";
    const FIREFOX_IPHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 19_0 like Mac OS X) \
        AppleWebKit/605.1.15 (KHTML, like Gecko) FxiOS/159.0 Mobile/15E148 Safari/605.1.15";
    const EDGE_ANDROID: &str = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 \
        (KHTML, like Gecko) Chrome/142.0.0.0 Mobile Safari/537.36 EdgA/142.0.3595.53";

    fn chrome_hints(platform: &str, version: &str, model: &str) -> Value {
        json!({
            "platform": platform,
            "platformVersion": version,
            "model": model,
            "fullVersionList": [
                {"brand": "Not_A Brand", "version": "99.0.0.0"},
                {"brand": "Chromium", "version": "142.0.7444.10"},
                {"brand": "Google Chrome", "version": "142.0.7444.10"},
            ],
        })
    }

    #[test]
    fn user_agent_alone_names_browser_and_system() {
        let table = [
            (FIREFOX_LINUX, "Firefox 159 · Linux"),
            (FIREFOX_WINDOWS, "Firefox 159 · Windows"),
            (FIREFOX_ANDROID, "Firefox 159 · Android 15"),
            // Урезанный user agent Chrome всегда пишет «Android 10; K»: версия ложная.
            (CHROME_ANDROID, "Chrome 142 · Android"),
            (CHROME_LINUX, "Chrome 142 · Linux"),
            (CHROME_MAC, "Chrome 142 · macOS"),
            (EDGE_WINDOWS, "Edge 142 · Windows"),
            (EDGE_ANDROID, "Edge 142 · Android"),
            (SAFARI_IPHONE, "Safari 19 · iPhone"),
            (SAFARI_IPAD, "Safari 19 · iPad"),
            (SAFARI_MAC, "Safari 19 · macOS"),
            (CHROME_IPHONE, "Chrome 142 · iPhone"),
            (FIREFOX_IPHONE, "Firefox 159 · iPhone"),
        ];
        for (agent, expected) in table {
            assert_eq!(
                label(Some(agent), None).as_deref(),
                Some(expected),
                "{agent}"
            );
        }
    }

    #[test]
    fn client_hints_win_over_the_user_agent() {
        let hints = chrome_hints("Android", "16.0.0", "Pixel 9");
        assert_eq!(
            label(Some(CHROME_ANDROID), Some(&hints)).as_deref(),
            Some("Chrome 142 · Android 16 · Pixel 9")
        );
        let hints = chrome_hints("macOS", "15.1.0", "");
        assert_eq!(
            label(Some(CHROME_MAC), Some(&hints)).as_deref(),
            Some("Chrome 142 · macOS 15")
        );
        let edge = json!({
            "platform": "Windows",
            "platformVersion": "15.0.0",
            "fullVersionList": [
                {"brand": "Chromium", "version": "142.0.7444.10"},
                {"brand": "Microsoft Edge", "version": "142.0.3595.53"},
            ],
        });
        assert_eq!(
            label(Some(EDGE_WINDOWS), Some(&edge)).as_deref(),
            Some("Edge 142 · Windows 11")
        );
        let windows10 = json!({"platform": "Windows", "platformVersion": "10.0.0"});
        assert_eq!(
            label(Some(EDGE_WINDOWS), Some(&windows10)).as_deref(),
            Some("Edge 142 · Windows 10")
        );
        let linux = json!({"platform": "Linux", "platformVersion": ""});
        assert_eq!(
            label(Some(CHROME_LINUX), Some(&linux)).as_deref(),
            Some("Chrome 142 · Linux")
        );
    }

    #[test]
    fn installed_app_is_marked() {
        let hints = json!({"standalone": true});
        assert_eq!(
            label(Some(SAFARI_IPHONE), Some(&hints)).as_deref(),
            Some("Safari 19 · iPhone · приложение")
        );
        let mut hints = chrome_hints("Android", "16.0.0", "Pixel 9");
        hints["standalone"] = json!(true);
        assert_eq!(
            label(Some(CHROME_ANDROID), Some(&hints)).as_deref(),
            Some("Chrome 142 · Android 16 · Pixel 9 · приложение")
        );
    }

    #[test]
    fn unknown_parts_are_left_out() {
        assert_eq!(label(Some("curl/8.11.1"), None), None);
        assert_eq!(label(None, None), None);
        assert_eq!(label(Some("   "), None), None);
        assert_eq!(
            label(Some("Mozilla/5.0 (X11; Linux x86_64) Something/1.0"), None).as_deref(),
            Some("Linux")
        );
        assert_eq!(
            label(
                Some("Mozilla/5.0 (Plan9) Gecko/20100101 Firefox/159.0"),
                None
            )
            .as_deref(),
            Some("Firefox 159")
        );
    }

    #[test]
    fn hostile_hints_are_bounded() {
        let hints = json!({
            "platform": 7,
            "model": format!("Pixel\u{0007}{}", "x".repeat(200)),
            "fullVersionList": "nonsense",
            "standalone": "yes",
        });
        let label = label(Some(CHROME_ANDROID), Some(&hints)).unwrap();
        assert!(label.starts_with("Chrome 142 · Android · Pixel"), "{label}");
        assert!(!label.contains('\u{0007}'));
        assert!(label.chars().count() <= 96, "{label}");
        assert!(!label.ends_with("приложение"));
    }

    #[test]
    fn passkey_name_follows_the_device_without_the_app_mark() {
        let hints = json!({"standalone": true});
        assert_eq!(
            passkey_name(Some(SAFARI_IPHONE), Some(&hints)),
            "Safari 19 · iPhone"
        );
        assert_eq!(passkey_name(None, None), "Ключ доступа");
        let long = json!({"model": "M".repeat(80), "platform": "Android"});
        assert!(
            passkey_name(Some(CHROME_ANDROID), Some(&long))
                .chars()
                .count()
                <= 64
        );
    }
}
