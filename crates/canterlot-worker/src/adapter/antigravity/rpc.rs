//! JSON-RPC клиента ACP: тот же разбор кадров, что у Codex, с `jsonrpc:"2.0"` в запросе.

pub use crate::adapter::jsonrpc::{Incoming, RpcClient, RpcError, RpcId, classify};
