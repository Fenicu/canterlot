//! Облачные раннеры носителя: долговечные подключения по `AttachRunner`.
//!
//! Команда хаба describes один сервер окна. Носитель запоминает её под идентификатором машины,
//! пишет `known_hosts` с закреплённым ключом и отдаёт спецификацию финансовому присмотру
//! раннеров ([`super::run`]), который ведёт облачный раннер тем же путём, что и статический.
//! Состояние переживает рестарт воркера: оно лежит в каталоге данных под `cloud/`.

use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::Mutex,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};
use uuid::Uuid;

use super::script;

/// Итог подключения облачного раннера: короткий код, без токенов и пользовательских данных.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttachOutcome {
    Started {
        window_id: String,
        machine_id: Uuid,
    },
    Failed {
        window_id: String,
        machine_id: Uuid,
        code: &'static str,
    },
}

impl AttachOutcome {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Started { .. } => "started",
            Self::Failed { code, .. } => code,
        }
    }
}

/// Всё, что нужно носителю для облачного раннера, кроме обмена с хабом.
#[derive(Clone, Debug)]
pub struct CloudConfig {
    pub data_dir: PathBuf,
    pub ssh_key: Option<PathBuf>,
    /// Пользователь облачного сервера. В бою это `runner` из cloud-init хаба.
    pub user: String,
    /// Маркер готовности cloud-init; тот же путь, что в `cloud_init.rs` хаба.
    pub ready_marker: PathBuf,
    pub ready_timeout: Duration,
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("/var/lib/canterlot-worker"),
            ssh_key: None,
            user: "runner".into(),
            ready_marker: PathBuf::from("/var/lib/canterlot-runner/ready"),
            ready_timeout: Duration::from_secs(15 * 60),
        }
    }
}

/// Приложение, сохранённое на диске. Пароль, ключ и токен в него не попадают.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub window_id: String,
    pub machine_id: Uuid,
    pub host: String,
    /// Публичный порт DNAT; 0 заменяется на 22 при подключении.
    pub ssh_port: u16,
    pub host_key_public: String,
    pub root_dir: PathBuf,
    pub max_running: u32,
    pub lease_slots: u32,
}

/// Что присмотр раннеров знает об облачном подключении.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudSpec {
    pub window_id: String,
    pub machine_id: Uuid,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity: Option<PathBuf>,
    pub known_hosts: PathBuf,
    pub root: PathBuf,
    pub max_running: u32,
    pub lease_slots: u32,
    pub ready_marker: PathBuf,
    pub ready_timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum AttachError {
    #[error("invalid cloud attachment")]
    Invalid,
    #[error("cloud attachment state I/O failed")]
    Io,
}

/// Долговечное состояние облачных подключений одного носителя.
pub struct CloudState {
    config: CloudConfig,
    file: PathBuf,
    attachments: Mutex<BTreeMap<Uuid, Attachment>>,
    specs: watch::Sender<Vec<CloudSpec>>,
    outcomes: Mutex<Vec<AttachOutcome>>,
    changed: Notify,
}

impl std::fmt::Debug for CloudState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudState")
            .field("file", &self.file)
            .field("attachments", &self.attachments.lock().unwrap().len())
            .finish_non_exhaustive()
    }
}

impl CloudState {
    /// Открывает состояние из каталога данных; битый или отсутствующий файл не ломает носитель.
    pub fn open(config: CloudConfig) -> Self {
        let file = config.data_dir.join("cloud").join("attachments.json");
        let mut map = BTreeMap::new();
        if let Ok(bytes) = std::fs::read(&file)
            && let Ok(list) = serde_json::from_slice::<Vec<Attachment>>(&bytes)
        {
            for attachment in list {
                if valid(&attachment) {
                    map.insert(attachment.machine_id, attachment);
                }
            }
        }
        let (specs, _) = watch::channel(Vec::new());
        let state = Self {
            config,
            file,
            attachments: Mutex::new(map),
            specs,
            outcomes: Mutex::new(Vec::new()),
            changed: Notify::const_new(),
        };
        state.publish();
        state
    }

