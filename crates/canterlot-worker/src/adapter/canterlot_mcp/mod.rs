//! Общий MCP-сервер canterlot для адаптеров Claude Code и Codex.

pub mod broker;
pub mod mcp;

pub use broker::{Broker, BrokerEvent, RELAY_SECRET_ENV, relay_hello};
pub use mcp::{
    AGENT_TOOLS, CODEX_SERVER_INSTRUCTIONS, SERVER_INSTRUCTIONS, SERVER_NAME, ToolSet,
    agent_tool_text, limits_tool_text, valid_preset_arguments,
};
