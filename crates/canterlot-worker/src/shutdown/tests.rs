//! Цикл задержки выключения на поддельном logind: к системной шине тесты не подключаются.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use tokio::{
    sync::{mpsc, watch},
    time::timeout,
};

use super::*;

/// Отпущенная задержка сообщает свой номер.
struct Guard {
    id: usize,
    released: mpsc::UnboundedSender<usize>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.released.send(self.id);
    }
}

struct FakeLogind {
    signals: mpsc::UnboundedReceiver<bool>,
    released: mpsc::UnboundedSender<usize>,
    taken: Arc<AtomicUsize>,
    delay: Option<Duration>,
    refuse: bool,
}

#[async_trait]
impl Logind for FakeLogind {
    async fn inhibit(&mut self) -> Result<InhibitLock, LogindError> {
        if self.refuse {
            return Err(LogindError::Refused("denied by polkit".into()));
        }
        let id = self.taken.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(InhibitLock::new(Guard {
            id,
            released: self.released.clone(),
        }))
    }

    async fn delay(&mut self) -> Result<Duration, LogindError> {
        self.delay
            .ok_or_else(|| LogindError::Unavailable("no property".into()))
    }

    async fn next_signal(&mut self) -> Option<bool> {
        self.signals.recv().await
    }
}

#[derive(Default)]
struct FakeDrain {
    refuse_hub: bool,
    /// Итоги никогда не подтверждаются: хаб не отвечает или сопровождение молчит.
    never_settles: bool,
    deadlines: Mutex<Vec<Duration>>,
    cancelled: AtomicUsize,
}

#[async_trait]
impl Drain for FakeDrain {
    fn hub_accepts(&self) -> bool {
        !self.refuse_hub
    }

    async fn begin(&self, deadline: Duration) -> Vec<Uuid> {
        self.deadlines.lock().unwrap().push(deadline);
        vec![Uuid::nil()]
    }

    async fn settled(&self, _stopping: &[Uuid]) {
        if self.never_settles {
            std::future::pending::<()>().await;
        }
    }

    async fn cancelled(&self) {
        self.cancelled.fetch_add(1, Ordering::SeqCst);
    }
}

