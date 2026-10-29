//! Общий MCP-сервер canterlot для адаптеров Claude Code и Codex.

pub mod broker;
pub mod mcp;

pub use broker::{Broker, BrokerEvent};
pub use mcp::{SERVER_INSTRUCTIONS, SERVER_NAME, ToolSet, limits_tool_text};