    /// Применяет команду `AttachRunner`: тот же `window_id` не меняет ничего, новый заменяет
    /// прежнее подключение этой машины. Состояние пишется до ответа хабу.
    pub fn apply(&self, attachment: Attachment) -> Result<(), AttachError> {
        if !valid(&attachment) {
            return Err(AttachError::Invalid);
        }
        let known_hosts = self.known_hosts_path(&attachment);
        if let Some(parent) = known_hosts.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return Err(AttachError::Io);
        }
        let port = if attachment.ssh_port == 0 {
            22
        } else {
            attachment.ssh_port
        };
        let line = format!(
            "[{}]:{} {}\n",
            attachment.host, port, attachment.host_key_public
        );
        // Атомарная замена: оборванная запись не оставит половинку ключа.
        let temp = known_hosts.with_extension("new");
        if std::fs::write(&temp, line).is_err() || std::fs::rename(&temp, &known_hosts).is_err() {
            return Err(AttachError::Io);
        }
        {
            let mut map = self.attachments.lock().unwrap();
            if map.get(&attachment.machine_id) == Some(&attachment) {
                return Ok(());
            }
            map.insert(attachment.machine_id, attachment);
        }
        self.persist()?;
        self.publish();
        Ok(())
    }

    /// Забывает одно подключение: машина ушла из `RunnersAssigned`, сервер удалит хаб.
    pub fn forget(&self, machine_id: Uuid) -> bool {
        let removed = self.attachments.lock().unwrap().remove(&machine_id).is_some();
        if removed {
            let _ = self.persist();
            self.publish();
        }
        removed
    }

    /// Забывает подключения машин, которых хаб больше не поручил этому носителю.
    pub fn forget_unassigned(&self, assigned: &HashSet<Uuid>) -> bool {
        let removed = {
            let mut map = self.attachments.lock().unwrap();
            let gone: Vec<Uuid> = map
                .keys()
                .filter(|id| !assigned.contains(id))
                .copied()
                .collect();
            for id in &gone {
                map.remove(id);
            }
            gone
        };
        if removed.is_empty() {
            return false;
        }
        let _ = self.persist();
        self.publish();
        true
    }

    pub fn subscribe(&self) -> watch::Receiver<Vec<CloudSpec>> {
        self.specs.subscribe()
    }

    pub fn specs(&self) -> Vec<CloudSpec> {
        self.specs.borrow().clone()
    }

    pub fn attachments(&self) -> Vec<Attachment> {
        self.attachments.lock().unwrap().values().cloned().collect()
    }

    pub fn outcomes(&self) -> Vec<AttachOutcome> {
        self.outcomes.lock().unwrap().clone()
    }

    pub fn changed(&self) -> &Notify {
        &self.changed
    }

    /// Итог подключения пишется в журнал и в список для тестов. Никаких секретов.
    pub fn report(&self, outcome: AttachOutcome) {
        if let AttachOutcome::Failed { code, .. } = &outcome {
            tracing::warn!(kind = "attach_failed", code, "cloud runner attach failed");
        } else {
            tracing::info!(kind = "attach_started", "cloud runner is up");
        }
        self.outcomes.lock().unwrap().push(outcome);
        self.changed.notify_waiters();
    }

    fn known_hosts_path(&self, attachment: &Attachment) -> PathBuf {
        let mut name = attachment.window_id.clone();
        name.retain(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if name.is_empty() {
            name = attachment.machine_id.to_string();
        }
        self.config
            .data_dir
            .join("cloud")
            .join(format!("{name}.known_hosts"))
    }

    fn publish(&self) {
        let specs = self
            .attachments
            .lock()
            .unwrap()
            .values()
            .filter_map(|attachment| {
                let port = if attachment.ssh_port == 0 {
                    22
                } else {
                    attachment.ssh_port
                };
                Some(CloudSpec {
                    window_id: attachment.window_id.clone(),
                    machine_id: attachment.machine_id,
                    host: attachment.host.clone(),
                    port,
                    user: self.config.user.clone(),
                    identity: self.config.ssh_key.clone(),
                    known_hosts: self.known_hosts_path(attachment),
                    root: attachment.root_dir.clone(),
                    max_running: attachment.max_running,
                    lease_slots: attachment.lease_slots,
                    ready_marker: self.config.ready_marker.clone(),
                    ready_timeout: self.config.ready_timeout,
                })
            })
            .collect();
        let _ = self.specs.send_replace(specs);
    }

    fn persist(&self) -> Result<(), AttachError> {
        let list: Vec<Attachment> = self.attachments.lock().unwrap().values().cloned().collect();
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent).map_err(|_| AttachError::Io)?;
        }
        let temp = self.file.with_extension("new");
        let bytes = serde_json::to_vec(&list).map_err(|_| AttachError::Io)?;
        std::fs::write(&temp, bytes).map_err(|_| AttachError::Io)?;
        std::fs::rename(&temp, &self.file).map_err(|_| AttachError::Io)
    }
}

