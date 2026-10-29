//! Pure stdio↔socket byte relay for the canterlot MCP server.
//!
//! The CLI spawns `canterlot-worker mcp-relay --socket <path>`; every line
//! read from stdin is forwarded to the supervisor socket, every line read
//! from the socket is written to stdout. No JSON-RPC handling here: the
//! supervisor side parses and answers. Only JSON-RPC goes to stdout;
//! diagnostics go to stderr.

use std::path::Path;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

pub async fn run(socket: &Path) -> std::io::Result<()> {
    let stream = UnixStream::connect(socket).await?;
    let (mut from_socket, mut to_socket) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let pump_out = async move {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = from_socket.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            stdout.write_all(&buf[..n]).await?;
            stdout.flush().await?;
        }
        Ok::<(), std::io::Error>(())
    };
    let pump_in = async move {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = stdin.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            to_socket.write_all(&buf[..n]).await?;
            to_socket.flush().await?;
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::select! {
        result = pump_out => result,
        result = pump_in => result,
    }
}
