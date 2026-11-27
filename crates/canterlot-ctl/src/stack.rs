//! Команды к стеку compose. Всё идёт через `Runner`, секреты — только через stdin.

use std::path::Path;

use crate::{
    error::CtlError,
    layout::Layout,
    render::{HUB_BIN, HUB_CONFIG},
    runner::{Cmd, Output, Runner},
};

pub fn cmd(program: &str, args: &[&str]) -> Cmd {
    Cmd {
        program: program.into(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        stdin: None,
        as_user: None,
        env: Vec::new(),
    }
}

/// `docker compose -f <compose.yaml> <args…>`; пути внутри файла абсолютные.
pub fn compose(layout: &Layout, args: &[&str]) -> Cmd {
    let file = layout.compose_dir().join("compose.yaml");
    let mut all = vec![
        "compose".to_owned(),
        "-f".into(),
        file.display().to_string(),
    ];
    all.extend(args.iter().map(|a| (*a).to_owned()));
    Cmd {
        program: "docker".into(),
        args: all,
        stdin: None,
        as_user: None,
        env: Vec::new(),
    }
}

/// Команда бинаря хаба в работающем контейнере `hub`.
pub fn hub_exec(layout: &Layout, args: &[&str]) -> Cmd {
    let mut all = vec!["exec", "-T", "hub", HUB_BIN];
    all.extend_from_slice(args);
    all.extend_from_slice(&["--config", HUB_CONFIG]);
    compose(layout, &all)
}

/// Текст команды хаба в контейнере для печати владельцу. Путь compose показывается
/// как на хосте: корень установки в тестах подменён, в работе это `/`.
pub fn hub_hint(layout: &Layout, args: &[&str]) -> String {
    let file = layout.compose_dir().join("compose.yaml");
    let host_file = Path::new("/").join(file.strip_prefix(&layout.root).unwrap_or(&file));
    let mut parts = vec![
        "docker".to_owned(),
        "compose".into(),
        "-f".into(),
        host_file.display().to_string(),
        "exec".into(),
        "hub".into(),
        HUB_BIN.to_owned(),
    ];
    parts.extend(args.iter().map(|a| (*a).to_owned()));
    parts.join(" ")
}

/// SQL суперпользователем через локальный сокет контейнера postgres; текст запроса — в stdin.
pub fn psql(layout: &Layout, database: &str, sql: &str) -> Cmd {
    let mut cmd = compose(
        layout,
        &[
            "exec",
            "-T",
            "postgres",
            "psql",
            "-U",
            "postgres",
            "-d",
            database,
            "-v",
            "ON_ERROR_STOP=1",
            "-q",
            "-t",
            "-A",
            "-F",
            "|",
        ],
    );
    cmd.stdin = Some(sql.as_bytes().to_vec());
    cmd
}

/// Запускает и требует код 0. В ошибку попадает только метка команды, не вывод.
pub async fn run_ok(runner: &dyn Runner, label: &str, cmd: Cmd) -> Result<Output, CtlError> {
    let out = runner.run(cmd).await?;
    if out.status != 0 {
        return Err(CtlError::CommandFailed {
            program: label.into(),
            status: out.status,
        });
    }
    Ok(out)
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}
