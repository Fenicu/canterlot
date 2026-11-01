#![forbid(unsafe_code)]

//! Харнесс OpenCode спрятан за таблицей `[opencode]`: без неё пилот не меняется.

use canterlot_worker::{
    adapter::{self, AdapterContext, opencode::catalog::ModelEntry},
    config::{OpenCodeConfig, WorkerConfig},
    supervisor::SupervisorSpec,
};
use std::{path::PathBuf, time::Duration};
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

fn context(opencode: Option<OpenCodeConfig>) -> AdapterContext {
    AdapterContext {
        transmit_deadline: Duration::from_secs(1),
        ready_deadline: Duration::from_secs(1),
        exec_dir: PathBuf::from("/tmp/exec"),
        claude_code: None,
        codex: None,
        opencode,
    }
}

#[test]
fn worker_without_table_has_no_opencode() {
    let config: WorkerConfig = toml::from_str(&worker_toml("")).unwrap();
    assert!(config.opencode.is_none());
    assert!(adapter::registry_with("opencode", &context(None)).is_none());
    let entries = adapter::catalog_with(None, None, None, None);
    assert!(entries.iter().all(|entry| entry.provider != "opencode"));
}

#[test]
fn empty_table_takes_defaults() {
    let config: WorkerConfig = toml::from_str(&worker_toml("[opencode]\n")).unwrap();
    let cfg = config.opencode.unwrap();
    assert_eq!(cfg.executable, PathBuf::from("opencode"));
    assert_eq!(cfg.data_root, None);
    assert!(cfg.refuse_paths.is_empty());
    assert!(adapter::registry_with("opencode", &context(Some(cfg))).is_some());
}

#[test]
fn table_reads_every_key() {
    let extra = r#"
[opencode]
executable = "/opt/opencode"
data_root = "/srv/canterlot/opencode"
refuse_paths = ["/srv/shared"]
providers = ["anthropic", "openrouter"]
owner_config = "/home/user/.config/opencode"
"#;
    let config: WorkerConfig = toml::from_str(&worker_toml(extra)).unwrap();
    let cfg = config.opencode.unwrap();
    assert_eq!(cfg.executable, PathBuf::from("/opt/opencode"));
    assert_eq!(
        cfg.data_root,
        Some(PathBuf::from("/srv/canterlot/opencode"))
    );
    assert_eq!(cfg.refuse_paths, vec![PathBuf::from("/srv/shared")]);
    assert_eq!(cfg.providers, ["anthropic", "openrouter"]);
    assert_eq!(
        cfg.owner_config,
        Some(PathBuf::from("/home/user/.config/opencode"))
    );
}

#[test]
fn unknown_key_is_refused() {
    let error = toml::from_str::<WorkerConfig>(&worker_toml("[opencode]\npassword = \"x\"\n"));
    assert!(error.is_err());
}

// Спека супервизора читается с deny_unknown_fields: таблица обязана дойти до него целой.
#[test]
fn table_survives_the_supervisor_spec() {
    let cfg = OpenCodeConfig {
        executable: PathBuf::from("/opt/opencode"),
        data_root: None,
        refuse_paths: vec![PathBuf::from("/srv/shared")],
        providers: vec!["anthropic".into()],
        owner_config: Some(PathBuf::from("/home/user/.config/opencode")),
        default_model: None,
    };
    let mut spec: toml::Value = toml::from_str(&format!(
        "execution_id = \"{id}\"\ngeneration = 1\nconversation_id = \"{id}\"\nprotocol_version = 1\n",
        id = Uuid::now_v7()
    ))
    .unwrap();
    spec.as_table_mut()
        .unwrap()
        .insert("opencode".into(), toml::Value::try_from(cfg).unwrap());
    let parsed: SupervisorSpec = toml::from_str(&toml::to_string(&spec).unwrap()).unwrap();
    let cfg = parsed.opencode.unwrap();
    assert_eq!(cfg.executable, PathBuf::from("/opt/opencode"));
    assert_eq!(cfg.refuse_paths, vec![PathBuf::from("/srv/shared")]);
    assert_eq!(cfg.providers, ["anthropic"]);
}

#[test]
fn harness_is_known_to_the_registry() {
    assert!(adapter::known_harness("opencode"));
    assert_eq!(
        canterlot_proto::harness::provider_of("opencode"),
        Some("opencode")
    );
}

#[test]
fn catalog_shows_the_harness_only_with_a_probe_result() {
    let catalog = adapter::opencode::OpenCodeCatalog {
        version: adapter::cli_version::CliVersion::Verified("2.1.4".into()),
        models: vec![ModelEntry {
            id: "opencode-go/muse-spark".into(),
            title: "Muse Spark".into(),
            efforts: vec!["low".into(), "high".into()],
            images: Some(true),
            context_window: Some(1_048_576),
        }],
    };
    let entries = adapter::catalog_with(None, None, None, Some(&catalog));
    let entry = entries
        .iter()
        .find(|entry| entry.harness == "opencode")
        .expect("catalog entry");
    assert_eq!(entry.provider, "opencode");
    assert_eq!(entry.title, "OpenCode");
    assert!(entry.settings_midsession);
    assert!(entry.settings_midsession_effort);
    assert_eq!(
        entry.permission_modes,
        ["owner", "ask", "bypass", "read_only"]
    );
    assert_eq!(entry.models[0].id, "opencode-go/muse-spark");
    assert_eq!(entry.models[0].title, "Muse Spark");
    assert_eq!(entry.models[0].efforts, ["low", "high"]);
}
