use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use canterlot_journal::{Journal, JournalError, JournalOptions, Record};
use canterlot_proto::limits::{MAX_INPUT_META_BYTES, MAX_INPUT_TEXT_BYTES, MAX_MESSAGE_BYTES};
use canterlot_proto::worker::v1::{
    CancelInput, Command, CommandStored, ContentBlock, DeliverInput, Interrupt, RecordBody,
    StartExecution, StopExecution, TextBlock, WithdrawInput, command, content_block, record_body,
};
use canterlot_worker::config::{ConfigError, ConfigErrorCategory, load, read_token};
use canterlot_worker::store::{Accept, StoreError, WorkerStore};
use prost::Message;
use tempfile::TempDir;
use uuid::Uuid;

fn private_dir() -> TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

fn delivery() -> Command {
    Command {
        command_id: Uuid::now_v7().to_string(),
        dispatch_attempt: 3,
        execution_id: Uuid::now_v7().to_string(),
        execution_generation: 7,
        kind: Some(command::Kind::DeliverInput(DeliverInput {
            input_id: Uuid::now_v7().to_string(),
            conversation_id: Uuid::now_v7().to_string(),
            content: vec![ContentBlock {
                block: Some(content_block::Block::Text(TextBlock {
                    text: "Привет, агент!".into(),
                })),
            }],
            ..Default::default()
        })),
    }
}

fn records(data_dir: &Path, name: &str) -> Vec<Record> {
    let (journal, report) = Journal::open(&data_dir.join(name), JournalOptions::default()).unwrap();
    assert!(report.corrupted_tail.is_none());
    journal.read_from(journal.first_seq(), usize::MAX).unwrap()
}

fn assert_receipt(record: &Record, cmd: &Command, class: &str) {
    let receipt: toml::Value =
        toml::from_str(std::str::from_utf8(&record.payload).unwrap()).unwrap();
    assert_eq!(
        receipt["command_id"].as_str(),
        Some(cmd.command_id.as_str())
    );
    assert_eq!(
        receipt["attempt"].as_integer(),
        Some(i64::from(cmd.dispatch_attempt))
    );
    assert_eq!(receipt["class"].as_str(), Some(class));
    assert_eq!(receipt["terminal"].as_bool(), Some(class == "rejected"));
    if class == "stored" {
        assert!(receipt["inbox_seq"].as_integer().is_some());
    }
}

#[test]
fn hundred_deliveries_store_once() {
    let dir = private_dir();
    let cmd = delivery();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert!(matches!(
        store.accept_command(&cmd).unwrap(),
        Accept::Stored { cj_seq: 1 }
    ));
    let mut duplicates = 0;
    for _ in 1..100 {
        assert!(matches!(
            store.accept_command(&cmd).unwrap(),
            Accept::Duplicate
        ));
        duplicates += 1;
    }
    assert_eq!(duplicates, 99);
    let cj = store.cj_records_from(1, usize::MAX).unwrap();
    assert_eq!(cj.len(), 1);
    assert_eq!(cj[0].seq, 1);
    assert!(matches!(
        &cj[0].body.as_ref().unwrap().body,
        Some(record_body::Body::CommandStored(CommandStored { command_id, dispatch_attempt, machine_time_ms }))
            if command_id == &cmd.command_id && *dispatch_attempt == cmd.dispatch_attempt
                && *machine_time_ms > 0 && *machine_time_ms <= chrono::Utc::now().timestamp_millis() as u64
    ));
    drop(store);
    let inbox = records(dir.path(), "inbox");
    assert_eq!(inbox.len(), 1);
    assert_eq!(Command::decode(inbox[0].payload.as_slice()).unwrap(), cmd);
    let cj_disk = records(dir.path(), "cj");
    assert_eq!(cj_disk.len(), 1);
    assert_eq!(
        RecordBody::decode(cj_disk[0].payload.as_slice()).unwrap(),
        *cj[0].body.as_ref().unwrap()
    );
    let receipts = records(dir.path(), "receipts");
    assert_eq!(receipts.len(), 1);
    assert_receipt(&receipts[0], &cmd, "stored");
}

#[test]
fn reopen_keeps_dedup_index() {
    let dir = private_dir();
    let mut cmd = delivery();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let id = store.cj_journal_id().unwrap();
    store.accept_command(&cmd).unwrap();
    drop(store);
    cmd.kind = None;
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.cj_journal_id().unwrap(), id);
    assert!(matches!(
        store.accept_command(&cmd).unwrap(),
        Accept::Duplicate
    ));
    assert_eq!(store.cj_cursor().unwrap().last_seq, 1);
    drop(store);
    assert_eq!(records(dir.path(), "inbox").len(), 1);
    assert_eq!(records(dir.path(), "receipts").len(), 1);
}

#[test]
fn unknown_command_kind_is_rejected_not_defaulted() {
    let dir = private_dir();
    let mut cmd = delivery();
    cmd.kind = None;
    let mut bytes = cmd.encode_to_vec();
    bytes.extend_from_slice(&[0x9a, 0x06, 0x00]);
    let cmd = Command::decode(bytes.as_slice()).unwrap();
    assert!(cmd.kind.is_none());
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert!(matches!(
        store.accept_command(&cmd).unwrap(),
        Accept::Rejected {
            cj_seq: 1,
            reason: "unsupported_command",
        }
    ));
    let cj = store.cj_records_from(1, usize::MAX).unwrap();
    assert_eq!(cj.len(), 1);
    let Some(record_body::Body::CommandRejected(rejected)) = &cj[0].body.as_ref().unwrap().body
    else {
        panic!("expected command_rejected");
    };
    assert_eq!(rejected.command_id, cmd.command_id);
    assert_eq!(rejected.dispatch_attempt, cmd.dispatch_attempt);
    assert_eq!(rejected.reason, "unsupported_command");
    assert!(matches!(
        store.accept_command(&cmd).unwrap(),
        Accept::Duplicate
    ));
    drop(store);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert!(matches!(
        store.accept_command(&cmd).unwrap(),
        Accept::Duplicate
    ));
    drop(store);
    assert!(records(dir.path(), "inbox").is_empty());
    let receipts = records(dir.path(), "receipts");
    assert_eq!(receipts.len(), 1);
    assert_receipt(&receipts[0], &cmd, "rejected");
}

