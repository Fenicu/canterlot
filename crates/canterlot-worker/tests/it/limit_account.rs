#![forbid(unsafe_code)]

//! Учётная запись лимитов харнесса: ключ `account` в таблицах `[claude_code]`, `[codex]`
//! и `[opencode]` файла worker.toml.

use canterlot_worker::config::{self, ConfigErrorCategory, WorkerConfig};
use canterlot_worker::exec::{SupervisorSpecParams, supervisor_spec_toml};
use std::path::Path;
use uuid::Uuid;

fn worker_toml(extra: &str) -> String {
    let id = Uuid::now_v7();
    format!(
        r#"
hub_url = "https://hub.test"
ca_cert = "/tmp/ca.pem"
token_file = "/tmp/token"
machine_id = "{id}"
installation_id = "{id}"
restore_generation = "{id}"
data_dir = "/tmp/data"
{extra}
"#
    )
}

fn load(extra: &str) -> Result<WorkerConfig, config::ConfigError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.toml");
    std::fs::write(&path, worker_toml(extra)).unwrap();
    config::load(&path)
}

#[test]
fn account_defaults_to_owner_in_every_table() {
    let config = load("[claude_code]\n[codex]\n[opencode]\n").unwrap();
    assert_eq!(config.claude_code.unwrap().account, "owner");
    assert_eq!(config.codex.unwrap().account, "owner");
    assert_eq!(config.opencode.unwrap().account, "owner");
}

#[test]
fn account_is_read_from_the_table() {
    let config =
        load("[opencode]\naccount = \"b-opencode\"\n[codex]\naccount = \"work2\"\n").unwrap();
    assert_eq!(config.opencode.unwrap().account, "b-opencode");
    assert_eq!(config.codex.unwrap().account, "work2");
}

#[test]
fn invalid_account_stops_the_worker_at_load() {
    for (table, bad) in [
        ("opencode", "Upper"),
        ("opencode", ""),
        ("codex", "has space"),
        ("claude_code", "under_score"),
        ("opencode", "ünï"),
        ("opencode", &"a".repeat(41)),
    ] {
        let error = load(&format!("[{table}]\naccount = \"{bad}\"\n"))
            .expect_err(&format!("{table} account {bad:?} must be refused"));
        match &error.category {
            ConfigErrorCategory::Account { harness } => assert_eq!(*harness, table),
            other => panic!("unexpected category {other:?}"),
        }
        assert!(
            error.to_string().contains("account"),
            "the message names the key: {error}"
        );
    }
    // Граница: ровно 40 знаков допустимы.
    load(&format!("[opencode]\naccount = \"{}\"\n", "a".repeat(40))).unwrap();
}

#[test]
fn poll_reports_are_stamped_with_the_account_of_their_provider() {
    use canterlot_proto::worker::v1::{LimitAbsence, LimitObservation};
    let config = load("[claude_code]\n[opencode]\naccount = \"b-opencode\"\n").unwrap();
    let observation = |provider: &str| LimitObservation {
        provider: provider.into(),
        ..Default::default()
    };
    let mut observations = vec![observation("opencode"), observation("claude")];
    let mut absent = vec![
        LimitAbsence {
            provider: "opencode".into(),
            ..Default::default()
        },
        LimitAbsence {
            provider: "claude".into(),
            ..Default::default()
        },
    ];
    canterlot_worker::machine::limits::stamp_report(&config, &mut observations, &mut absent);
    assert_eq!(observations[0].account, "b-opencode");
    assert_eq!(observations[1].account, "owner");
    assert_eq!(absent[0].account, "b-opencode");
    assert_eq!(absent[1].account, "owner");
}

#[test]
fn machine_report_names_the_account_per_harness() {
    use canterlot_worker::adapter::HarnessCatalog;
    let config = load("[opencode]\naccount = \"b-opencode\"\n[codex]\n").unwrap();
    let harness = |harness: &'static str, provider: &'static str| HarnessCatalog {
        harness,
        provider,
        title: "t",
        images: false,
        settings_midsession: false,
        settings_midsession_effort: false,
        permission_modes: Vec::new(),
        models: Vec::new(),
        goals: false,
    };
    let report = canterlot_worker::machine::build_report(
        &config,
        &[harness("opencode", "opencode"), harness("codex", "codex")],
    );
    let accounts: Vec<_> = report
        .harnesses
        .iter()
        .map(|h| (h.harness.as_str(), h.account.as_str()))
        .collect();
    assert_eq!(accounts, [("opencode", "b-opencode"), ("codex", "owner")]);
}

#[test]
fn session_limit_facts_of_the_supervisor_carry_the_account_of_the_spec() {
    let timers = config::SupervisorTimers::default();
    let spec_text = |account: &str| {
        let mut opencode = config::OpenCodeConfig::default();
        opencode.account = account.into();
        supervisor_spec_toml(&SupervisorSpecParams {
            data_dir: Path::new("/tmp/data"),
            execution_id: Uuid::nil(),
            generation: 1,
            conversation_id: Uuid::nil(),
            agent_nice: 10,
            timers: &timers,
            runtime_dir: None,
            claude_code: None,
            codex: None,
            opencode: Some(opencode),
            delegate_mcp_servers: &[],
        })
        .unwrap()
    };
    let spec: canterlot_worker::supervisor::SupervisorSpec =
        toml::from_str(&spec_text("b-opencode")).unwrap();
    assert_eq!(spec.limit_account("opencode"), "b-opencode");
    assert_eq!(spec.limit_account("claude"), "owner");
    let spec: canterlot_worker::supervisor::SupervisorSpec =
        toml::from_str(&spec_text("owner")).unwrap();
    assert_eq!(spec.limit_account("opencode"), "owner");
}

#[test]
fn spec_for_the_supervisor_is_unchanged_for_owner_and_carries_other_accounts() {
    let timers = config::SupervisorTimers::default();
    let spec = |account: &str| {
        let mut opencode = config::OpenCodeConfig::default();
        opencode.account = account.into();
        supervisor_spec_toml(&SupervisorSpecParams {
            data_dir: Path::new("/tmp/data"),
            execution_id: Uuid::nil(),
            generation: 1,
            conversation_id: Uuid::nil(),
            agent_nice: 10,
            timers: &timers,
            runtime_dir: None,
            claude_code: None,
            codex: None,
            opencode: Some(opencode),
            delegate_mcp_servers: &[],
        })
        .unwrap()
    };
    assert!(
        !spec("owner").contains("account"),
        "the owner account must not change the spec shape"
    );
    assert!(spec("b-opencode").contains("account = \"b-opencode\""));
}
