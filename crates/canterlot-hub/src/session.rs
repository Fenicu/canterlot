use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use canterlot_proto::{
    limits::{MAX_CLAUDE_HISTORIES, MAX_WORKDIR_SUGGESTIONS},
    worker::v1::{
        ClaudeHistories, ClaudeHistory, ControlDown, ListClaudeHistories, ProbePath,
        ReadOwnerInstructions, SuggestWorkdirs, WorkdirSuggestions, control_down,
    },
};
use tokio::sync::{Notify, broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::HubTimers;

pub const CAPABILITY_PILOT_V1: &str = "pilot_v1";
pub const CAPABILITY_LIMITS_FABLE_V1: &str = "limits_fable_v1";
pub const CAPABILITY_INSTRUCTIONS_V1: &str = "instructions_v1";
pub const CAPABILITY_TODO_V1: &str = "todo_v1";
pub const CAPABILITY_COMPACT_V1: &str = "compact_v1";
pub const CAPABILITY_BACKGROUND_V1: &str = "background_v1";
pub const CAPABILITY_SESSION_TOOLS_V1: &str = "session_tools_v1";
pub const CAPABILITY_SEND_NOW_V1: &str = "send_now_v1";
pub const CAPABILITY_OWNER_EXIT_V1: &str = "owner_exit_v1";
pub const CAPABILITY_PROMPT_SUGGESTIONS_V1: &str = "prompt_suggestions_v1";

pub const CAPABILITY_EXEC_V1: &str = "exec_v1";
pub const CAPABILITY_WORKDIR_SUGGEST_V1: &str = "workdir_suggest_v1";
pub const CAPABILITY_OWNER_INSTRUCTIONS_V1: &str = "owner_instructions_v1";
pub const CAPABILITY_CLAUDE_HISTORIES_V1: &str = "claude_histories_v1";
pub const CAPABILITY_PATH_PROBE_V1: &str = "path_probe_v1";
pub const CAPABILITY_WATCHDOG_V1: &str = "watchdog_v1";
pub const CAPABILITY_ALARMS_V1: &str = "alarms_v1";
pub const CAPABILITY_NUDGE_V1: &str = "nudge_v1";
pub const CAPABILITY_RESOURCE_LEASE_V1: &str = "resource_lease_v1";

/// Форма спрашивает по одному запросу с паузой на ввод, остальным ждать незачем.
pub const MAX_SUGGESTS_PER_MACHINE: usize = 4;
/// Параллельных чтений инструкций владельца на машину.
pub const MAX_OWNER_READS_PER_MACHINE: usize = 4;
/// Списков историй на машину одновременно.
pub const MAX_HISTORY_REQUESTS_PER_MACHINE: usize = 4;
/// Проб пути на машину одновременно: взвод, опрос будильника, автопинок.
pub const MAX_PATH_PROBES_PER_MACHINE: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub machine_id: Uuid,
    pub connection_id: Uuid,
    pub data_attached: bool,
    pub last_heartbeat: Instant,
}

struct SessionEntry {
    info: SessionInfo,
    key: Uuid,
    commands: mpsc::Sender<ControlDown>,
    cancel: CancellationToken,
    wake: Arc<Notify>,
    data: Option<(Uuid, CancellationToken)>,
    availability: Availability,
    revoked: Arc<AtomicBool>,
    credential: Vec<u8>,
    reconciled: bool,
    capabilities: Vec<String>,
    clock_samples: VecDeque<chrono::Duration>,
}

type SuggestWaiters = Arc<Mutex<HashMap<(Uuid, Uuid), oneshot::Sender<Vec<String>>>>>;
type ClaudeHistoryWaiters = Arc<Mutex<HashMap<(Uuid, Uuid), oneshot::Sender<Vec<ClaudeHistory>>>>>;
type PathProbeWaiters =
    Arc<Mutex<HashMap<(Uuid, Uuid), oneshot::Sender<canterlot_proto::worker::v1::PathProbe>>>>;
type OwnerOverviewWaiters = Arc<
    Mutex<
        HashMap<
            (Uuid, Uuid),
            oneshot::Sender<canterlot_proto::worker::v1::OwnerInstructionsOverview>,
        >,
    >,
>;
type OwnerFileWaiters = Arc<
    Mutex<
        HashMap<(Uuid, Uuid), oneshot::Sender<canterlot_proto::worker::v1::OwnerInstructionsFile>>,
    >,
