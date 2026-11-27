use std::{net::IpAddr, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    cli::TlsMode,
    error::CtlError,
    layout::Layout,
    ops::{self, OpId},
};

pub const FORMAT: u32 = 1;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Components {
    #[serde(default)]
    pub postgres: bool,
    #[serde(default)]
    pub hub: bool,
    #[serde(default)]
    pub web: bool,
    #[serde(default)]
    pub caddy: bool,
    #[serde(default)]
    pub worker: bool,
    #[serde(default)]
    pub host_agent: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallState {
    pub format: u32,
    #[serde(default)]
    pub components: Components,
    #[serde(default)]
    pub tls: Option<TlsMode>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub grpc_endpoint: Option<String>,
    #[serde(default)]
    pub front_ip: Option<IpAddr>,
    pub hub_uid: u32,
    pub hub_gid: u32,
    #[serde(default)]
    pub created_paths: Vec<PathBuf>,
    #[serde(default)]
    pub adopted_paths: Vec<PathBuf>,
}

impl InstallState {
    pub fn load(layout: &Layout) -> Result<Option<Self>, CtlError> {
        let path = layout.install_toml();
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(toml::from_str(&text)?)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub fn save(&self, layout: &Layout) -> Result<(), CtlError> {
        let text = toml::to_string_pretty(self)?;
        ops::write_atomic(&layout.install_toml(), text.as_bytes())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostClass {
    Fresh,
    Partial(OpId),
    Retained,
    NativeInstall,
}

pub fn classify_hub(layout: &Layout) -> Result<HostClass, CtlError> {
    // Прерванная операция важнее остальных признаков: её надо продолжить или объявить неоднозначной.
    if let Some(rec) = ops::unfinished(&layout.ops_dir())? {
        return Ok(HostClass::Partial(rec.id));
    }
    let native = layout.root.join("etc/canterlot/hub.toml").is_file()
        && layout
            .root
            .join("opt/canterlot/current/bin/canterlot-hub")
            .is_file();
    let managed = layout.install_toml().is_file();
    if native && !managed {
        return Ok(HostClass::NativeInstall);
    }
    if managed
        || layout.pg_data().exists()
        || layout.hub_data().exists()
        || layout.manifest_dir().exists()
    {
        return Ok(HostClass::Retained);
    }
    Ok(HostClass::Fresh)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkerClass {
    Fresh,
    Installed,
    Partial(OpId),
}

pub fn classify_worker(layout: &Layout) -> Result<WorkerClass, CtlError> {
    if let Some(rec) = ops::unfinished(&layout.worker_ops_dir())? {
        return Ok(WorkerClass::Partial(rec.id));
    }
    let config = layout.worker_config_dir();
    if config.join("worker.toml").is_file()
        && config.join("token").is_file()
        && layout.worker_unit().is_file()
    {
        return Ok(WorkerClass::Installed);
    }
    Ok(WorkerClass::Fresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cli::Version,
        ops::{self, OpKind},
    };

    fn layout(root: &std::path::Path) -> Layout {
        Layout::new(root.to_path_buf(), Some(root.join("home")))
    }

    #[test]
    fn classify_fresh() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(classify_hub(&layout(dir.path())).unwrap(), HostClass::Fresh);
        assert_eq!(
            classify_worker(&layout(dir.path())).unwrap(),
            WorkerClass::Fresh
        );
    }

    #[test]
    fn classify_retained_after_uninstall() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(dir.path());
        fs_create(&layout.pg_data());
        fs_create(&layout.manifest_dir());
        assert_eq!(classify_hub(&layout).unwrap(), HostClass::Retained);
    }

    #[test]
    fn classify_partial_from_op_record() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(dir.path());
        let rec = ops::begin(
            &layout.ops_dir(),
            ops::OpRecord::new(OpKind::Install, Some(Version::parse("1.2.3").unwrap())),
        )
        .unwrap();
        assert_eq!(classify_hub(&layout).unwrap(), HostClass::Partial(rec.id));
    }

    #[test]
    fn classify_native_install() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(dir.path());
        fs_create(layout.root.join("etc/canterlot/hub.toml").parent().unwrap());
        std::fs::write(layout.root.join("etc/canterlot/hub.toml"), b"").unwrap();
        fs_create(&layout.root.join("opt/canterlot/current/bin"));
        std::fs::write(
            layout.root.join("opt/canterlot/current/bin/canterlot-hub"),
            b"",
        )
        .unwrap();
        assert_eq!(classify_hub(&layout).unwrap(), HostClass::NativeInstall);
    }

    #[test]
    fn classify_worker_installed() {
        let dir = tempfile::tempdir().unwrap();
        let layout = layout(dir.path());
        fs_create(&layout.worker_config_dir());
        std::fs::write(layout.worker_config_dir().join("worker.toml"), b"").unwrap();
        std::fs::write(layout.worker_config_dir().join("token"), b"").unwrap();
        fs_create(layout.worker_unit().parent().unwrap());
        std::fs::write(layout.worker_unit(), b"").unwrap();
        assert_eq!(classify_worker(&layout).unwrap(), WorkerClass::Installed);
    }

    fn fs_create(path: &std::path::Path) {
        std::fs::create_dir_all(path).unwrap();
    }
}
