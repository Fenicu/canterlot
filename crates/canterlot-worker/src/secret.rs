//! Файлы секретов на машине сессии: tmpfs, 0700/0600 и ровно один срок жизни.
//!
//! Значение приходит одним управляющим сообщением и живёт только здесь, в файле.
//! Поэтому в этом модуле нет журналирования значения: `Debug`, ошибки и логи
//! называют лишь короткую причину, а не текст, путь или размер содержимого.
//!
//! Каталоги открываются один раз и дальше используются только относительные
//! `*at`-операции с `O_NOFOLLOW`: между проверкой и использованием путь не
//! переигрывается, а симлинк не уводит запись или уборку за пределы дерева.

use std::{
    collections::HashSet,
    fmt,
    io::{Read as _, Write as _},
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use canterlot_proto::worker::v1::{
    SecretDeliver, SecretDelivered, SecretFailed, SecretManifest, SecretManifests,
    SecretRemoveFailed, SecretRemoved,
};
use rustix::{
    fs::{
        AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, fstatfs, mkdirat, open, openat,
        renameat, statat, unlinkat,
    },
    io::Errno,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::SecretConfig;

/// Сколько файл живёт после записи.
pub const FILE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Все операции над файлами секретов идут по одной: уборка не может встать между
/// переименованием значения и манифеста или удалить файл свежей доставки.
static OPERATIONS: Mutex<()> = Mutex::new(());

fn operations() -> std::sync::MutexGuard<'static, ()> {
    OPERATIONS.lock().unwrap_or_else(|error| error.into_inner())
}

#[cfg(test)]
thread_local! {
    /// Тестовый крючок: следующая запись файла падает, не оставляя временного файла.
    static FAIL_NEXT_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Тестовый крючок: переименование значения падает после публикации манифеста.
    static FAIL_VALUE_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn take_write_failure() -> bool {
    FAIL_NEXT_WRITE.with(|flag| flag.replace(false))
}

#[cfg(not(test))]
fn take_write_failure() -> bool {
    false
}

#[cfg(test)]
fn take_value_commit_failure() -> bool {
    FAIL_VALUE_COMMIT.with(|flag| flag.replace(false))
}

#[cfg(not(test))]
fn take_value_commit_failure() -> bool {
    false
}

/// Короткий код причины: он уходит в факт и в журнал, но не содержит значения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// `XDG_RUNTIME_DIR` не задан.
    XdgUnset,
    /// Базовый каталог не на tmpfs: файл на диске был бы нарушением гарантии.
    NotTmpfs,
    /// Каталог не 0700, не принадлежит воркеру или является симлинком.
    InsecureDirectory,
    /// Записать или переименовать файл не удалось.
    Io,
    /// Манифест не читается или не совпадает с файлом.
    Manifest,
    /// Попытка уже отозвана или заменена более новой.
    StaleAttempt,
    /// Разговор, которого воркер не ведёт: файл писать некуда.
    UnknownConversation,
}

impl Failure {
    /// Причина уходит наружу строкой — только этот код, без путей и данных.
    pub fn code(self) -> &'static str {
        match self {
            Self::XdgUnset => "xdg_unset",
            Self::NotTmpfs => "not_tmpfs",
            Self::InsecureDirectory => "insecure_directory",
            Self::Io => "io",
            Self::Manifest => "manifest",
            Self::StaleAttempt => "stale_attempt",
            Self::UnknownConversation => "unknown_conversation",
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// Итог записи: путь и срок — метаданные, которые хаб отдаёт агенту.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: PathBuf,
    pub expires_at_ms: u64,
    pub conversation_id: Uuid,
}

/// Манифест рядом с файлом: только адреса и срок, значения нет.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    attempt_id: Uuid,
    conversation_id: Uuid,
    expires_at_ms: u64,
}

/// Надгробие запроса: только метаданные. Оно переживает файл и не даёт повторной
/// доставке воскресить отозванное или затереть более новое значение.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Tombstone {
    request_id: Uuid,
    conversation_id: Uuid,
    #[serde(default)]
    accepted: Option<Uuid>,
    #[serde(default)]
    terminal: Vec<Uuid>,
}

impl Tombstone {
    fn new(request_id: Uuid, conversation_id: Uuid) -> Self {
        Self {
            request_id,
            conversation_id,
            accepted: None,
            terminal: Vec::new(),
        }
    }

    fn is_terminal(&self, attempt_id: Uuid) -> bool {
        self.terminal.contains(&attempt_id)
    }

    fn supersede(&mut self, attempt_id: Uuid) {
        if !self.terminal.contains(&attempt_id) {
            self.terminal.push(attempt_id);
        }
        if self.accepted == Some(attempt_id) {
            self.accepted = None;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    Removed,
    /// Файла уже нет: отзыв повторный, повтор безвреден.
    Missing,
    /// Манифест принадлежит другой попытке: чужой файл не трогаем.
    Stale,
    /// Удалить не удалось: манифест остаётся, уборка повторит.
    Failed,
}

impl Removal {
    fn reason(self) -> &'static str {
        match self {
            Self::Removed | Self::Missing | Self::Stale => "revoked",
            Self::Failed => "io",
        }
    }
}

fn uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

const TMPFS_MAGIC: i64 = 0x0102_1994;
const BASE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Открытый и проверенный каталог: дальше все пути считаются от этого fd.
#[derive(Debug)]
struct PinnedDir {
    fd: OwnedFd,
}

impl PinnedDir {
    fn open_at(parent: &OwnedFd, name: &str) -> Result<Self, Failure> {
        let fd = openat(parent, name, BASE_FLAGS, Mode::empty())
            .map_err(|_| Failure::InsecureDirectory)?;
        Self::verify(fd)
    }

    /// Родитель базового каталога: нужен только как якорь для `mkdirat`/`openat`.
    /// Права и владельца у него не требуем, но симлинк и не-каталог отвергаем.
    fn open_parent(path: &Path) -> Result<Self, Failure> {
        let fd = open(path, BASE_FLAGS, Mode::empty()).map_err(|_| Failure::InsecureDirectory)?;
        let stat = fstat(&fd).map_err(|_| Failure::InsecureDirectory)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            return Err(Failure::InsecureDirectory);
        }
        Ok(Self { fd })
    }

    fn verify(fd: OwnedFd) -> Result<Self, Failure> {
        let stat = fstat(&fd).map_err(|_| Failure::InsecureDirectory)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
            || stat.st_uid != uid()
            || stat.st_mode & 0o777 != 0o700
        {
            return Err(Failure::InsecureDirectory);
        }
        Ok(Self { fd })
    }

    fn fd(&self) -> &OwnedFd {
        &self.fd
    }

    fn entries(&self) -> Result<Vec<String>, Failure> {
        let dir = Dir::read_from(&self.fd).map_err(|_| Failure::Io)?;
        Ok(dir
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                (!name.is_empty() && name != "." && name != "..").then_some(name)
            })
            .collect())
    }
}

/// Каталог разговора: открыт от базового fd, проверен на нём же.
struct ConversationDir {
    dir: PinnedDir,
    conversation_id: Uuid,
    root: PathBuf,
}

