//! Файлы секретов на машине сессии: tmpfs, 0700/0600 и поколения вместо попыток.
//!
//! Значение приходит одним управляющим сообщением и живёт только здесь, в файле.
//! Поэтому в этом модуле нет журналирования значения: `Debug`, ошибки и логи
//! называют лишь короткую причину, а не текст, путь или размер содержимого.
//!
//! Каталоги открываются один раз и дальше используются только относительные
//! `*at`-операции с `O_NOFOLLOW`: между проверкой и использованием путь не
//! переигрывается, а симлинк не уводит запись или уборку за пределы дерева.
//!
//! Инвариант: как только хаб объявил поколение `g` недействительным (отзыв,
//! истечение, замена, потеря, конец сессии), на машине не остаётся и не может
//! появиться ни одного файла с байтами поколения `g` или старше — при любом
//! порядке сообщений, сбое, перезапуске, переподключении и даже удалении или
//! восстановлении каталога разговора. Поэтому забор (`fence`) пишется раньше
//! файла, отзыв поднимает `dead` раньше удаления, а доставка отказывает всему,
//! что не выше `max(seen, dead)`.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    io::{Read as _, Write as _},
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use canterlot_proto::worker::v1::{
    SecretDeliver, SecretDelivered, SecretFailed, SecretRemoveFailed, SecretRemoved,
};
use rustix::{
    fs::{
        AtFlags, Dir, FileType, FlockOperation, Mode, OFlags, Stat, flock, fstat, fstatfs, fsync,
        mkdirat, open, openat, renameat, statat, unlinkat,
    },
    io::Errno,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::SecretConfig;

/// Сколько файл живёт после записи, если хаб не назвал свой срок.
pub const FILE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Сутки жизни файла: столько получает поколение, если хаб не назвал свой срок.
pub const FENCE_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// Запас после последнего известного срока: за него отзыв успевает дойти, а
/// часы хаба и воркера могут разойтись на минуты, не на сутки.
pub const FENCE_GRACE_MS: u64 = 60 * 60 * 1000;

/// Все операции над файлами секретов идут по одной: уборка не может встать между
/// переименованием значения и манифеста или удалить файл свежей доставки.
static OPERATIONS: Mutex<()> = Mutex::new(());

/// Запросы, у которых забор не записался при отзыве: доставка обязана отказать,
/// даже если в памяти остался прежний `dead`.
static REMOVAL_PENDING: Mutex<Option<HashSet<Uuid>>> = Mutex::new(None);

fn operations() -> std::sync::MutexGuard<'static, ()> {
    OPERATIONS.lock().unwrap_or_else(|error| error.into_inner())
}

fn mark_removal_pending(request_id: Uuid) {
    let mut pending = REMOVAL_PENDING
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    pending.get_or_insert_with(HashSet::new).insert(request_id);
}

fn clear_removal_pending(request_id: Uuid) {
    if let Some(pending) = REMOVAL_PENDING
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_mut()
    {
        pending.remove(&request_id);
    }
}

/// Корни, для которых подготовка хранилища не подтвердилась: доставка и отзыв
/// отказывают, пока следующий старт не доведёт уборку прежнего корня до конца.
static DISABLED_ROOTS: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

fn set_disabled(root: &Path, disabled: bool) {
    let mut roots = DISABLED_ROOTS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let roots = roots.get_or_insert_with(HashSet::new);
    if disabled {
        roots.insert(root.to_path_buf());
    } else {
        roots.remove(root);
    }
}

fn storage_disabled(config: &SecretConfig) -> bool {
    let Ok(root) = base_path(config) else {
        return true;
    };
    DISABLED_ROOTS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .is_some_and(|roots| roots.contains(&root))
}

pub(crate) fn disable_storage(config: &SecretConfig) {
    if let Ok(root) = base_path(config) {
        set_disabled(&root, true);
    }
}

fn is_removal_pending(request_id: Uuid) -> bool {
    REMOVAL_PENDING
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .is_some_and(|pending| pending.contains(&request_id))
}

#[cfg(test)]
thread_local! {
    /// Тестовый крючок: следующая запись файла падает, не оставляя временного файла.
    static FAIL_NEXT_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Тестовый крючок: следующая запись забора падает.
    static FAIL_NEXT_FENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Тестовый крючок: переименование значения падает после публикации забора.
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
fn take_fence_failure() -> bool {
    FAIL_NEXT_FENCE.with(|flag| flag.replace(false))
}

#[cfg(not(test))]
fn take_fence_failure() -> bool {
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
    /// Записать, переименовать или прочитать файл не удалось.
    Io,
    /// Манифест или забор не читается либо противоречит файлу.
    Manifest,
    /// Поколение уже отозвано или заменено более новым.
    StaleAttempt,
    /// Отзыв этого запроса не смог записать забор: доставка запрещена до повтора.
    RemovalPending,
    /// Разговор, которого воркер не ведёт: файл писать некуда.
    UnknownConversation,
    /// Срок, названный хабом, уже прошёл: публиковать просроченное нельзя.
    Expired,
    /// Хранилище не подтверждено после смены корня: файлы не трогаем.
    StorageDisabled,
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
            Self::RemovalPending => "removal_pending",
            Self::UnknownConversation => "unknown_conversation",
            Self::Expired => "expired",
            Self::StorageDisabled => "storage_disabled",
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

/// Манифест рядом с файлом: только поколение и срок, значения нет.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
    generation: u64,
    expires_at_ms: u64,
}

/// Забор запроса: переживает файл и каталог разговора, лежит вне их.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Fence {
    conversation_id: Uuid,
    #[serde(default)]
    seen: u64,
    #[serde(default)]
    dead: u64,
    #[serde(default)]
    updated_at: u64,
    /// Наибольший срок среди поколений, которые этот забор видел или отзывал:
    /// раньше него забор не убирается, иначе поздняя доставка нашла бы чистый лист.
    #[serde(default)]
    max_expires_at_ms: u64,
}

impl Fence {
    fn new(conversation_id: Uuid) -> Self {
        Self {
            conversation_id,
            seen: 0,
            dead: 0,
            updated_at: 0,
            max_expires_at_ms: 0,
        }
    }
}

/// Подтверждённое удаление или отказ: «удалено» без проверки не выдаётся.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    Removed,
    Failed,
}

fn uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

const TMPFS_MAGIC: i64 = 0x0102_1994;
const BASE_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

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

    /// Перебор каталога: ошибка чтения — ошибка, а не пустой список. Иначе
    /// удаление подтвердило бы отсутствие файла, которого просто не увидели.
    fn entries(&self) -> Result<Vec<String>, Failure> {
        let mut dir = Dir::read_from(&self.fd).map_err(|_| Failure::Io)?;
        let mut names = Vec::new();
        while let Some(entry) = dir.read() {
            let entry = entry.map_err(|_| Failure::Io)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.is_empty() && name != "." && name != ".." {
                names.push(name);
            }
        }
        Ok(names)
    }

    fn is_empty(&self) -> Result<bool, Failure> {
        Ok(self.entries()?.is_empty())
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
        Self::open_opt(base, root, conversation_id)?.ok_or(Failure::InsecureDirectory)
    }

    /// `Ok(None)` — каталога нет: файлов в нём тоже нет, отсутствие проверено.
    fn open_opt(
        base: &PinnedDir,
        root: &Path,
        conversation_id: Uuid,
    ) -> Result<Option<Self>, Failure> {
        let name = conversation_id.to_string();
        match openat(base.fd(), &name, BASE_FLAGS, Mode::empty()) {
            Ok(fd) => Ok(Some(Self {
                dir: PinnedDir::verify(fd)?,
                conversation_id,
                root: root.to_path_buf(),
            })),
            Err(Errno::NOENT) => Ok(None),
            Err(_) => Err(Failure::InsecureDirectory),
        }
    }

    fn fd(&self) -> &OwnedFd {
        self.dir.fd()
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(self.conversation_id.to_string()).join(name)
    }
}

/// Базовый каталог под замком: mutex процесса плюс `flock` на `$base/.lock`,
/// поэтому запись одна и внутри воркера, и между процессами.
struct BaseLock {
    base: PinnedDir,
    root: PathBuf,
    _lock: std::fs::File,
}

fn value_name(request_id: Uuid) -> String {
    request_id.to_string()
}

fn manifest_name(request_id: Uuid) -> String {
    format!("{request_id}.json")
}

fn value_temp_name(request_id: Uuid, generation: u64) -> String {
    format!(".{request_id}.{generation}.tmp")
}

fn manifest_temp_name(request_id: Uuid, generation: u64) -> String {
    format!(".{request_id}.{generation}.json.tmp")
}

fn fence_name(request_id: Uuid) -> String {
    format!("{request_id}.json")
}

fn is_temp_for(name: &str, request_id: Uuid) -> bool {
    name.len() > 1 && name.starts_with(&format!(".{request_id}.")) && name.ends_with(".tmp")
}