>;

#[derive(Clone)]
pub struct Registry {
    entries: Arc<Mutex<HashMap<Uuid, SessionEntry>>>,
    timers: HubTimers,
    changes: broadcast::Sender<(Uuid, Availability)>,
    suggests: SuggestWaiters,
    histories: ClaudeHistoryWaiters,
    probes: PathProbeWaiters,
    owner_overview: OwnerOverviewWaiters,
    owner_files: OwnerFileWaiters,
}

/// Снимает ожидание и на сроке, и при обрыве HTTP-запроса.
struct SuggestSlot {
    waiters: SuggestWaiters,
    key: (Uuid, Uuid),
}

impl Drop for SuggestSlot {
    fn drop(&mut self) {
        self.waiters
            .lock()
            .expect("suggest waiters lock")
            .remove(&self.key);
    }
}

struct ClaudeHistorySlot {
    waiters: ClaudeHistoryWaiters,
    key: (Uuid, Uuid),
}

impl Drop for ClaudeHistorySlot {
    fn drop(&mut self) {
        self.waiters
            .lock()
            .expect("history waiters lock")
            .remove(&self.key);
    }
}

struct PathProbeSlot {
    waiters: PathProbeWaiters,
    key: (Uuid, Uuid),
}

impl Drop for PathProbeSlot {
    fn drop(&mut self) {
        self.waiters
            .lock()
            .expect("probe waiters lock")
            .remove(&self.key);
    }
}

struct OwnerOverviewSlot {
    waiters: OwnerOverviewWaiters,
    key: (Uuid, Uuid),
}

impl Drop for OwnerOverviewSlot {
    fn drop(&mut self) {
        self.waiters
            .lock()
            .expect("owner overview waiters lock")
            .remove(&self.key);
    }
}

struct OwnerFileSlot {
    waiters: OwnerFileWaiters,
    key: (Uuid, Uuid),
}

impl Drop for OwnerFileSlot {
    fn drop(&mut self) {
        self.waiters
            .lock()
            .expect("owner file waiters lock")
            .remove(&self.key);
    }
}

/// Тестовое подключение к реестру: держит слот, пока живёт guard.
#[cfg(feature = "testkit")]
pub struct TestLease {
    _lease: ControlLease,
    _commands: mpsc::Receiver<ControlDown>,
}

/// Ошибка чтения инструкций владельца: нет сеанса, нет возможности или вышел срок.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerReadError {
    Unavailable,
    CapabilityMissing,
}

/// Итог пробы пути: ответ машины, отсутствие возможности или пропавший сеанс.
#[derive(Debug)]
pub enum ProbeReply {
    Answered(canterlot_proto::worker::v1::PathProbe),
    CapabilityMissing,
    Unavailable,
}

