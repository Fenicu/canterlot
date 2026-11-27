//! Сторона воркера: тождество машины, конфигурация, поиск CLI агентов.
//! `install` и `join` пишут одни и те же файлы одной функцией.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::{
    error::CtlError,
    fsx,
    layout::Layout,
    render::render_worker_toml,
    runner::{Cmd, Runner},
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
}