struct Rig {
    signals: mpsc::UnboundedSender<bool>,
    released: mpsc::UnboundedReceiver<usize>,
    taken: Arc<AtomicUsize>,
    drain: Arc<FakeDrain>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

fn rig(delay: Option<Duration>, refuse: bool, drain: FakeDrain) -> Rig {
    let (signals, signal_rx) = mpsc::unbounded_channel();
    let (released_tx, released) = mpsc::unbounded_channel();
    let taken = Arc::new(AtomicUsize::new(0));
    let drain = Arc::new(drain);
    let (stop, stop_rx) = watch::channel(false);
    let logind = FakeLogind {
        signals: signal_rx,
        released: released_tx,
        taken: taken.clone(),
        delay,
        refuse,
    };
    let task = tokio::spawn(run(logind, drain.clone(), stop_rx));
    Rig {
        signals,
        released,
        taken,
        drain,
        stop,
        task,
    }
}

/// Хаб не подтверждает итоги, сопровождение не отвечает: задержка всё равно отпускается
/// за секунду до срока logind, ни раньше, ни позже.
#[tokio::test(start_paused = true)]
async fn the_lock_is_released_a_second_before_the_logind_deadline_even_if_nothing_settles() {
    let mut rig = rig(
        Some(Duration::from_secs(5)),
        false,
        FakeDrain {
            never_settles: true,
            ..Default::default()
        },
    );
    rig.signals.send(true).unwrap();
    assert!(
        timeout(Duration::from_millis(3_999), rig.released.recv())
            .await
            .is_err(),
        "released before the deadline"
    );
    assert_eq!(
        timeout(Duration::from_millis(2), rig.released.recv()).await,
        Ok(Some(1))
    );
    assert_eq!(
        *rig.drain.deadlines.lock().unwrap(),
        vec![Duration::from_millis(3_500)],
        "supervisors finish half a second before the lock goes"
    );
}

/// Итоги подтверждены раньше срока: задержка отпускается сразу, выключение не ждёт.
#[tokio::test(start_paused = true)]
async fn the_lock_is_released_as_soon_as_every_session_settled() {
    let mut rig = rig(Some(Duration::from_secs(5)), false, FakeDrain::default());
    rig.signals.send(true).unwrap();
    assert_eq!(
        timeout(Duration::from_millis(1), rig.released.recv()).await,
        Ok(Some(1))
    );
}

/// Срок берётся из `InhibitDelayMaxUSec`: с drop-in на 15 секунд остановка получает 14.
#[tokio::test(start_paused = true)]
async fn the_budget_follows_the_configured_logind_delay() {
    let mut rig = rig(
        Some(Duration::from_secs(15)),
        false,
        FakeDrain {
            never_settles: true,
            ..Default::default()
        },
    );
    rig.signals.send(true).unwrap();
    assert!(
        timeout(Duration::from_millis(13_999), rig.released.recv())
            .await
            .is_err()
    );
    assert_eq!(
        timeout(Duration::from_millis(2), rig.released.recv()).await,
        Ok(Some(1))
    );
}

/// Свойство не прочиталось: срок по умолчанию, пять секунд.
#[tokio::test(start_paused = true)]
async fn an_unknown_delay_falls_back_to_five_seconds() {
    let mut rig = rig(
        None,
        false,
        FakeDrain {
            never_settles: true,
            ..Default::default()
        },
    );
    rig.signals.send(true).unwrap();
    assert!(
        timeout(Duration::from_millis(3_999), rig.released.recv())
            .await
            .is_err()
    );
    assert_eq!(
        timeout(Duration::from_millis(2), rig.released.recv()).await,
        Ok(Some(1))
    );
}

/// Старый хаб не знает `machine_shutdown`: сессии не трогаются, задержка уходит сразу.
#[tokio::test(start_paused = true)]
async fn a_hub_without_the_capability_releases_at_once_and_stops_nothing() {
    let mut rig = rig(
        Some(Duration::from_secs(5)),
        false,
        FakeDrain {
            refuse_hub: true,
            ..Default::default()
        },
    );
    rig.signals.send(true).unwrap();
    assert_eq!(
        timeout(Duration::from_millis(1), rig.released.recv()).await,
        Ok(Some(1))
    );
    assert!(rig.drain.deadlines.lock().unwrap().is_empty());
}

/// Выключение отменено: придержанные старты отпущены, задержка взята снова и снова
/// срабатывает на следующее выключение.
#[tokio::test(start_paused = true)]
async fn a_cancelled_shutdown_releases_held_starts_and_takes_the_lock_again() {
    let mut rig = rig(Some(Duration::from_secs(5)), false, FakeDrain::default());
    rig.signals.send(true).unwrap();
    assert_eq!(rig.released.recv().await, Some(1));
    rig.signals.send(false).unwrap();
    rig.signals.send(true).unwrap();
    assert_eq!(rig.released.recv().await, Some(2));
    assert_eq!(rig.drain.cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(rig.taken.load(Ordering::SeqCst), 2);
    assert_eq!(rig.drain.deadlines.lock().unwrap().len(), 2);
}

/// Без logind или без разрешения polkit цикл просто заканчивается: ни остановок, ни ошибок.
#[tokio::test(start_paused = true)]
async fn without_logind_nothing_happens() {
    let rig = rig(Some(Duration::from_secs(5)), true, FakeDrain::default());
    timeout(Duration::from_secs(1), rig.task)
        .await
        .expect("the loop ends")
        .unwrap();
    assert_eq!(rig.taken.load(Ordering::SeqCst), 0);
    assert!(rig.drain.deadlines.lock().unwrap().is_empty());
}

/// Остановка воркера (обычный перезапуск) снимает задержку и ничего не останавливает.
#[tokio::test(start_paused = true)]
async fn a_worker_stop_drops_the_lock_without_stopping_sessions() {
    let mut rig = rig(Some(Duration::from_secs(5)), false, FakeDrain::default());
    rig.stop.send_replace(true);
    timeout(Duration::from_secs(1), rig.task)
        .await
        .expect("the loop ends")
        .unwrap();
    assert_eq!(rig.released.recv().await, Some(1));
    assert!(rig.drain.deadlines.lock().unwrap().is_empty());
}

/// Шина закрылась: цикл заканчивается, задержка отпущена.
#[tokio::test(start_paused = true)]
async fn a_closed_bus_ends_the_loop() {
    let Rig {
        signals,
        mut released,
        task,
        ..
    } = rig(Some(Duration::from_secs(5)), false, FakeDrain::default());
    drop(signals);
    timeout(Duration::from_secs(1), task)
        .await
        .expect("the loop ends")
        .unwrap();
    assert_eq!(released.recv().await, Some(1));
}
