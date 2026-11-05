//! История Claude Code: каталог проектов CLI и короткий обзор бесед.
//!
//! Читается только `~/.claude/projects/<slug>/` (или `CLAUDE_CONFIG_DIR/projects/<slug>/`):
//! ни учётные данные, ни соседние проекты. В журналы пути не попадают, как и у подсказок папок.
use serde_json::Value;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use uuid::Uuid;

/// Предел хвоста, из которого берётся первый вопрос владельца.
const PREVIEW_BYTES: u64 = 256 * 1024;
/// Предел подписи в списке.
pub const MAX_PREVIEW_CHARS: usize = 120;
/// Длина слага до хеша: ровно столько же, сколько у CLI.
const SLUG_LIMIT: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeHistory {
    pub id: String,
    pub modified_at_ms: u64,
    pub size: u64,
    pub preview: String,
}

/// Слаг каталога проекта: CLI заменяет каждый небуквенно-цифровой символ на `-`.
/// Длинный путь обрезается до 200 символов и получает суффикс из хеша всего пути.
pub fn project_slug(workdir: &str) -> String {
    let path = fs::canonicalize(workdir)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| workdir.to_owned());
    let slug: String = path
        .encode_utf16()
        .map(|unit| {
            if unit < 128 && (unit as u8).is_ascii_alphanumeric() {
                char::from(unit as u8)
            } else {
                '-'
            }
        })
        .collect();
    if slug.len() <= SLUG_LIMIT {
        return slug;
    }
    format!("{}-{}", &slug[..SLUG_LIMIT], hash_base36(&path))
}

/// Хеш строки как у CLI: `(h << 5) - h + code` по 16-битным единицам, модуль в base 36.
fn hash_base36(text: &str) -> String {
    let mut hash = 0i32;
    for unit in text.encode_utf16() {
        hash = hash
            .wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(i32::from(unit));
    }
    let value = i64::from(hash).unsigned_abs();
    let mut digits = Vec::new();
    let mut rest = value;
    loop {
        let digit = (rest % 36) as u8;
        digits.push(if digit < 10 {
            char::from(b'0' + digit)
        } else {
            char::from(b'a' + digit - 10)
        });
        rest /= 36;
        if rest == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}

fn project_dir(workdir: &str, config_dir: &Path) -> Option<PathBuf> {
    if workdir.is_empty() || !workdir.starts_with('/') || workdir.contains('\0') {
        return None;
    }
    Some(config_dir.join("projects").join(project_slug(workdir)))
}

/// Файл беседы строго внутри каталога проекта: только UUID, никаких путей от клиента.
pub fn transcript(workdir: &str, config_dir: &Path, id: &str) -> Option<PathBuf> {
    let id = Uuid::parse_str(id).ok()?;
    let dir = project_dir(workdir, config_dir)?;
    let path = dir.join(format!("{id}.jsonl"));
    let meta = fs::symlink_metadata(&path).ok()?;
    meta.file_type().is_file().then_some(path)
}

/// Беседы рабочего каталога, свежие первыми. Записи живых исполнений не предлагаются.
pub fn list_histories(
    workdir: &str,
    config_dir: &Path,
    excluded: &HashSet<String>,
) -> Vec<ClaudeHistory> {
    let Some(dir) = project_dir(workdir, config_dir) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found: Vec<ClaudeHistory> = entries
        .flatten()
        .filter_map(|entry| {
            let kind = entry.file_type().ok()?;
            if !kind.is_file() {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            let id = name.strip_suffix(".jsonl")?;
            if Uuid::parse_str(id).is_err() || excluded.contains(id) {
                return None;
            }
            let meta = entry.metadata().ok()?;
            Some(ClaudeHistory {
                id: id.to_owned(),
                modified_at_ms: meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX)),
                size: meta.len(),
                preview: preview(&entry.path()).unwrap_or_default(),
            })
        })
        .collect();
    found.sort_by(|left, right| {
        right
            .modified_at_ms
            .cmp(&left.modified_at_ms)
            .then_with(|| right.id.cmp(&left.id))
    });
    found
}

/// Первый вопрос владельца из ограниченного начала файла; битые строки пропускаются.
fn preview(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file.take(PREVIEW_BYTES));
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line).ok()?;
        if read == 0 {
            return None;
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        if let Some(preview) = preview_from_line(text) {
            return Some(preview);
        }
    }
}

