//! Установка на FakeRunner: проверяется последовательность команд и файлы во временном
//! корне. Ни Docker, ни systemd, ни сеть не трогаются; релиз раздаёт локальный сервер.
// Ошибки утилиты несут запись операции, как и в самой библиотеке.
#![allow(clippy::result_large_err)]

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use canterlot_ctl::{
    cli::{InstallArgs, TlsMode, UninstallArgs},
    error::CtlError,
    install::{Host, install},
    layout::Layout,
    ops::{self, DbIdentity, OpKind, OpRecord, OpResult},
    release::ReleaseSource,
    runner::{Cmd, FakeRunner, Output},
    state::InstallState,
    status::status,
    uninstall::uninstall_hub,
};
use flate2::{Compression, write::GzEncoder};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const VERSION: &str = "1.2.3";
const IDS: [&str; 3] = [
    "00000000-0000-7000-8000-000000000001",
    "00000000-0000-7000-8000-000000000002",
    "00000000-0000-7000-8000-000000000003",
];
const OWNER: &str = "00000000-0000-7000-8000-000000000004";
const MACHINE: &str = "00000000-0000-7000-8000-000000000005";

#[derive(Default)]
struct Assets {
    bodies: Mutex<BTreeMap<String, Vec<u8>>>,
}

async fn serve(State(state): State<Arc<Assets>>, uri: Uri) -> Response {
    let path = uri.path().to_string();
    if path.ends_with("/releases/latest") {
        return Json(json!({ "tag_name": format!("v{VERSION}") })).into_response();
    }
    let name = path.rsplit('/').next().unwrap_or("").to_string();
    match state.bodies.lock().unwrap().get(&name).cloned() {
        Some(bytes) => bytes.into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, name, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::new();
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn release_files(version: &str) -> Vec<(String, Vec<u8>)> {
    vec![
        (
            format!("canterlot-hub-{version}-x86_64-linux-gnu.tar.gz"),
            tar_gz(&[("canterlot-hub", b"hub")]),
        ),
        (
            format!("canterlot-worker-{version}-x86_64-linux-gnu.tar.gz"),
            tar_gz(&[("canterlot-worker", b"worker")]),
        ),
        (
            format!("canterlot-web-{version}.tar.gz"),
            tar_gz(&[("200.html", b"<html></html>")]),
        ),
        (
            format!("canterlot-csp-{version}.txt"),
            b"default-src 'self'; connect-src 'self' wss://chat.example.com\n".to_vec(),
        ),
    ]
}

/// Каталог выпуска в форме `--from-dir`: те же архивы и контрольные суммы, что раздаёт сервер.
fn release_dir(dir: &Path, version: &str) {
    let files = release_files(version);
    let mut sums = String::new();
    for (name, body) in &files {
        let _ = writeln!(sums, "{}  {name}", hex(&Sha256::digest(body)));
        std::fs::write(dir.join(name), body).unwrap();
    }
    std::fs::write(dir.join("SHA256SUMS"), sums).unwrap();
    std::fs::write(dir.join("RELEASE"), format!("version={version}\n")).unwrap();
}

async fn release_server() -> (ReleaseSource, tokio::task::JoinHandle<()>) {
    let state = Arc::new(Assets::default());
    let files = release_files(VERSION);
    let mut sums = String::new();
    for (name, body) in &files {
        let _ = writeln!(sums, "{}  {name}", hex(&Sha256::digest(body)));
    }
    {
        let mut bodies = state.bodies.lock().unwrap();
        for (name, body) in files {
            bodies.insert(name, body);
        }
        bodies.insert("SHA256SUMS".into(), sums.into_bytes());
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().fallback(get(serve)).with_state(state);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let source = ReleaseSource {
        api_base: format!("http://{addr}/").parse().unwrap(),
        download_base: format!("http://{addr}/").parse().unwrap(),
        repo: "Fenicu/canterlot".into(),
    };
    (source, task)
}

struct Fixture {
    _dir: tempfile::TempDir,
    layout: Layout,
    home: PathBuf,
    runner: FakeRunner,
    probe_ready: Arc<AtomicBool>,
    identity: Arc<Mutex<String>>,
}

fn args() -> InstallArgs {
    InstallArgs {
        worker_user: Some("worker".into()),
        tls: Some(TlsMode::SelfSigned),
        origin: Some("https://192.0.2.10".into()),
        grpc_endpoint: Some("192.0.2.10:7443".into()),
        yes: true,
        ..InstallArgs::default()
    }
}

fn host() -> Host {
    Host {
        sudo_user: None,
        hostname: "workstation".into(),
        is_root: true,
        interactive: false,
        self_exe: None,
        retries: 2,
        retry_delay: Duration::ZERO,
    }
}

fn identity_line(installation: &str) -> String {
    format!(
        "7300000000000000001|16384|{installation}|{}|{OWNER}\n",
        IDS[2]
    )
}

fn stdin(cmd: &Cmd) -> String {
    String::from_utf8_lossy(cmd.stdin.as_deref().unwrap_or_default()).into_owned()
}

fn has(cmd: &Cmd, needle: &[&str]) -> bool {
    cmd.args
        .windows(needle.len())
        .any(|w| w.iter().map(String::as_str).eq(needle.iter().copied()))
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let layout = Layout::new(root, Some(home.clone()));
    let runner = FakeRunner::new();
    let probe_ready = Arc::new(AtomicBool::new(true));
    let identity = Arc::new(Mutex::new(identity_line(IDS[1])));
    let manifest = layout.manifest_dir().join("installation.toml");
    let home_text = home.display().to_string();
    {
        let probe_ready = probe_ready.clone();
        let identity = identity.clone();
        runner.respond(move |cmd| {
            let input = stdin(cmd);
            match cmd.program.as_str() {
                "getent" => Some(Output::ok(format!(
                    "worker:x:1000:1000::{home_text}:/bin/bash\n"
                ))),
                "id" => Some(Output::ok("1000\n")),
                "df" => Some(Output::ok("Avail\n100000000000\n")),
                "ldd" => Some(Output::ok("ldd (Debian GLIBC 2.41-12) 2.41\n")),
                "sh" => {
                    if cmd.args.iter().any(|a| a.contains("command -v claude")) {
                        Some(Output::ok("/srv/tools/claude\n"))
                    } else {
                        Some(Output::failed(1, ""))
                    }
                }
                "docker" if has(cmd, &["hub-init"]) => {
                    // Настоящий init пишет манифест; подделка делает то же.
                    std::fs::write(
                        &manifest,
                        format!(
                            "installation_id = \"{}\"\nrestore_generation = \"{}\"\nrestore_in_progress = false\n",
                            IDS[1], IDS[2]
                        ),
                    )
                    .unwrap();
                    Some(Output::ok(""))
                }
                "docker" if has(cmd, &["probe"]) => {
                    if probe_ready.load(Ordering::SeqCst) {
                        Some(Output::ok(format!(
                            "{{\"ready\":true,\"version\":\"{VERSION}\",\"status\":{{}}}}\n"
                        )))
                    } else {
                        Some(Output {
                            status: 1,
                            stdout: format!(
                                "{{\"ready\":false,\"version\":\"{VERSION}\",\"status\":{{}}}}\n"
                            )
                            .into_bytes(),
                            stderr: Vec::new(),
                        })
                    }
                }
                "docker" if has(cmd, &["machine", "add"]) => Some(Output::ok(format!(
                    "machine_id={MACHINE}\ninstallation_id={}\nrestore_generation={}\ntoken=test-token-value\n",
                    IDS[1], IDS[2]
                ))),
                "docker" if has(cmd, &["recovery-link"]) => {
                    Some(Output::ok("https://192.0.2.10/recover#test\n"))
                }
                "docker" if input.contains("pg_control_system") => {
                    Some(Output::ok(identity.lock().unwrap().clone()))
                }
                "docker" if input.contains("pg_tables") => Some(Output::ok("0\n")),
                "docker" if input.contains("availability") => Some(Output::ok("available\n")),
                "docker" if input.contains("from executions") => Some(Output::ok("0\n")),
                _ => None,
            }
        });
    }
    Fixture {
        _dir: dir,
        layout,
        home,
        runner,
        probe_ready,
        identity,
    }
}

fn position(calls: &[Cmd], what: &str, pred: impl Fn(&Cmd) -> bool) -> usize {
    calls
        .iter()
        .position(pred)
        .unwrap_or_else(|| panic!("missing {what} in {calls:#?}"))
}

#[tokio::test]
async fn install_fresh_runs_expected_commands() {
    let (src, server) = release_server().await;
    let fx = fixture();
    let report = install(args(), &fx.layout, &fx.runner, &src, &host())
        .await
        .unwrap();
    let calls = fx.runner.calls();
    let up_pg = position(&calls, "up postgres", |c| has(c, &["up", "-d", "postgres"]));
    let role = position(&calls, "role", |c| {
        stdin(c).contains("CREATE ROLE canterlot")
    });
    let init = position(&calls, "init", |c| has(c, &["run", "--rm", "hub-init"]));
    let up = position(&calls, "up", |c| {
        c.program == "docker" && c.args.ends_with(&["up".into(), "-d".into()])
    });
    let probe = position(&calls, "probe", |c| has(c, &["probe"]));
    let add = position(&calls, "machine add", |c| {
        has(c, &["machine", "add"]) && has(c, &["--name", "workstation"])
    });
    let enable = position(&calls, "enable", |c| {
        c.program == "systemctl"
            && has(c, &["--user", "enable", "--now", "canterlot-worker"])
            && c.as_user.as_deref() == Some("worker")
    });
    let connected = position(&calls, "connected", |c| stdin(c).contains("availability"));
    assert!(up_pg < role && role < init && init < up && up < probe);
    assert!(probe < add && add < enable && enable < connected);
    // Секреты не уходят в аргументы команд.
    let secret = std::fs::read_to_string(fx.layout.hub_etc().join("proxy.secret")).unwrap();
    let db_url = std::fs::read_to_string(fx.layout.hub_etc().join("db.url")).unwrap();
    let password = db_url
        .strip_prefix("postgres://canterlot:")
        .and_then(|rest| rest.split('@').next())
        .unwrap()
        .to_owned();
    assert_eq!(password.len(), 64);
    for call in &calls {
        for arg in &call.args {
            assert!(!arg.contains(&secret), "proxy secret in {call:?}");
            assert!(!arg.contains(&password), "db password in {call:?}");
            assert!(!arg.contains("test-token-value"), "token in {call:?}");
        }
    }

    let config = fx.home.join(".config/canterlot");
    assert_eq!(
        std::fs::read_to_string(config.join("token")).unwrap(),
        "test-token-value"
    );
    let worker: toml::Table =
        toml::from_str(&std::fs::read_to_string(config.join("worker.toml")).unwrap()).unwrap();
    assert_eq!(worker["machine_id"].as_str(), Some(MACHINE));
    assert_eq!(
        worker["claude_code"]["executable"].as_str(),
        Some("/srv/tools/claude")
    );
    assert!(fx.layout.worker_unit().is_file());
    assert!(
        fx.layout
            .worker_current()
            .join("bin/canterlot-worker")
            .is_file()
    );
    assert!(fx.layout.hub_current().join("bin/canterlot-hub").is_file());
    assert!(fx.layout.web_parent().join("build/200.html").is_file());
    let caddy = std::fs::read_to_string(fx.layout.compose_dir().join("caddy/Caddyfile")).unwrap();
    assert!(caddy.contains("wss://192.0.2.10"));
    assert!(caddy.contains("tls internal"));

    let text = report.to_string();
    assert!(text.contains("https://192.0.2.10"));
    assert!(text.contains("/recover#test"));
    assert!(text.contains("passkeys"), "self-signed caveats: {text}");
    let rec = ops::latest(&fx.layout.ops_dir()).unwrap().unwrap();
    assert_eq!(rec.result, Some(OpResult::Done));
    assert_eq!(rec.db_identity.unwrap().installation_id.to_string(), IDS[1]);

    let report = status(&fx.layout, &fx.runner).await.unwrap();
    assert_eq!(report.hub_version.as_deref(), Some(VERSION));
    assert_eq!(report.worker_version.as_deref(), Some(VERSION));
    assert!(report.running.is_none());
    assert!(report.to_string().contains(VERSION));
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["hub_version"], VERSION);
    server.abort();
}

/// Адрес, с которого нельзя скачать ничего: `--from-dir` не должен трогать сеть.
fn offline_source() -> ReleaseSource {
    ReleaseSource {
        api_base: "http://127.0.0.1:9/".parse().unwrap(),
        download_base: "http://127.0.0.1:9/".parse().unwrap(),
        repo: "Fenicu/canterlot".into(),
    }
}

#[tokio::test]
async fn install_from_dir_uses_no_network() {
    let release = tempfile::tempdir().unwrap();
    release_dir(release.path(), VERSION);
    let fx = fixture();
    let mut args = args();
    args.from_dir = Some(release.path().to_path_buf());
    install(args, &fx.layout, &fx.runner, &offline_source(), &host())
        .await
        .unwrap();
    assert!(fx.layout.hub_current().join("bin/canterlot-hub").is_file());
    assert!(fx.layout.web_parent().join("build/200.html").is_file());
    assert!(
        fx.layout
            .worker_current()
            .join("bin/canterlot-worker")
            .is_file()
    );
    let rec = ops::latest(&fx.layout.ops_dir()).unwrap().unwrap();
    assert_eq!(rec.target.unwrap().as_str(), VERSION);
    assert_eq!(rec.result, Some(OpResult::Done));
    // Каталог выпуска принадлежит пользователю: установка его не удаляет.
    assert!(release.path().join("SHA256SUMS").is_file());
}

#[tokio::test]
async fn install_from_dir_with_bad_checksum_is_refused() {
    let release = tempfile::tempdir().unwrap();
    release_dir(release.path(), VERSION);
    let hub = release
        .path()
        .join(format!("canterlot-hub-{VERSION}-x86_64-linux-gnu.tar.gz"));
    std::fs::write(&hub, b"tampered").unwrap();
    let fx = fixture();
    let mut args = args();
    args.from_dir = Some(release.path().to_path_buf());
    match install(args, &fx.layout, &fx.runner, &offline_source(), &host()).await {
        Err(CtlError::ChecksumMismatch(name)) => {
            assert_eq!(name, hub.file_name().unwrap().to_str().unwrap());
        }
        other => panic!("expected checksum_mismatch, got {other:?}"),
    }
    assert!(!fx.layout.hub_current().exists());
}

#[tokio::test]
async fn install_refuses_bad_csp_before_creating_files() {
    let release = tempfile::tempdir().unwrap();
    release_dir(release.path(), VERSION);
    let csp = release.path().join(format!("canterlot-csp-{VERSION}.txt"));
    let bad = b"default-src 'self'; connect-src 'self' wss://hub.example.internal\n".to_vec();
    std::fs::write(&csp, &bad).unwrap();
    // Контрольные суммы пересчитаны: отказ должен быть именно из-за CSP.
    let mut sums = String::new();
    for (name, body) in release_files(VERSION) {
        let body = if name.starts_with("canterlot-csp-") {
            bad.clone()
        } else {
            body
        };
        let _ = writeln!(sums, "{}  {name}", hex(&Sha256::digest(&body)));
    }
    std::fs::write(release.path().join("SHA256SUMS"), sums).unwrap();

    let fx = fixture();
    let mut args = args();
    args.from_dir = Some(release.path().to_path_buf());
    let err = install(args, &fx.layout, &fx.runner, &offline_source(), &host())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("csp_invalid"), "{err}");
    for path in [
        fx.layout.root.join("etc/canterlot"),
        fx.layout.root.join("var/lib/canterlot"),
        fx.layout.root.join("opt/canterlot"),
        fx.layout.ctl_link(),
    ] {
        assert!(
            !path.exists(),
            "created before the CSP check: {}",
            path.display()
        );
    }
}

/// Тождество воркера после `uninstall` без `--purge`: конфиг и токен есть,
/// а юнит уже удалён, поэтому `classify_worker` видит чистого воркера.
fn kept_identity(fx: &Fixture, id: &str) {
    let config = fx.home.join(".config/canterlot");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("worker.toml"),
        format!(
            "machine_id = \"{id}\"\nhub_url = \"https://localhost:7443\"\ninstallation_id = \"{}\"\nrestore_generation = \"{}\"\n",
            IDS[1], IDS[2]
        ),
    )
    .unwrap();
    std::fs::write(config.join("token"), "kept-token").unwrap();
}

