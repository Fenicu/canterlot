//! Тексты событий сторожа машины. Подлежащее — машина, а не сессия получателя:
//! сообщение говорит, что на ней не так и что с этим сделать.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

const GB: f64 = 1e9;
/// Каталогов сборки в тексте; в задании их больше.
const TEXT_HOTSPOTS: usize = 3;

fn gb(bytes: u64) -> String {
    let value = bytes as f64 / GB;
    if value >= 10.0 {
        format!("{value:.0} GB")
    } else {
        let text = format!("{value:.1}");
        format!("{} GB", text.trim_end_matches(".0"))
    }
}

fn age(since: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - since).num_seconds().max(0);
    if seconds < 3600 {
        format!("{} min", (seconds / 60).max(1))
    } else if seconds < 48 * 3600 {
        format!("{} h", seconds / 3600)
    } else {
        format!("{} d", seconds / 86_400)
    }
}

fn cache_name(kind: &str) -> &str {
    match kind {
        "svelte_kit" => ".svelte-kit",
        "web_build" => "web/build",
        other => other,
    }
}

fn hotspot(h: &Value, now: DateTime<Utc>) -> String {
    let bytes = h["bytes"].as_u64().unwrap_or(0);
    let incremental = h["incremental_bytes"].as_u64().unwrap_or(0);
    let newest = h["newest_at"]
        .as_str()
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.with_timezone(&Utc));
    match h["title"].as_str() {
        Some(title) => {
            let mut text = format!(
                "worktree «{title}» {} {}",
                cache_name(h["kind"].as_str().unwrap_or_default()),
                gb(bytes)
            );
            if let Some(newest) = newest {
                text.push_str(&format!(" (idle {})", age(newest, now)));
            }
            text
        }
        None => {
            let mut text = format!("{} {}", h["path"].as_str().unwrap_or_default(), gb(bytes));
            let mut notes = Vec::new();
            if incremental >= 1_000_000_000 {
                notes.push(format!("{} incremental", gb(incremental)));
            }
            if let Some(newest) = newest {
                notes.push(format!("written {} ago", age(newest, now)));
            }
            if !notes.is_empty() {
                text.push_str(&format!(" ({})", notes.join(", ")));
            }
            text
        }
    }
}

pub fn disk(machine: &str, critical: bool, details: &Value, now: DateTime<Utc>) -> String {
    let mut text = format!(
        "[canterlot] Machine \"{machine}\" is {}low on disk: {} GB free ({}%). \
         Builds and new sessions may fail.",
        if critical { "critically " } else { "" },
        details["free_gb"].as_u64().unwrap_or(0),
        details["free_pct"].as_u64().unwrap_or(0),
    );
    let caches: Vec<String> = details["hotspots"]
        .as_array()
        .into_iter()
        .flatten()
        .take(TEXT_HOTSPOTS)
        .map(|h| hotspot(h, now))
        .collect();
    if !caches.is_empty() {
        text.push_str(&format!(
            " Largest build caches: {}. Run scripts/prune-target.sh or remove \
             target/debug/incremental in idle checkouts.",
            caches.join("; ")
        ));
    }
    text
}

pub fn memory(machine: &str, details: &Value) -> String {
    let mut text = format!(
        "[canterlot] Machine \"{machine}\" is low on memory: {} available of {}.",
        gb(details["available_bytes"].as_u64().unwrap_or(0)),
        gb(details["total_bytes"].as_u64().unwrap_or(0)),
    );
    let heaviest: Vec<String> = details["heaviest"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| {
            let name = e["title"]
                .as_str()
                .filter(|t| !t.is_empty())
                .map(|t| format!("«{t}»"))
                .unwrap_or_else(|| {
                    format!(
                        "execution {}",
                        e["execution_id"].as_str().unwrap_or_default()
                    )
                });
            format!("{name} {}", gb(e["rss_bytes"].as_u64().unwrap_or(0)))
        })
        .collect();
    if !heaviest.is_empty() {
        text.push_str(&format!(" Largest sessions: {}.", heaviest.join(", ")));
    }
    text.push_str(" Builds may fail or be killed; avoid starting heavy work there now.");
    text
}

