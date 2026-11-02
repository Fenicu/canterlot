//! Чтение глобальных инструкций владельца для экрана «Как у владельца».
//! Запрос называет только вид, для памяти — идентификатор проекта из обзора
//! и имя файла, никогда путь. Пути строятся из фиксированного списка,
//! проверяются канонизацией и не выходят за разрешённые каталоги.
//! Глобальный файл и файл памяти ссылкой не бывают: ссылка вместо настоящего
//! файла не читается и видна с причиной `symlink`, чтение идёт через дескриптор
//! с `O_NOFOLLOW`, чтобы подмена между проверкой и чтением не увела в чужой файл.
//!
//! Подтверждённые пути CLI:
//! - Claude Code: пользовательский `~/.claude/CLAUDE.md` (или `CLAUDE_CONFIG_DIR`,
//!   если задан абсолютным путём) и `~/CLAUDE.md` выше рабочего каталога
//!   (документация Claude Code «How Claude remembers your project»: таблица
//!   областей и раздел «How CLAUDE.md files load» — файлы текущего каталога
//!   и всех каталогов выше него, пользовательский уровень `~/.claude/CLAUDE.md`).
//!   Память — `~/.claude/projects/<project>/memory/` с индексом `MEMORY.md`
//!   (раздел «Storage location» той же страницы).
//! - Codex: `$CODEX_HOME/AGENTS.md` (по умолчанию `~/.codex/AGENTS.md`),
//!   при наличии `AGENTS.override.md` он читается вместо него (документация
//!   Codex «Custom instructions with AGENTS.md»: глобальная область, порядок
//!   `AGENTS.override.md`, затем `AGENTS.md`).
//! - OpenCode: `~/.config/opencode/AGENTS.md` (документация OpenCode V2
//!   «Instructions»: блок Scope с примером глобального файла).

use canterlot_proto::worker::v1::{
    OwnerFileMeta, OwnerInstructionsFile, OwnerInstructionsOverview, OwnerMemoryFile,
    OwnerMemoryProject,
};
use std::ffi::OsString;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Урезанный файл: не больше 256 КиБ содержимого.
pub const MAX_OWNER_FILE_BYTES: usize = 256 * 1024;
pub const TRUNCATED_MARKER: &str = "\n…[truncated]";
/// Сколько проектов памяти и файлов в проекте отдаёт обзор.
pub const MAX_MEMORY_PROJECTS: usize = 200;
pub const MAX_MEMORY_FILES: usize = 50;

/// Виды запроса: обзор и шесть глобальных файлов плюс два вида памяти.
pub const KIND_OVERVIEW: &str = "overview";
pub const KIND_CLAUDE_USER: &str = "claude_user";
pub const KIND_CLAUDE_HOME: &str = "claude_home";
pub const KIND_CODEX_GLOBAL: &str = "codex_global";
pub const KIND_CODEX_OVERRIDE: &str = "codex_override";
pub const KIND_OPENCODE_GLOBAL: &str = "opencode_global";
pub const KIND_MEMORY_INDEX: &str = "memory_index";
pub const KIND_MEMORY_FILE: &str = "memory_file";

/// Причина отказа в показе при `present = false`: файл оказался ссылкой.
pub const REASON_SYMLINK: &str = "symlink";

/// Имена файлов, которые никогда не отдаются, даже при совпадении каталога.
const DENIED_NAMES: &[&str] = &[".credentials.json", "credentials.json", "auth.json", ".env"];

/// Корни, из которых строятся разрешённые пути.
#[derive(Clone, Debug)]
pub struct OwnerRoots {
    pub home: PathBuf,
    pub claude_config_dir: PathBuf,
    pub codex_home: PathBuf,
    pub opencode_config_dir: PathBuf,
}

impl OwnerRoots {
    /// Тестовый шов: все корни заданы явно.
    pub fn new(
        home: PathBuf,
        claude_config_dir: PathBuf,
        codex_home: PathBuf,
        opencode_config_dir: PathBuf,
    ) -> Self {
        Self {
            home,
            claude_config_dir,
            codex_home,
            opencode_config_dir,
        }
    }