fn answer_machine_credentials(fx: &Fixture, active: Arc<AtomicBool>) {
    fx.runner.respond(move |cmd| {
        if stdin(cmd).contains("machine_credentials") {
            return Some(Output::ok(format!(
                "{}\n",
                u8::from(active.load(Ordering::SeqCst))
            )));
        }
        None
    });
}

#[tokio::test]
async fn install_reuses_kept_machine_when_it_is_still_active() {
    let (src, server) = release_server().await;
    let fx = fixture();
    kept_identity(&fx, MACHINE);
    answer_machine_credentials(&fx, Arc::new(AtomicBool::new(true)));
    install(args(), &fx.layout, &fx.runner, &src, &host())
        .await
        .unwrap();
    let calls = fx.runner.calls();
    assert!(
        !calls.iter().any(|c| has(c, &["machine", "add"])),
        "a second machine was issued: {calls:#?}"
    );
    let config = fx.home.join(".config/canterlot");
    let worker: toml::Table =
        toml::from_str(&std::fs::read_to_string(config.join("worker.toml")).unwrap()).unwrap();
    assert_eq!(worker["machine_id"].as_str(), Some(MACHINE));
    assert_eq!(
        std::fs::read_to_string(config.join("token")).unwrap(),
        "kept-token"
    );
    server.abort();
}

