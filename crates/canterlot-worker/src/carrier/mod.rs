//! Носитель раннеров (`runners_v1`): воркер VM ведёт воркеров на статических раннерах по SSH.
//!
//! На каждый раннер, названный и в `[[runners.ssh]]`, и в `RunnersAssigned` хаба, носитель
//! держит: обратный проброс Unix-сокета к gRPC хаба, бинарник воркера нужной версии, токен
//! (выданный хабом ротацией и положенный по SSH без касания диска VM), сгенерированный
//! конфиг и сам процесс воркера. Раннер не требует ничего вне своего корня.
//!
//! Обмен с хабом спрятан за [`RunnerControl`]: токен запрашивается и наблюдения
//! отправляются через него, а список раннеров приходит каналом `watch`.

pub mod bridge;
mod collect;
pub mod commands;
mod runner;
pub mod script;
pub mod ssh;
pub mod stage;

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::{sync::watch, task::JoinHandle};
use uuid::Uuid;

use crate::config::{RunnerSsh, WorkerConfig};

/// Раннер, которого хаб поручил этому носителю.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssignedRunner {
    pub machine_id: Uuid,
    pub name: String,
    pub root_dir: PathBuf,
}

/// Токен раннера. Не печатается, не сериализуется и не клонируется: живёт от ответа хаба до
/// записи в stdin `ssh`.
pub struct SecretToken(String);

impl SecretToken {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretToken(..)")
    }
}

/// Ответ хаба на запрос токена: токен и идентификаторы для `worker.toml`.
#[derive(Debug)]
pub struct RunnerCredentials {
    pub machine_id: Uuid,
    pub installation_id: Uuid,
    pub restore_generation: Uuid,
    pub token: SecretToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenRefusal {
    NotCarrier,
    NotParent,
    Revoked,
    UnknownMachine,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TokenError {
    Refused(TokenRefusal),
    /// Нет связи с хабом или ответ не пришёл вовремя; спросить позже.
    Unavailable,
}

/// Достижимость раннера по SSH на момент `observed_at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub machine_id: Uuid,
    pub reachable: bool,
    pub consecutive_failures: u32,
    pub observed_at: SystemTime,
}

/// Обмен носителя с хабом.
#[async_trait]
pub trait RunnerControl: Send + Sync + 'static {
    /// Новый токен раннера ротацией; `reset_local_state` — раннер начинает с чистого состояния.
    async fn request_token(
        &self,
        machine_id: Uuid,
        reset_local_state: bool,
    ) -> Result<RunnerCredentials, TokenError>;

    /// Не блокирует надолго: при отсутствии связи наблюдение можно отбросить.
    async fn report_observation(&self, observation: Observation);
}

/// Сроки; боевые значения — по умолчанию, тесты сжимают их.
#[derive(Clone, Debug)]
pub struct Timings {
    /// Как часто проверять процесс раннера.
    pub liveness: Duration,
    /// Сколько `ssh -R` должен прожить, чтобы проброс считался поднятым.
    pub tunnel_settle: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// Наблюдение уходит хабу не реже этого.
    pub observe_every: Duration,
    /// Пауза между запросами токена по одному раннеру растёт от минимума к максимуму.
    pub token_retry_min: Duration,
    pub token_retry_max: Duration,
    /// Предел одной короткой команды по SSH.
    pub ssh_limit: Duration,
    /// Предел загрузки бинарника.
    pub upload_limit: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            liveness: Duration::from_secs(30),
            tunnel_settle: Duration::from_secs(2),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            observe_every: Duration::from_secs(60),
            token_retry_min: Duration::from_secs(30),
            token_retry_max: Duration::from_secs(600),
            ssh_limit: Duration::from_secs(60),
            upload_limit: Duration::from_secs(600),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CarrierSettings {
    pub hub_url: String,
    pub ca_cert: PathBuf,
    pub ssh_config: Option<PathBuf>,
    pub worker_binary: Option<PathBuf>,
    /// Только для тестов: хэш, под которым носитель объявляет свой бинарник. Расхождение с
    /// содержимым изображает отправку, которая дошла до раннера обрубком.
    pub worker_sha256: Option<String>,
    pub entries: Vec<RunnerSsh>,
    pub timings: Timings,
}

impl CarrierSettings {
    pub fn from_config(config: &WorkerConfig) -> Self {
        Self {
            hub_url: config.hub_url.clone(),
            ca_cert: config.ca_cert.clone(),
            ssh_config: config.runners.ssh_config.clone(),
            worker_binary: config.runners.worker_binary.clone(),
            worker_sha256: None,
            entries: config.runners.ssh.clone(),
            timings: Timings::default(),
        }
    }
}

/// Раннер по идентификатору машины для команд носителя: пересечение конфигурации и текущего
/// назначения хаба, как у присмотра (`desired`).
pub fn targets(
    entries: Vec<RunnerSsh>,
    assigned: watch::Receiver<Vec<AssignedRunner>>,
) -> stage::Targets {
    Arc::new(move |machine| {
        let list = assigned.borrow();
        let runner = list.iter().find(|runner| runner.machine_id == machine)?;
        let entry = entries.iter().find(|entry| entry.machine == runner.name)?;
        script::valid_root(&runner.root_dir).then(|| stage::RunnerTarget {
            alias: entry.ssh.clone(),
            root: runner.root_dir.clone(),
        })
    })
}

/// Бинарник, который носитель кладёт на раннеры: свой собственный.
#[derive(Clone, Debug)]
pub(crate) struct LocalBinary {
    pub path: PathBuf,
    pub sha256: String,
    pub version: Option<String>,
}

impl LocalBinary {
    fn load(path: &Path) -> std::io::Result<Self> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 20];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let version = std::process::Command::new(path)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| {
                String::from_utf8(output.stdout)
                    .ok()
                    .and_then(|text| text.lines().next().map(str::to_owned))
            });
        Ok(Self {
            path: path.to_path_buf(),
            sha256: hex(&hasher.finalize()),
            version,
        })
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Что нужно знать задаче одного раннера.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunnerSpec {
    pub machine_id: Uuid,
    pub name: String,
    pub root: PathBuf,
    pub alias: String,
    pub max_running: u32,
    pub lease_slots: u32,
}

