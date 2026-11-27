//! `canterlot status`: что установлено, какие версии, идёт ли операция и чем кончилась последняя.

use std::{fmt, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    cli::TlsMode,
    error::CtlError,
    layout::Layout,
    ops::{self, OpKind, OpRecord, OpResult},
    runner::Runner,
    stack::compose,
    state::{Components, InstallState},
};

#[derive(Clone, Debug, Serialize)]
pub struct OpSummary {
    pub id: String,
    pub kind: OpKind,
    pub target: Option<String>,
    pub phase: String,
    pub result: Option<OpResult>,
}

impl From<OpRecord> for OpSummary {
    fn from(rec: OpRecord) -> Self {
        Self {
            id: rec.id.0,
            kind: rec.kind,
            target: rec.target.map(|v| v.to_string()),
            phase: rec.phase,
            result: rec.result,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceState {
    #[serde(rename = "Service")]
    pub service: String,
    #[serde(rename = "State")]
    pub state: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StatusReport {
    pub installed: bool,
    pub components: Components,
    pub tls: Option<TlsMode>,
    pub origin: Option<String>,
    pub grpc_endpoint: Option<String>,
    pub hub_version: Option<String>,
    pub web_version: Option<String>,
    pub worker_version: Option<String>,
    pub services: Vec<ServiceState>,
    pub services_error: Option<String>,
    pub running: Option<OpSummary>,
    pub last: Option<OpSummary>,
    pub worker_running: Option<OpSummary>,
}

/// Версия — имя каталога, на который указывает ссылка `current`/`build`.
fn link_version(link: &Path) -> Option<String> {
    let target = std::fs::read_link(link).ok()?;
    target
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
}

fn parse_services(text: &str) -> Result<Vec<ServiceState>, serde_json::Error> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    // Старые compose печатают массив, новые — по объекту в строке.
    if text.starts_with('[') {
        return serde_json::from_str(text);
    }
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect()
}

pub async fn status(layout: &Layout, runner: &dyn Runner) -> Result<StatusReport, CtlError> {
    let state = InstallState::load(layout)?;
    let home = layout
        .home
        .clone()
        .or_else(|| state.as_ref().and_then(|s| s.worker_home.clone()));
    let worker_layout = home.map(|home| Layout::new(layout.root.clone(), Some(home)));

    let mut services = Vec::new();
    let mut services_error = None;
    if layout.compose_dir().join("compose.yaml").is_file() {
        match runner
            .run(compose(layout, &["ps", "--all", "--format", "json"]))
            .await
        {
            Ok(out) if out.status == 0 => {
                match parse_services(&String::from_utf8_lossy(&out.stdout)) {
                    Ok(parsed) => services = parsed,
                    Err(_) => services_error = Some("unreadable docker compose ps output".into()),
                }
            }
            Ok(out) => {
                services_error = Some(format!("docker compose ps exited with {}", out.status))
            }
            Err(err) => services_error = Some(err.to_string()),
        }
    }

    let ops_dir = layout.ops_dir();
    Ok(StatusReport {
        installed: state.is_some(),
        components: state
            .as_ref()
            .map(|s| s.components.clone())
            .unwrap_or_default(),
        tls: state.as_ref().and_then(|s| s.tls),
        origin: state.as_ref().and_then(|s| s.origin.clone()),
        grpc_endpoint: state.as_ref().and_then(|s| s.grpc_endpoint.clone()),
        hub_version: link_version(&layout.hub_current()),
        web_version: link_version(&layout.web_parent().join("build")),
        worker_version: worker_layout
            .as_ref()
            .and_then(|wl| link_version(&wl.worker_current())),
        services,
        services_error,
        running: ops::unfinished(&ops_dir)?.map(OpSummary::from),
        last: ops::latest(&ops_dir)?
            .filter(|rec| rec.result.is_some())
            .map(OpSummary::from),
        worker_running: match &worker_layout {
            Some(wl) => ops::unfinished(&wl.worker_ops_dir())?.map(OpSummary::from),
            None => None,
        },
    })
}

fn opt(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("-")
}

impl fmt::Display for StatusReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.installed && self.hub_version.is_none() && self.worker_version.is_none() {
            writeln!(f, "canterlot is not installed on this host.")?;
        }
        writeln!(f, "hub:    {}", opt(&self.hub_version))?;
        writeln!(f, "web:    {}", opt(&self.web_version))?;
        writeln!(f, "worker: {}", opt(&self.worker_version))?;
        if let Some(origin) = &self.origin {
            writeln!(f, "PWA:    {origin}")?;
        }
        if let Some(endpoint) = &self.grpc_endpoint {
            writeln!(f, "gRPC:   {endpoint}")?;
        }
        for service in &self.services {
            writeln!(f, "  {:<10} {}", service.service, service.state)?;
        }
        if let Some(error) = &self.services_error {
            writeln!(f, "services: {error}")?;
        }
        for (label, op) in [
            ("running", &self.running),
            ("worker running", &self.worker_running),
        ] {
            if let Some(op) = op {
                writeln!(
                    f,
                    "{label}: {:?} {} {} (phase {})",
                    op.kind,
                    opt(&op.target),
                    op.id,
                    op.phase
                )?;
            }
        }
        if let Some(op) = &self.last {
            writeln!(
                f,
                "last: {:?} {} {}: {:?}",
                op.kind,
                opt(&op.target),
                op.id,
                op.result
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_ps_output_in_both_formats() {
        let lines = "{\"Service\":\"hub\",\"State\":\"running\"}\n{\"Service\":\"caddy\",\"State\":\"exited\"}\n";
        assert_eq!(parse_services(lines).unwrap().len(), 2);
        let array = "[{\"Service\":\"hub\",\"State\":\"running\"}]";
        assert_eq!(parse_services(array).unwrap()[0].state, "running");
        assert!(parse_services("").unwrap().is_empty());
    }

    #[tokio::test]
    async fn status_on_empty_host() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().to_path_buf(), None);
        let runner = crate::runner::FakeRunner::new();
        let report = status(&layout, &runner).await.unwrap();
        assert!(!report.installed);
        assert!(report.to_string().contains("not installed"));
        assert!(runner.calls().is_empty());
    }
}
