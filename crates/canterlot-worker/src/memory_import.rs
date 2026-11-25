//! Разовый импорт памяти Claude: план (манифест TOML) и пакет (JSON).
//!
//! `plan` обходит каталог `~/.claude/projects` без перехода по ссылкам, с ограничением
//! размера, и описывает каждый файл `<проект>/memory/*.md`: хэш, размер, вид, тему и
//! причину пропуска. Владелец правит манифест; `bundle` перечитывает ровно перечисленные
//! файлы без пропуска, сверяет хэш и собирает пакет, который применяет хаб
//! (`canterlot-hub memory import`). Значение секрета не попадает ни в манифест, ни в
//! ошибку: наружу идёт только класс находки.
//!
//! Детектор секретов общий с хабом: тот же исходный файл, без копии.

use std::{
    fs,
    io::Read as _,
    os::fd::OwnedFd,
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::machine::nofollow;

#[allow(dead_code)]
#[path = "../../canterlot-hub/src/memory/secrets.rs"]
mod secrets;

/// Запись памяти не длиннее 8 КиБ: больше хаб не примет.
pub const BODY_MAX_BYTES: u64 = 8 * 1024;
/// Файл крупнее не читается вовсе: он помечается `too_large` без хэша.
const READ_CAP: u64 = 64 * 1024;
const SUBJECT_MAX_CHARS: usize = 160;
/// Сколько разделителей имени каталога перебирается; дальше остаётся один кандидат.
const MAX_SEPARATORS: usize = 6;

#[allow(dead_code)]
#[path = "../../canterlot-hub/src/memory/import_rules.rs"]
mod import_rules;

pub use import_rules::{SKIP_CREDENTIAL_NAME, SKIP_INDEX_FILE};
pub const SKIP_LOOKS_LIKE_SECRET: &str = "looks_like_secret";
pub const SKIP_TOO_LARGE: &str = "too_large";
pub const SKIP_NOT_TEXT: &str = "not_text";
pub const SKIP_EMPTY_BODY: &str = "empty_body";

#[derive(Debug, Clone)]
pub struct PlanOptions {
    /// Корни рабочих проектов (по умолчанию `~/work`): всё внутри них домен `work`.
    pub work_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Каталог, который обошёл план; `import-bundle` читает файлы оттуда же.
    pub root: String,
    #[serde(default)]
    pub dir: Vec<DirBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirBlock {
    /// Имя каталога источника, как оно лежит на диске.
    pub source: String,
    /// Лучшая догадка о настоящем пути; владелец вправе её заменить.
    pub decoded_path: String,
    /// Все пути, в которые раскрывается имя каталога.
    #[serde(default)]
    pub candidates: Vec<String>,
    /// Имя каталога не позволило однозначно восстановить путь: проверить вручную.
    pub ambiguous: bool,
    /// `work` или `personal`.
    pub domain: String,
    /// Корень проекта; пусто — записи всего домена.
    pub project: String,
    #[serde(default)]
    pub file: Vec<FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Относительно каталога источника: `memory/<имя>.md`.
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub kind: String,
    pub subject: String,
    /// Пусто или причина: `credential_name`, `looks_like_secret`, `too_large`,
    /// `index_file`, `not_text`, `empty_body`.
    pub skip: String,
}

impl Manifest {
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub bundle_id: String,
    pub created_at: String,
    pub entries: Vec<BundleEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleEntry {
    pub domain: String,
    pub project: Option<String>,
    pub kind: String,
    pub subject: String,
    pub body: String,
    pub source_path: String,
    pub sha256: String,
    pub updated_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("root is not a readable directory")]
    Root,
    #[error("root is a symbolic link")]
    SymlinkRoot,
}

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("manifest root is not a readable directory")]
    Root,
    #[error("bad manifest field {field}")]
    BadManifest { field: &'static str },
    #[error("path component is a symbolic link, refusing to follow it: {path}")]
    Symlink { path: String },
    #[error("file is missing or is not a regular file: {path}")]
    Missing { path: String },
    #[error("hash differs from the manifest: {path}")]
    HashMismatch { path: String },
    #[error("file is larger than the limit: {path}")]
    TooLarge { path: String },
    #[error("file is not text: {path}")]
    NotText { path: String },
    #[error("file body is empty: {path}")]
    EmptyBody { path: String },
    #[error("file looks like a secret ({class}): {path}")]
    LooksLikeSecret { path: String, class: &'static str },
}

/// Составить манифест. Только чтение; ссылки не раскрываются.
pub fn plan(root: &Path, options: &PlanOptions) -> Result<Manifest, PlanError> {
    if is_symlink(root) {
        return Err(PlanError::SymlinkRoot);
    }
    let root = fs::canonicalize(root).map_err(|_| PlanError::Root)?;
    nofollow::open_dir(&root).map_err(|_| PlanError::Root)?;
    let work_roots: Vec<PathBuf> = options
        .work_roots
        .iter()
        .map(|path| fs::canonicalize(path).unwrap_or_else(|_| path.clone()))
        .collect();
    let mut names: Vec<String> = fs::read_dir(&root)
        .map_err(|_| PlanError::Root)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with('-'))
        .collect();
    names.sort();
    let mut dir = Vec::new();
    for source in names {
        let Some(memory) = real_dir(&root.join(&source).join("memory")) else {
            continue;
        };
        if real_dir(&root.join(&source)).is_none() {
            continue;
        }
        let Ok(fd) = nofollow::open_dir(&memory) else {
            continue;
        };
        let (candidates, decoded, ambiguous) = decode(&source);
        let domain = if work_roots.iter().any(|work| decoded.starts_with(work)) {
            "work"
        } else {
            "personal"
        };
        let mut files: Vec<String> = fs::read_dir(&memory)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.ends_with(".md"))
            .collect();
        files.sort();
        let file = files
            .iter()
            .filter_map(|name| describe(&fd, name))
            .collect();
        let decoded_path = decoded.to_string_lossy().into_owned();
        dir.push(DirBlock {
            source,
            project: decoded_path.clone(),
            decoded_path,
            candidates: candidates
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            ambiguous,
            domain: domain.to_owned(),
            file,
        });
    }
    Ok(Manifest {
        root: root.to_string_lossy().into_owned(),
        dir,
    })
}