    pub fn from_env(
        home: &Path,
        env: &[(OsString, OsString)],
        codex_owner_home: Option<&Path>,
        opencode_owner_config: Option<&Path>,
    ) -> Self {
        let get = |key: &str| {
            env.iter()
                .rev()
                .find(|(name, _)| name.to_string_lossy() == key)
                .map(|(_, value)| PathBuf::from(value))
        };
        let claude_config_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(|| home.join(".claude"));
        let codex_home = codex_owner_home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| get("CODEX_HOME").unwrap_or_else(|| home.join(".codex")));
        let opencode_config_dir =
            opencode_owner_config
                .map(Path::to_path_buf)
                .unwrap_or_else(|| {
                    get("XDG_CONFIG_HOME")
                        .map(|dir| Path::new(&dir).join("opencode"))
                        .unwrap_or_else(|| home.join(".config/opencode"))
                });
        Self {
            home: home.to_path_buf(),
            claude_config_dir,
            codex_home,
            opencode_config_dir,
        }
    }
}

fn denied_name(name: &str) -> bool {
    DENIED_NAMES.contains(&name)
        || name.ends_with(".sqlite")
        || name.ends_with(".sqlite-wal")
        || name.ends_with(".sqlite-shm")
        || name.ends_with(".sqlite-journal")
        || name.ends_with(".lock")
}

fn valid_project(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-' || b == b'=')
        && id != "."
        && id != ".."
}

fn valid_memory_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name.ends_with(".md")
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
        && !denied_name(name)
}

