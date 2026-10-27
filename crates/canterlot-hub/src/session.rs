use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use canterlot_proto::worker::v1::ControlDown;
use tokio::sync::{Notify, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::HubTimers;

pub const CAPABILITY_PILOT_V1: &str = "pilot_v1";

pub const CAPABILITY_EXEC_V1: &str = "exec_v1";

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

#[derive(Clone)]
pub struct Registry {
    entries: Arc<Mutex<HashMap<Uuid, SessionEntry>>>,
    timers: HubTimers,
    changes: broadcast::Sender<(Uuid, Availability)>,
}

impl Registry {
    pub fn new(timers: HubTimers) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            timers,
            changes: broadcast::channel(256).0,
        }
    }

    pub fn has_capability(&self, machine_id: Uuid, capability: &str) -> bool {
        self.entries
            .lock()
            .expect("session registry lock")
            .get(&machine_id)
            .is_some_and(|e| e.capabilities.iter().any(|c| c == capability))
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
