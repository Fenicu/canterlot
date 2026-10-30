//! Общий MCP-сервер canterlot для адаптеров Claude Code и Codex.

pub mod broker;
pub mod mcp;

pub use broker::{Broker, BrokerEvent};
pub use mcp::{
    PRESET_TOOLS, SERVER_INSTRUCTIONS, SERVER_NAME, ToolSet, agent_tool_text, limits_tool_text,
    valid_preset_arguments,
};
