#![forbid(unsafe_code)]

use std::env;
use std::error::Error;
use std::io;
use std::path::Path;
use std::process::ExitCode;

use canterlot_worker::{config, store::WorkerStore};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let path = args
        .next()
        .filter(|_| args.next().is_none())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "usage: canterlot-worker <config.toml>",
            )
        })?;
    let config = config::load(Path::new(&path))?;
    let _token = config::read_token(&config.token_file)?;
    let _store = WorkerStore::open(&config.data_dir)?;
    Ok(())
}
