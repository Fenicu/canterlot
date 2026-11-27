//! Утилита управления установкой canterlot: разбор команд, расположение файлов,
//! состояние хоста, записи операций, загрузка релизов и запуск команд.

// Ошибки busy и unfinished несут запись операции: этот состав зафиксирован контрактом.
#![allow(clippy::result_large_err)]

pub mod cli;
pub mod db;
pub mod error;
pub mod fsx;
pub mod hub_update;
pub mod identity;
pub mod install;
pub mod join;
pub mod layout;
pub mod ops;
pub mod release;
pub mod remove;
pub mod render;
pub mod root;
pub mod runner;
pub mod stack;
pub mod state;
pub mod status;
pub mod tls;
pub mod uninstall;
pub mod web;
pub mod worker;
pub mod worker_update;