#[tokio::test]
async fn install_registers_a_new_machine_when_the_kept_one_is_revoked() {
    let (src, server) = release_server().await;
    let fx = fixture();
    kept_identity(&fx, MACHINE);
    answer_machine_credentials(&fx, Arc::new(AtomicBool::new(false)));
    install(args(), &fx.layout, &fx.runner, &src, &host())
        .await
        .unwrap();
    let calls = fx.runner.calls();
    assert!(calls.iter().any(|c| has(c, &["machine", "add"])));
    assert_eq!(
        std::fs::read_to_string(fx.home.join(".config/canterlot/token")).unwrap(),
        "test-token-value"
    );
    server.abort();
}

/// Плохой архив проходит проверку сумм, но падает при распаковке после создания каталогов.
fn release_dir_with_bad_hub(dir: &Path, version: &str) {
    release_dir(dir, version);
    let mut sums = String::new();
    for (name, body) in release_files(version) {
        let body = if name.starts_with("canterlot-hub-") {
            b"not a gzip".to_vec()
        } else {
            body
        };
        let _ = writeln!(sums, "{}  {name}", hex(&Sha256::digest(&body)));
    }
    std::fs::write(dir.join("SHA256SUMS"), sums).unwrap();
    std::fs::write(
        dir.join(format!("canterlot-hub-{version}-x86_64-linux-gnu.tar.gz")),
        b"not a gzip",
    )
    .unwrap();
}

