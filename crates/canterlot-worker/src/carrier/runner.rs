//! Задача одного раннера: проброс сокета, установка бинарника, токен, конфиг, запуск и
//! присмотр за процессом, наблюдения достижимости.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use canterlot_proto::worker::v1::{RunnerPrepareFailed, record_body::Body};
use tokio::{
    sync::watch,
    time::{MissedTickBehavior, interval, sleep},
};
use url::Url;

use crate::fatal;

use super::{
    CloudRunner, LocalBinary, Observation, RunnerControl, RunnerSpec, Stop, Timings, TokenError,
    cloud::{AttachOutcome, CloudState},
    script,
    script::{ConfigInputs, Ids, Mode, Probe},
    ssh::{Input, Output, Ssh, Unreachable},
};

/// Общее для всех раннеров носителя.
pub(super) struct Shared {
    pub hub_url: String,
    pub ssh_config: Option<PathBuf>,
    pub timings: Timings,
    pub egress_port: u16,
    /// `host:port` gRPC хаба для `-R`.
    pub target: String,
    pub ca_pem: Vec<u8>,
    pub ca_sha: String,
    pub local: Option<Arc<LocalBinary>>,
    pub control: Arc<dyn RunnerControl>,
    /// Итоги облачных подключений: короткие коды без секретов.
    pub cloud: Option<Arc<CloudState>>,
    /// Куда уходят факты подготовки облачного окна.
    pub facts: Option<Arc<dyn super::commands::FactSink>>,
}

/// Адрес хаба для обратного проброса, из `hub_url` самого носителя.
pub(super) fn hub_target(hub_url: &str) -> Option<String> {
    let url = Url::parse(hub_url).ok()?;
    let host = url.host_str()?;
    let port = url.port_or_known_default()?;
    Some(if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    })
}

/// Счёт неудач по SSH и отправка наблюдений.
struct Reach {
    machine_id: uuid::Uuid,
    control: Arc<dyn RunnerControl>,
    state: Mutex<ReachState>,
}

#[derive(Default)]
struct ReachState {
    reachable: Option<bool>,
    failures: u32,
    last_sent: Option<Instant>,
}

impl Reach {
    /// Итог одной попытки SSH: уходит хабу сразу.
    async fn record(&self, ok: bool) {
        let observation = {
            let mut state = self.state.lock().unwrap();
            state.failures = if ok {
                0
            } else {
                state.failures.saturating_add(1)
            };
            state.reachable = Some(ok);
            state.last_sent = Some(Instant::now());
            self.observation(&state)
        };
        if let Some(observation) = observation {
            self.control.report_observation(observation).await;
        }
    }

    /// Повтор последнего итога, если хаб давно ничего не слышал: свежесть — условие его «offline».
    async fn heartbeat(&self, every: Duration) {
        let observation = {
            let mut state = self.state.lock().unwrap();
            if state.last_sent.is_some_and(|sent| sent.elapsed() < every) {
                return;
            }
            state.last_sent = Some(Instant::now());
            self.observation(&state)
        };
        if let Some(observation) = observation {
            self.control.report_observation(observation).await;
        }
    }

    fn observation(&self, state: &ReachState) -> Option<Observation> {
        Some(Observation {
            machine_id: self.machine_id,
            reachable: state.reachable?,
            consecutive_failures: state.failures,
            observed_at: SystemTime::now(),
        })
    }
}

/// Пауза между запросами токена: растёт, пока воркер не живёт устойчиво.
struct TokenGate {
    min: Duration,
    max: Duration,
    interval: Duration,
    next_allowed: Instant,
}

impl TokenGate {
    fn new(timings: &Timings) -> Self {
        Self {
            min: timings.token_retry_min,
            max: timings.token_retry_max,
            interval: timings.token_retry_min,
            next_allowed: Instant::now(),
        }
    }

    fn allows(&self) -> bool {
        Instant::now() >= self.next_allowed
    }

    fn requested(&mut self) {
        self.next_allowed = Instant::now() + self.interval;
        self.interval = (self.interval * 2).min(self.max);
    }

    fn steady(&mut self) {
        self.interval = self.min;
        self.next_allowed = Instant::now();
    }
}