fn is_abandoned_temp(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(".tmp")
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
fn write_new_at(dir: &OwnedFd, name: &str, bytes: &[u8], value: bool) -> Result<(), Failure> {
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
    if value && take_write_failure() {
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
    /// Только значение проходит тестовый крючок отказа записи; забор — нет.
    value: bool,
}

impl<'a> TempFile<'a> {
    fn new(dir: &'a OwnedFd, name: String) -> Self {
        Self {
            dir,
            name,
            armed: true,
            value: true,
        }
    }

    fn fence(dir: &'a OwnedFd, name: String) -> Self {
        Self {
            dir,
            name,
            armed: true,
            value: false,
        }
    }

    fn write(&self, bytes: &[u8]) -> Result<(), Failure> {
        write_new_at(self.dir, &self.name, bytes, self.value)
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

/// `Ok(None)` — забора нет; нечитаемый или испорченный забор — `Err`: доставка
/// и отзыв тогда отказывают закрыто, а не угадывают поколения.
fn read_fence(fences: &PinnedDir, request_id: Uuid) -> Result<Option<Fence>, Failure> {
    match read_opt(fences.fd(), &fence_name(request_id))? {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| Failure::Manifest),
        None => Ok(None),
    }
}

/// Атомарная запись забора: временный файл, fsync, rename, fsync каталога.
fn write_fence(fences: &PinnedDir, request_id: Uuid, fence: &Fence) -> Result<(), Failure> {
    let encoded = serde_json::to_vec(fence).map_err(|_| Failure::Manifest)?;
    if take_fence_failure() {
        return Err(Failure::Io);
    }
    let temp = TempFile::fence(
        fences.fd(),
        format!(".fence-{request_id}-{}.tmp", Uuid::now_v7()),
    );
    temp.write(&encoded)?;
    temp.commit(&fence_name(request_id))?;
    fsync(fences.fd()).map_err(|_| Failure::Io)?;
    Ok(())
}

/// `Ok(None)` — файла нет; ошибка проверки — `Err`, а не «нет файла».
fn entry_stat(dir: &OwnedFd, name: &str) -> Result<Option<Stat>, Failure> {
    match statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(stat)),
        Err(Errno::NOENT) => Ok(None),
        Err(_) => Err(Failure::Io),
    }
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
fn base_path(config: &SecretConfig) -> Result<PathBuf, Failure> {
    match &config.base_dir {
        Some(path) => Ok(path.clone()),
        None => {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or(Failure::XdgUnset)?;
            Ok(PathBuf::from(runtime).join("canterlot-secrets"))
        }
    }
}

fn open_base(config: &SecretConfig) -> Result<(PinnedDir, PathBuf), Failure> {
    let base = base_path(config)?;
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

fn lock_base(config: &SecretConfig) -> Result<BaseLock, Failure> {
    let (base, root) = open_base(config)?;
    let fd = openat(
        base.fd(),
        ".lock",
        OFlags::CREATE
            .union(OFlags::RDWR)
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC),
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|_| Failure::Io)?;
    let lock = std::fs::File::from(fd);
    flock(&lock, FlockOperation::LockExclusive).map_err(|_| Failure::Io)?;
    Ok(BaseLock {
        base,
        root,
        _lock: lock,
    })
}

/// Каталог заборов лежит вне каталогов разговоров: удаление или восстановление
/// разговора не может вернуть отозванное поколение.
fn open_fences(base: &PinnedDir) -> Result<PinnedDir, Failure> {
    match mkdirat(base.fd(), ".fences", Mode::from_bits_truncate(0o700)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(_) => return Err(Failure::Io),
    }
    PinnedDir::open_at(base.fd(), ".fences")
}

/// Где воркер в прошлый раз держал секреты: путь и идентичность каталога на
/// диске. Смена корня без уборки прежнего оставила бы в нём открытый текст.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RootIdentity {
    path: String,
    dev: u64,
    ino: u64,
}

const ROOT_IDENTITY: &str = "secret-root.json";

fn verify_owned_dir(fd: OwnedFd) -> Result<PinnedDir, Failure> {
    let stat = fstat(&fd).map_err(|_| Failure::InsecureDirectory)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory || stat.st_uid != uid() {
        return Err(Failure::InsecureDirectory);
    }
    Ok(PinnedDir { fd })
}

fn open_existing_root_opt(path: &Path) -> Result<Option<PinnedDir>, Failure> {
    match open(path, BASE_FLAGS, Mode::empty()) {
        Ok(fd) => Ok(Some(verify_owned_dir(fd)?)),
        Err(Errno::NOENT) => Ok(None),
        Err(_) => Err(Failure::InsecureDirectory),
    }
}

fn data_dir_pinned(data_dir: &Path) -> Result<PinnedDir, Failure> {
    let fd = open(data_dir, BASE_FLAGS, Mode::empty()).map_err(|_| Failure::InsecureDirectory)?;
    verify_owned_dir(fd)
}

fn read_root_identity(data_dir: &PinnedDir) -> Result<Option<RootIdentity>, Failure> {
    let Some(bytes) = read_opt(data_dir.fd(), ROOT_IDENTITY)? else {
        return Ok(None);
    };
    if bytes.len() > 4096 {
        return Err(Failure::Manifest);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| Failure::Manifest)
}

fn write_root_identity(data_dir: &PinnedDir, identity: &RootIdentity) -> Result<(), Failure> {
    let encoded = serde_json::to_vec(identity).map_err(|_| Failure::Manifest)?;
    // Уникальное имя: брошенный прошлым падением временный файл не мешает записи.
    let temp = TempFile::fence(
        data_dir.fd(),
        format!(".secret-root-{}.tmp", Uuid::now_v7()),
    );
    temp.write(&encoded)?;
    temp.commit(ROOT_IDENTITY)?;
    fsync(data_dir.fd()).map_err(|_| Failure::Io)
}

/// Убирает значения, манифесты, временные файлы и заборы прежнего корня теми же
/// пришпиленными и не идущими по ссылкам операциями. Подкаталог внутри каталога
/// разговора считается чужим: такой корень не подтверждается.
fn wipe_root(root: &PinnedDir) -> Result<(), Failure> {
    for name in root.entries()? {
        if name == ".lock" {
            continue;
        }
        let Some(stat) = entry_stat(root.fd(), &name)? else {
            continue;
        };
        if is_directory(&stat) && !is_symlink(&stat) {
            let fd =
                openat(root.fd(), &name, BASE_FLAGS, Mode::empty()).map_err(|_| Failure::Io)?;
            let child = verify_owned_dir(fd)?;
            for entry in child.entries()? {
                let Some(entry_stat) = entry_stat(child.fd(), &entry)? else {
                    continue;
                };
                if is_directory(&entry_stat) && !is_symlink(&entry_stat) {
                    return Err(Failure::InsecureDirectory);
                }
                unlinkat(child.fd(), &entry, AtFlags::empty()).map_err(|_| Failure::Io)?;
            }
            if !child.is_empty()? {
                return Err(Failure::Io);
            }
            unlinkat(root.fd(), &name, AtFlags::REMOVEDIR).map_err(|_| Failure::Io)?;
        } else {
            unlinkat(root.fd(), &name, AtFlags::empty()).map_err(|_| Failure::Io)?;
        }
    }
    if root.entries()?.iter().any(|name| name != ".lock") {
        return Err(Failure::Io);
    }
    Ok(())
}

struct RootLock {
    dir: PinnedDir,
    _lock: std::fs::File,
}

fn lock_dir(dir: &PinnedDir) -> Result<std::fs::File, Failure> {
    let fd = openat(
        dir.fd(),
        ".lock",
        OFlags::CREATE
            .union(OFlags::RDWR)
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC),
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|_| Failure::Io)?;
    let lock = std::fs::File::from(fd);
    flock(&lock, FlockOperation::LockExclusive).map_err(|_| Failure::Io)?;
    Ok(lock)
}

fn lock_existing_root(path: &Path) -> Result<Option<RootLock>, Failure> {
    let Some(dir) = open_existing_root_opt(path)? else {
        return Ok(None);
    };
    let lock = lock_dir(&dir)?;
    Ok(Some(RootLock { dir, _lock: lock }))
}

/// Держит замок ещё одного корня, если он существует и ещё не взят.
fn lock_more(
    path: &Path,
    configured: &Path,
    locked: &mut Vec<PathBuf>,
    extras: &mut Vec<RootLock>,
) -> Result<(), Failure> {
    if path == configured || locked.iter().any(|known| known == path) {
        return Ok(());
    }
    if let Some(lock) = lock_existing_root(path)? {
        locked.push(path.to_path_buf());
        extras.push(lock);
    }
    Ok(())
}

/// Корень по умолчанию: его тоже надо очистить, если файл идентичности потерян,
/// иначе прежний запуск с XDG_RUNTIME_DIR оставил бы там открытый текст.
pub(crate) fn default_root() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|runtime| PathBuf::from(runtime).join("canterlot-secrets"))
}

