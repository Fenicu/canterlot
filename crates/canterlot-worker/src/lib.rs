#![forbid(unsafe_code)]

#[cfg(all(feature = "fault-injection", not(debug_assertions)))]
compile_error!("fault-injection must not be built in release");

pub mod adapter;
pub mod attachments;
pub mod config;
pub mod containment;
pub mod exec;
#[cfg(feature = "fault-injection")]
pub mod fault_injection;
pub mod machine;
pub mod ownership;
pub mod session;
pub mod session_store;
pub mod store;
pub mod supervisor;

#[cfg(test)]
extern crate self as canterlot_worker;
