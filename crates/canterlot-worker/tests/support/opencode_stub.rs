//! Заглушка провайдера для L2: OpenAI Chat Completions по SSE на loopback, порт
//! заглушки этапа 0. Ответы идут по сценарию, запросы сохраняются для проверок.

#![allow(dead_code)]

use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

pub enum Reply {
    Text { reasoning: String, text: String },
    Tools(Vec<(String, Value)>),
    Status { status: u16, body: Value },
}

impl Reply {
    pub fn text(text: &str) -> Self {
        Self::Text {
            reasoning: String::new(),
            text: text.into(),
        }
    }

    pub fn tool(name: &str, arguments: Value) -> Self {
        Self::Tools(vec![(name.into(), arguments)])
    }
}

#[derive(Default)]
struct Shared {
    replies: VecDeque<Reply>,
    requests: Vec<Value>,
}

pub struct Stub {
    pub port: u16,
    shared: Arc<Mutex<Shared>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn chunk(delta: Value, finish: Option<&str>) -> String {
    let frame = json!({
        "id": "chatcmpl-stub",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": "fake-1",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    });
    format!("data: {frame}\n\n")
}

fn usage() -> String {
    let frame = json!({
        "id": "chatcmpl-stub",
        "object": "chat.completion.chunk",
        "created": 1,
        "model": "fake-1",
        "choices": [],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 20,
            "total_tokens": 120,
            "prompt_tokens_details": {"cached_tokens": 30},
            "completion_tokens_details": {"reasoning_tokens": 5},
        },
    });
    format!("data: {frame}\n\n")
}

fn stream(reply: &Reply) -> String {
    let mut body = chunk(json!({"role": "assistant"}), None);
    let finish = match reply {
        Reply::Text { reasoning, text } => {
            if !reasoning.is_empty() {
                body += &chunk(json!({"reasoning_content": reasoning}), None);
            }
            let mut split = text.len() / 2;
            while !text.is_char_boundary(split) {
                split -= 1;
            }
            body += &chunk(json!({"content": &text[..split]}), None);
            body += &chunk(json!({"content": &text[split..]}), None);
            "stop"
        }
        Reply::Tools(calls) => {
            for (index, (name, arguments)) in calls.iter().enumerate() {
                body += &chunk(
                    json!({"tool_calls": [{
                        "index": index,
                        "id": format!("call_{index}_stub"),
                        "type": "function",
                        "function": {"name": name, "arguments": arguments.to_string()},
                    }]}),
                    None,
                );
            }
            "tool_calls"
        }
        Reply::Status { .. } => "stop",
    };
    body += &chunk(json!({}), Some(finish));
    body += &usage();
    body += "data: [DONE]\n\n";
    body
}

async fn serve(shared: Arc<Mutex<Shared>>, stream_socket: TcpStream) {
    let (read, mut write) = stream_socket.into_split();
    let mut reader = BufReader::new(read);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).await.unwrap_or(0) == 0 {
        return;
    }
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await.unwrap_or(0) == 0 {
            return;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).await.is_err() {
        return;
    }
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let reply = {
        let mut shared = shared.lock().unwrap();
        shared.requests.push(request);
        shared.replies.pop_front()
    };
    let reply = reply.unwrap_or_else(|| Reply::text("out of script"));
    let response = match &reply {
        Reply::Status { status, body } => {
            let body = body.to_string();
            format!(
                "HTTP/1.1 {status} Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
        }
        reply => format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{}",
            stream(reply)
        ),
    };
    let _ = write.write_all(response.as_bytes()).await;
    let _ = write.shutdown().await;
}

impl Stub {
    pub async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = Arc::new(Mutex::new(Shared {
            replies: replies.into(),
            requests: Vec::new(),
        }));
        let task = {
            let shared = shared.clone();
            tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    tokio::spawn(serve(shared.clone(), socket));
                }
            })
        };
        Self { port, shared, task }
    }

    pub fn requests(&self) -> Vec<Value> {
        self.shared.lock().unwrap().requests.clone()
    }

    /// То же с явным вводом модели: без него сервер считает, что картинки она принимает.
    pub fn config_with_input(&self, input: &[&str]) -> String {
        let mut config: Value = serde_json::from_str(&self.config()).unwrap();
        config["providers"]["fake"]["models"]["fake-1"]["capabilities"]["input"] = json!(input);
        config.to_string()
    }

    /// `opencode.json` с единственной моделью `fake/fake-1` на этой заглушке.
    pub fn config(&self) -> String {
        json!({
            "providers": {"fake": {
                "name": "Fake",
                "package": "@ai-sdk/openai-compatible",
                "settings": {"baseURL": format!("http://127.0.0.1:{}/v1", self.port), "apiKey": "stub"},
                "models": {"fake-1": {
                    "name": "Fake One",
                    "limit": {"context": 200000, "output": 8192},
                    "capabilities": {
                        "tools": true,
                        "reasoning": {"supported": true, "effort": ["low", "medium", "high"]},
                    },
                }},
            }},
            "model": "fake/fake-1",
        })
        .to_string()
    }
}
