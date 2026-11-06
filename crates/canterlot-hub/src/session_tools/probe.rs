//! Проба пути на машине сессии: RPC управляющего канала со сроком.
//!
//! Задание планировщика не ждёт пробу в своей транзакции: планировщик запускает её
//! отдельной задачей (`Prefetch`), откладывает задание на секунду и продолжает
//! остальные. Готовый ответ забирается следующим тиком.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use canterlot_proto::worker::v1::{self, path_probe};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::session::{ProbeReply, Registry};

/// Срок RPC: зависшая проба не держит ни задание, ни планировщик.
pub const RPC_DEADLINE: Duration = Duration::from_secs(10);
/// После срока к нему добавляется запас, прежде чем задать вопрос заново.
pub const STALE_GRACE: Duration = Duration::from_secs(2);
/// Столько байт файла несёт сообщение будильника (§3.2).
pub const ALARM_HEAD_BYTES: u32 = 256;
/// Запись кэша без задания дольше этого не нужна: задание живёт секунды.
const PREFETCH_TTL: Duration = Duration::from_secs(120);
/// Потолок записей кэша: головы файлов не должны копиться без границы.
const PREFETCH_CAP: usize = 256;

/// Что известно о пути. Ошибки — короткие коды воркера, не английский текст.
#[derive(Clone, PartialEq, Eq)]
pub enum PathProbe {
    Exists {
        size: u64,
        mtime: Option<DateTime<Utc>>,
        head: Option<String>,
        /// Почему `head` пуст: `binary`, `not_requested` или `unreadable`.
        head_omitted: Option<String>,
    },
    Missing,
    NotAllowed(String),
    Error(String),
    /// Машина не в сеансе или не ответила за срок.
    Unavailable,
    /// Воркер без `path_probe_v1`.
    CapabilityMissing,
}

impl std::fmt::Debug for PathProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exists {
                size,
                mtime,
                head,
                head_omitted,
            } => f
                .debug_struct("Exists")
                .field("size", size)
                .field("mtime", mtime)
                .field("head_bytes", &head.as_ref().map_or(0, String::len))
                .field("head_omitted", head_omitted)
                .finish(),
            Self::Missing => f.write_str("Missing"),
            Self::NotAllowed(reason) => f.debug_tuple("NotAllowed").field(reason).finish(),
            Self::Error(reason) => f.debug_tuple("Error").field(reason).finish(),
            Self::Unavailable => f.write_str("Unavailable"),
            Self::CapabilityMissing => f.write_str("CapabilityMissing"),
        }
    }
}

impl PathProbe {
    /// Ответ, на котором условие ещё может выполниться: нет файла или связи.
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Unavailable | Self::CapabilityMissing | Self::Error(_)
        )
    }

    pub fn reason(&self) -> String {
        match self {
            Self::Exists { .. } => "exists".into(),
            Self::Missing => "missing".into(),
            Self::NotAllowed(reason) | Self::Error(reason) => reason.clone(),
            Self::Unavailable => "machine_unavailable".into(),
            Self::CapabilityMissing => "capability_missing".into(),
        }
    }
}

impl From<v1::PathProbe> for PathProbe {
    fn from(reply: v1::PathProbe) -> Self {
        match reply.outcome {
            Some(path_probe::Outcome::Exists(exists)) => Self::Exists {
                size: exists.size,
                mtime: i64::try_from(exists.mtime_ms)
                    .ok()
                    .and_then(DateTime::from_timestamp_millis),
                head: (!exists.head.is_empty()).then_some(exists.head),
                head_omitted: (!exists.head_omitted.is_empty()).then_some(exists.head_omitted),
            },
            Some(path_probe::Outcome::Missing(_)) => Self::Missing,
            Some(path_probe::Outcome::NotAllowed(not_allowed)) => {
                Self::NotAllowed(not_allowed.reason)
            }
            Some(path_probe::Outcome::Error(error)) => Self::Error(error.reason),
            None => Self::Error("empty_reply".into()),
        }
    }
}

/// Спросить машину сейчас. `Unavailable` вместо ошибки: вызывающий решает,
/// повторять или ждать срока.
pub async fn ask(
    registry: &Registry,
    machine_id: Uuid,
    path: &str,
    read_head_bytes: u32,
) -> PathProbe {
    ask_clock(registry, machine_id, path, read_head_bytes)
        .await
        .0
}

