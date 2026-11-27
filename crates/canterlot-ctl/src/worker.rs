//! Сторона воркера: тождество машины, конфигурация, поиск CLI агентов.
//! `install` и `join` пишут одни и те же файлы одной функцией.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    cli::Version,
    error::CtlError,
    fsx,
    layout::Layout,
    release,
    render::render_worker_toml,
    runner::{Cmd, Runner},
    stack::cmd,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MachineIds {
    pub machine_id: Uuid,
    pub installation_id: Uuid,
    pub restore_generation: Uuid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Harness {
    ClaudeCode,
    Codex,
    OpenCode,
}

impl Harness {
    pub const ALL: [Harness; 3] = [Harness::ClaudeCode, Harness::Codex, Harness::OpenCode];

    pub fn binary(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude",
            Harness::Codex => "codex",
            Harness::OpenCode => "opencode",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetectedAgent {
    pub harness: Harness,
    pub executable: PathBuf,
}

/// Ответ `canterlot-hub machine add`: строки `ключ=значение`. Токен возвращается отдельно
/// и нигде не печатается.
pub fn parse_machine_add(text: &str) -> Result<(MachineIds, String), CtlError> {
    let mut machine = None;
    let mut installation = None;
    let mut generation = None;
    let mut token = None;
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        match key {
            "machine_id" => machine = value.parse::<Uuid>().ok(),
            "installation_id" => installation = value.parse::<Uuid>().ok(),
            "restore_generation" => generation = value.parse::<Uuid>().ok(),
            "token" if !value.is_empty() => token = Some(value.to_owned()),
            _ => {}
        }
    }
    match (machine, installation, generation, token) {
        (Some(machine_id), Some(installation_id), Some(restore_generation), Some(token)) => Ok((
            MachineIds {
                machine_id,
                installation_id,
                restore_generation,
            },
            token,
        )),
        _ => Err(CtlError::BadOutput("machine add".into())),
    }
}

/// Ищет CLI агентов на `PATH` учётной записи воркера (через её login shell).
pub async fn detect_agents(runner: &dyn Runner, user: Option<&str>) -> Vec<DetectedAgent> {
    let mut found = Vec::new();
    for harness in Harness::ALL {
        let cmd = Cmd {
            program: "sh".into(),
            args: vec!["-lc".into(), format!("command -v {}", harness.binary())],
            stdin: None,
            as_user: user.map(str::to_owned),
            env: Vec::new(),
        };
        let Ok(out) = runner.run(cmd).await else {
            continue;
        };
        let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if out.status == 0 && path.starts_with('/') {
            found.push(DetectedAgent {
                harness,
                executable: PathBuf::from(path),
            });
        }
    }
    found
}

/// Каталоги и файлы воркера, созданные этой функцией (для chown, когда пишет root).
pub struct Written {
    pub created_dirs: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
}

/// Пишет `worker.toml`, токен (0600) и `ca.pem` в `~/.config/canterlot` (0700).
pub fn write_identity(
    layout: &Layout,
    ids: &MachineIds,
    ca_pem: &str,
    token: &str,
    hub_url: &str,
    agents: &[DetectedAgent],
) -> Result<Written, CtlError> {
    let home = layout
        .home
        .as_deref()
        .ok_or_else(|| CtlError::Other("home_required".into()))?;
    let dir = layout.worker_config_dir();
    let created_dirs = fsx::ensure_dir(home, &dir, 0o700)?;
    let files = vec![
        dir.join("token"),
        dir.join("ca.pem"),
        dir.join("worker.toml"),
    ];
    fsx::write_atomic_mode(&files[0], token.as_bytes(), 0o600)?;
    fsx::write_atomic_mode(&files[1], ca_pem.as_bytes(), 0o644)?;
    let config = render_worker_toml(hub_url, ids, home, agents);
    fsx::write_atomic_mode(&files[2], config.as_bytes(), 0o644)?;
    Ok(Written {
        created_dirs,
        files,
    })
}

pub fn home_of(layout: &Layout) -> Result<&Path, CtlError> {
    layout
        .home
        .as_deref()
        .ok_or_else(|| CtlError::Other("home_required".into()))
}

/// Что `join`, обновление и удаление воркера узнают о машине помимо аргументов.
#[derive(Clone, Debug)]
pub struct WorkerHost {
    pub user: String,
    pub hostname: String,
    pub is_root: bool,
    /// Можно ли спрашивать через `/dev/tty`.
    pub interactive: bool,
    /// Собственный бинарь: копия ложится в `~/.local/opt/canterlot/ctl/<v>/`.
    pub self_exe: Option<PathBuf>,
    /// Уже внутри `systemd-run --user` (`CANTERLOT_CTL_IN_UNIT=1`).
    pub in_unit: bool,
    /// Опрос воркера: `polls` попыток с шагом `poll_delay` (в работе 180 × 2 с = 6 мин).
    pub polls: u32,
    pub poll_delay: Duration,
    /// Заранее данный ответ на вопрос в терминале (подтверждение `--purge`); в работе пусто.
    pub answer: Option<String>,
}

pub const IN_UNIT_ENV: &str = "CANTERLOT_CTL_IN_UNIT";

impl WorkerHost {
    pub fn from_env() -> Self {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_default();
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|name| name.trim().to_owned())
            .unwrap_or_else(|_| "localhost".into());
        Self {
            user,
            hostname,
            is_root: rustix::process::geteuid().is_root(),
            interactive: std::fs::File::options()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .is_ok(),
            self_exe: std::env::current_exe().ok(),
            in_unit: std::env::var(IN_UNIT_ENV).as_deref() == Ok("1"),
            polls: 180,
            poll_delay: Duration::from_secs(2),
            answer: None,
        }
    }

    /// Спрашивает строку через `/dev/tty`; без терминала — `missing_answer`.
    pub fn ask(&self, question: &str, flag: &'static str) -> Result<String, CtlError> {
        if let Some(answer) = &self.answer {
            return Ok(answer.clone());
        }
        if !self.interactive {
            return Err(CtlError::MissingAnswer(flag));
        }
        use std::io::{BufRead, Write};
        let mut tty = std::fs::File::options()
            .read(true)
            .write(true)
            .open("/dev/tty")?;
        write!(tty, "{question}: ")?;
        tty.flush()?;
        let mut line = String::new();
        std::io::BufReader::new(tty).read_line(&mut line)?;
        Ok(line.trim().to_owned())
    }
}

/// Ответ `canterlot-worker status --json`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub version: String,
    pub connected: bool,
    #[serde(default)]
    pub connected_since: Option<String>,
    #[serde(default)]
    pub hub_version: Option<String>,
    #[serde(default)]
    pub executions: Vec<ExecutionStatus>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionStatus {
    pub id: String,
    pub attached: bool,
    /// Бинарь живого сопровождения из `/proc/<pid>/exe`; ` (deleted)` в конце — файла нет.
    #[serde(default)]
    pub supervisor_exe: Option<PathBuf>,
}

pub fn worker_bin(layout: &Layout) -> PathBuf {
    layout.worker_current().join("bin/canterlot-worker")
}

pub fn status_cmd(layout: &Layout) -> Cmd {
    let config = layout.worker_config_dir().join("worker.toml");
    let mut status = cmd(&worker_bin(layout).display().to_string(), &[]);
    status.args = vec![
        "status".into(),
        "--json".into(),
        "--config".into(),
        config.display().to_string(),
    ];
    status
}

/// Состояние работающего воркера через его локальный сокет.
pub async fn worker_status(runner: &dyn Runner, layout: &Layout) -> Result<WorkerStatus, CtlError> {
    let out = runner.run(status_cmd(layout)).await?;
    if out.status != 0 {
        return Err(CtlError::NotReady(format!(
            "canterlot-worker status exited with {}",
            out.status
        )));
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|_| CtlError::BadOutput("canterlot-worker status --json".into()))
}

pub fn systemctl_user(args: &[&str]) -> Cmd {
    let mut all = vec!["--user"];
    all.extend_from_slice(args);
    cmd("systemctl", &all)
}

/// Повторяет проверку до успеха или до исчерпания попыток; отдаёт последнюю причину.
pub async fn poll<F, Fut>(host: &WorkerHost, mut check: F) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mut last = String::from("not checked");
    for n in 0..host.polls.max(1) {
        if n > 0 {
            tokio::time::sleep(host.poll_delay).await;
        }
        match check().await {
            Ok(()) => return Ok(()),
            Err(reason) => last = reason,
        }
    }
    Err(last)
}