impl ConversationDir {
    fn ensure(base: &PinnedDir, root: &Path, conversation_id: Uuid) -> Result<Self, Failure> {
        let name = conversation_id.to_string();
        match openat(base.fd(), &name, BASE_FLAGS, Mode::empty()) {
            Ok(fd) => Ok(Self {
                dir: PinnedDir::verify(fd)?,
                conversation_id,
                root: root.to_path_buf(),
            }),
            Err(Errno::NOENT) => {
                match mkdirat(base.fd(), &name, Mode::from_bits_truncate(0o700)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(_) => return Err(Failure::Io),
                }
                Ok(Self {
                    dir: PinnedDir::open_at(base.fd(), &name)?,
                    conversation_id,
                    root: root.to_path_buf(),
                })
            }
            // Симлинк или не каталог: открывать нельзя.
            Err(_) => Err(Failure::InsecureDirectory),
        }
    }

    fn open(base: &PinnedDir, root: &Path, conversation_id: Uuid) -> Result<Self, Failure> {
        let name = conversation_id.to_string();
        Ok(Self {
            dir: PinnedDir::open_at(base.fd(), &name)?,
            conversation_id,
            root: root.to_path_buf(),
        })
    }

    fn fd(&self) -> &OwnedFd {
        self.dir.fd()
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(self.conversation_id.to_string()).join(name)
    }
}

fn manifest_name(request_id: Uuid) -> String {
    format!("{request_id}.json")
}

fn tombstone_name(request_id: Uuid) -> String {
    format!("{request_id}.tomb.json")
}

fn value_name(request_id: Uuid) -> String {
    request_id.to_string()
}

fn temp_name(request_id: Uuid, kind: &str, attempt_id: Uuid) -> String {
    format!(".{request_id}.{kind}.tmp-{attempt_id}")
}

/// `Ok(None)` — файла нет; ошибка открытия или чтения — `Err`: отсутствие и сбой
/// проверки не одно и то же, иначе отзыв подтвердил бы несуществующее удаление.
fn read_opt(dir: &OwnedFd, name: &str) -> Result<Option<Vec<u8>>, Failure> {
    let fd = match openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(None),
        Err(_) => return Err(Failure::Io),
    };
    let mut file = std::fs::File::from(fd);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|_| Failure::Io)?;
    Ok(Some(bytes))
}

/// `O_CREAT|O_EXCL|O_NOFOLLOW` и 0600: чужой файл не подменить симлинком.
fn write_new_at(dir: &OwnedFd, name: &str, bytes: &[u8]) -> Result<(), Failure> {
    let fd = openat(
        dir,
        name,
        OFlags::CREATE
            .union(OFlags::EXCL)
            .union(OFlags::WRONLY)
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC),
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|_| Failure::Io)?;
    let mut file = std::fs::File::from(fd);
    if take_write_failure() {
        return Err(Failure::Io);
    }
    file.write_all(bytes).map_err(|_| Failure::Io)?;
    file.sync_all().map_err(|_| Failure::Io)?;
    Ok(())
}

/// Временный файл убирается на любом пути ошибки, кроме успешного переименования.
struct TempFile<'a> {
    dir: &'a OwnedFd,
    name: String,
    armed: bool,
}

impl<'a> TempFile<'a> {
    fn new(dir: &'a OwnedFd, name: String) -> Self {
        Self {
            dir,
            name,
            armed: true,
        }
    }

    fn write(&self, bytes: &[u8]) -> Result<(), Failure> {
        write_new_at(self.dir, &self.name, bytes)
    }

    fn commit(mut self, destination: &str) -> Result<(), Failure> {
        renameat(self.dir, &self.name, self.dir, destination).map_err(|_| Failure::Io)?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for TempFile<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = unlinkat(self.dir, &self.name, AtFlags::empty());
        }
    }
}

fn read_manifest(dir: &OwnedFd, request_id: Uuid) -> Result<Option<Manifest>, Failure> {
    match read_opt(dir, &manifest_name(request_id))? {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| Failure::Manifest),
        None => Ok(None),
    }
}

fn read_tombstone(dir: &OwnedFd, request_id: Uuid) -> Result<Option<Tombstone>, Failure> {
    match read_opt(dir, &tombstone_name(request_id))? {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| Failure::Manifest),
        None => Ok(None),
    }
}

/// Надгробие запроса, при необходимости созданное: пустые id заменяются на
/// известные команде, чтобы отзыв работал и до первой доставки.
fn tombstone_for(
    dir: &OwnedFd,
    request_id: Uuid,
    conversation_id: Uuid,
) -> Result<Tombstone, Failure> {
    let mut tombstone = read_tombstone(dir, request_id)?
        .unwrap_or_else(|| Tombstone::new(request_id, conversation_id));
    if tombstone.request_id == Uuid::nil() {
        tombstone.request_id = request_id;
    }
    if tombstone.conversation_id == Uuid::nil() {
        tombstone.conversation_id = conversation_id;
    }
    Ok(tombstone)
}

fn write_tombstone(dir: &OwnedFd, request_id: Uuid, tombstone: &Tombstone) -> Result<(), Failure> {
    let encoded = serde_json::to_vec(tombstone).map_err(|_| Failure::Manifest)?;
    let temp = TempFile::new(dir, temp_name(request_id, "tomb", Uuid::nil()));
    temp.write(&encoded)?;
    temp.commit(&tombstone_name(request_id))
}

fn entry_stat(dir: &OwnedFd, name: &str) -> Option<Stat> {
    statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()
}

fn is_regular(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
}

fn is_symlink(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::Symlink
}

fn is_directory(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::Directory
}

fn is_tmpfs(fd: &OwnedFd) -> bool {
    match fstatfs(fd) {
        Ok(stat) => stat.f_type == TMPFS_MAGIC,
        Err(_) => false,
    }
}

/// Базовый каталог: создаётся 0700 от открытого родителя и проверяется на том же
/// fd, который дальше используется (fs type, владелец, права). Проверять один fd,
/// а работать с переоткрытым путём нельзя: подмена предка увела бы файл на диск.
fn open_base(config: &SecretConfig) -> Result<(PinnedDir, PathBuf), Failure> {
    let base = match &config.base_dir {
        Some(path) => path.clone(),
        None => {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or(Failure::XdgUnset)?;
            PathBuf::from(runtime).join("canterlot-secrets")
        }
    };
    let parent = base.parent().ok_or(Failure::InsecureDirectory)?;
    let name = base
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Failure::InsecureDirectory)?;
    let parent = PinnedDir::open_parent(parent).map_err(|_| Failure::InsecureDirectory)?;
    match mkdirat(parent.fd(), name, Mode::from_bits_truncate(0o700)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(_) => return Err(Failure::Io),
    }
    let pinned = PinnedDir::open_at(parent.fd(), name)?;
    if !is_tmpfs(pinned.fd()) {
        return Err(Failure::NotTmpfs);
    }
    Ok((pinned, base))
}

