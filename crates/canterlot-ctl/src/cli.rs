use std::{fmt, net::IpAddr, path::PathBuf, str::FromStr};

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::error::CtlError;

/// Версия релиза: ровно три десятичные числа, без префикса и приставок.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct Version(String);

impl Version {
    pub fn parse(text: &str) -> Result<Self, CtlError> {
        // Свой разбор вместо semver: выпуск не имеет приставок и собирается только из трёх чисел.
        let mut parts = text.split('.');
        let numbers = [
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
        ];
        let valid = parts.next().is_none()
            && numbers
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        if !valid {
            return Err(CtlError::BadVersion(text.into()));
        }
        Ok(Self(text.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Числовой порядок версий: строковый ставил бы 1.10.0 раньше 1.9.0.
    pub fn key(&self) -> (u64, u64, u64) {
        let mut parts = self
            .0
            .split('.')
            .map(|p| p.parse::<u64>().unwrap_or(u64::MAX));
        (
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
        )
    }
}

impl FromStr for Version {
    type Err = CtlError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
pub enum TlsMode {
    Acme,
    External,
    SelfSigned,
}

#[derive(Debug, Parser)]
#[command(name = "canterlot", disable_version_flag = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    pub fn command(&self) -> Command {
        self.command.clone().unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum Command {
    Install(InstallArgs),
    Join(JoinArgs),
    Adopt,
    Update(UpdateArgs),
    Uninstall(UninstallArgs),
    Status(StatusArgs),
    #[command(subcommand)]
    Tls(TlsCommand),
    HostAgent,
}

impl Default for Command {
    fn default() -> Self {
        Self::Install(InstallArgs::default())
    }
}

#[derive(Debug, Args, Clone, PartialEq, Default)]
pub struct InstallArgs {
    #[arg(long)]
    pub hub_only: bool,
    #[arg(long)]
    pub worker_user: Option<String>,
    #[arg(long, value_enum)]
    pub tls: Option<TlsMode>,
    #[arg(long)]
    pub domain: Option<String>,
    #[arg(long)]
    pub origin: Option<String>,
    #[arg(long)]
    pub front_ip: Option<IpAddr>,
    #[arg(long)]
    pub grpc_endpoint: Option<String>,
    #[arg(long)]
    pub owner_login: Option<String>,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub version: Option<Version>,
    /// Каталог уже скачанного выпуска: сеть не используется, версия берётся из каталога.
    #[arg(long, conflicts_with = "version")]
    pub from_dir: Option<PathBuf>,
}

#[derive(Debug, Args, Clone, PartialEq)]
pub struct JoinArgs {
    #[arg(long)]
    pub hub: String,
    #[arg(long)]
    pub pin: String,
    #[arg(long)]
    pub code: Option<String>,
    #[arg(long)]
    pub replace: bool,
    #[arg(long)]
    pub version: Option<Version>,
    /// Каталог уже скачанного выпуска: сеть не используется, версия берётся из каталога.
    #[arg(long, conflicts_with = "version")]
    pub from_dir: Option<PathBuf>,
}

#[derive(Debug, Args, Clone, PartialEq, Default)]
pub struct UpdateArgs {
    #[arg(long)]
    pub version: Option<Version>,
    #[arg(long)]
    pub from_dir: Option<PathBuf>,
    #[arg(long)]
    pub force: bool,
    /// Скрытый флаг цепочки на хосте хаба: воркер ждёт, пока увидит новую версию хаба.
    #[arg(long, hide = true)]
    pub hub_chain: bool,
}

#[derive(Debug, Args, Clone, PartialEq, Default)]
pub struct UninstallArgs {
    #[arg(long)]
    pub purge: bool,
    #[arg(long)]
    pub force: bool,
    /// Подтверждение уже дал вызвавший uninstall хоста: воркеру спрашивать не нужно.
    #[arg(long, hide = true)]
    pub purge_confirmed: bool,
}

#[derive(Debug, Args, Clone, PartialEq, Default)]
pub struct StatusArgs {
    /// Отчёт одним JSON-объектом вместо текста.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand, Clone, PartialEq)]
pub enum TlsCommand {
    #[command(name = "add-name")]
    AddName { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_matches_spec() {
        let cli = Cli::try_parse_from([
            "canterlot",
            "join",
            "--hub",
            "hub.example.internal:7443",
            "--pin",
            "sha256:ab",
            "--code",
            "-",
        ])
        .unwrap();
        assert_eq!(
            cli.command(),
            Command::Join(JoinArgs {
                hub: "hub.example.internal:7443".into(),
                pin: "sha256:ab".into(),
                code: Some("-".into()),
                replace: false,
                version: None,
                from_dir: None,
            })
        );

        assert!(Cli::try_parse_from(["canterlot", "status", "--unknown"]).is_err());

        let install = Cli::try_parse_from([
            "canterlot",
            "install",
            "--worker-user",
            "workstation",
            "--tls",
            "self-signed",
        ])
        .unwrap();
        match install.command() {
            Command::Install(args) => {
                assert_eq!(args.worker_user.as_deref(), Some("workstation"));
                assert_eq!(args.tls, Some(TlsMode::SelfSigned));
            }
            other => panic!("expected install, got {other:?}"),
        }

        let update =
            Cli::try_parse_from(["canterlot", "update", "--from-dir", "/srv/releases"]).unwrap();
        match update.command() {
            Command::Update(args) => {
                assert_eq!(args.from_dir, Some(PathBuf::from("/srv/releases")));
            }
            other => panic!("expected update, got {other:?}"),
        }

        assert_eq!(
            Cli::try_parse_from(["canterlot"]).unwrap().command(),
            Command::Install(InstallArgs::default())
        );
        assert_eq!(
            Cli::try_parse_from(["canterlot", "tls", "add-name", "hub.example.internal"])
                .unwrap()
                .command(),
            Command::Tls(TlsCommand::AddName {
                name: "hub.example.internal".into()
            })
        );
    }

    #[test]
    fn install_and_join_accept_from_dir_and_conflict_with_version() {
        let install =
            Cli::try_parse_from(["canterlot", "install", "--from-dir", "/srv/releases"]).unwrap();
        match install.command() {
            Command::Install(args) => {
                assert_eq!(args.from_dir, Some(PathBuf::from("/srv/releases")));
            }
            other => panic!("expected install, got {other:?}"),
        }
        assert!(
            Cli::try_parse_from([
                "canterlot",
                "install",
                "--from-dir",
                "/srv/releases",
                "--version",
                "1.2.3",
            ])
            .is_err()
        );

        let join = Cli::try_parse_from([
            "canterlot",
            "join",
            "--hub",
            "hub.example.internal:7443",
            "--pin",
            "sha256:ab",
            "--from-dir",
            "/srv/releases",
        ])
        .unwrap();
        match join.command() {
            Command::Join(args) => {
                assert_eq!(args.from_dir, Some(PathBuf::from("/srv/releases")));
            }
            other => panic!("expected join, got {other:?}"),
        }
        assert!(
            Cli::try_parse_from([
                "canterlot",
                "join",
                "--hub",
                "hub.example.internal:7443",
                "--pin",
                "sha256:ab",
                "--from-dir",
                "/srv/releases",
                "--version",
                "1.2.3",
            ])
            .is_err()
        );
    }

    #[test]
    fn version_rejects_prerelease_and_v_prefix() {
        assert!(Version::parse("1.2.3-alpha.1").is_err());
        assert!(Version::parse("v1.2.3").is_err());
        assert!(Version::parse("1.2").is_err());
        assert!(Version::parse("1.2.3.4").is_err());
        assert!(Version::parse("1.2.x").is_err());
        assert!(Version::parse("1.2.3").is_ok());
        assert!(Cli::try_parse_from(["canterlot", "update", "--version", "1.2.3-rc1"]).is_err());
    }
}
