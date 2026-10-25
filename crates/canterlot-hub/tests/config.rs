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