fn relative_created(fx: &Fixture) -> Vec<PathBuf> {
    let state = InstallState::load(&fx.layout).unwrap().unwrap();
    let mut paths: Vec<_> = state
        .created_paths
        .iter()
        .map(|p| p.strip_prefix(&fx.layout.root).unwrap().to_path_buf())
        .collect();
    paths.sort();
    paths
}

#[tokio::test]
async fn failed_install_keeps_created_paths_for_a_retry() {
    let bad = tempfile::tempdir().unwrap();
    release_dir_with_bad_hub(bad.path(), VERSION);
    let good = tempfile::tempdir().unwrap();
    release_dir(good.path(), VERSION);
    let fx = fixture();
    let mut bad_args = args();
    bad_args.from_dir = Some(bad.path().to_path_buf());
    install(bad_args, &fx.layout, &fx.runner, &offline_source(), &host())
        .await
        .unwrap_err();
    // Как на ВМ: прерванная попытка уже создала каталоги и УЦ и оставила запись операции.
    assert!(fx.layout.root.join("etc/canterlot").is_dir());
    assert!(fx.layout.root.join("var/lib/canterlot").is_dir());
    assert!(fx.layout.ca_dir().join("ca.crt").is_file());
    assert!(fx.layout.ca_dir().join("ca.key").is_file());
    let unfinished = ops::unfinished(&fx.layout.ops_dir())
        .unwrap()
        .expect("failed attempt must leave an unfinished install record");
    assert_eq!(unfinished.kind, OpKind::Install);
    assert!(unfinished.result.is_none());

    let mut retry_args = args();
    retry_args.from_dir = Some(good.path().to_path_buf());
    install(
        retry_args,
        &fx.layout,
        &fx.runner,
        &offline_source(),
        &host(),
    )
    .await
    .unwrap();
    let done = ops::latest(&fx.layout.ops_dir()).unwrap().unwrap();
    assert_eq!(
        done.id, unfinished.id,
        "the rerun must resume the failed attempt's record"
    );
    assert_eq!(done.result, Some(OpResult::Done));
    let created = relative_created(&fx);
    for want in [
        "etc/canterlot",
        "usr/local/bin/canterlot",
        "var/lib/canterlot",
        "opt/canterlot",
    ] {
        assert!(
            created
                .iter()
                .any(|p| p == Path::new(want) || p.starts_with(want)),
            "{want} missing from {created:#?}"
        );
    }

    let clean = fixture();
    let mut clean_args = args();
    clean_args.from_dir = Some(good.path().to_path_buf());
    install(
        clean_args,
        &clean.layout,
        &clean.runner,
        &offline_source(),
        &host(),
    )
    .await
    .unwrap();
    assert_eq!(created, relative_created(&clean), "different created_paths");
}

