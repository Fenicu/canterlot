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
fn configuration_doc_documents_running_both_services() {
    let readme = include_str!("../../../docs/configuration.md");
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

#[test]
fn configuration_doc_hub_toml_examples_load_without_worker_tables() {
    let readme = include_str!("../../../docs/configuration.md");
    let hub_part = readme
        .split("На машине воркера сохраните `worker.toml`")
        .next()
        .unwrap();
    let mut config = String::new();
    for block in hub_part.split("```toml\n").skip(1) {
        config.push_str(block.split("\n```").next().unwrap());
        config.push('\n');
    }
    assert!(config.contains("[http]") && config.contains("[timers]"));
    assert!(
        !config.contains("[execution"),
        "worker.toml table in the hub example"
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    fs::write(&path, config).unwrap();
    HubConfig::load(&path).unwrap();
}

const PILOT_BASE: &str = "database_url_file = 'db'\ngrpc_listen = '127.0.0.1:7443'\ntls_cert = 'cert'\ntls_key = 'key'\nmanifest = 'manifest'\n";
const HTTP_BASE: &str = "[http]\ntrusted_proxies = ['127.0.0.0/8', '::1/128']\nowner_login = 'owner'\nallowed_origins = ['https://chat.example:8443']\n";

#[test]
fn pilot_sections_parse_with_defaults() {
    use std::time::Duration as D;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    fs::write(&path, PILOT_BASE).unwrap();
    assert!(HubConfig::load(&path).unwrap().http.is_none());
    fs::write(&path, format!("{PILOT_BASE}{HTTP_BASE}[http.ws]\n[limits]\n[activity]\n[scheduler]\n[attachments]\n[machines]\n")).unwrap();
    let config = HubConfig::load(&path).unwrap();
    let http = config.http.unwrap();
    assert_eq!(http.listen.to_string(), "127.0.0.1:8080");
    assert_eq!(http.user_header, "Remote-User");
    assert_eq!(http.max_json_body_bytes, 524288);
    assert_eq!(http.header_read_timeout, D::from_secs(10));
    assert_eq!(http.body_read_timeout, D::from_secs(30));
    assert_eq!(http.max_connections, 256);
    assert!(!http.multi_process);
    assert_eq!(http.ws.max_connection_age, D::from_secs(1800));
    assert_eq!(
        (
            http.ws.max_subscriptions,
            http.ws.queue_messages,
            http.ws.queue_bytes
        ),
        (16, 256, 1048576)
    );
    assert_eq!(http.ws.delta_window, D::from_millis(25));
    assert_eq!(http.ws.poll_interval, D::from_secs(2));
    assert_eq!(http.ws.stall_timeout, D::from_secs(15));
    let limits = config.limits;
    assert_eq!(limits.weekly_reserve_pct, 95);
    assert_eq!(limits.snapshot_min_interval, D::from_secs(600));
    assert_eq!(limits.history_days, 90);
    assert_eq!(
        limits.default_resume_text,
        "Лимиты восстановлены, продолжай с того места, где остановился."
    );
    assert_eq!(limits.reset_grace, D::from_secs(20));
    assert_eq!(limits.verify_backoff, [30, 60, 120, 300].map(D::from_secs));
    assert_eq!(limits.poll_min_interval, D::from_secs(300));
    assert_eq!(limits.unknown_reset_retry, D::from_secs(1800));
    assert_eq!(limits.poll_floor["claude"], D::from_secs(1800));
    assert_eq!(limits.poll_floor["codex"], D::from_secs(300));
    assert_eq!(limits.reserve_exit_hysteresis_permille, 20);
    assert_eq!(limits.warn_pcts, [80, 90, 95]);
    assert_eq!(limits.stale_after, D::from_secs(2100));
    assert_eq!(limits.unverified_wake_after, D::from_secs(1200));
    assert_eq!(config.activity.stalled_generating, D::from_secs(1800));
    assert_eq!(config.activity.stalled_tool, D::from_secs(1800));
    assert_eq!(config.activity.tick, D::from_secs(15));
    assert_eq!(config.activity.debounce, D::from_millis(250));
    assert_eq!(config.scheduler.max_pending_per_conversation, 50);
    assert_eq!(config.scheduler.retry_queue_full, D::from_secs(60));
    assert_eq!(config.scheduler.max_attempts, 20);
    assert_eq!(config.scheduler.idle_poll, D::from_secs(1));
    assert_eq!(config.attachments.max_concurrent_uploads, 4);
    assert_eq!(config.attachments.gc_grace, D::from_secs(86400));
    assert_eq!(config.attachments.gc_interval, D::from_secs(3600));
    assert_eq!(config.machines.disk_warn_free_gb, 30);
    assert_eq!(config.machines.disk_critical_free_gb, 10);
    assert_eq!(config.machines.disk_warn_pct, 85);
    assert_eq!(config.machines.metrics_stale_after, D::from_secs(90));
    fs::write(&path, format!("{PILOT_BASE}[limits]\nverify_backoff = ['1s', '2m']\n[limits.poll_floor]\nclaude = '45m'\ncodex = '7m'\n")).unwrap();
    let limits = HubConfig::load(&path).unwrap().limits;
    assert_eq!(limits.verify_backoff, [D::from_secs(1), D::from_secs(120)]);
    assert_eq!(limits.poll_floor["claude"], D::from_secs(2700));
    assert_eq!(limits.poll_floor["codex"], D::from_secs(420));
}

#[test]
fn deprecated_warn_pcts_still_parse_from_old_hub_toml() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    fs::write(
        &path,
        format!("{PILOT_BASE}[limits]\nwarn_pcts = [50, 75]\nweekly_reserve_pct = 80\n"),
    )
    .unwrap();
    let limits = HubConfig::load(&path).unwrap().limits;
    assert_eq!(limits.warn_pcts, [50, 75]);
    assert_eq!(limits.weekly_reserve_pct, 80);
    assert_eq!(
        canterlot_hub::limit_settings::default_settings(&limits).reserve_remaining_pct,
        20
    );
}

#[test]
fn unknown_pilot_fields_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    for section in ["limits", "activity", "scheduler", "attachments", "machines"] {
        fs::write(
            &path,
            format!("{PILOT_BASE}[{section}]\nmisspelled = true\n"),
        )
        .unwrap();
        assert!(HubConfig::load(&path).is_err(), "{section}");
    }
    for extra in [
        "misspelled = true",
        "[http.ws]\nmisspelled = true",
        "[http.ws]\ndelta_window = 25",
        "[limits]\nverify_backoff = [30]",
        "[limits.poll_floor]\nclaude = 30",
    ] {
        fs::write(&path, format!("{PILOT_BASE}{HTTP_BASE}{extra}\n")).unwrap();
        assert!(HubConfig::load(&path).is_err(), "{extra}");
    }
}