fn prepare_storage_inner(
    config: &SecretConfig,
    data_dir: &Path,
    default_root: Option<&Path>,
) -> Result<(), Failure> {
    let (base, root) = open_base(config)?;
    // Подготовка — тоже операция хранилища: замок настроенного корня, прежнего
    // записанного и корня по умолчанию держится до записи новой идентичности.
    let configured_lock = lock_dir(&base)?;
    let configured = RootLock {
        dir: base,
        _lock: configured_lock,
    };
    let stat = fstat(configured.dir.fd()).map_err(|_| Failure::InsecureDirectory)?;
    let identity = RootIdentity {
        path: root.display().to_string(),
        dev: stat.st_dev,
        ino: stat.st_ino,
    };
    let data_dir = data_dir_pinned(data_dir)?;
    let recorded = read_root_identity(&data_dir)?;
    let mut locked = vec![root.clone()];
    let mut extras = Vec::new();
    match recorded {
        Some(recorded) if recorded == identity => Ok(()),
        Some(recorded) => {
            let previous = PathBuf::from(&recorded.path);
            lock_more(&previous, &root, &mut locked, &mut extras)?;
            if previous != root
                && let Some(previous) = open_existing_root_opt(&previous)?
            {
                wipe_root(&previous)?;
            }
            // Настроенный корень тоже может хранить старые значения (например,
            // его вернули после смены).
            wipe_root(&configured.dir)?;
            write_root_identity(&data_dir, &identity)
        }
        None => {
            if let Some(default) = default_root {
                lock_more(default, &root, &mut locked, &mut extras)?;
                if default != root
                    && let Some(default) = open_existing_root_opt(default)?
                {
                    wipe_root(&default)?;
                }
            }
            wipe_root(&configured.dir)?;
            write_root_identity(&data_dir, &identity)
        }
    }
}

/// Подготовка хранилища до приёма доставок: если корень сменился (или файл
/// идентичности потерян), прежний, настроенный и корень по умолчанию очищаются
/// под замками до записи новой идентичности. Любая неподтверждённая уборка
/// оставляет корень выключенным до следующего успешного старта.
pub(crate) fn prepare_storage(config: &SecretConfig, data_dir: &Path) -> Result<(), Failure> {
    prepare_storage_with_default(config, data_dir, default_root().as_deref())
}

pub(crate) fn prepare_storage_with_default(
    config: &SecretConfig,
    data_dir: &Path,
    default_root: Option<&Path>,
) -> Result<(), Failure> {
    let _operations = operations();
    let root = base_path(config)?;
    set_disabled(&root, true);
    let prepared = prepare_storage_inner(config, data_dir, default_root);
    if prepared.is_ok() {
        set_disabled(&root, false);
    }
    prepared
}

/// Эпоха хранилища: загрузка плюс inode базового каталога. Новая эпоха означает,
/// что tmpfs очищен и прежних файлов нет, даже если строки хаба говорят иное.
pub fn epoch(config: &SecretConfig, boot_id: &str) -> String {
    let inode = open_base(config)
        .ok()
        .and_then(|(base, _)| fstat(base.fd()).ok())
        .map(|stat| stat.st_ino)
        .unwrap_or(0);
    format!("{boot_id}:{inode}")
}

/// Публикует файл поколения `generation`: забор первым, манифест последним.
///
/// Отказ до записи забора оставляет диск нетронутым; отказ после — убирает
/// временные файлы и значение, если манифест на диске не это поколение.
pub fn deliver(
    config: &SecretConfig,
    request: &SecretDeliver,
    live: &HashSet<Uuid>,
) -> Result<Written, Failure> {
    let _operations = operations();
    if storage_disabled(config) {
        return Err(Failure::StorageDisabled);
    }
    let request_id = Uuid::parse_str(&request.request_id).map_err(|_| Failure::Manifest)?;
    let conversation_id =
        Uuid::parse_str(&request.conversation_id).map_err(|_| Failure::Manifest)?;
    if request.generation == 0 {
        return Err(Failure::Manifest);
    }
    if request.expires_at_ms <= now_ms() {
        // Срок хаба уже прошёл: даже если забор успели убрать, публиковать
        // просроченные байты нельзя — иначе уборка забора воскресила бы файл.
        return Err(Failure::Expired);
    }
    if !live.contains(&conversation_id) {
        return Err(Failure::UnknownConversation);
    }
    if is_removal_pending(request_id) {
        return Err(Failure::RemovalPending);
    }
    let lock = lock_base(config)?;
    let fences = open_fences(&lock.base)?;

    let mut fence = match read_fence(&fences, request_id)? {
        Some(fence) => {
            if fence.conversation_id != conversation_id {
                return Err(Failure::Manifest);
            }
            fence
        }
        None => Fence::new(conversation_id),
    };
    let generation = request.generation;
    if generation < fence.seen || generation <= fence.dead {
        return Err(Failure::StaleAttempt);
    }
    if generation == fence.seen {
        // Повтор уже опубликованного поколения: файл не перезаписывается и срок
        // не продлевается. Иначе повтор после частичного сбоя дал бы новый срок.
        return match published(&lock, conversation_id, request_id, generation) {
            Some(expires_at_ms) => Ok(Written {
                path: conversation_path(&lock, conversation_id, &value_name(request_id)),
                expires_at_ms,
                conversation_id,
            }),
            None => Err(Failure::StaleAttempt),
        };
    }

    // Забор раньше файла: падение между ними не даст доставке воскресить
    // отозванное поколение, а восстановление увидит несовпадение и уберёт файл.
    fence.seen = generation;
    fence.updated_at = now_ms();
    fence.max_expires_at_ms = fence.max_expires_at_ms.max(request.expires_at_ms);
    write_fence(&fences, request_id, &fence)?;

    let dir = match ConversationDir::ensure(&lock.base, &lock.root, conversation_id) {
        Ok(dir) => dir,
        Err(error) => {
            retire_unpublished(&lock, conversation_id, request_id, generation);
            return Err(error);
        }
    };
    let manifest = Manifest {
        generation,
        expires_at_ms: request.expires_at_ms,
    };
    let encoded = match serde_json::to_vec(&manifest) {
        Ok(encoded) => encoded,
        Err(_) => {
            retire_unpublished(&lock, conversation_id, request_id, generation);
            return Err(Failure::Manifest);
        }
    };
    let published = (|| -> Result<(), Failure> {
        let value_temp = TempFile::new(dir.fd(), value_temp_name(request_id, generation));
        value_temp.write(&request.value)?;
        let manifest_temp = TempFile::new(dir.fd(), manifest_temp_name(request_id, generation));
        manifest_temp.write(&encoded)?;
        if take_value_commit_failure() {
            return Err(Failure::Io);
        }
        value_temp.commit(&value_name(request_id))?;
        manifest_temp.commit(&manifest_name(request_id))?;
        fsync(dir.fd()).map_err(|_| Failure::Io)
    })();
    if let Err(error) = published {
        retire_unpublished(&lock, conversation_id, request_id, generation);
        return Err(error);
    }
    Ok(Written {
        path: dir.path(&value_name(request_id)),
        expires_at_ms: request.expires_at_ms,
        conversation_id,
    })
}

fn conversation_path(root: &BaseLock, conversation_id: Uuid, name: &str) -> PathBuf {
    root.root.join(conversation_id.to_string()).join(name)
}

/// Срок опубликованного поколения; `None` — файла или манифеста этого поколения
/// нет, и повтор доставки не может считаться принятым.
fn published(
    lock: &BaseLock,
    conversation_id: Uuid,
    request_id: Uuid,
    generation: u64,
) -> Option<u64> {
    let dir = ConversationDir::open(&lock.base, &lock.root, conversation_id).ok()?;
    let manifest = read_manifest(dir.fd(), request_id).ok().flatten()?;
    if manifest.generation != generation {
        return None;
    }
    if !entry_stat(dir.fd(), &value_name(request_id))
        .ok()
        .flatten()
        .is_some_and(|stat| is_regular(&stat))
    {
        return None;
    }
    Some(manifest.expires_at_ms)
}

/// Неполная публикация: если манифест на диске не это поколение, значение
/// удаляется консервативно — чужие байты не должны остаться под нашим забором.
fn retire_unpublished(lock: &BaseLock, conversation_id: Uuid, request_id: Uuid, generation: u64) {
    let Ok(Some(dir)) = ConversationDir::open_opt(&lock.base, &lock.root, conversation_id) else {
        return;
    };
    let same = read_manifest(dir.fd(), request_id)
        .ok()
        .flatten()
        .is_some_and(|manifest| manifest.generation == generation);
    if !same {
        let _ = unlinkat(dir.fd(), value_name(request_id), AtFlags::empty());
        if !read_manifest(dir.fd(), request_id).ok().flatten().is_some() {
            let _ = unlinkat(dir.fd(), manifest_name(request_id), AtFlags::empty());
        }
    }
    for name in dir.dir.entries().unwrap_or_default() {
        if is_temp_for(&name, request_id) {
            let _ = unlinkat(dir.fd(), &name, AtFlags::empty());
        }
    }
}

