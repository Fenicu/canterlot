//! Аренда тяжёлых задач на машине: воркер держит очередь, `canterlot-worker lease run`
//! просит слот через локальный сокет и запускает команду с ограничениями ресурсов.
//!
//! Протокол v1 — строки JSON (`{"v":1,"op":...}`), одна строка на сообщение.

pub mod client;
pub mod scheduler;
pub mod server;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use scheduler::Kind;

pub const PROTOCOL_VERSION: u32 = 1;
/// Номер аренды, внутри которой идёт процесс: вложенный `lease run` запускает команду сразу.
pub const HELD_ENV: &str = "CANTERLOT_LEASE_HELD";
/// Бюджет потоков аренды: `check.sh` делит его между линтом и тестовой сборкой.
pub const BUDGET_ENV: &str = "CANTERLOT_LEASE_BUDGET";
pub const NO_LEASE_ENV: &str = "CANTERLOT_NO_LEASE";
/// Значение `CANTERLOT_LEASE_HELD` у команды, запущенной без аренды: вложенные скрипты
/// не стучатся в недоступный воркер второй раз.
pub const UNLEASED: &str = "unleased";
pub const SOCKET_NAME: &str = "lease.sock";
pub const LOCK_NAME: &str = "lease.lock";
pub const STATE_FILE: &str = "leases.json";

/// Сокет воркера: `<runtime>/canterlot/lease.sock`, без каталога времени выполнения —
/// `<data_dir>/lease.sock`.
pub fn socket_path(runtime_dir: Option<&Path>, data_dir: &Path) -> PathBuf {
    match runtime_dir {
        Some(runtime) => runtime.join("canterlot").join(SOCKET_NAME),
        None => data_dir.join(SOCKET_NAME),
    }
}

/// Бюджет потоков: `max(2, nproc * weight / slots)`.
pub fn budget(nproc: u32, weight: u32, slots: u32) -> u32 {
    let slots = slots.max(1);
    (nproc.saturating_mul(weight) / slots).max(2)
}

/// Воркеры Playwright: `max(1, (nproc / 2) * weight / slots)`.
pub fn playwright_workers(nproc: u32, weight: u32, slots: u32) -> u32 {
    let slots = slots.max(1);
    ((nproc / 2).saturating_mul(weight) / slots).max(1)
}

pub fn nproc() -> u32 {
    std::thread::available_parallelism()
        .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
        .unwrap_or(1)
}