impl Registry {
    pub fn new(timers: HubTimers) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            timers,
            changes: broadcast::channel(256).0,
            suggests: Arc::default(),
            histories: Arc::default(),
            probes: Arc::default(),
            owner_overview: Arc::default(),
            owner_files: Arc::default(),
        }
    }

    pub fn has_capability(&self, machine_id: Uuid, capability: &str) -> bool {
        self.entries
            .lock()
            .expect("session registry lock")
            .get(&machine_id)
            .is_some_and(|e| e.capabilities.iter().any(|c| c == capability))
    }

    pub fn capabilities(&self, machine_id: Uuid) -> Vec<String> {
        self.entries
            .lock()
            .expect("session registry lock")
            .get(&machine_id)
            .map(|entry| entry.capabilities.clone())
            .unwrap_or_default()
    }

    pub(crate) fn capabilities_for_connection(
        &self,
        machine_id: Uuid,
        connection_id: Uuid,
    ) -> Option<Vec<String>> {
        self.entries
            .lock()
            .expect("session registry lock")
            .get(&machine_id)
            .filter(|entry| entry.info.connection_id == connection_id)
            .map(|entry| entry.capabilities.clone())
    }

    pub fn observe_clock(
        &self,
        machine_id: Uuid,
        machine_ms: u64,
        received_at: chrono::DateTime<chrono::Utc>,
    ) {
        if machine_ms == 0 {
            return;
        }
        let Some(at) = i64::try_from(machine_ms)
            .ok()
            .and_then(chrono::DateTime::from_timestamp_millis)
        else {
            return;
        };
        let mut entries = self.entries.lock().expect("session registry lock");
        if let Some(entry) = entries.get_mut(&machine_id) {
            let received_at =
                chrono::DateTime::from_timestamp_micros(received_at.timestamp_micros())
                    .expect("valid clock sample");
            entry.clock_samples.push_back(received_at - at);
            if entry.clock_samples.len() > 8 {
                entry.clock_samples.pop_front();
            }
        }
    }

    pub fn clock_offset(&self, machine_id: Uuid) -> Option<chrono::Duration> {
        self.entries
            .lock()
            .expect("session registry lock")
            .get(&machine_id)?
            .clock_samples
            .iter()
            .min()
            .copied()
    }

    pub fn to_hub_time(
        &self,
        machine_id: Uuid,
        machine_ms: u64,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        if machine_ms == 0 {
            return None;
        }
        chrono::DateTime::from_timestamp_millis(i64::try_from(machine_ms).ok()?)?
            .checked_add_signed(self.clock_offset(machine_id)?)
    }

    pub fn availability(&self, machine_id: Uuid) -> Availability {
        let mut entries = self.entries.lock().expect("session registry lock");
        entries
            .get_mut(&machine_id)
            .map_or(Availability::Unavailable, |entry| self.refresh(entry))
    }

    pub fn is_reconciled(&self, machine_id: Uuid) -> bool {
        self.entries
            .lock()
            .expect("session registry lock")
            .get(&machine_id)
            .is_some_and(|entry| entry.reconciled)
    }

    pub(crate) fn mark_accepted(
        &self,
        machine_id: Uuid,
        connection_id: Uuid,
        capabilities: &[String],
    ) {
        {
            let mut entries = self.entries.lock().expect("session registry lock");
            if let Some(entry) = entries
                .get_mut(&machine_id)
                .filter(|entry| entry.info.connection_id == connection_id)
            {
                entry.capabilities = capabilities.to_vec();
            }
        }
        if !capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_EXEC_V1)
        {
            self.mark_reconciled(machine_id, connection_id);
        }
    }

    /// Тестовый шов: зарегистрировать машину с capabilities без полного handshake.
    #[cfg(feature = "testkit")]
    pub fn connect_for_tests(&self, machine_id: Uuid, capabilities: &[String]) -> TestLease {
        let connection = Uuid::now_v7();
        let (lease, commands) = self
            .register(machine_id, connection, Vec::new())
            .expect("test slot free");
        self.mark_accepted(machine_id, connection, capabilities);
        TestLease {
            _lease: lease,
            _commands: commands,
        }
    }

    pub(crate) fn mark_reconciled(&self, machine_id: Uuid, connection_id: Uuid) {
        let mut entries = self.entries.lock().expect("session registry lock");
        if let Some(entry) = entries
            .get_mut(&machine_id)
            .filter(|entry| entry.info.connection_id == connection_id)
        {
            entry.reconciled = true;
            let availability = self.refresh(entry);
            if availability == Availability::Available {
                let _ = self.changes.send((machine_id, availability));
            }
        }
    }

    /// Производитель проверяет encoded_len() перед постановкой команды в очередь.
    pub fn command_sender(&self, machine_id: Uuid) -> Option<mpsc::Sender<ControlDown>> {
        self.command_session(machine_id)
            .map(|(sender, _, _)| sender)
    }

    pub(crate) fn command_session(
        &self,
        machine_id: Uuid,
    ) -> Option<(mpsc::Sender<ControlDown>, Uuid, Vec<u8>)> {
        let mut entries = self.entries.lock().expect("session registry lock");
        let entry = entries.get_mut(&machine_id)?;
        (self.refresh(entry) != Availability::Unavailable).then(|| {
            (
                entry.commands.clone(),
                entry.info.connection_id,
                entry.credential.clone(),
            )
        })
    }

    /// Подсказка рабочей папки от живого воркера. Нет сеанса с возможностью, занято или
    /// вышел срок: пустой список, форма остаётся со свободным вводом.
    pub async fn suggest_workdirs(
        &self,
        machine_id: Uuid,
        path: &str,
        wait: std::time::Duration,
    ) -> Vec<String> {
        if !self.has_capability(machine_id, CAPABILITY_WORKDIR_SUGGEST_V1) {
            return Vec::new();
        }
        let Some(sender) = self.command_sender(machine_id) else {
            return Vec::new();
        };
        let request_id = Uuid::now_v7();
        let key = (machine_id, request_id);
        let (answer, answered) = oneshot::channel();
        {
            let mut waiters = self.suggests.lock().expect("suggest waiters lock");
            let busy = waiters.keys().filter(|(id, _)| *id == machine_id).count();
            if busy >= MAX_SUGGESTS_PER_MACHINE {
                return Vec::new();
            }
            waiters.insert(key, answer);
        }
        let _slot = SuggestSlot {
            waiters: self.suggests.clone(),
            key,
        };
        let message = ControlDown {
            msg: Some(control_down::Msg::SuggestWorkdirs(SuggestWorkdirs {
                request_id: request_id.to_string(),
                path: path.to_owned(),
            })),
        };
        if sender.try_send(message).is_err() {
            return Vec::new();
        }
        tokio::time::timeout(wait, answered)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
    }

    /// Ответ принимается только от машины, которую спросили.
    pub fn finish_suggest(&self, machine_id: Uuid, reply: WorkdirSuggestions) {
        let Ok(request_id) = Uuid::parse_str(&reply.request_id) else {
            return;
        };
        let waiter = self
            .suggests
            .lock()
            .expect("suggest waiters lock")
            .remove(&(machine_id, request_id));
        if let Some(waiter) = waiter {
            let mut directories = reply.directories;
            directories.truncate(MAX_WORKDIR_SUGGESTIONS);
            let _ = waiter.send(directories);
        }
    }

    /// История Claude Code выбранного рабочего каталога. Нет сеанса с возможностью,
    /// занято или вышел срок: пустой список, форма не показывает продолжение.
    pub async fn claude_histories(
        &self,
        machine_id: Uuid,
        workdir: &str,
        wait: std::time::Duration,
    ) -> Vec<ClaudeHistory> {
        if !self.has_capability(machine_id, CAPABILITY_CLAUDE_HISTORIES_V1) {
            return Vec::new();
        }
        let Some(sender) = self.command_sender(machine_id) else {
            return Vec::new();
        };
        let request_id = Uuid::now_v7();
        let key = (machine_id, request_id);
        let (answer, answered) = oneshot::channel();
        {
            let mut waiters = self.histories.lock().expect("history waiters lock");
            let busy = waiters.keys().filter(|(id, _)| *id == machine_id).count();
            if busy >= MAX_HISTORY_REQUESTS_PER_MACHINE {
                return Vec::new();
            }
            waiters.insert(key, answer);
        }
        let _slot = ClaudeHistorySlot {
            waiters: self.histories.clone(),
            key,
        };
        let message = ControlDown {
            msg: Some(control_down::Msg::ListClaudeHistories(
                ListClaudeHistories {
                    request_id: request_id.to_string(),
                    workdir: workdir.to_owned(),
                },
            )),
        };
        if sender.try_send(message).is_err() {
            return Vec::new();
        }
        tokio::time::timeout(wait, answered)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
    }

    /// Ответ принимается только от машины, которую спросили.
    pub fn finish_histories(&self, machine_id: Uuid, reply: ClaudeHistories) {
        let Ok(request_id) = Uuid::parse_str(&reply.request_id) else {
            return;
        };
        let waiter = self
            .histories
            .lock()
            .expect("history waiters lock")
            .remove(&(machine_id, request_id));
        if let Some(waiter) = waiter {
            let mut items = reply.items;
            items.truncate(MAX_CLAUDE_HISTORIES);
            items.retain(|item| {
                Uuid::parse_str(&item.id).is_ok() && item.preview.chars().count() <= 120
            });
            let _ = waiter.send(items);
        }
    }

    /// Проба пути на машине: тот же срок и корреляция по `request_id`, что у подсказок.
    /// Возможность не согласована — `CapabilityMissing`, нет живого сеанса или срок
    /// вышел — `Unavailable`; и то и другое вызывающий решает сам.
    pub async fn probe_path(
        &self,
        machine_id: Uuid,
        path: &str,
        read_head_bytes: u32,
        wait: std::time::Duration,
    ) -> ProbeReply {
        if self.session_info(machine_id).is_none() {
            return ProbeReply::Unavailable;
        }
        if !self.has_capability(machine_id, CAPABILITY_PATH_PROBE_V1) {
            return ProbeReply::CapabilityMissing;
        }
        let Some(sender) = self.command_sender(machine_id) else {
            return ProbeReply::Unavailable;
        };
        let request_id = Uuid::now_v7();
        let key = (machine_id, request_id);
        let (answer, answered) = oneshot::channel();
        {
            let mut waiters = self.probes.lock().expect("probe waiters lock");
            let busy = waiters.keys().filter(|(id, _)| *id == machine_id).count();
            if busy >= MAX_PATH_PROBES_PER_MACHINE {
                return ProbeReply::Unavailable;
            }
            waiters.insert(key, answer);
        }
        let _slot = PathProbeSlot {
            waiters: self.probes.clone(),
            key,
        };
        let message = ControlDown {
            msg: Some(control_down::Msg::ProbePath(ProbePath {
                request_id: request_id.to_string(),
                path: path.to_owned(),
                read_head_bytes,
            })),
        };
        if sender.try_send(message).is_err() {
            return ProbeReply::Unavailable;
        }
        tokio::time::timeout(wait, answered)
            .await
            .ok()
            .and_then(Result::ok)
            .map_or(ProbeReply::Unavailable, ProbeReply::Answered)
    }

    /// Ответ принимается только от машины, которую спросили.
    pub fn finish_probe(&self, machine_id: Uuid, reply: canterlot_proto::worker::v1::PathProbe) {
        let Ok(request_id) = Uuid::parse_str(&reply.request_id) else {
            return;
        };
        let waiter = self
            .probes
            .lock()
            .expect("probe waiters lock")
            .remove(&(machine_id, request_id));
        if let Some(waiter) = waiter {
            let _ = waiter.send(reply);
        }
    }

    fn owner_sender(&self, machine_id: Uuid) -> Result<mpsc::Sender<ControlDown>, OwnerReadError> {
        if self.session_info(machine_id).is_none() {
            return Err(OwnerReadError::Unavailable);
        }
        if !self.has_capability(machine_id, CAPABILITY_OWNER_INSTRUCTIONS_V1) {
            return Err(OwnerReadError::CapabilityMissing);
        }
        self.command_sender(machine_id)
            .ok_or(OwnerReadError::Unavailable)
    }

    /// Обзор без содержимого: метаданные файлов и список проектов памяти.
    pub async fn owner_overview(
        &self,
        machine_id: Uuid,
        wait: std::time::Duration,
    ) -> Result<canterlot_proto::worker::v1::OwnerInstructionsOverview, OwnerReadError> {
        let sender = self.owner_sender(machine_id)?;
        let request_id = Uuid::now_v7();
        let key = (machine_id, request_id);
        let (answer, answered) = oneshot::channel();
        {
            let mut waiters = self
                .owner_overview
                .lock()
                .expect("owner overview waiters lock");
            let busy = waiters.keys().filter(|(id, _)| *id == machine_id).count();
            if busy >= MAX_OWNER_READS_PER_MACHINE {
                return Err(OwnerReadError::Unavailable);
            }
            waiters.insert(key, answer);
        }
        let _slot = OwnerOverviewSlot {
            waiters: self.owner_overview.clone(),
            key,
        };
        let message = ControlDown {
            msg: Some(control_down::Msg::ReadOwner(ReadOwnerInstructions {
                request_id: request_id.to_string(),
                kind: "overview".into(),
                project: String::new(),
                name: String::new(),
            })),
        };
        if sender.try_send(message).is_err() {
            return Err(OwnerReadError::Unavailable);
        }
        tokio::time::timeout(wait, answered)
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or(OwnerReadError::Unavailable)
    }

    /// Один файл по виду; проверяются вид и длины, путь воркер строит сам.
    pub async fn owner_file(
        &self,
        machine_id: Uuid,
        kind: &str,
        project: &str,
        name: &str,
        wait: std::time::Duration,
    ) -> Result<canterlot_proto::worker::v1::OwnerInstructionsFile, OwnerReadError> {
        let sender = self.owner_sender(machine_id)?;
        let request_id = Uuid::now_v7();
        let key = (machine_id, request_id);
        let (answer, answered) = oneshot::channel();
        {
            let mut waiters = self.owner_files.lock().expect("owner file waiters lock");
            let busy = waiters.keys().filter(|(id, _)| *id == machine_id).count();
            if busy >= MAX_OWNER_READS_PER_MACHINE {
                return Err(OwnerReadError::Unavailable);
            }
            waiters.insert(key, answer);
        }
        let _slot = OwnerFileSlot {
            waiters: self.owner_files.clone(),
            key,
        };
        let message = ControlDown {
            msg: Some(control_down::Msg::ReadOwner(ReadOwnerInstructions {
                request_id: request_id.to_string(),
                kind: kind.to_owned(),
                project: project.to_owned(),
                name: name.to_owned(),
            })),
        };
        if sender.try_send(message).is_err() {
            return Err(OwnerReadError::Unavailable);
        }
        tokio::time::timeout(wait, answered)
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or(OwnerReadError::Unavailable)
    }

    pub fn finish_owner_overview(
        &self,
        machine_id: Uuid,
        reply: canterlot_proto::worker::v1::OwnerInstructionsOverview,
    ) {
        let Ok(request_id) = Uuid::parse_str(&reply.request_id) else {
            return;
        };
        let waiter = self
            .owner_overview
            .lock()
            .expect("owner overview waiters lock")
            .remove(&(machine_id, request_id));
        if let Some(waiter) = waiter {
            let _ = waiter.send(reply);
        }
    }

    pub fn finish_owner_file(
        &self,
        machine_id: Uuid,
        reply: canterlot_proto::worker::v1::OwnerInstructionsFile,
    ) {
        let Ok(request_id) = Uuid::parse_str(&reply.request_id) else {
            return;
        };
        let waiter = self
            .owner_files
            .lock()
            .expect("owner file waiters lock")
            .remove(&(machine_id, request_id));
        if let Some(waiter) = waiter {
            let _ = waiter.send(reply);
        }
    }

    pub fn subscribe_changes(&self) -> broadcast::Receiver<(Uuid, Availability)> {
        self.changes.subscribe()
    }

    pub fn session_info(&self, machine_id: Uuid) -> Option<SessionInfo> {
        self.entries
            .lock()
            .expect("session registry lock")
            .get(&machine_id)
            .map(|entry| entry.info.clone())
    }

    pub(crate) fn credentials(&self) -> Vec<(Uuid, Uuid, Vec<u8>)> {
        self.entries
            .lock()
            .expect("session registry lock")
            .values()
            .map(|entry| {
                (
                    entry.info.machine_id,
                    entry.info.connection_id,
                    entry.credential.clone(),
                )
            })
            .collect()
    }

    pub(crate) fn revoke(&self, machine_id: Uuid, connection_id: Uuid) {
        let mut entries = self.entries.lock().expect("session registry lock");
        if !entries
            .get(&machine_id)
            .is_some_and(|entry| entry.info.connection_id == connection_id)
        {
            return;
        }
        let entry = entries.remove(&machine_id).expect("current session");
        entry.revoked.store(true, Ordering::Release);
        entry.cancel.cancel();
        if let Some((_, data)) = entry.data {
            data.cancel();
        }
        let _ = self.changes.send((machine_id, Availability::Unavailable));
    }

    fn refresh(&self, entry: &mut SessionEntry) -> Availability {
        let age = entry.info.last_heartbeat.elapsed();
        let availability = if age >= self.timers.unavailable_after {
            Availability::Unavailable
        } else if !entry.info.data_attached || age >= self.timers.degraded_after {
            Availability::Degraded
        } else {
            Availability::Available
        };
        if entry.availability != availability {
            entry.availability = availability;
            let _ = self.changes.send((entry.info.machine_id, availability));
        }
        availability
    }

    pub(crate) fn register(
        &self,
        machine_id: Uuid,
        connection_id: Uuid,
        credential: Vec<u8>,
    ) -> Option<(ControlLease, mpsc::Receiver<ControlDown>)> {
        let cancel = CancellationToken::new();
        let wake = Arc::new(Notify::new());
        let key = Uuid::now_v7();
        let revoked = Arc::new(AtomicBool::new(false));
        let (commands, receiver) = mpsc::channel(64);
        let mut entries = self.entries.lock().expect("session registry lock");
        if entries.get(&machine_id).is_some_and(|entry| {
            entry.info.last_heartbeat.elapsed() < self.timers.unavailable_after
        }) {
            return None;
        }
        if let Some(old) = entries.remove(&machine_id) {
            old.cancel.cancel();
            if let Some((_, data)) = old.data {
                data.cancel();
            }
        }
        let mut entry = SessionEntry {
            info: SessionInfo {
                machine_id,
                connection_id,
                data_attached: false,
                last_heartbeat: Instant::now(),
            },
            key,
            commands,
            cancel: cancel.clone(),
            wake: wake.clone(),
            data: None,
            availability: Availability::Unavailable,
            revoked: revoked.clone(),
            credential,
            reconciled: false,
            capabilities: Vec::new(),
            clock_samples: VecDeque::new(),
        };
        self.refresh(&mut entry);
        entries.insert(machine_id, entry);
        drop(entries);
        let registry = self.clone();
        let monitor_cancel = cancel.clone();
        tokio::spawn(async move {
            registry
                .monitor(machine_id, key, monitor_cancel, wake)
                .await;
        });
        Some((
            ControlLease {
                registry: self.clone(),
                machine_id,
                key,
                cancel,
                revoked,
            },
            receiver,
        ))
    }

    pub(crate) fn attach_data(
        &self,
        machine_id: Uuid,
        connection_id: Uuid,
        credential: &[u8],
    ) -> Option<DataLease> {
        let mut entries = self.entries.lock().expect("session registry lock");
        let entry = entries.get_mut(&machine_id)?;
        if entry.info.connection_id != connection_id
            || entry.credential != credential
            || entry.info.last_heartbeat.elapsed() >= self.timers.unavailable_after
        {
            return None;
        }
        let key = Uuid::now_v7();
        let cancel = CancellationToken::new();
        if let Some((_, previous)) = entry.data.replace((key, cancel.clone())) {
            previous.cancel();
        }
        entry.info.data_attached = true;
        self.refresh(entry);
        Some(DataLease {
            registry: self.clone(),
            machine_id,
            revoked: entry.revoked.clone(),
            session_key: entry.key,
            key,
            cancel,
        })
    }

    pub(crate) fn heartbeat(&self, machine_id: Uuid, key: Uuid) -> bool {
        let mut entries = self.entries.lock().expect("session registry lock");
        let Some(entry) = entries
            .get_mut(&machine_id)
            .filter(|entry| entry.key == key)
        else {
            return false;
        };
        entry.info.last_heartbeat = Instant::now();
        self.refresh(entry);
        entry.wake.notify_one();
        true
    }

    async fn monitor(
        &self,
        machine_id: Uuid,
        key: Uuid,
        cancel: CancellationToken,
        wake: Arc<Notify>,
    ) {
        loop {
            let notified = wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let deadline = {
                let mut entries = self.entries.lock().expect("session registry lock");
                let Some(entry) = entries
                    .get_mut(&machine_id)
                    .filter(|entry| entry.key == key)
                else {
                    return;
                };
                self.refresh(entry);
                let age = entry.info.last_heartbeat.elapsed();
                [self.timers.degraded_after, self.timers.unavailable_after]
                    .into_iter()
                    .filter(|threshold| age < *threshold)
                    .min()
                    .map(|threshold| entry.info.last_heartbeat + threshold)
            };
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = &mut notified => {},
                _ = async {
                    if let Some(deadline) = deadline { tokio::time::sleep_until(deadline.into()).await; }
                    else { std::future::pending::<()>().await; }
                } => {},
            }
        }
    }
}