#[test]
fn ack_compacts_cj_but_keeps_receipts() {
    let dir = private_dir();
    let cmd = delivery();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    store.accept_command(&cmd).unwrap();
    let id = store.cj_journal_id().unwrap();
    store.on_cj_ack(1).unwrap();
    assert_eq!(store.cj_cursor().unwrap().first_stored_seq, 2);
    assert_eq!(store.cj_cursor().unwrap().last_seq, 1);
    assert_eq!(store.cj_cursor().unwrap().acked_seq_known_to_worker, 1);
    assert!(store.cj_records_from(2, usize::MAX).unwrap().is_empty());
    drop(store);
    assert!(records(dir.path(), "cj").is_empty());
    assert_eq!(records(dir.path(), "inbox").len(), 1);
    let receipts = records(dir.path(), "receipts");
    assert_eq!(receipts.len(), 1);
    assert_receipt(&receipts[0], &cmd, "stored");
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.cj_journal_id().unwrap(), id);
    assert_eq!(store.cj_cursor().unwrap().acked_seq_known_to_worker, 1);
    assert!(matches!(
        store.accept_command(&cmd).unwrap(),
        Accept::Duplicate
    ));
    store.on_cj_ack(1).unwrap();
    store.on_cj_ack(0).unwrap();
    assert_eq!(store.cj_cursor().unwrap().first_stored_seq, 2);
    assert!(matches!(
        store.accept_command(&delivery()).unwrap(),
        Accept::Stored { cj_seq: 2 }
    ));
}

#[test]
fn all_known_command_kinds_store_full_payloads() {
    let dir = private_dir();
    let kinds = [
        delivery().kind.unwrap(),
        command::Kind::CancelInput(CancelInput {
            target_command_id: Uuid::now_v7().to_string(),
            target_dispatch_attempt: 4,
        }),
        command::Kind::WithdrawInput(WithdrawInput {
            target_command_id: Uuid::now_v7().to_string(),
            target_dispatch_attempt: 5,
        }),
        command::Kind::Interrupt(Interrupt {}),
        command::Kind::StartExecution(StartExecution {
            conversation_id: Uuid::now_v7().to_string(),
            harness: "codex".into(),
            workdir: "/tmp/work".into(),
            native_conversation_id: "native".into(),
            ..Default::default()
        }),
        command::Kind::StopExecution(StopExecution { force: true }),
    ];
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let mut commands = Vec::new();
    for (index, kind) in kinds.into_iter().enumerate() {
        let cmd = Command {
            kind: Some(kind),
            ..delivery()
        };
        assert!(
            matches!(store.accept_command(&cmd).unwrap(), Accept::Stored { cj_seq } if cj_seq == index as u64 + 1)
        );
        commands.push(cmd);
    }
    assert_eq!(
        store.cj_records_from(1, usize::MAX).unwrap().len(),
        commands.len()
    );
    drop(store);
    let inbox: Vec<_> = records(dir.path(), "inbox")
        .into_iter()
        .map(|record| Command::decode(record.payload.as_slice()).unwrap())
        .collect();
    assert_eq!(inbox, commands);
    assert_eq!(records(dir.path(), "receipts").len(), 6);
}

#[test]
fn cursor_and_ack_preserve_unacknowledged_records() {
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let cursor = store.cj_cursor().unwrap();
    assert_eq!(
        cursor.journal_id,
        store.cj_journal_id().unwrap().to_string()
    );
    assert_eq!(
        (
            cursor.first_stored_seq,
            cursor.last_seq,
            cursor.acked_seq_known_to_worker
        ),
        (1, 0, 0)
    );
    for _ in 0..3 {
        store.accept_command(&delivery()).unwrap();
    }
    let second = store.cj_records_from(2, 1).unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].seq, 2);
    assert!(matches!(
        store.on_cj_ack(4),
        Err(StoreError::CjAckBeyondEnd)
    ));
    assert!(store.cj_cursor().is_ok());
    drop(store);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.cj_cursor().unwrap().acked_seq_known_to_worker, 0);
    store.on_cj_ack(1).unwrap();
    let suffix = store.cj_records_from(2, usize::MAX).unwrap();
    assert_eq!(suffix.iter().map(|r| r.seq).collect::<Vec<_>>(), [2, 3]);
}

#[test]
fn sealed_cj_refuses_open_even_after_quarantine() {
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    store.accept_command(&delivery()).unwrap();
    drop(store);
    let segment = fs::read_dir(dir.path().join("cj"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "log"))
        .unwrap();
    let mut file = OpenOptions::new().append(true).open(segment).unwrap();
    file.write_all(b"torn tail").unwrap();
    file.sync_all().unwrap();
    for _ in 0..2 {
        assert!(matches!(
            WorkerStore::open(dir.path()),
            Err(StoreError::CjSealed)
        ));
    }
}