struct Runner {
    spec: RunnerSpec,
    shared: Arc<Shared>,
    ssh: Ssh,
    reach: Arc<Reach>,
    /// Сколько раз проброс поднимался: смена значения — повод заново сверить бинарник.
    connects: Arc<AtomicU64>,
    /// Успех облачного подключения сообщается один раз за жизнь задачи.
    started: AtomicBool,
    /// Предел подключения окна: им же ограничена подготовка.
    deadline: Option<Instant>,
    /// Подготовка окна: пока она не позвала, воркер не запускается.
    prepare: PrepareGate,
    halt: watch::Sender<Stop>,
    parked: Arc<AtomicBool>,
}

pub(super) async fn run(
    spec: RunnerSpec,
    shared: Arc<Shared>,
    stop: watch::Receiver<Stop>,
    parked: Arc<AtomicBool>,
) {
    let ssh = match &spec.cloud {
        Some(cloud) => Ssh::host(
            cloud.host.clone(),
            cloud.port,
            cloud.user.clone(),
            cloud.identity.clone(),
            cloud.known_hosts.clone(),
        ),
        None => Ssh::new(spec.alias.clone(), shared.ssh_config.clone()),
    };
    let reach = Arc::new(Reach {
        machine_id: spec.machine_id,
        control: shared.control.clone(),
        state: Mutex::default(),
    });
    let connects = Arc::new(AtomicU64::new(0));
    // Внутренний сигнал остановки: внешний пересылается в него, а отказ хаба в токене
    // останавливает обе петли раннера сам (`Detach`: воркер раннера остаётся как есть).
    let (halt, inner) = watch::channel(Stop::Run);
    let forward = tokio::spawn({
        let halt = halt.clone();
        let mut stop = stop;
        async move {
            while stop.changed().await.is_ok() {
                let value = *stop.borrow();
                halt.send_replace(value);
                if value != Stop::Run {
                    break;
                }
            }
        }
    });
    let deadline = spec
        .cloud
        .as_ref()
        .map(|cloud| Instant::now() + cloud.ready_timeout);
    let runner = Arc::new(Runner {
        spec,
        shared,
        ssh,
        reach,
        connects,
        started: AtomicBool::new(false),
        deadline,
        prepare: PrepareGate::default(),
        halt,
        parked,
    });
    if let Some(cloud) = &runner.spec.cloud
        && let Err(code) = runner.wait_ready(cloud).await
    {
        runner.cloud_fail(cloud, code);
        forward.abort();
        return;
    }
    let tunnel = tokio::spawn({
        let runner = runner.clone();
        let inner = inner.clone();
        async move { runner.tunnel_loop(inner).await }
    });
    runner.maintenance_loop(inner).await;
    let _ = tunnel.await;
    forward.abort();
}

impl Runner {
    fn root(&self) -> &std::path::Path {
        &self.spec.root
    }

    fn cloud_fail(&self, cloud: &CloudRunner, code: &'static str) {
        if let Some(state) = &self.shared.cloud {
            state.report(AttachOutcome::Failed {
                window_id: cloud.window_id.clone(),
                machine_id: self.spec.machine_id,
                code,
            });
        }
    }

    fn install_failed(&self) {
        if let Some(cloud) = &self.spec.cloud {
            self.cloud_fail(cloud, "install_failed");
        }
    }

