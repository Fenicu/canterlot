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
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn spreads_requests_over_the_window() {
        let limiter = RateLimiter::new(RateLimit {
            max: 3,
            per: Duration::from_secs(1),
        });
        let start = Instant::now();
        let mut stamps = Vec::new();
        for _ in 0..7 {
            limiter.acquire("GET /a").await;
            stamps.push(start.elapsed());
        }
        for window in stamps.windows(4) {
            assert!(
                window[3] - window[0] >= Duration::from_secs(1),
                "four requests inside one second: {window:?}"
            );
        }
        // Первые три идут сразу.
        assert_eq!(stamps[2], Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn endpoints_are_limited_independently() {
        let limiter = RateLimiter::new(RateLimit {
            max: 1,
            per: Duration::from_secs(10),
        });
        let start = Instant::now();
        limiter.acquire("GET /a").await;
        limiter.acquire("GET /b").await;
        limiter.acquire("POST /a").await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        limiter.acquire("GET /a").await;
        assert_eq!(start.elapsed(), Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn default_is_twenty_per_second() {
        let limiter = RateLimiter::new(RateLimit::default());
        let start = Instant::now();
        for _ in 0..20 {
            limiter.acquire("GET /a").await;
        }
        assert_eq!(start.elapsed(), Duration::ZERO);
        limiter.acquire("GET /a").await;
        assert_eq!(start.elapsed(), Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_callers_share_the_window() {
        let limiter = std::sync::Arc::new(RateLimiter::new(RateLimit {
            max: 2,
            per: Duration::from_secs(1),
        }));
        let start = Instant::now();
        let tasks: Vec<_> = (0..6)
            .map(|_| {
                let limiter = limiter.clone();
                tokio::spawn(async move {
                    limiter.acquire("GET /a").await;
                    start.elapsed()
                })
            })
            .collect();
        let mut stamps = Vec::new();
        for task in tasks {
            stamps.push(task.await.unwrap());
        }
        stamps.sort();
        for window in stamps.windows(3) {
            assert!(
                window[2] - window[0] >= Duration::from_secs(1),
                "{stamps:?}"
            );
        }
    }
}
