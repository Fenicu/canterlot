//! Клиентский предел частоты: не больше `max` запросов за окно `per` на каждую конечную
//! точку. Окно скользящее, так что любые `max + 1` подряд идущих запросов к одной точке
//! растянуты не меньше чем на `per`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub max: u32,
    pub per: Duration,
}

impl Default for RateLimit {
    /// Предел Timeweb: 20 запросов в секунду на точку.
    fn default() -> Self {
        Self {
            max: 20,
            per: Duration::from_secs(1),
        }
    }
}

#[derive(Debug)]
pub struct RateLimiter {
    limit: RateLimit,
    sent: Mutex<HashMap<&'static str, VecDeque<Instant>>>,
}

impl RateLimiter {
    pub fn new(limit: RateLimit) -> Self {
        Self {
            limit,
            sent: Mutex::default(),
        }
    }

    /// Ждёт, пока запрос к точке `endpoint` можно отправить, и записывает его.
    pub async fn acquire(&self, endpoint: &'static str) {
        let max = usize::try_from(self.limit.max.max(1)).unwrap_or(usize::MAX);
        loop {
            let wait = {
                let mut sent = self
                    .sent
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let queue = sent.entry(endpoint).or_default();
                let now = Instant::now();
                while queue
                    .front()
                    .is_some_and(|at| now.duration_since(*at) >= self.limit.per)
                {
                    queue.pop_front();
                }
                if queue.len() < max {
                    queue.push_back(now);
                    return;
                }
                // Окно полно: освободится, когда выйдет самый старый запрос.
                queue
                    .front()
                    .map(|oldest| (*oldest + self.limit.per).saturating_duration_since(now))
                    .unwrap_or_default()
            };
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures_util::FutureExt as _;

    use super::*;

    /// Запрос проходит сразу, без ожидания. Незавершённое ожидание отбрасывается и ничего
    /// не записывает.
    fn passes_now(limiter: &RateLimiter, endpoint: &'static str) -> bool {
        limiter.acquire(endpoint).now_or_never().is_some()
    }

    fn limiter(max: u32, per: Duration) -> RateLimiter {
        RateLimiter::new(RateLimit { max, per })
    }

    #[tokio::test(start_paused = true)]
    async fn spreads_requests_over_the_window() {
        let limiter = limiter(3, Duration::from_secs(1));
        for _ in 0..3 {
            assert!(passes_now(&limiter, "GET /a"));
        }
        assert!(!passes_now(&limiter, "GET /a"), "fourth inside the window");
        tokio::time::advance(Duration::from_millis(999)).await;
        assert!(!passes_now(&limiter, "GET /a"), "still inside the window");
        tokio::time::advance(Duration::from_millis(1)).await;
        // Окно целиком вышло: снова три запроса сразу.
        for _ in 0..3 {
            assert!(passes_now(&limiter, "GET /a"));
        }
        assert!(!passes_now(&limiter, "GET /a"));
    }

    #[tokio::test(start_paused = true)]
    async fn the_window_slides_instead_of_resetting() {
        let limiter = limiter(2, Duration::from_secs(10));
        assert!(passes_now(&limiter, "GET /a"));
        tokio::time::advance(Duration::from_secs(6)).await;
        assert!(passes_now(&limiter, "GET /a"));
        assert!(!passes_now(&limiter, "GET /a"));
        // Первый запрос вышел из окна, второй ещё внутри: освободилось одно место.
        tokio::time::advance(Duration::from_secs(4)).await;
        assert!(passes_now(&limiter, "GET /a"));
        assert!(!passes_now(&limiter, "GET /a"));
    }

    #[tokio::test(start_paused = true)]
    async fn endpoints_are_limited_independently() {
        let limiter = limiter(1, Duration::from_secs(10));
        assert!(passes_now(&limiter, "GET /a"));
        assert!(passes_now(&limiter, "GET /b"));
        assert!(passes_now(&limiter, "POST /a"));
        assert!(!passes_now(&limiter, "GET /a"));
        tokio::time::advance(Duration::from_millis(9_999)).await;
        assert!(!passes_now(&limiter, "GET /a"));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(passes_now(&limiter, "GET /a"));
    }

    #[tokio::test(start_paused = true)]
    async fn default_is_twenty_per_second() {
        let limiter = RateLimiter::new(RateLimit::default());
        for _ in 0..20 {
            assert!(passes_now(&limiter, "GET /a"));
        }
        assert!(!passes_now(&limiter, "GET /a"));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(passes_now(&limiter, "GET /a"));
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_callers_are_released_a_window_at_a_time() {
        let limiter = Arc::new(limiter(2, Duration::from_secs(1)));
        let passed = Arc::new(AtomicUsize::new(0));
        for _ in 0..6 {
            let limiter = limiter.clone();
            let passed = passed.clone();
            tokio::spawn(async move {
                limiter.acquire("GET /a").await;
                passed.fetch_add(1, Ordering::SeqCst);
            });
        }
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(passed.load(Ordering::SeqCst), 2);
        for expected in [4, 6] {
            tokio::time::advance(Duration::from_secs(1)).await;
            for _ in 0..10 {
                tokio::task::yield_now().await;
            }
            assert_eq!(passed.load(Ordering::SeqCst), expected);
        }
    }
}
