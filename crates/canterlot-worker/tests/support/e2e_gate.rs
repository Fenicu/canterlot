use std::sync::OnceLock;

use tokio::sync::{Semaphore, SemaphorePermit};

const DEFAULT_PARALLEL: usize = 3;

static GATE: OnceLock<Semaphore> = OnceLock::new();

fn gate() -> &'static Semaphore {
    GATE.get_or_init(|| {
        let limit = match std::env::var("CANTERLOT_E2E_PARALLEL") {
            Ok(value) => value
                .parse()
                .ok()
                .filter(|limit| *limit > 0)
                .expect("CANTERLOT_E2E_PARALLEL должна быть положительным числом"),
            Err(_) => DEFAULT_PARALLEL,
        };
        Semaphore::new(limit)
    })
}

/// Допуск тяжёлого сценария: держать до конца теста, освобождается и при панике.
pub async fn acquire() -> SemaphorePermit<'static> {
    gate().acquire().await.unwrap()
}

/// Для синхронных тестов, которые запускают сценарий дочерним процессом.
#[allow(dead_code)]
pub fn acquire_blocking() -> SemaphorePermit<'static> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(acquire())
}