/// Сохраняет манифест до значения: файл значения не существует без манифеста.
/// Прежняя попытка сначала становится тупиковой и теряет свои файлы, поэтому
/// падение между шагами не оставит новому манифесту старые байты.
pub fn write(
    config: &SecretConfig,
    request: &SecretDeliver,
    live: &HashSet<Uuid>,
) -> Result<Written, Failure> {
    let _operations = operations();
    let request_id = Uuid::parse_str(&request.request_id).map_err(|_| Failure::Manifest)?;
    let attempt_id = Uuid::parse_str(&request.attempt_id).map_err(|_| Failure::Manifest)?;
    let conversation_id =
        Uuid::parse_str(&request.conversation_id).map_err(|_| Failure::Manifest)?;
    if !live.contains(&conversation_id) {
        return Err(Failure::UnknownConversation);
    }
    let (base, root) = open_base(config)?;
    let dir = ConversationDir::ensure(&base, &root, conversation_id)?;

    let mut tombstone = tombstone_for(dir.fd(), request_id, conversation_id)?;
    if tombstone.is_terminal(attempt_id) {
        return Err(Failure::StaleAttempt);
    }

    // Повтор той же принятой попытки: файл уже на месте, срок не сбрасывается.
    if tombstone.accepted == Some(attempt_id) {
        match read_manifest(dir.fd(), request_id)? {
            Some(manifest)
                if manifest.attempt_id == attempt_id
                    && entry_stat(dir.fd(), &value_name(request_id))
                        .is_some_and(|stat| is_regular(&stat)) =>
            {
                return Ok(Written {
                    path: dir.path(&value_name(request_id)),
                    expires_at_ms: manifest.expires_at_ms,
                    conversation_id,
                });
            }
            // Манифест не успел или принадлежит прежней попытке: значение
            // дописывается заново, старые байты не считаются принятыми.
            _ => {}
        }
    }

    // Новая попытка заменяет прежнюю: прежняя становится тупиковой и не воскреснет,
    // а её файлы уходят до записи нового значения.
    let mut retired: Vec<Uuid> = Vec::new();
    if let Some(previous) = tombstone
        .accepted
        .filter(|previous| *previous != attempt_id)
    {
        retired.push(previous);
    }
    if let Some(manifest) = read_manifest(dir.fd(), request_id)?
        && manifest.attempt_id != attempt_id
    {
        retired.push(manifest.attempt_id);
    }
    retired.sort_unstable();
    retired.dedup();
    if !retired.is_empty() {
        for previous in &retired {
            tombstone.supersede(*previous);
        }
        // Надгробие раньше файлов: упав между ними, повторная доставка прежней
        // попытки уже будет отвергнута.
        write_tombstone(dir.fd(), request_id, &tombstone)?;
        for _ in &retired {
            let _ = unlinkat(dir.fd(), value_name(request_id), AtFlags::empty());
            let _ = unlinkat(dir.fd(), manifest_name(request_id), AtFlags::empty());
        }
    }

    let expires_at_ms = u64::try_from(
        chrono::Utc::now()
            .checked_add_signed(chrono::TimeDelta::seconds(FILE_TTL.as_secs() as i64))
            .unwrap_or_else(chrono::Utc::now)
            .timestamp_millis(),
    )
    .unwrap_or(0);
    let manifest = Manifest {
        attempt_id,
        conversation_id,
        expires_at_ms,
    };
    let encoded_manifest = serde_json::to_vec(&manifest).map_err(|_| Failure::Manifest)?;
    tombstone.accepted = Some(attempt_id);
    let encoded_tombstone = serde_json::to_vec(&tombstone).map_err(|_| Failure::Manifest)?;

    let value_temp = TempFile::new(dir.fd(), temp_name(request_id, "value", attempt_id));
    value_temp.write(&request.value)?;
    let manifest_temp = TempFile::new(dir.fd(), temp_name(request_id, "manifest", attempt_id));
    manifest_temp.write(&encoded_manifest)?;
    let tomb_temp = TempFile::new(dir.fd(), temp_name(request_id, "tomb", attempt_id));
    tomb_temp.write(&encoded_tombstone)?;

    // Манифест и надгробие встают раньше значения: значение всегда опознано.
    tomb_temp.commit(&tombstone_name(request_id))?;
    manifest_temp.commit(&manifest_name(request_id))?;
    if take_value_commit_failure() {
        return Err(Failure::Io);
    }
    value_temp.commit(&value_name(request_id))?;
    Ok(Written {
        path: dir.path(&value_name(request_id)),
        expires_at_ms,
        conversation_id,
    })
}

fn remove_in_dir(
    dir: &ConversationDir,
    request_id: Uuid,
    attempt_id: Uuid,
) -> Result<Removal, Failure> {
    let manifest = read_manifest(dir.fd(), request_id)?;
    let mut tombstone = tombstone_for(dir.fd(), request_id, dir.conversation_id)?;

    // Отзыв всегда фиксирует попытку терминальной, даже если значения и манифеста
    // ещё нет: иначе опоздавшая доставка воскресила бы файл после «отозван».
    if !tombstone.is_terminal(attempt_id) {
        tombstone.supersede(attempt_id);
        if let Err(error) = write_tombstone(dir.fd(), request_id, &tombstone) {
            // Надгробие не записалось (например, кончилось место): файл всё равно
            // убираем, а хаб по ошибке повторит отзыв и допишет надгробие.
            if manifest
                .as_ref()
                .is_none_or(|manifest| manifest.attempt_id == attempt_id)
            {
                let _ = unlinkat(dir.fd(), value_name(request_id), AtFlags::empty());
                let _ = unlinkat(dir.fd(), manifest_name(request_id), AtFlags::empty());
            }
            return Err(error);
        }
    }

    let Some(manifest) = manifest else {
        // Манифеста нет: значения по контракту быть не должно, но файл мог
        // остаться от прерванной записи. Его удаление и есть подтверждение.
        let removed = match unlinkat(dir.fd(), value_name(request_id), AtFlags::empty()) {
            Ok(()) => true,
            Err(Errno::NOENT) => false,
            Err(_) => return Ok(Removal::Failed),
        };
        return Ok(if removed {
            Removal::Removed
        } else {
            Removal::Missing
        });
    };
    if manifest.attempt_id != attempt_id {
        return Ok(Removal::Stale);
    }
    let removed = match unlinkat(dir.fd(), value_name(request_id), AtFlags::empty()) {
        Ok(()) => true,
        Err(Errno::NOENT) => false,
        Err(_) => return Ok(Removal::Failed),
    };
    if let Err(error) = unlinkat(dir.fd(), manifest_name(request_id), AtFlags::empty())
        && error != Errno::NOENT
    {
        return Ok(Removal::Failed);
    }
    Ok(if removed {
        Removal::Removed
    } else {
        Removal::Missing
    })
}

/// Удаляет файл и манифест в каталоге названного разговора. `Missing` — только
/// проверенное отсутствие после успешной записи надгробия; любой сбой проверки
/// даёт `Failed`, чтобы хаб не объявил «отозван» без доказательства.
pub fn remove(
    config: &SecretConfig,
    conversation_id: Uuid,
    request_id: Uuid,
    attempt_id: Uuid,
) -> Removal {
    let _operations = operations();
    let Ok((base, root)) = open_base(config) else {
        return Removal::Failed;
    };
    let Ok(dir) = ConversationDir::ensure(&base, &root, conversation_id) else {
        return Removal::Failed;
    };
    remove_in_dir(&dir, request_id, attempt_id).unwrap_or(Removal::Failed)
}

