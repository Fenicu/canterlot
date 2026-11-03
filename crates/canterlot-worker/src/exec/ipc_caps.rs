//! Возможности IPC сопровождения, которое не объявляет их в `Attached` (1.0.0 и старше):
//! по выпуску, из каталога которого запущен его бинарник.

use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
};

use canterlot_proto::limits::{IPC_CAP_COMPACT, IPC_CAP_STEER};

use super::procfs;

/// Отгруженные выпуски и что понимает их сопровождение. Сжатие (`CompactRequest`) пришло
/// в bd7a9355, вливание (`Deliver` с `SteerInput`) в eb296b74; вхождение каждого проверено
/// `git merge-base --is-ancestor`.
const RELEASES: &[(&str, &[&str])] = &[
    // v1.0.0
    (
        "8be10fce886bfa48060b0fe342d893a9fa8a7bf3",
        &[IPC_CAP_COMPACT, IPC_CAP_STEER],
    ),
    (
        "d3efc1eb2e84d10453f2c5765b2141b553f02e56",
        &[IPC_CAP_COMPACT],
    ),
    (
        "ba35992ae19004adbdbfd2f0f4b1318cd4a1e57c",
        &[IPC_CAP_COMPACT],
    ),
    (
        "1d2ff79960dde5b8c39ea4330d51f6a2e8293c64",
        &[IPC_CAP_COMPACT],
    ),
    (
        "1dbdd180ffce2f57354079ef4ccd47f15dd3d112",
        &[IPC_CAP_COMPACT],
    ),
];

/// Набор по пути `…/releases/<sha>/…`. Неизвестный или неоднозначный выпуск — пусто.
pub(super) fn of_release_exe(exe: &Path) -> HashSet<String> {
    let parts: Vec<_> = exe
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    let Some(sha) = parts
        .windows(2)
        .rev()
        .find(|pair| pair[0] == "releases")
        .map(|pair| pair[1])
        .filter(|sha| sha.len() >= 7 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return HashSet::new();
    };
    let mut found = RELEASES
        .iter()
        .filter(|(full, _)| full.starts_with(&sha.to_ascii_lowercase()));
    match (found.next(), found.next()) {
        (Some((_, caps)), None) => caps.iter().map(|cap| (*cap).to_owned()).collect(),
        _ => HashSet::new(),
    }
}

/// Бинарник живого сопровождения; `None`, если процесс сменился или exe не читается.
pub(super) fn supervisor_exe(pid: u32, start_ticks: u64) -> Option<PathBuf> {
    if !procfs::is_same_process(pid, start_ticks) {
        return None;
    }
    let exe = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    procfs::is_same_process(pid, start_ticks).then_some(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(path: &str) -> Vec<String> {
        let mut caps: Vec<_> = of_release_exe(Path::new(path)).into_iter().collect();
        caps.sort();
        caps
    }

    #[test]
    fn release_directory_names_its_capabilities() {
        let root = "/home/owner/.local/opt/canterlot/releases";
        assert_eq!(
            caps(&format!("{root}/8be10fce886b/bin/canterlot-worker")),
            [IPC_CAP_COMPACT, IPC_CAP_STEER]
        );
        assert_eq!(
            caps(&format!("{root}/d3efc1e/bin/canterlot-worker (deleted)")),
            [IPC_CAP_COMPACT]
        );
        assert_eq!(
            caps(&format!("{root}/1dbdd180ffce/bin/canterlot-worker")),
            [IPC_CAP_COMPACT]
        );
    }

    #[test]
    fn unknown_or_unnamed_release_knows_nothing() {
        for path in [
            "/home/owner/.local/opt/canterlot/releases/deadbeefcafe/bin/canterlot-worker",
            "/home/owner/.local/opt/canterlot/releases/current/bin/canterlot-worker",
            "/home/owner/.local/opt/canterlot/releases/1d/bin/canterlot-worker",
            "/home/owner/src/canterlot/target/debug/canterlot-worker",
        ] {
            assert!(caps(path).is_empty(), "{path}");
        }
    }
}