/// То же, но с часами воркера: граница взвода живёт в часовом домене машины.
pub async fn ask_clock(
    registry: &Registry,
    machine_id: Uuid,
    path: &str,
    read_head_bytes: u32,
) -> (PathProbe, Option<DateTime<Utc>>) {
    match registry
        .probe_path(machine_id, path, read_head_bytes, RPC_DEADLINE)
        .await
    {
        ProbeReply::Answered(reply) => {
            let worker_now = worker_clock(reply.now_ms);
            (PathProbe::from(reply), worker_now)
        }
        ProbeReply::CapabilityMissing => (PathProbe::CapabilityMissing, None),
        ProbeReply::Unavailable => (PathProbe::Unavailable, None),
    }
}

/// Ноль — старый воркер без часов: границу взвода взять неоткуда.
fn worker_clock(now_ms: u64) -> Option<DateTime<Utc>> {
    (now_ms != 0)
        .then(|| i64::try_from(now_ms).ok())
        .flatten()
        .and_then(DateTime::from_timestamp_millis)
}

struct Entry {
    /// Поколение попытки: ответ старой задачи не должен заполнить новую.
    generation: u64,
    started: Instant,
    result: Option<PathProbe>,
}

/// Убрать записи, которые уже некому забрать: по сроку и по потолку.
fn sweep(entries: &mut HashMap<Uuid, Entry>) {
    entries.retain(|_, entry| entry.started.elapsed() < PREFETCH_TTL);
    while entries.len() > PREFETCH_CAP {
        let Some(oldest) = entries
            .iter()
            .min_by_key(|(_, entry)| entry.started)
            .map(|(id, _)| *id)
        else {
            break;
        };
        entries.remove(&oldest);
    }
}

/// Ответы проб по заданию: запуск отдельной задачей и выборка следующим тиком.
#[derive(Default)]
pub struct Prefetch {
    entries: Mutex<HashMap<Uuid, Entry>>,
}

impl Prefetch {
    /// Номер поколения новой попытки: `None` — запись жива, второй пробы не надо.
    /// Протухшая незавершённая запись заменяется новым поколением.
    pub fn claim(&self, job_id: Uuid) -> Option<u64> {
        let mut entries = self.entries.lock().expect("probe cache lock");
        sweep(&mut entries);
        let claimed = match entries.get(&job_id) {
            None => {
                entries.insert(
                    job_id,
                    Entry {
                        generation: 1,
                        started: Instant::now(),
                        result: None,
                    },
                );
                Some(1)
            }
            Some(entry)
                if entry.result.is_none()
                    && entry.started.elapsed() >= RPC_DEADLINE + STALE_GRACE =>
            {
                let generation = entry.generation.wrapping_add(1);
                entries.insert(
                    job_id,
                    Entry {
                        generation,
                        started: Instant::now(),
                        result: None,
                    },
                );
                Some(generation)
            }
            Some(_) => None,
        };
        sweep(&mut entries);
        claimed
    }

    /// Забрать готовый ответ; незавершённая запись остаётся ждать свою задачу.
    pub fn take(&self, job_id: Uuid) -> Option<PathProbe> {
        let mut entries = self.entries.lock().expect("probe cache lock");
        if entries
            .get(&job_id)
            .is_some_and(|entry| entry.result.is_some())
        {
            return entries.remove(&job_id).and_then(|entry| entry.result);
        }
        None
    }

    /// Ответ принимается только от той попытки, которую запустили для задания.
    pub fn finish(&self, job_id: Uuid, generation: u64, result: PathProbe) {
        let mut entries = self.entries.lock().expect("probe cache lock");
        if let Some(entry) = entries.get_mut(&job_id)
            && entry.generation == generation
        {
            entry.result = Some(result);
        }
        sweep(&mut entries);
    }

