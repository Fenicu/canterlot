use std::path::{Path, PathBuf};

use crate::cli::Version;

/// Все пути установки строятся от `root` (в работе — `/`, в тестах — временный каталог).
/// Пути воркера строятся от `home`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub root: PathBuf,
    pub home: Option<PathBuf>,
}

impl Layout {
    pub fn new(root: PathBuf, home: Option<PathBuf>) -> Self {
        Self { root, home }
    }

    fn home(&self) -> &Path {
        self.home
            .as_deref()
            .expect("worker paths require a home directory in Layout")
    }

    pub fn hub_release(&self, v: &Version) -> PathBuf {
        self.root
            .join("opt/canterlot/hub/releases")
            .join(v.as_str())
    }

    pub fn hub_current(&self) -> PathBuf {
        self.root.join("opt/canterlot/hub/current")
    }

    pub fn web_parent(&self) -> PathBuf {
        self.root.join("opt/canterlot/web")
    }

    pub fn web_release(&self, v: &Version) -> PathBuf {
        self.web_parent().join("releases").join(v.as_str())
    }

    pub fn install_toml(&self) -> PathBuf {
        self.root.join("etc/canterlot/install.toml")
    }

    pub fn compose_dir(&self) -> PathBuf {
        self.root.join("etc/canterlot/compose")
    }

    pub fn ca_dir(&self) -> PathBuf {
        self.root.join("etc/canterlot/ca")
    }

    pub fn hub_etc(&self) -> PathBuf {
        self.root.join("etc/canterlot/hub")
    }

    pub fn manifest_dir(&self) -> PathBuf {
        self.root.join("etc/canterlot/manifest")
    }

    pub fn pg_data(&self) -> PathBuf {
        self.root.join("var/lib/canterlot/postgres")
    }

    pub fn hub_data(&self) -> PathBuf {
        self.root.join("var/lib/canterlot/hub")
    }

    pub fn backups(&self) -> PathBuf {
        self.root.join("var/lib/canterlot/backups")
    }

    pub fn ops_dir(&self) -> PathBuf {
        self.root.join("var/lib/canterlot/ops")
    }

    pub fn agent_dir(&self) -> PathBuf {
        self.root.join("run/canterlot-host")
    }

    /// Каталог Caddyfile: монтируется в caddy целиком, без остальных файлов compose.
    pub fn caddy_dir(&self) -> PathBuf {
        self.compose_dir().join("caddy")
    }

    pub fn caddy_data(&self) -> PathBuf {
        self.root.join("var/lib/canterlot/caddy")
    }

    /// Временные каталоги загрузки релиза (0700 внутри).
    pub fn staging(&self) -> PathBuf {
        self.root.join("var/lib/canterlot/staging")
    }

    pub fn ctl_release(&self, v: &Version) -> PathBuf {
        self.root.join("opt/canterlot/ctl").join(v.as_str())
    }

    pub fn ctl_link(&self) -> PathBuf {
        self.root.join("usr/local/bin/canterlot")
    }

    pub fn worker_release(&self, v: &Version) -> PathBuf {
        self.home()
            .join(".local/opt/canterlot/releases")
            .join(v.as_str())
    }

    pub fn worker_current(&self) -> PathBuf {
        self.home().join(".local/opt/canterlot/current")
    }

    pub fn worker_config_dir(&self) -> PathBuf {
        self.home().join(".config/canterlot")
    }

    pub fn worker_unit(&self) -> PathBuf {
        self.home()
            .join(".config/systemd/user/canterlot-worker.service")
    }

    pub fn worker_ops_dir(&self) -> PathBuf {
        self.home().join(".local/state/canterlot-ctl/ops")
    }

    /// Всё, что ставит утилита у воркера: релизы, ссылки и копии самой утилиты.
    pub fn worker_opt(&self) -> PathBuf {
        self.home().join(".local/opt/canterlot")
    }

    pub fn worker_releases(&self) -> PathBuf {
        self.worker_opt().join("releases")
    }

    pub fn worker_previous(&self) -> PathBuf {
        self.worker_opt().join("previous")
    }

    pub fn worker_ctl(&self, v: &Version) -> PathBuf {
        self.worker_opt()
            .join("ctl")
            .join(v.as_str())
            .join("canterlot")
    }

    pub fn worker_ctl_link(&self) -> PathBuf {
        self.home().join(".local/bin/canterlot")
    }

    pub fn worker_data_dir(&self) -> PathBuf {
        self.home().join(".local/state/canterlot-worker")
    }

    pub fn worker_staging(&self) -> PathBuf {
        self.home().join(".local/state/canterlot-ctl/staging")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_paths_match_spec_table() {
        // Поправка на корень: все ожидаемые пути начинаются с `/`.
        let layout = Layout::new(PathBuf::from("/"), Some(PathBuf::from("/srv/test-home")));
        let v = Version::parse("1.2.3").unwrap();
        let expected = [
            (layout.hub_release(&v), "/opt/canterlot/hub/releases/1.2.3"),
            (layout.hub_current(), "/opt/canterlot/hub/current"),
            (layout.web_parent(), "/opt/canterlot/web"),
            (layout.web_release(&v), "/opt/canterlot/web/releases/1.2.3"),
            (layout.install_toml(), "/etc/canterlot/install.toml"),
            (layout.compose_dir(), "/etc/canterlot/compose"),
            (layout.ca_dir(), "/etc/canterlot/ca"),
            (layout.hub_etc(), "/etc/canterlot/hub"),
            (layout.manifest_dir(), "/etc/canterlot/manifest"),
            (layout.pg_data(), "/var/lib/canterlot/postgres"),
            (layout.hub_data(), "/var/lib/canterlot/hub"),
            (layout.backups(), "/var/lib/canterlot/backups"),
            (layout.ops_dir(), "/var/lib/canterlot/ops"),
            (layout.agent_dir(), "/run/canterlot-host"),
            (
                layout.worker_release(&v),
                "/srv/test-home/.local/opt/canterlot/releases/1.2.3",
            ),
            (
                layout.worker_current(),
                "/srv/test-home/.local/opt/canterlot/current",
            ),
            (
                layout.worker_config_dir(),
                "/srv/test-home/.config/canterlot",
            ),
            (
                layout.worker_unit(),
                "/srv/test-home/.config/systemd/user/canterlot-worker.service",
            ),
            (
                layout.worker_ops_dir(),
                "/srv/test-home/.local/state/canterlot-ctl/ops",
            ),
        ];
        for (got, want) in expected {
            assert_eq!(got, PathBuf::from(want), "path {want}");
        }
    }

    #[test]
    fn layout_paths_stay_under_root() {
        let root = PathBuf::from("/srv/fake-root");
        let layout = Layout::new(root.clone(), Some(root.join("home")));
        let v = Version::parse("2.0.0").unwrap();
        for path in [
            layout.hub_release(&v),
            layout.hub_current(),
            layout.web_parent(),
            layout.web_release(&v),
            layout.install_toml(),
            layout.compose_dir(),
            layout.ca_dir(),
            layout.hub_etc(),
            layout.manifest_dir(),
            layout.pg_data(),
            layout.hub_data(),
            layout.backups(),
            layout.ops_dir(),
            layout.agent_dir(),
            layout.worker_release(&v),
            layout.worker_current(),
            layout.worker_config_dir(),
            layout.worker_unit(),
            layout.worker_ops_dir(),
        ] {
            assert!(path.starts_with(&root), "outside root: {}", path.display());
        }
    }
}
