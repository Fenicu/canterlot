//! `canterlot install`: стек хаба в compose и, кроме `--hub-only`, воркер на этом же хосте.
//! Каждый шаг повторяем: секреты и тождество, которые уже есть, не создаются заново,
//! а `init` выполняется только на проверенно пустой базе без манифеста.

use std::{
    fmt,
    io::{BufRead, BufReader, Write as _},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use url::Url;
use uuid::Uuid;

use crate::{
    cli::{InstallArgs, TlsMode, Version},
    error::CtlError,
    fsx,
    identity::{check_identity, db_identity},
    layout::Layout,
    ops::{self, OpKind, OpRecord, OpResult},
    release::{self, ReleaseSource, Staged},
    render::{self, EXTERNAL_BACKEND_PORT, HUB_CONFIG, HUB_GID, HUB_UID},
    root::ensure_root,
    runner::Runner,
    stack::{cmd, compose, hub_exec, hub_hint, psql, run_ok, stdout},
    state::{self, Components, HostClass, InstallState, WorkerClass},
    tls,
    worker::{self, MachineIds},
};

/// uid пользователя postgres в официальном образе на Debian.
const POSTGRES_UID: u32 = 999;
const MIN_FREE_BYTES: u64 = 5_000_000_000;
const MIN_GLIBC: (u32, u32) = (2, 41);

/// Всё, что установка узнаёт о хосте помимо аргументов; в тестах задаётся явно.
#[derive(Clone, Debug)]
pub struct Host {
    pub sudo_user: Option<String>,
    pub hostname: String,
    pub is_root: bool,
    /// Можно ли задавать вопросы через `/dev/tty`.
    pub interactive: bool,
    /// Собственный бинарь: копируется в `/opt/canterlot/ctl/<v>/` и ссылкой в `/usr/local/bin`.
    pub self_exe: Option<PathBuf>,
    pub retries: u32,
    pub retry_delay: Duration,
}

impl Host {
    pub fn from_env() -> Self {
        let sudo_user = std::env::var("SUDO_USER")
            .ok()
            .filter(|user| !user.is_empty() && user != "root");
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|name| name.trim().to_owned())
            .unwrap_or_else(|_| "localhost".into());
        Self {
            sudo_user,
            hostname,
            is_root: rustix::process::geteuid().is_root(),
            interactive: std::fs::File::options()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .is_ok(),
            self_exe: std::env::current_exe().ok(),
            retries: 90,
            retry_delay: Duration::from_secs(2),
        }
    }
}

#[derive(Debug)]
pub struct InstallReport {
    pub version: Version,
    pub pwa_url: String,
    pub recovery_link: Option<String>,
    pub add_machine: String,
    pub caveats: Vec<String>,
    pub front_block: Option<String>,
    pub warnings: Vec<String>,
    pub worker_machine: Option<Uuid>,
}

impl fmt::Display for InstallReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "canterlot {} is installed.", self.version)?;
        writeln!(f)?;
        writeln!(f, "PWA: {}", self.pwa_url)?;
        if let Some(link) = &self.recovery_link {
            writeln!(f, "First sign-in (one-time link, valid 10 minutes): {link}")?;
        }
        if let Some(machine) = self.worker_machine {
            writeln!(f, "This host's worker is connected as machine {machine}.")?;
        }
        writeln!(f)?;
        writeln!(
            f,
            "Add another machine: issue a join code on this host with"
        )?;
        writeln!(f, "  {}", self.add_machine)?;
        writeln!(f, "and run the printed command on that machine.")?;
        if let Some(block) = &self.front_block {
            writeln!(f)?;
            writeln!(f, "Site block for your front proxy:")?;
            writeln!(f)?;
            write!(f, "{block}")?;
        }
        for caveat in &self.caveats {
            writeln!(f)?;
            writeln!(f, "Note: {caveat}")?;
        }
        for warning in &self.warnings {
            writeln!(f, "Warning: {warning}")?;
        }
        Ok(())
    }
}

/// Ограничения режима self-signed; печатаются до подтверждения и в отчёте.
pub const SELF_SIGNED_LIMITS: &str = "self-signed mode works as a site in Chrome on Android and desktop after you press \"proceed\"; passkeys do not work (browsers block WebAuthn on TLS errors), sign in with a password; it does not work on iPhone (Safari refuses wss:// to a self-signed server); the browser keeps warning and may not offer installing to the home screen.";

struct Tty {
    reader: BufReader<std::fs::File>,
    writer: std::fs::File,
}

impl Tty {
    fn open() -> Result<Self, CtlError> {
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .open("/dev/tty")?;
        Ok(Self {
            writer: file.try_clone()?,
            reader: BufReader::new(file),
        })
    }

    fn ask(&mut self, question: &str, default: Option<&str>) -> Result<String, CtlError> {
        match default {
            Some(default) => write!(self.writer, "{question} [{default}]: ")?,
            None => write!(self.writer, "{question}: ")?,
        }
        self.writer.flush()?;
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        let answer = line.trim();
        if answer.is_empty() {
            return default
                .map(str::to_owned)
                .ok_or_else(|| CtlError::BadAnswer(format!("{question}: an answer is required")));
        }
        Ok(answer.to_owned())
    }
}

struct Questions {
    tty: Option<Tty>,
}

