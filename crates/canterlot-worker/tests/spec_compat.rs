//! Спека на диске переживает релиз, который её записал: сопровождение живёт дольше воркера.

use canterlot_worker::{
    config::{self, ConfigErrorCategory},
    supervisor::{PersistedSpec, SupervisorSpec},
};
use std::path::Path;
use uuid::Uuid;

const B644882: &str = include_str!("fixtures/spec/b644882ac458.toml");

#[test]
fn frozen_spec_of_b644882_loads() {
    let spec: PersistedSpec = toml::from_str(B644882).unwrap();
    assert_eq!(
        spec.execution_id,
        Uuid::parse_str("01a193a4-8249-7024-ba2e-000000000001").unwrap()
    );
    assert_eq!(spec.generation, 1);
    assert_eq!(
        spec.ipc_socket.as_deref(),
        Some(Path::new(
            "/run/user/1000/canterlot/01a193a4-8249-7024-ba2e-000000000001.sock"
        ))
    );
}

#[test]
fn keys_unknown_to_this_release_are_ignored_at_any_depth() {
    let text = format!(
        "future_key = 1\n{B644882}\n[future_table]\nkey = true\n\n[claude_code.future]\nkey = 1\n"
    );
    toml::from_str::<PersistedSpec>(&text).unwrap();
}

#[test]
fn strict_spec_refuses_the_retired_key_that_caused_owner_lost() {
    let error = toml::from_str::<SupervisorSpec>(B644882).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unknown field `credentials_path`")
            || error
                .to_string()
                .contains("unknown field `oauth_poll_interval_s`"),
        "{error}"
    );
}

#[test]
fn worker_toml_stays_strict_about_retired_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("worker.toml");
    for key in ["oauth_poll_interval_s = 600", "credentials_path = \"/x\""] {
        std::fs::write(
            &path,
            format!(
                "hub_url = \"https://hub.example\"\nca_cert = \"/ca\"\ntoken_file = \"/token\"\n\
                 machine_id = \"{id}\"\ninstallation_id = \"{id}\"\n\
                 restore_generation = \"{id}\"\ndata_dir = \"/data\"\n\n[claude_code]\n{key}\n",
                id = Uuid::now_v7()
            ),
        )
        .unwrap();
        let error = config::load(&path).unwrap_err();
        assert_eq!(error.category, ConfigErrorCategory::Parse, "{key}");
    }
}
