//! Мягкая остановка сессий перед выключением машины.
//!
//! Воркер держит у systemd-logind задержку выключения (`Inhibit("shutdown", …, "delay")`).
//! По `PrepareForShutdown(true)` он останавливает живые исполнения с итогом
//! `machine_shutdown` и отпускает задержку, как только хаб подтвердил итоги, но не позже
//! чем за секунду до `InhibitDelayMaxUSec`: дольше logind всё равно не ждёт, а держать
//! замок до последнего значит опоздать с записью итога. Не успевшие исполнения systemd
//! убивает как раньше, и после загрузки они становятся `machine_rebooted`.
//!
//! Обычный перезапуск службы сюда не попадает: сигнала нет, сопровождения живут дальше.
//! Без logind (контейнеры, раннеры) или без разрешения polkit воркер пишет одну строку
//! в журнал и работает как раньше. При `shutdown_inhibit = false` модуль не запускается
//! и к системной шине никто не подключается.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use tokio::sync::watch;
use uuid::Uuid;

/// Срок, если logind не сказал свой: значение `InhibitDelayMaxSec` по умолчанию.
pub const DEFAULT_DELAY: Duration = Duration::from_secs(5);
/// Замок отпускается не позже чем за столько до срока logind.
pub const RELEASE_RESERVE: Duration = Duration::from_secs(1);
/// Сопровождения заканчивают раньше замка: остаток уходит на передачу итогов хабу.
pub const SHIP_RESERVE: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum LogindError {
    #[error("logind is not available: {0}")]
    Unavailable(String),
    #[error("logind refused the inhibitor: {0}")]
    Refused(String),
}