#[tokio::test]
async fn purge_cleans_a_fresh_install_completely() {
    let release = tempfile::tempdir().unwrap();
    release_dir(release.path(), VERSION);
    let fx = fixture();
    let mut args = args();
    args.from_dir = Some(release.path().to_path_buf());
    install(args, &fx.layout, &fx.runner, &offline_source(), &host())
        .await
        .unwrap();
    let purge = UninstallArgs {
        purge: true,
        ..UninstallArgs::default()
    };
    let report = uninstall_hub(&purge, &fx.layout, &fx.runner, &host(), &mut |_| {
        Ok("workstation".to_owned())
    })
    .await
    .unwrap();
    assert!(report.succeeded(), "{report}");
    let text = report.to_string();
    assert!(
        !text.contains("`canterlot uninstall --purge` deletes it"),
        "{text}"
    );
    for path in [
        fx.layout.root.join("etc/canterlot"),
        fx.layout.root.join("var/lib/canterlot"),
        fx.layout.root.join("opt/canterlot"),
        fx.layout.ctl_link(),
    ] {
        assert!(!path.exists(), "still present: {}", path.display());
    }
}

fn retained(fx: &Fixture) {
    std::fs::create_dir_all(fx.layout.pg_data()).unwrap();
    std::fs::create_dir_all(fx.layout.manifest_dir()).unwrap();
    std::fs::write(
        fx.layout.manifest_dir().join("installation.toml"),
        format!(
            "installation_id = \"{}\"\nrestore_generation = \"{}\"\nrestore_in_progress = false\n",
            IDS[1], IDS[2]
        ),
    )
    .unwrap();
    let mut rec = OpRecord::new(OpKind::Install, None);
    rec.db_identity = Some(DbIdentity {
        system_identifier: "7300000000000000001".into(),
        database_oid: 16384,
        installation_id: IDS[1].parse().unwrap(),
        restore_generation: IDS[2].parse().unwrap(),
        owner_id: OWNER.parse().unwrap(),
    });
    let dir = fx.layout.ops_dir();
    let mut rec = ops::begin(&dir, rec).unwrap();
    ops::finish(&dir, &mut rec, OpResult::Done).unwrap();
}

