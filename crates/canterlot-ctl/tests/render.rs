use std::{net::IpAddr, path::Path};

use canterlot_ctl::{
    cli::{TlsMode, Version},
    render::{
        csp_for_origin, render_caddyfile, render_compose, render_compose_fenced,
        render_front_block, render_hub_toml, render_worker_toml, render_worker_unit,
    },
    state::{Components, InstallState},
    worker::{DetectedAgent, Harness, MachineIds},
};
use uuid::Uuid;

const CSP_TXT: &str = "default-src 'self'; connect-src 'self' wss://chat.example.com\n";

fn state(mode: TlsMode) -> InstallState {
    let (origin, endpoint, front, login) = match mode {
        TlsMode::Acme => (
            "https://canterlot.example.org",
            "canterlot.example.org:7443",
            None,
            "owner",
        ),
        TlsMode::External => (
            "https://chat.example.com",
            "hub.example.internal:7444",
            Some("192.0.2.20".parse::<IpAddr>().unwrap()),
            "admin",
        ),
        TlsMode::SelfSigned => ("https://192.0.2.10", "192.0.2.10:7443", None, "owner"),
    };
    InstallState {
        format: 1,
        components: Components::default(),
        tls: Some(mode),
        origin: Some(origin.into()),
        grpc_endpoint: Some(endpoint.into()),
        front_ip: front,
        owner_login: Some(login.into()),
        hub_names: Vec::new(),
        worker_user: None,
        worker_home: None,
        hub_uid: 980,
        hub_gid: 980,
        created_paths: Vec::new(),
        adopted_paths: Vec::new(),
    }
}

fn golden(mode: &str, file: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(mode)
        .join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn check_mode(mode: TlsMode, dir: &str) {
    let s = state(mode);
    let v = Version::parse("1.2.3").unwrap();
    let csp = csp_for_origin(CSP_TXT, s.origin.as_deref().unwrap()).unwrap();
    assert_eq!(
        render_compose(&s, &v),
        golden(dir, "compose.yaml"),
        "{dir} compose"
    );
    assert_eq!(
        render_caddyfile(&s, &csp),
        golden(dir, "Caddyfile"),
        "{dir} Caddyfile"
    );
    assert_eq!(
        render_hub_toml(&s),
        golden(dir, "hub.toml"),
        "{dir} hub.toml"
    );
    // Хаб читает hub.toml как TOML: файл обязан разбираться.
    let _: toml::Table = toml::from_str(&render_hub_toml(&s)).unwrap();
}

#[test]
fn render_acme_matches_golden() {
    check_mode(TlsMode::Acme, "acme");
}

#[test]
fn render_external_matches_golden() {
    check_mode(TlsMode::External, "external");
}

#[test]
fn render_self_signed_matches_golden() {
    check_mode(TlsMode::SelfSigned, "self-signed");
}

fn position(text: &str, needle: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("missing {needle:?} in:\n{text}"))
}

#[test]
fn caddyfile_scrubs_headers_before_proxy() {
    for mode in [TlsMode::Acme, TlsMode::External, TlsMode::SelfSigned] {
        let text = render_caddyfile(&state(mode), "default-src 'self'");
        let proxy = position(&text, "reverse_proxy");
        let mut last = 0;
        for scrub in [
            "request_header -Remote-*",
            "request_header -X-Forwarded-*",
            "request_header -X-Canterlot-Proxy",
            "request_header -X-Canterlot-Client-Ip",
        ] {
            let at = position(&text, scrub);
            assert!(at > last && at < proxy, "{mode:?}: {scrub} out of order");
            last = at;
        }
        assert!(text.contains("header_up X-Canterlot-Proxy {file./run/secrets/canterlot-proxy}"));
    }
}

#[test]
fn caddyfile_external_trusts_only_front_ip() {
    let text = render_caddyfile(&state(TlsMode::External), "default-src 'self'");
    assert_eq!(text.matches("trusted_proxies").count(), 2);
    assert!(text.contains("trusted_proxies static 192.0.2.20/32\n"));
    assert!(!text.contains("private_ranges"));
    assert!(text.contains("header_up X-Canterlot-Client-Ip {client_ip}"));
    // Края берут адрес TCP-пира: там {client_ip} позволил бы подделать адрес.
    for mode in [TlsMode::Acme, TlsMode::SelfSigned] {
        let edge = render_caddyfile(&state(mode), "default-src 'self'");
        assert!(!edge.contains("trusted_proxies"), "{mode:?}");
        assert!(!edge.contains("{client_ip}"), "{mode:?}");
        assert!(edge.contains("header_up X-Canterlot-Client-Ip {remote_host}"));
    }
}

