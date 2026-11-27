//! Утилита управления установкой canterlot: разбор команд, расположение файлов,
//! состояние хоста, записи операций, загрузка релизов и запуск команд.

// Ошибки busy и unfinished несут запись операции: этот состав зафиксирован контрактом.
#![allow(clippy::result_large_err)]

pub mod cli;
pub mod error;
pub mod layout;
pub mod ops;
pub mod release;
pub mod runner;
pub mod state;
