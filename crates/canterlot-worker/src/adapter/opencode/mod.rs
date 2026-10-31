//! Адаптер OpenCode 2: сервер `opencode serve --stdio` на исполнение, HTTP и SSE по loopback.
//! Харнесс спрятан за таблицей `[opencode]`: без неё не показывается и не запускается.

pub mod process;
#[cfg(feature = "synthetic-adapter")]
pub mod replay;
pub mod version;

use self::process::{DataLayout, LayoutError};
use crate::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, Capabilities, CloseMode, DecisionOutcome,
        HarnessCatalog, InterruptOutcome, StartSpec, Started, Support, Transmission,
        TransmitOutcome,
        cli_version::{CliVersion, VersionError},
    },
    config::OpenCodeConfig,
    containment::Containment,
};
use canterlot_proto::requests::DecisionPayload;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

/// Итог проверки при старте воркера: версия CLI. Каталог моделей появится с его срезом.
#[derive(Clone)]
pub struct OpenCodeCatalog {
    pub version: CliVersion,
}

impl OpenCodeCatalog {
    pub fn harness(&self) -> HarnessCatalog {
        HarnessCatalog {
            harness: "opencode",
            provider: "opencode",
            title: "OpenCode",
            images: false,
            settings_midsession: false,
            settings_midsession_effort: false,
            permission_modes: vec!["owner", "bypass"],
            models: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum DiscoverError {
    Version(VersionError),
    DataRoot(LayoutError),
}

// Каталоги и окружение своих данных: без них проверка версии пишет к владельцу.
async fn prepared_env(
    cfg: &OpenCodeConfig,
    layout: &DataLayout,
) -> Result<Vec<(OsString, OsString)>, LayoutError> {
    let cfg = cfg.clone();
    let layout = layout.clone();
    tokio::task::spawn_blocking(move || {
        let parent: Vec<_> = std::env::vars_os().collect();
        process::check_root(&layout, &cfg, &parent)?;
        layout.create().map_err(LayoutError::Io)?;
        let tmp = layout.tmp();
        Ok(process::base_env(parent.into_iter(), &layout, &tmp))
    })
    .await
    .map_err(|error| LayoutError::Io(error.into()))?
}

/// Проверка при старте воркера; без неё харнесс не попадает в каталог.
pub async fn discover(
    cfg: &OpenCodeConfig,
    data_dir: &Path,
) -> Result<OpenCodeCatalog, DiscoverError> {
    let layout = DataLayout::for_worker(cfg, data_dir);
    let env = prepared_env(cfg, &layout)
        .await
        .map_err(DiscoverError::DataRoot)?;
    let version = version::check(&cfg.executable, &env)
        .await
        .map_err(DiscoverError::Version)?;
    Ok(OpenCodeCatalog { version })
}

pub struct OpenCodeAdapter {
    cfg: OpenCodeConfig,
    exec_dir: PathBuf,
}

impl OpenCodeAdapter {
    pub fn new(cfg: OpenCodeConfig, exec_dir: PathBuf, _deadline: Duration) -> Self {
        Self { cfg, exec_dir }
    }
}

#[async_trait::async_trait]
impl Adapter for OpenCodeAdapter {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            streaming: Support::Unavailable("opencode_start_path"),
            resume: Support::Unavailable("opencode_start_path"),
            interrupt: Support::Unavailable("opencode_start_path"),
            approvals: Support::Unavailable("opencode_start_path"),
            questions: Support::Unavailable("opencode_start_path"),
            background_work: Support::Unavailable("opencode_start_path"),
            interrupt_keeps_process: true,
        }
    }

    async fn start(
        &mut self,
        _spec: &StartSpec,
        _group: &dyn Containment,
    ) -> Result<Started, AdapterError> {
        let layout = DataLayout::for_execution(&self.cfg, &self.exec_dir);
        let env = prepared_env(&self.cfg, &layout)
            .await
            .map_err(|_| AdapterError::StartFailed("data_root_refused"))?;
        match version::check(&self.cfg.executable, &env).await {
            Ok(_) => Err(AdapterError::StartFailed("opencode_start_path_missing")),
            Err(VersionError::NotFound) => Err(AdapterError::StartFailed("cli_not_found")),
            Err(VersionError::BelowMinimum(_)) => Err(AdapterError::StartFailed("cli_too_old")),
            Err(_) => Err(AdapterError::StartFailed("cli_version_failed")),
        }
    }

    async fn transmit(
        &mut self,
        _input: &Transmission,
        _group: &dyn Containment,
    ) -> TransmitOutcome {
        TransmitOutcome::Undetermined
    }

    async fn deliver_decision(
        &mut self,
        _request_id: &str,
        _decision: &DecisionPayload,
    ) -> DecisionOutcome {
        DecisionOutcome::NotApplied("request_not_pending".into())
    }

    async fn interrupt(&mut self) -> InterruptOutcome {
        InterruptOutcome::NotApplicable
    }

    async fn next_event(&mut self) -> AdapterEvent {
        std::future::pending().await
    }

    async fn close(
        &mut self,
        _mode: CloseMode,
        _group: &dyn Containment,
    ) -> Result<(), AdapterError> {
        Ok(())
    }
}