#[test]
fn journals_have_one_owner() {
    let dir = private_dir();
    let store = WorkerStore::open(dir.path()).unwrap();
    assert!(matches!(
        WorkerStore::open(dir.path()),
        Err(StoreError::Journal(JournalError::Locked))
    ));
    drop(store);
    assert!(WorkerStore::open(dir.path()).is_ok());
}

#[test]
fn over_limit_commands_write_only_durable_rejections() {
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let mut text = delivery();
    if let Some(command::Kind::DeliverInput(input)) = &mut text.kind {
        input.content = vec![ContentBlock {
            block: Some(content_block::Block::Text(TextBlock {
                text: "я".repeat(MAX_INPUT_TEXT_BYTES / 2 + 1),
            })),
        }];
    }
    assert!(matches!(
        store.accept_command(&text).unwrap(),
        Accept::Rejected {
            reason: "resource_limit",
            ..
        }
    ));
    let mut meta = delivery();
    meta.execution_id = "x".repeat(MAX_INPUT_META_BYTES + 1);
    assert!(matches!(
        store.accept_command(&meta).unwrap(),
        Accept::Rejected {
            reason: "resource_limit",
            ..
        }
    ));
    let mut message = delivery();
    message.kind = Some(command::Kind::StartExecution(StartExecution {
        workdir: "x".repeat(MAX_MESSAGE_BYTES),
        ..Default::default()
    }));
    assert!(matches!(
        store.accept_command(&message).unwrap(),
        Accept::Rejected {
            reason: "resource_limit",
            ..
        }
    ));
    assert_eq!(store.cj_cursor().unwrap().last_seq, 3);
    drop(store);
    assert!(records(dir.path(), "inbox").is_empty());
    assert_eq!(records(dir.path(), "cj").len(), 3);
    assert_eq!(records(dir.path(), "receipts").len(), 3);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.accept_command(&text).unwrap(), Accept::Duplicate);
}

#[test]
fn restore_generation_is_persisted_in_hub_toml() {
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.restore_generation().unwrap(), None);
    for _ in 0..2 {
        let generation = Uuid::now_v7();
        store.set_restore_generation(generation).unwrap();
        assert_eq!(store.restore_generation().unwrap(), Some(generation));
        let hub: toml::Value =
            toml::from_str(&fs::read_to_string(dir.path().join("hub.toml")).unwrap()).unwrap();
        assert_eq!(
            hub["restore_generation"].as_str(),
            Some(generation.to_string().as_str())
        );
        drop(store);
        store = WorkerStore::open(dir.path()).unwrap();
        assert_eq!(store.restore_generation().unwrap(), Some(generation));
    }
}

#[test]
fn malformed_receipts_refuse_open() {
    let dir = private_dir();
    drop(WorkerStore::open(dir.path()).unwrap());
    let (mut receipts, _) =
        Journal::open(&dir.path().join("receipts"), JournalOptions::default()).unwrap();
    receipts.append(b"not a receipt").unwrap();
    drop(receipts);
    assert!(WorkerStore::open(dir.path()).is_err());
}

#[cfg(unix)]
#[test]
fn token_file_permissions_are_enforced() {
    use std::os::unix::fs::PermissionsExt;
    let dir = private_dir();
    let path = dir.path().join("token");
    fs::write(&path, "test-token\n").unwrap();
    for mode in [0o600, 0o400] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(read_token(&path).unwrap(), "test-token");
    }
    for mode in [0o644, 0o640, 0o604, 0o610, 0o601] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        assert!(read_token(&path).is_err(), "mode {mode:o}");
    }
}

fn config_text() -> String {
    format!(
        "hub_url = \"https://hub.example:8443\"\nca_cert = \"ca.pem\"\ntoken_file = \"token\"\nmachine_id = \"{}\"\ninstallation_id = \"{}\"\nrestore_generation = \"{}\"\ndata_dir = \"data\"\n",
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7()
    )
}

#[test]
fn config_loads_with_defaults() {
    let dir = private_dir();
    let path = dir.path().join("worker.toml");
    fs::write(&path, config_text()).unwrap();
    let config = load(&path).unwrap();
    assert_eq!(config.hub_url, "https://hub.example:8443");
    assert_eq!(config.ca_cert, Path::new("ca.pem"));
    assert_eq!(config.token_file, Path::new("token"));
    assert_eq!(config.data_dir, Path::new("data"));
    assert_eq!(config.machine_id.get_version_num(), 7);
    assert_eq!(config.installation_id.get_version_num(), 7);
    assert_eq!(config.restore_generation.get_version_num(), 7);
    assert_eq!(config.timers.handshake, Duration::from_secs(10));
    assert_eq!(config.timers.heartbeat, Duration::from_secs(5));
    assert_eq!(config.timers.heartbeat_ack, Duration::from_secs(5));
    assert_eq!(config.timers.range_ack, Duration::from_secs(10));
    assert_eq!(config.timers.backoff_min, Duration::from_millis(250));
    assert_eq!(config.timers.backoff_max, Duration::from_secs(30));
    assert_eq!(config.timers.backoff_reset_after, Duration::from_secs(30));
    assert_eq!(config.execution.build_jobs, 6);
}

#[test]
fn config_execution_build_jobs_can_be_set() {
    let dir = private_dir();
    let path = dir.path().join("worker.toml");
    fs::write(
        &path,
        format!("{}\n[execution]\nbuild_jobs = 2\n", config_text()),
    )
    .unwrap();
    let config = load(&path).unwrap();
    assert_eq!(config.execution.build_jobs, 2);
}

