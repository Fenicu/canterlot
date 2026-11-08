//! Сторож машины: фоновый поток с наименьшим приоритетом меряет кэши сборки,
//! отчёт `MachineWatch` собирается из готового кэша и одного прохода по `/proc`.

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use canterlot_proto::worker::v1::{DiskHotspot, MachineWatch};

use super::{hotspots::HotspotCache, leaks};

/// Срок поиска кэшей за проход и срок замеров размеров.
const DISCOVER_BUDGET: Duration = Duration::from_secs(20);
const MEASURE_BUDGET: Duration = Duration::from_secs(300);
/// Короткий проход раз в 5 минут, полный перемер раз в час.
pub const SHORT_PASS: Duration = Duration::from_secs(300);
const FULL_PASS: Duration = Duration::from_secs(3600);
/// Короткоживущий воркер (тесты, перезапуск) диск не обходит.
const FIRST_PASS_DELAY: Duration = Duration::from_secs(60);

/// Что нужно от хранилища исполнений для правила утечки.
#[derive(Clone, Debug, Default)]
pub struct WatchRecords {
    /// Отпущенные записи принадлежности этого воркера.
    pub released: HashSet<String>,
    /// Живые участники исполнений: `(pid, start_ticks)`.
    pub live: HashSet<(u32, u64)>,
}

pub struct Watchdog {
    hotspots: Arc<Mutex<Vec<DiskHotspot>>>,
    worktree_root: PathBuf,
    roots: Vec<PathBuf>,
    started: Mutex<Option<mpsc::Sender<()>>>,
    proc_root: PathBuf,
}

impl Watchdog {
    pub fn new(worktree_root: PathBuf, hotspot_roots: Vec<PathBuf>) -> Self {
        Self {
            hotspots: Arc::default(),
            worktree_root,
            roots: hotspot_roots,
            started: Mutex::new(None),
            proc_root: PathBuf::from("/proc"),
        }
    }

    /// Поток замеров запускается при первом согласовании `watchdog_v1` и живёт, пока
    /// жив сторож: отправитель в нём, поток кончается, когда канал закрыт.
    pub fn ensure_started(&self) {
        let mut started = self.started.lock().expect("watch start lock");
        if started.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel::<()>();
        let hotspots = self.hotspots.clone();
        let mut cache = HotspotCache::new(self.worktree_root.clone(), self.roots.clone());
        let spawned = std::thread::Builder::new()
            .name("canterlot-watch".into())
            .spawn(move || {
                lower_priority();
                if matches!(
                    rx.recv_timeout(FIRST_PASS_DELAY),
                    Err(mpsc::RecvTimeoutError::Disconnected)
                ) {
                    return;
                }
                let mut full_at: Option<Instant> = None;
                loop {
                    let full = full_at.is_none_or(|at| at.elapsed() >= FULL_PASS);
                    if full {
                        full_at = Some(Instant::now());
                    }
                    cache.refresh(full, DISCOVER_BUDGET, MEASURE_BUDGET);
                    *hotspots.lock().expect("hotspots lock") = cache.hotspots();
                    if matches!(
                        rx.recv_timeout(SHORT_PASS),
                        Err(mpsc::RecvTimeoutError::Disconnected)
                    ) {
                        return;
                    }
                }
            });
        match spawned {
            Ok(_) => *started = Some(tx),
            Err(error) => tracing::warn!(reason = %error, "watchdog thread failed to start"),
        }
    }

    pub fn hotspots(&self) -> Vec<DiskHotspot> {
        self.hotspots.lock().expect("hotspots lock").clone()
    }

    /// Отчёт: кэш размеров плюс проход по процессам. Блокирующий: вызывать из
    /// `spawn_blocking`.
    pub fn observe(&self, records: &WatchRecords) -> MachineWatch {
        let (leaked, unleased_count) = match leaks::process_table(&self.proc_root) {
            Ok(all) => {
                let uptime = leaks::uptime_ticks(&self.proc_root).unwrap_or(0);
                let ticks = rustix::param::clock_ticks_per_second();
                (
                    leaks::leaked(&all, &records.released, &records.live, uptime, ticks),
                    leaks::unleased_count(&all),
                )
            }
            Err(error) => {
                tracing::warn!(reason = %error, "process scan failed");
                (Vec::new(), 0)
            }
        };
        MachineWatch {
            hotspots: self.hotspots(),
            leaked,
            unleased_count,
            observed_at_ms: super::now_ms(),
        }
    }
}

/// Наименьший приоритет процессора у этого потока. Ввод-вывод без своего класса
/// планировщик выводит из приоритета процессора (best-effort 7), отдельный
/// `ioprio_set` без небезопасного кода недоступен.
fn lower_priority() {
    let tid = rustix::thread::gettid();
    if let Err(error) = rustix::process::setpriority_process(Some(tid), 19) {
        tracing::debug!(reason = %error, "watchdog thread keeps its priority");
    }
}

/// Пересёк ли минимум свободного места порог вниз. Первый замер ниже порога тоже
/// пересечение: хаб мог открыть эпизод без отчёта.
pub fn crossed_below(previous_above: Option<bool>, free: u64, warn: u64) -> (bool, bool) {
    let above = free > warn;
    (!above && previous_above != Some(false), above)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossing_fires_once_per_descent() {
        let warn = 30;
        let (fire, above) = crossed_below(None, 40, warn);
        assert!(!fire);
        let (fire, above) = crossed_below(Some(above), 20, warn);
        assert!(fire);
        let (fire, above) = crossed_below(Some(above), 10, warn);
        assert!(!fire, "still below");
        let (fire, above) = crossed_below(Some(above), 35, warn);
        assert!(!fire);
        let (fire, _) = crossed_below(Some(above), 25, warn);
        assert!(fire, "second descent");
        assert!(crossed_below(None, 5, warn).0, "first sample below");
    }
}
