//! Пробы каталога харнессов: стартовая и повторные после сбоя.
//!
//! Сбой пробы на старте не должен убирать харнесс до перезапуска воркера: загрузка
//! машины проходит, и повтор возвращает каталог в живой поток управления.

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use canterlot_proto::worker::v1::MachineReport;
use tokio::sync::watch;

use crate::{
    adapter::{
        HarnessCatalog,
        antigravity::{self, AntigravityCatalog},
        claude_code::version::{self as claude_version, CliVersion},
        codex::{self, CodexCatalog},
        opencode::{self, OpenCodeCatalog},
    },
    config::{AntigravityConfig, ClaudeCodeConfig, CodexConfig, OpenCodeConfig, WorkerConfig},
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
    antigravity: Option<AntigravityCatalog>,
}

impl CatalogState {
    pub(super) fn set(&mut self, slot: Slot) {
        match slot {
            Slot::Claude(version) => self.claude_version = Some(version),
            Slot::Codex(catalog) => self.codex = Some(catalog),
            Slot::OpenCode(catalog) => self.opencode = Some(catalog),
            Slot::Antigravity(catalog) => self.antigravity = Some(catalog),
        }
    }

    fn harnesses(&self, config: &WorkerConfig) -> Vec<HarnessCatalog> {
        crate::adapter::catalog_with(
            config.claude_code.as_ref(),
            self.claude_version.as_ref(),
            self.codex.as_ref(),
            self.opencode.as_ref(),
            self.antigravity.as_ref(),
        )
    }
}

/// Каталог одного харнесса из успешной пробы.
pub(super) enum Slot {
    Claude(CliVersion),
    Codex(CodexCatalog),
    OpenCode(OpenCodeCatalog),
    Antigravity(AntigravityCatalog),
}

/// Настроенные харнессы. Выключенный в конфигурации харнесс сюда не попадает:
/// его не пробуют и не повторяют.
#[derive(Clone)]
pub(super) enum Probe {
    Claude(ClaudeCodeConfig),
    Codex(CodexConfig),
    OpenCode(OpenCodeConfig),
    Antigravity(AntigravityConfig),
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
        if let Some(cfg) = &config.antigravity
            && cfg.enabled
        {
            probes.push(Self::Antigravity(cfg.clone()));
        }
        if let Some(cfg) = &config.opencode {
            probes.push(Self::OpenCode(cfg.clone()));
        }
        probes
    }

    pub(super) async fn discover(&self, data_dir: &Path) -> Result<Slot, String> {
        let cache = data_dir.join(".cli-versions.json");
        match self {
            Self::Claude(cfg) => match claude_version::check_with_cache(
                &cfg.executable,
                crate::adapter::cli_version::DEFAULT_PROBE_DEADLINE,
                Some(&cache),
            )
            .await
            {
                Ok(version) => Ok(Slot::Claude(version)),
                Err(reason) => Err(format!("{reason:?}")),
            },
            Self::Codex(cfg) => codex::discover_with_cache(cfg, Some(&cache))
                .await
                .map(Slot::Codex)
                .map_err(|reason| format!("{reason:?}")),
            Self::OpenCode(cfg) => opencode::discover_with_cache(cfg, data_dir, Some(&cache))
                .await
                .map(Slot::OpenCode)
                .map_err(|reason| format!("{reason:?}")),
            Self::Antigravity(cfg) => antigravity::catalog::discover_with_cache(cfg, Some(&cache))
                .await
                .map(Slot::Antigravity)
                .map_err(|reason| format!("{reason:?}")),
        }
    }

    pub(super) fn failure_kind(&self) -> &'static str {
        match self {
            Self::Claude(_) => "claude_code_unavailable",
            Self::Codex(_) => "codex_unavailable",
            Self::OpenCode(_) => "opencode_unavailable",
            Self::Antigravity(_) => "antigravity_unavailable",
        }
    }

    fn discovered_kind(&self) -> &'static str {
        match self {
            Self::Claude(_) => "claude_code_discovered",
            Self::Codex(_) => "codex_discovered",
            Self::OpenCode(_) => "opencode_discovered",
            Self::Antigravity(_) => "antigravity_discovered",
        }
    }

    pub(super) fn omitted(&self) -> &'static str {
        match self {
            Self::Claude(_) => "Claude Code omitted from catalog",
            Self::Codex(_) => "Codex omitted from catalog",
            Self::OpenCode(_) => "OpenCode omitted from catalog",
            Self::Antigravity(_) => "Antigravity omitted from catalog",
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Claude(_) => "Claude Code",
            Self::Codex(_) => "Codex",
            Self::OpenCode(_) => "OpenCode",
            Self::Antigravity(_) => "Antigravity",
        }
    }
}

/// Текущий каталог машины и его последний отчёт для хаба.
pub(super) struct Catalogs {
    state: Mutex<CatalogState>,
    report: watch::Sender<Arc<MachineReport>>,
}

impl Catalogs {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(CatalogState::default()),
            report: watch::channel(Arc::new(MachineReport::default())).0,
        }
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<Arc<MachineReport>> {
        self.report.subscribe()
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
            crate::machine::build_report(config, &state.harnesses(config))
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
            result = probe.discover(&config.data_dir) => result,
        };
        match result {
            Ok(slot) => {
                log_no_models(&probe, &slot);
                catalogs.insert(&config, slot);
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
        (Probe::OpenCode(_), Slot::OpenCode(catalog)) if catalog.models.is_empty() => {
            ("opencode_no_models", "OpenCode has no available models")
        }
        (Probe::Antigravity(_), Slot::Antigravity(catalog)) if catalog.models.is_empty() => (
            "antigravity_no_models",
            "Antigravity has no available models",
        ),
        _ => return,
    };
    tracing::warn!(kind, "{message}");
}