#[test]
fn external_refuses_peers_other_than_front() {
    let text = render_caddyfile(&state(TlsMode::External), "default-src 'self'");
    let site = position(&text, ":80 {");
    let matcher = position(&text, "@notfront not remote_ip 192.0.2.20/32");
    let route = position(&text, "route {");
    let abort = position(&text, "abort @notfront");
    let scrub = position(&text, "request_header -Remote-*");
    let maint = position(&text, "@maint {");
    let proxy = position(&text, "reverse_proxy");
    // Внутри route directive идут в порядке записи; снаружи Caddy поставил бы abort после route.
    assert!(site < matcher && matcher < route && route < abort);
    assert!(abort < scrub && abort < maint && abort < proxy);
    assert!(text.contains("trusted_proxies static 192.0.2.20/32\n"));
    for mode in [TlsMode::Acme, TlsMode::SelfSigned] {
        let edge = render_caddyfile(&state(mode), "default-src 'self'");
        assert!(!edge.contains("@notfront"), "{mode:?}");
        assert!(!edge.contains("abort"), "{mode:?}");
    }
}

#[test]
fn maintenance_matcher_precedes_api_proxy() {
    for mode in [TlsMode::Acme, TlsMode::External, TlsMode::SelfSigned] {
        let text = render_caddyfile(&state(mode), "default-src 'self'");
        let maint = position(&text, "handle @maint");
        let api = position(&text, "handle @api");
        assert!(maint < api, "{mode:?}");
        assert!(text.contains("try_files /maintenance"));
        assert!(text.contains("respond `{\"error\":\"maintenance\"}` 503"));
    }
}

#[test]
fn csp_substitutes_origin_host() {
    assert_eq!(
        csp_for_origin(CSP_TXT, "https://pwa.example.org").unwrap(),
        "default-src 'self'; connect-src 'self' wss://pwa.example.org"
    );
    assert!(csp_for_origin("default-src 'self'", "https://pwa.example.org").is_err());
    assert!(csp_for_origin("x \"y\" wss://chat.example.com", "https://pwa.example.org").is_err());
}

#[test]
fn fenced_override_resets_hub_ports() {
    let text = render_compose_fenced();
    assert!(text.contains("  hub:\n    ports: !reset []\n"));
}

#[test]
fn worker_toml_parses_and_restricts_roots_to_home() {
    let ids = MachineIds {
        machine_id: Uuid::nil(),
        installation_id: Uuid::nil(),
        restore_generation: Uuid::nil(),
    };
    let agents = [DetectedAgent {
        harness: Harness::ClaudeCode,
        executable: "/srv/home/.local/bin/claude".into(),
    }];
    let text = render_worker_toml(
        "https://localhost:7443",
        &ids,
        Path::new("/srv/home"),
        &agents,
    );
    let table: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(table["hub_url"].as_str(), Some("https://localhost:7443"));
    assert_eq!(
        table["token_file"].as_str(),
        Some("/srv/home/.config/canterlot/token")
    );
    assert_eq!(
        table["execution"]["allowed_workdir_roots"]
            .as_array()
            .unwrap()[0]
            .as_str(),
        Some("/srv/home")
    );
    assert_eq!(
        table["claude_code"]["executable"].as_str(),
        Some("/srv/home/.local/bin/claude")
    );
    assert!(!table.contains_key("codex"));
}

#[test]
fn worker_unit_runs_current_release() {
    let unit = render_worker_unit();
    assert!(unit.contains(
        "ExecStart=%h/.local/opt/canterlot/current/bin/canterlot-worker run --config %h/.config/canterlot/worker.toml\n"
    ));
    assert!(unit.contains("KillMode=process\n"));
    assert!(unit.contains("Type=notify\n"));
}

#[test]
fn front_block_overwrites_forwarding_headers() {
    let text = render_front_block(
        &state(TlsMode::External),
        "192.0.2.10:8080".parse().unwrap(),
    );
    assert!(text.starts_with("chat.example.com {\n"));
    assert!(text.contains("reverse_proxy 192.0.2.10:8080"));
    assert!(text.contains("header_up X-Forwarded-For {remote_host}"));
}