#[test]
fn http_section_requires_trusted_proxies_owner_and_origins() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    for http in [
        HTTP_BASE.replace("['127.0.0.0/8', '::1/128']", "[]"),
        HTTP_BASE.replace("owner_login = 'owner'", "owner_login = ''"),
        HTTP_BASE.replace("['https://chat.example:8443']", "[]"),
        HTTP_BASE.replace("['127.0.0.0/8', '::1/128']", "['127.0.0.1/33']"),
        "[http]\n".into(),
    ] {
        fs::write(&path, format!("{PILOT_BASE}{http}")).unwrap();
        assert!(HubConfig::load(&path).is_err());
    }
    for origin in [
        "chat.example",
        "https://chat.example/",
        "https://chat.example/path",
        "https://chat.example?q=x",
        "https://chat.example#x",
        "https://user@chat.example",
        "file:///tmp",
        "https://",
        "https://chat.example:bad",
    ] {
        fs::write(
            &path,
            format!(
                "{PILOT_BASE}{}",
                HTTP_BASE.replace("https://chat.example:8443", origin)
            ),
        )
        .unwrap();
        assert!(HubConfig::load(&path).is_err(), "{origin}");
    }
    for origin in [
        "https://chat.example",
        "http://localhost:8080",
        "https://[::1]:8443",
    ] {
        fs::write(
            &path,
            format!(
                "{PILOT_BASE}{}",
                HTTP_BASE.replace("https://chat.example:8443", origin)
            ),
        )
        .unwrap();
        HubConfig::load(&path)
            .unwrap()
            .http
            .unwrap()
            .validate()
            .unwrap();
    }
}

#[test]
fn http_section_validates_logout_url_and_proxy_secret() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    for url in [
        "/logout",
        "http://auth.example/logout",
        "https://",
        "https://user:password@auth.example/logout",
    ] {
        fs::write(
            &path,
            format!("{PILOT_BASE}{HTTP_BASE}logout_url = '{url}'\n"),
        )
        .unwrap();
        assert!(HubConfig::load(&path).is_err());
    }
    fs::write(&path, format!("{PILOT_BASE}{HTTP_BASE}logout_url = 'https://auth.example/logout?rd=https%3A%2F%2Fchat.example'\nproxy_secret_file = 'proxy-secret'\n")).unwrap();
    assert!(HubConfig::load(&path).is_err());
    fs::write(dir.path().join("proxy-secret"), [42; 31]).unwrap();
    assert!(HubConfig::load(&path).is_err());
    fs::write(dir.path().join("proxy-secret"), [42; 32]).unwrap();
    let http = HubConfig::load(&path).unwrap().http.unwrap();
    assert_eq!(
        http.proxy_secret_file.unwrap(),
        dir.path().join("proxy-secret")
    );
    assert!(
        http.logout_url
            .unwrap()
            .starts_with("https://auth.example/")
    );
}

