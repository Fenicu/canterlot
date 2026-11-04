#![forbid(unsafe_code)]

//! Конфигурация `[antigravity]`, регистрация профиля и место в каталоге машин.

use canterlot_worker::{
    adapter::{
        self,
        antigravity::{AntigravityCatalog, version::CliVersion},
    },
    config::{AntigravityConfig, WorkerConfig},
};
use uuid::Uuid;

fn config(extra: &str) -> Result<WorkerConfig, toml::de::Error> {
    toml::from_str(&format!(
        "hub_url='https://localhost'\nca_cert='ca'\ntoken_file='token'\nmachine_id='{}'\ninstallation_id='{}'\nrestore_generation='{}'\ndata_dir='/tmp/canterlot-test'\n{extra}",
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    ))
}

#[test]
fn table_defaults_enable_the_harness_and_use_the_owner_binary() {
    let parsed = config("[antigravity]\n").unwrap();
    let antigravity = parsed.antigravity.unwrap();
    assert!(antigravity.enabled);
    assert!(antigravity.binary.is_none());
    assert!(antigravity.data_dir.is_none());
    assert_eq!(
        AntigravityConfig::default().binary,
        None,
        "default source is $HOME/.local/bin/agy"
    );
}

#[test]
fn enabled_false_and_an_explicit_binary_are_kept() {
    let parsed = config("[antigravity]\nenabled = false\nbinary = '/opt/agy'\n").unwrap();
    let antigravity = parsed.antigravity.unwrap();
    assert!(!antigravity.enabled);
    assert_eq!(
        antigravity.binary.as_deref(),
        Some(std::path::Path::new("/opt/agy"))
    );
}

#[test]
fn unknown_keys_are_rejected() {
    assert!(config("[antigravity]\ngeminidir = '/tmp/x'\n").is_err());
}

#[test]
fn profile_is_known_and_only_appears_with_a_catalog() {
    assert!(adapter::known_harness("antigravity"));
    assert!(!adapter::known_harness("antigravity-unknown"));
    assert!(
        !adapter::catalog_with(None, None, None, None, None)
            .iter()
            .any(|entry| entry.harness == "antigravity")
    );
    let catalog = AntigravityCatalog {
        version: CliVersion::Verified("1.4.12".into()),
        models: vec![],
        current_model: None,
        efforts: vec!["low".into()],
        current_effort: None,
    };
    let entries = adapter::catalog_with(None, None, None, None, Some(&catalog));
    let entry = entries
        .iter()
        .find(|entry| entry.harness == "antigravity")
        .expect("antigravity catalog entry");
    assert_eq!(entry.provider, "antigravity");
    assert_eq!(entry.permission_modes, vec!["owner", "ask", "bypass"]);
    assert!(!entry.images);
}
