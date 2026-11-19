use crate::{
    adapter::HarnessCatalog,
    config::WorkerConfig,
    machine::{
        cli_versions::HarnessVersion,
        slash_catalog::{CatalogEntry, EntryKind},
    },
};
use canterlot_proto::{
    limits::{MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS_PER_INPUT},
    worker::v1::{CatalogEntry as ProtoCatalogEntry, HarnessInfo, MachineReport, ModelInfo},
};
use std::{
    collections::BTreeMap,
    fs::DirBuilder,
    os::unix::fs::DirBuilderExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
pub mod claude_history;
pub mod cli_versions;
pub mod codexbar;
pub mod hotspots;
pub mod leaks;
pub mod limits;
pub mod metrics;
mod nofollow;
pub mod owner_instructions;
pub mod precheck;
pub mod probe;
pub mod share;
pub mod slash_catalog;
pub mod watch;
pub mod workdirs;

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Новые поля версий уходят только хабу, объявившему `cli_versions_v1`.
/// Старый хаб получает прежнюю форму отчёта: пустые строки в добавленных полях.
pub fn gated_report(mut report: MachineReport, capabilities: &[String]) -> MachineReport {
    let versions = capabilities
        .iter()
        .any(|capability| capability == crate::session::CAPABILITY_CLI_VERSIONS_V1);
    let catalog = capabilities
        .iter()
        .any(|capability| capability == crate::session::CAPABILITY_SLASH_CATALOG_V1);
    if versions && catalog {
        return report;
    }
    for harness in &mut report.harnesses {
        if !versions {
            harness.installed_version.clear();
            harness.latest_version.clear();
            harness.min_version.clear();
            harness.update_command.clear();
        }
        if !catalog {
            harness.catalog.clear();
        }
    }
    report
}

pub fn build_report(config: &WorkerConfig, catalog: &[HarnessCatalog]) -> MachineReport {
    build_report_with_versions(config, catalog, &BTreeMap::new())
}

/// Отчёт с версиями CLI, собранными пробами старта. Без записи о харнессе поля пусты.
pub fn build_report_with_versions(
    config: &WorkerConfig,
    catalog: &[HarnessCatalog],
    versions: &BTreeMap<String, HarnessVersion>,
) -> MachineReport {
    build_report_with_extras(config, catalog, versions, &BTreeMap::new())
}

fn proto_catalog(entries: &[CatalogEntry]) -> Vec<ProtoCatalogEntry> {
    entries
        .iter()
        .map(|entry| ProtoCatalogEntry {
            name: entry.name.clone(),
            description: entry.description.clone(),
            kind: match entry.kind {
                EntryKind::Command => "command",
                EntryKind::Skill => "skill",
            }
            .into(),
            argument_hint: entry.argument_hint.clone(),
        })
        .collect()
}

/// Отчёт с каталогом slash-команд, собранным фоновыми пробами.
pub fn build_report_with_extras(
    config: &WorkerConfig,
    catalog: &[HarnessCatalog],
    versions: &BTreeMap<String, HarnessVersion>,
    catalogs: &BTreeMap<String, Vec<CatalogEntry>>,
) -> MachineReport {
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
            .map(|c| {
                let version = versions.get(c.harness);
                HarnessInfo {
                    harness: c.harness.into(),
                    provider: c.provider.into(),
                    title: c.title.into(),
                    images: c.images,
                    settings_midsession: c.settings_midsession,
                    settings_midsession_effort: Some(c.settings_midsession_effort),
                    goals: c.goals,
                    installed_version: version.map_or_else(String::new, |v| v.installed.clone()),
                    latest_version: version.map_or_else(String::new, |v| v.latest.clone()),
                    min_version: version.map_or_else(String::new, |v| v.minimum.clone()),
                    update_command: version.map_or_else(String::new, |v| v.update_command.clone()),
                    catalog: catalogs
                        .get(c.harness)
                        .map(|entries| proto_catalog(entries))
                        .unwrap_or_default(),
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
                }
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
        memory_work_roots: std::env::home_dir()
            .filter(|home| home.is_absolute())
            .map(|home| crate::memory_scope::canonical_roots(&config.memory.work_roots, &home))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|root| root.to_str().map(str::to_owned))
            .collect(),
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

#[cfg(test)]
mod cli_versions_gate_tests {
    use super::*;
    use crate::machine::slash_catalog::{CatalogEntry, EntryKind};

    fn catalog() -> Vec<ProtoCatalogEntry> {
        proto_catalog(&[CatalogEntry {
            name: "review".into(),
            description: "Review changes".into(),
            kind: EntryKind::Command,
            argument_hint: String::new(),
        }])
    }

    fn report() -> MachineReport {
        MachineReport {
            harnesses: vec![HarnessInfo {
                harness: "codex".into(),
                installed_version: "0.164.0".into(),
                latest_version: "0.167.0".into(),
                min_version: "0.164.0".into(),
                update_command: "npm i -g @openai/codex@latest".into(),
                catalog: catalog(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn old_peer_gets_no_new_fields() {
        let report = gated_report(report(), &[]);
        let h = &report.harnesses[0];
        assert_eq!(h.installed_version, "");
        assert_eq!(h.latest_version, "");
        assert_eq!(h.min_version, "");
        assert_eq!(h.update_command, "");
        assert!(h.catalog.is_empty());
    }

    #[test]
    fn negotiated_peer_gets_all_new_fields() {
        let report = gated_report(
            report(),
            &["cli_versions_v1".into(), "slash_catalog_v1".into()],
        );
        let h = &report.harnesses[0];
        assert_eq!(h.installed_version, "0.164.0");
        assert_eq!(h.latest_version, "0.167.0");
        assert_eq!(h.min_version, "0.164.0");
        assert_eq!(h.update_command, "npm i -g @openai/codex@latest");
        assert_eq!(h.catalog.len(), 1);
    }

    #[test]
    fn catalog_is_hidden_without_its_own_capability() {
        let report = gated_report(report(), &["cli_versions_v1".into()]);
        assert!(report.harnesses[0].catalog.is_empty());
        assert_eq!(report.harnesses[0].installed_version, "0.164.0");
    }
}