pub(crate) struct ControlLease {
    registry: Registry,
    pub machine_id: Uuid,
    pub key: Uuid,
    pub cancel: CancellationToken,
    revoked: Arc<AtomicBool>,
}

impl ControlLease {
    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }
}

impl Drop for ControlLease {
    fn drop(&mut self) {
        let mut entries = self.registry.entries.lock().expect("session registry lock");
        if entries
            .get(&self.machine_id)
            .is_some_and(|entry| entry.key == self.key)
        {
            let entry = entries.remove(&self.machine_id).expect("current session");
            entry.cancel.cancel();
            if let Some((_, data)) = entry.data {
                data.cancel();
            }
            let _ = self
                .registry
                .changes
                .send((self.machine_id, Availability::Unavailable));
        }
    }
}

pub(crate) struct DataLease {
    registry: Registry,
    machine_id: Uuid,
    session_key: Uuid,
    key: Uuid,
    pub cancel: CancellationToken,
    revoked: Arc<AtomicBool>,
}

impl DataLease {
    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }
}

impl Drop for DataLease {
    fn drop(&mut self) {
        let mut entries = self.registry.entries.lock().expect("session registry lock");
        if let Some(entry) = entries.get_mut(&self.machine_id).filter(|entry| {
            entry.key == self.session_key
                && entry.data.as_ref().is_some_and(|(key, _)| *key == self.key)
        }) {
            entry.data = None;
            entry.info.data_attached = false;
            self.registry.refresh(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_capabilities_follow_the_current_connection() {
        let registry = Registry::new(HubTimers::default());
        let machine = Uuid::now_v7();
        let connection = Uuid::now_v7();
        let (lease, _commands) = registry.register(machine, connection, vec![]).unwrap();
        let caps = vec!["exec_v1".to_owned(), "pilot_v1".to_owned()];
        registry.mark_accepted(machine, connection, &caps);
        assert_eq!(registry.capabilities(machine), caps);
        assert_eq!(
            registry.capabilities_for_connection(machine, connection),
            Some(caps)
        );
        assert!(
            registry
                .capabilities_for_connection(machine, Uuid::now_v7())
                .is_none()
        );
        drop(lease);
        assert!(registry.capabilities(machine).is_empty());
    }

    #[tokio::test]
    async fn a_probe_reply_is_correlated_by_machine_and_request_id() {
        let registry = Registry::new(HubTimers::default());
        let machine = Uuid::now_v7();
        let other = Uuid::now_v7();
        let request = Uuid::now_v7();
        let (answer, mut waiter) = oneshot::channel();
        registry
            .probes
            .lock()
            .unwrap()
            .insert((machine, request), answer);
        let reply = |request_id: Uuid| canterlot_proto::worker::v1::PathProbe {
            request_id: request_id.to_string(),
            outcome: Some(canterlot_proto::worker::v1::path_probe::Outcome::Missing(
                Default::default(),
            )),
            now_ms: 0,
        };
        registry.finish_probe(other, reply(request));
        assert!(waiter.try_recv().is_err(), "another machine cannot answer");
        registry.finish_probe(machine, reply(Uuid::now_v7()));
        assert!(waiter.try_recv().is_err(), "an unknown request is ignored");
        registry.finish_probe(machine, reply(request));
        assert!(waiter.try_recv().is_ok(), "the asked machine answers");
        // Поздний повтор уже снятого ожидания ничего не ломает.
        registry.finish_probe(machine, reply(request));
    }

    #[tokio::test]
    async fn accepted_requires_reconciliation_only_with_exec_v1() {
        for capabilities in [vec![], vec![CAPABILITY_EXEC_V1.to_string()]] {
            let registry = Registry::new(HubTimers::default());
            let machine = Uuid::now_v7();
            let connection = Uuid::now_v7();
            let (_lease, _commands) = registry.register(machine, connection, vec![]).unwrap();
            let mut changes = registry.subscribe_changes();
            assert!(!registry.is_reconciled(machine));
            registry.mark_accepted(machine, Uuid::now_v7(), &capabilities);
            assert!(!registry.is_reconciled(machine));
            registry.mark_accepted(machine, connection, &capabilities);
            assert_eq!(registry.is_reconciled(machine), capabilities.is_empty());
            assert!(matches!(
                changes.try_recv(),
                Err(broadcast::error::TryRecvError::Empty)
            ));
            registry.mark_reconciled(machine, connection);
            assert!(registry.is_reconciled(machine));
        }
    }

    #[tokio::test]
    async fn reconciliation_is_bound_to_the_current_connection() {
        let registry = Registry::new(HubTimers::default());
        let machine = Uuid::now_v7();
        let connection = Uuid::now_v7();
        assert!(!registry.is_reconciled(machine));
        let (lease, _commands) = registry.register(machine, connection, vec![]).unwrap();
        let data = registry.attach_data(machine, connection, &[]).unwrap();
        assert_eq!(registry.availability(machine), Availability::Available);
        assert!(!registry.is_reconciled(machine));
        registry.mark_reconciled(machine, Uuid::now_v7());
        assert!(!registry.is_reconciled(machine));
        registry.mark_reconciled(machine, connection);
        assert!(registry.is_reconciled(machine));
        drop(data);
        drop(lease);
        assert!(!registry.is_reconciled(machine));
        let replacement = Uuid::now_v7();
        let (_lease, _commands) = registry.register(machine, replacement, vec![]).unwrap();
        registry.mark_reconciled(machine, connection);
        assert!(!registry.is_reconciled(machine));
        registry.mark_reconciled(machine, replacement);
        assert!(registry.is_reconciled(machine));
    }
}
