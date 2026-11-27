//! Общая подделка хоста хаба для тестов обновления, удаления и `tls add-name`.
#![allow(dead_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use async_trait::async_trait;
use canterlot_ctl::{
    cli::{TlsMode, UpdateArgs, Version},
    db::ScratchDb,
    error::CtlError,
    hub_update::{UpdateReport, update_hub},
    install::Host,
    layout::Layout,
    ops::{self, DbIdentity, OpKind, OpRecord, OpResult},
    release::ReleaseSource,
    render,
    runner::{Cmd, FakeRunner, Output, Runner},
    state::{Components, InstallState},
    tls, web,
};
use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};

pub const OLD: &str = "1.2.3";
pub const NEW: &str = "1.3.0";
pub const GIB: u64 = 1 << 30;
pub const LIVE_OID: u32 = 16384;
pub const SYSID: &str = "7300000000000000001";
pub const INSTALLATION: &str = "00000000-0000-7000-8000-000000000001";
pub const GENERATION: &str = "00000000-0000-7000-8000-000000000002";
pub const OWNER: &str = "00000000-0000-7000-8000-000000000004";

pub fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

pub fn host() -> Host {
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

pub fn manifest(rows: &[(i64, &str)]) -> String {
    let list: Vec<String> = rows
        .iter()
        .map(|(v, c)| format!("{{\"version\":{v},\"description\":\"m{v}\",\"checksum\":\"{c}\"}}"))
        .collect();
    format!("{{\"migrations\":[{}]}}", list.join(","))
}

pub fn stdin(cmd: &Cmd) -> String {
    String::from_utf8_lossy(cmd.stdin.as_deref().unwrap_or_default()).into_owned()
}

pub fn has(cmd: &Cmd, needle: &[&str]) -> bool {
    cmd.args
        .windows(needle.len())
        .any(|w| w.iter().map(String::as_str).eq(needle.iter().copied()))
}

fn arg_after(cmd: &Cmd, flag: &str) -> Option<String> {
    cmd.args
        .iter()
        .position(|a| a == flag)
        .and_then(|i| cmd.args.get(i + 1).cloned())
}

pub fn pos(events: &[String], prefix: &str) -> usize {
    events
        .iter()
        .position(|e| e.starts_with(prefix))
        .unwrap_or_else(|| panic!("missing event {prefix:?} in {events:#?}"))
}

fn quoted(sql: &str) -> Vec<String> {
    sql.split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_owned())
        .collect()
}

/// Ни одна команда не удаляет живую базу; любое удаление — только служебной базы отката.
pub fn assert_no_live_drop(calls: &[Cmd]) {
    for call in calls {
        let sql = stdin(call).to_lowercase();
        for statement in sql.split(';') {
            let statement = statement.trim();
            if !statement.contains("drop database") {
                continue;
            }
            assert!(
                !statement.contains("drop database canterlot")
                    || statement.contains("drop database canterlot_"),
                "unquoted drop: {statement}"
            );
            let names = quoted(statement);
            assert_eq!(names.len(), 1, "{statement}");
            assert_ne!(names[0], "canterlot", "{statement}");
            ScratchDb::parse(&names[0]).unwrap_or_else(|_| panic!("drop of {statement}"));
        }
    }
}

fn identity_line(oid: u32) -> String {
    format!("{SYSID}|{oid}|{INSTALLATION}|{GENERATION}|{OWNER}\n")
}

pub fn identity(oid: u32) -> DbIdentity {
    DbIdentity {
        system_identifier: SYSID.into(),
        database_oid: oid,
        installation_id: INSTALLATION.parse().unwrap(),
        restore_generation: GENERATION.parse().unwrap(),
        owner_id: OWNER.parse().unwrap(),
    }
}

/// Состояние подделанного хоста.
pub struct World {
    pub dbs: BTreeMap<String, u32>,
    pub dbs_created: BTreeMap<String, u32>,
    pub migrations: BTreeMap<String, Vec<String>>,
    pub dumped: Vec<String>,
    pub hub: Option<String>,
    pub swapped: bool,
    pub events: Vec<String>,
    pub fail: BTreeSet<&'static str>,
    pub free: u64,
    pub db_size: u64,
    pub manifest: String,
    pub live_sessions: u64,
    next_oid: u32,
}

