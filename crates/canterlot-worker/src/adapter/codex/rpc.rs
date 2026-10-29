// Классификация строк JSON-RPC `codex app-server`: ответ, уведомление или запрос сервера.
// Порт прототипа spikes/codex-probe/src/protocol.rs; поля `jsonrpc` на проводе нет,
// поэтому смотрим только на `id`/`method`. Значения параметров могут нести текст
// сообщений, так что Debug здесь вручную и показывает только имена и наличие.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;

// ID запроса: сервер присылает числа, клиент всегда числа.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    Int(i64),
    Str(String),
}

impl RpcId {
    pub fn to_value(&self) -> Value {
        match self {
            RpcId::Int(i) => (*i).into(),
            RpcId::Str(s) => s.clone().into(),
        }
    }
}

impl fmt::Debug for RpcId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RpcId::Int(i) => write!(f, "Int({i})"),
            RpcId::Str(_) => f.write_str("Str"),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

impl fmt::Debug for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcError")
            .field("code", &self.code)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq)]
pub enum Incoming {
    Response {
        id: RpcId,
        result: Result<Value, RpcError>,
    },
    Notification {
        method: String,
        params: Value,
    },
    ServerRequest {
        id: RpcId,
        method: String,
        params: Value,
    },
    Invalid,
}

impl fmt::Debug for Incoming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Incoming::Response { id, result } => f
                .debug_struct("Response")
                .field("id", id)
                .field("ok", &result.is_ok())
                .finish(),
            Incoming::Notification { method, .. } => f
                .debug_struct("Notification")
                .field("method", method)
                .finish_non_exhaustive(),
            Incoming::ServerRequest { id, method, .. } => f
                .debug_struct("ServerRequest")
                .field("id", id)
                .field("method", method)
                .finish_non_exhaustive(),
            Incoming::Invalid => f.write_str("Invalid"),
        }
    }
}

// id+method — запрос сервера; только id — ответ; только method — уведомление.
pub fn classify(line: &str) -> Incoming {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Incoming::Invalid,
    };
    let Value::Object(mut map) = value else {
        return Incoming::Invalid;
    };
    let method = match map.remove("method") {
        Some(Value::String(m)) => Some(m),
        Some(_) => return Incoming::Invalid,
        None => None,
    };
    let id = match map.remove("id") {
        None | Some(Value::Null) => None,
        Some(raw) => match serde_json::from_value::<RpcId>(raw) {
            Ok(id) => Some(id),
            Err(_) => return Incoming::Invalid,
        },
    };
    let params = map.remove("params").unwrap_or(Value::Null);
    match (method, id) {
        (Some(method), Some(id)) => Incoming::ServerRequest { id, method, params },
        (Some(method), None) => Incoming::Notification { method, params },
        (None, Some(id)) => {
            if let Some(err) = map.remove("error") {
                match serde_json::from_value::<RpcError>(err) {
                    Ok(e) => Incoming::Response { id, result: Err(e) },
                    Err(_) => Incoming::Invalid,
                }
            } else if let Some(result) = map.remove("result") {
                Incoming::Response {
                    id,
                    result: Ok(result),
                }
            } else {
                Incoming::Invalid
            }
        }
        (None, None) => Incoming::Invalid,
    }
}

// Исходящие сообщения; id клиентов идут с нуля, соответствие ответов ждёт задача 7.
pub struct RpcClient {
    next: i64,
    pending: HashMap<i64, &'static str>,
}

impl Default for RpcClient {
    fn default() -> Self {
        Self::new()
    }
}

impl RpcClient {
    pub fn new() -> Self {
        Self {
            next: 0,
            pending: HashMap::new(),
        }
    }

    pub fn request(&mut self, method: &'static str, params: Value) -> (i64, Value) {
        let id = self.next;
        self.next += 1;
        self.pending.insert(id, method);
        (
            id,
            serde_json::json!({ "id": id, "method": method, "params": params }),
        )
    }

    pub fn notification(method: &str, params: Value) -> Value {
        serde_json::json!({ "method": method, "params": params })
    }

    pub fn result(id: &RpcId, result: Value) -> Value {
        serde_json::json!({ "id": id.to_value(), "result": result })
    }

    pub fn error(id: &RpcId, code: i64, message: &str) -> Value {
        serde_json::json!({ "id": id.to_value(), "error": { "code": code, "message": message } })
    }

    pub fn resolve(&mut self, id: &RpcId) -> Option<&'static str> {
        match id {
            RpcId::Int(n) => self.pending.remove(n),
            RpcId::Str(_) => None,
        }
    }
}