impl Questions {
    fn get(
        &mut self,
        given: Option<String>,
        flag: &'static str,
        question: &str,
        default: Option<&str>,
    ) -> Result<String, CtlError> {
        if let Some(value) = given {
            return Ok(value);
        }
        match &mut self.tty {
            Some(tty) => tty.ask(question, default),
            None => default
                .map(str::to_owned)
                .ok_or(CtlError::MissingAnswer(flag)),
        }
    }

    fn confirm(&mut self, text: &str) -> Result<(), CtlError> {
        let Some(tty) = &mut self.tty else {
            return Ok(());
        };
        writeln!(tty.writer, "{text}")?;
        let answer = tty.ask("Continue? (yes/no)", Some("no"))?;
        if answer.eq_ignore_ascii_case("yes") || answer.eq_ignore_ascii_case("y") {
            Ok(())
        } else {
            Err(CtlError::Cancelled)
        }
    }
}

fn https_origin(text: &str) -> Result<String, CtlError> {
    let text = if text.contains("://") {
        text.to_owned()
    } else {
        format!("https://{text}")
    };
    let url = Url::parse(&text).map_err(|_| CtlError::BadAnswer(format!("origin {text}")))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.path() != "/"
        || url.query().is_some()
    {
        return Err(CtlError::BadAnswer(format!(
            "origin must be https://<host> without a path: {text}"
        )));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn origin_host(origin: &str) -> String {
    Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default()
}

fn endpoint(text: &str) -> Result<String, CtlError> {
    let text = text.trim();
    if text.is_empty() || text.contains("://") || text.contains('/') {
        return Err(CtlError::BadAnswer(format!(
            "gRPC endpoint host:port: {text}"
        )));
    }
    let has_port = text
        .rsplit_once(':')
        .is_some_and(|(host, port)| !host.ends_with(':') && port.parse::<u16>().is_ok())
        && (!text.starts_with('[') || text.contains("]:"));
    if has_port {
        Ok(text.to_owned())
    } else if text.parse::<std::net::Ipv6Addr>().is_ok() {
        Ok(format!("[{text}]:{}", render::DEFAULT_GRPC_PORT))
    } else {
        Ok(format!("{text}:{}", render::DEFAULT_GRPC_PORT))
    }
}

/// Ответы на вопросы установки: из флагов, затем из терминала, иначе `missing_answer`.
fn answers(args: &InstallArgs, host: &Host) -> Result<InstallState, CtlError> {
    let mut q = Questions {
        tty: if host.interactive && !args.yes {
            Some(Tty::open()?)
        } else {
            None
        },
    };
    let mode = match args.tls {
        Some(mode) => mode,
        None => match q
            .get(
                None,
                "--tls",
                "TLS mode (acme, external, self-signed)",
                Some("acme"),
            )?
            .as_str()
        {
            "acme" => TlsMode::Acme,
            "external" => TlsMode::External,
            "self-signed" => TlsMode::SelfSigned,
            other => return Err(CtlError::BadAnswer(format!("TLS mode {other}"))),
        },
    };
    let given_origin = args.origin.clone().or_else(|| args.domain.clone());
    let (origin, front_ip) = match mode {
        TlsMode::Acme => {
            let domain = q.get(
                given_origin,
                "--domain",
                "PWA domain (must resolve to this host)",
                None,
            )?;
            (https_origin(&domain)?, None)
        }
        TlsMode::External => {
            let origin = q.get(
                given_origin,
                "--origin",
                "PWA origin served by your front proxy (https://...)",
                None,
            )?;
            let front = q.get(
                args.front_ip.map(|ip| ip.to_string()),
                "--front-ip",
                "IP address of the front proxy",
                None,
            )?;
            let front: IpAddr = front
                .parse()
                .map_err(|_| CtlError::BadAnswer(format!("front IP {front}")))?;
            (https_origin(&origin)?, Some(front))
        }
        TlsMode::SelfSigned => {
            let name = q.get(
                given_origin,
                "--origin",
                "LAN name or address of this host",
                None,
            )?;
            (https_origin(&name)?, None)
        }
    };
    // Воркеры ходят на хаб напрямую, мимо внешнего прокси: в external точку спрашиваем отдельно.
    let default_endpoint = match mode {
        TlsMode::External => None,
        _ => Some(format!(
            "{}:{}",
            origin_host(&origin),
            render::DEFAULT_GRPC_PORT
        )),
    };
    let grpc = q.get(
        args.grpc_endpoint.clone(),
        "--grpc-endpoint",
        "gRPC endpoint workers connect to (host:port)",
        default_endpoint.as_deref(),
    )?;
    let grpc = endpoint(&grpc)?;
    let owner_login = q.get(
        args.owner_login.clone(),
        "--owner-login",
        "Owner login",
        Some("owner"),
    )?;
    if mode == TlsMode::SelfSigned {
        q.confirm(SELF_SIGNED_LIMITS)?;
    }
    Ok(InstallState {
        format: state::FORMAT,
        components: Components::default(),
        tls: Some(mode),
        origin: Some(origin),
        hub_names: vec![render::endpoint_host(&grpc)],
        grpc_endpoint: Some(grpc),
        front_ip,
        owner_login: Some(owner_login),
        worker_user: None,
        worker_home: None,
        hub_uid: HUB_UID,
        hub_gid: HUB_GID,
        created_paths: Vec::new(),
        adopted_paths: Vec::new(),
    })
}

struct Passwd {
    home: PathBuf,
}

async fn passwd(runner: &dyn Runner, user: &str) -> Result<Passwd, CtlError> {
    let out = runner.run(cmd("getent", &["passwd", user])).await?;
    let line = stdout(&out);
    let fields: Vec<&str> = line.split(':').collect();
    if out.status != 0 || fields.len() < 7 || fields[5].is_empty() {
        return Err(CtlError::UserNotFound(user.into()));
    }
    Ok(Passwd {
        home: PathBuf::from(fields[5]),
    })
}

fn parse_glibc(text: &str) -> Option<(u32, u32)> {
    let first = text.lines().next()?;
    let version = first.split_whitespace().last()?;
    let (major, minor) = version.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

async fn preflight(
    runner: &dyn Runner,
    layout: &Layout,
    worker: bool,
    warnings: &mut Vec<String>,
) -> Result<(), CtlError> {
    let docker = runner.run(cmd("docker", &["compose", "version"])).await;
    if !matches!(docker, Ok(ref out) if out.status == 0) {
        return Err(CtlError::DockerMissing);
    }
    let root = layout.root.display().to_string();
    let df = runner
        .run(cmd("df", &["-B1", "--output=avail", &root]))
        .await?;
    match stdout(&df)
        .lines()
        .last()
        .and_then(|line| line.trim().parse::<u64>().ok())
    {
        Some(free) if free < MIN_FREE_BYTES => return Err(CtlError::DiskLow(free)),
        Some(_) => {}
        None => warnings.push("could not read free disk space".into()),
    }
    if worker {
        let ldd = runner.run(cmd("ldd", &["--version"])).await?;
        match parse_glibc(&String::from_utf8_lossy(&ldd.stdout)) {
            Some(found) if found < MIN_GLIBC => {
                return Err(CtlError::GlibcTooOld(format!("{}.{}", found.0, found.1)));
            }
            Some(_) => {}
            None => warnings.push("could not read the glibc version".into()),
        }
    }
    Ok(())
}

/// Распаковка во временный каталог рядом и переименование: каталог релиза либо полный, либо его нет.
fn extract_once(archive: &Path, dest: &Path) -> Result<(), CtlError> {
    if dest.exists() {
        return Ok(());
    }
    let parent = dest.parent().unwrap_or_else(|| Path::new("/"));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".extract-{}", Uuid::now_v7()));
    std::fs::create_dir(&tmp)?;
    if let Err(err) = release::extract_tar_gz(archive, &tmp) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(err);
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

fn staged_file<'a>(staged: &'a Staged, name: &str) -> Result<&'a Path, CtlError> {
    staged
        .files
        .get(name)
        .map(PathBuf::as_path)
        .ok_or_else(|| CtlError::AssetMissing(name.into()))
}

