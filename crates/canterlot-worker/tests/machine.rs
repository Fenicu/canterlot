#![forbid(unsafe_code)]

use canterlot_proto::limits::{MAX_WORKDIR_BYTES, MAX_WORKDIR_SUGGESTIONS};
use canterlot_worker::{
    adapter::catalog,
    config::WorkerConfig,
    machine::{
        build_report,
        metrics::{
            ExecutionProbe, MetricsCollector, parse_loadavg, parse_meminfo, parse_proc_stat,
        },
        workdirs::{WorkdirSuggester, list_directories},
    },
};
use prost::Message;
use std::{fs, os::unix::fs::MetadataExt, path::Path};
use uuid::Uuid;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("stat"),
        "cpu 100 0 100 800 0 0 0 0 0 0\ncpu0 100 0 100 800\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("meminfo"),
        "MemTotal: 1000 kB\nMemAvailable: 250 kB\n",
    )
    .unwrap();
    fs::write(dir.path().join("loadavg"), "0.25 1.50 12.01 1/100 123\n").unwrap();
    dir
}
fn collector(dir: &Path) -> MetricsCollector {
    MetricsCollector::new(dir.into(), vec![dir.into(), dir.join(".")], dir.into())
}
fn process(dir: &Path, pid: u32, ppid: u32, ticks: u64, cpu: u64) {
    let path = dir.join(pid.to_string());
    fs::create_dir_all(&path).unwrap();
    let mut fields = vec!["0".to_owned(); 22];
    fields[0] = "S".into();
    fields[1] = ppid.to_string();
    fields[11] = cpu.to_string();
    fields[19] = ticks.to_string();
    fs::write(
        path.join("stat"),
        format!("{pid} (name with ) spaces) {}", fields.join(" ")),
    )
    .unwrap();
    fs::write(path.join("statm"), "100 12 0 0 0 0 0").unwrap();
}
#[test]
fn proc_stat_delta_gives_cpu_permille() {
    let dir = fixture();
    let mut c = collector(dir.path());
    assert!(parse_proc_stat("cpu broken").is_err());
    assert_eq!(c.sample(&[]).unwrap().cpu_permille, 0);
    fs::write(
        dir.path().join("stat"),
        "cpu 150 0 150 900 0 0 0 0 50 0\ncpu0 0\n",
    )
    .unwrap();
    assert_eq!(c.sample(&[]).unwrap().cpu_permille, 500);
}
#[tokio::test]
async fn proc_source_samples_through_one_collector_between_calls() {
    use canterlot_worker::machine::metrics::{MetricsSource, ProcMetricsSource};
    let dir = fixture();
    let source = ProcMetricsSource::new(collector(dir.path()));
    let first = source.sample(vec![]).await.unwrap();
    assert_eq!(first.mem_total_bytes, 1_024_000);
    assert_eq!(first.load1_centi, 25);
    assert_eq!(first.cpu_permille, 0);
    fs::write(
        dir.path().join("stat"),
        "cpu 150 0 150 900 0 0 0 0 50 0\ncpu0 0\n",
    )
    .unwrap();
    assert_eq!(source.sample(vec![]).await.unwrap().cpu_permille, 500);
}
#[test]
fn meminfo_and_loadavg_parse_fixtures() {
    assert_eq!(
        parse_meminfo("MemTotal: 1000 kB\nMemAvailable: 250 kB\n").unwrap(),
        (1024000, 256000)
    );
    assert_eq!(
        parse_loadavg("0.25 1.50 12.01 1/100 123").unwrap(),
        (25, 150, 1201)
    );
    assert!(parse_meminfo("MemTotal: nope kB").is_err());
    assert!(parse_loadavg("NaN 1 2").is_err());
}
#[test]
fn disk_usage_reports_total_and_free_and_dedupes_by_device() {
    let dir = fixture();
    let sample = collector(dir.path()).sample(&[]).unwrap();
    assert_eq!(sample.disks.len(), 1);
    assert_eq!(sample.disks[0].label, "data");
    assert!(sample.disks[0].total_bytes > 0);
    assert!(sample.disks[0].free_bytes > 0);
    assert!(sample.disks[0].free_bytes <= sample.disks[0].total_bytes);
    // Корень на другом устройстве получает собственную метку по своему номеру.
    let other = Path::new("/dev/shm");
    if other.exists()
        && fs::metadata(other).unwrap().dev() != fs::metadata(dir.path()).unwrap().dev()
    {
        let roots = vec![dir.path().into(), other.into()];
        let sample = MetricsCollector::new(dir.path().into(), roots, dir.path().into())
            .sample(&[])
            .unwrap();
        let labels: Vec<_> = sample.disks.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(labels, ["data", "root_1"]);
    }
}
#[test]
fn execution_probe_counts_only_members_with_matching_start_ticks() {
    let dir = fixture();
    process(dir.path(), 123, 1, 50, 10);
    let mut c = collector(dir.path());
    let id = Uuid::now_v7();
    let sample = c
        .sample(&[ExecutionProbe {
            execution_id: id,
            members: vec![(123, 49)],
        }])
        .unwrap();
    assert!(!sample.executions[0].alive);
    assert_eq!(sample.executions[0].rss_bytes, 0);
    let sample = c
        .sample(&[ExecutionProbe {
            execution_id: id,
            members: vec![(123, 50)],
        }])
        .unwrap();
    assert!(sample.executions[0].alive);
    assert_eq!(
        sample.executions[0].rss_bytes,
        12 * rustix::param::page_size() as u64
    );
}
#[test]
fn execution_cpu_sums_descendants_of_the_cli() {
    let dir = fixture();
    process(dir.path(), 10, 1, 50, 0);
    process(dir.path(), 11, 10, 51, 0);
    process(dir.path(), 12, 11, 52, 0);
    process(dir.path(), 20, 1, 53, 0);
    process(dir.path(), 21, 10, 49, 0);
    let probe = ExecutionProbe {
        execution_id: Uuid::now_v7(),
        members: vec![(10, 50)],
    };
    let mut c = collector(dir.path());
    c.sample(std::slice::from_ref(&probe)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    process(dir.path(), 11, 10, 51, 5);
    process(dir.path(), 12, 11, 52, 5);
    process(dir.path(), 20, 1, 53, 1000);
    process(dir.path(), 21, 10, 49, 1000);
    let row = c.sample(&[probe]).unwrap().executions.remove(0);
    assert!(row.cpu_permille > 0 && row.cpu_permille < 5000);
    assert_eq!(row.rss_bytes, 36 * rustix::param::page_size() as u64);
}
#[test]
fn metrics_message_has_only_whitelisted_labels_and_stays_small() {
    let dir = fixture();
    let sample = collector(dir.path()).sample(&[]).unwrap();
    assert!(sample.encoded_len() < 4096);
    assert!(
        sample
            .disks
            .iter()
            .all(|d| d.label == "data" || d.label.starts_with("root_"))
    );
    assert!(!format!("{sample:?}").contains(dir.path().to_str().unwrap()));
}
#[test]
fn report_carries_the_home_of_the_user_the_worker_runs_as() {
    let dir = fixture();
    let report = build_report(&config(dir.path()), &catalog());
    let home = std::env::home_dir().unwrap();
    assert!(home.is_absolute());
    assert_eq!(report.home_dir, home.to_str().unwrap());
}
fn roots_of(dir: &Path) -> Vec<std::path::PathBuf> {
    vec![dir.to_path_buf()]
}
fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}
#[test]
fn suggestions_list_only_directories_matching_the_prefix_in_order() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["zeta", "alpha", "alps", "beta"] {
        fs::create_dir(dir.path().join(name)).unwrap();
    }
    fs::write(dir.path().join("almanac.txt"), "file").unwrap();
    let parent = text(dir.path());
    assert_eq!(
        list_directories(&format!("{parent}/al"), &roots_of(dir.path())),
        vec![format!("{parent}/alpha"), format!("{parent}/alps")]
    );
    assert_eq!(
        list_directories(&format!("{parent}/"), &roots_of(dir.path())),
        vec![
            format!("{parent}/alpha"),
            format!("{parent}/alps"),
            format!("{parent}/beta"),
            format!("{parent}/zeta"),
        ]
    );
    assert!(list_directories(&format!("{parent}/missing/"), &roots_of(dir.path())).is_empty());
}
#[test]
fn suggestions_show_hidden_directories_only_for_a_dot_prefix() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join(".cache")).unwrap();
    fs::create_dir(dir.path().join("cache")).unwrap();
    let parent = text(dir.path());
    assert_eq!(
        list_directories(&format!("{parent}/"), &roots_of(dir.path())),
        vec![format!("{parent}/cache")]
    );
    assert_eq!(
        list_directories(&format!("{parent}/."), &roots_of(dir.path())),
        vec![format!("{parent}/.cache")]
    );
}
#[test]
fn suggestions_are_capped_after_sorting() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..(MAX_WORKDIR_SUGGESTIONS + 5) {
        fs::create_dir(dir.path().join(format!("d{index:03}"))).unwrap();
    }
    let parent = text(dir.path());
    let found = list_directories(&format!("{parent}/d"), &roots_of(dir.path()));
    assert_eq!(found.len(), MAX_WORKDIR_SUGGESTIONS);
    assert_eq!(found[0], format!("{parent}/d000"));
    assert_eq!(found[29], format!("{parent}/d029"));
}
#[test]
fn suggestions_refuse_relative_parent_nul_and_oversized_paths() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("work")).unwrap();
    let parent = text(dir.path());
    let roots = roots_of(dir.path());
    let oversized = format!("{parent}/{}", "a".repeat(MAX_WORKDIR_BYTES));
    for path in [
        String::new(),
        "work".to_owned(),
        format!("{parent}/../"),
        format!("{parent}/work/../"),
        format!("{parent}/\0"),
        oversized,
    ] {
        assert!(list_directories(&path, &roots).is_empty(), "{path:?}");
    }
}
#[test]
fn suggestions_obey_allowed_roots_by_canonical_path() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let outside = base.path().join("outside");
    fs::create_dir_all(root.join("inner")).unwrap();
    fs::create_dir_all(outside.join("secret")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    std::os::unix::fs::symlink(root.join("inner"), outside.join("back")).unwrap();
    let roots = vec![root.clone()];
    assert_eq!(
        list_directories(&format!("{}/", text(&root)), &roots),
        vec![format!("{}/inner", text(&root))],
        "a link out of the root would be refused at start"
    );
    assert!(
        list_directories(&format!("{}/se", text(&outside)), &roots).is_empty(),
        "a directory outside the root is not listed"
    );
    assert_eq!(
        list_directories(&format!("{}/back", text(&outside)), &roots),
        Vec::<String>::new(),
        "a directory unrelated to the roots is not read at all"
    );
    assert_eq!(
        list_directories(&format!("{}/ro", text(base.path())), &roots),
        vec![format!("{}/root", text(base.path()))],
        "an ancestor of a root leads to the root and nowhere else"
    );
    assert_eq!(
        list_directories(&format!("{}/", text(base.path())), &roots),
        vec![format!("{}/root", text(base.path()))],
    );
    assert!(list_directories(&format!("{}/", text(&root)), &[]).is_empty());
}
#[test]
fn suggestions_of_an_unreadable_directory_are_empty() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked");
    fs::create_dir_all(locked.join("inner")).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let found = list_directories(&format!("{}/", text(&locked)), &roots_of(dir.path()));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(found.is_empty());
}
#[tokio::test]
async fn suggester_gives_up_at_the_time_limit_and_keeps_one_listing_at_a_time() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    let (release, hold) = mpsc::channel::<()>();
    let hold = Arc::new(Mutex::new(hold));
    let calls = Arc::new(AtomicUsize::new(0));
    let (held, counted) = (hold.clone(), calls.clone());
    let suggester = WorkdirSuggester::with_lister(
        std::time::Duration::from_millis(30),
        Arc::new(move |_: &str| {
            counted.fetch_add(1, Ordering::SeqCst);
            held.lock().unwrap().recv().unwrap();
            vec!["/found".to_owned()]
        }),
    );
    assert!(suggester.suggest("/slow".into()).await.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        suggester.suggest("/slow".into()).await.is_empty(),
        "a stuck listing is not joined by a second thread"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    release.send(()).unwrap();
    release.send(()).unwrap();
    let found = loop {
        let found = suggester.suggest("/slow".into()).await;
        if !found.is_empty() {
            break found;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    assert_eq!(found, ["/found"]);
}
fn config(dir: &Path) -> WorkerConfig {
    toml::from_str(&format!("hub_url='https://localhost'\nca_cert='ca'\ntoken_file='token'\nmachine_id='{}'\ninstallation_id='{}'\nrestore_generation='{}'\ndata_dir='{}'\n[execution]\nallowed_workdir_roots=['{}']",Uuid::now_v7(),Uuid::now_v7(),Uuid::now_v7(),dir.display(),dir.join(".").display())).unwrap()
}
#[test]
fn report_carries_canonical_roots_without_instruction_profiles() {
    // Профили инструкций воркера убраны планом 2c задачей 3: пресеты хранит хаб,
    // поле `profiles` зарезервировано в proto.
    let dir = fixture();
    let report = build_report(&config(dir.path()), &catalog());
    assert_eq!(
        report.allowed_roots,
        vec![dir.path().canonicalize().unwrap().to_string_lossy()]
    );
    assert_eq!(
        report.max_attachment_bytes,
        canterlot_proto::limits::MAX_ATTACHMENT_BYTES
    );
    assert_eq!(report.max_attachments_per_input, 10);
}
#[test]
fn report_carries_the_os_line_from_os_release_and_kernel() {
    let dir = fixture();
    let report = build_report(&config(dir.path()), &catalog());
    let text = fs::read_to_string("/etc/os-release").unwrap();
    let get = |key: &str| {
        text.lines()
            .find_map(|l| {
                l.strip_prefix(&format!("{key}="))
                    .map(|s| s.trim_matches('"'))
            })
            .unwrap()
    };
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap();
    let version = kernel
        .trim()
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
    assert_eq!(
        report.os,
        format!("{} {} · {version}", get("NAME"), get("VERSION_ID"))
    );
}

#[cfg(all(feature = "synthetic-adapter", feature = "fault-injection"))]
#[path = "support/process_guard.rs"]
mod process_guard;

#[cfg(all(feature = "synthetic-adapter", feature = "fault-injection"))]
#[test]
fn real_cli_descendant_cpu_is_counted() {
    use std::io::BufRead;
    let root = tempfile::tempdir().unwrap();
    let guard = process_guard::ProcessGuard::new(root.path());
    let mut cli = std::process::Command::new(env!("CARGO_BIN_EXE_canterlot-worker"))
        .args(["synthetic-cli", "--profile", "__burn_parent"])
        .current_dir(root.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(cli.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let child = serde_json::from_str::<serde_json::Value>(&line).unwrap()["child_pid"]
        .as_u64()
        .unwrap() as u32;
    let members = guard.members();
    let ticks = members.iter().find(|(pid, _)| *pid == cli.id()).unwrap().1;
    let mut collector = MetricsCollector::new(root.path().into(), vec![], "/proc".into());
    let probe = ExecutionProbe {
        execution_id: Uuid::now_v7(),
        members: vec![(cli.id(), ticks)],
    };
    collector.sample(std::slice::from_ref(&probe)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(150));
    let row = collector.sample(&[probe]).unwrap().executions.remove(0);
    assert!(row.alive);
    assert!(row.cpu_permille > 0);
    assert!(row.rss_bytes > 0);
    assert!(guard.members().iter().any(|(pid, _)| *pid == child));
    guard.cleanup();
    cli.wait().unwrap();
}
#[test]
fn report_carries_a_private_worktree_root_under_the_data_dir() {
    let dir = fixture();
    let config = config(dir.path());
    let root = dir.path().join("worktrees");
    assert!(!root.exists());
    let report = build_report(&config, &catalog());
    assert_eq!(report.worktree_root, root.to_str().unwrap());
    assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o700);
    // Повторный отчёт не меняет существующий каталог.
    assert_eq!(
        build_report(&config, &catalog()).worktree_root,
        report.worktree_root
    );
}
#[test]
fn report_leaves_the_worktree_root_empty_when_it_cannot_be_created() {
    let dir = fixture();
    let config = config(dir.path());
    fs::write(dir.path().join("worktrees"), "file").unwrap();
    assert_eq!(build_report(&config, &catalog()).worktree_root, "");
}
