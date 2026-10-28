#![forbid(unsafe_code)]

#[cfg(all(feature = "fault-injection", not(debug_assertions)))]
compile_error!("fault-injection must not be built in release");

pub mod activity;
pub mod attachments;
pub mod background;
pub mod blocks;
pub mod bus;
pub mod config;
pub mod conversations;
pub mod crunch;
pub mod db;
pub mod dberr;
pub mod delivery;
pub mod dispatcher;
pub mod events;
pub mod executions;
#[cfg(feature = "fault-injection")]
pub mod fault_injection;
pub mod groups;
pub mod grpc;
pub mod history;
pub mod http;
pub mod idempotency;
pub mod ingest;
pub mod installation;
pub mod limits;
pub mod machine_info;
pub mod machines;
pub mod model;
pub mod outbox;
pub mod runs;
pub mod scheduler;
pub mod session;
pub mod singleton;
pub mod snapshot;
pub mod subscriptions;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
