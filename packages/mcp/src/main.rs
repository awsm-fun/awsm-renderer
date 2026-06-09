//! `awsm-renderer-mcp` — drives the awsm-renderer editor from an AI agent.
//!
//! The editor (browser wasm) dials *out* to this process over WebTransport
//! (QUIC); this process exposes MCP tools (added in a later phase) that relay to
//! the attached editor. For now: the QUIC listener + the `/control` HTTP
//! endpoint (cert-hash discovery), proving the editor can attach and round-trip
//! a request.

mod cert;
mod http;
mod link;
mod mcp;
mod quic;

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;

use crate::cert::GeneratedCert;
use crate::link::EditorLink;

const DEFAULT_CLIENT_PORT: u16 = 9086;
const DEFAULT_BROWSER_PORT: u16 = 9087;

/// CLI arguments: the two listen ports.
///
/// The server runs two listeners for two different peers:
///
///   --client-port  (HTTP, TCP)  the MCP client / agent connects here: the rmcp
///                               `/mcp` endpoint, plus `/control` + `/debug`. This
///                               is also the `?mcp=http://127.0.0.1:<port>` origin
///                               the editor points at to fetch the cert + dial info.
///
///   --browser-port (WebTransport / QUIC, UDP)  the in-browser editor dials *out*
///                               to this for the live data link.
#[derive(Debug, Parser)]
#[command(
    name = "awsm-renderer-mcp",
    version,
    about = "Native MCP server for the awsm-renderer editor.",
    long_about = "Native MCP server for the awsm-renderer editor — a stateless bridge \
between an MCP client and the in-browser editor.\n\n\
It runs two listeners for two different peers:\n\
  - --client-port  (HTTP / TCP):  the MCP client / agent connects here (rmcp `/mcp`, \
plus `/control` + `/debug`). This is also the `?mcp=http://127.0.0.1:<port>` origin \
the editor points at.\n\
  - --browser-port (WebTransport / QUIC, UDP):  the in-browser editor dials out to \
this for the live data link."
)]
struct Args {
    /// HTTP/TCP port for the MCP client + control surface (rmcp `/mcp`, `/control`,
    /// `/debug`); this is also the `?mcp=http://127.0.0.1:<port>` origin the editor points at.
    #[arg(long, default_value_t = DEFAULT_CLIENT_PORT)]
    client_port: u16,

    /// WebTransport (QUIC / UDP) port the in-browser editor dials out to for the
    /// live data link.
    #[arg(long, default_value_t = DEFAULT_BROWSER_PORT)]
    browser_port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,awsm_renderer_mcp=debug".into()),
        )
        .init();

    // rustls needs a process-wide default crypto provider installed for the
    // aws-lc-rs backend that quinn's TLS uses.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let args = Args::parse();

    let cert = Arc::new(GeneratedCert::new("localhost").context("generate dev cert")?);
    tracing::info!("dev cert hash (base64url): {}", cert.hash_base64url());

    let link = EditorLink::shared();

    let quic_addr = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), args.browser_port);
    let endpoint = quic::build_endpoint(&cert, quic_addr).context("build QUIC endpoint")?;
    tracing::info!("WebTransport (QUIC) listening on udp/{}", args.browser_port);
    tokio::spawn(quic::accept_loop(endpoint, link.clone()));

    let http_addr = SocketAddr::from(([127, 0, 0, 1], args.client_port));
    http::serve(http_addr, cert, args.browser_port, link)
        .await
        .context("control http server")?;

    Ok(())
}
