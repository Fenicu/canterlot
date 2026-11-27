use std::{
    fmt, fs, io,
    io::Write as _,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{cli::Version, error::CtlError};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct OpId(pub String);

impl OpId {
    pub fn new() -> Self {
        Self(Uuid::now_v7().to_string())
    }
}

impl Default for OpId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for OpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Traffic {
    Fenced,
    Admitted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OpKind {
    Install,
    Adopt,
    Update,
    Uninstall,
    TlsAddName,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OpResult {
    Done,
    FailedBeforeChange { reason: String },
    RolledBack { reason: String },
    NeedsOwner { steps: Vec<String> },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkerUpdate {
    pub machine_id: Uuid,
    pub version: Option<Version>,
    pub result: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DbIdentity {
    pub system_identifier: String,
    pub database_oid: u32,
    pub installation_id: Uuid,
    pub restore_generation: Uuid,
    pub owner_id: Uuid,
}

/// Подробности обновления хаба, нужные продолжению после сбоя: всё, что откат и проверка
/// восстановленной базы должны знать о состоянии до обновления.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct OpDetails {
    /// Строки `_sqlx_migrations` до обновления: «версия hex успех».
    #[serde(default)]
    pub migrations: Vec<String>,
    /// Число таблиц схемы `public` до обновления.
    #[serde(default)]
    pub tables: Option<u64>,
    /// Базы, которые создаёт и переименовывает откат этой операции.
    #[serde(default)]
    pub restore_db: Option<String>,
    #[serde(default)]
    pub restore_oid: Option<u32>,
    #[serde(default)]
    pub failed_db: Option<String>,
    /// Причина отката или отказа; печатается в отчёте.
    #[serde(default)]
    pub reason: Option<String>,
    /// Мягкие сбои после точки фиксации и ручная проверка для неоднозначного состояния.
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub manual: Vec<String>,
    /// Имя, которое добавляет `tls add-name`.
    #[serde(default)]
    pub name: Option<String>,
    /// Каталоги, созданные установкой: переживают прерванную попытку, чтобы повтор
    /// знал, что можно удалить `--purge`.
    #[serde(default)]
    pub created_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OpRecord {
    pub id: OpId,
    pub kind: OpKind,
    pub target: Option<Version>,
    pub previous: Option<Version>,
    pub db_identity: Option<DbIdentity>,
    pub dump: Option<PathBuf>,
    pub phase: String,
    pub traffic: Traffic,
    pub history: Vec<(DateTime<Utc>, String)>,
    pub result: Option<OpResult>,
    pub workers: Vec<WorkerUpdate>,
    #[serde(default)]
    pub details: OpDetails,
}

impl OpRecord {
    pub fn new(kind: OpKind, target: Option<Version>) -> Self {
        let now = Utc::now();
        Self {
            id: OpId::new(),
            kind,
            target,
            previous: None,
            db_identity: None,
            dump: None,
            phase: "begin".into(),
            traffic: Traffic::Admitted,
            history: vec![(now, "begin".into())],
            result: None,
            workers: Vec::new(),
            details: OpDetails::default(),
        }
    }
}

/// Хранит эксклюзивный flock на каталоге операций до окончания операции.
#[derive(Debug)]
pub struct HostLock {
    _file: fs::File,
}

impl Drop for HostLock {
    fn drop(&mut self) {
        let _ = flock(&self._file, FlockOperation::Unlock);
    }
}

/// Одна блокировка на хост. Занятость означает, что уже идёт операция: отдаём её запись.
pub fn lock(dir: &Path) -> Result<HostLock, CtlError> {
    fs::create_dir_all(dir)?;
    let file = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(".lock"))?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(HostLock { _file: file }),
        Err(err) => {
            let err: io::Error = err.into();
            if err.kind() == io::ErrorKind::WouldBlock {
                Err(CtlError::Busy(unfinished(dir)?))
            } else {
                Err(err.into())
            }
        }
    }
}

fn record_path(dir: &Path, id: &OpId) -> PathBuf {
    dir.join(format!("{}.json", id.0))
}

fn save(dir: &Path, rec: &OpRecord) -> Result<(), CtlError> {
    let bytes = serde_json::to_vec_pretty(rec)?;
    write_atomic(&record_path(dir, &rec.id), &bytes)
}

/// Новая операция возможна только когда предыдущая завершена.
pub fn begin(dir: &Path, rec: OpRecord) -> Result<OpRecord, CtlError> {
    if let Some(existing) = unfinished(dir)? {
        return Err(CtlError::Unfinished(existing));
    }
    save(dir, &rec)?;
    Ok(rec)
}

/// Фаза записывается до начала работы и сбрасывается на диск вместе с каталогом.
pub fn advance(dir: &Path, rec: &mut OpRecord, phase: &str) -> Result<(), CtlError> {
    rec.phase = phase.into();
    rec.history.push((Utc::now(), phase.into()));
    save(dir, rec)
}

/// Сохраняет изменённые подробности, не меняя фазу.
pub fn checkpoint(dir: &Path, rec: &OpRecord) -> Result<(), CtlError> {
    save(dir, rec)
}

pub fn finish(dir: &Path, rec: &mut OpRecord, result: OpResult) -> Result<(), CtlError> {
    rec.result = Some(result);
    rec.phase = "done".into();
    rec.history.push((Utc::now(), "done".into()));
    save(dir, rec)
}

fn records(dir: &Path) -> Result<Vec<OpRecord>, CtlError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut found: Vec<OpRecord> = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(path)?;
        found.push(serde_json::from_str(&text)?);
    }
    // Идентификатор uuid v7 сортируется по времени создания.
    found.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    Ok(found)
}

pub fn unfinished(dir: &Path) -> Result<Option<OpRecord>, CtlError> {
    Ok(records(dir)?
        .into_iter()
        .rev()
        .find(|rec| rec.result.is_none()))
}

/// Тождество живой базы из последней операции, после которой оно известно точно: с ним
/// сверяются повторная установка поверх сохранённых данных и каждое обновление. Откат
/// записывает тождество восстановленной базы (у неё другой oid), отказ до изменений —
/// прежнее; после `needs_owner` состояние базы решает владелец, такие записи пропускаются.
pub fn last_identity(dir: &Path) -> Result<Option<DbIdentity>, CtlError> {
    Ok(records(dir)?
        .into_iter()
        .rev()
        .filter(|rec| {
            matches!(
                rec.result,
                Some(
                    OpResult::Done
                        | OpResult::RolledBack { .. }
                        | OpResult::FailedBeforeChange { .. }
                )
            )
        })
        .find_map(|rec| rec.db_identity))
}

/// Все записи по возрастанию времени создания: хранение дампов и баз по ним.
pub fn all(dir: &Path) -> Result<Vec<OpRecord>, CtlError> {
    records(dir)
}

pub fn latest(dir: &Path) -> Result<Option<OpRecord>, CtlError> {
    Ok(records(dir)?.pop())
}

/// Атомарная замена файла: temp + fsync + rename + fsync каталога.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CtlError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("record");
    let tmp = parent.join(format!(".{name}.{}.tmp", Uuid::now_v7()));
    let mut file = fs::File::options()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version() -> Version {
        Version::parse("1.2.3").unwrap()
    }

    #[test]
    fn ops_begin_refuses_unfinished_record() {
        let dir = tempfile::tempdir().unwrap();
        let first = begin(dir.path(), OpRecord::new(OpKind::Install, Some(version()))).unwrap();
        let second = begin(dir.path(), OpRecord::new(OpKind::Update, None));
        match second {
            Err(CtlError::Unfinished(existing)) => assert_eq!(existing.id, first.id),
            other => panic!("expected unfinished, got {other:?}"),
        }
    }

    #[test]
    fn ops_advance_survives_crash() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = begin(dir.path(), OpRecord::new(OpKind::Update, Some(version()))).unwrap();
        advance(dir.path(), &mut rec, "candidate").unwrap();
        // Запись не завершена: имитируем смерть процесса.
        drop(rec);
        let found = unfinished(dir.path()).unwrap().expect("record");
        assert_eq!(found.id.0.len(), 36);
        assert_eq!(found.phase, "candidate");
        assert!(found.result.is_none());
        assert!(found.history.iter().any(|(_, phase)| phase == "candidate"));
    }

    #[test]
    fn lock_busy_returns_running_record() {
        let dir = tempfile::tempdir().unwrap();
        let rec = begin(dir.path(), OpRecord::new(OpKind::Install, Some(version()))).unwrap();
        let _guard = lock(dir.path()).unwrap();
        match lock(dir.path()) {
            Err(CtlError::Busy(Some(running))) => assert_eq!(running.id, rec.id),
            other => panic!("expected busy with record, got {other:?}"),
        }
    }

    #[test]
    fn finish_marks_record_complete() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = begin(dir.path(), OpRecord::new(OpKind::Install, None)).unwrap();
        finish(dir.path(), &mut rec, OpResult::Done).unwrap();
        assert!(unfinished(dir.path()).unwrap().is_none());
        let got = latest(dir.path()).unwrap().expect("record");
        assert_eq!(got.result, Some(OpResult::Done));
    }
}
