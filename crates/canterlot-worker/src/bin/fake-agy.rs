#![forbid(unsafe_code)]

//! Двойник `agy acp` для тестов: см. `adapter::antigravity::replay`.

fn main() -> std::process::ExitCode {
    canterlot_worker::adapter::antigravity::replay::main(std::env::args().skip(1).collect())
}