fn mtime_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn canonical_base(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Канонический путь остаётся внутри разрешённого каталога.
fn inside(canonical: &Path, base: &Path) -> bool {
    canonical == base || canonical.starts_with(base)
}

/// Исход чтения: текст, отсутствие или отказ из-за ссылки.
enum ReadOutcome {
    Text {
        text: String,
        size: u64,
        mtime: u64,
        truncated: bool,
    },
    Absent,
    Symlink,
}

fn is_loop(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
}

/// Открыть файл без перехода по ссылке: подмена между проверкой и чтением
/// упрётся в ошибку, а не в чужое содержимое.
fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

/// Прочитать обычный текстовый файл с лимитом и проверками. Ссылка —
/// отдельный исход с причиной, а не молчаливое отсутствие: настоящий
/// глобальный файл или файл памяти ссылкой никогда не бывает.
fn read_text(expected: &Path, base: &Path) -> ReadOutcome {
    use std::io::Read as _;
    let meta = match std::fs::symlink_metadata(expected) {
        Ok(meta) => meta,
        Err(_) => return ReadOutcome::Absent,
    };
    if meta.file_type().is_symlink() {
        return ReadOutcome::Symlink;
    }
    if !meta.is_file() {
        return ReadOutcome::Absent;
    }
    let (Some(parent), Some(name)) = (expected.parent(), expected.file_name()) else {
        return ReadOutcome::Absent;
    };
    // Канонический путь обязан совпасть с ожидаемым побайтово: ссылка
    // в любом месте цепочки выше уже раскрыта канонизацией родителя.
    let Ok(parent_canon) = std::fs::canonicalize(parent) else {
        return ReadOutcome::Absent;
    };
    let Ok(canonical) = std::fs::canonicalize(expected) else {
        return ReadOutcome::Absent;
    };
    if canonical != parent_canon.join(name) || !inside(&canonical, base) {
        return ReadOutcome::Absent;
    }
    if let Some(name) = canonical.file_name().and_then(|n| n.to_str())
        && denied_name(name)
    {
        return ReadOutcome::Absent;
    }
    let file = match open_nofollow(expected) {
        Ok(file) => file,
        Err(error) if is_loop(&error) => return ReadOutcome::Symlink,
        Err(_) => return ReadOutcome::Absent,
    };
    let handle = match file.metadata() {
        Ok(handle) if handle.is_file() => handle,
        _ => return ReadOutcome::Absent,
    };
    let size = handle.len();
    let mtime = mtime_ms(expected);
    // Читать не больше лимита с запасом в байт: признак усечения виден
    // без загрузки огромного файла в память целиком.
    let mut capped = Vec::new();
    if file
        .take(MAX_OWNER_FILE_BYTES as u64 + 1)
        .read_to_end(&mut capped)
        .is_err()
    {
        return ReadOutcome::Absent;
    }
    if capped.contains(&0) {
        return ReadOutcome::Absent;
    }
    let truncated = capped.len() > MAX_OWNER_FILE_BYTES;
    capped.truncate(MAX_OWNER_FILE_BYTES);
    // Срез только по границе символа, иначе markdown покажет половину знака.
    while String::from_utf8(capped.clone()).is_err() && !capped.is_empty() {
        capped.pop();
    }
    let mut text = String::from_utf8_lossy(&capped).into_owned();
    if truncated {
        text.push_str(TRUNCATED_MARKER);
    }
    ReadOutcome::Text {
        text,
        size,
        mtime,
        truncated,
    }
}

/// Ожидаемый путь глобального файла без чтения.
fn expected_path(kind: &str, roots: &OwnerRoots) -> Option<PathBuf> {
    match kind {
        KIND_CLAUDE_USER => Some(roots.claude_config_dir.join("CLAUDE.md")),
        KIND_CLAUDE_HOME => Some(roots.home.join("CLAUDE.md")),
        KIND_CODEX_GLOBAL => Some(roots.codex_home.join("AGENTS.md")),
        KIND_CODEX_OVERRIDE => Some(roots.codex_home.join("AGENTS.override.md")),
        KIND_OPENCODE_GLOBAL => Some(roots.opencode_config_dir.join("AGENTS.md")),
        _ => None,
    }
}

/// База, внутри которой должен остаться канонический путь глобального файла.
fn global_base(kind: &str, roots: &OwnerRoots) -> Option<PathBuf> {
    match kind {
        KIND_CLAUDE_USER => Some(roots.claude_config_dir.clone()),
        KIND_CLAUDE_HOME => Some(roots.home.clone()),
        KIND_CODEX_GLOBAL | KIND_CODEX_OVERRIDE => Some(roots.codex_home.clone()),
        KIND_OPENCODE_GLOBAL => Some(roots.opencode_config_dir.clone()),
        _ => None,
    }
}

fn memory_dir(roots: &OwnerRoots, project: &str) -> Option<PathBuf> {
    if !valid_project(project) {
        return None;
    }
    Some(
        roots
            .claude_config_dir
            .join("projects")
            .join(project)
            .join("memory"),
    )
}

fn meta_for(kind: &str, roots: &OwnerRoots) -> OwnerFileMeta {
    let Some(expected) = expected_path(kind, roots) else {
        return OwnerFileMeta {
            kind: kind.into(),
            ..Default::default()
        };
    };
    let display = expected.to_string_lossy().into_owned();
    let absent = |reason: &str| OwnerFileMeta {
        kind: kind.into(),
        path: display.clone(),
        reason: reason.into(),
        ..Default::default()
    };
    // Ссылка вместо настоящего файла не показывается, но видна в обзоре
    // с причиной: молчаливое отсутствие скрыло бы подмену.
    if std::fs::symlink_metadata(&expected).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return absent(REASON_SYMLINK);
    }
    let base = global_base(kind, roots).unwrap_or_else(|| expected.clone());
    let base_canon = canonical_base(&base);
    let canonical = std::fs::canonicalize(&expected).unwrap_or(expected);
    if !inside(&canonical, &base_canon) {
        return absent("");
    }
    match std::fs::metadata(&canonical) {
        Ok(meta) if meta.is_file() => OwnerFileMeta {
            kind: kind.into(),
            path: display,
            present: true,
            size: meta.len(),
            mtime_ms: mtime_ms(&canonical),
            ..Default::default()
        },
        _ => absent(""),
    }
}

