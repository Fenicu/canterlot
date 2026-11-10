//! Чтение файла, который агент отдаёт владельцу. Путь проверяется один раз:
//! открытие с `O_NOFOLLOW` и `O_NONBLOCK`, дальше только дескриптор (`fstat`):
//! обычный файл, одна жёсткая ссылка, владелец-процесс. Секретные имена общие
//! с инструкциями владельца; каталог `.ssh` и файлы больше 50 МиБ запрещены.

use std::{
    collections::VecDeque,
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Write},
    os::{
        fd::OwnedFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        },
    },
    path::{Component, Path, PathBuf},
};

use canterlot_proto::limits::MAX_SHARE_BYTES;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{nofollow, owner_instructions::denied_name};
use crate::media;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareError {
    Symlink,
    HardLink,
    NotRegular,
    Denied,
    TooLarge,
    Unreadable,
    InvalidText,
    NotOwned,
    OutsideWorkdir,
    Missing,
}

impl ShareError {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Symlink => "share_symlink",
            Self::HardLink => "share_hard_link",
            Self::NotRegular => "share_not_regular",
            Self::Denied => "share_denied",
            Self::TooLarge => "share_too_large",
            Self::Unreadable => "share_unreadable",
            Self::InvalidText => "invalid_arguments",
            Self::NotOwned => "share_not_owned",
            Self::OutsideWorkdir => "share_outside_workdir",
            Self::Missing => "share_missing",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Symlink => "The file is a symbolic link",
            Self::HardLink => "The file has more than one hard link",
            Self::NotRegular => "Only a regular file can be shared",
            Self::Denied => "This file is on the secret deny list",
            Self::TooLarge => "The file is larger than 50 MiB",
            Self::Unreadable => "The file cannot be read",
            Self::InvalidText => "The title or note is too long or contains control characters",
            Self::NotOwned => "The file belongs to another user",
            Self::OutsideWorkdir => {
                "Only files under the session workdir or the temp directory can be shared; copy the file there first"
            }
            Self::Missing => "The file does not exist",
        }
    }

    /// Короткий код для пробы пути: наружу хаб отдаёт только его, не английский текст.
    pub fn probe_reason(self) -> &'static str {
        match self {
            Self::Symlink => "symlink",
            Self::HardLink => "hard_link",
            Self::NotRegular => "not_regular",
            Self::Denied => "denied",
            Self::TooLarge => "too_large",
            Self::Unreadable => "unreadable",
            Self::InvalidText => "invalid_text",
            Self::NotOwned => "not_owned",
            Self::OutsideWorkdir => "outside_roots",
            Self::Missing => "missing",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub id: Uuid,
    pub name: String,
    pub title: String,
    pub note: String,
    pub mime: String,
    pub size: u64,
    pub sha256: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Секретные каталоги: путь под любым из них не отдаётся, даже через ссылки.
fn denied_dir(part: &str) -> bool {
    matches!(
        part,
        ".ssh"
            | ".gnupg"
            | ".claude"
            | ".codex"
            | ".gemini"
            | ".cursor"
            | ".codexbar"
            | ".docker"
            | ".kube"
            | ".aws"
            | ".azure"
            | ".password-store"
            | ".terraform.d"
            | ".mozilla"
    )
}

/// Секретные каталоги внутри `.config`.
fn denied_config_dir(part: &str) -> bool {
    matches!(
        part,
        "opencode" | "canterlot" | "gh" | "rclone" | "codexbar" | "forgejo" | "gcloud" | "glab-cli"
    )
}

/// Имя без хвоста `.txt`/`.json`: эти расширения не меняют смысл имени токена.
fn token_stem(lower: &str) -> &str {
    lower
        .strip_suffix(".txt")
        .or_else(|| lower.strip_suffix(".json"))
        .unwrap_or(lower)
}

/// Настоящие хранилища токенов, а не всякое имя со словом `token`:
/// `tokens-report.md` и `tokenizer.rs` остаются обычными файлами.
fn denied_token_name(lower: &str) -> bool {
    let stem = token_stem(lower);
    // Имена самих хранилищ: `token`/`tokens` в любом из трёх видов. Хвост
    // `.txt`/`.json` уже снят, поэтому `token.txt` и `tokens.json` тоже здесь.
    matches!(stem, "token" | "tokens" | ".token")
        || lower.ends_with(".token")
        || stem.ends_with("_token")
        || stem.ends_with("-token")
        || lower.starts_with("access_token")
        || lower.starts_with("refresh_token")
        || lower.starts_with("api_token")
        || lower.starts_with("api-key")
        || lower.starts_with("apikey")
        || lower.ends_with(".secret")
        || lower.starts_with("secrets.")
}

/// Закрытые ключи и токены: имена действуют в любом каталоге, вне `.ssh` тоже.
fn denied_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "id_rsa"
            | "id_ed25519"
            | "id_ecdsa"
            | "id_dsa"
            | "credentials.json"
            | "auth.json"
            | ".credentials.json"
            | ".codexbar"
    ) || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || denied_token_name(&lower)
}

/// Имена ключей SSH: узкий набор префиксов, а не любое `id_*`, чтобы обычные
/// `id_mapping.json` и `id_list.csv` оставались обычными файлами.
const SSH_KEY_PREFIXES: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", "id_xmss"];

fn denied_ssh_key_name(lower: &str) -> bool {
    !lower.ends_with(".pub")
        && SSH_KEY_PREFIXES
            .iter()
            .any(|prefix| lower.starts_with(prefix))
}

/// Файлы с токенами и паролями инструментов, где бы они ни лежали. Шаблоны имён
/// действуют только на имя самого файла: каталог `token` или `api-keys` ничего не
/// запрещает.
fn denied_share_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    denied_secret_name(name)
        || denied_env_name(&lower)
        || matches!(
            lower.as_str(),
            ".git-credentials"
                | ".netrc"
                | ".npmrc"
                | ".pypirc"
                | ".pgpass"
                | ".bash_history"
                | ".zsh_history"
                | "fish_history"
                | ".python_history"
                | ".psql_history"
                | ".mysql_history"
                | "secret"
                | "secrets"
                | "credentials"
                | ".dockercfg"
                | ".htpasswd"
                | ".my.cnf"
                | "kubeconfig"
        )
        || lower.starts_with(".claude.json")
        || denied_ssh_key_name(&lower)
        || lower.ends_with(".ppk")
        || lower.starts_with("secret.")
        || lower.starts_with("client_secret")
        || lower.starts_with("credentials.")
        || (lower.starts_with("service-account") && lower.ends_with(".json"))
        || (lower.starts_with("service_account") && lower.ends_with(".json"))
        || lower.ends_with(".kubeconfig")
        || lower.ends_with(".tfstate")
        || lower.ends_with(".tfstate.backup")
        || lower.ends_with(".tfvars")
        || lower.ends_with(".jks")
        || lower.ends_with(".keystore")
        || lower.ends_with(".kdbx")
        || lower.ends_with(".sqlite3")
        || lower.ends_with(".db")
        || lower.starts_with("krb5cc_")
}