#[test]
fn config_timers_use_humantime_and_allow_partial_overrides() {
    let dir = private_dir();
    let path = dir.path().join("worker.toml");
    fs::write(
        &path,
        format!(
            "{}\n[timers]\nheartbeat = '25ms'\nbackoff_min = '1ms'\n",
            config_text()
        ),
    )
    .unwrap();
    let config = load(&path).unwrap();
    assert_eq!(config.timers.heartbeat, Duration::from_millis(25));
    assert_eq!(config.timers.backoff_min, Duration::from_millis(1));
    assert_eq!(config.timers.handshake, Duration::from_secs(10));
}

#[test]
fn config_requires_https_and_valid_fields() {
    let dir = private_dir();
    let path = dir.path().join("worker.toml");
    for contents in [
        config_text().replace("https://", "http://"),
        config_text().replace("https://hub.example:8443", "https://"),
        "hub_url = \"https://hub.example\"".into(),
    ] {
        fs::write(&path, contents).unwrap();
        assert!(load(&path).is_err());
    }
}

fn assert_error_redacted(error: &(dyn std::error::Error + 'static), markers: &[&str]) {
    assert!(error.source().is_none());
    let mut current = Some(error);
    while let Some(error) = current {
        for rendered in [error.to_string(), format!("{error:?}")] {
            for marker in markers {
                assert!(!rendered.contains(marker), "error exposed file contents");
            }
        }
        current = error.source();
    }
}

fn sensitive_config_inputs() -> Vec<String> {
    vec![
        "secret-token-marker-never-print".into(),
        config_text().replace(
            "hub_url = \"https://hub.example:8443\"",
            "hub_url = \"https://secret-user:secret-pass@[\" trailing-secret-marker",
        ),
        config_text().replace(
            "\"https://hub.example:8443\"",
            "[\"https://secret-user:secret-pass@[\"]",
        ),
        config_text().replace(
            "https://hub.example:8443",
            "https://secret-user:secret-pass@[",
        ),
        config_text().replace(
            "https://hub.example:8443",
            "https://secret-user:secret-pass@hub.example:8443",
        ),
    ]
}

#[test]
fn config_errors_do_not_expose_file_contents() {
    let dir = private_dir();
    let path = dir.path().join("worker.toml");
    for contents in sensitive_config_inputs() {
        fs::write(&path, contents).unwrap();
        let error = load(&path).unwrap_err();
        assert!(error.to_string().contains(path.to_str().unwrap()));
        assert_eq!(error.path, path);
        assert!(match &error.category {
            ConfigErrorCategory::Parse => {
                matches!(error.location, Some((line, column)) if line > 0 && column > 0)
            }
            ConfigErrorCategory::HubUrl => error.location.is_none(),
            _ => false,
        });
        assert_error_redacted(
            &error,
            &[
                "secret-token-marker",
                "secret-user",
                "secret-pass",
                "trailing-secret-marker",
            ],
        );
    }
}

#[test]
fn config_error_conversions_discard_source_errors() {
    let marker = "secret-token-marker";
    let error = ConfigError::from(toml::from_str::<toml::Value>(marker).unwrap_err());
    assert_error_redacted(&error, &[marker]);
    let ConfigError {
        path,
        category,
        location,
    } = error;
    assert!(path.as_os_str().is_empty());
    assert_eq!(category, ConfigErrorCategory::Parse);
    assert_eq!(location, None);
    let error = ConfigError::from(std::io::Error::other(marker));
    assert_error_redacted(&error, &[marker]);
    assert!(error.path.as_os_str().is_empty());
    assert_eq!(
        error.category,
        ConfigErrorCategory::Io(std::io::ErrorKind::Other)
    );
    assert_eq!(error.location, None);
}

#[test]
fn config_io_errors_keep_path_and_kind() {
    let dir = private_dir();
    let path = dir.path().join("missing");
    for error in [load(&path).unwrap_err(), read_token(&path).unwrap_err()] {
        assert_eq!(error.path, path);
        assert_eq!(
            error.category,
            ConfigErrorCategory::Io(std::io::ErrorKind::NotFound)
        );
        assert_eq!(error.location, None);
        assert_error_redacted(&error, &[]);
    }
    fs::write(&path, b"secret-token-marker\xff").unwrap();
    let error = load(&path).unwrap_err();
    assert_eq!(error.path, path);
    assert_eq!(
        error.category,
        ConfigErrorCategory::Io(std::io::ErrorKind::InvalidData)
    );
    assert_eq!(error.location, None);
    assert_error_redacted(&error, &["secret-token-marker"]);
}

#[test]
fn binary_config_errors_do_not_expose_file_contents() {
    let dir = private_dir();
    let path = dir.path().join("worker.toml");
    for contents in sensitive_config_inputs() {
        fs::write(&path, contents).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
            .args(["run", "--config"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        for marker in [
            "secret-token-marker",
            "secret-user",
            "secret-pass",
            "trailing-secret-marker",
        ] {
            assert!(!stderr.contains(marker), "stderr exposed file contents");
        }
    }
}

#[cfg(unix)]
#[test]
fn binary_checks_config_and_token_before_starting_session() {
    use std::os::unix::fs::PermissionsExt;

    let dir = private_dir();
    fs::write(dir.path().join("worker.toml"), config_text()).unwrap();
    let token = dir.path().join("token");
    fs::write(&token, "test-token-that-must-not-be-logged\n").unwrap();
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
            .current_dir(dir.path())
            .args(["run", "--config", "worker.toml"])
            .output()
            .unwrap()
    };
    fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
    let output = run();
    assert!(!output.status.success());
    assert!(!dir.path().join("data").exists());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("test-token"));
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let output = run();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("test-token"));
    for name in ["cj", "inbox", "receipts"] {
        assert!(dir.path().join("data").join(name).join("header").is_file());
    }
}

