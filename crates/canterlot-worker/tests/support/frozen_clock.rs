use std::{future::Future, sync::mpsc, time::Duration};

use tokio::time::Instant;

const STALL: Duration = Duration::from_secs(30);

/// Выполняет тест на остановленных часах tokio: виртуальное время идёт только по `Timeline::at`.
/// Пока держится блокирующая задача, tokio не двигает часы сам, даже когда рантайм ждёт сеть или поток
/// хранилища. Тест обязан быть `#[tokio::test(start_paused = true)]`: тогда все моменты кратны миллисекунде.
/// Реальные часы нужны только как предохранитель от зависания.
pub async fn run<F: Future>(body: F) -> F::Output {
    let (release, held) = mpsc::channel::<()>();
    let stalled = tokio::task::spawn_blocking(move || held.recv_timeout(STALL));
    tokio::pin!(body);
    let output = tokio::select! {
        output = &mut body => output,
        _ = stalled => panic!("the test made no progress for {STALL:?} of real time"),
    };
    drop(release);
    output
}

/// Отсчёт виртуального времени от начала цикла сессии.
pub struct Timeline {
    origin: Instant,
}

impl Timeline {
    pub fn start() -> Self {
        Self {
            origin: Instant::now(),
        }
    }

    pub async fn at(&self, offset: Duration) {
        let elapsed = self.origin.elapsed();
        assert!(
            offset >= elapsed,
            "{offset:?} is already behind {elapsed:?}"
        );
        tokio::time::advance(offset - elapsed).await;
    }
}
