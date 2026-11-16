//! Watchdog пользовательской службы systemd и уведомление о готовности.
//!
//! `WATCHDOG=1` шлёт только главный цикл воркера, а не отдельный таймер:
//! если цикл завис, пинги прекращаются и systemd перезапускает службу.
//! Без `NOTIFY_SOCKET` (тесты, ручной запуск) всё это — no-op.

use std::{
    env,
    os::unix::net::UnixDatagram,
    path::PathBuf,
    time::{Duration, Instant},
};

pub struct SystemdWatchdog {
    socket: Option<UnixDatagram>,
    target: Option<PathBuf>,
    /// Половина `WATCHDOG_USEC`: чаще пинговать незачем.
    interval: Option<Duration>,
    last_ping: Option<Instant>,
}

impl SystemdWatchdog {
    pub fn from_env() -> Self {
        let target = env::var_os("NOTIFY_SOCKET")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let interval = env::var("WATCHDOG_USEC")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|micros| *micros >= 2)
            .map(|micros| Duration::from_micros(micros / 2));
        Self::new(target, interval)
    }

    fn new(target: Option<PathBuf>, interval: Option<Duration>) -> Self {
        let socket = target.as_ref().and_then(|_| UnixDatagram::unbound().ok());
        Self {
            socket,
            target,
            interval,
            last_ping: None,
        }
    }

    /// Готовность: после неё systemd ждёт первый пинг не дольше `WatchdogSec`.
    pub fn ready(&mut self) {
        self.notify("READY=1");
        self.last_ping = None;
    }

    /// Пинг из главного цикла; чаще половины `WATCHDOG_USEC` не отправляется.
    pub fn ping(&mut self) {
        let Some(interval) = self.interval else {
            return;
        };
        if self.last_ping.is_some_and(|last| last.elapsed() < interval) {
            return;
        }
        if self.notify("WATCHDOG=1") {
            self.last_ping = Some(Instant::now());
        }
    }

    fn notify(&self, message: &str) -> bool {
        let (Some(socket), Some(target)) = (&self.socket, &self.target) else {
            return false;
        };
        match socket.send_to(message.as_bytes(), target) {
            Ok(_) => true,
            Err(error) => {
                tracing::debug!(reason = %error, "systemd notify failed");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::net::UnixDatagram, thread::sleep};

    fn receive(socket: &UnixDatagram, timeout: Duration) -> Option<String> {
        socket
            .set_read_timeout(Some(timeout))
            .expect("read timeout");
        let mut bytes = [0u8; 128];
        let read = socket.recv(&mut bytes).ok()?;
        Some(String::from_utf8_lossy(&bytes[..read]).into_owned())
    }

    fn socket(directory: &tempfile::TempDir) -> (PathBuf, UnixDatagram) {
        let path = directory.path().join("notify");
        let socket = UnixDatagram::bind(&path).expect("bind notify socket");
        (path, socket)
    }

    fn short_directory() -> tempfile::TempDir {
        // Длина пути сокета укладывается в SUN_LEN даже в глубоком рабочем каталоге.
        tempfile::Builder::new()
            .prefix("wd")
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[test]
    fn fake_notify_socket_receives_ready_and_periodic_watchdog_pings() {
        let directory = short_directory();
        let (path, socket) = socket(&directory);
        let mut watchdog = SystemdWatchdog::new(Some(path), Some(Duration::from_millis(20)));

        watchdog.ready();
        assert_eq!(
            receive(&socket, Duration::from_secs(1)).as_deref(),
            Some("READY=1")
        );

        watchdog.ping();
        assert_eq!(
            receive(&socket, Duration::from_secs(1)).as_deref(),
            Some("WATCHDOG=1")
        );

        for _ in 0..2 {
            sleep(Duration::from_millis(30));
            watchdog.ping();
            assert_eq!(
                receive(&socket, Duration::from_secs(1)).as_deref(),
                Some("WATCHDOG=1"),
                "the loop keeps sending watchdog pings"
            );
        }
    }

    #[test]
    fn blocked_loop_stops_watchdog_pings() {
        let directory = short_directory();
        let (path, socket) = socket(&directory);
        let mut watchdog = SystemdWatchdog::new(Some(path), Some(Duration::from_millis(20)));

        watchdog.ready();
        assert_eq!(
            receive(&socket, Duration::from_secs(1)).as_deref(),
            Some("READY=1")
        );
        watchdog.ping();
        assert_eq!(
            receive(&socket, Duration::from_secs(1)).as_deref(),
            Some("WATCHDOG=1")
        );

        // Цикл «завис»: его больше никто не вызывает.
        sleep(Duration::from_millis(100));
        assert_eq!(receive(&socket, Duration::from_millis(50)), None);
    }

    #[test]
    fn without_notify_socket_everything_is_a_noop() {
        let mut watchdog = SystemdWatchdog::new(None, Some(Duration::from_millis(20)));
        watchdog.ready();
        watchdog.ping();
    }
}
