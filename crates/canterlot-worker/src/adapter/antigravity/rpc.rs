//! JSON-RPC клиента ACP: тот же разбор кадров, что у Codex, с `jsonrpc:"2.0"` в запросе.

pub use crate::adapter::jsonrpc::{Incoming, RpcClient, RpcError, RpcId, classify};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_request_carries_the_protocol_version() {
        let mut rpc = RpcClient::new();
        let (id, message) = rpc.request_v2("initialize", serde_json::json!({"protocolVersion": 1}));
        assert_eq!(id, 0);
        assert_eq!(message["jsonrpc"], "2.0");
        assert_eq!(message["method"], "initialize");
        assert_eq!(rpc.resolve(&RpcId::Int(0)), Some("initialize"));
    }

    #[test]
    fn acp_error_frames_keep_code_and_message() {
        let Incoming::Response {
            result: Err(error), ..
        } = classify(
            r#"{"jsonrpc":"2.0","id":11,"error":{"code":-32602,"message":"Unknown config option: "}}"#,
        )
        else {
            panic!("not an error");
        };
        assert_eq!(error.code, -32602);
        assert_eq!(error.message, "Unknown config option: ");
    }
}
