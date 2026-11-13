//! Живая диктовка: хаб пересылает звук из PWA службе `canterlot-speech`
//! по unix-сокету и возвращает распознанный текст, применяя словарь замен.

use std::{
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use crate::config::DictationConfig;

pub mod client;
pub mod dictionary;
pub mod frame;
pub mod terms;

pub use client::Health;

/// Сроки диктовки; умолчания — из контракта, тесты подставляют короткие.
#[derive(Clone, Debug)]
pub struct Timers {
    /// Первое сообщение после апгрейда.
    pub first_message: Duration,
    /// Соединение со службой речи и запрос `health`.
    pub speech_connect: Duration,
    /// Нет звука до `finish`.
    pub idle: Duration,
    /// От `start` до `listening`, включая очередь и холодную загрузку.
    pub start_deadline: Duration,
    /// Жизнь диктовки от `start`.
    pub lifetime: Duration,
    /// От `finish` до `final`.
    pub finish_final: Duration,
    /// Запись кадра службе или сообщения клиенту.
    pub write_stall: Duration,
    /// Срок показания `health` в `GET /api/v1/dictation/status`.
    pub status_cache: Duration,
}

impl Default for Timers {
    fn default() -> Self {
        Self {
            first_message: Duration::from_secs(5),
            speech_connect: Duration::from_secs(2),
            idle: Duration::from_secs(15),
            start_deadline: Duration::from_secs(30),
            lifetime: Duration::from_secs(11 * 60),
            finish_final: Duration::from_secs(60),
            write_stall: Duration::from_secs(5),
            status_cache: Duration::from_secs(10),
        }
    }
}

/// Звук не может опережать время с `start` больше чем на столько.
pub const MAX_LEAD: Duration = Duration::from_secs(5);
/// Байт звука в секунду: PCM s16le, 16 кГц, моно.
pub const AUDIO_BYTES_PER_SECOND: u64 = 32_000;
/// Занятый слот диктовки: поколение и сигнал замены новым `start`.
pub(crate) struct Slot {
    pub generation: u64,
    pub replaced: tokio_util::sync::CancellationToken,
}

/// Одна диктовка на владельца: сеанс устройства и поколение.
pub(crate) struct Active {
    pub session_id: uuid::Uuid,
    pub generation: u64,
    pub replaced: tokio_util::sync::CancellationToken,
}

/// Показание службы речи для статуса: доступна или нет.
#[derive(Clone, Debug)]
pub enum HealthState {
    Up(Health),
    Down,
}

/// Общее состояние хаба для диктовки: слот владельца и кэш `health`.
pub struct State {
    pub(crate) active: Mutex<Option<Active>>,
    generation: AtomicU64,
    health: tokio::sync::Mutex<Option<(Instant, HealthState)>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            active: Mutex::new(None),
            generation: AtomicU64::new(0),
            health: tokio::sync::Mutex::new(None),
        }
    }
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// Показание `health` не чаще раза в `timers.status_cache`; одновременные
    /// вызовы ждут один запрос, а не идут в службу каждый сам.
    pub async fn health(&self, config: &DictationConfig, timers: &Timers) -> HealthState {
        let mut cached = self.health.lock().await;
        if let Some((at, state)) = cached.as_ref()
            && at.elapsed() < timers.status_cache
        {
            return state.clone();
        }
        let state = match client::health(&config.socket, timers.speech_connect).await {
            Ok(health) => HealthState::Up(health),
            Err(client::NoService) => {
                tracing::info!(
                    kind = "speech_unavailable",
                    socket = %config.socket.display(),
                    "speech service is not reachable"
                );
                HealthState::Down
            }
        };
        *cached = Some((Instant::now(), state.clone()));
        state
    }

    /// Занять слот владельца. `Some(true)` — прежняя диктовка того же сеанса
    /// заменена, `None` — идёт диктовка другого сеанса.
    pub(crate) fn acquire(&self, session_id: uuid::Uuid) -> Result<Slot, uuid::Uuid> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = active.as_ref() {
            if current.session_id != session_id {
                return Err(current.session_id);
            }
            current.replaced.cancel();
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let replaced = tokio_util::sync::CancellationToken::new();
        *active = Some(Active {
            session_id,
            generation,
            replaced: replaced.clone(),
        });
        Ok(Slot {
            generation,
            replaced,
        })
    }

    /// Освободить слот, только если он всё ещё наш: уборка старой диктовки
    /// не снимает новую.
    pub(crate) fn release(&self, generation: u64) {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if active
            .as_ref()
            .is_some_and(|current| current.generation == generation)
        {
            *active = None;
        }
    }
}