pub fn now_ms() -> u64 {
    crate::machine::now_ms()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ClientMessage {
    Acquire {
        kind: Kind,
        #[serde(default)]
        label: String,
        /// Рабочий каталог клиента: в строке ожидания вместо названия сессии.
        #[serde(default)]
        dir: String,
        requested_at_ms: u64,
    },
    /// Держатель после рестарта воркера: снова следить за ним по соединению.
    Attach {
        lease_id: String,
    },
    Status,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Envelope<T> {
    pub v: u32,
    #[serde(flatten)]
    pub message: T,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct EntryView {
    pub lease_id: String,
    pub kind: String,
    pub weight: u32,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub dir: String,
    /// `execution` или `owner`.
    pub holder: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub wrapper_pid: u32,
    /// Состояние процесса обёртки из `/proc/<pid>/stat` (`S`, `R`, `T`…).
    #[serde(default)]
    pub state: String,
    pub requested_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
}

impl EntryView {
    /// Короткое имя для строки ожидания: метка, каталог и чей процесс.
    pub fn describe(&self) -> String {
        let who = match &self.execution_id {
            Some(id) => id.chars().take(8).collect::<String>(),
            None => "owner".into(),
        };
        let name = if self.label.is_empty() {
            self.dir.clone()
        } else if self.dir.is_empty() {
            self.label.clone()
        } else {
            format!("{} · {}", self.label, self.dir)
        };
        if name.is_empty() {
            format!("{} {who}", self.kind)
        } else {
            format!("{} «{name}» {who}", self.kind)
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Snapshot {
    pub slots: u32,
    pub holders: Vec<EntryView>,
    pub waiters: Vec<EntryView>,
    pub observed_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ServerMessage {
    Queued {
        lease_id: String,
        position: u32,
        holders: Vec<EntryView>,
    },
    Granted {
        lease_id: String,
        kind: Kind,
        weight: u32,
        slots: u32,
        budget: u32,
        playwright_workers: u32,
    },
    Attached {
        lease_id: String,
    },
    Status(Snapshot),
    Error {
        reason: String,
    },
}

pub fn encode<T: Serialize>(message: T) -> String {
    let mut line = serde_json::to_string(&Envelope {
        v: PROTOCOL_VERSION,
        message,
    })
    .expect("lease message serializes");
    line.push('\n');
    line
}

/// Строка другой версии или незнакомой формы — ошибка протокола.
pub fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, String> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|error| format!("invalid json: {error}"))?;
    match value.get("v").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(PROTOCOL_VERSION) => {}
        Some(v) => return Err(format!("unsupported protocol version {v}")),
        None => return Err("missing protocol version".into()),
    }
    let envelope: Envelope<T> =
        serde_json::from_value(value).map_err(|error| format!("invalid message: {error}"))?;
    Ok(envelope.message)
}

/// Снимок в сообщение хаба `LeaseState`.
pub fn to_proto(snapshot: &Snapshot) -> canterlot_proto::worker::v1::LeaseState {
    use canterlot_proto::worker::v1::{LeaseEntry, LeaseState};
    let entry = |view: &EntryView| LeaseEntry {
        lease_id: view.lease_id.clone(),
        kind: view.kind.clone(),
        weight: view.weight,
        label: view.label.clone(),
        execution_id: view.execution_id.clone().unwrap_or_default(),
        holder: view.holder.clone(),
        since_ms: view.since_ms.unwrap_or_default(),
        requested_at_ms: view.requested_at_ms,
        position: view.position.unwrap_or_default(),
        dir: view.dir.clone(),
    };
    LeaseState {
        slots: snapshot.slots,
        holders: snapshot.holders.iter().map(entry).collect(),
        waiters: snapshot.waiters.iter().map(entry).collect(),
        observed_at_ms: snapshot.observed_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_follow_spec_formula() {
        // (nproc, weight, slots) -> (budget, playwright)
        for (nproc, weight, slots, want_budget, want_pw) in [
            (36, 1, 2, 18, 9),
            (36, 2, 2, 36, 18),
            (36, 1, 3, 12, 6),
            (2, 1, 2, 2, 1),
            (1, 1, 1, 2, 1),
            (8, 1, 0, 8, 4),
        ] {
            assert_eq!(
                budget(nproc, weight, slots),
                want_budget,
                "{nproc}/{weight}/{slots}"
            );
            assert_eq!(
                playwright_workers(nproc, weight, slots),
                want_pw,
                "{nproc}/{weight}/{slots}"
            );
        }
    }

    #[test]
    fn protocol_lines_carry_version_and_reject_others() {
        let line = encode(ClientMessage::Acquire {
            kind: Kind::Gate,
            label: "gate".into(),
            dir: "canterlot".into(),
            requested_at_ms: 5,
        });
        assert!(line.ends_with('\n'));
        assert!(
            line.starts_with(r#"{"v":1,"op":"acquire","kind":"gate""#),
            "{line}"
        );
        assert_eq!(
            decode::<ClientMessage>(&line).unwrap(),
            ClientMessage::Acquire {
                kind: Kind::Gate,
                label: "gate".into(),
                dir: "canterlot".into(),
                requested_at_ms: 5
            }
        );
        assert!(
            decode::<ClientMessage>(r#"{"v":2,"op":"status"}"#)
                .unwrap_err()
                .contains("version 2")
        );
        assert!(decode::<ClientMessage>(r#"{"op":"status"}"#).is_err());
        assert!(decode::<ClientMessage>(r#"{"v":1,"op":"upgrade"}"#).is_err());
        assert_eq!(
            decode::<ClientMessage>(r#"{"v":1,"op":"status"}"#).unwrap(),
            ClientMessage::Status
        );
        let granted = encode(ServerMessage::Granted {
            lease_id: "x".into(),
            kind: Kind::Build,
            weight: 1,
            slots: 2,
            budget: 18,
            playwright_workers: 9,
        });
        assert!(matches!(
            decode::<ServerMessage>(&granted).unwrap(),
            ServerMessage::Granted { budget: 18, .. }
        ));
    }

    #[test]
    fn socket_lives_in_runtime_dir_or_data_dir() {
        assert_eq!(
            socket_path(Some(Path::new("/run/user/1")), Path::new("/data")),
            Path::new("/run/user/1/canterlot/lease.sock")
        );
        assert_eq!(
            socket_path(None, Path::new("/data")),
            Path::new("/data/lease.sock")
        );
    }

    #[test]
    fn describe_uses_label_dir_and_short_execution() {
        let mut view = EntryView {
            kind: "build".into(),
            label: "check.sh".into(),
            dir: "wt".into(),
            execution_id: Some("0123456789abcdef".into()),
            ..Default::default()
        };
        assert_eq!(view.describe(), "build «check.sh · wt» 01234567");
        view.execution_id = None;
        view.label.clear();
        assert_eq!(view.describe(), "build «wt» owner");
    }
}
