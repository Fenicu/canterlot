#![forbid(unsafe_code)]

#[cfg(all(feature = "fault-injection", not(debug_assertions)))]
compile_error!("fault-injection must not be built in release");

pub mod adapter;
pub mod attachments;
pub mod config;
pub mod containment;
pub mod exec;
pub mod fatal;
#[cfg(feature = "fault-injection")]
pub mod fault_injection;
pub mod jobs;
pub mod lease;
pub mod machine;
pub mod mcp_relay;
pub mod media;
pub mod memory_import;
pub mod memory_scope;
pub mod ownership;
pub mod secret;
pub mod session;
pub mod session_store;
pub mod store;
pub mod supervisor;
pub mod watchdog;
pub mod worktree;

#[cfg(test)]
extern crate self as canterlot_worker;
