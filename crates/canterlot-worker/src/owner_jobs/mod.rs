//! Задачи владельца без разговора: `canterlot-worker job run`.
//!
//! Команда идёт к воркеру по локальному сокету `jobs.sock` рядом с сокетом аренды (то же
//! доверие, что у `lease run`: каталог и сокет закрыты для чужих). Воркер носителя шлёт хабу
//! `OwnerJobSubmit`, хаб отвечает принятием или отказом, а итог приходит позже отдельной
//! командой `OwnerJobFinished`. Итог лежит файлом по `job_id` в `<data_dir>/owner-jobs/`:
//! `--wait` команды, запущенной после прихода итога или после рестарта воркера, всё равно
//! его получает.
//!
//! Протокол сокета — строки JSON `{"v":1,"op":...}` как у аренды.

pub mod client;
pub mod server;

use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use uuid::Uuid;

pub const SOCKET_NAME: &str = "jobs.sock";
pub const SOCKET_ENV: &str = "CANTERLOT_JOBS_SOCKET";
const STORE_DIR: &str = "owner-jobs";

/// Сокет задач владельца: `<runtime>/canterlot/jobs.sock`, без каталога времени выполнения —
/// `<data_dir>/jobs.sock`.
pub fn socket_path(runtime_dir: Option<&Path>, data_dir: &Path) -> PathBuf {
    match runtime_dir {
        Some(runtime) => runtime.join("canterlot").join(SOCKET_NAME),
        None => data_dir.join(SOCKET_NAME),
    }
}

/// Исходники задачи: репозиторий на этой машине и полный sha.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Source {
    pub repo: String,
    pub rev: String,
}

/// Описание задачи; поля называются как аргументы `job_run`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct JobSpec {
    pub title: String,
    /// Имя, UUID или `auto`; без ключа задача идёт на саму эту машину.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_minutes: Option<u32>,
    /// `none`, `build` или `gate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
    /// Команда оболочки.
    pub shell: String,
}

/// Итог задачи, как его прислал хаб.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Finished {
    pub job_id: String,
    /// `succeeded`, `failed`, `timed_out`, `canceled` или `lost`.
    pub state: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub tail: String,
    #[serde(default)]
    pub outputs_dir: String,
    #[serde(default)]
    pub outputs_note: String,
}

impl Finished {
    /// Код выхода `job run --wait`: код команды, 124 по таймауту, 130 при отмене, 1 для
    /// `failed` и `lost` без кода.
    pub fn exit_code(&self) -> i32 {
        match self.state.as_str() {
            "timed_out" => 124,
            "canceled" => 130,
            "succeeded" => self.exit_code.unwrap_or(0),
            _ => match self.exit_code {
                Some(code) if code != 0 => code,
                _ => 1,
            },
        }
    }
}

/// Отказ хаба или воркера принять задачу.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: String,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