#[test]
fn config_requires_valid_restore_generation() {
    let dir = private_dir();
    let path = dir.path().join("worker.toml");
    let text = config_text();
    let missing = text
        .lines()
        .filter(|line| !line.starts_with("restore_generation"))
        .collect::<Vec<_>>()
        .join("\n");
    for contents in [
        missing.clone(),
        format!("{missing}\nrestore_generation = 'invalid'\n"),
    ] {
        fs::write(&path, contents).unwrap();
        assert!(load(&path).is_err());
    }
}

#[test]
fn configs_reject_unknown_fields_and_numeric_durations() {
    let dir = private_dir();
    let path = dir.path().join("worker.toml");
    for extra in [
        "handshake = 10",
        "[timer]\nheartbeat = '5s'",
        "[timers]\nheartbeet = '5s'",
        "[timers]\nheartbeat = 5",
    ] {
        fs::write(&path, format!("{}\n{extra}\n", config_text())).unwrap();
        assert!(load(&path).is_err(), "accepted {extra}");
    }
}

#[cfg(unix)]
#[test]
fn worker_store_creates_private_directories_and_files() {
    use std::os::unix::fs::PermissionsExt;
    fn check(path: &Path) {
        let metadata = fs::metadata(path).unwrap();
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            if metadata.is_dir() { 0o700 } else { 0o600 },
            "{}",
            path.display()
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                check(&entry.unwrap().path());
            }
        }
    }
    let dir = private_dir();
    let path = dir.path().join("nested/store");
    let mut store = WorkerStore::open(&path).unwrap();
    store.accept_command(&delivery()).unwrap();
    store.set_restore_generation(Uuid::now_v7()).unwrap();
    check(&path);
    store.on_cj_ack(1).unwrap();
    check(&path);
    drop(store);
    WorkerStore::open(&path).unwrap();
    check(&path);
}

#[cfg(unix)]
#[test]
fn worker_store_refuses_insecure_existing_data_dir_without_chmod() {
    use std::os::unix::fs::PermissionsExt;
    let dir = private_dir();
    for mode in [0o750, 0o707, 0o755, 0o777] {
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(mode)).unwrap();
        let result = WorkerStore::open(dir.path());
        assert!(result.is_err(), "insecure data_dir accepted");
        let error = result.err().unwrap();
        assert_eq!(error.to_string(), "insecure_data_dir");
        assert_eq!(
            error.kind(),
            canterlot_worker::session::StoreErrorKind::InsecureDataDir
        );
        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            mode
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(WorkerStore::open(dir.path()).is_ok());
}

#[test]
fn redispatched_attempt_gets_a_new_stored_receipt() {
    use canterlot_worker::store::ReceiptClass;
    let dir = private_dir();
    let mut cmd = delivery();
    cmd.dispatch_attempt = 1;
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(
        store.accept_command(&cmd).unwrap(),
        Accept::Stored { cj_seq: 1 }
    );
    let first = cmd.clone();
    cmd.dispatch_attempt = 2;
    cmd.execution_generation += 1;
    assert_eq!(
        store.accept_command(&cmd).unwrap(),
        Accept::Redelivered { cj_seq: 2 }
    );
    for _ in 0..100 {
        assert_eq!(store.accept_command(&cmd).unwrap(), Accept::Duplicate);
    }
    let state = store.command_state(&cmd.command_id).unwrap().unwrap();
    assert_eq!(state.attempt, 2);
    assert_eq!(state.class, ReceiptClass::Stored);
    assert_eq!(state.inbox_seq, Some(2));
    assert!(!state.terminal);
    assert_eq!(
        store.inbox_command(&cmd.command_id, 2).unwrap(),
        Some(cmd.clone())
    );
    assert_eq!(store.inbox_command(&cmd.command_id, 1).unwrap(), None);
    let cj = store.cj_records_from(1, usize::MAX).unwrap();
    assert_eq!(cj.len(), 2);
    for (record, attempt) in cj.iter().zip([1, 2]) {
        assert!(matches!(&record.body.as_ref().unwrap().body,
            Some(record_body::Body::CommandStored(fact)) if fact.dispatch_attempt == attempt));
    }
    drop(store);
    let inbox: Vec<_> = records(dir.path(), "inbox")
        .iter()
        .map(|record| Command::decode(record.payload.as_slice()).unwrap())
        .collect();
    assert_eq!(inbox, [first, cmd.clone()]);
    assert_eq!(records(dir.path(), "receipts").len(), 2);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.command_state(&cmd.command_id).unwrap(), Some(state));
    assert_eq!(store.accept_command(&cmd).unwrap(), Accept::Duplicate);
}

#[test]
fn stale_attempt_is_ignored_without_write() {
    let dir = private_dir();
    let mut cmd = delivery();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    store.accept_command(&cmd).unwrap();
    let state = store.command_state(&cmd.command_id).unwrap();
    cmd.dispatch_attempt -= 1;
    cmd.kind = None;
    assert_eq!(store.accept_command(&cmd).unwrap(), Accept::Stale);
    assert_eq!(store.command_state(&cmd.command_id).unwrap(), state);
    assert_eq!(store.cj_cursor().unwrap().last_seq, 1);
    drop(store);
    assert_eq!(records(dir.path(), "inbox").len(), 1);
    assert_eq!(records(dir.path(), "receipts").len(), 1);
}