    /// Задание закрыто или отменено: недоделанная запись больше не нужна.
    pub fn forget(&self, job_id: Uuid) {
        self.entries
            .lock()
            .expect("probe cache lock")
            .remove(&job_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exists(head: Option<&str>) -> v1::PathProbe {
        v1::PathProbe {
            request_id: Uuid::now_v7().to_string(),
            outcome: Some(path_probe::Outcome::Exists(v1::PathProbeExists {
                size: 2,
                mtime_ms: 1_762_345_260_000,
                head: head.unwrap_or_default().into(),
                head_omitted: String::new(),
            })),
            now_ms: 1_762_345_300_000,
        }
    }

    #[test]
    fn a_zero_worker_clock_means_unknown_not_the_epoch() {
        assert_eq!(worker_clock(0), None);
        assert_eq!(
            worker_clock(1_762_345_300_000),
            DateTime::from_timestamp_millis(1_762_345_300_000)
        );
    }

    #[test]
    fn debug_does_not_print_the_head() {
        let probe = PathProbe::Exists {
            size: 2,
            mtime: None,
            head: Some("secret-token".into()),
            head_omitted: None,
        };
        let text = format!("{probe:?}");
        assert!(!text.contains("secret-token"), "{text}");
        assert!(text.contains("head_bytes: 12"), "{text}");
    }

    #[test]
    fn a_reply_becomes_size_mtime_and_an_optional_head() {
        let probe = PathProbe::from(exists(Some("0")));
        let PathProbe::Exists {
            size, mtime, head, ..
        } = probe
        else {
            panic!()
        };
        assert_eq!(size, 2);
        assert!(mtime.is_some());
        assert_eq!(head.as_deref(), Some("0"));
        assert!(matches!(
            PathProbe::from(exists(None)),
            PathProbe::Exists { head: None, .. }
        ));
    }

    #[test]
    fn not_allowed_and_error_keep_their_codes() {
        let reply = v1::PathProbe {
            request_id: String::new(),
            outcome: Some(path_probe::Outcome::NotAllowed(v1::PathProbeNotAllowed {
                reason: "denied".into(),
            })),
            now_ms: 0,
        };
        assert_eq!(
            PathProbe::from(reply),
            PathProbe::NotAllowed("denied".into())
        );
        let reply = v1::PathProbe {
            request_id: String::new(),
            outcome: Some(path_probe::Outcome::Error(v1::PathProbeError {
                reason: "unreadable".into(),
            })),
            now_ms: 0,
        };
        assert_eq!(
            PathProbe::from(reply),
            PathProbe::Error("unreadable".into())
        );
    }

    #[test]
    fn an_unfinished_probe_is_claimed_once_and_then_taken() {
        let cache = Prefetch::default();
        let job = Uuid::now_v7();
        let generation = cache.claim(job).expect("first attempt");
        assert_eq!(
            cache.claim(job),
            None,
            "an unfinished probe is not re-polled"
        );
        assert_eq!(
            cache.take(job),
            None,
            "an unfinished entry stays for the running task"
        );
        cache.finish(job, generation, PathProbe::Missing);
        assert_eq!(cache.take(job), Some(PathProbe::Missing));
        assert_eq!(cache.take(job), None);
    }

    #[test]
    fn a_stale_attempt_does_not_fill_the_next_one() {
        let cache = Prefetch::default();
        let job = Uuid::now_v7();
        let first = cache.claim(job).expect("first attempt");
        // Попытка зависла дольше срока: её запись считается протухшей.
        {
            let mut entries = cache.entries.lock().unwrap();
            let entry = entries.get_mut(&job).unwrap();
            entry.started = Instant::now() - RPC_DEADLINE - STALE_GRACE;
        }
        let second = cache.claim(job).expect("stale attempt is replaced");
        assert_ne!(first, second);
        cache.finish(job, first, PathProbe::Error("late".into()));
        assert_eq!(
            cache.take(job),
            None,
            "a late reply of the old attempt is ignored"
        );
        cache.finish(job, second, PathProbe::Missing);
        assert_eq!(cache.take(job), Some(PathProbe::Missing));
    }

    #[test]
    fn an_unrelated_generation_does_not_fill_the_entry() {
        let cache = Prefetch::default();
        let job = Uuid::now_v7();
        let generation = cache.claim(job).expect("first attempt");
        cache.finish(job, generation + 7, PathProbe::Missing);
        assert_eq!(cache.take(job), None);
        cache.finish(job, generation, PathProbe::Missing);
        assert_eq!(cache.take(job), Some(PathProbe::Missing));
    }

    #[test]
    fn entries_left_without_a_job_expire() {
        let cache = Prefetch::default();
        let stale = Uuid::now_v7();
        {
            let mut entries = cache.entries.lock().unwrap();
            entries.insert(
                stale,
                Entry {
                    generation: 1,
                    started: Instant::now() - PREFETCH_TTL - Duration::from_secs(1),
                    result: None,
                },
            );
        }
        let _ = cache.claim(Uuid::now_v7());
        assert!(!cache.entries.lock().unwrap().contains_key(&stale));
    }

    #[test]
    fn the_cache_is_bounded() {
        let cache = Prefetch::default();
        for _ in 0..PREFETCH_CAP + 50 {
            let _ = cache.claim(Uuid::now_v7());
        }
        assert!(cache.entries.lock().unwrap().len() <= PREFETCH_CAP);
    }

    #[test]
    fn a_cancelled_job_drops_its_unfinished_entry() {
        let cache = Prefetch::default();
        let job = Uuid::now_v7();
        let _ = cache.claim(job).expect("attempt");
        cache.forget(job);
        assert!(!cache.entries.lock().unwrap().contains_key(&job));
    }
}