/// Метаданные всех файлов: хаб сверяет `sending` после переподключения.
pub fn manifests(config: &SecretConfig, snapshot_id: &str) -> SecretManifests {
    let _operations = operations();
    let mut items = Vec::new();
    let Ok((base, root)) = open_base(config) else {
        return SecretManifests {
            manifests: items,
            snapshot_id: snapshot_id.to_owned(),
        };
    };
    let Ok(entries) = base.entries() else {
        return SecretManifests {
            manifests: items,
            snapshot_id: snapshot_id.to_owned(),
        };
    };
    for name in entries {
        let Some(stat) = entry_stat(base.fd(), &name) else {
            continue;
        };
        if !is_directory(&stat) || is_symlink(&stat) {
            continue;
        }
        let Ok(conversation_id) = Uuid::parse_str(&name) else {
            continue;
        };
        let Ok(dir) = ConversationDir::open(&base, &root, conversation_id) else {
            continue;
        };
        let Ok(files) = dir.dir.entries() else {
            continue;
        };
        for file in files {
            let Some(request) = file.strip_suffix(".json") else {
                continue;
            };
            if request.is_empty() || request.starts_with('.') || request.ends_with(".tomb") {
                continue;
            }
            let Ok(request_id) = Uuid::parse_str(request) else {
                continue;
            };
            let Some(manifest) = read_manifest(dir.fd(), request_id).ok().flatten() else {
                continue;
            };
            if manifest.conversation_id != conversation_id {
                continue;
            }
            // Отчитываемся только о принятой попытке: манифест, оставшийся от
            // прежней или терминальной попытки, не описывает байты на диске.
            let Some(tombstone) = read_tombstone(dir.fd(), request_id).ok().flatten() else {
                continue;
            };
            if tombstone.is_terminal(manifest.attempt_id)
                || tombstone.accepted != Some(manifest.attempt_id)
            {
                continue;
            }
            let value = value_name(request_id);
            if !entry_stat(dir.fd(), &value).is_some_and(|stat| is_regular(&stat)) {
                continue;
            }
            items.push(SecretManifest {
                attempt_id: manifest.attempt_id.to_string(),
                request_id: request_id.to_string(),
                conversation_id: manifest.conversation_id.to_string(),
                path: dir.path(&value).display().to_string(),
                expires_at_ms: manifest.expires_at_ms,
            });
        }
    }
    SecretManifests {
        manifests: items,
        snapshot_id: snapshot_id.to_owned(),
    }
}

/// Что уборка нашла и убрала: каждый известный файл отчитывается отдельно.
#[derive(Debug, Default)]
pub struct Swept {
    pub removed: Vec<SecretRemoved>,
    pub failed: Vec<SecretRemoveFailed>,
    pub corrupt: u64,
    pub orphans: u64,
}

impl Swept {
    fn fail(&mut self, attempt_id: Uuid, reason: &str) {
        self.failed.push(SecretRemoveFailed {
            attempt_id: attempt_id.to_string(),
            reason: reason.to_owned(),
        });
    }
}

fn unlink_entry(dir: &OwnedFd, name: &str) -> bool {
    unlinkat(dir, name, AtFlags::empty()).is_ok()
}

/// Убирает каталог, не заходя в симлинки: симлинк снимается как ссылка, а не как путь.
fn remove_tree_at(parent: &OwnedFd, name: &str, dir: &PinnedDir) -> u64 {
    let mut removed = 0;
    if let Ok(entries) = dir.entries() {
        for entry in entries {
            let Some(stat) = entry_stat(dir.fd(), &entry) else {
                continue;
            };
            if is_symlink(&stat) {
                if unlink_entry(dir.fd(), &entry) {
                    removed += 1;
                }
            } else if is_directory(&stat) {
                if let Ok(fd) = openat(dir.fd(), &entry, BASE_FLAGS, Mode::empty())
                    && let Ok(stat) = fstat(&fd)
                    && stat.st_uid == uid()
                    && let Ok(child) = PinnedDir::verify(fd)
                {
                    removed += remove_tree_at(dir.fd(), &entry, &child);
                }
            } else if unlink_entry(dir.fd(), &entry) {
                removed += 1;
            }
        }
    }
    if unlinkat(parent, name, AtFlags::REMOVEDIR).is_ok() {
        removed += 1;
    }
    removed
}

/// Уборка: просроченные файлы, сироты, брошенные временные файлы, испорченные
/// манифесты и каталоги разговоров, которых воркер больше не ведёт.
pub fn sweep(config: &SecretConfig, live: &[Uuid], now_ms: u64) -> Swept {
    sweep_with(config, live, now_ms, true)
}

