//! Отладка медленных тестов: `CANTERLOT_TEST_LOG=<фильтр>` включает журнал процесса теста в
//! stderr, `Steps` печатает время каждого шага. Без переменной ничего не пишется.

use std::time::Instant;

pub fn init() {
    let Ok(filter) = std::env::var("CANTERLOT_TEST_LOG") else {
        return;
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_thread_names(true)
        .try_init();
}

pub struct Steps {
    started: Instant,
    last: Instant,
    enabled: bool,
}

impl Steps {
    pub fn new() -> Self {
        init();
        let now = Instant::now();
        Self {
            started: now,
            last: now,
            enabled: std::env::var_os("CANTERLOT_TEST_LOG").is_some(),
        }
    }

    pub fn step(&mut self, what: &str) {
        let now = Instant::now();
        if self.enabled {
            eprintln!(
                "[step +{:>6} ms, total {:>6} ms] {what}",
                (now - self.last).as_millis(),
                (now - self.started).as_millis()
            );
        }
        self.last = now;
    }
}
