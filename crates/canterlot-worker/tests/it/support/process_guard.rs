use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use rustix::process::{Pid, Signal, kill_process};

pub struct ProcessGuard {
    root: PathBuf,
    executable: PathBuf,
}

/// Бинарник воркера для двойника CLI. Порядок: явная подмена
/// `CANTERLOT_E2E_SUPERVISOR_BIN`, путь от cargo этому тестовому бинарнику
/// (`CARGO_BIN_EXE_canterlot-worker` в окружении или на этапе компиляции), затем
/// `CARGO_TARGET_DIR` и `<repo>/target`. Внешний каталог сборки — обычный случай
/// сессий canterlot, и символическая ссылка `target` им больше не нужна.
fn supervisor_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("CANTERLOT_E2E_SUPERVISOR_BIN") {
        return fs::canonicalize(path).expect("CANTERLOT_E2E_SUPERVISOR_BIN points to no binary");
    }
    let from_cargo = std::env::var_os("CARGO_BIN_EXE_canterlot-worker")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_canterlot-worker").map(PathBuf::from));
    let from_target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(|dir| PathBuf::from(dir).join("debug").join("canterlot-worker"));
    let repo_target =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/canterlot-worker");
    for candidate in from_cargo
        .into_iter()
        .chain(from_target_dir)
        .chain([repo_target])
    {
        if let Ok(path) = fs::canonicalize(candidate) {
            return path;
        }
    }
    panic!("worker binary not found: set CARGO_TARGET_DIR or CANTERLOT_E2E_SUPERVISOR_BIN");
}

impl ProcessGuard {
    #[allow(dead_code)] // Не каждый модуль, включивший помощник, сам запускает CLI.
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            executable: supervisor_binary(),
        }
    }

    /// Сопровождение, запущенное из другого пути (каталог выпуска с жёсткой ссылкой на сборку).
    #[allow(dead_code)]
    pub fn for_executable(root: &Path, executable: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            executable: fs::canonicalize(executable).unwrap(),
        }
    }

    pub fn members(&self) -> Vec<(u32, u64)> {
        fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter_map(|entry| {
                let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
                let proc = entry.path();
                if fs::read_link(proc.join("exe")).ok()? != self.executable {
                    return None;
                }
                let args = fs::read(proc.join("cmdline")).ok()?;
                let args: Vec<_> = args.split(|b| *b == 0).collect();
                if !args
                    .iter()
                    .any(|arg| *arg == b"supervise" || *arg == b"synthetic-cli")
                {
                    return None;
                }
                let cwd_matches =
                    fs::read_link(proc.join("cwd")).is_ok_and(|cwd| cwd.starts_with(&self.root));
                let root_arg = args.iter().any(|arg| {
                    std::str::from_utf8(arg).is_ok_and(|arg| Path::new(arg).starts_with(&self.root))
                });
                if !cwd_matches && !root_arg {
                    return None;
                }
                let stat = fs::read_to_string(proc.join("stat")).ok()?;
                let fields: Vec<_> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
                if matches!(fields.first(), Some(&"Z") | Some(&"X")) {
                    return None;
                }
                Some((pid, fields.get(19)?.parse().ok()?))
            })
            .collect()
    }

    pub fn signal(&self, member: (u32, u64), signal: Signal) -> bool {
        // Повторная проверка защищает от повторного использования PID.
        if self.members().contains(&member) {
            return kill_process(Pid::from_raw(member.0 as i32).unwrap(), signal).is_ok();
        }
        false
    }

    pub fn cleanup(&self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut empty_since = None;
        loop {
            let members = self.members();
            if members.is_empty() {
                // Тишина нужна только против запуска в полёте: до exec сопровождение ещё
                // ребёнок процесса теста с чужим exe. Нет живых детей — запускать некому,
                // а потомки участников до своего exec сами видны участниками.
                if !has_live_children() {
                    break;
                }
                let since = *empty_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(600) {
                    break;
                }
            } else {
                empty_since = None;
                for member in members {
                    self.signal(member, Signal::KILL);
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let remaining = self.members();
        if !remaining.is_empty() && thread::panicking() {
            eprintln!("test processes survived cleanup: {remaining:?}");
        } else {
            assert!(
                remaining.is_empty(),
                "test processes survived cleanup: {remaining:?}"
            );
        }
    }
}

/// Есть ли у процесса теста живой (не зомби) ребёнок. Не прочли `/proc` — считаем, что есть.
fn has_live_children() -> bool {
    let me = std::process::id().to_string();
    let Ok(entries) = fs::read_dir("/proc") else {
        return true;
    };
    entries.flatten().any(|entry| {
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()))
        {
            return false;
        }
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            return false;
        };
        let Some(end) = stat.rfind(')') else {
            return false;
        };
        let mut fields = stat[end + 1..].split_whitespace();
        let state = fields.next();
        fields.next() == Some(me.as_str()) && !matches!(state, Some("Z" | "X"))
    })
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}