/// Собрать пакет: перечитать ровно перечисленные файлы без пропуска, сверить хэш.
pub fn bundle(
    manifest: &Manifest,
    bundle_id: &str,
    created_at: &str,
) -> Result<Bundle, BundleError> {
    if is_symlink(Path::new(&manifest.root)) {
        return Err(BundleError::Symlink {
            path: "manifest root".to_owned(),
        });
    }
    let root = fs::canonicalize(&manifest.root).map_err(|_| BundleError::Root)?;
    let mut entries = Vec::new();
    for block in &manifest.dir {
        if block.source.is_empty()
            || block.source.contains('/')
            || block.source == "."
            || block.source == ".."
        {
            return Err(BundleError::BadManifest { field: "source" });
        }
        if !matches!(block.domain.as_str(), "work" | "personal") {
            return Err(BundleError::BadManifest { field: "domain" });
        }
        // Имена индекса и учётных данных отсекаются заново: пометка в манифесте правится рукой.
        let wanted: Vec<&FileEntry> = block
            .file
            .iter()
            .filter(|f| f.skip.is_empty())
            .filter(|f| {
                let base = import_rules::basename(&f.path);
                import_rules::forbidden_name(base).is_none()
            })
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let missing = |path: &str| BundleError::Missing {
            path: format!("{}/{path}", block.source),
        };
        for link in [
            root.join(&block.source),
            root.join(&block.source).join("memory"),
        ] {
            if is_symlink(&link) {
                return Err(BundleError::Symlink {
                    path: format!(
                        "{}{}",
                        block.source,
                        if link.ends_with("memory") {
                            "/memory"
                        } else {
                            ""
                        }
                    ),
                });
            }
        }
        let memory = real_dir(&root.join(&block.source).join("memory"))
            .filter(|_| real_dir(&root.join(&block.source)).is_some())
            .ok_or_else(|| missing("memory"))?;
        let fd = nofollow::open_dir(&memory).map_err(|_| missing("memory"))?;
        for file in wanted {
            let name =
                memory_file_name(&file.path).ok_or(BundleError::BadManifest { field: "path" })?;
            let shown = format!("{}/{}", block.source, file.path);
            let loaded = load(&fd, name).ok_or_else(|| missing(&file.path))?;
            let Some(data) = loaded.data else {
                return Err(BundleError::TooLarge { path: shown });
            };
            if hex(&Sha256::digest(&data)) != file.sha256 {
                return Err(BundleError::HashMismatch { path: shown });
            }
            let Ok(text) = String::from_utf8(data) else {
                return Err(BundleError::NotText { path: shown });
            };
            if text.contains('\0') {
                return Err(BundleError::NotText { path: shown });
            }
            if let Some(class) = secrets::scan(&text) {
                return Err(BundleError::LooksLikeSecret {
                    path: shown,
                    class: class.as_str(),
                });
            }
            let parsed = parse(&text, name);
            if parsed.body.len() as u64 > BODY_MAX_BYTES {
                return Err(BundleError::TooLarge { path: shown });
            }
            if parsed.body.is_empty() {
                return Err(BundleError::EmptyBody { path: shown });
            }
            entries.push(BundleEntry {
                domain: block.domain.clone(),
                project: Some(block.project.trim().to_owned()).filter(|p| !p.is_empty()),
                kind: parsed.kind,
                subject: parsed.subject,
                body: parsed.body,
                source_path: shown,
                sha256: file.sha256.clone(),
                updated_at: loaded.mtime,
            });
        }
    }
    Ok(Bundle {
        bundle_id: bundle_id.to_owned(),
        created_at: created_at.to_owned(),
        entries,
    })
}