pub fn leaked(machine: &str, details: &Value) -> String {
    let rows: Vec<&Value> = details["leaked"].as_array().into_iter().flatten().collect();
    let mut by_comm: BTreeMap<&str, usize> = BTreeMap::new();
    let mut rss = 0u64;
    let mut sources: Vec<String> = Vec::new();
    for row in &rows {
        *by_comm
            .entry(row["comm"].as_str().unwrap_or("?"))
            .or_default() += 1;
        rss = rss.saturating_add(row["rss_bytes"].as_u64().unwrap_or(0));
        let source = row["title"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(|t| format!("«{t}»"))
            .unwrap_or_else(|| {
                format!(
                    "execution {}",
                    row["execution_id"].as_str().unwrap_or_default()
                )
            });
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    let mut comms: Vec<(&str, usize)> = by_comm.into_iter().collect();
    comms.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let comms: Vec<String> = comms.iter().map(|(c, n)| format!("{c} {n}")).collect();
    let count = rows.len();
    format!(
        "[canterlot] {count} {} on \"{machine}\" ({}; {} RSS): from {}. \
         Nothing is stopped automatically.",
        if count == 1 {
            "process outlived its session"
        } else {
            "processes outlived their sessions"
        },
        comms.join(", "),
        gb(rss),
        sources.join(", "),
    )
}

/// Аренда тяжёлой задачи держится дольше порога. Держатель — сессия или терминал
/// владельца; остальные тяжёлые задачи машины ждут за ней.
pub fn lease_long_hold(machine: &str, details: &Value) -> String {
    let lease = if details["lease_kind"].as_str() == Some("gate") {
        "gate"
    } else {
        "build"
    };
    let holder = match details["holder_title"].as_str().filter(|t| !t.is_empty()) {
        _ if details["holder"].as_str() == Some("owner") => "the owner's terminal".to_owned(),
        Some(title) => format!(
            "session «{title}» (id {})",
            details["conversation_id"].as_str().unwrap_or_default()
        ),
        None => format!(
            "execution {}",
            details["execution_id"].as_str().unwrap_or_default()
        ),
    };
    let label = details["label"]
        .as_str()
        .filter(|l| !l.is_empty())
        .map(|l| format!(" \"{l}\""))
        .unwrap_or_default();
    format!(
        "[canterlot] Machine \"{machine}\": {holder} has held the {lease} lease{label} for {} min. \
         Other heavy tasks on the machine queue behind it; check whether the command hangs. \
         Nothing is stopped automatically.",
        details["held_minutes"].as_i64().unwrap_or(0).max(1)
    )
}

/// Числа и имена события для PWA: только то, из чего собран английский текст.
/// Списки урезаны, служебных полей воркера здесь нет.
pub fn view_details(kind: &str, details: &Value) -> Value {
    match kind {
        "disk_low" | "disk_critical" => {
            let hotspots: Vec<Value> = details["hotspots"]
                .as_array()
                .into_iter()
                .flatten()
                .take(5)
                .map(|h| {
                    json!({
                        "path": h["path"],
                        "kind": h["kind"],
                        "bytes": h["bytes"],
                        "incremental_bytes": h["incremental_bytes"],
                        "newest_at": h["newest_at"],
                        "title": h["title"],
                    })
                })
                .collect();
            json!({
                "free_gb": details["free_gb"],
                "free_pct": details["free_pct"],
                "hotspots": hotspots,
            })
        }
        "mem_low" => {
            let heaviest: Vec<Value> = details["heaviest"]
                .as_array()
                .into_iter()
                .flatten()
                .take(5)
                .map(|e| {
                    json!({
                        "execution_id": e["execution_id"],
                        "title": e["title"],
                        "rss_bytes": e["rss_bytes"],
                    })
                })
                .collect();
            json!({
                "available_bytes": details["available_bytes"],
                "total_bytes": details["total_bytes"],
                "heaviest": heaviest,
            })
        }
        "processes_leaked" => {
            let leaked: Vec<Value> = details["leaked"]
                .as_array()
                .into_iter()
                .flatten()
                .take(5)
                .map(|e| {
                    json!({
                        "execution_id": e["execution_id"],
                        "comm": e["comm"],
                        "rss_bytes": e["rss_bytes"],
                        "title": e["title"],
                    })
                })
                .collect();
            json!({"leaked": leaked})
        }
        "lease_long_hold" => json!({
            "lease_kind": details["lease_kind"],
            "holder": details["holder"],
            "holder_title": details["holder_title"],
            "conversation_id": details["conversation_id"],
            "execution_id": details["execution_id"],
            "label": details["label"],
            "held_minutes": details["held_minutes"],
        }),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;
    use serde_json::json;

    #[test]
    fn disk_text_names_the_largest_caches_and_the_fix() {
        let now = Utc::now();
        let at = |h: i64| crate::conversations::time(now - TimeDelta::hours(h));
        let details = json!({
            "free_gb": 28, "free_pct": 9,
            "hotspots": [
                {"path": "/home/o/p/canterlot-2a/target", "kind": "target", "bytes": 77_000_000_000u64,
                 "incremental_bytes": 31_000_000_000u64, "newest_at": at(2), "title": null},
                {"path": "/data/worktrees/x/target", "kind": "target", "bytes": 12_000_000_000u64,
                 "incremental_bytes": 4_000_000_000u64, "newest_at": at(5), "title": "Баннер связи"},
                {"path": "/home/o/p/web/node_modules", "kind": "node_modules", "bytes": 1_200_000_000u64,
                 "incremental_bytes": 0, "newest_at": null, "title": null},
                {"path": "/home/o/q/target", "kind": "target", "bytes": 500_000_000u64,
                 "incremental_bytes": 0, "newest_at": null, "title": null}
            ]
        });
        assert_eq!(
            disk("dev-vm", false, &details, now),
            "[canterlot] Machine \"dev-vm\" is low on disk: 28 GB free (9%). Builds and new \
             sessions may fail. Largest build caches: /home/o/p/canterlot-2a/target 77 GB \
             (31 GB incremental, written 2 h ago); worktree «Баннер связи» target 12 GB \
             (idle 5 h); /home/o/p/web/node_modules 1.2 GB. Run scripts/prune-target.sh or \
             remove target/debug/incremental in idle checkouts."
        );
        let bare = json!({"free_gb": 8, "free_pct": 3, "hotspots": []});
        assert_eq!(
            disk("dev-vm", true, &bare, now),
            "[canterlot] Machine \"dev-vm\" is critically low on disk: 8 GB free (3%). \
             Builds and new sessions may fail."
        );
    }

    #[test]
    fn memory_text_names_the_heaviest_sessions() {
        let details = json!({
            "available_bytes": 2_100_000_000u64, "total_bytes": 64_000_000_000u64,
            "heaviest": [
                {"execution_id": "e1", "title": "Toolkit", "rss_bytes": 6_200_000_000u64},
                {"execution_id": "e2", "title": null, "rss_bytes": 3_000_000_000u64}
            ]
        });
        assert_eq!(
            memory("dev-vm", &details),
            "[canterlot] Machine \"dev-vm\" is low on memory: 2.1 GB available of 64 GB. \
             Largest sessions: «Toolkit» 6.2 GB, execution e2 3 GB. Builds may fail or be \
             killed; avoid starting heavy work there now."
        );
    }

    #[test]
    fn leak_text_counts_by_command_and_names_sources_once() {
        let details = json!({"leaked": [
            {"execution_id": "e1", "title": "Toolkit", "comm": "cargo", "rss_bytes": 600_000_000u64},
            {"execution_id": "e1", "title": "Toolkit", "comm": "cargo", "rss_bytes": 380_000_000u64},
            {"execution_id": "e2", "title": "Баннер", "comm": "node", "rss_bytes": 420_000_000u64}
        ]});
        assert_eq!(
            leaked("dev-vm", &details),
            "[canterlot] 3 processes outlived their sessions on \"dev-vm\" (cargo 2, node 1; \
             1.4 GB RSS): from «Toolkit», «Баннер». Nothing is stopped automatically."
        );
        let one = json!({"leaked": [{"execution_id": "e9", "title": null, "comm": "node", "rss_bytes": 50_000_000u64}]});
        assert_eq!(
            leaked("m", &one),
            "[canterlot] 1 process outlived its session on \"m\" (node 1; 0.1 GB RSS): from \
             execution e9. Nothing is stopped automatically."
        );
    }
}
