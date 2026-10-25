//! Прототип адаптера Claude Code (этап 0): управляет официальным CLI
//! через документированный stream-json интерфейс и пишет доказательства.

mod broker;
mod config;
mod correlate;
mod evidence;
mod framing;
mod mcp_server;
mod process;
mod protocol;
#[cfg(test)]
mod real_samples;
mod scenarios;

const USAGE: &str =
    "usage: claude-probe <scenario|all|list> | claude-probe mcp-permission --socket <path>";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    match args.first().map(String::as_str) {
        Some("mcp-permission") => {
            let socket = args
                .iter()
                .position(|a| a == "--socket")
                .and_then(|i| args.get(i + 1))
                .ok_or_else(|| anyhow::anyhow!("--socket <path> is required"))?;
            rt.block_on(mcp_server::run(socket))
        }
        Some("list") => {
            for s in scenarios::ALL {
                println!("{s}");
            }
            Ok(())
        }
        Some("all") => {
            let cfg = config::Config::from_env()?;
            rt.block_on(async {
                for s in scenarios::ALL {
                    if let Err(e) = scenarios::run(s, &cfg).await {
                        eprintln!("{s}: error: {e:#}");
                    }
                }
            });
            Ok(())
        }
        Some(name) if !name.starts_with('-') => {
            let cfg = config::Config::from_env()?;
            rt.block_on(scenarios::run(name, &cfg))
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