#[test]
fn higher_attempt_after_transmission_is_rejected() {
    use canterlot_proto::worker::v1::InputTransmissionStarted;
    use canterlot_worker::store::ReceiptClass;
    let dir = private_dir();
    let mut cmd = delivery();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    store.accept_command(&cmd).unwrap();
    assert_eq!(
        store
            .append_cj_fact(record_body::Body::TransmissionStarted(
                InputTransmissionStarted {
                    command_id: cmd.command_id.clone(),
                    dispatch_attempt: cmd.dispatch_attempt,
                    machine_time_ms: 1,
                }
            ))
            .unwrap(),
        2
    );
    store.on_cj_ack(2).unwrap();
    drop(store);
    for attempt in [4, 5] {
        let mut store = WorkerStore::open(dir.path()).unwrap();
        cmd.dispatch_attempt = attempt;
        assert!(matches!(
            store.accept_command(&cmd).unwrap(),
            Accept::Rejected {
                reason: "already_transmitted",
                ..
            }
        ));
        let state = store.command_state(&cmd.command_id).unwrap().unwrap();
        assert_eq!(state.attempt, attempt);
        assert_eq!(state.class, ReceiptClass::Rejected);
        assert_eq!(store.accept_command(&cmd).unwrap(), Accept::Duplicate);
        store
            .on_cj_ack(store.cj_cursor().unwrap().last_seq)
            .unwrap();
    }
    let (inbox, _) = Journal::open(&dir.path().join("inbox"), JournalOptions::default()).unwrap();
    assert_eq!(inbox.last_seq(), 1);
}

#[cfg(feature = "fault-injection")]
#[test]
fn crash_between_redelivery_appends_is_reconciled() {
    use canterlot_worker::fault_injection::{StoreAppendPoint, StoreProbe};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    for point in [
        StoreAppendPoint::Inbox,
        StoreAppendPoint::Cj,
        StoreAppendPoint::Receipt,
    ] {
        for repair in [
            None,
            Some(StoreAppendPoint::Cj),
            Some(StoreAppendPoint::Receipt),
        ] {
            if point != StoreAppendPoint::Inbox && repair == Some(StoreAppendPoint::Cj) {
                continue;
            }
            if point == StoreAppendPoint::Receipt && repair.is_some() {
                continue;
            }
            let dir = private_dir();
            let mut cmd = delivery();
            cmd.dispatch_attempt = 1;
            let mut store = WorkerStore::open(dir.path()).unwrap();
            store.accept_command(&cmd).unwrap();
            cmd.dispatch_attempt = 2;
            let probe = StoreProbe::default();
            probe.crash_after(point);
            store = store.with_probe(probe);
            assert!(catch_unwind(AssertUnwindSafe(|| store.accept_command(&cmd))).is_err());
            drop(store);
            if let Some(repair) = repair {
                let probe = StoreProbe::default();
                probe.crash_after(repair);
                assert!(catch_unwind(|| WorkerStore::open_with_probe(dir.path(), probe)).is_err());
            }
            let mut store = WorkerStore::open(dir.path()).unwrap();
            assert_eq!(
                store
                    .command_state(&cmd.command_id)
                    .unwrap()
                    .unwrap()
                    .attempt,
                2
            );
            assert_eq!(store.cj_records_from(1, usize::MAX).unwrap().len(), 2);
            assert_eq!(store.accept_command(&cmd).unwrap(), Accept::Duplicate);
            drop(store);
            assert_eq!(records(dir.path(), "inbox").len(), 2);
            assert_eq!(records(dir.path(), "receipts").len(), 2);
        }
    }
}

#[cfg(feature = "fault-injection")]
#[test]
fn open_scans_only_the_unreferenced_inbox_tail() {
    use canterlot_worker::fault_injection::StoreProbe;
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    for _ in 0..20 {
        store.accept_command(&delivery()).unwrap();
    }
    drop(store);
    let probe = StoreProbe::default();
    let store = WorkerStore::open_with_probe(dir.path(), probe.clone()).unwrap();
    assert_eq!(probe.scanned_inbox_records(), 0);
    drop(store);
    let (mut inbox, _) =
        Journal::open(&dir.path().join("inbox"), JournalOptions::default()).unwrap();
    let cmd = delivery();
    inbox.append(&cmd.encode_to_vec()).unwrap();
    drop(inbox);
    let probe = StoreProbe::default();
    let store = WorkerStore::open_with_probe(dir.path(), probe.clone()).unwrap();
    assert_eq!(probe.scanned_inbox_records(), 1);
    assert_eq!(
        store
            .inbox_command(&cmd.command_id, cmd.dispatch_attempt)
            .unwrap(),
        Some(cmd)
    );
}