/// `retire_dead = false` — каталоги разговоров не трогаются: до первого сеанса с
/// хабом список живых разговоров пуст, и уборка приняла бы их за брошенные.
/// Просроченные файлы, сироты и временные файлы убираются в любом случае.
pub fn sweep_with(config: &SecretConfig, live: &[Uuid], now_ms: u64, retire_dead: bool) -> Swept {
    let _operations = operations();
    let mut swept = Swept::default();
    let Ok((base, root)) = open_base(config) else {
        return swept;
    };
    let Ok(entries) = base.entries() else {
        return swept;
    };
    let live: HashSet<Uuid> = live.iter().copied().collect();
    for name in entries {
        let Some(stat) = entry_stat(base.fd(), &name) else {
            continue;
        };
        if is_symlink(&stat) {
            if unlink_entry(base.fd(), &name) {
                swept.orphans += 1;
            }
            continue;
        }
        if !is_directory(&stat) {
            if unlink_entry(base.fd(), &name) {
                swept.orphans += 1;
            }
            continue;
        }
        let Ok(conversation_id) = Uuid::parse_str(&name) else {
            // Не каталог разговора: не наш, не трогаем.
            continue;
        };
        let Ok(dir) = ConversationDir::open(&base, &root, conversation_id) else {
            continue;
        };
        // Каталог сессии, которую воркер больше не ведёт, убирается целиком.
        if retire_dead && !live.contains(&conversation_id) {
            swept.orphans += remove_tree_at(base.fd(), &name, &dir.dir);
            continue;
        }
        let Ok(files) = dir.dir.entries() else {
            continue;
        };
        for file in files {
            let Some(stat) = entry_stat(dir.fd(), &file) else {
                continue;
            };
            if is_symlink(&stat) {
                if unlink_entry(dir.fd(), &file) {
                    swept.orphans += 1;
                }
                continue;
            }
            if is_directory(&stat) {
                // Настоящий подкаталог в каталоге разговора не наш: не рекурсируем.
                continue;
            }
            if file.starts_with('.') {
                // Под блокировкой ни одна запись не идёт: все временные файлы брошены.
                if unlink_entry(dir.fd(), &file) {
                    swept.orphans += 1;
                }
                continue;
            }
            if file.ends_with(".tomb.json") {
                continue;
            }
            let Some(request) = file.strip_suffix(".json") else {
                // Файл без манифеста: его писал прерванный вызов, срок неизвестен.
                if let Ok(request_id) = Uuid::parse_str(&file)
                    && read_manifest(dir.fd(), request_id).ok().flatten().is_none()
                    && unlink_entry(dir.fd(), &file)
                {
                    swept.orphans += 1;
                }
                continue;
            };
            let Ok(request_id) = Uuid::parse_str(request) else {
                if unlink_entry(dir.fd(), &file) {
                    swept.corrupt += 1;
                }
                continue;
            };
            let manifest = match read_manifest(dir.fd(), request_id) {
                Ok(Some(manifest)) => manifest,
                Ok(None) | Err(_) => {
                    // Испорченный или пустой манифест: опознать байты нечем.
                    if unlink_entry(dir.fd(), &file) {
                        let _ = unlink_entry(dir.fd(), &value_name(request_id));
                        swept.corrupt += 1;
                    }
                    continue;
                }
            };
            let tombstone = match read_tombstone(dir.fd(), request_id) {
                Ok(tombstone) => tombstone,
                // Надгробие не читается: удалять нельзя, иначе отзыв подтвердил бы
                // отсутствие файла, не увидев запрет на его воскрешение.
                Err(_) => {
                    swept.fail(manifest.attempt_id, "io");
                    continue;
                }
            };
            let terminal = tombstone
                .as_ref()
                .is_some_and(|tombstone| tombstone.is_terminal(manifest.attempt_id));
            let expired = manifest.expires_at_ms <= now_ms;
            let value = value_name(request_id);
            let present = entry_stat(dir.fd(), &value).is_some_and(|stat| is_regular(&stat));
            if terminal || expired {
                // Сначала терминальная отметка, потом удаление: падение между
                // шагами не даст повторной доставке воскресить файл с новым сроком.
                if terminalize_attempt(&dir, request_id, manifest.attempt_id, tombstone).is_err() {
                    // Надгробие не записалось: файл всё равно убираем, а хаб по
                    // ошибке повторит отзыв и допишет отметку.
                    let _ = unlinkat(dir.fd(), &value, AtFlags::empty());
                    let _ = unlinkat(dir.fd(), &file, AtFlags::empty());
                    swept.fail(manifest.attempt_id, "io");
                    continue;
                }
                match unlinkat(dir.fd(), &value, AtFlags::empty()) {
                    Ok(()) | Err(Errno::NOENT) => {}
                    Err(_) => {
                        swept.fail(manifest.attempt_id, "io");
                        continue;
                    }
                }
                match unlinkat(dir.fd(), &file, AtFlags::empty()) {
                    Ok(()) | Err(Errno::NOENT) => {}
                    Err(_) => {
                        swept.fail(manifest.attempt_id, "io");
                        continue;
                    }
                }
                swept.removed.push(SecretRemoved {
                    attempt_id: manifest.attempt_id.to_string(),
                    reason: if terminal { "revoked" } else { "expired" }.into(),
                });
                continue;
            }
            if !present {
                // Значения нет у непросроченного манифеста: это прерванная запись
                // или чужое удаление, подтверждения «удалено» тут нет.
                match unlinkat(dir.fd(), &file, AtFlags::empty()) {
                    Ok(()) => {
                        swept.orphans += 1;
                    }
                    Err(Errno::NOENT) => {}
                    Err(_) => swept.fail(manifest.attempt_id, "io"),
                }
                continue;
            }
        }
    }
    swept
}

/// Записывает попытку в надгробие: `Ok(true)` — запись сделана, `Ok(false)` —
/// попытка уже терминальна.
fn terminalize_attempt(
    dir: &ConversationDir,
    request_id: Uuid,
    attempt_id: Uuid,
    tombstone: Option<Tombstone>,
) -> Result<bool, Failure> {
    let mut tombstone =
        tombstone.unwrap_or_else(|| Tombstone::new(request_id, dir.conversation_id));
    if tombstone.request_id == Uuid::nil() {
        tombstone.request_id = request_id;
    }
    if tombstone.conversation_id == Uuid::nil() {
        tombstone.conversation_id = dir.conversation_id;
    }
    if tombstone.is_terminal(attempt_id) {
        return Ok(false);
    }
    tombstone.supersede(attempt_id);
    write_tombstone(dir.fd(), request_id, &tombstone)?;
    Ok(true)
}

/// Факт доставки: только метаданные.
pub fn delivered_fact(request: &SecretDeliver, written: &Written) -> SecretDelivered {
    SecretDelivered {
        attempt_id: request.attempt_id.clone(),
        request_id: request.request_id.clone(),
        path: written.path.display().to_string(),
        expires_at_ms: written.expires_at_ms,
    }
}

/// Факт отказа: короткая причина без значения и без пути.
pub fn failed_fact(request: &SecretDeliver, failure: Failure) -> SecretFailed {
    SecretFailed {
        attempt_id: request.attempt_id.clone(),
        reason: failure.code().into(),
    }
}

/// Факт удаления: тот же `attempt_id`, который назвал отзыв.
pub fn removed_fact(attempt_id: Uuid, removal: Removal) -> SecretRemoved {
    SecretRemoved {
        attempt_id: attempt_id.to_string(),
        reason: removal.reason().into(),
    }
}

/// Файл не убран: хаб оставит `revoking` с подписью и повторит уборку.
pub fn remove_failed_fact(attempt_id: Uuid, reason: &str) -> SecretRemoveFailed {
    SecretRemoveFailed {
        attempt_id: attempt_id.to_string(),
        reason: reason.to_owned(),
    }
}

/// Живые разговоры: списанные и остановленные исполнения не держат каталоги.
pub(crate) fn live_conversations(
    infos: &std::collections::HashMap<Uuid, crate::exec::ExecInfo>,
) -> Vec<Uuid> {
    infos
        .values()
        .filter(|info| {
            !info.retired && !matches!(info.state.as_str(), "stopped" | "failed" | "owner_lost")
        })
        .map(|info| info.conversation_id)
        .collect()
}

/// Убирает файлы разговоров, которые воркер больше не ведёт, и просроченные файлы.
/// Запускается сразу при старте и раз в 10 минут, не дожидаясь хаба: просроченный
/// файл и брошенный временный не должны жить до первого переподключения.
pub(crate) async fn run_sweeper(
    config: SecretConfig,
    shared: std::sync::Arc<crate::exec::Shared>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(600));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // До первого сеанса с хабом `infos` ещё пуст: каталоги живых разговоров
    // нельзя принимать за брошенные. Всё остальное убирается и без хаба.
    let mut hub_ready = shared.hub.subscribe();
    let mut have_hub = hub_ready.borrow().is_some();
    loop {
        sweep_and_report(&config, &shared, have_hub).await;
        tokio::select! {
            changed = hub_ready.changed(), if !have_hub => {
                if changed.is_err() {
                    return;
                }
                if hub_ready.borrow_and_update().is_some() {
                    have_hub = true;
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow_and_update() {
                    return;
                }
            }
            _ = ticker.tick() => {}
        }
    }
}