/// Файлы окружения: `.env`, `.env.*`, `*.env`, `.envrc`; шаблоны отдаются.
fn denied_env_name(lower: &str) -> bool {
    if matches!(
        lower,
        ".env.example" | ".env.sample" | ".env.template" | ".env.dist"
    ) {
        return false;
    }
    lower == ".env" || lower.starts_with(".env.") || lower.ends_with(".env") || lower == ".envrc"
}

/// Запрет по каталогам в любом компоненте пути: секретные каталоги и псевдофайловые
/// системы. Имена файлов здесь не проверяются — компонент может быть каталогом.
fn denied_directory_parts(parts: &[&str]) -> bool {
    parts.iter().any(|part| denied_dir(part))
        || parts
            .windows(2)
            .any(|pair| pair[0] == ".config" && denied_config_dir(pair[1]))
        || parts
            .windows(3)
            .any(|triple| triple == [".local", "share", "keyrings"])
}

/// Разрешённый путь после раскрытия ссылок в родителях: секретные каталоги
/// (`~/.ssh`, `~/.gnupg`, `~/.claude`, `~/.codex`, `~/.gemini`, `~/.docker`, `~/.kube`, `~/.aws`,
/// `~/.azure`, `~/.password-store`, `~/.terraform.d`, `~/.mozilla`,
/// `~/.config/{opencode,canterlot,gh,rclone,gcloud,glab-cli}`, `~/.local/share/keyrings`),
/// секретные имена последнего компонента и любой `.env` не отдаются.
fn denied_parts(parts: &[&str]) -> bool {
    denied_directory_parts(parts)
        || parts
            .last()
            .is_some_and(|name| denied_name(name) || denied_share_name(name))
}

/// Псевдофайловые системы: `/proc/<pid>/environ` проходит все проверки дескриптора.
fn denied_system(resolved: &Path) -> bool {
    ["/proc", "/sys", "/dev"]
        .iter()
        .any(|root| resolved.starts_with(root))
}

/// Временные каталоги, из которых можно отдавать файлы помимо рабочего каталога сессии.
fn temp_roots() -> Vec<std::path::PathBuf> {
    let mut roots = vec![
        std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from("/var/tmp"),
        std::env::temp_dir(),
    ];
    roots.retain(|root| root.is_absolute());
    roots
}

/// Раскрытый путь лежит под одним из разрешённых корней.
fn inside_roots(resolved: &Path, roots: &[std::path::PathBuf]) -> bool {
    roots
        .iter()
        .filter_map(|root| fs::canonicalize(root).ok())
        .any(|root| resolved.starts_with(root))
}

/// Корни `share_file`: рабочий каталог сессии и временные каталоги.
fn share_roots(workdir: Option<&Path>) -> Vec<std::path::PathBuf> {
    workdir
        .map(Path::to_path_buf)
        .into_iter()
        .chain(temp_roots())
        .collect()
}

/// Путь как он задан: `.` и `..` нормализуются лексически, ссылки не раскрываются.
/// Иначе `~/.ssh -> /srv/private-keys` прячет секретное имя от проверки.
fn denied_lexical(path: &Path) -> bool {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                if let Some(part) = part.to_str() {
                    parts.push(part);
                }
            }
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    denied_parts(&parts)
}

fn valid_title(title: &str) -> bool {
    title.len() <= canterlot_proto::limits::MAX_SHARE_TITLE_BYTES
        && !title.chars().any(char::is_control)
}

fn name_of(path: &Path) -> Result<String, ShareError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 255
                && !name.chars().any(char::is_control)
                && *name != "."
                && *name != ".."
        })
        .ok_or(ShareError::Unreadable)?;
    Ok(name.to_owned())
}

fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Тип по сигнатуре, затем по расширению; неизвестное отдаётся как поток байтов.
/// Пустой файл не бывает картинкой или видео: он уходит обычным файлом, а не плеером.
pub fn mime_for(name: &str, head: &[u8]) -> String {
    let mime = mime_by_content(name, head);
    if head.is_empty() && (mime.starts_with("image/") || mime.starts_with("video/")) {
        return "application/octet-stream".into();
    }
    mime
}

fn mime_by_content(name: &str, head: &[u8]) -> String {
    if let Some(image) = media::sniff(head) {
        return image.to_owned();
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        return "video/mp4".into();
    }
    if head.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return "video/webm".into();
    }
    match extension(name).as_str() {
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
    .into()
}

/// Проверенный файл: родители раскрыты, финал открыт от дескриптора без ссылок.
pub(crate) struct Checked {
    pub file: fs::File,
    pub meta: fs::Metadata,
    pub name: String,
}

/// Ссылка или пропавший путь в раскрытии родителя: первое — отказ, второе — «нет файла».
fn resolve_error(error: std::io::Error) -> ShareError {
    match error.raw_os_error() {
        Some(code) if code == rustix::io::Errno::NOENT.raw_os_error() => ShareError::Missing,
        Some(code) if code == rustix::io::Errno::NOTDIR.raw_os_error() => ShareError::Missing,
        Some(code) if code == rustix::io::Errno::LOOP.raw_os_error() => ShareError::Symlink,
        _ => ShareError::Unreadable,
    }
}

fn open_error(error: rustix::io::Errno) -> ShareError {
    if nofollow::is_link_error(error) {
        ShareError::Symlink
    } else if error == rustix::io::Errno::NOENT {
        ShareError::Missing
    } else {
        ShareError::Unreadable
    }
}