async fn retry<T, F, Fut>(host: &Host, mut attempt: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut last = String::new();
    for n in 0..host.retries.max(1) {
        if n > 0 {
            tokio::time::sleep(host.retry_delay).await;
        }
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(reason) => last = reason,
        }
    }
    Err(last)
}

fn db_password(url: &str) -> Result<String, CtlError> {
    url.trim()
        .strip_prefix("postgres://canterlot:")
        .and_then(|rest| rest.split_once('@'))
        .map(|(password, _)| password.to_owned())
        .ok_or_else(|| CtlError::BadOutput("db.url".into()))
}

fn role_sql(password: &str) -> String {
    // Пароль — hex, кавычки в нём невозможны; идёт только через stdin psql.
    format!(
        "SELECT 'CREATE ROLE canterlot LOGIN' WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'canterlot')\\gexec\n\
         ALTER ROLE canterlot WITH LOGIN PASSWORD '{password}';\n\
         SELECT 'CREATE DATABASE canterlot OWNER canterlot ENCODING ''UTF8'' TEMPLATE template0' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'canterlot')\\gexec\n"
    )
}

/// Файлы и каталоги, чьим владельцем должен стать не root; chown идёт одной командой.
#[derive(Default)]
struct Owners {
    hub_group: Vec<PathBuf>,
    hub_user: Vec<PathBuf>,
    postgres: Vec<PathBuf>,
}

/// В список для `--purge` кладём верхний созданный каталог: всё под ним тоже наше.
fn merge_created(paths: &mut Vec<PathBuf>, candidate: &Path) {
    if paths.iter().any(|p| candidate.starts_with(p)) {
        return;
    }
    paths.retain(|p| !p.starts_with(candidate));
    paths.push(candidate.to_path_buf());
}

/// Список сразу пишется в запись операции: если установка прервётся между созданием
/// каталога и сохранением install.toml, повтор знает, что именно создал первый запуск.
fn remember(
    layout: &Layout,
    state: &mut InstallState,
    rec: &mut OpRecord,
    created: Vec<PathBuf>,
) -> Result<(), CtlError> {
    let roots = crate::uninstall::install_roots(layout);
    if let Some(top) = created
        .into_iter()
        .find(|p| roots.iter().any(|r| p.starts_with(r)))
    {
        merge_created(&mut state.created_paths, &top);
    }
    rec.details.created_paths = state.created_paths.clone();
    ops::checkpoint(&layout.ops_dir(), rec)
}

struct HubFiles {
    ca_pem: String,
}