/// Сам путь (последний компонент) ссылка; без раскрытия.
fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Каталог, который сам не ссылка и в пути которого нет ссылок.
fn real_dir(path: &Path) -> Option<PathBuf> {
    if !fs::symlink_metadata(path).ok()?.is_dir() {
        return None;
    }
    let canonical = fs::canonicalize(path).ok()?;
    nofollow::open_dir(&canonical).ok()?;
    Some(canonical)
}

/// `memory/<имя>.md` и ничего другого: ни подкаталогов, ни `..`.
fn memory_file_name(path: &str) -> Option<&str> {
    let mut parts = Path::new(path).components();
    let (Some(Component::Normal(first)), Some(Component::Normal(name)), None) =
        (parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    (first == "memory").then_some(())?;
    name.to_str().filter(|name| name.ends_with(".md"))
}

struct Loaded {
    size: u64,
    mtime: String,
    /// `None`, если файл крупнее предела чтения.
    data: Option<Vec<u8>>,
}

/// Открыть файл от дескриптора каталога без ссылок и прочитать не больше предела.
fn load(dir: &OwnedFd, name: &str) -> Option<Loaded> {
    let file = nofollow::open_file(dir, name).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let size = meta.len();
    let mtime = DateTime::<Utc>::from(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH))
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    let data = if size > READ_CAP {
        None
    } else {
        let mut buffer = Vec::new();
        file.take(READ_CAP + 1).read_to_end(&mut buffer).ok()?;
        (buffer.len() as u64 <= READ_CAP).then_some(buffer)
    };
    Some(Loaded { size, mtime, data })
}

/// Запись манифеста. Пропущенный файл несёт только безопасные поля: путь, размер и причину.
/// Тема, вид и хэш заполняются лишь у файла, который пойдёт в пакет, и только после
/// проверки имени и поиска секрета по сырому тексту.
fn describe(dir: &OwnedFd, name: &str) -> Option<FileEntry> {
    let path = format!("memory/{name}");
    let skipped = |size: u64, reason: &str| FileEntry {
        path: path.clone(),
        sha256: String::new(),
        size,
        kind: String::new(),
        subject: String::new(),
        skip: reason.to_owned(),
    };
    // Имя решает до чтения содержимого.
    if let Some(reason) = import_rules::forbidden_name(name) {
        let size = nofollow::open_file(dir, name).ok()?.metadata().ok()?.len();
        return Some(skipped(size, reason));
    }
    let loaded = load(dir, name)?;
    let Some(data) = loaded.data else {
        return Some(skipped(loaded.size, SKIP_TOO_LARGE));
    };
    let Some(text) = String::from_utf8(data.clone())
        .ok()
        .filter(|text| !text.contains('\0'))
    else {
        return Some(skipped(loaded.size, SKIP_NOT_TEXT));
    };
    if secrets::scan(&text).is_some() {
        return Some(skipped(loaded.size, SKIP_LOOKS_LIKE_SECRET));
    }
    let parsed = parse(&text, name);
    if parsed.body.len() as u64 > BODY_MAX_BYTES {
        return Some(skipped(loaded.size, SKIP_TOO_LARGE));
    }
    if parsed.body.is_empty() {
        return Some(skipped(loaded.size, SKIP_EMPTY_BODY));
    }
    Some(FileEntry {
        path,
        sha256: hex(&Sha256::digest(&data)),
        size: loaded.size,
        kind: parsed.kind,
        subject: parsed.subject,
        skip: String::new(),
    })
}