/// Распаковывает архив воркера в `releases/<v>/bin` через временный каталог рядом:
/// каталог релиза либо полный, либо его нет. Существующий не трогается.
pub fn install_release(layout: &Layout, archive: &Path, v: &Version) -> Result<PathBuf, CtlError> {
    let home = home_of(layout)?;
    let dir = layout.worker_release(v);
    fsx::ensure_dir(home, &layout.worker_releases(), 0o755)?;
    if std::fs::symlink_metadata(&dir).is_ok() {
        return Ok(dir);
    }
    let tmp = layout
        .worker_releases()
        .join(format!(".extract-{}", Uuid::now_v7()));
    std::fs::create_dir(&tmp)?;
    if let Err(err) = release::extract_tar_gz(archive, &tmp.join("bin")) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(err);
    }
    std::fs::rename(&tmp, &dir)?;
    Ok(dir)
}

/// Копия утилиты в `~/.local/opt/canterlot/ctl/<v>/` и ссылка `~/.local/bin/canterlot`:
/// `canterlot update` на этой машине потом идёт без root.
pub fn install_ctl(layout: &Layout, exe: &Path, v: &Version) -> Result<PathBuf, CtlError> {
    let home = home_of(layout)?;
    let target = layout.worker_ctl(v);
    if let Some(dir) = target.parent() {
        fsx::ensure_dir(home, dir, 0o755)?;
    }
    if exe != target {
        let bytes = std::fs::read(exe)?;
        fsx::write_atomic_mode(&target, &bytes, 0o755)?;
    }
    let link = layout.worker_ctl_link();
    if let Some(dir) = link.parent() {
        fsx::ensure_dir(home, dir, 0o755)?;
    }
    fsx::replace_symlink(&target, &link)?;
    Ok(target)
}

/// Числовое сравнение версий: строки сравнили бы `1.10.0` меньше `1.9.0`.
pub fn version_key(text: &str) -> Option<(u64, u64, u64)> {
    let v = Version::parse(text).ok()?;
    let mut parts = v.as_str().split('.').map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_machine_add_requires_all_lines() {
        let nil = Uuid::nil();
        let text = format!(
            "machine_id={nil}\ninstallation_id={nil}\nrestore_generation={nil}\ntoken=abc\n"
        );
        let (ids, token) = parse_machine_add(&text).unwrap();
        assert_eq!(ids.machine_id, nil);
        assert_eq!(token, "abc");
        assert!(parse_machine_add(&format!("machine_id={nil}\ntoken=abc\n")).is_err());
        // Ошибка разбора не несёт вывода команды, где лежит токен.
        let err = parse_machine_add("token=secret-value\n").unwrap_err();
        assert!(!err.to_string().contains("secret-value"));
    }

    #[test]
    fn version_key_compares_numbers() {
        assert!(version_key("1.10.0") > version_key("1.9.0"));
        assert_eq!(version_key("abc"), None);
    }
}
