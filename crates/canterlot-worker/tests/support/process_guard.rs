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

impl ProcessGuard {
    pub fn new(root: &Path) -> Self {
        let executable = fs::canonicalize(env!("CARGO_BIN_EXE_canterlot-worker")).unwrap();
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        assert!(executable.starts_with(fs::canonicalize(target).unwrap()));
        Self {
            root: root.to_path_buf(),
            executable,
        }
    }

    fn members(&self) -> Vec<(u32, u64)> {
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

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[test]
fn process_guard_cleans_orphaned_cli_on_panic() {
    let root = tempfile::tempdir().unwrap();
    let check = ProcessGuard::new(root.path());
    let result = std::panic::catch_unwind(|| {
        let _guard = ProcessGuard::new(root.path());
        let status = std::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
            .args(["synthetic-cli", "--profile", "__detach_session"])
            .current_dir(root.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(!check.members().is_empty());
        panic!("exercise process cleanup during unwind");
    });
    assert!(result.is_err());
    assert!(check.members().is_empty(), "orphan survived guard unwind");
}