    fn cloud_started(&self, cloud: &CloudRunner) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(state) = &self.shared.cloud {
            state.report(AttachOutcome::Started {
                window_id: cloud.window_id.clone(),
                machine_id: self.spec.machine_id,
            });
        }
    }

    /// Ждёт маркер готовности cloud-init с откатом до предельного срока. Закреплённый ключ
    /// хоста проверяется первым: при расхождении ничего не ставится и повтор не помогает.
    async fn wait_ready(&self, cloud: &CloudRunner) -> Result<(), &'static str> {
        let deadline = self.deadline.unwrap_or_else(Instant::now);
        let marker = cloud.ready_marker.display().to_string();
        let mut backoff = self.shared.timings.backoff_min;
        let mut connected = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(if connected {
                    "ready_timeout"
                } else {
                    "ssh_unreachable"
                });
            }
            let limit = self.shared.timings.ssh_limit.min(remaining);
            match self.ssh.raw(&["test", "-f", &marker], limit).await {
                Ok(output) if output.code == 0 => return Ok(()),
                Ok(output) if Ssh::host_key_rejected(&output) => return Err("host_key_mismatch"),
                Ok(_) => connected = true,
                Err(Unreachable) => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(if connected {
                    "ready_timeout"
                } else {
                    "ssh_unreachable"
                });
            }
            tokio::time::sleep(backoff.min(remaining)).await;
            backoff = (backoff * 2).min(self.shared.timings.backoff_max);
        }
    }

    /// Одна короткая команда: итог попытки сразу идёт в наблюдения.
    async fn call(&self, remote: &[&str], input: Input<'_>) -> Result<Output, Unreachable> {
        self.tracked(self.ssh.run(remote, input, self.shared.timings.ssh_limit))
            .await
    }

    async fn script(&self, text: &str) -> Result<Output, Unreachable> {
        self.tracked(self.ssh.script(text, self.shared.timings.ssh_limit))
            .await
    }

    async fn tracked(
        &self,
        attempt: impl Future<Output = Result<Output, Unreachable>>,
    ) -> Result<Output, Unreachable> {
        let result = attempt.await;
        self.reach.record(result.is_ok()).await;
        result
    }

    // --- проброс ---

    async fn tunnel_loop(&self, mut stop: watch::Receiver<Stop>) {
        let timings = &self.shared.timings;
        let socket = self.root().join("hub.sock");
        let mut backoff = timings.backoff_min;
        loop {
            if *stop.borrow() != Stop::Run {
                return;
            }
            let lived = self.tunnel_once(&socket, &mut stop).await;
            if *stop.borrow() != Stop::Run {
                return;
            }
            // Долго державшийся проброс после обрыва поднимается сразу, а не по накопленной паузе.
            if lived >= Duration::from_secs(30).min(timings.backoff_max) {
                backoff = timings.backoff_min;
            }
            tokio::select! {
                _ = sleep(backoff) => {}
                _ = stop.changed() => {}
            }
            backoff = (backoff * 2).min(timings.backoff_max);
        }
    }

    /// Одна попытка: каталог, чистка старого сокета, `ssh -R`. Возвращает, сколько проброс прожил.
    async fn tunnel_once(
        &self,
        socket: &std::path::Path,
        stop: &mut watch::Receiver<Stop>,
    ) -> Duration {
        let make = script::make_root(self.root());
        let clean = script::remove_stale_socket(self.root());
        for command in [&make, &clean] {
            match self.call(&[command], Input::None).await {
                Ok(output) if output.code == 0 => {}
                Ok(output) => {
                    tracing::warn!(runner = %self.spec.machine_id, code = output.code,
                        "runner socket preparation failed");
                    return Duration::ZERO;
                }
                Err(Unreachable) => return Duration::ZERO,
            }
        }
        let mut child = match self.ssh.forward(
            socket,
            &self.shared.target,
            self.spec.cloud.is_some().then_some(self.shared.egress_port),
        ) {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(runner = %self.spec.machine_id, kind = ?error.kind(), "ssh could not be started");
                self.reach.record(false).await;
                return Duration::ZERO;
            }
        };
        let started = Instant::now();
        tokio::select! {
            _ = child.wait() => {
                // Проброс не поднялся: нет связи, отказ ключа или сокет занят.
                self.reach.record(false).await;
                return started.elapsed();
            }
            _ = sleep(self.shared.timings.tunnel_settle) => {}
            _ = stop.changed() => {
                let _ = child.kill().await;
                return started.elapsed();
            }
        }
        self.reach.record(true).await;
        self.connects.fetch_add(1, Ordering::SeqCst);
        if let Some(cloud) = &self.spec.cloud {
            // Окно с подготовкой готово только после неё: воркер стартует отдельной петлёй
            // лишь тогда, когда отметка подготовки пущена.
            if cloud.prepare_script.is_empty() || self.prepare(cloud, stop).await {
                self.cloud_started(cloud);
            }
        }
        tracing::info!(runner = %self.spec.machine_id, "runner tunnel is up");
        tokio::select! {
            _ = child.wait() => {
                self.reach.record(false).await;
                tracing::warn!(runner = %self.spec.machine_id, "runner tunnel dropped");
            }
            _ = stop.changed() => {
                let _ = child.kill().await;
            }
        }
        started.elapsed()
    }

    // --- подготовка окна ---

    /// Скрипт подготовки: один раз на окно, от root, с окружением выхода в сеть, после того
    /// как проброс с SOCKS поднят, и до запуска воркера. Отметки на сервере переживают
    /// рестарт носителя, поэтому повтор скрипта не запускает.
    async fn prepare(&self, cloud: &CloudRunner, stop: &mut watch::Receiver<Stop>) -> bool {
        match self.prepare.state() {
            Prepared::Ready => return true,
            Prepared::Failed => return false,
            Prepared::Pending => {}
        }
        let failed = cloud.prepare_failed_marker.display().to_string();
        match self.wait_marker(cloud, &failed, stop).await {
            Marker::Exists => {
                // Прошлый запуск не досказал факт: отметка хранит код и хвост.
                let read = self.call(&["cat", &failed], Input::None).await;
                let (code, tail) = match read {
                    Ok(output) if output.code == 0 => parse_failure(&output.stdout),
                    _ => (-1, String::new()),
                };
                return self.prepare_failed(cloud, code, &tail).await;
            }
            Marker::Missing => {}
            Marker::Deadline => {
                let _ = self.kill_prepare(cloud).await;
                return self.prepare_failed(cloud, -1, "").await;
            }
            Marker::Stopped => return false,
        }
        let prepared = cloud.prepared_marker.display().to_string();
        match self.wait_marker(cloud, &prepared, stop).await {
            Marker::Exists => {
                self.prepare.set(Prepared::Ready);
                return true;
            }
            Marker::Missing => {}
            Marker::Deadline => {
                let _ = self.kill_prepare(cloud).await;
                return self.prepare_failed(cloud, -1, "").await;
            }
            Marker::Stopped => return false,
        }
        let remaining = self
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_default();
        if remaining.is_zero() {
            let _ = self.kill_prepare(cloud).await;
            return self.prepare_failed(cloud, -1, "").await;
        }
        let command = script::prepare_run(&cloud.sudo, &cloud.egress_env, &cloud.prepare_group);
        let remote = [command.as_str()];
        let attempt = self.ssh.until(
            &remote,
            Input::Bytes(cloud.prepare_script.as_bytes()),
            remaining,
        );
        tokio::pin!(attempt);
        let result = tokio::select! {
            result = &mut attempt => result,
            _ = stop.changed() => {
                // Остановка носителя посреди подготовки: скрипт не доигрывает и не пойдёт
                // заново, а окно считается неудачным.
                let _ = self.kill_prepare(cloud).await;
                return self.prepare_failed(cloud, -1, "").await;
            }
        };
        match result {
            Ok(Some(output)) if output.code == 0 => self.prepared(cloud).await,
            Ok(Some(output)) => {
                let tail = tail_of(&output.stdout, &output.stderr);
                self.prepare_failed(cloud, output.code, &tail).await
            }
            // Срок вышел: снимаем удалённый скрипт, вывод не собран.
            Ok(None) => {
                let _ = self.kill_prepare(cloud).await;
                self.prepare_failed(cloud, -1, "").await
            }
            Err(Unreachable) => self.prepare_failed(cloud, 255, "").await,
        }
    }

    /// Проверка отметки с повтором: обрыв SSH до запуска скрипта ещё ничего не решает, окно
    /// может успеть. Решает только срок; до него проба повторяется с паузой носителя.
    async fn wait_marker(
        &self,
        cloud: &CloudRunner,
        path: &str,
        stop: &mut watch::Receiver<Stop>,
    ) -> Marker {
        let mut backoff = self.shared.timings.backoff_min;
        loop {
            let simulated = cloud.marker_probe_failures.as_ref().is_some_and(|left| {
                left.0
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                        value.checked_sub(1)
                    })
                    .is_ok()
            });
            if !simulated {
                match self.call(&["test", "-f", path], Input::None).await {
                    Ok(output) if output.code == 0 => return Marker::Exists,
                    Ok(_) => return Marker::Missing,
                    Err(Unreachable) => {}
                }
            }
            let remaining = self
                .deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_default();
            if remaining.is_zero() {
                return Marker::Deadline;
            }
            tokio::select! {
                _ = sleep(backoff.min(remaining)) => {}
                _ = stop.changed() => {
                    if *stop.borrow() != Stop::Run {
                        return Marker::Stopped;
                    }
                }
            }
            backoff = (backoff * 2).min(self.shared.timings.backoff_max);
        }
    }

    /// Отметка успеха: без неё рестарт носителя повторил бы скрипт.
    async fn prepared(&self, cloud: &CloudRunner) -> bool {
        let marker = script::prepare_marker(&cloud.sudo, &cloud.prepared_marker);
        let wrote = match self.call(&[&marker], Input::None).await {
            Ok(output) => output.code == 0,
            Err(Unreachable) => false,
        };
        let _ = self.clean_pid(cloud).await;
        if !wrote {
            return self.prepare_failed(cloud, 255, "").await;
        }
        self.prepare.set(Prepared::Ready);
        tracing::info!(runner = %self.spec.machine_id, "cloud runner is prepared");
        true
    }

    async fn prepare_failed(&self, cloud: &CloudRunner, code: i32, tail: &str) -> bool {
        self.prepare.set(Prepared::Failed);
        let path = cloud.prepare_failed_marker.clone();
        let body = format!("{code}\n{tail}");
        let command = script::prepare_marker_write(&cloud.sudo, &path);
        let _ = self.call(&[&command], Input::Bytes(body.as_bytes())).await;
        let _ = self.clean_pid(cloud).await;
        self.journal_prepare_failed(cloud, code, tail).await;
        tracing::warn!(runner = %self.spec.machine_id, code, "cloud runner preparation failed");
        self.cloud_fail(cloud, "prepare_failed");
        false
    }

    async fn journal_prepare_failed(&self, cloud: &CloudRunner, code: i32, tail: &str) {
        let Some(facts) = &self.shared.facts else {
            return;
        };
        let body = Body::RunnerPrepareFailed(RunnerPrepareFailed {
            window_id: cloud.window_id.clone(),
            machine_id: self.spec.machine_id.to_string(),
            exit_code: code,
            output_tail: tail.to_owned(),
        });
        if facts.facts(vec![body], vec![]).await.is_err() {
            tracing::warn!(
                kind = "prepare_fact_not_written",
                runner = %self.spec.machine_id,
                "the preparation fact could not be journaled"
            );
        }
    }

    async fn kill_prepare(&self, cloud: &CloudRunner) -> Result<Output, Unreachable> {
        let command = script::prepare_kill(&cloud.sudo, &cloud.prepare_group);
        self.call(&[&command], Input::None).await
    }

    async fn clean_pid(&self, cloud: &CloudRunner) -> Result<Output, Unreachable> {
        let command = script::prepare_clean(&cloud.sudo, &cloud.prepare_group);
        self.call(&[&command], Input::None).await
    }

    // --- присмотр ---

    async fn maintenance_loop(&self, mut stop: watch::Receiver<Stop>) {
        let timings = &self.shared.timings;
        let mut ticker = interval(timings.liveness);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut beat = interval(timings.observe_every);
        beat.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut state = Maintenance {
            gate: TokenGate::new(timings),
            need_full: true,
            seen_connects: 0,
            alive_streak: 0,
        };
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    let connects = self.connects.load(Ordering::SeqCst);
                    if connects != state.seen_connects {
                        state.seen_connects = connects;
                        state.need_full = true;
                    }
                    if self.sync(&mut state).await.is_err() {
                        // Связи нет: попытка уже учтена, следующий круг повторит.
                        if !self.started.load(Ordering::SeqCst)
                            && let Some(cloud) = &self.spec.cloud
                        {
                            self.cloud_fail(cloud, "ssh_unreachable");
                        }
                        state.alive_streak = 0;
                    }
                }
                _ = beat.tick() => self.reach.heartbeat(timings.observe_every).await,
                _ = stop.changed() => {
                    if *stop.borrow() != Stop::Run {
                        break;
                    }
                }
            }
        }
        if *stop.borrow() == Stop::Remove {
            // Раннер ушёл: воркер останавливается, пока SSH ещё есть.
            let _ = self.script(&script::stop(self.root())).await;
            tracing::info!(runner = %self.spec.machine_id, "runner worker stopped");
        }
    }

    /// Один круг присмотра. `Err` только при потере связи.
    async fn sync(&self, state: &mut Maintenance) -> Result<(), Unreachable> {
        if let Some(cloud) = &self.spec.cloud
            && !cloud.prepare_script.is_empty()
            && self.prepare.state() != Prepared::Ready
        {
            // Подготовка ещё идёт или не удалась: воркер не запускается, скрипт не повторяется.
            return Ok(());
        }
        let layout = self.script(&script::layout(self.root())).await?;
        if layout.code != 0 {
            tracing::warn!(runner = %self.spec.machine_id, "runner layout could not be created");
            return Ok(());
        }
        let full = state.need_full;
        let output = self.script(&script::probe(self.root(), full)).await?;
        let probe = Probe::parse(&output.stdout, full);
        if !probe.root_exists {
            return Ok(());
        }
        // Нужен ли после этого круга запуск или перезапуск воркера.
        let mut restart = false;
        let mut stopped = !probe.alive;

        // Бинарник и CA.
        if let (true, Some(local)) = (full, &self.shared.local) {
            let remote_sha = probe.bin_sha.as_deref().unwrap_or("");
            let remote_version = probe.bin_version.as_deref().unwrap_or("");
            let version_differs = local
                .version
                .as_deref()
                .is_some_and(|version| !remote_version.is_empty() && remote_version != version);
            if remote_sha != local.sha256 || version_differs {
                tracing::info!(runner = %self.spec.machine_id, "runner binary is replaced");
                if !self.upload_binary(local).await? {
                    self.install_failed();
                    return Ok(());
                }
                restart = true;
            }
        }
        if probe.ca_sha != self.shared.ca_sha {
            let put = script::receive_file(self.root(), "ca.pem", Mode::Plain);
            let sent = self
                .call(&[&put], Input::Bytes(&self.shared.ca_pem))
                .await?;
            if sent.code != 0 {
                self.install_failed();
                tracing::warn!(runner = %self.spec.machine_id, "ca.pem could not be installed");
                return Ok(());
            }
            restart = true;
        }
        // Полная сверка считается сделанной только после бинарника и CA: оборванная отправка
        // выходит выше с `return`, `need_full` остаётся, и следующий круг сверяет хэш заново.
        if full {
            state.need_full = false;
        }

        // Токен и конфиг.
        // Любое смертельное слово, кроме `config`, лечится новым токеном: недоверие хаба,
        // новое поколение или то, чего носитель ещё не знает.
        let needs_token = !probe.token
            || !probe.toml
            || (!probe.fatal.is_empty() && probe.fatal != fatal::CONFIG);
        if probe.alive && probe.fatal.is_empty() {
            state.alive_streak += 1;
            if state.alive_streak >= 2 {
                state.gate.steady();
            }
        } else {
            state.alive_streak = 0;
        }
        if needs_token && !probe.alive {
            if !state.gate.allows() {
                return Ok(());
            }
            state.gate.requested();
            // Запечатанный журнал раннера лечится так же, как новое поколение: чистым
            // состоянием под новый токен.
            let reset = !probe.store
                || probe.fatal == fatal::RESTORE_GENERATION
                || probe.fatal == fatal::JOURNAL_SEALED;
            let credentials = match tokio::time::timeout(
                Duration::from_secs(30),
                self.shared
                    .control
                    .request_token(self.spec.machine_id, reset),
            )
            .await
            {
                Ok(Ok(credentials)) => credentials,
                Ok(Err(TokenError::Refused(refusal))) => {
                    // Повтор ничего не изменит: петли раннера останавливаются до следующего
                    // списка от хаба (его присылают при каждом подключении).
                    if let Some(cloud) = &self.spec.cloud {
                        self.cloud_fail(cloud, "token_refused");
                    }
                    tracing::warn!(runner = %self.spec.machine_id, ?refusal,
                        "hub refused the runner token; the runner is parked");
                    self.parked.store(true, Ordering::SeqCst);
                    self.halt.send_replace(Stop::Detach);
                    return Ok(());
                }
                Ok(Err(TokenError::Unavailable)) | Err(_) => {
                    tracing::warn!(runner = %self.spec.machine_id, "runner token could not be obtained; will ask again");
                    return Ok(());
                }
            };
            if reset && probe.store {
                // Новое поколение: прежнее локальное состояние хабу не подходит.
                self.script(&script::stop(self.root())).await?;
                self.script(&script::wipe_state(self.root())).await?;
            }
            let put = script::receive_file(self.root(), "token", Mode::Secret);
            let sent = self
                .call(&[&put], Input::Bytes(credentials.token.expose().as_bytes()))
                .await?;
            if sent.code != 0 {
                self.install_failed();
                tracing::warn!(runner = %self.spec.machine_id, "runner token could not be installed; will ask again");
                return Ok(());
            }
            let ids = Ids {
                machine_id: credentials.machine_id,
                installation_id: credentials.installation_id,
                restore_generation: credentials.restore_generation,
            };
            if !self.write_config(&ids).await? {
                self.install_failed();
                return Ok(());
            }
            self.call(&[&script::remove_fatal(self.root())], Input::None)
                .await?;
            stopped = true;
            restart = true;
        } else if probe.fatal == fatal::CONFIG || (full && probe.toml) {
            match self.refresh_config(&probe.fatal).await? {
                Some(changed) => restart |= changed,
                None => {
                    // Конфиг не читается, взять идентификаторы неоткуда: убираем его,
                    // со следующего круга пойдёт запрос токена.
                    let path = format!("{}/worker.toml", self.root().display());
                    self.call(&[&format!("rm -f -- {path}")], Input::None)
                        .await?;
                    return Ok(());
                }
            }
        }

        if probe.alive && restart {
            self.script(&script::stop(self.root())).await?;
            stopped = true;
        }
        if stopped || restart {
            let started = self.script(&script::start(self.root())).await?;
            if started.code != 0 {
                self.install_failed();
                tracing::warn!(runner = %self.spec.machine_id, "runner worker could not be started");
            } else {
                tracing::info!(runner = %self.spec.machine_id, "runner worker started");
            }
        }
        Ok(())
    }

    async fn upload_binary(&self, local: &LocalBinary) -> Result<bool, Unreachable> {
        // Отправка сверяется на раннере по хэшу, посчитанному носителем: обрубок не встанет на
        // место рабочего бинарника, а код выхода не ноль возвращает круг присмотра к повтору.
        let put = script::receive_binary(self.root(), "bin/canterlot-worker", &local.sha256);
        let output = self
            .tracked(self.ssh.run(
                &[&put],
                Input::File(&local.path),
                self.shared.timings.upload_limit,
            ))
            .await?;
        if output.code != 0 {
            tracing::warn!(runner = %self.spec.machine_id, "runner binary upload failed");
        }
        Ok(output.code == 0)
    }

    async fn write_config(&self, ids: &Ids) -> Result<bool, Unreachable> {
        let text = script::render_config(
            &ConfigInputs {
                hub_url: &self.shared.hub_url,
                root: self.root(),
                max_running: self.spec.max_running,
                lease_slots: self.spec.lease_slots,
                cloud: self.spec.cloud.is_some(),
            },
            ids,
        );
        let put = script::receive_file(self.root(), "worker.toml", Mode::Plain);
        let output = self.call(&[&put], Input::Bytes(text.as_bytes())).await?;
        if output.code != 0 {
            tracing::warn!(runner = %self.spec.machine_id, "runner config could not be written");
        }
        Ok(output.code == 0)
    }

    /// Сверяет лежащий конфиг с тем, что носитель сгенерировал бы сейчас, и переписывает при
    /// расхождении. `None` — конфиг не прочитался, взять идентификаторы неоткуда.
    async fn refresh_config(&self, fatal: &str) -> Result<Option<bool>, Unreachable> {
        let path = format!("{}/worker.toml", self.root().display());
        let current = self.call(&["cat", "--", &path], Input::None).await?;
        let Some(ids) = (current.code == 0)
            .then(|| Ids::from_config(&current.stdout))
            .flatten()
        else {
            return Ok(None);
        };
        let wanted = script::render_config(
            &ConfigInputs {
                hub_url: &self.shared.hub_url,
                root: self.root(),
                max_running: self.spec.max_running,
                lease_slots: self.spec.lease_slots,
                cloud: self.spec.cloud.is_some(),
            },
            &ids,
        );
        if wanted == current.stdout && fatal != "config" {
            return Ok(Some(false));
        }
        if wanted != current.stdout && !self.write_config(&ids).await? {
            return Ok(Some(false));
        }
        self.call(&[&script::remove_fatal(self.root())], Input::None)
            .await?;
        Ok(Some(true))
    }
}

