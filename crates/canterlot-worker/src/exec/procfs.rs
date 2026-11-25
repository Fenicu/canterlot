use std::{fs, io, os::unix::fs::MetadataExt, path::Path};

pub(super) struct ProcStat {
    pub start_ticks: u64,
    pub zombie: bool,
}

pub(super) fn boot_id() -> io::Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}

pub(super) fn stat(pid: u32) -> Option<ProcStat> {
    let text = crate::containment::read_stat(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<_> = text[text.rfind(')')? + 1..].split_whitespace().collect();
    Some(ProcStat {
        start_ticks: fields.get(19)?.parse().ok()?,
        zombie: matches!(fields.first(), Some(&"Z") | Some(&"X")),
    })
}

/// Процесс жив и это тот же процесс: совпадают pid и время старта.
pub(super) fn is_same_process(pid: u32, start_ticks: u64) -> bool {
    stat(pid).is_some_and(|stat| !stat.zombie && stat.start_ticks == start_ticks)
}

/// Состояние процесса для отладочных строк: буква состояния и время старта или `absent`.
pub(super) fn describe(pid: u32) -> String {
    match crate::containment::read_stat(format!("/proc/{pid}/stat")) {
        Ok(text) => {
            let fields: Vec<_> = text
                .rfind(')')
                .map(|end| text[end + 1..].split_whitespace().collect())
                .unwrap_or_default();
            format!(
                "state={} ppid={} pgrp={} ticks={}",
                fields.first().unwrap_or(&"?"),
                fields.get(1).unwrap_or(&"?"),
                fields.get(2).unwrap_or(&"?"),
                fields.get(19).unwrap_or(&"?")
            )
        }
        Err(error) => format!("absent({:?})", error.kind()),
    }
}

/// Процесс сопровождения этого пользователя, запущенный для каталога `exec_dir`
/// (`<exe> supervise --exec-dir <exec_dir>`): pid и время старта.
pub(super) fn find_supervisor(exec_dir: &Path) -> Option<(u32, u64)> {
    if exec_dir.as_os_str().is_empty() {
        return None;
    }
    let uid = rustix::process::getuid().as_raw();
    let wanted = exec_dir.as_os_str().as_encoded_bytes();
    for entry in fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if entry.metadata().map_or(true, |meta| meta.uid() != uid) {
            continue;
        }
        let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let args: Vec<&[u8]> = cmdline.split(|byte| *byte == 0).collect();
        let ours = args.contains(&b"supervise".as_slice())
            && args
                .windows(2)
                .any(|pair| pair[0] == b"--exec-dir" && pair[1] == wanted);
        if ours && let Some(stat) = stat(pid).filter(|stat| !stat.zombie) {
            return Some((pid, stat.start_ticks));
        }
    }
    None
}