/// Держатель задержки выключения: задержка действует, пока он жив.
pub struct InhibitLock(#[allow(dead_code)] Box<dyn Send + Sync>);

impl InhibitLock {
    pub fn new(guard: impl Send + Sync + 'static) -> Self {
        Self(Box::new(guard))
    }
}

/// Задержка выключения и её сигнал. Боевая реализация — [`SystemLogind`]; тесты
/// подставляют свою и к системной шине не подключаются.
#[async_trait]
pub trait Logind: Send {
    async fn inhibit(&mut self) -> Result<InhibitLock, LogindError>;
    /// `InhibitDelayMaxUSec`: сколько logind ждёт держателей задержки.
    async fn delay(&mut self) -> Result<Duration, LogindError>;
    /// Следующий `PrepareForShutdown`; `None`, когда шина закрылась.
    async fn next_signal(&mut self) -> Option<bool>;
}

/// Что делает воркер с исполнениями; боевая реализация — `ExecutionManager`.
#[async_trait]
pub trait Drain: Send + Sync {
    /// Хаб последнего рукопожатия объявил `shutdown_stop_v1`.
    fn hub_accepts(&self) -> bool;
    /// Остановить живые исполнения со сроком `deadline` и придержать новые старты.
    /// Возвращает исполнения, которым остановка ушла.
    async fn begin(&self, deadline: Duration) -> Vec<Uuid>;
    /// Ждёт, пока хаб подтвердит итоги этих исполнений.
    async fn settled(&self, stopping: &[Uuid]);
    /// Выключение отменено: придержанные старты идут дальше.
    async fn cancelled(&self);
}

/// Цикл задержки выключения до остановки воркера или потери шины.
pub async fn run(
    mut logind: impl Logind,
    drain: Arc<dyn Drain>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut held = take(&mut logind).await;
    if held.is_none() {
        return;
    }
    loop {
        let signal = tokio::select! {
            biased;
            _ = stopped(&mut shutdown) => return,
            signal = logind.next_signal() => signal,
        };
        match signal {
            None => {
                tracing::warn!(
                    kind = "shutdown_inhibit_lost",
                    "logind bus closed; sessions are not stopped before shutdown"
                );
                return;
            }
            Some(true) => {
                let Some((lock, delay)) = held.take() else {
                    continue;
                };
                stop_before_shutdown(drain.as_ref(), delay).await;
                drop(lock);
                tracing::info!(
                    kind = "shutdown_inhibit_released",
                    "shutdown delay released"
                );
            }
            Some(false) => {
                tracing::info!(kind = "shutdown_cancelled", "machine shutdown cancelled");
                drain.cancelled().await;
                if held.is_none() {
                    held = take(&mut logind).await;
                    if held.is_none() {
                        return;
                    }
                }
            }
        }
    }
}

/// Остановка в пределах срока: всё, что не успело, остаётся на SIGKILL от systemd.
async fn stop_before_shutdown(drain: &dyn Drain, delay: Duration) {
    let budget = delay.saturating_sub(RELEASE_RESERVE);
    if !drain.hub_accepts() {
        tracing::info!(
            kind = "machine_shutdown",
            "hub does not accept machine_shutdown; leaving sessions as they are"
        );
        return;
    }
    tracing::info!(
        kind = "machine_shutdown",
        budget_ms = budget.as_millis() as u64,
        "machine is shutting down; stopping sessions"
    );
    let drained = tokio::time::timeout(budget, async {
        let stopping = drain.begin(budget.saturating_sub(SHIP_RESERVE)).await;
        drain.settled(&stopping).await;
        stopping.len()
    })
    .await;
    match drained {
        Ok(count) => {
            tracing::info!(kind = "machine_shutdown_settled", count, "sessions stopped")
        }
        Err(_) => tracing::warn!(
            kind = "machine_shutdown_overdue",
            "not every session settled before the shutdown deadline"
        ),
    }
}

/// Задержка и её срок; `None`, если logind недоступен или отказал.
async fn take(logind: &mut impl Logind) -> Option<(InhibitLock, Duration)> {
    let lock = match logind.inhibit().await {
        Ok(lock) => lock,
        Err(error) => {
            tracing::info!(
                kind = "shutdown_inhibit_unavailable",
                reason = %error,
                "sessions are not stopped before shutdown"
            );
            return None;
        }
    };
    let delay = logind.delay().await.unwrap_or(DEFAULT_DELAY);
    tracing::info!(
        kind = "shutdown_inhibit_taken",
        delay_ms = delay.as_millis() as u64,
        "shutdown delay taken"
    );
    Some((lock, delay))
}

async fn stopped(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LoginManager {
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> zbus::Result<zbus::OwnedFd>;

    #[zbus(signal)]
    fn prepare_for_shutdown(&self, start: bool) -> zbus::Result<()>;

    #[zbus(property, name = "InhibitDelayMaxUSec")]
    fn inhibit_delay_max_usec(&self) -> zbus::Result<u64>;
}

/// logind на системной шине.
pub struct SystemLogind {
    proxy: LoginManagerProxy<'static>,
    signals: PrepareForShutdownStream,
}

impl SystemLogind {
    /// Подключается к системной шине и подписывается на сигнал до того, как взять
    /// задержку: иначе выключение между ними прошло бы незамеченным.
    pub async fn connect() -> Result<Self, LogindError> {
        let unavailable = |error: zbus::Error| LogindError::Unavailable(error.to_string());
        let connection = zbus::Connection::system().await.map_err(unavailable)?;
        let proxy = LoginManagerProxy::new(&connection)
            .await
            .map_err(unavailable)?;
        let signals = proxy
            .receive_prepare_for_shutdown()
            .await
            .map_err(unavailable)?;
        Ok(Self { proxy, signals })
    }
}

#[async_trait]
impl Logind for SystemLogind {
    async fn inhibit(&mut self) -> Result<InhibitLock, LogindError> {
        let fd = self
            .proxy
            .inhibit("shutdown", "canterlot", "stopping agent sessions", "delay")
            .await
            .map_err(|error| LogindError::Refused(error.to_string()))?;
        Ok(InhibitLock::new(fd))
    }

    async fn delay(&mut self) -> Result<Duration, LogindError> {
        self.proxy
            .inhibit_delay_max_usec()
            .await
            .map(Duration::from_micros)
            .map_err(|error| LogindError::Unavailable(error.to_string()))
    }

    async fn next_signal(&mut self) -> Option<bool> {
        use tokio_stream::StreamExt;
        loop {
            let signal = self.signals.next().await?;
            match signal.args() {
                Ok(args) => return Some(args.start),
                Err(error) => tracing::warn!(
                    kind = "shutdown_signal_malformed",
                    reason = %error,
                    "PrepareForShutdown without a valid argument"
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests;