impl Default for World {
    fn default() -> Self {
        Self {
            dbs: BTreeMap::from([("canterlot".into(), LIVE_OID), ("postgres".into(), 5)]),
            dbs_created: BTreeMap::new(),
            migrations: BTreeMap::from([(
                "canterlot".into(),
                vec!["1 aa 1".into(), "2 bb 1".into()],
            )]),
            dumped: Vec::new(),
            hub: Some(OLD.into()),
            swapped: false,
            events: Vec::new(),
            fail: BTreeSet::new(),
            free: 100 * GIB,
            db_size: GIB,
            manifest: manifest(&[(1, "aa"), (2, "bb"), (3, "cc")]),
            live_sessions: 0,
            next_oid: 30000,
        }
    }
}

fn ok(text: impl Into<String>) -> Option<Output> {
    Some(Output::ok(text.into()))
}

fn failed() -> Option<Output> {
    Some(Output::failed(1, "simulated failure"))
}

fn probe_json(ready: bool, version: &str) -> String {
    format!("{{\"ready\":{ready},\"version\":\"{version}\",\"status\":{{}}}}\n")
}

impl World {
    fn maint(layout: &Layout) -> bool {
        web::maintenance_flag(layout).exists()
    }

    fn compose_version(layout: &Layout) -> String {
        let text = fs::read_to_string(layout.compose_dir().join("compose.yaml")).unwrap();
        let start = text.find("/opt/canterlot/hub/releases/").unwrap() + 28;
        text[start..].split(':').next().unwrap().to_owned()
    }

    fn handle(&mut self, cmd: &Cmd, layout: &Layout) -> Option<Output> {
        match cmd.program.as_str() {
            "df" => ok(format!("Avail\n{}\n", self.free)),
            "chown" => ok(""),
            "sh" => self.redirect(cmd),
            "docker" if cmd.args.first().map(String::as_str) == Some("run") => {
                if has(cmd, &["migrations", "manifest", "--json"]) {
                    ok(self.manifest.clone())
                } else {
                    failed()
                }
            }
            "docker" => self.compose(cmd, layout),
            _ => None,
        }
    }

    fn redirect(&mut self, cmd: &Cmd) -> Option<Output> {
        let file = PathBuf::from(&cmd.args[3]);
        let inner = &cmd.args[4..];
        let joined = inner.join(" ");
        if joined.contains("pg_dump") {
            if self.fail.contains("dump") {
                fs::write(&file, "PGDMP partial").unwrap();
                self.events.push("pg_dump failed".into());
                return failed();
            }
            fs::write(&file, "PGDMP fake").unwrap();
            self.dumped = self.migrations["canterlot"].clone();
            self.events.push("pg_dump ok".into());
            return ok("");
        }
        assert!(file.is_file(), "pg_restore reads a missing file {file:?}");
        if joined.contains("--list") {
            return ok("; TABLE public _sqlx_migrations postgres\n");
        }
        if joined.contains("/dev/null") {
            return ok("");
        }
        if joined.contains("--exit-on-error") {
            let db = inner
                .iter()
                .position(|a| a == "-d")
                .map(|i| inner[i + 1].clone())
                .unwrap();
            assert_ne!(db, "canterlot", "restore into the live database");
            if self.fail.contains("restore") {
                self.events.push(format!("pg_restore failed {db}"));
                return failed();
            }
            assert!(self.dbs.contains_key(&db), "restore into missing {db}");
            self.migrations.insert(db.clone(), self.dumped.clone());
            self.events.push(format!("pg_restore into {db}"));
            return ok("");
        }
        failed()
    }

