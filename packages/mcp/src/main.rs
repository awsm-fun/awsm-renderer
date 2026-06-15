//! `awsm-scene-mcp` — drives the awsm-renderer editor from an AI agent.
//!
//! Topology: an MCP client speaks rmcp `/mcp` to this server; the in-browser
//! editor (browser wasm) dials *out* to the server's `/editor` WebSocket and
//! serves `Request`s against its `EditorController`. The server is a stateless
//! bridge — the browser holds the document truth. Rendered PNGs ride a `/png/<id>`
//! byte side-channel, never the control link.

mod http;
mod link;
mod mcp;
mod ws;

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;

use crate::link::EditorLink;

const DEFAULT_PORT: u16 = 9086;

/// CLI arguments: the single listen port.
///
/// One HTTP listener serves two peers: the MCP client / agent (rmcp `/mcp` +
/// `/debug`) and the in-browser editor (the `/editor` WebSocket it dials out to,
/// plus the `/png/<id>` byte side-channel). This is the
/// `?mcp=http://127.0.0.1:<port>` origin the editor points at.
#[derive(Debug, Parser)]
#[command(
    name = "awsm-scene-mcp",
    version,
    about = "Native MCP server for the awsm-renderer editor.",
    long_about = "Native MCP server for the awsm-renderer editor — a stateless bridge \
between an MCP client and the in-browser editor.\n\n\
One HTTP listener (--port) serves both peers:\n\
  - the MCP client / agent: the rmcp `/mcp` endpoint (+ `/debug`),\n\
  - the in-browser editor: the `/editor` WebSocket it dials out to, plus the \
`/png/<id>` side-channel it uploads rendered PNGs on.\n\
This is the `?mcp=http://127.0.0.1:<port>` origin the editor points at."
)]
struct Args {
    /// HTTP port for the MCP client (rmcp `/mcp`, `/debug`), the editor `/editor`
    /// WebSocket, and the `/png` side-channel; this is also the
    /// `?mcp=http://127.0.0.1:<port>` origin the editor points at.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,awsm_scene_mcp=debug".into()),
        )
        .init();

    let args = Args::parse();

    let link = EditorLink::shared(format!("http://127.0.0.1:{}", args.port));

    let http_addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    tracing::info!("awsm-scene-mcp: rmcp /mcp + editor /editor ws + /png on http://{http_addr}");
    http::serve(http_addr, link).await.context("http server")?;
    Ok(())
}
