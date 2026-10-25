//! Использование: `codex-probe <scenario>|all|list`.

use std::process::ExitCode;

use codex_probe::scenarios;

#[tokio::main]
async fn main() -> ExitCode {
    let arg = std::env::args().nth(1).unwrap_or_else(|| "list".into());
    let names: Vec<&str> = match arg.as_str() {
        "list" => {
            for s in scenarios::SCENARIOS {
                println!(
                    "{:<16} planned model turns: {}",
                    s.name, s.model_turns_planned
                );
            }
            return ExitCode::SUCCESS;
        }
        "all" => scenarios::SCENARIOS.iter().map(|s| s.name).collect(),
        one => vec![one],
    };
    let mut failed = false;
    for name in names {
        match scenarios::run(name).await {
            Ok(v) => {
                println!("{name}: {} - {}", v.verdict, v.reason);
                failed |= v.verdict == "FAIL";
            }
            Err(e) => {
                eprintln!("{name}: probe setup error: {e:#}");
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
