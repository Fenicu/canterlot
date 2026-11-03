use canterlot_proto::worker::v1::{DiskUsage, ExecutionProcess, MachineMetrics};
use std::{
    collections::{HashMap, HashSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("metrics I/O failed")]
    Io(#[from] std::io::Error),
    #[error("invalid proc metrics")]
    Invalid,
    #[error("filesystem metrics failed")]
    Fs(#[from] rustix::io::Errno),
    #[error("metrics task failed")]
    Task,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}
pub fn parse_proc_stat(text: &str) -> Result<CpuTimes, MetricsError> {
    let line = text
        .lines()
        .find(|l| l.starts_with("cpu "))
        .ok_or(MetricsError::Invalid)?;
    let values = line
        .split_whitespace()
        .skip(1)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| MetricsError::Invalid)?;
    if values.len() < 4 {
        return Err(MetricsError::Invalid);
    }
    let total = values
        .iter()
        .take(8)
        .try_fold(0u64, |sum, v| sum.checked_add(*v))
        .ok_or(MetricsError::Invalid)?;
    let idle = values[3]
        .checked_add(values.get(4).copied().unwrap_or(0))
        .ok_or(MetricsError::Invalid)?;
    Ok(CpuTimes {
        busy: total.saturating_sub(idle),
        total,
    })
}
pub fn parse_meminfo(text: &str) -> Result<(u64, u64), MetricsError> {
    let field = |key: &str| -> Result<u64, MetricsError> {
        let line = text
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .ok_or(MetricsError::Invalid)?;
        let mut parts = line.split_whitespace();
        let size = parts
            .next()
            .ok_or(MetricsError::Invalid)?
            .parse::<u64>()
            .map_err(|_| MetricsError::Invalid)?;
        if parts.next() != Some("kB") {
            return Err(MetricsError::Invalid);
        }
        size.checked_mul(1024).ok_or(MetricsError::Invalid)
    };
    let total = field("MemTotal:")?;
    let available = field("MemAvailable:")?;
    if available > total {
        return Err(MetricsError::Invalid);
    }
    Ok((total, available))
}
pub fn parse_loadavg(text: &str) -> Result<(u32, u32, u32), MetricsError> {
    let mut parts = text.split_whitespace();
    let mut next = || {
        let value = parts
            .next()
            .ok_or(MetricsError::Invalid)?
            .parse::<f64>()
            .map_err(|_| MetricsError::Invalid)?;
        if !value.is_finite() || value < 0.0 || value * 100.0 > u32::MAX as f64 {
            return Err(MetricsError::Invalid);
        }
        Ok((value * 100.0).round() as u32)
    };
    Ok((next()?, next()?, next()?))
}
/// Участники исполнения: супервизор, затем CLI; потомки ищутся под последним.
#[derive(Clone)]
pub struct ExecutionProbe {
    pub execution_id: Uuid,
    pub members: Vec<(u32, u64)>,
}
#[async_trait::async_trait]
pub trait MetricsSource: Send + Sync {
    async fn sample(&self, executions: Vec<ExecutionProbe>)
    -> Result<MachineMetrics, MetricsError>;
}
pub struct ProcMetricsSource(Arc<Mutex<MetricsCollector>>);
impl ProcMetricsSource {
    pub fn new(collector: MetricsCollector) -> Self {
        Self(Arc::new(Mutex::new(collector)))
    }
}
#[async_trait::async_trait]
impl MetricsSource for ProcMetricsSource {
    async fn sample(
        &self,
        executions: Vec<ExecutionProbe>,
    ) -> Result<MachineMetrics, MetricsError> {
        let collector = self.0.clone();
        tokio::task::spawn_blocking(move || {
            collector.lock().expect("metrics lock").sample(&executions)
        })
        .await
        .map_err(|_| MetricsError::Task)?
    }
}
struct Process {
    ppid: u32,
    start: u64,
    cpu: u64,
    rss: u64,
}
pub struct MetricsCollector {
    data_dir: PathBuf,
    roots: Vec<PathBuf>,
    proc_root: PathBuf,
    cpu: Option<CpuTimes>,
    previous: HashMap<(u32, u64), u64>,
    at: Option<Instant>,
}
impl MetricsCollector {
    pub fn new(data_dir: PathBuf, roots: Vec<PathBuf>, proc_root: PathBuf) -> Self {
        Self {
            data_dir,
            roots,
            proc_root,
            cpu: None,
            previous: HashMap::new(),
            at: None,
        }
    }
    pub fn sample(
        &mut self,
        executions: &[ExecutionProbe],
    ) -> Result<MachineMetrics, MetricsError> {
        let stat = fs::read_to_string(self.proc_root.join("stat"))?;
        let current = parse_proc_stat(&stat)?;
        let cpu_permille = self.cpu.map_or(0, |p| {
            let total = current.total.saturating_sub(p.total);
            if total == 0 {
                0
            } else {
                current
                    .busy
                    .saturating_sub(p.busy)
                    .saturating_mul(1000)
                    .checked_div(total)
                    .unwrap_or(0)
                    .min(1000) as u32
            }
        });
        let (mem_total_bytes, mem_available_bytes) =
            parse_meminfo(&fs::read_to_string(self.proc_root.join("meminfo"))?)?;
        let (load1_centi, load5_centi, load15_centi) =
            parse_loadavg(&fs::read_to_string(self.proc_root.join("loadavg"))?)?;
        let disks = self.disks()?;
        let processes = self.processes()?;
        let now = Instant::now();
        let elapsed = self.at.map(|at| now.duration_since(at).as_secs_f64());
        let ticks = rustix::param::clock_ticks_per_second() as f64;
        let mut rows = Vec::new();
        for execution in executions {
            let mut included = HashSet::new();
            for &(pid, start) in &execution.members {
                if !processes.get(&pid).is_some_and(|p| p.start == start) {
                    continue;
                }
                included.insert(pid);
                if execution.members.last() != Some(&(pid, start)) {
                    continue;
                }
                for (&child, process) in &processes {
                    if process.start < start {
                        continue;
                    }
                    let mut parent = process.ppid;
                    let mut visited = HashSet::new();
                    while parent != 0 && visited.insert(parent) {
                        if parent == pid {
                            included.insert(child);
                            break;
                        }
                        let Some(p) = processes.get(&parent) else {
                            break;
                        };
                        if p.start < start || p.start > process.start {
                            break;
                        }
                        parent = p.ppid;
                    }
                }
            }
            let members_alive = included
                .iter()
                .filter(|pid| execution.members.iter().any(|(member, _)| member == *pid))
                .count();
            let descendants = included.len().saturating_sub(members_alive);
            let mut delta = 0u64;
            let mut rss = 0u64;
            for pid in &included {
                let p = &processes[pid];
                delta = delta.saturating_add(
                    p.cpu.saturating_sub(
                        self.previous
                            .get(&(*pid, p.start))
                            .copied()
                            .unwrap_or(if elapsed.is_some() { 0 } else { p.cpu }),
                    ),
                );
                rss = rss.saturating_add(p.rss);
            }
            let cpu = elapsed.filter(|s| *s > 0.0).map_or(0, |s| {
                ((delta as f64 / ticks / s * 1000.0).round()).min(u32::MAX as f64) as u32
            });
            rows.push(ExecutionProcess {
                execution_id: execution.execution_id.to_string(),
                alive: !included.is_empty(),
                cpu_permille: cpu,
                rss_bytes: rss,
                descendants: Some(descendants.min(u32::MAX as usize) as u32),
            });
        }
        self.cpu = Some(current);
        self.at = Some(now);
        self.previous = processes
            .into_iter()
            .map(|(pid, p)| ((pid, p.start), p.cpu))
            .collect();
        Ok(MachineMetrics {
            machine_time_ms: super::now_ms(),
            cpu_cores: stat
                .lines()
                .filter(|l| {
                    l.strip_prefix("cpu")
                        .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_digit()))
                })
                .count()
                .max(1) as u32,
            cpu_permille,
            mem_total_bytes,
            mem_available_bytes,
            disks,
            load1_centi,
            load5_centi,
            load15_centi,
            executions: rows,
        })
    }
    fn disks(&self) -> Result<Vec<DiskUsage>, MetricsError> {
        let mut devices = HashSet::new();
        let mut disks = Vec::new();
        for (path, label) in std::iter::once((&self.data_dir, "data".to_owned())).chain(
            self.roots
                .iter()
                .enumerate()
                .map(|(i, r)| (r, format!("root_{i}"))),
        ) {
            let metadata = fs::metadata(path)?;
            if !devices.insert(metadata.dev()) {
                continue;
            }
            let stat = rustix::fs::statvfs(path)?;
            disks.push(DiskUsage {
                label,
                total_bytes: stat.f_blocks.saturating_mul(stat.f_frsize),
                free_bytes: stat.f_bavail.saturating_mul(stat.f_frsize),
            });
        }
        Ok(disks)
    }
    fn processes(&self) -> Result<HashMap<u32, Process>, MetricsError> {
        let mut result = HashMap::new();
        for entry in fs::read_dir(&self.proc_root)?.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
                continue;
            };
            if let Some(process) = read_process(&entry.path()) {
                result.insert(pid, process);
            }
        }
        Ok(result)
    }
}
fn read_process(path: &Path) -> Option<Process> {
    let text = crate::containment::read_stat(path.join("stat")).ok()?;
    let fields: Vec<_> = text[text.rfind(')')? + 1..].split_whitespace().collect();
    if matches!(fields.first(), Some(&"Z") | Some(&"X")) {
        return None;
    }
    let ppid = fields.get(1)?.parse().ok()?;
    let start = fields.get(19)?.parse().ok()?;
    let cpu = fields
        .get(11)?
        .parse::<u64>()
        .ok()?
        .saturating_add(fields.get(12)?.parse().ok()?);
    let rss = fs::read_to_string(path.join("statm"))
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_mul(rustix::param::page_size() as u64);
    Some(Process {
        ppid,
        start,
        cpu,
        rss,
    })
}
