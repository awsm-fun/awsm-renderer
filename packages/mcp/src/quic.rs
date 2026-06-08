//! The WebTransport (QUIC) listener: accept the editor's outbound connection,
//! complete the WebTransport handshake, and store the session as the active
//! editor link.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use web_transport::quinn::quinn::{
    crypto::rustls::QuicServerConfig, Endpoint, Incoming, ServerConfig,
};
use web_transport::Session;

use awsm_editor_protocol::Request;

use crate::cert::GeneratedCert;
use crate::link::{self, EditorLink};

/// Build a WebTransport-capable QUIC server endpoint bound to `addr`.
pub fn build_endpoint(cert: &GeneratedCert, addr: SocketAddr) -> Result<Endpoint> {
    let mut tls = rustls::ServerConfig::builder_with_provider(
        web_transport::quinn::crypto::default_provider(),
    )
    .with_protocol_versions(&[&rustls::version::TLS13])
    .context("set TLS 1.3")?
    .with_no_client_auth()
    .with_single_cert(vec![cert.rustls_cert()], cert.rustls_key())
    .context("install single cert")?;
    tls.alpn_protocols = vec![web_transport::quinn::ALPN.as_bytes().to_vec()];
    tls.max_early_data_size = u32::MAX;

    let qsc: QuicServerConfig = tls.try_into().context("build QUIC server config")?;
    let server_config = ServerConfig::with_crypto(Arc::new(qsc));

    let endpoint = Endpoint::server(server_config, addr).context("bind QUIC endpoint")?;
    Ok(endpoint)
}

/// Accept editor connections forever, installing each as the active link.
pub async fn accept_loop(endpoint: Endpoint, link: EditorLink) {
    loop {
        let Some(incoming) = endpoint.accept().await else {
            tracing::warn!("QUIC endpoint closed");
            break;
        };
        let link = link.clone();
        tokio::spawn(async move {
            match accept_session(incoming).await {
                Ok(session) => {
                    tracing::info!("editor attached");
                    link.set(Some(session.clone())).await;
                    // Phase-2 gate: prove the round-trip by asking the editor its
                    // mode the moment it attaches.
                    match link::request(&session, &Request::Mode).await {
                        Ok(resp) => tracing::info!("mode round-trip ok: {resp:?}"),
                        Err(e) => tracing::warn!("mode round-trip failed: {e}"),
                    }
                }
                Err(e) => tracing::error!("accept failed: {e:#}"),
            }
        });
    }
}

async fn accept_session(incoming: Incoming) -> Result<Session> {
    let conn = incoming.await.context("await incoming connection")?;
    let req = web_transport::quinn::Request::accept(conn)
        .await
        .context("WebTransport handshake")?;
    let session = req.ok().await.context("WebTransport session")?;
    Ok(session.into())
}
