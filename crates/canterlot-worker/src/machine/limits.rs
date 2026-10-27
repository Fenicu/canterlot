use canterlot_proto::worker::v1::LimitObservation;
#[async_trait::async_trait]
pub trait LimitsSource: Send + Sync {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation>;
}
pub struct NoLimitsSource;
#[async_trait::async_trait]
impl LimitsSource for NoLimitsSource {
    async fn poll(&self, _provider: &str) -> Vec<LimitObservation> {
        Vec::new()
    }
}

#[cfg(feature = "synthetic-adapter")]
pub struct FileLimitsSource {
    data_dir: std::path::PathBuf,
}
#[cfg(feature = "synthetic-adapter")]
impl FileLimitsSource {
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        Self { data_dir }
    }
}
#[cfg(feature = "synthetic-adapter")]
#[async_trait::async_trait]
impl LimitsSource for FileLimitsSource {
    async fn poll(&self, provider: &str) -> Vec<LimitObservation> {
        use canterlot_proto::worker::v1::LimitWindow;
        if !matches!(provider, "claude" | "codex") {
            return Vec::new();
        }
        let path = self
            .data_dir
            .join("synthetic-limits")
            .join(format!("{provider}.json"));
        let Ok(file) = tokio::fs::File::open(path).await else {
            return Vec::new();
        };
        let mut bytes = Vec::new();
        use tokio::io::AsyncReadExt;
        if file.take(65537).read_to_end(&mut bytes).await.is_err() || bytes.len() > 65536 {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Vec::new();
        };
        let at = super::now_ms();
        [
            ("five_hour", LimitWindow::FiveHour),
            ("weekly", LimitWindow::Weekly),
        ]
        .into_iter()
        .filter_map(|(key, window)| {
            let row = value.get(key)?;
            let used = row["used_permille"].as_u64().filter(|v| *v <= 1000)?;
            Some(LimitObservation {
                provider: provider.into(),
                window: window as i32,
                used_permille: used as u32,
                resets_at_ms: row["resets_at_ms"].as_u64().unwrap_or(0),
                observed_machine_ms: at,
            })
        })
        .collect()
    }
}
