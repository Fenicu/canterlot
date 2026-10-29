#![forbid(unsafe_code)]

use canterlot_worker::{
    adapter::catalog,
    config::WorkerConfig,
    machine::{
        build_report,
        metrics::{
            ExecutionProbe, MetricsCollector, parse_loadavg, parse_meminfo, parse_proc_stat,
        },
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
fn config(dir: &Path) -> WorkerConfig {
    toml::from_str(&format!("hub_url='https://localhost'\nca_cert='ca'\ntoken_file='token'\nmachine_id='{}'\ninstallation_id='{}'\nrestore_generation='{}'\ndata_dir='{}'\n[execution]\nallowed_workdir_roots=['{}']",Uuid::now_v7(),Uuid::now_v7(),Uuid::now_v7(),dir.display(),dir.join(".").display())).unwrap()
}
#[test]
fn report_carries_an_empty_profile_list_and_canonical_roots() {
    // Профили инструкций воркера убраны планом 2c: хаб хранит пресеты, поле `profiles`
    // отправляется пустым до резервирования в задаче 3.
    let dir = fixture();
    let report = build_report(&config(dir.path()), &catalog());
    assert!(report.profiles.is_empty());
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
