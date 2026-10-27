#![forbid(unsafe_code)]

#[cfg(all(feature = "fault-injection", not(debug_assertions)))]
compile_error!("fault-injection must not be built in release");

pub mod blocks;
pub mod config;
pub mod db;
pub mod delivery;
pub mod dispatcher;
pub mod events;
pub mod executions;
#[cfg(feature = "fault-injection")]
pub mod fault_injection;
pub mod grpc;
pub mod idempotency;
pub mod ingest;
pub mod installation;
pub mod limits;
pub mod machine_info;
pub mod machines;
pub mod model;
pub mod outbox;
pub mod runs;
pub mod session;
pub mod singleton;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
