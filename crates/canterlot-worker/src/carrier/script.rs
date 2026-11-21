//! Тексты команд на раннере, разбор ответа зонда и генерация `worker.toml`.
//!
//! Скрипты идут в `sh -s` через stdin ssh: ни кавычек оболочки входа, ни временных файлов.
//! Корень каталога раннера приходит от хаба и вставляется буквально, поэтому `valid_root`
//! пропускает только безопасные знаки.

use std::{collections::HashMap, path::Path};

use uuid::Uuid;

/// Корень раннера: абсолютный, нормализованный, из знаков, которым не нужны кавычки.
pub fn valid_root(root: &Path) -> bool {
    let Some(text) = root.to_str() else {
        return false;
    };
    // Путь сокета `<root>/hub.sock` должен влезть в `sun_path` (108 байт).
    text.starts_with('/')
        && text.len() > 1
        && text.len() <= 90
        && !text.ends_with('/')
        && !text.contains("//")
        && text
            .split('/')
            .skip(1)
            .all(|part| part != "." && part != "..")
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
}

/// Состояние раннера, как его видит зонд.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub root_exists: bool,
    pub token: bool,
    pub store: bool,
    pub toml: bool,
    pub alive: bool,
    /// Слово из `state/fatal`; пусто, если файла нет.
    pub fatal: String,
    pub ca_sha: String,
    /// Заполняется только полным зондом.
    pub bin_sha: Option<String>,
    pub bin_version: Option<String>,
}

impl Probe {
    pub fn parse(output: &str, full: bool) -> Probe {
        let values: HashMap<&str, &str> = output
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect();
        let get = |key: &str| values.get(key).copied().unwrap_or("");
        Probe {
            root_exists: get("root") == "ok",
            token: get("token") == "1",
            store: get("store") == "1",
            toml: get("toml") == "1",
            alive: get("alive") == "1",
            fatal: get("fatal").to_owned(),
            ca_sha: get("ca").to_owned(),
            bin_sha: full.then(|| get("sha").to_owned()),
            bin_version: full.then(|| get("version").to_owned()),
        }
    }
}

/// Общая часть: функция `alive`, живой ли процесс из pid-файла. Строка pid-файла —
/// `<pid> <время старта по /proc/<pid>/stat>`: так чужой процесс с переиспользованным pid
/// не принимается за воркер, а зомби не считается живым.
const ALIVE_FN: &str = r#"alive() {
  [ -s state/worker.pid ] || return 1
  read pid start < state/worker.pid
  [ -r /proc/$pid/stat ] || return 1
  set -- $(sed 's/^.*) //' /proc/$pid/stat)
  [ "$1" != Z ] && [ "${20}" = "$start" ]
}
"#;

/// Создаёт каталоги раннера; `state` закрыт для чужих, как требует хранилище воркера.
pub fn layout(root: &Path) -> String {
    let root = root.display();
    format!(
        "umask 022\n\
         mkdir -p {root}/bin {root}/jobs/personal {root}/jobs/work {root}/repos/personal {root}/repos/work || exit 1\n\
         mkdir -p {root}/state || exit 1\n\
         chmod 700 {root}/state\n"
    )
}

pub fn probe(root: &Path, full: bool) -> String {
    let root = root.display();
    let mut script = format!(
        "cd {root} 2>/dev/null || {{ echo root=missing; exit 0; }}\n\
         {ALIVE_FN}\
         echo root=ok\n\
         if [ -f token ]; then echo token=1; else echo token=0; fi\n\
         if [ -d state/cj ]; then echo store=1; else echo store=0; fi\n\
         if [ -f worker.toml ]; then echo toml=1; else echo toml=0; fi\n\
         echo fatal=$(head -c 64 state/fatal 2>/dev/null | tr -c 'a-z_' '_')\n\
         if alive; then echo alive=1; else echo alive=0; fi\n\
         echo ca=$(sha256sum ca.pem 2>/dev/null | cut -d' ' -f1)\n"
    );
    if full {
        script.push_str(
            "echo sha=$(sha256sum bin/canterlot-worker 2>/dev/null | cut -d' ' -f1)\n\
             echo version=$(bin/canterlot-worker --version < /dev/null 2>/dev/null | head -n 1)\n",
        );
    }
    script
}