#[test]
fn relative_blob_dir_resolves_against_the_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    fs::write(
        &path,
        format!("{PILOT_BASE}[attachments]\nblob_dir = 'data/blobs'\n"),
    )
    .unwrap();
    assert_eq!(
        HubConfig::load(&path).unwrap().attachments.blob_dir,
        dir.path().join("data/blobs")
    );
    fs::write(
        &path,
        format!("{PILOT_BASE}[attachments]\nblob_dir = '/tmp/canterlot-blobs'\n"),
    )
    .unwrap();
    assert_eq!(
        HubConfig::load(&path).unwrap().attachments.blob_dir,
        std::path::Path::new("/tmp/canterlot-blobs")
    );
}

#[test]
fn trusted_proxy_cidrs_parse_v4_v6_and_reject_garbage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    fs::write(&path, format!("{PILOT_BASE}{HTTP_BASE}")).unwrap();
    let proxies = HubConfig::load(&path)
        .unwrap()
        .http
        .unwrap()
        .trusted_proxies;
    let cases = [
        ("127.255.255.255", [true, false]),
        ("128.0.0.0", [false, false]),
        ("::1", [false, true]),
        ("::2", [false, false]),
        ("::ffff:127.0.0.1", [false, false]),
    ];
    for (address, expected) in cases {
        let address: std::net::IpAddr = address.parse().unwrap();
        assert_eq!(
            [proxies[0].contains(&address), proxies[1].contains(&address)],
            expected
        );
    }
    for cidr in [
        "garbage",
        "127.0.0.1",
        "127.0.0.1/33",
        "::1/129",
        "127.0.0.1/nope",
        "::1/-1",
        "300.0.0.1/8",
        "gggg::/64",
        "127.0.0.1/8/8",
    ] {
        let http = HTTP_BASE.replace("['127.0.0.0/8', '::1/128']", &format!("['{cidr}']"));
        fs::write(&path, format!("{PILOT_BASE}{http}")).unwrap();
        assert!(HubConfig::load(&path).is_err(), "accepted {cidr}");
    }
}

#[test]
fn http_connection_limits_parse_and_reject_disabled_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    fs::write(&path, format!("{PILOT_BASE}{HTTP_BASE}header_read_timeout = '75ms'\nbody_read_timeout = '150ms'\nmax_connections = 2\n")).unwrap();
    let http = HubConfig::load(&path).unwrap().http.unwrap();
    assert_eq!(
        http.header_read_timeout,
        std::time::Duration::from_millis(75)
    );
    assert_eq!(
        http.body_read_timeout,
        std::time::Duration::from_millis(150)
    );
    assert_eq!(http.max_connections, 2);
    for extra in [
        "header_read_timeout = '0s'",
        "body_read_timeout = '0s'",
        "max_connections = 0",
        "header_read_timeout = 10",
        "body_read_timeout = 30",
    ] {
        fs::write(&path, format!("{PILOT_BASE}{HTTP_BASE}{extra}\n")).unwrap();
        assert!(HubConfig::load(&path).is_err(), "{extra}");
    }
}

#[test]
fn session_tools_config_defaults_match_the_plan() {
    use std::time::Duration as D;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.toml");
    fs::write(&path, PILOT_BASE).unwrap();
    let config = HubConfig::load(&path).unwrap();
    let tools = config.session_tools;
    assert_eq!(tools.max_spawn_depth, 3);
    assert_eq!(tools.max_chain_depth, 8);
    assert_eq!(
        (tools.pair_sends, tools.pair_window),
        (10, D::from_secs(600))
    );
    assert_eq!(
        (tools.sender_sends, tools.sender_window),
        (30, D::from_secs(600))
    );
    assert_eq!(
        (tools.parent_spawns, tools.spawn_window),
        (10, D::from_secs(3600))
    );
    assert_eq!(config.activity.idle_quiet, D::from_secs(5));

    fs::write(
        &path,
        format!(
            "{PILOT_BASE}[session_tools]\nmax_spawn_depth = 1\npair_window = '2m'\n[activity]\nidle_quiet = '9s'\n"
        ),
    )
    .unwrap();
    let config = HubConfig::load(&path).unwrap();
    assert_eq!(config.session_tools.max_spawn_depth, 1);
    assert_eq!(config.session_tools.pair_window, D::from_secs(120));
    assert_eq!(config.session_tools.max_chain_depth, 8);
    assert_eq!(config.activity.idle_quiet, D::from_secs(9));

    fs::write(&path, format!("{PILOT_BASE}[session_tools]\nbogus = 1\n")).unwrap();
    assert!(HubConfig::load(&path).is_err());
}