#[test]
fn terminal_commands_release_inbox_prefix() {
    use canterlot_journal::JournalId;
    let dir = private_dir();
    let mut inbox = Journal::create(
        &dir.path().join("inbox"),
        JournalId(Uuid::now_v7()),
        JournalOptions {
            segment_max_bytes: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let commands: Vec<_> = (0..4).map(|_| delivery()).collect();
    for cmd in &commands {
        inbox.append(&cmd.encode_to_vec()).unwrap();
    }
    drop(inbox);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    store
        .mark_terminal(&commands[1].command_id, commands[1].dispatch_attempt)
        .unwrap();
    drop(store);
    let (inbox, _) = Journal::open(&dir.path().join("inbox"), JournalOptions::default()).unwrap();
    assert_eq!(inbox.acked_seq(), 0);
    assert_eq!(inbox.first_seq(), 1);
    drop(inbox);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    store
        .mark_terminal(&commands[0].command_id, commands[0].dispatch_attempt - 1)
        .unwrap();
    assert!(
        !store
            .command_state(&commands[0].command_id)
            .unwrap()
            .unwrap()
            .terminal
    );
    store
        .mark_terminal(&commands[0].command_id, commands[0].dispatch_attempt)
        .unwrap();
    assert_eq!(
        store
            .inbox_command(&commands[0].command_id, commands[0].dispatch_attempt)
            .unwrap(),
        None
    );
    drop(store);
    let (inbox, _) = Journal::open(&dir.path().join("inbox"), JournalOptions::default()).unwrap();
    assert_eq!(inbox.acked_seq(), 2);
    assert_eq!(inbox.first_seq(), 3);
    drop(inbox);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    for cmd in &commands[2..] {
        store
            .mark_terminal(&cmd.command_id, cmd.dispatch_attempt)
            .unwrap();
    }
    drop(store);
    let (inbox, _) = Journal::open(&dir.path().join("inbox"), JournalOptions::default()).unwrap();
    assert_eq!(inbox.acked_seq(), 4);
    assert_eq!(inbox.first_seq(), 5);
    drop(inbox);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    for cmd in &commands {
        assert!(
            store
                .command_state(&cmd.command_id)
                .unwrap()
                .unwrap()
                .terminal
        );
        assert_eq!(store.accept_command(cmd).unwrap(), Accept::Duplicate);
    }
}

fn ownership_record(execution_id: Uuid) -> canterlot_proto::worker::ipc::Ownership {
    use canterlot_proto::worker::ipc::{Ownership, OwnershipPhase};
    Ownership {
        execution_id: execution_id.to_string(),
        generation: 1,
        phase: OwnershipPhase::Spawning as i32,
        exec_dir: "/private/execution".into(),
        ipc_socket: "/private/execution/ipc.sock".into(),
        secret_sha256: vec![42; 32],
        ..Default::default()
    }
}

#[test]
fn ownership_records_survive_reopen_latest_wins() {
    use canterlot_proto::worker::ipc::OwnershipPhase;
    let dir = private_dir();
    let id = Uuid::now_v7();
    let other = Uuid::now_v7();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let mut record = ownership_record(id);
    store
        .ownership_mut()
        .unwrap()
        .append(record.clone())
        .unwrap();
    store
        .ownership_mut()
        .unwrap()
        .append(ownership_record(other))
        .unwrap();
    record.phase = OwnershipPhase::Attached as i32;
    record.supervisor_pid = 123;
    store
        .ownership_mut()
        .unwrap()
        .append(record.clone())
        .unwrap();
    drop(store);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.ownership().get(id), Some(&record));
    assert_eq!(store.ownership().live().len(), 2);
    record.phase = OwnershipPhase::Released as i32;
    record.released_reason = "stopped".into();
    store
        .ownership_mut()
        .unwrap()
        .append(record.clone())
        .unwrap();
    drop(store);
    let store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.ownership().get(id), Some(&record));
    assert_eq!(store.ownership().live().len(), 1);
    assert_eq!(store.ownership().live()[0].execution_id, other.to_string());
}