/// Лексически нормализованные компоненты абсолютного пути: `.` и `..` не остаются.
/// Выход выше корня файловой системы — отказ.
fn lexical_parts(path: &Path) -> Option<Vec<OsString>> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => parts.clear(),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::Normal(part) => parts.push(part.to_os_string()),
            Component::Prefix(_) => return None,
        }
    }
    path.is_absolute().then_some(parts)
}

/// Самый длинный корень, под которым путь лежит лексически: `(корень, остаток)`.
fn lexical_root(path: &Path, roots: &[std::path::PathBuf]) -> Option<(PathBuf, PathBuf)> {
    let parts = lexical_parts(path)?;
    let (root, depth) = root_prefix(&parts, roots)?;
    let rest: PathBuf = parts[depth..].iter().collect();
    Some((root, rest))
}

fn root_prefix(parts: &[OsString], roots: &[std::path::PathBuf]) -> Option<(PathBuf, usize)> {
    let mut best: Option<(PathBuf, usize)> = None;
    for root in roots {
        let Some(root_parts) = lexical_parts(root) else {
            continue;
        };
        let depth = root_parts.len();
        if parts.len() >= depth
            && parts[..depth] == root_parts[..]
            && best.as_ref().is_none_or(|(_, found)| depth > *found)
        {
            best = Some((root.clone(), depth));
        }
    }
    best
}

/// Раскрытые компоненты пути как строки: политика запретов работает с именами.
fn resolved_names(path: &Path) -> Result<Vec<String>, ShareError> {
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => {
                names.push(part.to_str().ok_or(ShareError::Unreadable)?.to_owned())
            }
            Component::ParentDir | Component::Prefix(_) => return Err(ShareError::Unreadable),
        }
    }
    Ok(names)
}

fn path_of_names(names: &[String]) -> PathBuf {
    let mut path = PathBuf::from("/");
    path.extend(names);
    path
}

/// Запрет по каталогам в раскрытом префиксе: секретные каталоги и псевдофайловые
/// системы. Имена файлов здесь не проверяются — префикс может быть каталогом.
fn refuse_directories(names: &[String]) -> Result<(), ShareError> {
    let parts: Vec<&str> = names.iter().map(String::as_str).collect();
    if denied_directory_parts(&parts) || denied_system(&path_of_names(names)) {
        return Err(ShareError::Denied);
    }
    Ok(())
}

/// Имя самого файла: шаблоны токенов, ключей и секретов действуют только на
/// последний компонент пути.
fn refuse_file_name(name: &str) -> Result<(), ShareError> {
    if denied_name(name) || denied_share_name(name) {
        return Err(ShareError::Denied);
    }
    Ok(())
}

fn open_component(
    dir: &OwnedFd,
    name: &str,
) -> Result<(OwnedFd, rustix::fs::FileType), ShareError> {
    let fd = rustix::fs::openat(
        dir,
        name,
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| match error {
        rustix::io::Errno::NOENT => ShareError::Missing,
        rustix::io::Errno::NOTDIR => ShareError::Missing,
        rustix::io::Errno::ACCESS => ShareError::Unreadable,
        _ => ShareError::Unreadable,
    })?;
    let kind = rustix::fs::FileType::from_raw_mode(
        rustix::fs::fstat(&fd)
            .map_err(|_| ShareError::Unreadable)?
            .st_mode,
    );
    Ok((fd, kind))
}

/// Проверить запреты по именам компонентов, которые ещё не раскрыты: для
/// абсолютной цели ссылки — до `canonicalize` и до первого перехода.
fn refuse_component_names(parts: &[OsString]) -> Result<(), ShareError> {
    let mut names: Vec<&str> = Vec::with_capacity(parts.len());
    for part in parts {
        match part.to_str() {
            Some(part) => names.push(part),
            // Не-UTF-8 имя раскроется позже и даст `Unreadable`; сейчас скрывать нечего.
            None => return Ok(()),
        }
    }
    if denied_directory_parts(&names) {
        return Err(ShareError::Denied);
    }
    Ok(())
}

/// Раскрыть ссылку в родителе: ссылка не должна увести из разрешённых корней
/// и не должна спрятать секретный каталог. Возвращает новые компоненты для обхода.
///
/// Каждое имя из цели проверяется лексически в тот момент, когда оно вставляется
/// в обход, — до открытия или `stat` этого компонента. Поэтому запрет не зависит
/// от того, существует ли он, и не даёт оракула существования.
fn expand_link(
    target: &Path,
    names: &mut Vec<String>,
    stack: &mut Vec<OwnedFd>,
    root_depth: &mut usize,
    roots: &[std::path::PathBuf],
    outside: ShareError,
) -> Result<Vec<OsString>, ShareError> {
    if target.is_absolute() {
        let parts = lexical_parts(target).ok_or(outside)?;
        // Запрет по именам — до раскрытия корня и до перехода по цели.
        refuse_component_names(&parts)?;
        let (root, depth) = root_prefix(&parts, roots).ok_or(outside)?;
        let canonical = fs::canonicalize(&root).map_err(resolve_error)?;
        let root_names = resolved_names(&canonical)?;
        refuse_directories(&root_names)?;
        let rest = &parts[depth..];
        let mut projected = root_names.clone();
        for part in rest {
            projected.push(part.to_str().ok_or(ShareError::Unreadable)?.to_owned());
            refuse_directories(&projected)?;
        }
        *root_depth = root_names.len();
        *names = root_names;
        *stack = vec![nofollow::open_dir(&canonical).map_err(open_error)?];
        Ok(rest.to_vec())
    } else {
        // Ссылка относительна каталога, в котором лежит: `..` поднимается не выше корня.
        let mut depth = names.len();
        let mut pending: Vec<OsString> = Vec::new();
        for component in target.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(part) => {
                    let name = part.to_str().ok_or(ShareError::Unreadable)?;
                    // Запрет проверяется на спроецированном пути: уже пройденные
                    // каталоги + ещё не открытые компоненты этой же цели + имя.
                    let mut projected: Vec<String> = names[..depth].to_vec();
                    for pending_part in &pending {
                        projected.push(
                            pending_part
                                .to_str()
                                .ok_or(ShareError::Unreadable)?
                                .to_owned(),
                        );
                    }
                    projected.push(name.to_owned());
                    refuse_directories(&projected)?;
                    pending.push(part.to_os_string());
                }
                Component::ParentDir => {
                    if pending.pop().is_none() {
                        if depth <= *root_depth {
                            return Err(outside);
                        }
                        depth -= 1;
                    }
                }
                Component::RootDir | Component::Prefix(_) => return Err(outside),
            }
        }
        names.truncate(depth);
        stack.truncate(depth - *root_depth + 1);
        Ok(pending)
    }
}