#[tokio::test]
async fn install_never_inits_retained_db() {
    let (src, server) = release_server().await;
    let fx = fixture();
    retained(&fx);
    let args = InstallArgs {
        hub_only: true,
        ..args()
    };
    install(args.clone(), &fx.layout, &fx.runner, &src, &host())
        .await
        .unwrap();
    let calls = fx.runner.calls();
    assert!(!calls.iter().any(|c| has(c, &["hub-init"])), "{calls:#?}");
    assert!(!calls.iter().any(|c| stdin(c).contains("pg_tables")));
    assert!(calls.iter().any(|c| stdin(c).contains("pg_control_system")));
    assert!(!calls.iter().any(|c| has(c, &["machine", "add"])));

    // Другая база под тем же каталогом: установка останавливается до запуска хаба.
    let fx = fixture();
    retained(&fx);
    *fx.identity.lock().unwrap() = identity_line(&Uuid::nil().to_string());
    match install(args, &fx.layout, &fx.runner, &src, &host()).await {
        Err(CtlError::IdentityMismatch(fields)) => {
            assert!(fields.contains("installation_id"), "{fields}");
            assert!(!fields.contains(IDS[1]), "values leaked: {fields}");
        }
        other => panic!("expected identity_mismatch, got {other:?}"),
    }
    let calls = fx.runner.calls();
    assert!(!calls.iter().any(|c| has(c, &["hub-init"])));
    assert!(
        !calls
            .iter()
            .any(|c| c.args.ends_with(&["up".into(), "-d".into()]))
    );
    server.abort();
}