/// Запускает воркер отдельным сеансом; pid и время старта пишет сам процесс, а скрипт
/// ждёт файл, чтобы следующий зонд не принял ещё не записанный pid за смерть.
pub fn start(root: &Path) -> String {
    let root = root.display();
    format!(
        "cd {root} || exit 1\n\
         rm -f state/worker.pid\n\
         if [ \"$(wc -c < state/worker.log 2>/dev/null || echo 0)\" -gt 5242880 ]; then mv -f state/worker.log state/worker.log.1; fi\n\
         setsid sh -c 'set -- $(sed \"s/^.*) //\" /proc/$$/stat); echo \"$$ ${{20}}\" > state/worker.pid.tmp; mv state/worker.pid.tmp state/worker.pid; exec bin/canterlot-worker run --config worker.toml >> state/worker.log 2>&1 < /dev/null' > /dev/null 2>&1 < /dev/null &\n\
         i=0\n\
         while [ ! -s state/worker.pid ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done\n\
         [ -s state/worker.pid ] || exit 1\n"
    )
}

/// Просит воркер завершиться; через десять секунд убивает. Работы на месте переживают это:
/// их оболочки живут отдельными сеансами.
pub fn stop(root: &Path) -> String {
    let root = root.display();
    format!(
        "cd {root} 2>/dev/null || exit 0\n\
         {ALIVE_FN}\
         alive || {{ rm -f state/worker.pid; exit 0; }}\n\
         pid=$(cut -d' ' -f1 state/worker.pid)\n\
         kill -TERM \"$pid\" 2>/dev/null\n\
         i=0\n\
         while [ $i -lt 50 ] && alive; do sleep 0.2; i=$((i+1)); done\n\
         if alive; then kill -KILL \"$pid\" 2>/dev/null; sleep 0.2; fi\n\
         rm -f state/worker.pid\n"
    )
}

/// Сносит локальное состояние воркера: после сброса поколения хаб ждёт чистого хранилища.
pub fn wipe_state(root: &Path) -> String {
    let root = root.display();
    format!(
        "cd {root} || exit 1\n\
         rm -rf -- {root}/state\n\
         mkdir {root}/state && chmod 700 {root}/state\n"
    )
}

/// Строка для `ssh <alias> …`: сначала чистит старый сокет, потом создаёт каталог.
pub fn remove_stale_socket(root: &Path) -> String {
    format!("rm -f -- {}/hub.sock", root.display())
}

pub fn make_root(root: &Path) -> String {
    format!("mkdir -p -- {}", root.display())
}

/// Приём файла со stdin: во временное имя, затем переименование. Для токена `umask 077`.
pub fn receive_file(root: &Path, name: &str, mode: Mode) -> String {
    let root = root.display();
    let umask = if matches!(mode, Mode::Secret) {
        "umask 077; "
    } else {
        ""
    };
    let chmod = if matches!(mode, Mode::Executable) {
        format!(" && chmod 755 {root}/{name}.new")
    } else {
        String::new()
    };
    format!("{umask}cat > {root}/{name}.new{chmod} && mv -f {root}/{name}.new {root}/{name}")
}

#[derive(Clone, Copy)]
pub enum Mode {
    Secret,
    Plain,
    Executable,
}

pub fn remove_fatal(root: &Path) -> String {
    format!("rm -f -- {}/state/fatal", root.display())
}

/// Идентификаторы машины, которые носитель получил вместе с токеном.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ids {
    pub machine_id: Uuid,
    pub installation_id: Uuid,
    pub restore_generation: Uuid,
}

impl Ids {
    /// Те же идентификаторы из уже лежащего на раннере `worker.toml`.
    pub fn from_config(text: &str) -> Option<Ids> {
        let table: toml::Table = toml::from_str(text).ok()?;
        let id = |key: &str| Uuid::parse_str(table.get(key)?.as_str()?).ok();
        Some(Ids {
            machine_id: id("machine_id")?,
            installation_id: id("installation_id")?,
            restore_generation: id("restore_generation")?,
        })
    }
}

/// Всё, что попадает в генерируемый `worker.toml` кроме идентификаторов.
#[derive(Clone, Debug)]
pub struct ConfigInputs<'a> {
    pub hub_url: &'a str,
    pub root: &'a Path,
    pub max_running: u32,
    pub lease_slots: u32,
}

fn quoted(text: &str) -> String {
    toml::Value::String(text.to_owned()).to_string()
}