fn write_hub_files(
    layout: &Layout,
    state: &mut InstallState,
    rec: &mut OpRecord,
    staged: &Staged,
    version: &Version,
    owners: &mut Owners,
) -> Result<HubFiles, CtlError> {
    let root = &layout.root;
    let assets = release::asset_names(version);
    for (dir, mode) in [
        (root.join("etc/canterlot"), 0o755),
        (layout.compose_dir(), 0o700),
        (layout.caddy_dir(), 0o755),
        (layout.hub_etc(), 0o750),
        (layout.hub_etc().join("tls"), 0o750),
        (layout.manifest_dir(), 0o750),
        (layout.pg_data(), 0o700),
        (layout.hub_data(), 0o750),
        (layout.hub_data().join("attachments"), 0o750),
        (layout.hub_data().join("geoip"), 0o750),
        (layout.caddy_data(), 0o700),
        (layout.backups(), 0o700),
        (root.join("opt/canterlot/hub/releases"), 0o755),
        (layout.web_parent().join("releases"), 0o755),
    ] {
        let created = fsx::ensure_dir(root, &dir, mode)?;
        remember(layout, state, rec, created)?;
    }
    owners.hub_group.push(layout.hub_etc());
    owners.hub_group.push(layout.hub_etc().join("tls"));
    owners.hub_user.extend([
        layout.manifest_dir(),
        layout.hub_data(),
        layout.hub_data().join("attachments"),
        layout.hub_data().join("geoip"),
    ]);
    owners.postgres.push(layout.pg_data());

    // Секреты создаются один раз: повтор установки берёт существующие.
    let hub = layout.hub_etc();
    let db_url = format!(
        "postgres://canterlot:{}@postgres:5432/canterlot",
        fsx::random_hex()?
    );
    fsx::write_new(&hub.join("db.url"), db_url.as_bytes(), 0o640)?;
    fsx::write_new(
        &hub.join("proxy.secret"),
        fsx::random_hex()?.as_bytes(),
        0o640,
    )?;
    fsx::write_new(
        &layout.compose_dir().join("postgres.secret"),
        fsx::random_hex()?.as_bytes(),
        0o600,
    )?;

    if !layout.ca_dir().join("ca.key").is_file() {
        tls::save_ca(layout, &tls::new_ca()?)?;
    }
    let ca = tls::load_ca(layout)?;
    let tls_dir = hub.join("tls");
    if !tls_dir.join("hub.key").is_file() || !tls_dir.join("hub.crt").is_file() {
        let (cert, key) = tls::issue_hub_cert(&ca, &tls::hub_names(&state.hub_names))?;
        fsx::write_atomic_mode(&tls_dir.join("hub.key"), key.as_bytes(), 0o640)?;
        fsx::write_atomic_mode(&tls_dir.join("hub.crt"), cert.as_bytes(), 0o640)?;
    }
    fsx::write_atomic_mode(&tls_dir.join("ca.crt"), ca.cert_pem.as_bytes(), 0o640)?;
    fsx::write_atomic_mode(
        &hub.join("hub.toml"),
        render::render_hub_toml(state).as_bytes(),
        0o640,
    )?;
    for name in [
        "db.url",
        "proxy.secret",
        "hub.toml",
        "tls/hub.key",
        "tls/hub.crt",
        "tls/ca.crt",
    ] {
        owners.hub_group.push(hub.join(name));
    }

    // Релизы хаба и PWA; `current` и `build` — относительные ссылки на каталог релиза.
    extract_once(
        staged_file(staged, &assets.hub)?,
        &layout.hub_release(version).join("bin"),
    )?;
    fsx::replace_symlink(
        &Path::new("releases").join(version.as_str()),
        &layout.hub_current(),
    )?;
    extract_once(
        staged_file(staged, &assets.web)?,
        &layout.web_release(version),
    )?;
    fsx::replace_symlink(
        &Path::new("releases").join(version.as_str()),
        &layout.web_parent().join("build"),
    )?;

    let csp_txt = std::fs::read_to_string(staged_file(staged, &assets.csp)?)?;
    let origin = state.origin.clone().unwrap_or_default();
    let csp = render::csp_for_origin(&csp_txt, &origin)?;
    fsx::write_atomic_mode(
        &layout.caddy_dir().join("Caddyfile"),
        render::render_caddyfile(state, &csp).as_bytes(),
        0o644,
    )?;
    fsx::write_atomic_mode(
        &layout.compose_dir().join("compose.yaml"),
        render::render_compose(state, version).as_bytes(),
        0o600,
    )?;
    fsx::write_atomic_mode(
        &layout.compose_dir().join("compose.fenced.yaml"),
        render::render_compose_fenced().as_bytes(),
        0o600,
    )?;
    Ok(HubFiles {
        ca_pem: ca.cert_pem,
    })
}

async fn chown(runner: &dyn Runner, owner: &str, paths: &[PathBuf]) -> Result<(), CtlError> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args = vec!["-h".to_owned(), owner.to_owned(), "--".to_owned()];
    args.extend(paths.iter().map(|p| p.display().to_string()));
    let mut command = cmd("chown", &[]);
    command.args = args;
    run_ok(runner, "chown", command).await?;
    Ok(())
}