/// Поднимает `dead` и `seen` до `up_to`, затем убирает файл и манифест, если
/// манифест не новее `up_to` (или не читается, или принадлежит другому разговору).
/// `Removed` выдаётся только после проверки отсутствия и успешной записи забора.
pub fn revoke(
    config: &SecretConfig,
    conversation_id: Uuid,
    request_id: Uuid,
    up_to: u64,
    max_expires_at_ms: u64,
    _reason: &str,
) -> Removal {
    let _operations = operations();
    if storage_disabled(config) {
        mark_removal_pending(request_id);
        return Removal::Failed;
    }
    let lock = match lock_base(config) {
        Ok(lock) => lock,
        Err(_) => {
            mark_removal_pending(request_id);
            return Removal::Failed;
        }
    };
    let fences = match open_fences(&lock.base) {
        Ok(fences) => fences,
        Err(_) => {
            mark_removal_pending(request_id);
            return Removal::Failed;
        }
    };
    let fence_ok = match read_fence(&fences, request_id) {
        Ok(fence) => {
            let mut fence = fence.unwrap_or_else(|| Fence::new(conversation_id));
            if fence.conversation_id != conversation_id {
                false
            } else {
                fence.dead = fence.dead.max(up_to);
                fence.seen = fence.seen.max(up_to);
                fence.updated_at = now_ms();
                // Срок — тот, что хаб отправил с покрытым поколением: сравнение
                // идёт с часами воркера, но число выдано хабом, поэтому
                // расхождение часов не сокращает горизонт забора.
                fence.max_expires_at_ms = fence.max_expires_at_ms.max(max_expires_at_ms);
                match write_fence(&fences, request_id, &fence) {
                    Ok(()) => true,
                    Err(_) => {
                        mark_removal_pending(request_id);
                        false
                    }
                }
            }
        }
        Err(_) => {
            // Забор не читается: не перезаписываем его вслепую (можно потерять
            // большее `dead`), но файл всё равно убираем.
            mark_removal_pending(request_id);
            false
        }
    };
    let removed = revoke_locked(&lock, request_id, conversation_id, up_to);
    if fence_ok && removed {
        clear_removal_pending(request_id);
        Removal::Removed
    } else {
        Removal::Failed
    }
}

fn revoke_locked(lock: &BaseLock, request_id: Uuid, conversation_id: Uuid, up_to: u64) -> bool {
    let Some(dir) = (match ConversationDir::open_opt(&lock.base, &lock.root, conversation_id) {
        Ok(dir) => dir,
        Err(_) => return false,
    }) else {
        // Каталога нет — файлов в нём тоже нет. Проверять больше нечего.
        return true;
    };
    let manifest = read_manifest(dir.fd(), request_id);
    let manifest = manifest.unwrap_or_default();
    let covers = manifest
        .as_ref()
        .is_some_and(|manifest| manifest.generation <= up_to);
    if manifest.is_none() || covers {
        match unlinkat(dir.fd(), value_name(request_id), AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(_) => return false,
        }
        match unlinkat(dir.fd(), manifest_name(request_id), AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(_) => return false,
        }
    }
    for name in match dir.dir.entries() {
        Ok(names) => names,
        Err(_) => return false,
    } {
        if is_temp_for(&name, request_id)
            && !matches!(
                unlinkat(dir.fd(), &name, AtFlags::empty()),
                Ok(()) | Err(Errno::NOENT)
            )
        {
            return false;
        }
    }
    // Проверка, а не вера в unlink: отсутствие подтверждается тем же fd.
    if manifest.is_none() || covers {
        match entry_stat(dir.fd(), &value_name(request_id)) {
            Ok(None) => {}
            Ok(Some(_)) | Err(_) => return false,
        }
    }
    for name in match dir.dir.entries() {
        Ok(names) => names,
        Err(_) => return false,
    } {
        if is_temp_for(&name, request_id) {
            return false;
        }
    }
    true
}

/// Что восстановление нашло и убрало; нужно только для журнала.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Recovered {
    pub temps: u64,
    pub values: u64,
    pub failed: u64,
}

/// Восстановление до приёма любых доставок: брошенные временные файлы, значения
/// без манифеста, значения мёртвого поколения и неполной публикации.
pub fn recover(config: &SecretConfig) -> Recovered {
    let _operations = operations();
    let mut stats = Recovered::default();
    if storage_disabled(config) {
        return stats;
    }
    let Ok(lock) = lock_base(config) else {
        return stats;
    };
    let Ok(fences) = open_fences(&lock.base) else {
        stats.failed += 1;
        return stats;
    };
    let Ok(names) = fences.entries() else {
        stats.failed += 1;
        return stats;
    };
    for name in names {
        if is_abandoned_temp(&name) && unlinkat(fences.fd(), &name, AtFlags::empty()).is_ok() {
            stats.temps += 1;
        }
    }
    let Ok(entries) = lock.base.entries() else {
        stats.failed += 1;
        return stats;
    };
    for name in entries {
        if name == ".fences" || name == ".lock" {
            continue;
        }
        let Some(stat) = entry_stat(lock.base.fd(), &name).ok().flatten() else {
            continue;
        };
        if !is_directory(&stat) || is_symlink(&stat) {
            continue;
        }
        let Ok(conversation_id) = Uuid::parse_str(&name) else {
            continue;
        };
        let Ok(dir) = ConversationDir::open(&lock.base, &lock.root, conversation_id) else {
            stats.failed += 1;
            continue;
        };
        let Ok(files) = dir.dir.entries() else {
            stats.failed += 1;
            continue;
        };
        for file in files {
            if is_abandoned_temp(&file) {
                if unlinkat(dir.fd(), &file, AtFlags::empty()).is_ok() {
                    stats.temps += 1;
                } else {
                    stats.failed += 1;
                }
                continue;
            }
            let Ok(request_id) = Uuid::parse_str(&file) else {
                continue;
            };
            let fence = match read_fence(&fences, request_id) {
                Ok(fence) => fence.unwrap_or_else(|| Fence::new(conversation_id)),
                // Испорченный забор: доставки всё равно откажут, файл не трогаем.
                Err(_) => {
                    stats.failed += 1;
                    continue;
                }
            };
            let manifest = read_manifest(dir.fd(), request_id);
            let retire = match &manifest {
                Ok(Some(manifest)) => {
                    manifest.generation <= fence.dead || manifest.generation != fence.seen
                }
                // Манифеста нет или он испорчен: опознать байты нечем.
                Ok(None) | Err(_) => true,
            };
            if !retire {
                continue;
            }
            let unlinked = unlinkat(dir.fd(), &file, AtFlags::empty()).is_ok();
            let manifest_unlinked = matches!(
                unlinkat(dir.fd(), manifest_name(request_id), AtFlags::empty()),
                Ok(()) | Err(Errno::NOENT)
            );
            if unlinked && manifest_unlinked {
                stats.values += 1;
            } else {
                stats.failed += 1;
            }
        }
    }
    stats
}

/// Что уборка нашла и убрала.
#[derive(Debug, Default, Clone)]
pub struct Swept {
    pub removed: Vec<SecretRemoved>,
    pub failed: Vec<SecretRemoveFailed>,
    pub temps: u64,
    pub fences: u64,
}