/// Конфигурация раннера: без таблиц харнессов, с рабочими корнями только под `jobs/`.
pub fn render_config(inputs: &ConfigInputs<'_>, ids: &Ids) -> String {
    let root = inputs.root.display().to_string();
    format!(
        "# Сгенерировано носителем; правки теряются.\n\
         hub_url = {hub_url}\n\
         hub_dial = {dial}\n\
         ca_cert = {ca}\n\
         token_file = {token}\n\
         machine_id = \"{machine}\"\n\
         installation_id = \"{installation}\"\n\
         restore_generation = \"{generation}\"\n\
         data_dir = {data}\n\
         \n\
         [execution]\n\
         allowed_workdir_roots = [{jobs}]\n\
         \n\
         [memory]\n\
         work_roots = [{work}]\n\
         \n\
         [jobs]\n\
         max_running = {max_running}\n\
         \n\
         [machine]\n\
         lease_slots = {lease_slots}\n",
        hub_url = quoted(inputs.hub_url),
        dial = quoted(&format!("unix:{root}/hub.sock")),
        ca = quoted(&format!("{root}/ca.pem")),
        token = quoted(&format!("{root}/token")),
        machine = ids.machine_id,
        installation = ids.installation_id,
        generation = ids.restore_generation,
        data = quoted(&format!("{root}/state")),
        jobs = quoted(&format!("{root}/jobs")),
        work = quoted(&format!("{root}/jobs/work")),
        max_running = inputs.max_running,
        lease_slots = inputs.lease_slots,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_must_be_absolute_normalized_and_quote_free() {
        for good in [
            "/srv/runner",
            "/home/builder/canterlot-runner",
            "/a/b_c.d-e",
        ] {
            assert!(valid_root(Path::new(good)), "{good}");
        }
        for bad in [
            "",
            "/",
            "relative/dir",
            "/srv/runner/",
            "/srv//runner",
            "/srv/./runner",
            "/srv/../runner",
            "/srv/run ner",
            "/srv/run'ner",
            "/srv/run$ner",
            "/srv/run;ner",
            "/srv/runner\n",
            "/srv/run~ner",
        ] {
            assert!(!valid_root(Path::new(bad)), "{bad:?}");
        }
    }

    #[test]
    fn probe_output_is_parsed_and_ignores_noise_lines() {
        let output = "Welcome back\nroot=ok\ntoken=1\nstore=0\ntoml=1\nfatal=unauthenticated\n\
                      alive=0\nca=abc\nsha=def\nversion=canterlot-worker 1.2.3\n";
        let probe = Probe::parse(output, true);
        assert!(probe.root_exists && probe.token && probe.toml && !probe.store && !probe.alive);
        assert_eq!(probe.fatal, "unauthenticated");
        assert_eq!(probe.ca_sha, "abc");
        assert_eq!(probe.bin_sha.as_deref(), Some("def"));
        assert_eq!(probe.bin_version.as_deref(), Some("canterlot-worker 1.2.3"));
        let light = Probe::parse(output, false);
        assert_eq!(light.bin_sha, None);
        assert_eq!(Probe::parse("", false), Probe::default());
    }

    #[test]
    fn generated_config_round_trips_through_the_worker_schema() {
        let ids = Ids {
            machine_id: Uuid::now_v7(),
            installation_id: Uuid::now_v7(),
            restore_generation: Uuid::now_v7(),
        };
        let root = Path::new("/srv/runner");
        let text = render_config(
            &ConfigInputs {
                hub_url: "https://hub.example:8443",
                root,
                max_running: 3,
                lease_slots: 2,
            },
            &ids,
        );
        let config: crate::config::WorkerConfig = toml::from_str(&text).unwrap();
        assert_eq!(
            config.hub_dial.as_deref(),
            Some("unix:/srv/runner/hub.sock")
        );
        assert_eq!(config.data_dir, Path::new("/srv/runner/state"));
        assert_eq!(config.token_file, Path::new("/srv/runner/token"));
        assert_eq!(config.ca_cert, Path::new("/srv/runner/ca.pem"));
        assert_eq!(
            config.execution.allowed_workdir_roots,
            [Path::new("/srv/runner/jobs")]
        );
        assert_eq!(config.memory.work_roots, ["/srv/runner/jobs/work"]);
        assert_eq!(config.jobs.max_running, 3);
        assert_eq!(config.machine.lease_slots, 2);
        assert!(
            config.claude_code.is_none() && config.codex.is_none() && config.opencode.is_none()
        );
        assert_eq!(Ids::from_config(&text), Some(ids));
    }
}