fn install_self(layout: &Layout, host: &Host) -> Result<Option<PathBuf>, CtlError> {
    let Some(exe) = &host.self_exe else {
        return Ok(None);
    };
    let version = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let dir = layout.ctl_release(&version);
    fsx::ensure_dir(&layout.root, &dir, 0o755)?;
    let target = dir.join("canterlot");
    if exe != &target {
        let bytes = std::fs::read(exe)?;
        fsx::write_atomic_mode(&target, &bytes, 0o755)?;
    }
    let link = layout.ctl_link();
    fsx::ensure_dir(&layout.root, link.parent().unwrap_or(&layout.root), 0o755)?;
    fsx::replace_symlink(&target, &link)?;
    Ok(Some(link))
}

async fn probe(
    runner: &dyn Runner,
    layout: &Layout,
    version: &Version,
    host: &Host,
) -> Result<(), CtlError> {
    #[derive(serde::Deserialize)]
    struct Probe {
        ready: bool,
        version: String,
    }
    retry(host, || async {
        let out = runner
            .run(hub_exec(layout, &["probe"]))
            .await
            .map_err(|err| err.to_string())?;
        let parsed: Probe = serde_json::from_str(&stdout(&out))
            .map_err(|_| format!("hub probe exited with {}", out.status))?;
        if out.status == 0 && parsed.ready && parsed.version == version.as_str() {
            Ok(())
        } else {
            Err(format!(
                "hub probe: ready={}, version={} (want {version})",
                parsed.ready, parsed.version
            ))
        }
    })
    .await
    .map_err(CtlError::NotReady)
}

struct WorkerPart {
    machine_id: Uuid,
}

/// Тождество воркера, пережившее uninstall без `--purge`: конфиг и токен остались,
/// а юнит и бинари уже удалены, поэтому `classify_worker` видел бы чистого воркера.
fn kept_machine_id(wl: &Layout) -> Option<Uuid> {
    let dir = wl.worker_config_dir();
    if !dir.join("worker.toml").is_file() || !dir.join("token").is_file() {
        return None;
    }
    let text = std::fs::read_to_string(dir.join("worker.toml")).ok()?;
    let table: toml::Table = toml::from_str(&text).ok()?;
    table.get("machine_id")?.as_str()?.parse().ok()
}

/// Машина в сохранённой базе ещё не отозвана: у неё есть действующая учётная запись.
async fn machine_active(runner: &dyn Runner, layout: &Layout, id: Uuid) -> Result<bool, CtlError> {
    let sql = format!(
        "select count(*) from machine_credentials where machine_id = '{id}' and revoked_at is null;\n"
    );
    let out = run_ok(
        runner,
        "psql machine credentials",
        psql(layout, "canterlot", &sql),
    )
    .await?;
    Ok(stdout(&out) != "0")
}

#[allow(clippy::too_many_arguments)]
async fn install_worker(
    runner: &dyn Runner,
    layout: &Layout,
    state: &InstallState,
    staged: &Staged,
    version: &Version,
    user: &str,
    home: &Path,
    ca_pem: &str,
    ctl_link: Option<&Path>,
    host: &Host,
) -> Result<WorkerPart, CtlError> {
    let wl = Layout::new(layout.root.clone(), Some(home.to_path_buf()));
    let assets = release::asset_names(version);
    let class = state::classify_worker(&wl)?;
    let mut owned: Vec<PathBuf> = Vec::new();

    let releases = wl.worker_release(version).parent().map(Path::to_path_buf);
    if let Some(releases) = &releases {
        owned.extend(fsx::ensure_dir(home, releases, 0o755)?);
    }
    extract_once(
        staged_file(staged, &assets.worker)?,
        &wl.worker_release(version).join("bin"),
    )?;
    fsx::replace_symlink(
        &Path::new("releases").join(version.as_str()),
        &wl.worker_current(),
    )?;
    owned.push(home.join(".local/opt/canterlot"));

    // Свою машину и токен не меняем, если тождество сохранилось и машина не отозвана:
    // иначе повторная установка оставляла бы в базе старую строку «unavailable».
    let machine_id = match kept_machine_id(&wl) {
        Some(id) if machine_active(runner, layout, id).await? => id,
        _ => {
            let out = run_ok(
                runner,
                "canterlot-hub machine add",
                hub_exec(layout, &["machine", "add", "--name", &host.hostname]),
            )
            .await?;
            let (ids, token): (MachineIds, String) =
                worker::parse_machine_add(&String::from_utf8_lossy(&out.stdout))?;
            let agents = worker::detect_agents(runner, Some(user)).await;
            let hub_url = format!("https://localhost:{}", render::grpc_port(state));
            let written = worker::write_identity(&wl, &ids, ca_pem, &token, &hub_url, &agents)?;
            owned.extend(written.created_dirs);
            owned.extend(written.files);
            ids.machine_id
        }
    };
    owned.push(wl.worker_config_dir());

    let unit = wl.worker_unit();
    if let Some(dir) = unit.parent() {
        owned.extend(fsx::ensure_dir(home, dir, 0o755)?);
    }
    fsx::write_atomic_mode(&unit, render::render_worker_unit().as_bytes(), 0o644)?;
    owned.push(unit);
    if let Some(link) = ctl_link {
        let bin = home.join(".local/bin");
        owned.extend(fsx::ensure_dir(home, &bin, 0o755)?);
        fsx::replace_symlink(link, &bin.join("canterlot"))?;
        owned.push(bin.join("canterlot"));
    }
    let mut args = vec!["-hR".to_owned(), format!("{user}:"), "--".to_owned()];
    args.extend(owned.iter().map(|p| p.display().to_string()));
    let mut chown = cmd("chown", &[]);
    chown.args = args;
    run_ok(runner, "chown", chown).await?;

    let user_cmd = |args: &[&str]| {
        let mut c = cmd("systemctl", args);
        c.as_user = Some(user.to_owned());
        c
    };
    run_ok(
        runner,
        "systemctl --user daemon-reload",
        user_cmd(&["--user", "daemon-reload"]),
    )
    .await?;
    run_ok(
        runner,
        "systemctl --user enable",
        user_cmd(&["--user", "enable", "--now", "canterlot-worker"]),
    )
    .await?;
    if class == WorkerClass::Installed {
        run_ok(
            runner,
            "systemctl --user restart",
            user_cmd(&["--user", "restart", "canterlot-worker"]),
        )
        .await?;
    }

    let sql = format!("select availability from machines where id = '{machine_id}';\n");
    retry(host, || async {
        let out = runner
            .run(psql(layout, "canterlot", &sql))
            .await
            .map_err(|err| err.to_string())?;
        if out.status == 0 && stdout(&out) == "available" {
            Ok(())
        } else {
            Err(format!("worker machine {machine_id} is not connected"))
        }
    })
    .await
    .map_err(CtlError::NotReady)?;
    Ok(WorkerPart { machine_id })
}