#[tokio::test]
async fn install_refuses_native_layout_with_adopt_hint() {
    let fx = fixture();
    let root = &fx.layout.root;
    std::fs::create_dir_all(root.join("etc/canterlot")).unwrap();
    std::fs::write(root.join("etc/canterlot/hub.toml"), b"").unwrap();
    std::fs::create_dir_all(root.join("opt/canterlot/current/bin")).unwrap();
    std::fs::write(root.join("opt/canterlot/current/bin/canterlot-hub"), b"").unwrap();
    let src = ReleaseSource::default();
    let err = install(args(), &fx.layout, &fx.runner, &src, &host())
        .await
        .unwrap_err();
    assert!(matches!(err, CtlError::NativeInstallFound), "{err:?}");
    assert!(err.to_string().contains("canterlot adopt"), "{err}");
    assert!(fx.runner.calls().is_empty());
    assert!(!fx.layout.install_toml().exists());
}

fn secrets(layout: &Layout) -> Vec<(PathBuf, Vec<u8>)> {
    [
        layout.hub_etc().join("db.url"),
        layout.hub_etc().join("proxy.secret"),
        layout.hub_etc().join("tls/hub.key"),
        layout.hub_etc().join("tls/hub.crt"),
        layout.ca_dir().join("ca.key"),
        layout.ca_dir().join("ca.crt"),
        layout.compose_dir().join("postgres.secret"),
    ]
    .into_iter()
    .map(|path| {
        let body = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        (path, body)
    })
    .collect()
}

