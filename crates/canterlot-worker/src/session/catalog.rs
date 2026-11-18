//! Пробы каталога харнессов: стартовая и повторные после сбоя.
//!
//! Сбой пробы на старте не должен убирать харнесс до перезапуска воркера: загрузка
//! машины проходит, и повтор возвращает каталог в живой поток управления.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use canterlot_proto::worker::v1::MachineReport;
use tokio::sync::watch;

use crate::{
    adapter::{
        HarnessCatalog,
        claude_code::version::{self as claude_version, CliVersion},
        cli_version,
        codex::{self, CodexCatalog},
        opencode::{self, OpenCodeCatalog},
    },
    config::{ClaudeCodeConfig, CodexConfig, OpenCodeConfig, WorkerConfig},
    machine::{
        cli_versions::{self, HarnessVersion, LatestVersions, NPM_REGISTRY},
        slash_catalog::{self, CatalogCache, CatalogEntry, Source},
    },
};

/// Первые паузы повторов, дальше раз в пять минут: сбой обычно кончается сам.
const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(300),
    Duration::from_secs(300),
];
/// Пока проба не проходит, напоминаем о причине не чаще этого: иначе повторы её топят.
const FAILURE_WARN_EVERY: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Default)]
pub(super) struct CatalogState {
    claude_version: Option<CliVersion>,
    codex: Option<CodexCatalog>,
    opencode: Option<OpenCodeCatalog>,
    latest: BTreeMap<&'static str, String>,
    catalogs: BTreeMap<String, Vec<CatalogEntry>>,
}

impl CatalogState {
    pub(super) fn set(&mut self, slot: Slot) {
        match slot {
            Slot::Claude { version, latest } => {
                self.latest.insert("claude_code", latest);
                self.claude_version = Some(version);
            }
            Slot::Codex { catalog, latest } => {
                self.latest.insert("codex", latest);
                self.codex = Some(catalog);
            }
            Slot::OpenCode { catalog, latest } => {
                self.latest.insert("opencode", latest);
                self.opencode = Some(catalog);
            }
        }
    }

    fn harnesses(&self, config: &WorkerConfig) -> Vec<HarnessCatalog> {
        crate::adapter::catalog_with(
            config.claude_code.as_ref(),
            self.claude_version.as_ref(),
            self.codex.as_ref(),
            self.opencode.as_ref(),
        )
    }

    /// Версии одним проходом по успешным пробам: минимум из кода, способ установки
    /// из разрешённого пути бинарника, последний выпуск из кэша реестра npm.
    fn versions(&self, config: &WorkerConfig) -> BTreeMap<String, HarnessVersion> {
        let mut versions = BTreeMap::new();
        let mut add = |harness: &'static str, executable: &Path, label: Option<&str>| {
            let Some(label) = label else { return };
            let resolved =
                cli_version::resolve(executable).unwrap_or_else(|| executable.to_owned());
            versions.insert(
                harness.to_owned(),
                HarnessVersion {
                    installed: label.to_owned(),
                    latest: self.latest.get(harness).cloned().unwrap_or_default(),
                    minimum: cli_versions::minimum(harness),
                    update_command: cli_versions::update_command(
                        harness,
                        cli_versions::install_method(&resolved),
                    ),
                },
            );
        };
        if let Some(cfg) = &config.claude_code {
            add(
                "claude_code",
                &cfg.executable,
                self.claude_version.as_ref().map(CliVersion::label),
            );
        }
        if let Some(cfg) = &config.codex {
            add(
                "codex",
                &cfg.executable,
                self.codex.as_ref().map(|c| c.version.label()),
            );
        }
        if let Some(cfg) = &config.opencode {
            add(
                "opencode",
                &cfg.executable,
                self.opencode.as_ref().map(|c| c.version.label()),
            );
        }
        versions
    }
}

/// Каталог одного харнесса из успешной пробы.
pub(super) enum Slot {
    Claude {
        version: CliVersion,
        latest: String,
    },
    Codex {
        catalog: CodexCatalog,
        latest: String,
    },
    OpenCode {
        catalog: OpenCodeCatalog,
        latest: String,
    },
}

impl Slot {
    pub(super) fn version_label(&self) -> &str {
        match self {
            Self::Claude { version, .. } => version.label(),
            Self::Codex { catalog, .. } => catalog.version.label(),
            Self::OpenCode { catalog, .. } => catalog.version.label(),
        }
    }
}

