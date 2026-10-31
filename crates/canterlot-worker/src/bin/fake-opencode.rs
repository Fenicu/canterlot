#![forbid(unsafe_code)]

//! Двойник `opencode` для тестов: см. `adapter::opencode::replay`.

#[tokio::main]
async fn main() -> std::process::ExitCode {
    canterlot_worker::adapter::opencode::replay::main(std::env::args().skip(1).collect()).await
}