/// Отвергает команды, которые нельзя безопасно вставить в скрипт или `known_hosts`.
fn valid(attachment: &Attachment) -> bool {
    !attachment.window_id.is_empty()
        && attachment
            .window_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !attachment.host.is_empty()
        && attachment
            .host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
        && !attachment.host_key_public.contains('\n')
        && attachment.host_key_public.starts_with("ssh-")
        && attachment.host_key_public.split(' ').count() >= 2
        && script::valid_root(&attachment.root_dir)
        && attachment.max_running > 0
        && attachment.lease_slots > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(window: &str) -> Attachment {
        Attachment {
            window_id: window.into(),
            machine_id: Uuid::now_v7(),
            host: "192.0.2.10".into(),
            ssh_port: 2222,
            host_key_public: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITEST".into(),
            root_dir: PathBuf::from("/srv/runner"),
            max_running: 3,
            lease_slots: 2,
        }
    }

    #[tokio::test]
    async fn persistence_and_idempotent_replay() {
        let dir = tempfile::Builder::new().prefix("cloud-").tempdir_in("/tmp").unwrap();
        let config = CloudConfig {
            data_dir: dir.path().to_path_buf(),
            ..CloudConfig::default()
        };
        let state = CloudState::open(config.clone());
        let first = attachment("window-1");
        state.apply(first.clone()).unwrap();
        let known = dir.path().join("cloud/window-1.known_hosts");
        assert_eq!(
            std::fs::read_to_string(&known).unwrap(),
            "[192.0.2.10]:2222 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITEST\n"
        );
        state.apply(first.clone()).unwrap();
        assert_eq!(state.attachments().len(), 1);
        assert_eq!(state.specs().len(), 1);
        assert_eq!(state.specs()[0].port, 2222);
        // Рестарт: состояние читается с диска.
        let resumed = CloudState::open(config);
        assert_eq!(resumed.attachments(), vec![first.clone()]);
        let mut assigned = HashSet::new();
        assert!(resumed.forget_unassigned(&assigned));
        assert!(resumed.attachments().is_empty());
        assert!(!resumed.forget_unassigned(&assigned));
        assigned.insert(first.machine_id);
        let second = Attachment {
            window_id: "window-2".into(),
            ..first.clone()
        };
        resumed.apply(second.clone()).unwrap();
        assert_eq!(resumed.attachments(), vec![second]);
    }

    #[test]
    fn unsafe_attachments_are_refused() {
        let dir = tempfile::Builder::new().prefix("cloud-").tempdir_in("/tmp").unwrap();
        let state = CloudState::open(CloudConfig {
            data_dir: dir.path().to_path_buf(),
            ..CloudConfig::default()
        });
        let mut bad = attachment("window 1");
        assert!(matches!(state.apply(bad.clone()), Err(AttachError::Invalid)));
        bad = attachment("w");
        bad.root_dir = PathBuf::from("/srv/../etc");
        assert!(matches!(state.apply(bad.clone()), Err(AttachError::Invalid)));
        bad = attachment("w");
        bad.host_key_public = "ssh-ed25519 AAAA\nmore".into();
        assert!(matches!(state.apply(bad), Err(AttachError::Invalid)));
        assert!(state.attachments().is_empty());
    }
}