    fn compose(&mut self, cmd: &Cmd, layout: &Layout) -> Option<Output> {
        let fenced = cmd.args.iter().any(|a| a.ends_with("compose.fenced.yaml"));
        if has(cmd, &["config"]) {
            return ok("");
        }
        if has(cmd, &["exec", "-T", "postgres", "psql"]) {
            let db = arg_after(cmd, "-d").unwrap();
            return self.sql(&db, &stdin(cmd));
        }
        if has(cmd, &["stop", "hub"]) {
            let traffic = ops::unfinished(&layout.ops_dir())
                .unwrap()
                .map(|r| format!("{:?}", r.traffic))
                .unwrap_or_default();
            self.events.push(format!(
                "stop hub maintenance={} traffic={traffic}",
                Self::maint(layout)
            ));
            self.hub = None;
            return ok("");
        }
        if has(cmd, &["exec", "-T", "hub"]) && has(cmd, &["probe"]) {
            return match self.hub.clone() {
                None => Some(Output::failed(2, "")),
                Some(version) if version == NEW && self.fail.contains("candidate") => {
                    self.events.push(format!("probe not ready {version}"));
                    Some(Output {
                        status: 1,
                        stdout: probe_json(false, &version).into_bytes(),
                        stderr: Vec::new(),
                    })
                }
                Some(version) => {
                    self.events.push(format!("probe ok {version}"));
                    ok(probe_json(true, &version))
                }
            };
        }
        if has(cmd, &["exec", "-T", "caddy"]) {
            self.events
                .push(format!("caddy reload maintenance={}", Self::maint(layout)));
            return ok("");
        }
        if has(cmd, &["down"]) {
            self.events.push("compose down".into());
            self.hub = None;
            return ok("");
        }
        if has(cmd, &["up"]) && cmd.args.last().map(String::as_str) == Some("hub") {
            let version = Self::compose_version(layout);
            self.events.push(format!(
                "up hub {version} {} maintenance={}",
                if fenced { "fenced" } else { "public" },
                Self::maint(layout)
            ));
            if version == NEW {
                let rows = self.migrations.get_mut("canterlot").unwrap();
                if !rows.iter().any(|r| r.starts_with("3 ")) {
                    rows.push("3 cc 1".into());
                }
            }
            self.hub = Some(version);
            return ok("");
        }
        None
    }

