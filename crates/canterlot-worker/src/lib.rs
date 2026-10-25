#![forbid(unsafe_code)]

pub mod config;
pub mod session;
pub mod store;

mod session_store;

#[cfg(test)]
extern crate self as canterlot_worker;
