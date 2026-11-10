//! Каталог моделей без хода: `initialize` + `session/new` во временном cwd, затем закрытие.
//! `configOptions.model` — семейство (`gemini-3.8-flash`), в каталог идут только
//! `models.availableModels`; `thought_level` хранится отдельно до контрактного теста P14.

use super::process::{PrepareError, Process, prepare_pinned};
use super::rpc::RpcClient;
use super::session::{self, ModelEntry, SessionInfo};
use super::version::CliVersion;
use crate::adapter::{HarnessCatalog, InstructionMode, InstructionSpec, ModelCatalog};
use crate::config::AntigravityConfig;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

const PROBE_LIMIT: Duration = Duration::from_secs(20);
const PROBE_STEP: Duration = Duration::from_secs(20);
const PROBE_CLOSE: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct AntigravityCatalog {
    pub version: CliVersion,
    pub models: Vec<ModelEntry>,
    pub current_model: Option<String>,
    pub efforts: Vec<String>,
    pub current_effort: Option<String>,
}

impl AntigravityCatalog {
    pub fn harness(&self) -> HarnessCatalog {
        HarnessCatalog {
            harness: "antigravity",
            provider: "antigravity",
            title: "Antigravity",
            // Картинки ACP молча выбрасывает (P22): передаётся только путь в тексте.
            images: false,
            settings_midsession: true,
            settings_midsession_effort: true,
            // read_only не подтверждён: настройки владельца разрешают часть инструментов.
            permission_modes: vec!["owner", "ask", "bypass"],
            models: self
                .models
                .iter()
                .map(|entry| {
                    let window = super::limits::context_window(&entry.id);
                    // У модели один effort: суффикс семейства. Переключателя нет,
                    // поэтому форма не показывает выбор, но значению есть откуда взяться.
                    let effort = entry
                        .id
                        .rsplit('-')
                        .next()
                        .filter(|suffix| matches!(*suffix, "low" | "medium" | "high"))
                        .map(str::to_owned)
                        .or_else(|| self.current_effort.clone())
                        .or_else(|| self.efforts.first().cloned());
                    ModelCatalog {
                        id: entry.id.clone(),
                        title: entry.title.clone(),
                        efforts: effort.into_iter().collect(),
                        images: Some(false),
                        context_window: (window > 0).then_some(window),
                    }
                })
                .collect(),
        }
    }
}

#[derive(Debug)]
pub enum DiscoverError {
    Prepare(PrepareError),
    Probe(ProbeError),
    Timeout,
}

#[derive(Debug)]
pub enum ProbeError {
    Launch(super::process::LaunchError),
    Session(session::SessionError),
    Protocol,
}

/// Закреплённая копия и один короткий `agy acp` без хода.
pub async fn discover(cfg: &AntigravityConfig) -> Result<AntigravityCatalog, DiscoverError> {
    discover_with_cache(cfg, None).await
}

/// То же с общим на процессы файлом кэша версии.
pub async fn discover_with_cache(
    cfg: &AntigravityConfig,
    cache: Option<&std::path::Path>,
) -> Result<AntigravityCatalog, DiscoverError> {
    let pinned = prepare_pinned(
        cfg,
        crate::adapter::cli_version::DEFAULT_PROBE_DEADLINE,
        cache,
    )
    .await
    .map_err(DiscoverError::Prepare)?;
    let data_dir = cfg
        .data_dir
        .clone()
        .ok_or(DiscoverError::Prepare(PrepareError::MissingDataDir))?;
    let root = data_dir
        .join("antigravity")
        .join("probe")
        .join(Uuid::now_v7().to_string());
    let work = probe(&pinned.executable, &root);
    let info = tokio::time::timeout(PROBE_LIMIT, work)
        .await
        .map_err(|_| DiscoverError::Timeout)?
        .map_err(DiscoverError::Probe)?;
    Ok(AntigravityCatalog {
        version: pinned.version,
        models: info.models,
        current_model: info.current_model,
        efforts: info.efforts,
        current_effort: info.current_effort,
    })
}

async fn probe(executable: &Path, root: &Path) -> Result<SessionInfo, ProbeError> {
    let guard = ProbeDir(root.to_owned());
    let workdir = root.join("ws");
    std::fs::create_dir_all(&workdir).map_err(|_| ProbeError::Protocol)?;
    let mut proc = Process::spawn_probe(executable, &workdir, root).map_err(ProbeError::Launch)?;
    let mut rpc = RpcClient::new();
    let result = async {
        session::initialize(&mut proc, &mut rpc, PROBE_STEP)
            .await
            .map_err(ProbeError::Session)?;
        session::new_session(
            &mut proc,
            &mut rpc,
            &workdir,
            &empty_instructions(),
            &serde_json::json!([]),
            PROBE_STEP,
        )
        .await
        .map_err(ProbeError::Session)
    }
    .await;
    proc.close_stdin();
    if tokio::time::timeout(PROBE_CLOSE, proc.wait())
        .await
        .is_err()
    {
        proc.kill_now();
        let _ = tokio::time::timeout(PROBE_CLOSE, proc.wait()).await;
    }
    drop(guard);
    result
}

fn empty_instructions() -> InstructionSpec {
    InstructionSpec {
        mode: InstructionMode::Clean,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

struct ProbeDir(PathBuf);

impl Drop for ProbeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Разбор ответа `session/new` из кадра этапа 0 (для тестов и каталога).
pub fn parse_frame(result: &serde_json::Value) -> Option<SessionInfo> {
    session::parse_session_result(result)
}