#[tokio::test]
async fn install_resumes_partial_without_new_secrets() {
    let (src, server) = release_server().await;
    let fx = fixture();
    fx.probe_ready.store(false, Ordering::SeqCst);
    let err = install(args(), &fx.layout, &fx.runner, &src, &host())
        .await
        .unwrap_err();
    assert!(matches!(err, CtlError::NotReady(_)), "{err:?}");
    let before = secrets(&fx.layout);
    let unfinished = ops::unfinished(&fx.layout.ops_dir()).unwrap().unwrap();
    assert_eq!(unfinished.kind, OpKind::Install);

    fx.probe_ready.store(true, Ordering::SeqCst);
    install(args(), &fx.layout, &fx.runner, &src, &host())
        .await
        .unwrap();
    assert_eq!(secrets(&fx.layout), before);
    let calls = fx.runner.calls();
    assert_eq!(calls.iter().filter(|c| has(c, &["hub-init"])).count(), 1);
    let done = ops::latest(&fx.layout.ops_dir()).unwrap().unwrap();
    assert_eq!(done.id, unfinished.id, "resume continues the same record");
    assert_eq!(done.result, Some(OpResult::Done));
    server.abort();
}

#[tokio::test]
async fn install_requires_worker_user_as_plain_root() {
    let fx = fixture();
    let args = InstallArgs {
        worker_user: None,
        ..args()
    };
    let err = install(
        args,
        &fx.layout,
        &fx.runner,
        &ReleaseSource::default(),
        &host(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CtlError::WorkerUserRequired), "{err:?}");
    assert!(err.to_string().contains("--worker-user"));
    assert!(fx.runner.calls().is_empty());
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn secrets_have_expected_modes() {
    let (src, server) = release_server().await;
    let fx = fixture();
    install(args(), &fx.layout, &fx.runner, &src, &host())
        .await
        .unwrap();
    let l = &fx.layout;
    for (path, want) in [
        (l.hub_etc().join("db.url"), 0o640),
        (l.hub_etc().join("proxy.secret"), 0o640),
        (l.hub_etc().join("tls/hub.key"), 0o640),
        (l.hub_etc().join("hub.toml"), 0o640),
        (l.ca_dir().join("ca.key"), 0o600),
        (l.ca_dir(), 0o700),
        (l.compose_dir().join("postgres.secret"), 0o600),
        (l.install_toml(), 0o600),
        (l.backups(), 0o700),
        (l.ops_dir(), 0o700),
        (fx.home.join(".config/canterlot/token"), 0o600),
        (fx.home.join(".config/canterlot"), 0o700),
    ] {
        assert_eq!(mode(&path), want, "{}", path.display());
    }
    // Владелец секретов хаба — root:<gid хаба>; chown идёт через Runner.
    let chowned = fx.runner.calls().into_iter().any(|c| {
        c.program == "chown"
            && c.args.iter().any(|a| a == "0:980")
            && c.args.iter().any(|a| a.ends_with("proxy.secret"))
    });
    assert!(chowned);
    let db_url = std::fs::read_to_string(l.hub_etc().join("db.url")).unwrap();
    assert!(db_url.starts_with("postgres://canterlot:"));
    assert!(db_url.contains("@postgres:5432/canterlot"));
    server.abort();
}