/// Настроенные харнессы. Выключенный в конфигурации харнесс сюда не попадает:
/// его не пробуют и не повторяют.
#[derive(Clone)]
pub(super) enum Probe {
    Claude(ClaudeCodeConfig),
    Codex(CodexConfig),
    OpenCode(OpenCodeConfig),
}

impl Probe {
    pub(super) fn configured(config: &WorkerConfig) -> Vec<Self> {
        let mut probes = Vec::new();
        if let Some(cfg) = &config.claude_code {
            probes.push(Self::Claude(cfg.clone()));
        }
        if let Some(cfg) = &config.codex {
            probes.push(Self::Codex(cfg.clone()));
        }
        if let Some(cfg) = &config.opencode {
            probes.push(Self::OpenCode(cfg.clone()));
        }
        probes
    }

    pub(super) async fn discover(
        &self,
        data_dir: &Path,
        npm: &LatestVersions,
    ) -> Result<Slot, String> {
        let cache = data_dir.join(".cli-versions.json");
        let npm_cache = data_dir.join(".npm-latest.json");
        let now = crate::machine::now_ms();
        match self {
            Self::Claude(cfg) => match claude_version::check_with_cache(
                &cfg.executable,
                crate::adapter::cli_version::DEFAULT_PROBE_DEADLINE,
                Some(&cache),
            )
            .await
            {
                Ok(version) => Ok(Slot::Claude {
                    latest: npm
                        .fetch(
                            cli_versions::npm_package("claude_code"),
                            NPM_REGISTRY,
                            &npm_cache,
                            now,
                        )
                        .await,
                    version,
                }),
                Err(reason) => Err(format!("{reason:?}")),
            },
            Self::Codex(cfg) => match codex::discover_with_cache(cfg, Some(&cache)).await {
                Ok(catalog) => Ok(Slot::Codex {
                    latest: npm
                        .fetch(
                            cli_versions::npm_package("codex"),
                            NPM_REGISTRY,
                            &npm_cache,
                            now,
                        )
                        .await,
                    catalog,
                }),
                Err(reason) => Err(format!("{reason:?}")),
            },
            Self::OpenCode(cfg) => {
                match opencode::discover_with_cache(cfg, data_dir, Some(&cache)).await {
                    Ok(catalog) => Ok(Slot::OpenCode {
                        latest: npm
                            .fetch(
                                cli_versions::npm_package("opencode"),
                                NPM_REGISTRY,
                                &npm_cache,
                                now,
                            )
                            .await,
                        catalog,
                    }),
                    Err(reason) => Err(format!("{reason:?}")),
                }
            }
        }
    }

    pub(super) fn failure_kind(&self) -> &'static str {
        match self {
            Self::Claude(_) => "claude_code_unavailable",
            Self::Codex(_) => "codex_unavailable",
            Self::OpenCode(_) => "opencode_unavailable",
        }
    }

    fn discovered_kind(&self) -> &'static str {
        match self {
            Self::Claude(_) => "claude_code_discovered",
            Self::Codex(_) => "codex_discovered",
            Self::OpenCode(_) => "opencode_discovered",
        }
    }

    pub(super) fn omitted(&self) -> &'static str {
        match self {
            Self::Claude(_) => "Claude Code omitted from catalog",
            Self::Codex(_) => "Codex omitted from catalog",
            Self::OpenCode(_) => "OpenCode omitted from catalog",
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Claude(_) => "Claude Code",
            Self::Codex(_) => "Codex",
            Self::OpenCode(_) => "OpenCode",
        }
    }

    fn catalog_source(&self, data_dir: &std::path::Path) -> Source {
        match self {
            Self::Claude(cfg) => Source::Claude {
                executable: cfg.executable.clone(),
            },
            Self::Codex(cfg) => Source::Codex {
                executable: cfg.executable.clone(),
            },
            Self::OpenCode(cfg) => Source::OpenCode {
                cfg: cfg.clone(),
                data_dir: data_dir.to_owned(),
            },
        }
    }
}

/// Фоновая проба каталога: не задерживает старт сеанса и отчёт машины.
/// Каталог берётся из кэша по пути бинарника и версии; отказ означает «нет каталога».
pub(super) fn spawn_catalog_probe(
    probe: &Probe,
    version: &str,
    config: &WorkerConfig,
    catalogs: &Arc<Catalogs>,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let source = probe.catalog_source(&config.data_dir);
    let harness = source.harness().to_owned();
    let version = version.to_owned();
    let config = config.clone();
    let cache = catalogs.slash();
    let catalogs = catalogs.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = super::stopped(&mut shutdown) => {}
            _ = async {
                if let Some(entries) =
                    slash_catalog::load(&source, &version, &cache, &config.data_dir).await
                {
                    catalogs.publish_catalog(&config, &harness, entries);
                }
            } => {}
        }
    })
}

