//! Мост между носителем и потоком управления воркера: настоящая реализация [`RunnerControl`].
//!
//! Носитель живёт дольше одного соединения, поэтому мост хранит последний список раннеров и
//! очередь исходящих сообщений, а «подключён» означает только согласованную `runners_v1`
//! на текущем соединении. Пока не подключён, запросы токена отвечают «недоступно», а
//! наблюдения отбрасываются: носитель спросит снова, хаб услышит следующее.
//! Токен проходит через мост одним сообщением: ни в очереди, ни в логе, ни в `Debug` его нет.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use canterlot_proto::worker::v1::{
    ControlUp, RunnerObservation, RunnerToken, RunnerTokenRefused, RunnerTokenRequest,
    RunnersAssigned, control_up,
};
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

use super::{
    AssignedRunner, Observation, RunnerControl, RunnerCredentials, SecretToken, TokenError,
    TokenRefusal,
};

type Waiter = oneshot::Sender<Result<RunnerCredentials, TokenRefusal>>;

#[derive(Default)]
struct State {
    connected: bool,
    pending: HashMap<Uuid, Waiter>,
}

pub struct ControlBridge {
    assigned: watch::Sender<Vec<AssignedRunner>>,
    up_tx: mpsc::Sender<ControlUp>,
    up_rx: tokio::sync::Mutex<mpsc::Receiver<ControlUp>>,
    state: Mutex<State>,
}

/// Пока жив, мост принимает запросы; на выходе из соединения все ожидающие получают «недоступно».
pub struct Connected(Arc<ControlBridge>);

impl Drop for Connected {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        state.connected = false;
        state.pending.clear();
    }
}

/// Убирает запись ожидания, если запрос оборвался раньше ответа (срок носителя вышел).
struct PendingGuard<'a>(&'a ControlBridge, Uuid);

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().pending.remove(&self.1);
    }
}

fn refusal_of(code: &str) -> TokenRefusal {
    match code {
        "not_carrier" => TokenRefusal::NotCarrier,
        "not_parent" => TokenRefusal::NotParent,
        "revoked" => TokenRefusal::Revoked,
        _ => TokenRefusal::UnknownMachine,
    }
}

impl ControlBridge {
    /// Мост, очередь исходящих внутри него и приёмник списка раннеров для носителя.
    pub fn new() -> (Arc<Self>, watch::Receiver<Vec<AssignedRunner>>) {
        let (assigned, assigned_rx) = watch::channel(Vec::new());
        let (up_tx, up_rx) = mpsc::channel(64);
        (
            Arc::new(Self {
                assigned,
                up_tx,
                up_rx: tokio::sync::Mutex::new(up_rx),
                state: Mutex::default(),
            }),
            assigned_rx,
        )
    }

    /// Начало соединения с согласованной `runners_v1`: оставшееся от прошлого соединения
    /// из очереди выбрасывается.
    pub fn connect(self: &Arc<Self>) -> Connected {
        if let Ok(mut rx) = self.up_rx.try_lock() {
            while rx.try_recv().is_ok() {}
        }
        self.state.lock().unwrap().connected = true;
        Connected(self.clone())
    }

    /// Следующее сообщение для хаба; `None`, когда очередь закрыта.
    pub async fn next_up(&self) -> Option<ControlUp> {
        self.up_rx.lock().await.recv().await
    }

    /// Список полный, не приращение. `send_replace` будит носитель и при равном списке:
    /// так хаб после переподключения повторно запускает остановленных отказом.
    pub fn on_assigned(&self, message: RunnersAssigned) {
        let runners = message
            .runners
            .into_iter()
            .filter_map(|runner| {
                Some(AssignedRunner {
                    machine_id: Uuid::parse_str(&runner.machine_id).ok()?,
                    name: runner.name,
                    root_dir: runner.root_dir.into(),
                })
            })
            .collect();
        self.assigned.send_replace(runners);
    }

    pub fn on_token(&self, message: RunnerToken) {
        let ids = (
            Uuid::parse_str(&message.machine_id),
            Uuid::parse_str(&message.installation_id),
            Uuid::parse_str(&message.restore_generation),
        );
        let (Ok(machine_id), Ok(installation_id), Ok(restore_generation)) = ids else {
            tracing::warn!("runner token message with invalid identifiers dropped");
            return;
        };
        let waiter = self.state.lock().unwrap().pending.remove(&machine_id);
        match waiter {
            Some(waiter) => {
                let _ = waiter.send(Ok(RunnerCredentials {
                    machine_id,
                    installation_id,
                    restore_generation,
                    token: SecretToken::new(message.token),
                }));
            }
            None => tracing::warn!(runner = %machine_id, "runner token nobody waits for dropped"),
        }
    }

    pub fn on_refused(&self, message: RunnerTokenRefused) {
        let Ok(machine_id) = Uuid::parse_str(&message.machine_id) else {
            return;
        };
        if let Some(waiter) = self.state.lock().unwrap().pending.remove(&machine_id) {
            let _ = waiter.send(Err(refusal_of(&message.code)));
        }
    }
}

#[async_trait]
impl RunnerControl for ControlBridge {
    async fn request_token(
        &self,
        machine_id: Uuid,
        reset_local_state: bool,
    ) -> Result<RunnerCredentials, TokenError> {
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            if !state.connected {
                return Err(TokenError::Unavailable);
            }
            state.pending.insert(machine_id, tx);
        }
        let _guard = PendingGuard(self, machine_id);
        self.up_tx
            .try_send(ControlUp {
                msg: Some(control_up::Msg::RunnerTokenRequest(RunnerTokenRequest {
                    machine_id: machine_id.to_string(),
                    reset_local_state,
                })),
            })
            .map_err(|_| TokenError::Unavailable)?;
        match rx.await {
            Ok(Ok(credentials)) => Ok(credentials),
            Ok(Err(refusal)) => Err(TokenError::Refused(refusal)),
            Err(_) => Err(TokenError::Unavailable),
        }
    }

    async fn report_observation(&self, observation: Observation) {
        if !self.state.lock().unwrap().connected {
            return;
        }
        let observed_at = observation
            .observed_at
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as u64);
        // Переполненная очередь теряет наблюдение: следующее придёт не позже чем через минуту.
        let _ = self.up_tx.try_send(ControlUp {
            msg: Some(control_up::Msg::RunnerObservation(RunnerObservation {
                machine_id: observation.machine_id.to_string(),
                reachable: observation.reachable,
                consecutive_failures: observation.consecutive_failures,
                observed_at,
            })),
        });
    }
}