pub async fn install(
    args: InstallArgs,
    layout: &Layout,
    runner: &dyn Runner,
    src: &ReleaseSource,
    host: &Host,
) -> Result<InstallReport, CtlError> {
    // Утилита сама просит sudo: он сохранит SUDO_USER для пользователя воркера.
    if !host.is_root {
        let argv = std::env::args_os()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        ensure_root(&argv)?;
    }
    // Проверки без побочных эффектов идут до первой команды и первой записи.
    let worker_user = if args.hub_only {
        None
    } else {
        Some(
            args.worker_user
                .clone()
                .or_else(|| host.sudo_user.clone())
                .ok_or(CtlError::WorkerUserRequired)?,
        )
    };
    let class = state::classify_hub(layout)?;
    if class == HostClass::NativeInstall {
        return Err(CtlError::NativeInstallFound);
    }
    // Чистая установка сама создаёт эти корни, в том числе каталог блокировки ops.
    let fresh = class == HostClass::Fresh;
    // Локальный выпуск проверяется до первой записи: негодная CSP не оставляет каталогов.
    if let Some(dir) = &args.from_dir {
        let found = release::dir_version(dir)?;
        let assets = release::asset_names(&found);
        let csp_txt = std::fs::read_to_string(dir.join(assets.csp))?;
        render::csp_for_origin(&csp_txt, "https://chat.example.com")?;
    }

    let ops_dir = layout.ops_dir();
    fsx::ensure_dir(&layout.root, &ops_dir, 0o700)?;
    let _lock = ops::lock(&ops_dir)?;
    let mut rec = match state::classify_hub(layout)? {
        HostClass::Partial(_) => {
            let rec =
                ops::unfinished(&ops_dir)?.ok_or_else(|| CtlError::Other("op_vanished".into()))?;
            if rec.kind != OpKind::Install {
                return Err(CtlError::Unfinished(rec));
            }
            rec
        }
        HostClass::NativeInstall => return Err(CtlError::NativeInstallFound),
        other => {
            let version = match &args.from_dir {
                Some(dir) => {
                    let found = release::dir_version(dir)?;
                    if let Some(want) = &args.version
                        && want != &found
                    {
                        return Err(CtlError::Other(format!(
                            "version_mismatch: --version {want} but {} holds {found}",
                            dir.display()
                        )));
                    }
                    found
                }
                None => release::resolve(src, args.version.as_ref()).await?,
            };
            let mut rec = OpRecord::new(OpKind::Install, Some(version));
            if other == HostClass::Retained {
                // База от прежней установки: ожидаемое тождество фиксируется до первого шага.
                rec.db_identity =
                    Some(ops::last_identity(&ops_dir)?.ok_or(CtlError::IdentityUnknown)?);
            }
            ops::begin(&ops_dir, rec)?
        }
    };
    if fresh && rec.details.created_paths.is_empty() {
        for root in crate::uninstall::install_roots(layout) {
            merge_created(&mut rec.details.created_paths, &root);
        }
        ops::checkpoint(&ops_dir, &rec)?;
    }
    let version = rec
        .target
        .clone()
        .ok_or_else(|| CtlError::Other("install record without target".into()))?;

    let mut warnings = Vec::new();
    ops::advance(&ops_dir, &mut rec, "preflight")?;
    preflight(runner, layout, worker_user.is_some(), &mut warnings).await?;
    let worker_home = match &worker_user {
        Some(user) => {
            let entry = passwd(runner, user).await?;
            Some(layout.home.clone().unwrap_or(entry.home))
        }
        None => None,
    };
    if let Some(user) = &worker_user {
        // Без linger пользовательский менеджер воркера живёт только пока открыт сеанс.
        run_ok(
            runner,
            "loginctl enable-linger",
            cmd("loginctl", &["enable-linger", user]),
        )
        .await?;
    }

    let mut state = match InstallState::load(layout)? {
        Some(existing) => existing,
        None => answers(&args, host)?,
    };
    state.components = Components {
        postgres: true,
        hub: true,
        web: true,
        caddy: true,
        worker: state.components.worker || worker_user.is_some(),
        host_agent: false,
    };
    if worker_user.is_some() {
        state.worker_user = worker_user.clone();
        state.worker_home = worker_home.clone();
    }

    ops::advance(&ops_dir, &mut rec, "fetch")?;
    let assets = release::asset_names(&version);
    let mut wanted = vec![
        assets.hub.as_str(),
        assets.web.as_str(),
        assets.csp.as_str(),
        assets.sums.as_str(),
    ];
    if worker_user.is_some() {
        wanted.push(assets.worker.as_str());
    }
    let (staged, fetched) = match &args.from_dir {
        Some(dir) => (release::from_dir(dir, &wanted)?, false),
        None => {
            fsx::ensure_dir(&layout.root, &layout.staging(), 0o700)?;
            (
                release::fetch(src, &version, &wanted, &layout.staging()).await?,
                true,
            )
        }
    };
    if staged.version != version {
        return Err(CtlError::Other(format!(
            "version_mismatch: {} holds {}, the operation targets {version}",
            staged.dir.display(),
            staged.version
        )));
    }
    // Плохой выпуск отклоняется до первой записи в каталоги установки.
    let csp_txt = std::fs::read_to_string(staged_file(&staged, &assets.csp)?)?;
    render::csp_for_origin(&csp_txt, state.origin.as_deref().unwrap_or_default())?;

    let result = run(
        layout,
        runner,
        host,
        &mut rec,
        &mut state,
        &staged,
        &version,
        worker_user,
        worker_home,
        warnings,
    )
    .await;
    if fetched {
        // Каталог загрузки создан этим запуском; удаляем его по точному пути.
        let _ = std::fs::remove_dir_all(&staged.dir);
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn run(
    layout: &Layout,
    runner: &dyn Runner,
    host: &Host,
    rec: &mut OpRecord,
    state: &mut InstallState,
    staged: &Staged,
    version: &Version,
    worker_user: Option<String>,
    worker_home: Option<PathBuf>,
    mut warnings: Vec<String>,
) -> Result<InstallReport, CtlError> {
    let ops_dir = layout.ops_dir();
    // Повтор после прерванной попытки продолжает её список созданных путей.
    for path in rec.details.created_paths.clone() {
        merge_created(&mut state.created_paths, &path);
    }
    ops::advance(&ops_dir, rec, "configs")?;
    let mut owners = Owners::default();
    let files = write_hub_files(layout, state, rec, staged, version, &mut owners)?;
    let ctl_link = install_self(layout, host)?;
    if let Some(link) = &ctl_link {
        remember(layout, state, rec, vec![link.clone()])?;
    }
    state.save(layout)?;
    chown(runner, &format!("0:{}", state.hub_gid), &owners.hub_group).await?;
    chown(
        runner,
        &format!("{}:{}", state.hub_uid, state.hub_gid),
        &owners.hub_user,
    )
    .await?;
    chown(
        runner,
        &format!("{POSTGRES_UID}:{POSTGRES_UID}"),
        &owners.postgres,
    )
    .await?;

    ops::advance(&ops_dir, rec, "postgres")?;
    run_ok(
        runner,
        "compose up postgres",
        compose(layout, &["up", "-d", "postgres"]),
    )
    .await?;
    retry(host, || async {
        match runner
            .run(compose(
                layout,
                &["exec", "-T", "postgres", "pg_isready", "-U", "postgres"],
            ))
            .await
        {
            Ok(out) if out.status == 0 => Ok(()),
            _ => Err("postgres is not accepting connections".to_owned()),
        }
    })
    .await
    .map_err(CtlError::NotReady)?;
    let password = db_password(&std::fs::read_to_string(layout.hub_etc().join("db.url"))?)?;
    run_ok(
        runner,
        "psql create role",
        psql(layout, "postgres", &role_sql(&password)),
    )
    .await?;

    match rec.db_identity.clone() {
        Some(expected) => {
            // Сохранённая или уже инициализированная этой операцией база: только сверка.
            let got = db_identity(runner, layout).await?;
            check_identity(&expected, &got)?;
        }
        None => {
            let manifest = layout.manifest_dir().join("installation.toml");
            let ran_before = rec.history.iter().any(|(_, phase)| phase == "init");
            if manifest.exists() && !ran_before {
                return Err(CtlError::ManifestExists);
            }
            if !manifest.exists() {
                let out = run_ok(
                    runner,
                    "psql table count",
                    psql(
                        layout,
                        "canterlot",
                        "select count(*) from pg_tables where schemaname = 'public';\n",
                    ),
                )
                .await?;
                if stdout(&out) != "0" && !ran_before {
                    return Err(CtlError::DatabaseNotEmpty);
                }
                ops::advance(&ops_dir, rec, "init")?;
                run_ok(
                    runner,
                    "canterlot-hub init",
                    compose(layout, &["run", "--rm", "hub-init"]),
                )
                .await?;
            }
            rec.db_identity = Some(db_identity(runner, layout).await?);
            ops::advance(&ops_dir, rec, "initialized")?;
        }
    }
    // Владелец без пароля появился в этой операции (возможно, в прерванном запуске).
    let initialized = rec.history.iter().any(|(_, phase)| phase == "init");

    ops::advance(&ops_dir, rec, "stack")?;
    run_ok(runner, "compose up", compose(layout, &["up", "-d"])).await?;
    ops::advance(&ops_dir, rec, "probe")?;
    probe(runner, layout, version, host).await?;

    let mut worker_machine = None;
    if let (Some(user), Some(home)) = (&worker_user, &worker_home) {
        ops::advance(&ops_dir, rec, "worker")?;
        let part = install_worker(
            runner,
            layout,
            state,
            staged,
            version,
            user,
            home,
            &files.ca_pem,
            ctl_link.as_deref(),
            host,
        )
        .await?;
        worker_machine = Some(part.machine_id);
    }

    ops::advance(&ops_dir, rec, "geoip")?;
    match runner.run(hub_exec(layout, &["geoip", "update"])).await {
        Ok(out) if out.status == 0 => {}
        _ => warnings.push(
            "GeoIP database download failed; sessions show no location until `canterlot-hub geoip update` succeeds".into(),
        ),
    }
    // Ссылка первого входа нужна только новой установке: у сохранённой владелец уже есть.
    let recovery_link = if initialized {
        let out = run_ok(
            runner,
            "canterlot-hub auth recovery-link",
            hub_exec(layout, &["auth", "recovery-link"]),
        )
        .await?;
        Some(stdout(&out))
    } else {
        None
    };
    ops::finish(&ops_dir, rec, OpResult::Done)?;

    let mut caveats = Vec::new();
    let mut front_block = None;
    match state.tls {
        Some(TlsMode::SelfSigned) => caveats.push(SELF_SIGNED_LIMITS.into()),
        Some(TlsMode::Acme) => caveats.push(
            "the domain must resolve to this host and ports 80 and 443 must be reachable for Let's Encrypt.".into(),
        ),
        Some(TlsMode::External) => {
            let backend = SocketAddr::new(
                IpAddr::from([192, 0, 2, 10]),
                EXTERNAL_BACKEND_PORT,
            );
            front_block = Some(render::render_front_block(state, backend));
            caveats.push(format!(
                "replace 192.0.2.10 in the block with this host's address; the site accepts only the front proxy's address {}.",
                state.front_ip.map(|ip| ip.to_string()).unwrap_or_default()
            ));
        }
        None => {}
    }
    Ok(InstallReport {
        version: version.clone(),
        pwa_url: state.origin.clone().unwrap_or_default(),
        recovery_link,
        add_machine: hub_hint(
            layout,
            &[
                "machine",
                "join-code",
                "--config",
                HUB_CONFIG,
                "--name",
                "<machine name>",
            ],
        ),
        caveats,
        front_block,
        warnings,
        worker_machine,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_and_endpoint_are_normalized() {
        assert_eq!(
            https_origin("chat.example.com").unwrap(),
            "https://chat.example.com"
        );
        assert_eq!(
            https_origin("https://192.0.2.10/").unwrap(),
            "https://192.0.2.10"
        );
        assert!(https_origin("http://chat.example.com").is_err());
        assert!(https_origin("https://chat.example.com/app").is_err());
        assert_eq!(
            endpoint("hub.example.internal").unwrap(),
            "hub.example.internal:7443"
        );
        assert_eq!(endpoint("192.0.2.10:7444").unwrap(), "192.0.2.10:7444");
        assert_eq!(endpoint("2001:db8::1").unwrap(), "[2001:db8::1]:7443");
        assert!(endpoint("https://hub.example.internal").is_err());
    }

    #[test]
    fn glibc_version_is_read_from_ldd() {
        assert_eq!(
            parse_glibc("ldd (Debian GLIBC 2.41-12) 2.41\n"),
            Some((2, 41))
        );
        assert_eq!(parse_glibc("ldd (GNU libc) 2.36\n"), Some((2, 36)));
        assert_eq!(parse_glibc(""), None);
    }

    #[test]
    fn answers_without_terminal_require_flags() {
        let host = Host {
            sudo_user: None,
            hostname: "workstation".into(),
            is_root: true,
            interactive: false,
            self_exe: None,
            retries: 1,
            retry_delay: Duration::ZERO,
        };
        let args = InstallArgs {
            tls: Some(TlsMode::External),
            origin: Some("https://chat.example.com".into()),
            ..InstallArgs::default()
        };
        assert!(matches!(
            answers(&args, &host),
            Err(CtlError::MissingAnswer("--front-ip"))
        ));
        let args = InstallArgs {
            front_ip: Some("192.0.2.20".parse().unwrap()),
            ..args
        };
        assert!(matches!(
            answers(&args, &host),
            Err(CtlError::MissingAnswer("--grpc-endpoint"))
        ));
        let acme = answers(
            &InstallArgs {
                tls: Some(TlsMode::Acme),
                domain: Some("chat.example.com".into()),
                ..InstallArgs::default()
            },
            &host,
        )
        .unwrap();
        assert_eq!(acme.grpc_endpoint.as_deref(), Some("chat.example.com:7443"));
        assert_eq!(acme.hub_names, ["chat.example.com"]);
        assert_eq!(acme.owner_login.as_deref(), Some("owner"));
    }
}