/// Пересечение конфигурации и назначения хаба; раннер с негодным корнем пропускается.
fn desired(entries: &[RunnerSsh], assigned: &[AssignedRunner]) -> BTreeMap<String, RunnerSpec> {
    let mut specs = BTreeMap::new();
    for entry in entries {
        let Some(assigned) = assigned.iter().find(|runner| runner.name == entry.machine) else {
            continue;
        };
        if !script::valid_root(&assigned.root_dir) {
            tracing::warn!(
                runner = %assigned.machine_id,
                "runner root_dir is not an absolute normalized path of safe characters; runner skipped"
            );
            continue;
        }
        specs
            .entry(entry.machine.clone())
            .or_insert_with(|| RunnerSpec {
                machine_id: assigned.machine_id,
                name: entry.machine.clone(),
                root: assigned.root_dir.clone(),
                alias: entry.ssh.clone(),
                max_running: entry.max_running,
                lease_slots: entry.lease_slots,
            });
    }
    specs
}

/// Как задача раннера должна закончиться.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Run,
    /// Носитель выходит: проброс гасится, воркер раннера и его задачи остаются.
    Detach,
    /// Раннер ушёл из конфигурации или назначения: гасится и воркер.
    Remove,
}

struct Handle {
    spec: RunnerSpec,
    stop: watch::Sender<Stop>,
    join: JoinHandle<()>,
    /// Хаб отказал в токене: петли раннера остановлены до следующего списка.
    parked: Arc<AtomicBool>,
}

/// Ведёт раннеры, пока не придёт `shutdown`.
pub async fn run(
    settings: CarrierSettings,
    control: Arc<dyn RunnerControl>,
    mut assigned: watch::Receiver<Vec<AssignedRunner>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let worker_binary = settings.worker_binary.clone();
    let local = tokio::task::spawn_blocking(move || {
        let path = match worker_binary {
            Some(path) => path,
            None => std::env::current_exe()?,
        };
        LocalBinary::load(&path)
    })
    .await;
    let local = match local {
        Ok(Ok(mut local)) => {
            if let Some(sha256) = settings.worker_sha256.clone() {
                local.sha256 = sha256;
            }
            Some(Arc::new(local))
        }
        _ => {
            tracing::error!("carrier binary is unreadable; runner binaries will not be installed");
            None
        }
    };
    let ca_pem = match std::fs::read(&settings.ca_cert) {
        Ok(pem) => pem,
        Err(error) => {
            tracing::error!(kind = ?error.kind(), "carrier cannot read ca_cert; runners are not managed");
            return;
        }
    };
    let Some(target) = runner::hub_target(&settings.hub_url) else {
        tracing::error!(
            "carrier cannot derive the hub address from hub_url; runners are not managed"
        );
        return;
    };
    let shared = Arc::new(runner::Shared {
        hub_url: settings.hub_url.clone(),
        ssh_config: settings.ssh_config.clone(),
        timings: settings.timings.clone(),
        target,
        ca_sha: hex(&Sha256::digest(&ca_pem)),
        ca_pem,
        local,
        control,
    });
    let mut running: BTreeMap<String, Handle> = BTreeMap::new();
    while !*shutdown.borrow() {
        // Раннер, которого остановил отказ хаба, запускается заново со следующим списком.
        let parked: Vec<String> = running
            .iter()
            .filter(|(_, handle)| handle.parked.load(Ordering::SeqCst))
            .map(|(name, _)| name.clone())
            .collect();
        for name in parked {
            if let Some(handle) = running.remove(&name) {
                let _ = handle.join.await;
            }
        }
        running.retain(|_, handle| !handle.join.is_finished());
        let wanted = desired(&settings.entries, &assigned.borrow_and_update());
        let gone: Vec<String> = running
            .iter()
            .filter(|(name, handle)| wanted.get(*name) != Some(&handle.spec))
            .map(|(name, _)| name.clone())
            .collect();
        for name in gone {
            if let Some(handle) = running.remove(&name) {
                tracing::info!(runner = %handle.spec.machine_id, "runner leaves the carrier");
                let _ = handle.stop.send(Stop::Remove);
                let _ = handle.join.await;
            }
        }
        for (name, spec) in wanted {
            running.entry(name).or_insert_with(|| {
                tracing::info!(runner = %spec.machine_id, "runner joins the carrier");
                let (stop, stopped) = watch::channel(Stop::Run);
                let parked = Arc::new(AtomicBool::new(false));
                let join = tokio::spawn(runner::run(
                    spec.clone(),
                    shared.clone(),
                    stopped,
                    parked.clone(),
                ));
                Handle {
                    spec,
                    stop,
                    join,
                    parked,
                }
            });
        }
        tokio::select! {
            changed = assigned.changed() => {
                if changed.is_err() {
                    // Источник назначений закрыт: раннеры остаются как есть до выхода.
                    let _ = shutdown.wait_for(|stop| *stop).await;
                    break;
                }
            }
            _ = shutdown.changed() => {}
        }
    }
    for handle in running.values() {
        let _ = handle.stop.send(Stop::Detach);
    }
    for handle in running.into_values() {
        let _ = handle.join.await;
    }
}
