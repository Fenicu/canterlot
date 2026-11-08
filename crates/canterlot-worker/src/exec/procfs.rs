use std::{fs, io};

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