/// Обход пути от дескриптора разрешённого корня по одному компоненту. Имя каждого
/// компонента проверяется до `open`/`stat`, поэтому запрет не выдаёт существование;
/// `..` в ссылке не поднимается выше корня.
fn open_beneath(
    path: &Path,
    roots: &[std::path::PathBuf],
    outside: ShareError,
) -> Result<(fs::File, String), ShareError> {
    let name = name_of(path)?;
    let (root, rest) = lexical_root(path, roots).ok_or(outside)?;
    let canonical = fs::canonicalize(&root).map_err(resolve_error)?;
    let mut names = resolved_names(&canonical)?;
    let mut root_depth = names.len();
    refuse_directories(&names)?;
    let mut stack = vec![nofollow::open_dir(&canonical).map_err(open_error)?];
    let mut pending: VecDeque<OsString> = rest
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_os_string()),
            _ => None,
        })
        .collect();
    let mut followed = 0usize;
    loop {
        let Some(component) = pending.pop_front() else {
            // Путь называет сам корень: это каталог, а не файл.
            return Err(ShareError::NotRegular);
        };
        let part = component.to_str().ok_or(ShareError::Unreadable)?;
        // Запрет решается по именам, до обращения к файловой системе: иначе
        // `alias -> .cursor` отвечал бы `Missing` при отсутствующем `.cursor`.
        let mut projected = names.clone();
        projected.push(part.to_owned());
        refuse_directories(&projected)?;
        if pending.is_empty() {
            // Имя файла проверяется только на последнем компоненте пути.
            refuse_file_name(part)?;
            if !inside_roots(&path_of_names(&projected), roots) {
                return Err(outside);
            }
            let file =
                nofollow::open_file(stack.last().expect("root fd"), part).map_err(open_error)?;
            return Ok((file, name));
        }
        let (next, kind) = open_component(stack.last().expect("root fd"), part)?;
        match kind {
            rustix::fs::FileType::Directory => {
                names.push(part.to_owned());
                stack.push(next);
            }
            rustix::fs::FileType::Symlink => {
                followed += 1;
                if followed > 40 {
                    return Err(ShareError::Symlink);
                }
                // Цель читается через уже открытый `O_PATH|O_NOFOLLOW` дескриптор
                // самой ссылки, а не по изменяемому имени: подмена записи после
                // `open` не меняет inode, из которого читается цель. Гонка остаётся
                // только для подмены имени до `open`: тогда откроется другой объект
                // (каталог или файл), но имя уже проверено по запретам, а новая цель
                // снова раскрывается по именам — секрет или выход из корня так не
                // прочитать.
                let target = rustix::fs::readlinkat(&next, "", Vec::new())
                    .map_err(|_| ShareError::Unreadable)?;
                let target = Path::new(OsStr::from_bytes(target.as_bytes()));
                let inserted = expand_link(
                    target,
                    &mut names,
                    &mut stack,
                    &mut root_depth,
                    roots,
                    outside,
                )?;
                for inserted in inserted.into_iter().rev() {
                    pending.push_front(inserted);
                }
            }
            _ => return Err(ShareError::Missing),
        }
    }
}

/// Общая защита `share_file` и пробы пути: путь как задан и при раскрытии не
/// секретный, не в `/proc`, `/sys`, `/dev`, лежит под одним из `roots`, финал —
/// обычный файл этого пользователя с одной жёсткой ссылкой. Лексические запреты
/// срабатывают до первого обращения к файловой системе: ответ не зависит от того,
/// существует ли запретный путь.
pub(crate) fn open_checked(
    path: &Path,
    roots: &[std::path::PathBuf],
    outside: ShareError,
) -> Result<Checked, ShareError> {
    if denied_lexical(path) {
        return Err(ShareError::Denied);
    }
    if lexical_root(path, roots).is_none() {
        return Err(outside);
    }
    let (file, name) = open_beneath(path, roots, outside)?;
    let meta = file.metadata().map_err(|_| ShareError::Unreadable)?;
    if meta.file_type().is_symlink() {
        return Err(ShareError::Symlink);
    }
    if !meta.is_file() {
        return Err(ShareError::NotRegular);
    }
    if meta.nlink() != 1 {
        return Err(ShareError::HardLink);
    }
    if meta.uid() != rustix::process::getuid().as_raw() {
        return Err(ShareError::NotOwned);
    }
    Ok(Checked { file, meta, name })
}

