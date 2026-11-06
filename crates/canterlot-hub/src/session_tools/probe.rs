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

use chrono::{DateTime, Utc};
use canterlot_proto::worker::v1::{self, path_probe};
use uuid::Uuid;

use crate::session::{ProbeReply, Registry};

/// Срок RPC: зависшая проба не держит ни задание, ни планировщик.
pub const RPC_DEADLINE: Duration = Duration::from_secs(10);
/// После срока к нему добавляется запас, прежде чем задать вопрос заново.
pub const STALE_GRACE: Duration = Duration::from_secs(2);
/// Столько байт файла несёт сообщение будильника (§3.2).
pub const ALARM_HEAD_BYTES: u32 = 256;

/// Что известно о пути. Ошибки — короткие коды воркера, не английский текст.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    match registry
        .probe_path(machine_id, path, read_head_bytes, RPC_DEADLINE)
        .await
    {
        ProbeReply::Answered(reply) => PathProbe::from(reply),
        ProbeReply::CapabilityMissing => PathProbe::CapabilityMissing,
        ProbeReply::Unavailable => PathProbe::Unavailable,
    }
}

struct Entry {
    started: Instant,
    result: Option<PathProbe>,
}

/// Ответы проб по заданию: запуск отдельной задачей и выборка следующим тиком.
#[derive(Default)]
pub struct Prefetch {
    entries: Mutex<HashMap<Uuid, Entry>>,
}

impl Prefetch {
    /// `true` — записи нет или она протухла: пробу надо запустить заново.
    pub fn claim(&self, job_id: Uuid) -> bool {
        let mut entries = self.entries.lock().expect("probe cache lock");
        match entries.get(&job_id) {
            None => {
                entries.insert(
                    job_id,
                    Entry {
                        started: Instant::now(),
                        result: None,
                    },
                );
                true
            }
            Some(entry)
                if entry.result.is_none()
                    && entry.started.elapsed() >= RPC_DEADLINE + STALE_GRACE =>
            {
                entries.insert(
                    job_id,
                    Entry {
                        started: Instant::now(),
                        result: None,
                    },
                );
                true
            }
            Some(_) => false,
        }
    }

    /// Забрать готовый ответ: запись снимается, задание применяет его в транзакции.
    pub fn take(&self, job_id: Uuid) -> Option<PathProbe> {
        self.entries
            .lock()
            .expect("probe cache lock")
            .remove(&job_id)
            .and_then(|entry| entry.result)
    }

    pub fn finish(&self, job_id: Uuid, result: PathProbe) {
        let mut entries = self.entries.lock().expect("probe cache lock");
        if let Some(entry) = entries.get_mut(&job_id) {
            entry.result = Some(result);
        }
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
        }
    }

    #[test]
    fn a_reply_becomes_size_mtime_and_an_optional_head() {
        let probe = PathProbe::from(exists(Some("0")));
        let PathProbe::Exists { size, mtime, head, .. } = probe else {
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
        };
        assert_eq!(PathProbe::from(reply), PathProbe::NotAllowed("denied".into()));
        let reply = v1::PathProbe {
            request_id: String::new(),
            outcome: Some(path_probe::Outcome::Error(v1::PathProbeError {
                reason: "unreadable".into(),
            })),
        };
        assert_eq!(PathProbe::from(reply), PathProbe::Error("unreadable".into()));
    }

    #[test]
    fn an_unfinished_probe_is_claimed_once_and_then_taken() {
        let cache = Prefetch::default();
        let job = Uuid::now_v7();
        assert!(cache.claim(job));
        assert!(!cache.claim(job));
        assert_eq!(cache.take(job), None);
        assert!(cache.claim(job));
        cache.finish(job, PathProbe::Missing);
        assert_eq!(cache.take(job), Some(PathProbe::Missing));
        assert_eq!(cache.take(job), None);
    }
}