struct Maintenance {
    gate: TokenGate,
    need_full: bool,
    seen_connects: u64,
    alive_streak: u32,
}

/// Решение по подготовке окна; по умолчанию — ещё не принято.
#[derive(Default)]
struct PrepareGate(AtomicU8);

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Prepared {
    #[default]
    Pending,
    Ready,
    Failed,
}

impl PrepareGate {
    fn state(&self) -> Prepared {
        match self.0.load(Ordering::SeqCst) {
            1 => Prepared::Ready,
            2 => Prepared::Failed,
            _ => Prepared::Pending,
        }
    }

    fn set(&self, state: Prepared) {
        self.0.store(
            match state {
                Prepared::Pending => 0,
                Prepared::Ready => 1,
                Prepared::Failed => 2,
            },
            Ordering::SeqCst,
        );
    }
}

/// Итог одной проверки отметки подготовки.
enum Marker {
    Exists,
    Missing,
    /// Срок подключения вышел, так и не дав ответа.
    Deadline,
    /// Носитель останавливают: ничего не решено и ничего не отмечено.
    Stopped,
}

/// Хвост общего вывода для факта: последние 2048 байт, срез по границе UTF-8.
fn tail_of(stdout: &str, stderr: &str) -> String {
    let mut merged = String::with_capacity(stdout.len() + stderr.len());
    merged.push_str(stdout);
    merged.push_str(stderr);
    const LIMIT: usize = 2048;
    if merged.len() <= LIMIT {
        return merged;
    }
    let mut start = merged.len() - LIMIT;
    while start < merged.len() && !merged.is_char_boundary(start) {
        start += 1;
    }
    merged[start..].to_owned()
}