/// Обмен с хабом: боевая реализация шлёт `OwnerJobSubmit` по потоку управления.
#[async_trait]
pub trait Hub: Send + Sync + 'static {
    /// `spec_json` — [`JobSpec`] как JSON; ответ — `OwnerJobAccepted` или `OwnerJobRefused`.
    async fn submit(&self, spec_json: String) -> Result<Uuid, Refusal>;
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ClientMessage {
    Submit { spec: Box<JobSpec> },
    Wait { job_id: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ServerMessage {
    Accepted { job_id: String },
    Refused { code: String, message: String },
    Finished(Finished),
}

pub use crate::lease::{decode, encode};

/// Итоги задач владельца на диске: по файлу `<job_id>.json`, запись целиком через переименование.
pub struct Store {
    dir: PathBuf,
    arrived: Notify,
}

impl Store {
    pub fn open(data_dir: &Path) -> io::Result<Arc<Self>> {
        let dir = data_dir.join(STORE_DIR);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        Ok(Arc::new(Self {
            dir,
            arrived: Notify::new(),
        }))
    }

    fn file(&self, job_id: &str) -> Option<PathBuf> {
        let id = Uuid::parse_str(job_id).ok()?;
        Some(self.dir.join(format!("{}.json", id.hyphenated())))
    }

    /// Запись итога. Повтор того же итога (повторная доставка команды) безвреден.
    pub fn record(&self, finished: &Finished) -> io::Result<()> {
        let path = self
            .file(&finished.job_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "job id is not a UUID"))?;
        let temporary = path.with_extension("json.tmp");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(finished).map_err(io::Error::other)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &path)?;
        self.arrived.notify_waiters();
        Ok(())
    }

    pub fn get(&self, job_id: &str) -> Option<Finished> {
        let bytes = fs::read(self.file(job_id)?).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Ждёт итог задачи сколько нужно. Итог, пришедший раньше вызова, возвращается сразу.
    pub async fn wait(&self, job_id: &str) -> Option<Finished> {
        loop {
            let arrived = self.arrived.notified();
            tokio::pin!(arrived);
            arrived.as_mut().enable();
            if let Some(found) = self.get(job_id) {
                return Some(found);
            }
            self.file(job_id)?;
            // Пробуждение теряется только при записи из другого процесса: раз в секунду
            // ожидание всё равно сверяется с диском.
            let _ = tokio::time::timeout(Duration::from_secs(1), arrived).await;
        }
    }

    /// Убирает итоги старше `retention`. Имена берутся из списка одного каталога.
    pub fn sweep(&self, retention: Duration, now: SystemTime) -> usize {
        let Ok(read) = fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut removed = 0;
        for entry in read.flatten() {
            let name = entry.file_name();
            let Some(text) = name.to_str() else { continue };
            let Some(stem) = text.strip_suffix(".json") else {
                continue;
            };
            if self.file(stem).is_none() {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let old = kind.is_file()
                && entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .is_ok_and(|modified| {
                        now.duration_since(modified)
                            .is_ok_and(|age| age >= retention)
                    });
            if old && fs::remove_file(self.dir.join(text)).is_ok() {
                removed += 1;
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(state: &str, code: Option<i32>) -> Finished {
        Finished {
            job_id: Uuid::now_v7().to_string(),
            state: state.into(),
            exit_code: code,
            ..Finished::default()
        }
    }

    #[test]
    fn exit_code_follows_state_and_code() {
        for (state, code, want) in [
            ("succeeded", Some(0), 0),
            ("succeeded", None, 0),
            ("failed", Some(3), 3),
            ("failed", None, 1),
            ("failed", Some(0), 1),
            ("lost", None, 1),
            ("timed_out", None, 124),
            ("timed_out", Some(137), 124),
            ("canceled", None, 130),
            ("canceled", Some(1), 130),
        ] {
            assert_eq!(finished(state, code).exit_code(), want, "{state} {code:?}");
        }
    }

    #[tokio::test]
    async fn store_keeps_results_across_instances_and_wakes_waiters() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let early = finished("failed", Some(2));
        store.record(&early).unwrap();
        // Итог, пришедший раньше ожидающего, и после «рестарта» воркера (новый `Store`).
        let reopened = Store::open(dir.path()).unwrap();
        assert_eq!(reopened.wait(&early.job_id).await, Some(early.clone()));
        let late = finished("succeeded", Some(0));
        let waiter = {
            let reopened = reopened.clone();
            let id = late.job_id.clone();
            tokio::spawn(async move { reopened.wait(&id).await })
        };
        // Ожидающий мог начать до записи или после: итог находится при любом порядке.
        tokio::task::yield_now().await;
        reopened.record(&late).unwrap();
        assert_eq!(waiter.await.unwrap(), Some(late));
        assert_eq!(store.wait("not-a-uuid").await, None);
    }

    #[test]
    fn sweep_removes_only_old_result_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let old = finished("succeeded", Some(0));
        let fresh = finished("succeeded", Some(0));
        store.record(&old).unwrap();
        store.record(&fresh).unwrap();
        let stranger = dir.path().join(STORE_DIR).join("notes.txt");
        fs::write(&stranger, "x").unwrap();
        // Условное «сейчас»: время изменения файлов задаётся явно от него.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        let aged = |id: &str, age: Duration| {
            fs::File::open(store.file(id).unwrap())
                .unwrap()
                .set_modified(now - age)
                .unwrap();
        };
        aged(&old.job_id, Duration::from_secs(9 * 24 * 3600));
        aged(&fresh.job_id, Duration::from_secs(60));
        assert_eq!(store.sweep(Duration::from_secs(7 * 24 * 3600), now), 1);
        assert!(store.get(&old.job_id).is_none());
        assert!(store.get(&fresh.job_id).is_some());
        assert!(stranger.exists());
    }
}