/// Все пути, в которые раскрывается имя каталога Claude (`/` и `-` кодируются одинаково),
/// лучшая догадка и признак неоднозначности. Если на диске существует ровно один
/// кандидат, это и есть ответ.
fn decode(source: &str) -> (Vec<PathBuf>, PathBuf, bool) {
    let mut parts: Vec<String> = Vec::new();
    let mut dotted = false;
    for token in source.trim_start_matches('-').split('-') {
        if token.is_empty() {
            dotted = true;
            continue;
        }
        parts.push(if dotted {
            format!(".{token}")
        } else {
            token.to_owned()
        });
        dotted = false;
    }
    let separators = parts.len().saturating_sub(1);
    let combos = if separators > MAX_SEPARATORS {
        1
    } else {
        1 << separators
    };
    let join = |mask: usize| {
        let mut path = String::new();
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                path.push(if mask & (1 << (index - 1)) == 0 {
                    '/'
                } else {
                    '-'
                });
            } else {
                path.push('/');
            }
            path.push_str(part);
        }
        PathBuf::from(path)
    };
    let candidates: Vec<PathBuf> = Vec::from_fn(combos, join);
    let existing: Vec<&PathBuf> = candidates.iter().filter(|path| path.exists()).collect();
    if let [only] = existing.as_slice() {
        return (vec![(*only).clone()], (*only).clone(), false);
    }
    let first = candidates
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("/"));
    (candidates, first, separators > 0)
}

struct Parsed {
    kind: String,
    subject: String,
    body: String,
}

/// Заголовок `---`: `description` даёт тему (иначе `name`), `type` или `metadata.type` вид.
fn parse(text: &str, file_name: &str) -> Parsed {
    let (front, body) = split_frontmatter(text);
    let (mut name, mut description, mut kind, mut meta_kind) = (None, None, None, None);
    let mut in_metadata = false;
    for line in front.lines() {
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']).trim().to_owned();
        let indented = line.starts_with([' ', '\t']);
        if indented {
            if in_metadata && key == "type" && !value.is_empty() {
                meta_kind = Some(value);
            }
            continue;
        }
        in_metadata = key == "metadata" && value.is_empty();
        match key {
            "name" if !value.is_empty() => name = Some(value),
            "description" if !value.is_empty() => description = Some(value),
            "type" if !value.is_empty() => kind = Some(value),
            _ => {}
        }
    }
    let body = body.to_owned();
    let first_line = body
        .lines()
        .map(|line| line.trim().trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .map(str::to_owned);
    let subject = description
        .or(name)
        .or(first_line)
        .unwrap_or_else(|| file_name.trim_end_matches(".md").to_owned());
    let kind = kind
        .or(meta_kind)
        .map(|kind| kind.to_lowercase())
        .filter(|kind| kind.chars().count() <= 40)
        .unwrap_or_else(|| "reference".to_owned());
    Parsed {
        kind,
        subject: truncate_words(&subject.split_whitespace().collect::<Vec<_>>().join(" ")),
        body,
    }
}

/// Заголовок и тело. Без заголовка всё содержимое тело.
fn split_frontmatter(text: &str) -> (&str, &str) {
    let rest = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"));
    let Some(rest) = rest else {
        return ("", text.trim());
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return (&rest[..offset], rest[offset + line.len()..].trim());
        }
        offset += line.len();
    }
    ("", text.trim())
}

fn truncate_words(line: &str) -> String {
    if line.chars().count() <= SUBJECT_MAX_CHARS {
        return line.to_owned();
    }
    let cut: String = line.chars().take(SUBJECT_MAX_CHARS).collect();
    let on_boundary = line
        .chars()
        .nth(SUBJECT_MAX_CHARS)
        .is_some_and(char::is_whitespace);
    let cut = if on_boundary {
        cut.as_str()
    } else {
        cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head)
    };
    cut.trim_end().to_owned()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_lists_every_split_and_flags_it() {
        let (candidates, best, ambiguous) = decode("-zz-qq-yy");
        assert_eq!(best, PathBuf::from("/zz/qq/yy"));
        assert_eq!(candidates.len(), 4);
        assert!(candidates.contains(&PathBuf::from("/zz-qq/yy")));
        assert!(ambiguous);
        let (candidates, best, ambiguous) = decode("-zz");
        assert_eq!(
            (candidates.len(), best, ambiguous),
            (1, "/zz".into(), false)
        );
    }

    #[test]
    fn frontmatter_without_closing_fence_is_plain_body() {
        let (front, body) = split_frontmatter("---\ndescription: x\nтело");
        assert_eq!(front, "");
        assert!(body.starts_with("---"));
    }
}
