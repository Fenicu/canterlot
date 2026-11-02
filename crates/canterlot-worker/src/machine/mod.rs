use crate::{adapter::HarnessCatalog, config::WorkerConfig};
use canterlot_proto::{
    limits::{MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS_PER_INPUT},
    worker::v1::{HarnessInfo, MachineReport, ModelInfo},
};
use std::{
    fs::DirBuilder,
    os::unix::fs::DirBuilderExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
pub mod codexbar;
pub mod limits;
pub mod metrics;
pub mod owner_instructions;
pub mod share;
pub mod workdirs;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub fn build_report(config: &WorkerConfig, catalog: &[HarnessCatalog]) -> MachineReport {
    let mut roots: Vec<_> = config
        .execution
        .allowed_workdir_roots
        .iter()
        .filter_map(|r| r.canonicalize().ok())
        .map(|r| r.to_string_lossy().into_owned())
        .collect();
    roots.sort();
    roots.dedup();
    MachineReport {
        worker_version: env!("CARGO_PKG_VERSION").into(),
        allowed_roots: roots,
        harnesses: catalog
            .iter()
            .map(|c| HarnessInfo {
                harness: c.harness.into(),
                provider: c.provider.into(),
                title: c.title.into(),
                images: c.images,
                settings_midsession: c.settings_midsession,
                settings_midsession_effort: Some(c.settings_midsession_effort),
                permission_modes: c.permission_modes.iter().map(|s| (*s).into()).collect(),
                models: c
                    .models
                    .iter()
                    .map(|m| ModelInfo {
                        id: m.id.clone(),
                        title: m.title.clone(),
                        efforts: m.efforts.to_vec(),
                        images: m.images,
                        context_window: m.context_window,
                    })
                    .collect(),
            })
            .collect(),
        max_attachment_bytes: MAX_ATTACHMENT_BYTES,
        max_attachments_per_input: MAX_ATTACHMENTS_PER_INPUT as u32,
        containment: "tree".into(),
        os: os_line(),
        home_dir: std::env::home_dir()
            .filter(|home| home.is_absolute())
            .and_then(|home| home.to_str().map(str::to_owned))
            .unwrap_or_default(),
        worktree_root: worktree_root(&config.data_dir),
    }
}

/// Корень изолированных рабочих копий: личный каталог воркера; пусто, если создать не удалось.
pub fn worktree_root(data_dir: &Path) -> String {
    let root = data_dir.join("worktrees");
    let created = DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .and_then(|()| {
            if root.is_dir() {
                Ok(())
            } else {
                Err(std::io::ErrorKind::NotADirectory.into())
            }
        });
    match (created, root.to_str()) {
        (Ok(()), Some(text)) => text.to_owned(),
        _ => String::new(),
    }
}
fn os_line() -> String {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let field = |name: &str| {
        release
            .lines()
            .find_map(|line| {
                line.strip_prefix(&format!("{name}="))
                    .map(|v| v.trim_matches(['\'', '"']))
            })
            .unwrap_or("unknown")
    };
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    format!(
        "{} {} · {}",
        field("NAME"),
        field("VERSION_ID"),
        kernel
            .trim()
            .split('.')
            .take(2)
            .collect::<Vec<_>>()
            .join(".")
    )
}