/// Уборка: просроченные файлы, брошенные временные файлы, пустые каталоги
/// разговоров и старые заборы без значения. Идёт без сеанса с хабом.
pub fn sweep(config: &SecretConfig, now: u64) -> Swept {
    let _operations = operations();
    let mut swept = Swept::default();
    if storage_disabled(config) {
        return swept;
    }
    let Ok(lock) = lock_base(config) else {
        return swept;
    };
    let Ok(fences) = open_fences(&lock.base) else {
        return swept;
    };
    let Ok(entries) = lock.base.entries() else {
        return swept;
    };
    for name in entries {
        if name == ".fences" || name == ".lock" {
            continue;
        }
        let Some(stat) = entry_stat(lock.base.fd(), &name).ok().flatten() else {
            continue;
        };
        if !is_directory(&stat) || is_symlink(&stat) {
            if is_abandoned_temp(&name) && unlinkat(lock.base.fd(), &name, AtFlags::empty()).is_ok()
            {
                swept.temps += 1;
            }
            continue;
        }
        let Ok(conversation_id) = Uuid::parse_str(&name) else {
            continue;
        };
        let Ok(dir) = ConversationDir::open(&lock.base, &lock.root, conversation_id) else {
            continue;
        };
        let Ok(files) = dir.dir.entries() else {
            continue;
        };
        let mut failed_here = false;
        for file in files {
            if is_abandoned_temp(&file) {
                match unlinkat(dir.fd(), &file, AtFlags::empty()) {
                    Ok(()) => swept.temps += 1,
                    Err(Errno::NOENT) => {}
                    Err(_) => failed_here = true,
                }
                continue;
            }
            let Ok(request_id) = Uuid::parse_str(&file) else {
                continue;
            };
            let manifest = match read_manifest(dir.fd(), request_id) {
                Ok(Some(manifest)) => manifest,
                // Без манифеста байты не опознать: это прерванная публикация.
                Ok(None) | Err(_) => {
                    let value_gone = matches!(
                        unlinkat(dir.fd(), &file, AtFlags::empty()),
                        Ok(()) | Err(Errno::NOENT)
                    );
                    let manifest_gone = matches!(
                        unlinkat(dir.fd(), manifest_name(request_id), AtFlags::empty()),
                        Ok(()) | Err(Errno::NOENT)
                    );
                    if !(value_gone && manifest_gone) {
                        failed_here = true;
                    }
                    continue;
                }
            };
            if manifest.expires_at_ms > now {
                continue;
            }
            // Истечение — это отзыв до своего поколения: сначала забор, потом файл.
            let fence_ok = match read_fence(&fences, request_id) {
                Ok(fence) => {
                    let mut fence = fence.unwrap_or_else(|| Fence::new(conversation_id));
                    fence.dead = fence.dead.max(manifest.generation);
                    fence.seen = fence.seen.max(manifest.generation);
                    fence.updated_at = now;
                    write_fence(&fences, request_id, &fence).is_ok()
                }
                Err(_) => false,
            };
            let removed = revoke_locked(&lock, request_id, conversation_id, manifest.generation);
            if fence_ok && removed {
                swept.removed.push(SecretRemoved {
                    request_id: request_id.to_string(),
                    conversation_id: conversation_id.to_string(),
                    cap: manifest.generation,
                    reason: "expired".into(),
                });
            } else {
                swept.failed.push(SecretRemoveFailed {
                    request_id: request_id.to_string(),
                    conversation_id: conversation_id.to_string(),
                    cap: manifest.generation,
                    reason: if removed { "fence" } else { "io" }.into(),
                });
            }
        }
        // Пустой каталог разговора убирается; непустой не трогаем ни при каком
        // локальном списке живых разговоров — иначе удаление разговора на хабе
        // не дождалось бы отзыва.
        if !failed_here && dir.dir.is_empty().unwrap_or(false) {
            let _ = unlinkat(lock.base.fd(), &name, AtFlags::REMOVEDIR);
        }
    }

    // Заборы без значения старше 25 часов: volatile-доставка не переживает
    // соединение, а срок жизни файла — сутки.
    let Ok(names) = fences.entries() else {
        return swept;
    };
    for name in names {
        if is_abandoned_temp(&name) {
            if unlinkat(fences.fd(), &name, AtFlags::empty()).is_ok() {
                swept.temps += 1;
            }
            continue;
        }
        let Some(request) = name.strip_suffix(".json") else {
            continue;
        };
        let Ok(request_id) = Uuid::parse_str(request) else {
            continue;
        };
        let Ok(Some(fence)) = read_fence(&fences, request_id) else {
            continue;
        };
        if now <= fence.max_expires_at_ms.saturating_add(FENCE_GRACE_MS) {
            continue;
        }
        if value_exists(&lock, fence.conversation_id, request_id) != Some(false) {
            continue;
        }
        if unlinkat(fences.fd(), &name, AtFlags::empty()).is_ok() {
            swept.fences += 1;
        }
    }
    swept
}

/// `Some(true/false)` — проверено; `None` — проверить не удалось. Прунинг забора
/// требует именно проверенного отсутствия значения.
fn value_exists(lock: &BaseLock, conversation_id: Uuid, request_id: Uuid) -> Option<bool> {
    let dir = ConversationDir::open_opt(&lock.base, &lock.root, conversation_id).ok()?;
    let Some(dir) = dir else {
        return Some(false);
    };
    entry_stat(dir.fd(), &value_name(request_id))
        .ok()
        .map(|stat| stat.is_some_and(|stat| is_regular(&stat)))
}

/// Факт доставки: только метаданные.
pub fn delivered_fact(request: &SecretDeliver, written: &Written) -> SecretDelivered {
    SecretDelivered {
        request_id: request.request_id.clone(),
        conversation_id: request.conversation_id.clone(),
        generation: request.generation,
        path: written.path.display().to_string(),
        expires_at_ms: written.expires_at_ms,
    }
}

/// Факт отказа: короткая причина без значения и без пути.
pub fn failed_fact(request: &SecretDeliver, failure: Failure) -> SecretFailed {
    SecretFailed {
        request_id: request.request_id.clone(),
        conversation_id: request.conversation_id.clone(),
        generation: request.generation,
        reason: failure.code().into(),
    }
}

/// Факт подтверждённого удаления: поколение, до которого убрано.
pub fn removed_fact(
    conversation_id: Uuid,
    request_id: Uuid,
    cap: u64,
    reason: &str,
) -> SecretRemoved {
    SecretRemoved {
        request_id: request_id.to_string(),
        conversation_id: conversation_id.to_string(),
        cap,
        reason: reason.to_owned(),
    }
}

/// Файл не убран: хаб оставит `revoking` с подписью и повторит отзыв.
pub fn remove_failed_fact(
    conversation_id: Uuid,
    request_id: Uuid,
    cap: u64,
    reason: &str,
) -> SecretRemoveFailed {
    SecretRemoveFailed {
        request_id: request_id.to_string(),
        conversation_id: conversation_id.to_string(),
        cap,
        reason: reason.to_owned(),
    }
}

/// Живые разговоры: списанные и остановленные исполнения не принимают доставку.
pub(crate) fn live_conversations(infos: &HashMap<Uuid, crate::exec::ExecInfo>) -> Vec<Uuid> {
    infos
        .values()
        .filter(|info| {
            !info.retired && !matches!(info.state.as_str(), "stopped" | "failed" | "owner_lost")
        })
        .map(|info| info.conversation_id)
        .collect()
}

/// Уборка при старте и раз в 10 минут, без ожидания хаба: просроченный файл и
/// брошенный временный не должны жить до первого переподключения.
pub(crate) async fn run_sweeper(
    config: SecretConfig,
    shared: std::sync::Arc<crate::exec::Shared>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(600));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        sweep_and_report(&config, &shared).await;
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow_and_update() {
                    return;
                }
            }
            _ = ticker.tick() => {}
        }
    }
}

async fn sweep_and_report(config: &SecretConfig, shared: &std::sync::Arc<crate::exec::Shared>) {
    let swept = sweep(config, now_ms());
    if !swept.removed.is_empty() || !swept.failed.is_empty() {
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
    }
    if !swept.removed.is_empty() || !swept.failed.is_empty() || swept.temps > 0 || swept.fences > 0
    {
        tracing::info!(
            kind = "secret_sweep",
            removed = swept.removed.len(),
            failed = swept.failed.len(),
            temps = swept.temps,
            fences = swept.fences,
            "secret files swept"
        );
    }
}

