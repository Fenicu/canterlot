//! Повтор captured app-server для тестов: читает свидетельство, ждёт методы
//! адаптера и пишет ответы с перезаписями id. Только тесты и ручной прогон
//! (feature `synthetic-adapter`); боевые тесты реплея живут в tests/it/codex_e2e.rs.

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn method_of(wrapper: &Value) -> Option<&str> {
    wrapper
        .get("data")
        .and_then(|data| data.get("method"))
        .and_then(Value::as_str)
}

fn is_omitted(data: &Value) -> bool {
    data.get("result")
        .and_then(Value::as_str)
        .is_some_and(|result| result.starts_with("<omitted"))
}

fn fixture_for(fixtures: &Path, method: &str) -> Option<Value> {
    let name = format!("{}.json", method.replace('/', "_"));
    let text = std::fs::read_to_string(fixtures.join(name)).ok()?;
    serde_json::from_str(&text).ok()
}

// id ответа: записанный → id адаптера из совпавшего запроса.
fn rewrite_ids(
    data: &mut Value,
    idmap: &HashMap<String, Value>,
    clients: &HashMap<String, String>,
) {
    if data.get("method").is_none()
        && let Some(id) = data.get("id").cloned()
    {
        let key = serde_json::to_string(&id).unwrap_or_default();
        if let Some(adapter) = idmap.get(&key) {
            data["id"] = adapter.clone();
        }
    }
    let thread = data
        .get("params")
        .and_then(|params| params.get("threadId"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(client) = clients.get(thread)
        && let Some(item) = data
            .get_mut("params")
            .and_then(|params| params.get_mut("item"))
        && item.get("type").and_then(Value::as_str) == Some("userMessage")
    {
        item["clientId"] = client.clone().into();
    }
}

pub async fn run(
    evidence: &Path,
    fixtures: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Player::load(evidence, fixtures)?.run().await
}

struct Player {
    lines: Vec<Value>,
    fixtures: PathBuf,
    idmap: HashMap<String, Value>,
    clients: HashMap<String, String>,
    current: String,
    pos: usize,
}

impl Player {
    fn load(
        evidence: &Path,
        fixtures: &Path,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let text = std::fs::read_to_string(evidence)?;
        let lines = text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        Ok(Self {
            lines,
            fixtures: fixtures.to_owned(),
            idmap: HashMap::new(),
            clients: HashMap::new(),
            current: String::new(),
            pos: 0,
        })
    }

    async fn emit(
        &self,
        stdout: &mut tokio::io::Stdout,
        stderr: &mut tokio::io::Stderr,
        wrapper: &Value,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let dir = wrapper
            .get("dir")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let data = wrapper.get("data").cloned().unwrap_or(Value::Null);
        match dir {
            "out" => {
                let mut data = data;
                if is_omitted(&data) {
                    let id = data.get("id").cloned().unwrap_or(Value::Null);
                    let key = serde_json::to_string(&id).unwrap_or_default();
                    let adapter = self.idmap.get(&key).cloned().unwrap_or(id);
                    let result = fixture_for(&self.fixtures, &self.current)
                        .unwrap_or(Value::Object(Default::default()));
                    let line = serde_json::json!({"id": adapter, "result": result});
                    stdout.write_all(line.to_string().as_bytes()).await?;
                    stdout.write_all(b"\n").await?;
                    stdout.flush().await?;
                    return Ok(());
                }
                rewrite_ids(&mut data, &self.idmap, &self.clients);
                stdout.write_all(data.to_string().as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            "err" => {
                let text = match &data {
                    Value::String(line) => line.clone(),
                    other => other.to_string(),
                };
                stderr.write_all(text.as_bytes()).await?;
                stderr.write_all(b"\n").await?;
                stderr.flush().await?;
            }
            _ => {}
        }
        Ok(())
    }

    // Кадры до следующей входящей строки с методом.
    async fn flush_to_request(
        &mut self,
        stdout: &mut tokio::io::Stdout,
        stderr: &mut tokio::io::Stderr,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        while self.pos < self.lines.len() && method_of(&self.lines[self.pos]).is_none() {
            // Ответы зонда на запросы сервера не ждём: они без метода.
            if self.lines[self.pos].get("dir").and_then(Value::as_str) == Some("in") {
                self.pos += 1;
                continue;
            }
            let wrapper = self.lines[self.pos].clone();
            self.emit(stdout, stderr, &wrapper).await?;
            self.pos += 1;
        }
        Ok(())
    }

    async fn run(mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let stdin = tokio::io::stdin();
        let mut input = BufReader::new(stdin).lines();
        let mut stdout = tokio::io::stdout();
        let mut stderr = tokio::io::stderr();
        while let Some(line) = input.next_line().await? {
            let message: Value = match serde_json::from_str(&line) {
                Ok(message) => message,
                Err(_) => continue,
            };
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                // Ответы адаптера — просто повод отдать накопившееся.
                self.flush_to_request(&mut stdout, &mut stderr).await?;
                continue;
            };
            let method = method.to_owned();
            if method == "turn/start"
                && let Some(params) = message.get("params")
                && let (Some(thread), Some(client)) = (
                    params.get("threadId").and_then(Value::as_str),
                    params.get("clientUserMessageId").and_then(Value::as_str),
                )
            {
                self.clients.insert(thread.to_owned(), client.to_owned());
            }
            self.flush_to_request(&mut stdout, &mut stderr).await?;
            // Ближайшая входящая строка с этим методом.
            let mut found = None;
            for (index, wrapper) in self.lines.iter().enumerate().skip(self.pos) {
                if method_of(wrapper) == Some(method.as_str()) {
                    found = Some(index);
                    break;
                }
            }
            if let Some(index) = found {
                let recorded = self.lines[index]
                    .get("data")
                    .cloned()
                    .unwrap_or(Value::Null);
                if let (Some(adapter), Some(recorded_id)) = (message.get("id"), recorded.get("id"))
                {
                    let key = serde_json::to_string(recorded_id).unwrap_or_default();
                    self.idmap.insert(key, adapter.clone());
                }
                self.current = method.clone();
                self.pos = index + 1;
                self.flush_to_request(&mut stdout, &mut stderr).await?;
            } else if method != "initialized"
                && let Some(adapter) = message.get("id")
            {
                let result = fixture_for(&self.fixtures, &method)
                    .unwrap_or(Value::Object(Default::default()));
                let line = serde_json::json!({"id": adapter, "result": result});
                stdout.write_all(line.to_string().as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
        }
        Ok(())
    }
}