#[test]
fn ownership_journal_is_private_and_never_in_cj() {
    use std::os::unix::fs::PermissionsExt;
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    store.accept_command(&delivery()).unwrap();
    let before = store.cj_records_from(1, usize::MAX).unwrap();
    store
        .ownership_mut()
        .unwrap()
        .append(ownership_record(Uuid::now_v7()))
        .unwrap();
    assert_eq!(store.cj_records_from(1, usize::MAX).unwrap(), before);
    assert!(matches!(
        Journal::open(&dir.path().join("ownership"), JournalOptions::default()),
        Err(JournalError::Locked)
    ));
    assert_eq!(
        fs::metadata(dir.path().join("ownership"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for file in fs::read_dir(dir.path().join("ownership")).unwrap() {
        assert_eq!(
            file.unwrap().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    store.on_cj_ack(1).unwrap();
    drop(store);
    let (ownership, _) =
        Journal::open(&dir.path().join("ownership"), JournalOptions::default()).unwrap();
    assert_eq!(ownership.acked_seq(), 0);
    assert_eq!(ownership.last_seq(), 1);
}

#[cfg(feature = "fault-injection")]
#[test]
fn legacy_receipts_are_upgraded_with_one_full_inbox_scan() {
    use canterlot_worker::fault_injection::StoreProbe;
    let dir = private_dir();
    let cmd = delivery();
    let mut rejected = delivery();
    rejected.kind = None;
    let mut store = WorkerStore::open(dir.path()).unwrap();
    store.accept_command(&cmd).unwrap();
    store.accept_command(&rejected).unwrap();
    store.on_cj_ack(2).unwrap();
    drop(store);
    let (mut receipts, _) =
        Journal::open(&dir.path().join("receipts"), JournalOptions::default()).unwrap();
    for (command, class) in [(&cmd, "stored"), (&rejected, "rejected")] {
        receipts
            .append(
                format!(
                    "command_id = '{}'\nattempt = {}\nclass = '{}'\n",
                    command.command_id, command.dispatch_attempt, class
                )
                .as_bytes(),
            )
            .unwrap();
    }
    drop(receipts);
    let probe = StoreProbe::default();
    let mut store = WorkerStore::open_with_probe(dir.path(), probe.clone()).unwrap();
    assert_eq!(probe.scanned_inbox_records(), 1);
    assert_eq!(
        store
            .command_state(&cmd.command_id)
            .unwrap()
            .unwrap()
            .inbox_seq,
        Some(1)
    );
    assert_eq!(
        store
            .inbox_command(&cmd.command_id, cmd.dispatch_attempt)
            .unwrap(),
        Some(cmd.clone())
    );
    assert!(
        store
            .command_state(&rejected.command_id)
            .unwrap()
            .unwrap()
            .terminal
    );
    assert_eq!(store.accept_command(&cmd).unwrap(), Accept::Duplicate);
    drop(store);
    let probe = StoreProbe::default();
    let store = WorkerStore::open_with_probe(dir.path(), probe.clone()).unwrap();
    assert_eq!(probe.scanned_inbox_records(), 0);
    assert_eq!(store.cj_cursor().unwrap().last_seq, 2);
}

#[cfg(feature = "fault-injection")]
#[test]
fn transmission_and_ownership_crashes_preserve_fsynced_records() {
    use canterlot_proto::worker::v1::InputTransmissionStarted;
    use canterlot_worker::fault_injection::{StoreAppendPoint, StoreProbe};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    for point in [StoreAppendPoint::Cj, StoreAppendPoint::Receipt] {
        let dir = private_dir();
        let mut cmd = delivery();
        let mut store = WorkerStore::open(dir.path()).unwrap();
        store.accept_command(&cmd).unwrap();
        let probe = StoreProbe::default();
        probe.crash_after(point);
        store = store.with_probe(probe);
        assert!(
            catch_unwind(AssertUnwindSafe(|| store.append_cj_fact(
                record_body::Body::TransmissionStarted(InputTransmissionStarted {
                    command_id: cmd.command_id.clone(),
                    dispatch_attempt: cmd.dispatch_attempt,
                    machine_time_ms: 1,
                })
            )))
            .is_err()
        );
        drop(store);
        let mut store = WorkerStore::open(dir.path()).unwrap();
        store.on_cj_ack(2).unwrap();
        drop(store);
        let mut store = WorkerStore::open(dir.path()).unwrap();
        cmd.dispatch_attempt += 1;
        assert!(matches!(
            store.accept_command(&cmd).unwrap(),
            Accept::Rejected {
                reason: "already_transmitted",
                ..
            }
        ));
    }
    let dir = private_dir();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    let id = Uuid::now_v7();
    let record = ownership_record(id);
    let probe = StoreProbe::default();
    probe.crash_after(StoreAppendPoint::Ownership);
    store = store.with_probe(probe);
    assert!(
        catch_unwind(AssertUnwindSafe(|| store
            .ownership_mut()
            .unwrap()
            .append(record.clone())))
        .is_err()
    );
    drop(store);
    let store = WorkerStore::open(dir.path()).unwrap();
    assert_eq!(store.ownership().get(id), Some(&record));
    assert_eq!(store.cj_cursor().unwrap().last_seq, 0);
}

#[test]
fn redelivery_after_terminal_compaction_uses_a_new_inbox_sequence() {
    let dir = private_dir();
    let mut cmd = delivery();
    let mut store = WorkerStore::open(dir.path()).unwrap();
    store.accept_command(&cmd).unwrap();
    store
        .mark_terminal(&cmd.command_id, cmd.dispatch_attempt)
        .unwrap();
    drop(store);
    let mut store = WorkerStore::open(dir.path()).unwrap();
    cmd.dispatch_attempt += 1;
    assert_eq!(
        store.accept_command(&cmd).unwrap(),
        Accept::Redelivered { cj_seq: 2 }
    );
    let state = store.command_state(&cmd.command_id).unwrap().unwrap();
    assert_eq!(state.inbox_seq, Some(2));
    assert!(!state.terminal);
    store
        .mark_terminal(&cmd.command_id, cmd.dispatch_attempt - 1)
        .unwrap();
    assert_eq!(store.command_state(&cmd.command_id).unwrap(), Some(state));
    assert_eq!(
        store
            .inbox_command(&cmd.command_id, cmd.dispatch_attempt)
            .unwrap(),
        Some(cmd)
    );
}

#[test]
fn pilot_worker_sections_parse_strictly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.toml");
    fs::write(&path, config_text()).unwrap();
    let config = load(&path).unwrap();
    assert_eq!(config.machine.metrics_interval, Duration::from_secs(30));
    assert_eq!(config.attachments.fetch_parallelism, 2);
    assert_eq!(config.attachments.fetch_retry, Duration::from_secs(2));
    assert_eq!(config.attachments.fetch_attempts, 5);
    assert_eq!(
        config.attachments.fetch_backoff_cap,
        Duration::from_secs(30)
    );
    let extra = "[machine]\nmetrics_interval = '5s'\n[attachments]\nfetch_parallelism = 1\nfetch_retry = '250ms'\nfetch_attempts = 3\nfetch_backoff_cap = '10s'\n";
    fs::write(&path, format!("{}\n{extra}", config_text())).unwrap();
    let config = load(&path).unwrap();
    assert_eq!(config.machine.metrics_interval, Duration::from_secs(5));
    assert_eq!(config.attachments.fetch_parallelism, 1);
    assert_eq!(config.attachments.fetch_retry, Duration::from_millis(250));
    assert_eq!(config.attachments.fetch_attempts, 3);
    assert_eq!(
        config.attachments.fetch_backoff_cap,
        Duration::from_secs(10)
    );
    for extra in [
        "[machine]\nunknown = true",
        "[machine]\nmetrics_interval = 30",
        "[attachments]\nunknown = true",
        "[attachments]\nfetch_retry = 2",
        "[[instruction_profiles]]\nname = 'executor'\ntitle = 'Исполнитель'\ndescription = ''\nharnesses = ['codex']",
        "[[instruction_profiles]]\nname = 'executor'\nunknown = true",
    ] {
        fs::write(&path, format!("{}\n{extra}\n", config_text())).unwrap();
        assert!(load(&path).is_err());
    }
}