/// Запрет держать лишние открытые дескрипторы в тестах: число fd под замком
/// видно снаружи, и утечка была бы тихой.
#[cfg(test)]
fn open_fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|entries| entries.count())
        .unwrap_or(0)
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

    fn request(conversation: Uuid, request: Uuid, generation: u64, value: &str) -> SecretDeliver {
        SecretDeliver {
            request_id: request.to_string(),
            conversation_id: conversation.to_string(),
            value: value.as_bytes().to_vec(),
            generation,
            expires_at_ms: now_ms() + FILE_TTL.as_millis() as u64,
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

    fn fence_path(base: &Path, request: Uuid) -> PathBuf {
        base.join(".fences").join(format!("{request}.json"))
    }

    fn read_fence_file(base: &Path, request: Uuid) -> Fence {
        serde_json::from_slice(&std::fs::read(fence_path(base, request)).unwrap()).unwrap()
    }

    fn write_fence_file(base: &Path, request: Uuid, fence: &Fence) {
        std::fs::create_dir_all(base.join(".fences")).unwrap();
        std::fs::write(
            fence_path(base, request),
            serde_json::to_vec(fence).unwrap(),
        )
        .unwrap();
    }

    fn value_path(base: &Path, conversation: Uuid, request: Uuid) -> PathBuf {
        base.join(conversation.to_string())
            .join(request.to_string())
    }

    #[test]
    fn writes_exact_bytes_with_a_metadata_only_manifest_and_a_fence_outside_the_dir() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let value = "canterlot-secret-PROBE-строка\n";
        let written = deliver(
            &config,
            &request(conversation, request_id, 1, value),
            &live_set(conversation),
        )
        .expect("deliver");
        assert_eq!(std::fs::read(&written.path).unwrap(), value.as_bytes());
        let file = std::fs::metadata(&written.path).expect("file metadata");
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        let dir = std::fs::metadata(written.path.parent().unwrap()).expect("dir metadata");
        assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        assert_eq!(dir.uid(), uid());
        assert_eq!(written.conversation_id, conversation);

        let raw: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                written
                    .path
                    .parent()
                    .unwrap()
                    .join(format!("{request_id}.json")),
            )
            .expect("manifest"),
        )
        .unwrap();
        let mut keys: Vec<&String> = raw.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["expires_at_ms", "generation"]);
        assert!(
            !std::fs::read_to_string(
                written
                    .path
                    .parent()
                    .unwrap()
                    .join(format!("{request_id}.json"))
            )
            .unwrap()
            .contains("PROBE"),
            "the manifest is metadata only"
        );

        let fence = read_fence_file(&base.0, request_id);
        assert_eq!(fence.conversation_id, conversation);
        assert_eq!(fence.seen, 1);
        assert_eq!(fence.dead, 0);
        assert!(fence.updated_at > 0);
        assert!(
            !std::fs::read_to_string(fence_path(&base.0, request_id))
                .unwrap()
                .contains("PROBE"),
            "the fence is metadata only"
        );
        assert!(
            !written.path.parent().unwrap().join(".fences").exists(),
            "the fence lives outside the conversation directory"
        );
    }

    #[test]
    fn a_delayed_older_generation_after_a_newer_one_is_refused() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let newer = deliver(
            &config,
            &request(conversation, request_id, 2, "new\n"),
            &live_set(conversation),
        )
        .unwrap();
        let older = deliver(
            &config,
            &request(conversation, request_id, 1, "old\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(older, Failure::StaleAttempt);
        assert_eq!(std::fs::read(&newer.path).unwrap(), b"new\n");
    }

    #[test]
    fn a_duplicate_generation_is_idempotent_and_does_not_extend_the_expiry() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let first = deliver(
            &config,
            &request(conversation, request_id, 1, "first\n"),
            &live_set(conversation),
        )
        .unwrap();
        let mut replay = request(conversation, request_id, 1, "second\n");
        replay.expires_at_ms = first.expires_at_ms + 60_000;
        let again = deliver(&config, &replay, &live_set(conversation)).unwrap();
        assert_eq!(first.path, again.path);
        assert_eq!(first.expires_at_ms, again.expires_at_ms, "expiry stays");
        assert_eq!(std::fs::read(&first.path).unwrap(), b"first\n");
    }

    #[test]
    fn a_new_generation_replaces_the_value_and_an_old_replay_is_refused() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let first = deliver(
            &config,
            &request(conversation, request_id, 1, "old\n"),
            &live_set(conversation),
        )
        .unwrap();
        let second = deliver(
            &config,
            &request(conversation, request_id, 2, "new\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(first.path, second.path);
        assert_eq!(std::fs::read(&second.path).unwrap(), b"new\n");
        let replay = deliver(
            &config,
            &request(conversation, request_id, 1, "resurrected\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(replay, Failure::StaleAttempt);
        assert_eq!(std::fs::read(&second.path).unwrap(), b"new\n");
    }

    #[test]
    fn a_revoke_before_any_delivery_blocks_the_late_file() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Removed,
            "a verifiable absence confirms the revoke"
        );
        let failure = deliver(
            &config,
            &request(conversation, request_id, 1, "resurrected\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(failure, Failure::StaleAttempt);
        assert!(files_under(&base.0).iter().all(|path| {
            path.file_name()
                .is_none_or(|name| name != std::ffi::OsStr::new(&request_id.to_string()))
        }));
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Removed,
            "a repeated revoke is harmless"
        );
    }

    #[test]
    fn a_delivery_after_revoke_is_refused_and_the_value_stays_gone() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let written = deliver(
            &config,
            &request(conversation, request_id, 1, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Removed
        );
        assert!(!written.path.exists());
        let again = deliver(
            &config,
            &request(conversation, request_id, 1, "resurrected\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(again, Failure::StaleAttempt);
        assert!(!written.path.exists());
    }

    #[test]
    fn a_revoke_of_an_older_generation_leaves_a_newer_published_file_intact() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let older = deliver(
            &config,
            &request(conversation, request_id, 1, "old\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Removed
        );
        assert!(!older.path.exists());
        let newer = deliver(
            &config,
            &request(conversation, request_id, 2, "new\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(std::fs::read(&newer.path).unwrap(), b"new\n");
        // Отзыв до первого поколения не трогает второе: оно новее `up_to`.
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Removed
        );
        assert_eq!(
            std::fs::read(&newer.path).unwrap(),
            b"new\n",
            "a newer published generation survives an older cap"
        );
    }

    #[test]
    fn a_fence_write_failure_leaves_no_value_and_keeps_delivery_refused() {
        // Забор не записался: значение не публикуется, отзыв сообщает об ошибке,
        // а доставка остаётся запрещённой до успешного отзыва.
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        FAIL_NEXT_FENCE.with(|flag| flag.set(true));
        let failure = deliver(
            &config,
            &request(conversation, request_id, 1, "canterlot-secret-PROBE\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(failure, Failure::Io);
        assert!(
            files_under(&base.0).iter().all(|path| path
                .file_name()
                .is_some_and(|name| name != std::ffi::OsStr::new(&request_id.to_string()))),
            "no value or temp survives"
        );

        // Отзыв не смог записать забор до `up_to` (флаг снова срабатывает).
        FAIL_NEXT_FENCE.with(|flag| flag.set(true));
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Failed
        );
        assert!(is_removal_pending(request_id));
        let refused = deliver(
            &config,
            &request(conversation, request_id, 1, "late\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(refused, Failure::RemovalPending);
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Removed
        );
        assert!(!is_removal_pending(request_id));
        assert_eq!(read_fence_file(&base.0, request_id).dead, 1);
    }

    #[test]
    fn a_failed_value_write_leaves_no_temp_file_and_no_value() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        FAIL_NEXT_WRITE.with(|flag| flag.set(true));
        let failure = deliver(
            &config,
            &request(conversation, request_id, 1, "canterlot-secret-PROBE\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(failure, Failure::Io);
        let dir = base.0.join(conversation.to_string());
        assert!(
            !dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none(),
            "no value or temp survives in the conversation dir"
        );
        assert!(
            files_under(&base.0)
                .iter()
                .all(|path| !path.to_string_lossy().ends_with(".tmp")),
            "no temp file survives"
        );
        assert_eq!(read_fence_file(&base.0, request_id).seen, 1);
    }

    #[test]
    fn a_crash_between_the_fence_and_the_unlink_is_cleaned_by_recovery() {
        // Забор уже говорит «мертво», файл и манифест остались: восстановление
        // до приёма доставок обязано убрать байты.
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let written = deliver(
            &config,
            &request(conversation, request_id, 1, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        write_fence_file(
            &base.0,
            request_id,
            &Fence {
                conversation_id: conversation,
                seen: 1,
                dead: 1,
                updated_at: now_ms(),
                max_expires_at_ms: 0,
            },
        );
        let recovered = recover(&config);
        assert_eq!(recovered.values, 1);
        assert!(!written.path.exists());
        assert!(
            !written
                .path
                .parent()
                .unwrap()
                .join(format!("{request_id}.json"))
                .exists()
        );
    }

    #[test]
    fn a_crash_between_the_value_and_manifest_renames_is_cleaned_by_recovery() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        deliver(
            &config,
            &request(conversation, request_id, 1, "old\n"),
            &live_set(conversation),
        )
        .unwrap();
        // Неполная публикация второго поколения: забор уже видит 2, а манифест нет.
        let dir = base.0.join(conversation.to_string());
        std::fs::write(dir.join(request_id.to_string()), b"new\n").unwrap();
        write_fence_file(
            &base.0,
            request_id,
            &Fence {
                conversation_id: conversation,
                seen: 2,
                dead: 0,
                updated_at: now_ms(),
                max_expires_at_ms: 0,
            },
        );
        let recovered = recover(&config);
        assert_eq!(recovered.values, 1);
        assert!(!value_path(&base.0, conversation, request_id).exists());
    }

    #[test]
    fn recovery_removes_abandoned_temps_and_verifies_it() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        deliver(
            &config,
            &request(conversation, request_id, 1, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        let dir = base.0.join(conversation.to_string());
        std::fs::write(dir.join(format!(".{request_id}.2.tmp")), b"temp").unwrap();
        std::fs::write(dir.join(format!(".{request_id}.2.json.tmp")), b"temp").unwrap();
        std::fs::write(base.0.join(".fences/.fence-x.tmp"), b"temp").unwrap();
        let recovered = recover(&config);
        assert_eq!(recovered.temps, 3);
        assert_eq!(recovered.failed, 0);
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp"))
        );
    }

    #[test]
    fn an_inspection_failure_is_not_a_successful_removal() {
        // Нечитаемый каталог разговора — не доказательство отсутствия файла.
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let written = deliver(
            &config,
            &request(conversation, request_id, 1, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        let dir = written.path.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Failed,
            "the directory mode is not 0700: the inspection failed"
        );
        assert!(written.path.exists());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        // Нечитаемый забор: отзыв не подтверждается, файл всё равно убирается.
        let manifest = dir.join(format!("{request_id}.json"));
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::set_permissions(
            fence_path(&base.0, request_id),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Failed,
            "an unreadable fence is not proof of removal"
        );
        assert!(!written.path.exists(), "the value goes anyway");
        assert!(
            !manifest.exists(),
            "an unreadable manifest is retired together with the value"
        );
        std::fs::set_permissions(
            fence_path(&base.0, request_id),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Removed
        );
        assert!(!is_removal_pending(request_id));
    }

    #[test]
    fn an_expired_value_is_removed_without_a_hub_session() {
        // Уборка не ждёт хаба: срок в манифесте — единственное условие.
        let base = TestBase::new();
        let config = base.config();
        let conversation = Uuid::now_v7();
        let (expired, kept) = (Uuid::now_v7(), Uuid::now_v7());
        let old = deliver(
            &config,
            &request(conversation, expired, 1, "old\n"),
            &live_set(conversation),
        )
        .unwrap();
        let fresh = deliver(
            &config,
            &request(conversation, kept, 1, "fresh\n"),
            &live_set(conversation),
        )
        .unwrap();
        let manifest_path = old.path.parent().unwrap().join(format!("{expired}.json"));
        let mut manifest: Manifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest.expires_at_ms = 1;
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

        let swept = sweep(&config, 2);
        assert!(!old.path.exists());
        assert!(!manifest_path.exists());
        assert!(fresh.path.exists(), "a fresh file stays");
        assert_eq!(swept.removed.len(), 1);
        assert_eq!(swept.removed[0].reason, "expired");
        assert_eq!(swept.removed[0].cap, 1);
        assert_eq!(swept.failed.len(), 0);
        assert_eq!(read_fence_file(&base.0, expired).dead, 1);
        // После истечения поколение не воскресает.
        assert_eq!(
            deliver(
                &config,
                &request(conversation, expired, 1, "resurrected\n"),
                &live_set(conversation)
            )
            .unwrap_err(),
            Failure::StaleAttempt
        );
    }

    #[test]
    fn a_fence_survives_conversation_removal_and_a_restored_dir_cannot_resurrect_it() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let written = deliver(
            &config,
            &request(conversation, request_id, 1, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "session_end"
            ),
            Removal::Removed
        );
        std::fs::remove_dir_all(written.path.parent().unwrap()).unwrap();
        assert!(fence_path(&base.0, request_id).exists());
        // Восстановленный каталог: доставка того же поколения отвергается забором.
        std::fs::create_dir(written.path.parent().unwrap()).unwrap();
        std::fs::set_permissions(
            written.path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert_eq!(
            deliver(
                &config,
                &request(conversation, request_id, 1, "resurrected\n"),
                &live_set(conversation)
            )
            .unwrap_err(),
            Failure::StaleAttempt
        );
        assert!(!written.path.exists());
    }

    #[test]
    fn a_changed_root_wipes_the_old_values_before_it_is_recorded() {
        let data = tempfile::tempdir().unwrap();
        let first = TestBase::new();
        let second = TestBase::new();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        prepare_storage_with_default(&first.config(), data.path(), None).expect("first root");
        let old = deliver(
            &first.config(),
            &request(conversation, request_id, 1, "old\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(std::fs::read(&old.path).unwrap(), b"old\n");

        // Новый корень: старый не должен пережить смену, даже если отзыв до него
        // не дошёл (машина была offline).
        prepare_storage_with_default(&second.config(), data.path(), None).expect("second root");
        assert!(!old.path.exists(), "the old value is gone");
        assert!(
            !old.path.parent().unwrap().exists(),
            "the empty old conversation dir is gone"
        );
        let identity: serde_json::Value =
            serde_json::from_slice(&std::fs::read(data.path().join("secret-root.json")).unwrap())
                .unwrap();
        assert_eq!(identity["path"], second.0.display().to_string());

        // Новый корень принимает доставки.
        let fresh = deliver(
            &second.config(),
            &request(conversation, Uuid::now_v7(), 1, "new\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(std::fs::read(&fresh.path).unwrap(), b"new\n");
    }

    #[test]
    fn losing_the_identity_wipes_the_configured_and_default_roots() {
        let data = tempfile::tempdir().unwrap();
        let configured = TestBase::new();
        let default = TestBase::new();
        let (conversation, first, second) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        prepare_storage_with_default(&configured.config(), data.path(), Some(&default.0)).unwrap();
        let in_configured = deliver(
            &configured.config(),
            &request(conversation, first, 1, "configured\n"),
            &live_set(conversation),
        )
        .unwrap();
        let default_config = SecretConfig {
            base_dir: Some(default.0.clone()),
        };
        let in_default = deliver(
            &default_config,
            &request(conversation, second, 1, "default\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert!(in_configured.path.exists());
        assert!(in_default.path.exists());

        // Файл идентичности потерян: оба корня очищаются до записи новой.
        std::fs::remove_file(data.path().join("secret-root.json")).unwrap();
        prepare_storage_with_default(&configured.config(), data.path(), Some(&default.0))
            .expect("identity loss is recoverable");
        assert!(!in_configured.path.exists());
        assert!(!in_default.path.exists());
        assert!(data.path().join("secret-root.json").exists());
    }

    #[test]
    fn a_failed_wipe_keeps_the_new_root_disabled_until_it_succeeds() {
        let data = tempfile::tempdir().unwrap();
        let first = TestBase::new();
        let second = TestBase::new();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        prepare_storage_with_default(&first.config(), data.path(), None).unwrap();
        let old = deliver(
            &first.config(),
            &request(conversation, request_id, 1, "old\n"),
            &live_set(conversation),
        )
        .unwrap();
        let old_dir = old.path.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&old_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

        assert_eq!(
            prepare_storage_with_default(&second.config(), data.path(), None),
            Err(Failure::Io),
            "an unverifiable wipe is not a prepared root"
        );
        let late = request(conversation, Uuid::now_v7(), 1, "blocked\n");
        assert_eq!(
            deliver(&second.config(), &late, &live_set(conversation)).unwrap_err(),
            Failure::StorageDisabled
        );
        assert_eq!(
            revoke(
                &second.config(),
                conversation,
                late.request_id.parse().unwrap(),
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Failed
        );

        std::fs::set_permissions(&old_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        prepare_storage_with_default(&second.config(), data.path(), None)
            .expect("the wipe succeeds on a later start");
        assert!(!old.path.exists());
        let after = request(conversation, Uuid::now_v7(), 1, "blocked\n");
        let written = deliver(&second.config(), &after, &live_set(conversation)).unwrap();
        assert_eq!(std::fs::read(&written.path).unwrap(), b"blocked\n");
    }

    #[test]
    fn the_same_root_keeps_its_value_across_preparation() {
        let data = tempfile::tempdir().unwrap();
        let base = TestBase::new();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        prepare_storage_with_default(&base.config(), data.path(), None).unwrap();
        let written = deliver(
            &base.config(),
            &request(conversation, request_id, 1, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        prepare_storage_with_default(&base.config(), data.path(), None).expect("same root");
        assert_eq!(std::fs::read(&written.path).unwrap(), b"value\n");
        assert_eq!(read_fence_file(&base.0, request_id).seen, 1);
    }

    #[test]
    fn a_corrupt_root_identity_disables_deliveries() {
        let data = tempfile::tempdir().unwrap();
        let base = TestBase::new();
        std::fs::write(data.path().join("secret-root.json"), b"{not json").unwrap();
        assert_eq!(
            prepare_storage_with_default(&base.config(), data.path(), None),
            Err(Failure::Manifest)
        );
        let conversation = Uuid::now_v7();
        assert_eq!(
            deliver(
                &base.config(),
                &request(conversation, Uuid::now_v7(), 1, "value\n"),
                &live_set(conversation)
            )
            .unwrap_err(),
            Failure::StorageDisabled
        );
        std::fs::remove_file(data.path().join("secret-root.json")).unwrap();
        prepare_storage_with_default(&base.config(), data.path(), None).expect("a clean identity");
    }

    #[test]
    fn a_delivery_that_outlived_its_expiry_is_refused() {
        // Даже с чистым забором просроченные байты не публикуются: иначе уборка
        // забора воскресила бы значение, которое хаб уже считает истёкшим.
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let mut late = request(conversation, request_id, 1, "late\n");
        late.expires_at_ms = now_ms().saturating_sub(1);
        let failure = deliver(&config, &late, &live_set(conversation)).unwrap_err();
        assert_eq!(failure, Failure::Expired);
        assert_eq!(failure.code(), "expired");
        assert!(
            !base.0.join(conversation.to_string()).exists(),
            "no value or manifest is published"
        );
    }

    #[test]
    fn a_fence_is_not_pruned_before_its_last_expiry_plus_the_grace() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Removed,
            "a revoke before any delivery fences a future generation"
        );
        let fence = read_fence_file(&base.0, request_id);
        assert!(
            fence.max_expires_at_ms >= now_ms() + FENCE_TTL_MS - 5_000,
            "a revoked generation may still arrive with a full TTL"
        );

        let last = fence.max_expires_at_ms;
        let swept = sweep(&config, last + FENCE_GRACE_MS - 1_000);
        assert_eq!(swept.fences, 0);
        assert!(fence_path(&base.0, request_id).exists());

        let swept = sweep(&config, last + FENCE_GRACE_MS + 1_000);
        assert_eq!(swept.fences, 1);
        assert!(!fence_path(&base.0, request_id).exists());
    }

    #[test]
    fn a_revoke_carries_the_hub_expiry_so_clock_skew_cannot_prune_the_fence() {
        // Часы хаба впереди на 2 часа: срок поколения по часам воркера — +26 ч.
        // Отзыв несёт именно этот срок, поэтому забор нельзя убрать на +25 ч,
        // как было бы при горизонте «часы воркера + 24 ч».
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let now = now_ms();
        let hub_expiry = now + FENCE_TTL_MS + 2 * 60 * 60 * 1000;
        assert_eq!(
            revoke(&config, conversation, request_id, 1, hub_expiry, "revoked"),
            Removal::Removed
        );
        assert_eq!(
            read_fence_file(&base.0, request_id).max_expires_at_ms,
            hub_expiry
        );
        let swept = sweep(
            &config,
            now + FENCE_TTL_MS + 60 * 60 * 1000 + 30 * 60 * 1000,
        );
        assert_eq!(swept.fences, 0);
        assert!(
            fence_path(&base.0, request_id).exists(),
            "a 2h-ahead hub keeps the fence past worker-now + 25h"
        );
        let swept = sweep(&config, hub_expiry + FENCE_GRACE_MS + 1_000);
        assert_eq!(swept.fences, 1);
        assert!(!fence_path(&base.0, request_id).exists());
    }

    #[test]
    fn a_pruned_fence_cannot_resurrect_an_expired_delivery() {
        // Атака: отзыв до доставки, подтверждение, уборка забора, поздняя
        // доставка. Срок поздней доставки уже прошёл, поэтому она отвергается.
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Removed
        );
        let fence = read_fence_file(&base.0, request_id);
        let swept = sweep(&config, fence.max_expires_at_ms + FENCE_GRACE_MS + 1_000);
        assert_eq!(swept.fences, 1);
        assert!(
            !fence_path(&base.0, request_id).exists(),
            "the fence is gone"
        );

        let mut late = request(conversation, request_id, 1, "resurrected\n");
        late.expires_at_ms = now_ms().saturating_sub(1);
        assert_eq!(
            deliver(&config, &late, &live_set(conversation)).unwrap_err(),
            Failure::Expired
        );
        assert!(!value_path(&base.0, conversation, request_id).exists());
    }

    #[test]
    fn a_fence_is_pruned_only_after_25_hours_and_only_without_a_value() {
        let base = TestBase::new();
        let config = base.config();
        let conversation = Uuid::now_v7();
        let (dead, live) = (Uuid::now_v7(), Uuid::now_v7());
        deliver(
            &config,
            &request(conversation, dead, 1, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        assert_eq!(
            revoke(
                &config,
                conversation,
                dead,
                1,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Removed
        );
        let mut live_request = request(conversation, live, 1, "value\n");
        // Значение живёт дольше забора: иначе уборка снимет его по сроку и
        // проверка «забор со значением не прунится» ничего бы не показала.
        live_request.expires_at_ms = now_ms() + 3 * FENCE_TTL_MS;
        let live_written = deliver(&config, &live_request, &live_set(conversation)).unwrap();

        let now = now_ms();
        // Молодой забор не трогается.
        let swept = sweep(&config, now);
        assert_eq!(swept.fences, 0);
        assert!(fence_path(&base.0, dead).exists());

        // Старый забор без значения убирается; забор со значением — нет.
        let old = now + FENCE_TTL_MS + FENCE_GRACE_MS + 1_000;
        let swept = sweep(&config, old);
        assert_eq!(swept.fences, 1);
        assert!(!fence_path(&base.0, dead).exists());
        assert!(
            fence_path(&base.0, live).exists(),
            "a fence with a value survives"
        );
        assert!(live_written.path.exists());
    }

    #[test]
    fn sweep_removes_empty_conversation_dirs_but_never_a_non_empty_one() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let written = deliver(
            &config,
            &request(conversation, request_id, 1, "value\n"),
            &live_set(conversation),
        )
        .unwrap();
        // Непустой каталог живого разговора не убирается, даже если список живых пуст.
        let swept = sweep(&config, now_ms());
        assert!(swept.removed.is_empty());
        assert!(written.path.exists());
        assert!(written.path.parent().unwrap().exists());
        assert!(
            std::fs::read_dir(base.0.join(conversation.to_string()))
                .unwrap()
                .next()
                .is_some()
        );
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
        let failure = deliver(
            &config,
            &request(conversation, Uuid::now_v7(), 1, "value\n"),
            &live_set(conversation),
        )
        .unwrap_err();
        assert_eq!(failure, Failure::InsecureDirectory);
        assert_eq!(failure.code(), "insecure_directory");
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_base_outside_tmpfs_is_refused() {
        // Каталог репозитория обычно лежит на диске, а не на tmpfs: годной базой
        // он быть не может. Но рабочая копия может сама быть на tmpfs (например,
        // в /tmp), и тогда продукт прав — тест к такой среде неприменим.
        let directory = tempfile::Builder::new()
            .prefix("canterlot-secret-on-disk-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .unwrap();
        let probe = std::fs::File::open(directory.path()).unwrap();
        if fstatfs(std::os::fd::AsFd::as_fd(&probe)).is_ok_and(|stat| stat.f_type == TMPFS_MAGIC) {
            eprintln!(
                "skipping a_base_outside_tmpfs_is_refused: {} is on tmpfs",
                directory.path().display()
            );
            return;
        }
        let config = SecretConfig {
            base_dir: Some(directory.path().join("secrets")),
        };
        assert_eq!(open_base(&config).unwrap_err(), Failure::NotTmpfs);
        assert!(!directory.path().join("secrets").join(".lock").exists());
    }

    #[test]
    fn a_conversation_the_worker_does_not_run_is_refused() {
        let base = TestBase::new();
        let config = base.config();
        let conversation = Uuid::now_v7();
        let failure = deliver(
            &config,
            &request(conversation, Uuid::now_v7(), 1, "value\n"),
            &HashSet::new(),
        )
        .unwrap_err();
        assert_eq!(failure, Failure::UnknownConversation);
        assert_eq!(failure.code(), "unknown_conversation");
    }

    #[test]
    fn the_base_fd_returned_for_use_is_the_checked_tmpfs_directory() {
        let base = TestBase::new();
        let config = base.config();
        let (pinned, root) = open_base(&config).expect("checked base");
        assert_eq!(root, base.0);
        assert!(is_tmpfs(pinned.fd()));
        let stat = fstat(pinned.fd()).expect("base stat");
        assert_eq!(stat.st_uid, uid());
        assert_eq!(stat.st_mode & 0o777, 0o700);
    }

    #[test]
    fn the_epoch_names_the_boot_and_the_base_inode() {
        let base = TestBase::new();
        let config = base.config();
        let first = epoch(&config, "boot");
        let stat = std::fs::metadata(&base.0).unwrap();
        assert_eq!(first, format!("boot:{}", stat.ino()));
        assert_eq!(first, epoch(&config, "boot"), "the epoch is stable");
        assert_ne!(first, epoch(&config, "another-boot"));
    }

    #[test]
    fn the_tmpfs_message_never_holds_the_value() {
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let deliver = request(conversation, request_id, 1, "canterlot-secret-PROBE");
        let failed = failed_fact(&deliver, Failure::NotTmpfs);
        assert!(!format!("{failed:?}").contains("PROBE"));
        assert_eq!(failed.reason, "not_tmpfs");
        assert_eq!(failed.generation, 1);
    }

    #[test]
    fn operations_do_not_leak_descriptors() {
        let base = TestBase::new();
        let config = base.config();
        let (conversation, request_id) = (Uuid::now_v7(), Uuid::now_v7());
        let before = open_fd_count();
        for generation in 1..=3 {
            deliver(
                &config,
                &request(conversation, request_id, generation, "value\n"),
                &live_set(conversation),
            )
            .unwrap();
        }
        assert_eq!(
            revoke(
                &config,
                conversation,
                request_id,
                3,
                now_ms() + FENCE_TTL_MS,
                "revoked"
            ),
            Removal::Removed
        );
        let after = open_fd_count();
        assert!(after <= before + 2, "fd count {before} -> {after}");
    }
}
