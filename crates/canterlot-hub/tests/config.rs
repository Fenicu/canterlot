use std::{error::Error, fs};

use canterlot_hub::config::HubConfig;

#[test]
fn config_errors_retain_location_without_source_contents() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    let marker = "PRIVATE_CONFIG_MARKER_491";
    for contents in [
        format!("{marker} is not TOML"),
        format!("password = '{marker}'"),
        format!("grpc_listen = '{marker}'"),
    ] {
        fs::write(&path, contents).unwrap();
        let error = HubConfig::load(&path).unwrap_err();
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(!rendered.contains(marker));
            assert!(!rendered.contains("password ="));
            assert!(rendered.contains(path.to_str().unwrap()));
            assert!(rendered.contains("config_invalid"));
            assert!(rendered.contains("line"));
            assert!(rendered.contains("column"));
        }
        let mut source = error.source();
        while let Some(error) = source {
            assert!(!error.to_string().contains(marker));
            assert!(!format!("{error:?}").contains(marker));
            source = error.source();
        }
    }
    fs::write(&path, [0xff, 0xfe]).unwrap();
    let error = HubConfig::load(&path).unwrap_err();
    assert!(error.to_string().contains(path.to_str().unwrap()));
    assert!(error.to_string().contains("config_io_error"));
    assert!(error.source().is_none());
    fs::remove_file(&path).unwrap();
    let error = HubConfig::load(&path).unwrap_err();
    assert!(format!("{error:?}").contains(path.to_str().unwrap()));
    assert!(error.source().is_none());
}

#[test]
fn hub_durations_are_strings_and_unknown_fields_are_rejected() {
    let base = "database_url_file = 'db'\ngrpc_listen = '127.0.0.1:7443'\ntls_cert = 'cert'\ntls_key = 'key'\nmanifest = 'manifest'\n";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    fs::write(&path, format!("{base}[timers]\nhandshake = '250ms'\n")).unwrap();
    assert_eq!(
        HubConfig::load(&path).unwrap().timers.handshake,
        std::time::Duration::from_millis(250)
    );
    for extra in [
        "handshake = '10s'",
        "[timer]\nhandshake = '10s'",
        "[timers]\nhandshak = '10s'",
        "[timers]\nhandshake = 10",
    ] {
        fs::write(&path, format!("{base}{extra}\n")).unwrap();
        assert!(HubConfig::load(&path).is_err());
    }
}

#[test]
fn readme_documents_running_both_services() {
    let readme = include_str!("../../../README.md");
    for required in [
        "canterlot-worker run --config",
        "hub_url =",
        "ca_cert =",
        "token_file =",
        "backoff_min = \"250ms\"",
        "rotate-token",
        "--reset-local-state",
    ] {
        assert!(readme.contains(required), "missing {required}");
    }
    assert!(!readme.contains("сетевой сервер ещё не запускается"));
}