/// Скопировать проверенный файл в `exec_dir/media/<id>` и посчитать SHA-256.
/// Отдаются только файлы под рабочим каталогом сессии `workdir` и под временными каталогами.
pub fn prepare(
    exec_dir: &Path,
    id: Uuid,
    path: &Path,
    title: String,
    note: String,
    workdir: Option<&Path>,
) -> Result<Prepared, ShareError> {
    if !valid_title(&title) || note.len() > canterlot_proto::limits::MAX_SHARE_NOTE_BYTES {
        return Err(ShareError::InvalidText);
    }
    let roots = share_roots(workdir);
    let Checked { file, meta, name } = open_checked(path, &roots, ShareError::OutsideWorkdir)?;
    if meta.len() > MAX_SHARE_BYTES {
        return Err(ShareError::TooLarge);
    }
    let directory = exec_dir.join(media::MEDIA_DIR);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .map_err(|_| ShareError::Unreadable)?;
    let temporary = directory.join(format!(".{id}.share"));
    let result = (|| {
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| ShareError::Unreadable)?;
        let mut reader = file.take(MAX_SHARE_BYTES + 1);
        let mut hash = Sha256::new();
        let mut head = Vec::with_capacity(512);
        let mut buffer = vec![0; 128 * 1024];
        let mut size = 0u64;
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|_| ShareError::Unreadable)?;
            if read == 0 {
                break;
            }
            size += read as u64;
            if size > MAX_SHARE_BYTES {
                return Err(ShareError::TooLarge);
            }
            hash.update(&buffer[..read]);
            if head.len() < 512 {
                let take = read.min(512 - head.len());
                head.extend_from_slice(&buffer[..take]);
            }
            out.write_all(&buffer[..read])
                .map_err(|_| ShareError::Unreadable)?;
        }
        out.sync_all().map_err(|_| ShareError::Unreadable)?;
        fs::rename(&temporary, media::media_path(exec_dir, id))
            .map_err(|_| ShareError::Unreadable)?;
        let mime = mime_for(&name, &head);
        let (width, height) =
            media::dimensions(&mime, &head).map_or((None, None), |(w, h)| (Some(w), Some(h)));
        Ok(Prepared {
            id,
            name,
            title,
            note,
            mime,
            size,
            sha256: hash
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            width,
            height,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(media::media_path(exec_dir, id));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Файлы тестов лежат во временном каталоге: рабочий каталог сессии не нужен.
    fn prepare_tmp(
        exec_dir: &Path,
        id: Uuid,
        path: &Path,
        title: String,
        note: String,
    ) -> Result<Prepared, ShareError> {
        prepare(exec_dir, id, path, title, note, None)
    }

    /// Каталог вне временных: рядом с исходниками крейта, удаляется при сбросе.
    fn outside_tmp() -> tempfile::TempDir {
        tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap()
    }

    fn share(path: &Path, workdir: Option<&Path>) -> Result<Prepared, ShareError> {
        let exec = dir();
        prepare(
            exec.path(),
            Uuid::now_v7(),
            path,
            String::new(),
            String::new(),
            workdir,
        )
    }

    #[test]
    fn a_bad_title_or_note_is_a_share_argument_error() {
        let place = dir();
        let path = place.path().join("report.txt");
        fs::write(&path, b"report").unwrap();
        let exec = dir();
        for (title, note) in [
            (
                "x".repeat(canterlot_proto::limits::MAX_SHARE_TITLE_BYTES + 1),
                String::new(),
            ),
            ("две\nстроки".into(), String::new()),
            (
                String::new(),
                "x".repeat(canterlot_proto::limits::MAX_SHARE_NOTE_BYTES + 1),
            ),
        ] {
            let error = prepare(
                exec.path(),
                Uuid::now_v7(),
                &path,
                title,
                note,
                Some(place.path()),
            )
            .unwrap_err();
            assert_eq!(
                error,
                ShareError::InvalidText,
                "bad title/note is not an unreadable file"
            );
        }
    }

    #[test]
    fn shares_only_from_the_workdir_or_the_temp_directory() {
        let place = outside_tmp();
        if inside_roots(&fs::canonicalize(place.path()).unwrap(), &share_roots(None)) {
            // Исходники сами лежат во временном каталоге: проверить «снаружи» нечем.
            return;
        }
        let path = place.path().join("report.txt");
        fs::write(&path, b"report").unwrap();
        assert_eq!(share(&path, None).unwrap_err(), ShareError::OutsideWorkdir);
        let other = outside_tmp();
        assert_eq!(
            share(&path, Some(other.path())).unwrap_err(),
            ShareError::OutsideWorkdir
        );
        assert_eq!(share(&path, Some(place.path())).unwrap().size, 6);
        let temp = dir();
        let path = temp.path().join("report.txt");
        fs::write(&path, b"report").unwrap();
        assert_eq!(share(&path, Some(place.path())).unwrap().size, 6);
    }

    #[test]
    fn refuses_proc_and_sys() {
        for path in [
            "/proc/self/environ",
            "/proc/self/status",
            "/sys/kernel/uevent_seqnum",
        ] {
            let error = share(Path::new(path), Some(Path::new("/"))).unwrap_err();
            assert_eq!(error, ShareError::Denied, "{path}");
        }
    }

    #[test]
    fn refuses_common_secret_stores() {
        let home = dir();
        for relative in [
            ".claude.json",
            ".gemini/oauth_creds.json",
            ".config/canterlot/token",
            ".config/gh/hosts.yml",
            ".git-credentials",
            ".netrc",
            ".npmrc",
            ".pypirc",
            ".docker/config.json",
            ".kube/config",
            ".aws/credentials",
            ".pgpass",
            ".cargo/credentials.toml",
        ] {
            let path = home.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"secret").unwrap();
            assert_eq!(
                share(&path, None).unwrap_err(),
                ShareError::Denied,
                "{relative}"
            );
        }
        let fine = home.path().join("claude.json.md");
        fs::write(&fine, b"doc").unwrap();
        assert!(share(&fine, None).is_ok());
    }

    #[test]
    fn refuses_symlink() {
        let dir = dir();
        let target = dir.path().join("real.txt");
        fs::write(&target, b"secret").unwrap();
        let link = dir.path().join("link.txt");
        symlink(&target, &link).unwrap();
        assert_eq!(
            prepare_tmp(
                dir.path(),
                Uuid::now_v7(),
                &link,
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::Symlink
        );
    }

    #[test]
    fn refuses_hard_link() {
        let dir = dir();
        let first = dir.path().join("one.txt");
        fs::write(&first, b"secret").unwrap();
        let second = dir.path().join("two.txt");
        fs::hard_link(&first, &second).unwrap();
        assert_eq!(
            prepare_tmp(
                dir.path(),
                Uuid::now_v7(),
                &first,
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::HardLink
        );
    }

    #[test]
    fn refuses_fifo() {
        let dir = dir();
        let fifo = dir.path().join("pipe");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_bits_truncate(0o644),
            0,
        )
        .unwrap();
        assert_eq!(
            prepare_tmp(
                dir.path(),
                Uuid::now_v7(),
                &fifo,
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::NotRegular
        );
    }

    #[test]
    fn refuses_denied_names_and_ssh() {
        let dir = dir();
        for name in [".env", "auth.json", ".credentials.json"] {
            let path = dir.path().join(name);
            fs::write(&path, b"secret").unwrap();
            assert_eq!(
                prepare_tmp(
                    dir.path(),
                    Uuid::now_v7(),
                    &path,
                    String::new(),
                    String::new()
                )
                .unwrap_err(),
                ShareError::Denied
            );
        }
        let ssh = dir.path().join(".ssh");
        fs::create_dir(&ssh).unwrap();
        let key = ssh.join("id_ed25519");
        fs::write(&key, b"secret").unwrap();
        assert_eq!(
            prepare_tmp(
                dir.path(),
                Uuid::now_v7(),
                &key,
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::Denied
        );
    }

    #[test]
    fn refuses_common_secret_file_names() {
        let dir = dir();
        let denied = [
            // Окружение: любые варианты, кроме шаблонов.
            ".env",
            ".env.local",
            ".env.production",
            "prod.env",
            ".envrc",
            // Ключи SSH: узкие префиксы без `.pub`, плюс PuTTY.
            "id_rsa",
            "id_dsa",
            "id_ecdsa",
            "id_ed25519",
            "id_xmss",
            "id_ed25519_sk",
            "id_ecdsa_sk",
            "id_rsa.bak",
            "id_rsa.old",
            "backup.ppk",
            // Секреты.
            "secret",
            "secret.json",
            "secrets",
            "client_secret.json",
            "service-account.json",
            "service_account_1.json",
            // Учётные данные.
            "credentials",
            "credentials.yml",
            ".dockercfg",
            ".htpasswd",
            ".my.cnf",
            ".netrc",
            ".pgpass",
            "kubeconfig",
            "prod.kubeconfig",
            // Инфраструктура и хранилища ключей.
            "terraform.tfstate",
            "terraform.tfstate.backup",
            "prod.tfvars",
            "app.jks",
            "app.keystore",
            "app.kdbx",
            "app.p12",
            "app.pfx",
            // Базы.
            "app.sqlite3",
            "app.db",
            // Истории.
            "fish_history",
            ".python_history",
            ".psql_history",
            ".mysql_history",
            ".bash_history",
            ".zsh_history",
            // Билеты Kerberos.
            "krb5cc_1000",
        ];
        for name in denied {
            let path = dir.path().join(name);
            fs::write(&path, b"secret").unwrap();
            assert_eq!(
                share(&path, Some(dir.path())).unwrap_err(),
                ShareError::Denied,
                "{name}"
            );
        }
        for name in [
            ".env.example",
            ".env.sample",
            ".env.template",
            ".env.dist",
            "id_ed25519.pub",
            "id_mapping.json",
            "id_list.csv",
            "id_card.png",
        ] {
            let path = dir.path().join(name);
            fs::write(&path, b"public").unwrap();
            assert!(
                share(&path, Some(dir.path())).is_ok(),
                "{name} must stay shareable"
            );
        }
    }

    #[test]
    fn refuses_secret_directories_and_everything_beneath_them() {
        let home = dir();
        for path in [
            ".azure/credentials",
            ".password-store/site.gpg",
            ".terraform.d/plugins",
            ".mozilla/profile",
            ".config/gcloud/access_tokens.db",
            ".config/glab-cli/config.yml",
            ".local/share/keyrings/login.keyring",
        ] {
            let path = home.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"secret").unwrap();
            assert_eq!(
                share(&path, Some(home.path())).unwrap_err(),
                ShareError::Denied,
                "{}",
                path.display()
            );
        }
    }

    #[test]
    fn file_name_patterns_apply_only_to_the_final_component() {
        let workdir = dir();
        for path in [
            "src/token/report.md",
            "tokens/usage.csv",
            "api-keys/README.md",
            "certs.key/notes.txt",
            "locks/report.lock/data.md",
        ] {
            let path = workdir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"report").unwrap();
            assert!(
                share(&path, Some(workdir.path())).is_ok(),
                "{} must stay shareable",
                path.display()
            );
        }
        // Рабочий каталог с «секретным» именем не запрещает файлы внутри него.
        let named = workdir.path().join("api-key-manager");
        fs::create_dir(&named).unwrap();
        let notes = named.join("notes.md");
        fs::write(&notes, b"notes").unwrap();
        assert!(share(&notes, Some(&named)).is_ok());

        // Настоящий секрет по-прежнему закрыт на последнем компоненте.
        let env = workdir.path().join(".env.local");
        fs::write(&env, b"A=1").unwrap();
        assert_eq!(
            share(&env, Some(workdir.path())).unwrap_err(),
            ShareError::Denied
        );
        let ssh = workdir.path().join(".ssh/config");
        fs::create_dir_all(ssh.parent().unwrap()).unwrap();
        fs::write(&ssh, b"Host *").unwrap();
        assert_eq!(
            share(&ssh, Some(workdir.path())).unwrap_err(),
            ShareError::Denied
        );
    }

    #[test]
    fn symlinks_near_the_final_component_do_not_hide_secrets() {
        // a) Обычное имя — ссылка на секретный файл. Конечная ссылка не
        // раскрывается: отказ `Symlink`, до секрета дело не доходит.
        let workdir = dir();
        fs::write(workdir.path().join(".env.local"), b"A=1").unwrap();
        let notes = workdir.path().join("notes.txt");
        symlink(".env.local", &notes).unwrap();
        assert_eq!(
            share(&notes, Some(workdir.path())).unwrap_err(),
            ShareError::Symlink
        );

        // b) Секретное имя — ссылка на обычный файл: запрет по имени срабатывает
        // до обращения к файловой системе.
        let workdir = dir();
        fs::write(workdir.path().join("plain.txt"), b"plain").unwrap();
        let env = workdir.path().join(".env");
        symlink("plain.txt", &env).unwrap();
        assert_eq!(
            share(&env, Some(workdir.path())).unwrap_err(),
            ShareError::Denied
        );

        // c) Ссылка-каталог с обычным именем раскрывается, файл внутри отдаётся.
        let workdir = dir();
        let data = workdir.path().join("data");
        fs::create_dir(&data).unwrap();
        fs::write(data.join("usage.csv"), b"a,b").unwrap();
        symlink("data", workdir.path().join("tokens")).unwrap();
        assert!(
            share(
                &workdir.path().join("tokens/usage.csv"),
                Some(workdir.path())
            )
            .is_ok()
        );

        // d) Ссылка-каталог ведёт в `.ssh`: запрет каталога срабатывает после
        // раскрытия ссылки, ещё до открытия файла.
        let workdir = dir();
        let ssh = workdir.path().join(".ssh");
        fs::create_dir(&ssh).unwrap();
        fs::write(ssh.join("config"), b"Host *").unwrap();
        symlink(".ssh", workdir.path().join("data")).unwrap();
        assert_eq!(
            share(&workdir.path().join("data/config"), Some(workdir.path())).unwrap_err(),
            ShareError::Denied
        );
    }

    #[test]
    fn refuses_too_large() {
        let dir = dir();
        let path = dir.path().join("big.bin");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_SHARE_BYTES + 1).unwrap();
        assert_eq!(
            prepare_tmp(
                dir.path(),
                Uuid::now_v7(),
                &path,
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::TooLarge
        );
    }

    #[test]
    fn copies_a_regular_file_and_hashes_it() {
        let dir = dir();
        let path = dir.path().join("note.txt");
        fs::write(&path, b"hello").unwrap();
        let prepared = prepare_tmp(
            dir.path(),
            Uuid::now_v7(),
            &path,
            "Title".into(),
            "Note".into(),
        )
        .unwrap();
        assert_eq!(prepared.name, "note.txt");
        assert_eq!(prepared.mime, "text/plain");
        assert_eq!(prepared.size, 5);
        assert!(media::media_path(dir.path(), prepared.id).exists());
        assert_eq!(
            fs::metadata(media::media_path(dir.path(), prepared.id))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn copies_an_empty_file_as_a_plain_zero_byte_file() {
        let dir = dir();
        for (name, mime) in [
            ("empty.txt", "text/plain"),
            (".gitkeep", "application/octet-stream"),
            // Пустой файл не бывает картинкой или видео, даже по расширению.
            ("empty.png", "application/octet-stream"),
            ("clip.mp4", "application/octet-stream"),
            ("clip.webm", "application/octet-stream"),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, b"").unwrap();
            let prepared = prepare_tmp(
                dir.path(),
                Uuid::now_v7(),
                &path,
                String::new(),
                String::new(),
            )
            .unwrap();
            assert_eq!(prepared.name, name);
            assert_eq!(prepared.mime, mime, "{name}");
            assert_eq!(prepared.size, 0);
            assert_eq!(
                prepared.sha256,
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            );
            assert_eq!((prepared.width, prepared.height), (None, None));
            let copy = media::media_path(dir.path(), prepared.id);
            assert_eq!(fs::metadata(&copy).unwrap().len(), 0);
        }
        assert_eq!(mime_for("clip.mp4", b"x"), "video/mp4");
    }

    #[test]
    fn refuses_new_secret_directories_and_key_names() {
        let home = dir();
        for relative in [
            ".cursor/mcp.json",
            ".codexbar/config.json",
            ".config/codexbar/config.json",
            ".config/forgejo/app.ini",
            ".config/canterlot/token",
            "id_rsa",
            "id_ed25519",
            "id_ecdsa",
            "deploy.pem",
            "signing.key",
            "bundle.p12",
            "cert.pfx",
            "token",
            "api.token",
            "service-token",
            "credentials.json",
            "auth.json",
        ] {
            let path = home.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"secret").unwrap();
            assert_eq!(
                share(&path, None).unwrap_err(),
                ShareError::Denied,
                "{relative}"
            );
        }
        // Отсутствующие имена того же вида уже запрещены: ответ не зависит от файла.
        for relative in [
            ".cursor/absent",
            ".config/forgejo/absent",
            "missing.pem",
            "missing_token",
            "credentials.json",
        ] {
            assert_eq!(
                share(&home.path().join(relative), None).unwrap_err(),
                ShareError::Denied,
                "{relative}"
            );
        }
    }

    #[test]
    fn refuses_new_secret_names_through_a_parent_alias() {
        let home = dir();
        let private = home.path().join("private");
        fs::create_dir(&private).unwrap();
        fs::write(private.join("auth.json"), b"secret").unwrap();
        symlink(&private, home.path().join("alias")).unwrap();
        assert_eq!(
            share(&home.path().join("alias").join("auth.json"), None).unwrap_err(),
            ShareError::Denied
        );
        assert_eq!(
            share(&home.path().join("alias").join("missing.pem"), None).unwrap_err(),
            ShareError::Denied
        );
        let cursor = home.path().join(".cursor");
        fs::create_dir(&cursor).unwrap();
        fs::write(cursor.join("mcp.json"), b"secret").unwrap();
        symlink(&cursor, home.path().join("cursor-alias")).unwrap();
        assert_eq!(
            share(&home.path().join("cursor-alias").join("mcp.json"), None).unwrap_err(),
            ShareError::Denied
        );
        assert_eq!(
            share(&home.path().join("cursor-alias").join("absent"), None).unwrap_err(),
            ShareError::Denied
        );
    }

    #[test]
    fn refuses_token_stores_but_allows_files_that_mention_tokens() {
        let home = dir();
        for name in [
            "token",
            ".token",
            "api.token",
            "token.json",
            "tokens.json",
            "my_token",
            "my_token.txt",
            "my_token.json",
            "my-token",
            "my-token.txt",
            "my-token.json",
            "access_token",
            "access_token.txt",
            "refresh_token.json",
            "api_token",
            "api-key",
            "api-key-v2.txt",
            "apikey",
            "apikey.json",
            "deploy.secret",
            "secrets.json",
            "secrets.env",
        ] {
            let path = home.path().join(name);
            fs::write(&path, b"secret").unwrap();
            assert_eq!(
                share(&path, Some(home.path())).unwrap_err(),
                ShareError::Denied,
                "{name}"
            );
        }
        for name in [
            "tokens-report.md",
            "token-usage.png",
            "tokenizer.rs",
            "tokens-compact.test.ts",
        ] {
            let path = home.path().join(name);
            fs::write(&path, b"report").unwrap();
            assert!(
                share(&path, Some(home.path())).is_ok(),
                "{name} must stay shareable"
            );
        }
    }

    #[test]
    fn refuses_a_missing_secret_path_without_touching_the_filesystem() {
        // Родитель `~/.ssh` отсутствует: ответ всё равно «секрет», не «файла нет».
        let dir = dir();
        let missing = dir.path().join(".ssh").join("absent");
        assert_eq!(share(&missing, None).unwrap_err(), ShareError::Denied);
    }

    #[test]
    fn refuses_key_under_symlinked_ssh_directory() {
        // `export -> ~/.ssh`: лексически секрета нет, после раскрытия — есть.
        let dir = dir();
        let home = dir.path().join("home");
        let ssh = home.join(".ssh");
        fs::create_dir_all(&ssh).unwrap();
        fs::write(ssh.join("id_ed25519"), b"secret").unwrap();
        symlink(&ssh, dir.path().join("export")).unwrap();
        assert_eq!(
            prepare_tmp(
                dir.path(),
                Uuid::now_v7(),
                &dir.path().join("export").join("id_ed25519"),
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::Denied
        );
    }

    #[test]
    fn refuses_key_when_ssh_itself_is_a_symlink() {
        // `~/.ssh -> /srv/private-keys`: после раскрытия секретного имени нет,
        // поэтому путь как он задан проверяется отдельно.
        let dir = dir();
        let keys = dir.path().join("srv").join("private-keys");
        fs::create_dir_all(&keys).unwrap();
        fs::write(keys.join("id_ed25519"), b"secret").unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        symlink(&keys, home.join(".ssh")).unwrap();
        assert_eq!(
            prepare_tmp(
                dir.path(),
                Uuid::now_v7(),
                &home.join(".ssh").join("id_ed25519"),
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::Denied
        );
    }

    #[test]
    fn refuses_key_through_chained_parent_symlinks() {
        // Ссылка несколькими уровнями выше прячет секретный каталог целиком.
        let dir = dir();
        let outer = dir.path().join("outer");
        let ssh = outer.join(".ssh");
        fs::create_dir_all(&ssh).unwrap();
        fs::write(ssh.join("id_ed25519"), b"secret").unwrap();
        symlink(&outer, dir.path().join("a")).unwrap();
        symlink(ssh.clone(), dir.path().join("a").join("b")).unwrap();
        assert_eq!(
            prepare_tmp(
                dir.path(),
                Uuid::now_v7(),
                &dir.path().join("a").join("b").join("id_ed25519"),
                String::new(),
                String::new()
            )
            .unwrap_err(),
            ShareError::Denied
        );
    }

    #[test]
    fn shares_file_through_benign_parent_symlink() {
        let dir = dir();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("note.txt"), b"hello").unwrap();
        symlink(&real, dir.path().join("alias")).unwrap();
        let prepared = prepare_tmp(
            dir.path(),
            Uuid::now_v7(),
            &dir.path().join("alias").join("note.txt"),
            String::new(),
            String::new(),
        )
        .unwrap();
        assert_eq!(prepared.name, "note.txt");
        assert_eq!(prepared.size, 5);
    }

    /// `share_file` идёт тем же `open_checked`, что и проба пути.
    fn denied(path: &Path, workdir: &Path) -> bool {
        share(path, Some(workdir)).unwrap_err() == ShareError::Denied
    }

    #[test]
    fn refuses_two_hop_chain_into_a_secret_directory() {
        // `alias -> .cursor`, `.cursor -> private`: раскрытие не должно прочитать
        // `private/config.json`, хотя ни одно имя в заданном пути не секретно.
        let home = dir();
        fs::create_dir(home.path().join("private")).unwrap();
        fs::write(home.path().join("private").join("config.json"), b"secret").unwrap();
        symlink("private", home.path().join(".cursor")).unwrap();
        symlink(".cursor", home.path().join("alias")).unwrap();
        assert!(denied(
            &home.path().join("alias").join("config.json"),
            home.path()
        ));
        assert!(denied(
            &home.path().join("alias").join("absent"),
            home.path()
        ));
    }

    #[test]
    fn an_alias_into_a_secret_directory_is_denied_whether_it_exists_or_not() {
        // Существующий `.cursor` и отсутствующий дают одинаковый ответ:
        // запрет решается по именам, до открытия компонента.
        let present = dir();
        fs::create_dir(present.path().join(".cursor")).unwrap();
        symlink(present.path().join(".cursor"), present.path().join("alias")).unwrap();
        let existing = share(
            &present.path().join("alias").join("sentinel"),
            Some(present.path()),
        )
        .unwrap_err();
        let absent = dir();
        symlink(absent.path().join(".cursor"), absent.path().join("alias")).unwrap();
        let missing = share(
            &absent.path().join("alias").join("sentinel"),
            Some(absent.path()),
        )
        .unwrap_err();
        assert_eq!(existing, ShareError::Denied);
        assert_eq!(existing, missing);
    }

    #[test]
    fn refuses_a_chain_whose_target_names_a_secret_symlink() {
        // `a -> .ssh/c`, а сам `.ssh` — ссылка на безобидный каталог: имя `.ssh`
        // встречается при раскрытии и запрещается до перехода по ссылке.
        let home = dir();
        let benign = home.path().join("benign");
        fs::create_dir_all(benign.join("c")).unwrap();
        fs::write(benign.join("c").join("note.txt"), b"secret").unwrap();
        symlink("benign", home.path().join(".ssh")).unwrap();
        symlink(".ssh/c", home.path().join("a")).unwrap();
        assert!(denied(&home.path().join("a").join("note.txt"), home.path()));
        assert!(denied(&home.path().join("a").join("absent"), home.path()));
    }

    #[test]
    fn refuses_an_absolute_chain_through_a_secret_config_directory() {
        // Абсолютная ссылка остаётся под `/R`, но проходит через `.config/forgejo`.
        let home = dir();
        fs::create_dir_all(home.path().join(".config")).unwrap();
        fs::create_dir_all(home.path().join("private").join("sub")).unwrap();
        fs::write(
            home.path().join("private").join("sub").join("note.txt"),
            b"secret",
        )
        .unwrap();
        symlink(
            home.path().join("private"),
            home.path().join(".config").join("forgejo"),
        )
        .unwrap();
        symlink(
            home.path().join(".config").join("forgejo").join("sub"),
            home.path().join("alias"),
        )
        .unwrap();
        assert!(denied(
            &home.path().join("alias").join("note.txt"),
            home.path()
        ));
        assert!(denied(
            &home.path().join("alias").join("absent"),
            home.path()
        ));
    }

    #[test]
    fn refuses_token_stems_with_text_and_json_suffixes() {
        let home = dir();
        for name in [
            "token",
            "tokens",
            "token.txt",
            "tokens.txt",
            "token.json",
            "tokens.json",
        ] {
            let path = home.path().join(name);
            fs::write(&path, b"secret").unwrap();
            assert!(denied(&path, home.path()), "{name} must be denied");
        }
        for name in [
            "tokens-report.md",
            "token-usage.png",
            "tokenizer.rs",
            "tokens-compact.test.ts",
        ] {
            let path = home.path().join(name);
            fs::write(&path, b"report").unwrap();
            assert!(
                share(&path, Some(home.path())).is_ok(),
                "{name} must stay shareable"
            );
        }
    }
}
