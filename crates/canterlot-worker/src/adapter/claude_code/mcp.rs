//! Совместимость: MCP Claude Code делегирует в общий модуль.

pub use crate::adapter::canterlot_mcp::mcp::{
    self, McpAction, SERVER_INSTRUCTIONS, ToolKind, allow_text, cli_policy_mark, deny_message,
    deny_text, instructions, is_question, normalize_questions, question_text, requested_tool_name,
    tool_call_reply,
};

use crate::adapter::canterlot_mcp::ToolSet;
use serde_json::Value;

pub fn handle_rpc(msg: &Value) -> McpAction {
    crate::adapter::canterlot_mcp::mcp::handle_rpc(msg, ToolSet::ApproveAndLimits)
}