/// Текущий каталог машины и его последний отчёт для хаба.
pub(super) struct Catalogs {
    state: Mutex<CatalogState>,
    report: watch::Sender<Arc<MachineReport>>,
    npm: LatestVersions,
    slash: Arc<CatalogCache>,
}

impl Catalogs {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(CatalogState::default()),
            report: watch::channel(Arc::new(MachineReport::default())).0,
            npm: LatestVersions::default(),
            slash: Arc::new(CatalogCache::default()),
        }
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<Arc<MachineReport>> {
        self.report.subscribe()
    }

    pub(super) fn npm(&self) -> &LatestVersions {
        &self.npm
    }

    pub(super) fn slash(&self) -> Arc<CatalogCache> {
        Arc::clone(&self.slash)
    }

    /// Фоновая проба каталога обновляет только свой харнесс и пересылает отчёт.
    pub(super) fn publish_catalog(
        &self,
        config: &WorkerConfig,
        harness: &str,
        entries: Vec<CatalogEntry>,
    ) {
        self.state
            .lock()
            .expect("catalog lock")
            .catalogs
            .insert(harness.to_owned(), entries);
        self.emit(config);
    }

    pub(super) fn publish(&self, config: &WorkerConfig, state: CatalogState) {
        *self.state.lock().expect("catalog lock") = state;
        self.emit(config);
    }

    pub(super) fn insert(&self, config: &WorkerConfig, slot: Slot) {
        self.state.lock().expect("catalog lock").set(slot);
        self.emit(config);
    }

    fn emit(&self, config: &WorkerConfig) {
        let report = {
            let state = self.state.lock().expect("catalog lock");
            crate::machine::build_report_with_extras(
                config,
                &state.harnesses(config),
                &state.versions(config),
                &state.catalogs,
            )
        };
        self.report.send_replace(Arc::new(report));
    }
}

pub(super) fn retry_delays() -> Vec<Duration> {
    RETRY_DELAYS.to_vec()
}

/// Повтор пробы до успеха. Останавливается вместе с воркером; брошенная проба
/// снимает свой процесс через `Drop`/SIGTERM, как и стартовая.
pub(super) async fn retry_until_discovered(
    probe: Probe,
    config: WorkerConfig,
    catalogs: Arc<Catalogs>,
    mut shutdown: watch::Receiver<bool>,
    delays: Vec<Duration>,
) {
    let mut attempt = 0usize;
    let mut warned_at = super::Instant::now();
    loop {
        let delay = delays.get(attempt).copied().unwrap_or_else(|| {
            delays
                .last()
                .copied()
                .unwrap_or(RETRY_DELAYS[RETRY_DELAYS.len() - 1])
        });
        attempt += 1;
        tokio::select! {
            _ = super::stopped(&mut shutdown) => return,
            _ = tokio::time::sleep(delay) => {}
        }
        let result = tokio::select! {
            _ = super::stopped(&mut shutdown) => return,
            result = probe.discover(&config.data_dir, catalogs.npm()) => result,
        };
        match result {
            Ok(slot) => {
                let version = slot.version_label().to_owned();
                log_no_models(&probe, &slot);
                catalogs.insert(&config, slot);
                let _catalog_task = spawn_catalog_probe(
                    &probe,
                    &version,
                    &config,
                    &catalogs,
                    shutdown.clone(),
                );
                tracing::info!(
                    kind = probe.discovered_kind(),
                    "{} discovered after retry",
                    probe.title()
                );
                return;
            }
            Err(reason) => {
                if warned_at.elapsed() >= FAILURE_WARN_EVERY {
                    warned_at = super::Instant::now();
                    tracing::warn!(
                        kind = probe.failure_kind(),
                        reason = %reason,
                        "{}",
                        probe.omitted()
                    );
                }
            }
        }
    }
}

pub(super) fn log_no_models(probe: &Probe, slot: &Slot) {
    let (kind, message) = match (probe, slot) {
        (Probe::OpenCode(_), Slot::OpenCode { catalog, .. }) if catalog.models.is_empty() => {
            ("opencode_no_models", "OpenCode has no available models")
        }
        _ => return,
    };
    tracing::warn!(kind, "{message}");
}