    fn sql(&mut self, db: &str, sql: &str) -> Option<Output> {
        let lower = sql.to_lowercase();
        if lower.contains("pg_database_size") {
            return ok(format!("{}\n", self.db_size));
        }
        if lower.contains("_sqlx_migrations") {
            return match self.migrations.get(db) {
                Some(rows) => ok(rows.join("\n")),
                None => failed(),
            };
        }
        if lower.contains("pg_tables") {
            return ok("42\n");
        }
        if lower.contains("pg_control_system") {
            return match self.dbs.get(db) {
                Some(oid) => ok(identity_line(*oid)),
                None => failed(),
            };
        }
        if lower.contains("from executions") {
            return ok(format!("{}\n", self.live_sessions));
        }
        if lower.contains("from pg_database where datname in") {
            let mut out = String::new();
            for (name, oid) in &self.dbs {
                if sql.contains(&format!("'{name}'")) {
                    let _ = writeln!(out, "{name}|{oid}");
                }
            }
            return ok(out);
        }
        if lower.starts_with("create database") {
            let name = quoted(sql).remove(0);
            if self.dbs.contains_key(&name) {
                return failed();
            }
            self.next_oid += 1;
            self.dbs.insert(name.clone(), self.next_oid);
            self.dbs_created.insert(name.clone(), self.next_oid);
            self.events.push(format!("create database {name}"));
            return ok("");
        }
        if lower.starts_with("drop database") {
            let name = quoted(sql).remove(0);
            if self.fail.contains("drop") {
                self.events.push(format!("drop failed {name}"));
                return failed();
            }
            self.dbs.remove(&name);
            self.migrations.remove(&name);
            self.events.push(format!("drop database {name}"));
            return ok("");
        }
        if lower.starts_with("begin;") {
            let names = quoted(sql);
            let [from1, to1, from2, to2] = names.as_slice() else {
                return failed();
            };
            let mut dbs = self.dbs.clone();
            for (from, to) in [(from1, to1), (from2, to2)] {
                let Some(oid) = dbs.remove(from) else {
                    return failed();
                };
                if dbs.contains_key(to) {
                    return failed();
                }
                dbs.insert(to.clone(), oid);
                if let Some(rows) = self.migrations.remove(from) {
                    self.migrations.insert(to.clone(), rows);
                }
            }
            self.dbs = dbs;
            self.swapped = true;
            self.events
                .push(format!("swap {from1}->{to1} {from2}->{to2}"));
            return ok("");
        }
        ok("")
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

pub fn install_state(root: &Path) -> InstallState {
    InstallState {
        format: 1,
        components: Components {
            postgres: true,
            hub: true,
            web: true,
            caddy: true,
            worker: false,
            host_agent: false,
        },
        tls: Some(TlsMode::SelfSigned),
        origin: Some("https://192.0.2.10".into()),
        grpc_endpoint: Some("192.0.2.10:7443".into()),
        front_ip: None,
        owner_login: Some("owner".into()),
        hub_names: vec!["192.0.2.10".into()],
        worker_user: None,
        worker_home: None,
        hub_uid: 980,
        hub_gid: 980,
        created_paths: vec![
            root.join("etc/canterlot"),
            root.join("var/lib/canterlot/postgres"),
            root.join("var/lib/canterlot/hub"),
            root.join("var/lib/canterlot/caddy"),
            root.join("var/lib/canterlot/backups"),
            root.join("opt/canterlot"),
        ],
        adopted_paths: Vec::new(),
    }
}

type CrashPred = Arc<dyn Fn(&World, &Cmd) -> bool + Send + Sync>;

/// Если команда подходит под условие, «процесс» умирает до её запуска (как SIGKILL).
struct Crash {
    inner: FakeRunner,
    world: Arc<Mutex<World>>,
    pred: CrashPred,
}

#[async_trait]
impl Runner for Crash {
    async fn run(&self, cmd: Cmd) -> Result<Output, CtlError> {
        let crash = (self.pred)(&self.world.lock().unwrap(), &cmd);
        if crash {
            panic!("simulated crash before {cmd:?}");
        }
        self.inner.run(cmd).await
    }
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub layout: Layout,
    pub runner: FakeRunner,
    pub world: Arc<Mutex<World>>,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let layout = Layout::new(root.clone(), None);
        let state = install_state(&root);
        let old = v(OLD);

        let bin = layout.hub_release(&old).join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("canterlot-hub"), "hub old").unwrap();
        symlink(format!("releases/{OLD}"), layout.hub_current()).unwrap();
        let web_old = layout.web_release(&old);
        fs::create_dir_all(web_old.join("_app/immutable")).unwrap();
        fs::write(web_old.join("200.html"), "old").unwrap();
        fs::write(web_old.join("_app/immutable/old.js"), "old js").unwrap();
        fs::write(web_old.join("_app/immutable/old.js.br"), "old br").unwrap();
        symlink(format!("releases/{OLD}"), layout.web_parent().join("build")).unwrap();

        fs::create_dir_all(layout.caddy_dir()).unwrap();
        fs::write(
            layout.compose_dir().join("compose.yaml"),
            render::render_compose(&state, &old),
        )
        .unwrap();
        fs::write(
            layout.compose_dir().join("compose.fenced.yaml"),
            render::render_compose_fenced(),
        )
        .unwrap();
        fs::write(layout.caddy_dir().join("Caddyfile"), "old caddyfile").unwrap();
        fs::create_dir_all(layout.manifest_dir()).unwrap();
        fs::write(
            layout.manifest_dir().join("installation.toml"),
            format!(
                "installation_id = \"{INSTALLATION}\"\nrestore_generation = \"{GENERATION}\"\nrestore_in_progress = false\n"
            ),
        )
        .unwrap();
        for dir in [
            layout.backups(),
            layout.ops_dir(),
            layout.pg_data(),
            layout.hub_data().join("attachments"),
            layout.caddy_data(),
            layout.hub_etc().join("tls"),
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        let ca = tls::new_ca().unwrap();
        tls::save_ca(&layout, &ca).unwrap();
        let (cert, key) = tls::issue_hub_cert(&ca, &tls::hub_names(&state.hub_names)).unwrap();
        fs::write(layout.hub_etc().join("tls/hub.crt"), cert).unwrap();
        fs::write(layout.hub_etc().join("tls/hub.key"), key).unwrap();
        fs::write(
            layout.hub_etc().join("hub.toml"),
            render::render_hub_toml(&state),
        )
        .unwrap();
        state.save(&layout).unwrap();

        let mut rec = OpRecord::new(OpKind::Install, Some(old));
        rec.db_identity = Some(identity(LIVE_OID));
        let mut rec = ops::begin(&layout.ops_dir(), rec).unwrap();
        ops::finish(&layout.ops_dir(), &mut rec, OpResult::Done).unwrap();

        let runner = FakeRunner::new();
        let world = Arc::new(Mutex::new(World::default()));
        {
            let world = world.clone();
            let layout = layout.clone();
            runner.respond(move |cmd| world.lock().unwrap().handle(cmd, &layout));
        }
        Self {
            dir,
            layout,
            runner,
            world,
        }
    }

    pub fn world(&self) -> MutexGuard<'_, World> {
        self.world.lock().unwrap()
    }