async fn sweep_and_report(
    config: &SecretConfig,
    shared: &std::sync::Arc<crate::exec::Shared>,
    retire_dead: bool,
) {
    let swept = {
        let live = if retire_dead {
            shared
                .infos
                .lock()
                .map(|infos| live_conversations(&infos))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0);
        sweep_with(config, &live, now_ms, retire_dead)
    };
    if !swept.removed.is_empty()
        || !swept.failed.is_empty()
        || swept.corrupt > 0
        || swept.orphans > 0
    {
        let removed = swept.removed.clone();
        let failed = swept.failed.clone();
        let _ = shared
            .store
            .call(move |slot| {
                let store = crate::exec::open(slot)?;
                for fact in removed {
                    store.append_cj_fact(
                        canterlot_proto::worker::v1::record_body::Body::SecretRemoved(fact),
                    )?;
                }
                for fact in failed {
                    store.append_cj_fact(
                        canterlot_proto::worker::v1::record_body::Body::SecretRemoveFailed(fact),
                    )?;
                }
                Ok(())
            })
            .await;
        tracing::info!(
            kind = "secret_sweep",
            removed = swept.removed.len(),
            failed = swept.failed.len(),
            corrupt = swept.corrupt,
            orphans = swept.orphans,
            "secret files swept"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    /// Базовый каталог на настоящем tmpfs: `$XDG_RUNTIME_DIR` не трогаем.
    struct TestBase(PathBuf);

    impl TestBase {
        fn new() -> Self {
            let path =
                PathBuf::from("/dev/shm").join(format!("canterlot-secret-{}", Uuid::now_v7()));
            Self(path)
        }

        fn config(&self) -> SecretConfig {
            SecretConfig {
                base_dir: Some(self.0.clone()),
            }
        }
    }

    impl Drop for TestBase {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn request(conversation: Uuid, request: Uuid, attempt: Uuid, value: &str) -> SecretDeliver {
        SecretDeliver {
            attempt_id: attempt.to_string(),
            request_id: request.to_string(),
            conversation_id: conversation.to_string(),
            value: value.as_bytes().to_vec(),
        }
    }

    fn live_set(conversation: Uuid) -> HashSet<Uuid> {
        HashSet::from([conversation])
    }

    fn files_under(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(root) else {
            return found;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(files_under(&path));
            } else {
                found.push(path);
            }
        }
        found
    }

    #[test]
    fn writes_exact_bytes_into_a_private_file_with_a_metadata_only_manifest() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let value = "canterlot-secret-PROBE-строка\n";
        let written = write(
            &config,
            &request(conversation, request_id, attempt, value),
            &live_set(conversation),
        )
        .expect("write");
        let bytes = std::fs::read(&written.path).expect("file");
        assert_eq!(bytes, value.as_bytes(), "byte-exact, newline preserved");
        let file = std::fs::metadata(&written.path).expect("file metadata");
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let dir = std::fs::metadata(written.path.parent().unwrap()).expect("dir metadata");
        assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        assert_eq!(dir.uid(), uid());
        assert_eq!(written.conversation_id, conversation);

        let manifest = written
            .path
            .parent()
            .unwrap()
            .join(format!("{request_id}.json"));
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).expect("manifest")).unwrap();
        let mut keys: Vec<&String> = raw.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["attempt_id", "conversation_id", "expires_at_ms"],
            "the manifest never carries the value"
        );
        assert!(
            !std::fs::read_to_string(&manifest)
                .unwrap()
                .contains("PROBE")
        );
        let tomb = written
            .path
            .parent()
            .unwrap()
            .join(format!("{request_id}.tomb.json"));
        let text = std::fs::read_to_string(&tomb).unwrap();
        assert!(!text.contains("PROBE"), "the tombstone is metadata only");
        assert!(text.contains(&attempt.to_string()));
        let expires = written.expires_at_ms;
        assert!(expires > 0);
    }

    #[test]
    fn a_replayed_attempt_re_reports_the_file_without_rewriting_it() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let first = write(
            &config,
            &request(conversation, request_id, attempt, "first\n"),
            &live_set(conversation),
        )
        .unwrap();
        let again = write(
            &config,
            &request(conversation, request_id, attempt, "second\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(first.path, again.path);
        assert_eq!(first.expires_at_ms, again.expires_at_ms, "the expiry stays");
        assert_eq!(std::fs::read(&first.path).unwrap(), b"first\n");
    }

    #[test]
    fn a_new_attempt_replaces_the_old_value_and_a_replay_of_the_old_one_is_refused() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let first_attempt = Uuid::now_v7();
        let first = write(
            &config,
            &request(conversation, request_id, first_attempt, "old\n"),
            &live_set(conversation),
        )
        .unwrap();
        let second = write(
            &config,
            &request(conversation, request_id, Uuid::now_v7(), "new\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(first.path, second.path);
        assert_eq!(std::fs::read(&second.path).unwrap(), b"new\n");

        // A → B → A: повтор старой попытки не затирает новую.
        let replay = write(
            &config,
            &request(conversation, request_id, first_attempt, "resurrected\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(replay, Failure::StaleAttempt);
        assert_eq!(replay.code(), "stale_attempt");
        assert_eq!(std::fs::read(&second.path).unwrap(), b"new\n");
    }

    #[test]
    fn removal_deletes_the_file_and_ignores_a_stale_attempt() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let written = write(
            &config,
            &request(conversation, request_id, attempt, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(
            remove(&config, conversation, request_id, Uuid::now_v7()),
            Removal::Stale
        );
        assert!(
            written.path.exists(),
            "another attempt leaves the file alone"
        );
        assert_eq!(
            remove(&config, conversation, request_id, attempt),
            Removal::Removed
        );
        assert!(!written.path.exists());
        assert!(
            !written
                .path
                .parent()
                .unwrap()
                .join(format!("{request_id}.json"))
                .exists()
        );
        assert_eq!(
            remove(&config, conversation, request_id, attempt),
            Removal::Missing
        );

        // Доставка после отзыва: та же попытка уже тупиковая.
        let replay = write(
            &config,
            &request(conversation, request_id, attempt, "resurrected\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(replay, Failure::StaleAttempt);
        assert!(!written.path.exists());
    }

    #[test]
    fn manifests_list_only_files_with_a_manifest() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let written = write(
            &config,
            &request(conversation, request_id, attempt, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        let listed = manifests(&config, "snapshot-1");
        assert_eq!(listed.manifests.len(), 1);
        assert_eq!(listed.snapshot_id, "snapshot-1");
        let manifest = &listed.manifests[0];
        assert_eq!(manifest.attempt_id, attempt.to_string());
        assert_eq!(manifest.request_id, request_id.to_string());
        assert_eq!(manifest.path, written.path.display().to_string());
        assert_eq!(manifest.expires_at_ms, written.expires_at_ms);
    }

    #[test]
    fn a_symlinked_conversation_directory_is_refused() {
        let base = TestBase::new();
        let config = base.config();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(&base.0).unwrap();
        std::fs::set_permissions(&base.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        let conversation = Uuid::now_v7();
        std::os::unix::fs::symlink(elsewhere.path(), base.0.join(conversation.to_string()))
            .unwrap();
        let failure = write(
            &config,
            &request(conversation, Uuid::now_v7(), Uuid::now_v7(), "value\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(failure, Failure::InsecureDirectory);
        assert_eq!(failure.code(), "insecure_directory");
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_base_outside_tmpfs_is_refused() {
        // Каталог репозитория лежит на диске, а не на tmpfs: годной базой он быть не может.
        let directory = tempfile::Builder::new()
            .prefix("canterlot-secret-on-disk-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .unwrap();
        let config = SecretConfig {
            base_dir: Some(directory.path().join("secrets")),
        };
        assert_eq!(open_base(&config).unwrap_err(), Failure::NotTmpfs);
        assert!(!directory.path().join("secrets").join(".tmp").exists());
    }

    #[test]
    fn a_conversation_the_worker_does_not_run_is_refused() {
        let base = TestBase::new();
        let config = base.config();
        let conversation = Uuid::now_v7();
        let failure = write(
            &config,
            &request(conversation, Uuid::now_v7(), Uuid::now_v7(), "value\n"),
            &HashSet::new(),
        )
        .unwrap_err();
        assert_eq!(failure, Failure::UnknownConversation);
        assert_eq!(failure.code(), "unknown_conversation");
    }

    #[test]
    fn a_failed_write_leaves_no_temp_file_and_no_value() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        FAIL_NEXT_WRITE.with(|flag| flag.set(true));
        let failure = write(
            &config,
            &request(
                conversation,
                request_id,
                attempt,
                "canterlot-secret-PROBE\n",
            ),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(failure, Failure::Io);
        let files = files_under(&base.0);
        assert!(files.is_empty(), "no temp file survives: {files:?}");
    }

    #[test]
    fn a_failed_unlink_keeps_the_manifest_for_the_next_sweep() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let written = write(
            &config,
            &request(conversation, request_id, attempt, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        let dir = written.path.parent().unwrap().to_path_buf();
        // Значение — непустой каталог: unlinkat без REMOVEDIR ответит ошибкой.
        std::fs::remove_file(&written.path).unwrap();
        std::fs::create_dir(&written.path).unwrap();
        std::fs::write(written.path.join("inner"), b"keep").unwrap();
        assert_eq!(
            remove(&config, conversation, request_id, attempt),
            Removal::Failed
        );
        assert!(written.path.is_dir(), "the value stays until removal works");
        let manifest = dir.join(format!("{request_id}.json"));
        assert!(manifest.exists(), "the manifest is kept for the next sweep");
        // Файл действительно убрали, повтор подтверждает отсутствие и снимает манифест.
        std::fs::remove_dir_all(&written.path).unwrap();
        assert_eq!(
            remove(&config, conversation, request_id, attempt),
            Removal::Missing
        );
        assert!(!manifest.exists());
    }

    #[test]
    fn sweep_unlinks_a_planted_symlink_without_touching_its_target() {
        let base = TestBase::new();
        let config = base.config();
        let live = Uuid::now_v7();
        let written = write(
            &config,
            &request(live, Uuid::now_v7(), Uuid::now_v7(), "live\n"),
            &live_set(live),
        )
        .unwrap();
        let dir = written.path.parent().unwrap().to_path_buf();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("keep"), b"keep").unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), dir.join("planted")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path().join("keep"), dir.join("planted-file"))
            .unwrap();

        let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
        let swept = sweep(&config, &[live], now_ms);
        assert!(
            elsewhere.path().join("keep").exists(),
            "the target survives"
        );
        assert_eq!(
            std::fs::read(elsewhere.path().join("keep")).unwrap(),
            b"keep"
        );
        assert!(!dir.join("planted").exists());
        assert!(!dir.join("planted-file").exists());
        assert!(swept.orphans >= 2);
    }

    #[test]
    fn sweep_removes_expired_orphans_corrupt_manifests_and_dead_conversations() {
        let base = TestBase::new();
        let config = base.config();
        let live = Uuid::now_v7();
        let dead = Uuid::now_v7();
        let live_ok = write(
            &config,
            &request(live, Uuid::now_v7(), Uuid::now_v7(), "live\n"),
            &live_set(live),
        )
        .unwrap();
        let expired_request = Uuid::now_v7();
        let expired = write(
            &config,
            &request(live, expired_request, Uuid::now_v7(), "old\n"),
            &live_set(live),
        )
        .unwrap();
        let dead_written = write(
            &config,
            &request(dead, Uuid::now_v7(), Uuid::now_v7(), "dead\n"),
            &live_set(dead),
        )
        .unwrap();
        // Просроченный манифест: срок в прошлом.
        let manifest_path = expired
            .path
            .parent()
            .unwrap()
            .join(format!("{expired_request}.json"));
        let mut manifest: Manifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest.expires_at_ms = 1;
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        // Испорченный манифест и файл-сирота без манифеста.
        let corrupt = live_ok
            .path
            .parent()
            .unwrap()
            .join(format!("{}.json", Uuid::now_v7()));
        std::fs::write(&corrupt, b"{not json").unwrap();
        let orphan = live_ok
            .path
            .parent()
            .unwrap()
            .join(Uuid::now_v7().to_string());
        std::fs::write(&orphan, b"orphan").unwrap();
        // Брошенный временный файл: под блокировкой он уже не пишется.
        let temp = live_ok.path.parent().unwrap().join(".tmp-test");
        std::fs::write(&temp, b"temp").unwrap();

        let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
        let swept = sweep(&config, &[live], now_ms);
        assert!(live_ok.path.exists(), "a live file stays");
        assert!(!expired.path.exists(), "expired file removed");
        assert!(!manifest_path.exists());
        assert!(!dead_written.path.exists(), "dead conversation removed");
        assert!(!corrupt.exists());
        assert!(!orphan.exists());
        assert!(!temp.exists());
        assert_eq!(swept.removed.len(), 1);
        assert_eq!(swept.removed[0].reason, "expired");
        assert_eq!(swept.corrupt, 1);
        assert!(swept.orphans >= 1);
    }

    #[test]
    fn a_file_written_before_restart_is_reported_again_after_it() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let written = write(
            &config,
            &request(conversation, request_id, attempt, "before\n"),
            &live_set(conversation),
        )
        .unwrap();
        // Новый процесс: тот же каталог, пустая память.
        let restarted = SecretConfig {
            base_dir: Some(base.0.clone()),
        };
        let listed = manifests(&restarted, "after-restart");
        assert_eq!(listed.manifests.len(), 1);
        assert_eq!(listed.manifests[0].attempt_id, attempt.to_string());
        let again = write(
            &restarted,
            &request(conversation, request_id, attempt, "after\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(again.path, written.path);
        assert_eq!(std::fs::read(&again.path).unwrap(), b"before\n");
        // Следующая попытка после рестарта по-прежнему заменяет файл.
        let newer = write(
            &restarted,
            &request(conversation, request_id, Uuid::now_v7(), "newer\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(std::fs::read(&newer.path).unwrap(), b"newer\n");
    }

    #[test]
    fn sweep_confirms_a_removal_whose_unlink_failed_once() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let written = write(
            &config,
            &request(conversation, request_id, attempt, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        let dir = written.path.parent().unwrap().to_path_buf();
        std::fs::remove_file(&written.path).unwrap();
        std::fs::create_dir(&written.path).unwrap();
        std::fs::write(written.path.join("inner"), b"keep").unwrap();
        assert_eq!(
            remove(&config, conversation, request_id, attempt),
            Removal::Failed
        );
        // Значение исчезло другим путём, а манифест и надгробие остались.
        std::fs::remove_dir_all(&written.path).unwrap();
        let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
        let swept = sweep(&config, &[conversation], now_ms);
        assert_eq!(swept.removed.len(), 1);
        assert_eq!(swept.removed[0].attempt_id, attempt.to_string());
        assert_eq!(swept.removed[0].reason, "revoked");
        assert!(!dir.join(format!("{request_id}.json")).exists());
    }

    #[test]
    fn a_revoke_before_delivery_blocks_the_late_file() {
        // Инвариант: отозванная попытка не может появиться на машине позже,
        // даже если отзыв пришёл раньше доставки и каталога ещё не было.
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        assert_eq!(
            remove(&config, conversation, request_id, attempt),
            Removal::Missing,
            "проверенное отсутствие подтверждает отзыв"
        );
        let failure = write(
            &config,
            &request(conversation, request_id, attempt, "resurrected\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(failure, Failure::StaleAttempt);
        let name = attempt.to_string();
        assert!(
            !files_under(&base.0)
                .iter()
                .any(|path| path.file_name() == Some(std::ffi::OsStr::new(&name))),
            "the delayed value never materialised"
        );
        assert_eq!(
            remove(&config, conversation, request_id, attempt),
            Removal::Missing,
            "a repeated revoke is harmless"
        );
    }

    #[test]
    fn expiry_terminalises_the_attempt_before_the_file_disappears() {
        // Инвариант: истёкшая попытка не воскресает с новым сроком после повтора.
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let written = write(
            &config,
            &request(conversation, request_id, attempt, "old\n"),
            &live_set(conversation),
        )
        .unwrap();
        let manifest_path = written
            .path
            .parent()
            .unwrap()
            .join(format!("{request_id}.json"));
        let mut manifest: Manifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest.expires_at_ms = 1;
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

        let swept = sweep(&config, &[conversation], 2);
        assert_eq!(swept.removed.len(), 1);
        assert_eq!(swept.removed[0].reason, "expired");
        assert!(!written.path.exists());
        let failure = write(
            &config,
            &request(conversation, request_id, attempt, "resurrected\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(failure, Failure::StaleAttempt);
        assert!(!written.path.exists());
    }

    #[test]
    fn an_inspection_failure_is_not_a_successful_removal() {
        // Нечитаемый каталог разговора — не доказательство отсутствия файла.
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let written = write(
            &config,
            &request(conversation, request_id, attempt, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        let dir = written.path.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            remove(&config, conversation, request_id, attempt),
            Removal::Failed,
            "the directory mode is not 0700: the inspection failed"
        );
        assert!(written.path.exists(), "the value is untouched");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            remove(&config, conversation, request_id, attempt),
            Removal::Removed
        );
        assert!(!written.path.exists());
    }

    #[test]
    fn a_failed_tombstone_write_removes_the_value_and_asks_for_a_retry() {
        // Место на tmpfs кончилось на надгробии: файл всё равно убираем, но
        // отзыв не подтверждаем, чтобы хаб повторил и дописал отметку.
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let written = write(
            &config,
            &request(conversation, request_id, attempt, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        FAIL_NEXT_WRITE.with(|flag| flag.set(true));
        assert_eq!(
            remove(&config, conversation, request_id, attempt),
            Removal::Failed
        );
        assert!(!written.path.exists(), "the value is gone anyway");
        assert!(
            !written
                .path
                .parent()
                .unwrap()
                .join(format!("{request_id}.json"))
                .exists()
        );
        // Повтор дописывает надгробие и подтверждает отсутствие.
        assert_eq!(
            remove(&config, conversation, request_id, attempt),
            Removal::Missing
        );
        let replay = write(
            &config,
            &request(conversation, request_id, attempt, "resurrected\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(replay, Failure::StaleAttempt);
    }

    #[test]
    fn an_interrupted_replacement_never_certifies_old_bytes_as_new() {
        // Падение между манифестом новой попытки и её значением: манифест B не
        // должен описывать байты A, иначе сверка после рестарта объявит B доставленным.
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let first = write(
            &config,
            &request(conversation, request_id, Uuid::now_v7(), "old\n"),
            &live_set(conversation),
        )
        .unwrap();
        let second_attempt = Uuid::now_v7();
        FAIL_VALUE_COMMIT.with(|flag| flag.set(true));
        let failure = write(
            &config,
            &request(conversation, request_id, second_attempt, "new\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(failure, Failure::Io);
        assert!(
            !first.path.exists(),
            "the old value is retired before the new manifest is published"
        );
        let listed = manifests(&config, "after-crash");
        assert!(
            listed.manifests.is_empty(),
            "no attempt is certified without its own value: {listed:?}"
        );
        // Повтор той же новой попытки дописывает её значение и срок.
        let again = write(
            &config,
            &request(conversation, request_id, second_attempt, "new\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(std::fs::read(&again.path).unwrap(), b"new\n");
        let listed = manifests(&config, "after-replay");
        assert_eq!(listed.manifests.len(), 1);
        assert_eq!(listed.manifests[0].attempt_id, second_attempt.to_string());
    }

    #[test]
    fn the_base_fd_returned_for_use_is_the_checked_tmpfs_directory() {
        // Проверки tmpfs, владельца и 0700 идут на том же fd, с которым дальше
        // работают `*at`-операции: подмена пути после проверки никуда не ведёт.
        let base = TestBase::new();
        let config = base.config();
        let (pinned, root) = open_base(&config).expect("checked base");
        assert_eq!(root, base.0);
        assert!(is_tmpfs(pinned.fd()), "the used fd is on tmpfs");
        let stat = fstat(pinned.fd()).expect("base stat");
        assert_eq!(stat.st_uid, uid());
        assert_eq!(stat.st_mode & 0o777, 0o700);
    }

    #[test]
    fn sweep_before_a_hub_session_keeps_conversation_dirs_but_removes_expired_files() {
        // До первого сеанса с хабом список живых разговоров пуст: каталоги
        // трогать нельзя, а срок жизни файлов от connectivity не зависит.
        let base = TestBase::new();
        let config = base.config();
        let live = Uuid::now_v7();
        let dead = Uuid::now_v7();
        let dead_written = write(
            &config,
            &request(dead, Uuid::now_v7(), Uuid::now_v7(), "dead\n"),
            &live_set(dead),
        )
        .unwrap();
        let expired_request = Uuid::now_v7();
        let expired = write(
            &config,
            &request(live, expired_request, Uuid::now_v7(), "old\n"),
            &live_set(live),
        )
        .unwrap();
        let manifest_path = expired
            .path
            .parent()
            .unwrap()
            .join(format!("{expired_request}.json"));
        let mut manifest: Manifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest.expires_at_ms = 1;
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

        let swept = sweep_with(&config, &[], 2, false);
        assert!(dead_written.path.exists(), "a conversation dir stays");
        assert!(!expired.path.exists(), "an expired value is removed");
        assert_eq!(swept.removed.len(), 1);

        let swept = sweep_with(&config, &[live], 2, true);
        assert!(!dead_written.path.exists(), "after a hub session it goes");
        assert!(swept.orphans >= 1);
    }

    #[test]
    fn the_tmpfs_message_never_holds_the_value() {
        let (conversation, request_id, attempt) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let deliver = request(conversation, request_id, attempt, "canterlot-secret-PROBE");
        let failed = failed_fact(&deliver, Failure::NotTmpfs);
        assert!(!format!("{failed:?}").contains("PROBE"));
        assert_eq!(failed.reason, "not_tmpfs");
    }
}