fn preview_from_line(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "user"
        || value.get("isMeta").and_then(Value::as_bool) == Some(true)
        || value.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let content = value.pointer("/message/content")?;
    let text = match content {
        Value::String(text) => text.as_str(),
        // Блоки инструментов не текст вопроса: берём первый текстовый блок.
        Value::Array(blocks) => blocks
            .iter()
            .find_map(|block| block.get("text").and_then(Value::as_str))?,
        _ => return None,
    };
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(MAX_PREVIEW_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    fn history_file(dir: &Path, id: &str, lines: &[&str]) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(format!("{id}.jsonl")), lines.join("\n")).unwrap();
    }

    fn at(path: &Path, seconds: u64) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
            .unwrap();
    }

    fn lines(workdir: &str) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("projects").join(project_slug(workdir));
        fs::create_dir_all(&dir).unwrap();
        (root, dir)
    }

    #[test]
    fn slug_matches_the_cli_rule() {
        assert_eq!(project_slug("/home/fenicu"), "-home-fenicu");
        assert_eq!(
            project_slug("/home/fenicu/work/my.repo_x"),
            "-home-fenicu-work-my-repo-x"
        );
        let long = format!("/{}", "a".repeat(250));
        let slug = project_slug(&long);
        assert_eq!(&slug[..SLUG_LIMIT], format!("-{}", "a".repeat(199)));
        let tail = slug[SLUG_LIMIT + 1..].to_owned();
        assert!(!tail.is_empty());
        assert!(tail.chars().all(|ch| ch.is_ascii_alphanumeric()));
        assert_eq!(tail, hash_base36(&long));
    }

    #[test]
    fn listing_sorts_by_mtime_desc_reads_a_bounded_preview_and_skips_noise() {
        let workdir = "/srv/project";
        let (root, dir) = lines(workdir);
        let older = Uuid::now_v7().to_string();
        let newer = Uuid::now_v7().to_string();
        let live = Uuid::now_v7().to_string();
        history_file(
            &dir,
            &older,
            &[
                "not json",
                r#"{"type":"summary","summary":"old"}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x"}]}}"#,
                r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"system note"}}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"  Первая\n строка\tвопроса  "}]}}"#,
                r#"{"type":"user","message":{"role":"user","content":"второй вопрос"}}"#,
            ],
        );
        let text = "слово ".repeat(40);
        history_file(
            &dir,
            &newer,
            &[&format!(
                r#"{{"type":"user","message":{{"role":"user","content":"{text}"}}}}"#
            )],
        );
        history_file(
            &dir,
            &live,
            &[r#"{"type":"user","message":{"role":"user","content":"живая"}}"#],
        );
        // Не UUID, не .jsonl и символическая ссылка не считаются историей.
        history_file(&dir, "not-a-uuid", &["{}"]);
        fs::write(dir.join("notes.txt"), "{}").unwrap();
        let outside = root.path().join("outside.jsonl");
        fs::write(
            &outside,
            r#"{"type":"user","message":{"role":"user","content":"снаружи"}}"#,
        )
        .unwrap();
        let link = Uuid::now_v7().to_string();
        std::os::unix::fs::symlink(&outside, dir.join(format!("{link}.jsonl"))).unwrap();
        at(&dir.join(format!("{older}.jsonl")), 100);
        at(&dir.join(format!("{newer}.jsonl")), 200);
        at(&dir.join(format!("{live}.jsonl")), 300);

        let excluded = HashSet::from([live.clone()]);
        let found = list_histories(workdir, root.path(), &excluded);
        assert_eq!(
            found
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            [newer.as_str(), older.as_str()]
        );
        assert_eq!(found[0].preview.chars().count(), MAX_PREVIEW_CHARS);
        assert_eq!(found[1].preview, "Первая строка вопроса");
        assert_eq!(
            found[1].size,
            fs::metadata(dir.join(format!("{older}.jsonl")))
                .unwrap()
                .len()
        );
        assert_eq!(found[0].modified_at_ms, 200_000);
        assert_eq!(found[1].modified_at_ms, 100_000);
    }

    #[test]
    fn listing_is_empty_for_a_missing_project_directory() {
        let root = tempfile::tempdir().unwrap();
        assert!(list_histories("/srv/never", root.path(), &HashSet::new()).is_empty());
        assert!(list_histories("relative", root.path(), &HashSet::new()).is_empty());
    }

    #[test]
    fn transcript_accepts_only_an_existing_uuid_in_this_workdir() {
        let workdir = "/srv/project";
        let (root, dir) = lines(workdir);
        let id = Uuid::now_v7().to_string();
        history_file(
            &dir,
            &id,
            &[r#"{"type":"user","message":{"role":"user","content":"привет"}}"#],
        );
        assert!(transcript(workdir, root.path(), &id).is_some());

        let other = "/srv/other";
        assert!(transcript(other, root.path(), &id).is_none());
        assert!(transcript(workdir, root.path(), &Uuid::now_v7().to_string()).is_none());
        assert!(transcript(workdir, root.path(), "../../etc/passwd").is_none());
        assert!(transcript(workdir, root.path(), "not-a-uuid").is_none());

        std::os::unix::fs::symlink(
            root.path().join("outside.jsonl"),
            dir.join(format!("{}.jsonl", Uuid::now_v7())),
        )
        .unwrap();
    }
}
