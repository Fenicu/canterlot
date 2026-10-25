#![forbid(unsafe_code)]

#[cfg(all(feature = "fault-injection", not(debug_assertions)))]
compile_error!("fault-injection must not be built in release");

pub mod config;
#[cfg(feature = "fault-injection")]
pub mod fault_injection;
pub mod session;
pub mod store;

mod session_store;

#[cfg(test)]
extern crate self as canterlot_worker;
