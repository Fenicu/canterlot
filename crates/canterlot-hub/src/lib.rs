// Единственное исключение: отображение базы гео в память (`owner_auth::geo::open_reader`).
#![deny(unsafe_code)]

#[cfg(all(feature = "fault-injection", not(debug_assertions)))]
compile_error!("fault-injection must not be built in release");

pub mod activity;
pub mod agents;
pub mod attachments;
pub mod background;
pub mod background_tasks;
pub mod blocks;
pub mod bookmarks;
pub mod bus;
pub mod compactions;
pub mod config;
pub mod conversations;
pub mod crunch;
pub mod db;
pub mod dberr;
pub mod deletion;
pub mod delivery;
pub mod dispatcher;
pub mod drafts;
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
pub mod leases;
pub mod limit_settings;
pub mod limits;
pub mod limits_forecast;
pub mod machine_info;
pub mod machine_watch;
pub mod machines;
pub mod media;
pub mod model;
pub mod model_resolution;
pub mod notify;
pub mod outbox;
pub mod owner_auth;
pub mod presets;
pub mod preview;
pub mod prompt_suggestions;
pub mod provider_status;
pub mod requests;
pub mod runs;
pub mod scheduler;
pub mod search;
pub mod session;
pub mod session_tools;
pub mod session_usage;
pub mod singleton;
pub mod snapshot;
pub mod speed;
pub mod subscriptions;
pub mod todos;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