/// Обзор: метаданные пяти глобальных файлов и список проектов памяти.
pub fn overview(roots: &OwnerRoots) -> OwnerInstructionsOverview {
    let files = [
        KIND_CLAUDE_USER,
        KIND_CLAUDE_HOME,
        KIND_CODEX_GLOBAL,
        KIND_CODEX_OVERRIDE,
        KIND_OPENCODE_GLOBAL,
    ]
    .into_iter()
    .map(|kind| meta_for(kind, roots))
    .collect();
    OwnerInstructionsOverview {
        request_id: String::new(),
        files,
        projects: memory_projects(roots),
    }
}

fn memory_projects(roots: &OwnerRoots) -> Vec<OwnerMemoryProject> {
    let base = roots.claude_config_dir.join("projects");
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let base_canon = canonical_base(&base);
    let mut out = Vec::new();
    for entry in entries.flatten().take(MAX_MEMORY_PROJECTS * 2) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_project(&name) {
            continue;
        }
        let memory = base.join(&name).join("memory");
        let Ok(canon) = std::fs::canonicalize(&memory) else {
            continue;
        };
        if !inside(&canon, &base_canon) || !canon.is_dir() {
            continue;
        }
        let (files, has_index) = memory_stats(&canon);
        // Проект без памяти в обзор не попадает.
        if files == 0 && !has_index {
            continue;
        }
        out.push(OwnerMemoryProject {
            id: name,
            files,
            has_index,
        });
        if out.len() >= MAX_MEMORY_PROJECTS {
            break;
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn memory_stats(dir: &Path) -> (u64, bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, false);
    };
    let mut files = 0u64;
    let mut has_index = false;
    for entry in entries.flatten().take(MAX_MEMORY_FILES + 1) {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        // Настоящий файл памяти ссылкой не бывает: ссылки не считаются,
        // даже если цель осталась внутри каталога.
        if !kind.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_memory_name(&name) {
            continue;
        }
        let target = std::fs::canonicalize(entry.path()).unwrap_or_default();
        if !inside(&target, dir) || !target.is_file() {
            continue;
        }
        if name == "MEMORY.md" {
            has_index = true;
        }
        files += 1;
    }
    (files.min(MAX_MEMORY_FILES as u64), has_index)
}