/// Код и хвост из отметки неудачи: первая строка — код, остальное — вывод.
fn parse_failure(text: &str) -> (i32, String) {
    match text.split_once('\n') {
        Some((code, tail)) => (code.trim().parse().unwrap_or(-1), tail.to_owned()),
        None => (text.trim().parse().unwrap_or(-1), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_pause_grows_to_the_max_and_a_steady_worker_resets_it() {
        let timings = Timings {
            token_retry_min: Duration::from_secs(2),
            token_retry_max: Duration::from_secs(4),
            ..Timings::default()
        };
        let mut gate = TokenGate::new(&timings);
        assert!(gate.allows());
        gate.requested();
        assert!(!gate.allows());
        assert_eq!(gate.interval, Duration::from_secs(4));
        gate.requested();
        assert_eq!(gate.interval, Duration::from_secs(4));
        assert!(!gate.allows());
        gate.steady();
        assert!(gate.allows());
        assert_eq!(gate.interval, Duration::from_secs(2));
    }

    #[test]
    fn the_output_tail_keeps_the_last_bytes_on_a_character_boundary() {
        let text = "ж".repeat(2000);
        let tail = tail_of(&text, "");
        assert!(tail.len() <= 2048, "{}", tail.len());
        assert!(tail.chars().all(|c| c == 'ж'));
        assert!(text.ends_with(&tail));
        assert_eq!(tail_of("short", ""), "short");
        // stderr идёт за stdout: скрипт пишет туда одно, ssh — другое.
        let merged = tail_of("a", "b");
        assert_eq!(merged, "ab");
    }

    #[test]
    fn a_failed_marker_carries_the_code_and_the_tail() {
        assert_eq!(parse_failure("3\nboom\n"), (3, "boom\n".into()));
        assert_eq!(parse_failure("-1\n"), (-1, String::new()));
        assert_eq!(parse_failure("garbage"), (-1, String::new()));
    }
}