    pub fn events(&self) -> Vec<String> {
        self.world().events.clone()
    }

    pub fn count(&self, prefix: &str) -> usize {
        self.events()
            .iter()
            .filter(|e| e.starts_with(prefix))
            .count()
    }

    pub fn src(&self) -> ReleaseSource {
        // Обновления в тестах берутся из --from-dir; сеть не нужна.
        ReleaseSource {
            api_base: "http://127.0.0.1:9/".parse().unwrap(),
            download_base: "http://127.0.0.1:9/".parse().unwrap(),
            repo: "Fenicu/canterlot".into(),
        }
    }

    /// Каталог выпуска в форме `--from-dir`.
    pub fn release(&self, version: &str) -> PathBuf {
        let dir = self.dir.path().join(format!("release-{version}"));
        if dir.exists() {
            return dir;
        }
        fs::create_dir_all(&dir).unwrap();
        let files: Vec<(String, Vec<u8>)> = vec![
            (
                format!("canterlot-hub-{version}-x86_64-linux-gnu.tar.gz"),
                tar_gz(&[("canterlot-hub", format!("hub {version}").as_bytes())]),
            ),
            (
                format!("canterlot-web-{version}.tar.gz"),
                tar_gz(&[
                    ("200.html", b"new"),
                    ("_app/immutable/new.js", b"new js"),
                ]),
            ),
            (
                format!("canterlot-csp-{version}.txt"),
                b"default-src 'self'; connect-src 'self' wss://chat.example.com https://new.example\n"
                    .to_vec(),
            ),
            (
                format!("canterlot-{version}-x86_64-linux.tar.gz"),
                tar_gz(&[("canterlot", b"ctl")]),
            ),
        ];
        let mut sums = String::new();
        for (name, body) in &files {
            let _ = writeln!(sums, "{}  {name}", hex(&Sha256::digest(body)));
            fs::write(dir.join(name), body).unwrap();
        }
        fs::write(dir.join("SHA256SUMS"), sums).unwrap();
        fs::write(
            dir.join("RELEASE"),
            format!("sha=000000000000\nversion={version}\n"),
        )
        .unwrap();
        dir
    }

    fn args(dir: PathBuf) -> UpdateArgs {
        UpdateArgs {
            version: None,
            from_dir: Some(dir),
            force: false,
        }
    }

    pub async fn update(&self) -> Result<UpdateReport, CtlError> {
        let dir = self.release(NEW);
        self.update_from(&dir).await
    }

    pub async fn update_from(&self, dir: &Path) -> Result<UpdateReport, CtlError> {
        update_hub(
            &Self::args(dir.to_path_buf()),
            &self.layout,
            &self.runner,
            &self.src(),
            &host(),
        )
        .await
    }

    /// Запускает обновление, которое «падает» на первой подходящей команде. true — упало.
    pub async fn update_crashing(
        &self,
        pred: impl Fn(&World, &Cmd) -> bool + Send + Sync + 'static,
    ) -> bool {
        let runner = Crash {
            inner: self.runner.clone(),
            world: self.world.clone(),
            pred: Arc::new(pred),
        };
        let layout = self.layout.clone();
        let args = Self::args(self.release(NEW));
        let src = self.src();
        let task = tokio::spawn(async move {
            update_hub(&args, &layout, &runner, &src, &host())
                .await
                .map(|_| ())
        });
        match task.await {
            Err(err) => err.is_panic(),
            Ok(_) => false,
        }
    }
}