fn memory_listing(dir: &Path) -> Vec<OwnerMemoryFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if out.len() >= MAX_MEMORY_FILES {
            break;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_memory_name(&name) {
            continue;
        }
        let target = std::fs::canonicalize(entry.path()).unwrap_or_default();
        if !inside(&target, dir) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&target) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        out.push(OwnerMemoryFile {
            name,
            size: meta.len(),
            mtime_ms: mtime_ms(&target),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.truncate(MAX_MEMORY_FILES);
    out
}

/// Один файл по виду запроса. Неизвестный вид и выход за каталог — отсутствие,
/// а не содержимое чужого пути. Ссылка вместо настоящего файла — отсутствие
/// с причиной `symlink`.
pub fn read_one(
    kind: &str,
    project: &str,
    name: &str,
    roots: &OwnerRoots,
    request_id: &str,
) -> OwnerInstructionsFile {
    fn absent(request_id: &str, kind: &str, path: String, reason: &str) -> OwnerInstructionsFile {
        OwnerInstructionsFile {
            request_id: request_id.into(),
            kind: kind.into(),
            path,
            reason: reason.into(),
            ..Default::default()
        }
    }
    fn refusal(reason: &ReadOutcome) -> &str {
        match reason {
            ReadOutcome::Symlink => REASON_SYMLINK,
            _ => "",
        }
    }
    if let Some(expected) = expected_path(kind, roots) {
        // Лишние project/name у глобального файла запрос портят.
        if !project.is_empty() || !name.is_empty() {
            return OwnerInstructionsFile {
                request_id: request_id.into(),
                kind: kind.into(),
                path: expected.to_string_lossy().into_owned(),
                ..Default::default()
            };
        }
        let base = global_base(kind, roots).unwrap_or_else(|| expected.clone());
        let base_canon = canonical_base(&base);
        let display = expected.to_string_lossy().into_owned();
        match read_text(&expected, &base_canon) {
            ReadOutcome::Text {
                text: content,
                size,
                mtime,
                truncated,
            } => {
                // Время берётся из системных часов машины; хаб его не пересчитывает.
                return OwnerInstructionsFile {
                    request_id: request_id.into(),
                    kind: kind.into(),
                    path: display,
                    present: true,
                    size,
                    mtime_ms: mtime,
                    truncated,
                    content,
                    ..Default::default()
                };
            }
            outcome => return absent(request_id, kind, display, refusal(&outcome)),
        }
    }
    if kind == KIND_MEMORY_INDEX || kind == KIND_MEMORY_FILE {
        let Some(dir) = memory_dir(roots, project) else {
            return OwnerInstructionsFile {
                request_id: request_id.into(),
                kind: kind.into(),
                ..Default::default()
            };
        };
        if name.contains('\0') {
            return OwnerInstructionsFile {
                request_id: request_id.into(),
                kind: kind.into(),
                ..Default::default()
            };
        }
        let projects_base = canonical_base(&roots.claude_config_dir.join("projects"));
        let Ok(dir_canon) = std::fs::canonicalize(&dir) else {
            let leaf = if kind == KIND_MEMORY_INDEX {
                "MEMORY.md"
            } else {
                name
            };
            return OwnerInstructionsFile {
                request_id: request_id.into(),
                kind: kind.into(),
                path: dir.join(leaf).to_string_lossy().into_owned(),
                ..Default::default()
            };
        };
        if !inside(&dir_canon, &projects_base) {
            return OwnerInstructionsFile {
                request_id: request_id.into(),
                kind: kind.into(),
                ..Default::default()
            };
        }
        if kind == KIND_MEMORY_INDEX {
            let expected = dir.join("MEMORY.md");
            let display = expected.to_string_lossy().into_owned();
            let listing = memory_listing(&dir_canon);
            match read_text(&expected, &dir_canon) {
                ReadOutcome::Text {
                    text: content,
                    size,
                    mtime,
                    truncated,
                } => {
                    return OwnerInstructionsFile {
                        request_id: request_id.into(),
                        kind: kind.into(),
                        path: display,
                        present: true,
                        size,
                        mtime_ms: mtime,
                        truncated,
                        content,
                        files: listing,
                        ..Default::default()
                    };
                }
                outcome => {
                    return OwnerInstructionsFile {
                        request_id: request_id.into(),
                        kind: kind.into(),
                        path: display,
                        files: listing,
                        reason: refusal(&outcome).into(),
                        ..Default::default()
                    };
                }
            }
        }
        if !valid_memory_name(name) {
            return OwnerInstructionsFile {
                request_id: request_id.into(),
                kind: kind.into(),
                ..Default::default()
            };
        }
        let expected = dir.join(name);
        let display = expected.to_string_lossy().into_owned();
        match read_text(&expected, &dir_canon) {
            ReadOutcome::Text {
                text: content,
                size,
                mtime,
                truncated,
            } => {
                return OwnerInstructionsFile {
                    request_id: request_id.into(),
                    kind: kind.into(),
                    path: display,
                    present: true,
                    size,
                    mtime_ms: mtime,
                    truncated,
                    content,
                    ..Default::default()
                };
            }
            outcome => return absent(request_id, kind, display, refusal(&outcome)),
        }
    }
    OwnerInstructionsFile {
        request_id: request_id.into(),
        kind: kind.into(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn roots(dir: &Path) -> OwnerRoots {
        let home = dir.join("home");
        OwnerRoots::new(
            home.clone(),
            home.join(".claude"),
            home.join(".codex"),
            home.join(".config/opencode"),
        )
    }

    fn setup(dir: &Path) -> OwnerRoots {
        let r = roots(dir);
        std::fs::create_dir_all(r.claude_config_dir.join("projects/proj1/memory")).unwrap();
        std::fs::create_dir_all(&r.codex_home).unwrap();
        std::fs::create_dir_all(&r.opencode_config_dir).unwrap();
        std::fs::write(r.claude_config_dir.join("CLAUDE.md"), "# user").unwrap();
        std::fs::write(r.home.join("CLAUDE.md"), "# home").unwrap();
        std::fs::write(r.codex_home.join("AGENTS.md"), "# codex").unwrap();
        std::fs::write(r.opencode_config_dir.join("AGENTS.md"), "# opencode").unwrap();
        std::fs::write(
            r.claude_config_dir.join("projects/proj1/memory/MEMORY.md"),
            "- topic in a.md",
        )
        .unwrap();
        std::fs::write(
            r.claude_config_dir.join("projects/proj1/memory/a.md"),
            "# a",
        )
        .unwrap();
        // Приманки: читать их через виды нельзя.
        std::fs::write(r.claude_config_dir.join(".credentials.json"), "secret").unwrap();
        std::fs::write(r.codex_home.join("auth.json"), "secret").unwrap();
        std::fs::write(r.home.join(".env"), "secret").unwrap();
        r
    }

    #[test]
    fn global_files_read_from_the_allowlist() {
        let dir = tempfile::tempdir().unwrap();
        let r = setup(dir.path());
        for (kind, marker) in [
            (KIND_CLAUDE_USER, "# user"),
            (KIND_CLAUDE_HOME, "# home"),
            (KIND_CODEX_GLOBAL, "# codex"),
            (KIND_OPENCODE_GLOBAL, "# opencode"),
        ] {
            let file = read_one(kind, "", "", &r, "req");
            assert!(file.present, "{kind}");
            assert!(file.content.contains(marker), "{kind}");
            assert!(!file.truncated);
        }
        let missing = read_one(KIND_CODEX_OVERRIDE, "", "", &r, "req");
        assert!(!missing.present);
        let view = overview(&r);
        assert_eq!(view.files.len(), 5);
        assert_eq!(view.projects.len(), 1);
        assert_eq!(view.projects[0].id, "proj1");
    }

    #[test]
    fn memory_index_and_file_use_ids_from_the_overview() {
        let dir = tempfile::tempdir().unwrap();
        let r = setup(dir.path());
        let index = read_one(KIND_MEMORY_INDEX, "proj1", "", &r, "req");
        assert!(index.present);
        assert!(index.content.contains("topic"));
        assert!(index.files.iter().any(|f| f.name == "MEMORY.md"));
        assert!(index.files.iter().any(|f| f.name == "a.md"));
        let file = read_one(KIND_MEMORY_FILE, "proj1", "a.md", &r, "req");
        assert!(file.present);
        assert_eq!(file.content, "# a");
    }

    #[test]
    fn credentials_env_and_outside_paths_are_not_readable() {
        let dir = tempfile::tempdir().unwrap();
        let r = setup(dir.path());
        // Неизвестный вид: пустой ответ, а не чужой файл.
        for (kind, project, name) in [
            ("credentials", "", ""),
            ("codex_auth", "", ""),
            ("env", "", ""),
            ("memory_file", "../..", "a.md"),
            ("memory_file", "proj1", "../../.env"),
            ("memory_file", "proj1", ".env"),
            ("memory_file", "proj1", "auth.json"),
            ("memory_file", "proj1", "../proj1/memory/a.md"),
            ("claude_user", "proj1", "a.md"),
            ("", "", ""),
        ] {
            let file = read_one(kind, project, name, &r, "req");
            assert!(!file.present, "{kind}/{project}/{name}");
            assert!(file.content.is_empty(), "{kind}/{project}/{name}");
        }
        // Прямое чтение приманок через виды невозможно: их имён в списке нет.
        let view = overview(&r);
        assert!(
            view.files
                .iter()
                .all(|f| !f.path.ends_with(".credentials.json") && !f.path.ends_with("auth.json"))
        );
    }

    #[test]
    fn symlink_in_memory_that_leaves_the_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let r = setup(dir.path());
        let memory = r.claude_config_dir.join("projects/proj1/memory");
        symlink(
            r.claude_config_dir.join(".credentials.json"),
            memory.join("evil.md"),
        )
        .unwrap();
        symlink("/etc/hostname", memory.join("outer.md")).unwrap();
        let evil = read_one(KIND_MEMORY_FILE, "proj1", "evil.md", &r, "req");
        assert!(!evil.present);
        assert!(evil.content.is_empty());
        let outer = read_one(KIND_MEMORY_FILE, "proj1", "outer.md", &r, "req");
        assert!(!outer.present);
        let index = read_one(KIND_MEMORY_INDEX, "proj1", "", &r, "req");
        assert!(
            index
                .files
                .iter()
                .all(|f| f.name != "evil.md" && f.name != "outer.md"),
            "ссылки наружу не попадают даже в список"
        );
        let view = overview(&r);
        let proj = view.projects.iter().find(|p| p.id == "proj1").unwrap();
        assert_eq!(proj.files, 2);
    }

    #[test]
    fn large_file_is_truncated_with_a_marker() {
        let dir = tempfile::tempdir().unwrap();
        let r = setup(dir.path());
        let big = "x".repeat(MAX_OWNER_FILE_BYTES + 100);
        std::fs::write(r.codex_home.join("AGENTS.md"), &big).unwrap();
        let file = read_one(KIND_CODEX_GLOBAL, "", "", &r, "req");
        assert!(file.present);
        assert!(file.truncated);
        assert!(file.content.ends_with(TRUNCATED_MARKER));
        assert_eq!(file.size as usize, big.len());
    }

    #[test]
    fn global_symlink_to_another_home_file_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let r = setup(dir.path());
        // Приманка без NUL-байта: имя не из запретного списка, внутри дома.
        std::fs::create_dir_all(r.home.join(".ssh")).unwrap();
        std::fs::write(r.home.join(".ssh/id_ed25519"), "secret-key-material").unwrap();
        std::fs::remove_file(r.home.join("CLAUDE.md")).unwrap();
        symlink(r.home.join(".ssh/id_ed25519"), r.home.join("CLAUDE.md")).unwrap();
        let file = read_one(KIND_CLAUDE_HOME, "", "", &r, "req");
        assert!(!file.present);
        assert!(file.content.is_empty());
        assert_eq!(file.reason, "symlink");
        let view = overview(&r);
        let meta = view
            .files
            .iter()
            .find(|f| f.kind == KIND_CLAUDE_HOME)
            .unwrap();
        assert!(!meta.present);
        assert_eq!(meta.reason, "symlink");
    }

    #[test]
    fn global_symlink_inside_config_dir_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let r = setup(dir.path());
        std::fs::write(r.claude_config_dir.join("other.md"), "чужой текст").unwrap();
        std::fs::remove_file(r.claude_config_dir.join("CLAUDE.md")).unwrap();
        symlink(
            r.claude_config_dir.join("other.md"),
            r.claude_config_dir.join("CLAUDE.md"),
        )
        .unwrap();
        let file = read_one(KIND_CLAUDE_USER, "", "", &r, "req");
        assert!(!file.present);
        assert!(file.content.is_empty());
        assert_eq!(file.reason, "symlink");
    }

    #[test]
    fn symlinked_memory_file_is_neither_listed_nor_read() {
        let dir = tempfile::tempdir().unwrap();
        let r = setup(dir.path());
        let memory = r.claude_config_dir.join("projects/proj1/memory");
        // Цель внутри каталога памяти: всё равно ссылка, настоящие файлы такими не бывают.
        symlink(memory.join("a.md"), memory.join("linked.md")).unwrap();
        let linked = read_one(KIND_MEMORY_FILE, "proj1", "linked.md", &r, "req");
        assert!(!linked.present);
        assert!(linked.content.is_empty());
        let index = read_one(KIND_MEMORY_INDEX, "proj1", "", &r, "req");
        assert!(
            index.files.iter().all(|f| f.name != "linked.md"),
            "ссылка не попадает даже в список"
        );
        let view = overview(&r);
        let proj = view.projects.iter().find(|p| p.id == "proj1").unwrap();
        assert_eq!(proj.files, 2);
    }
}
