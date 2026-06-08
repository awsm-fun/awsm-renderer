//! The link to the attached editor + the per-request stream exchange.
//!
//! The server holds at most one attached editor `Session`. Each request opens a
//! fresh server-initiated bidirectional stream, writes the bitcode-encoded
//! [`Request`] and `finish()`es, then reads the editor's [`Response`] to end.
//! Stream identity is the request/response correlation; framing is by
//! stream-finish.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::Mutex;
use web_transport::{RecvStream, SendStream, Session};

use awsm_editor_protocol::{Request, Response};

/// Cap on a single response (PNGs are the large case). 64 MiB is far above any
/// legitimate payload and bounds memory if a peer streams without finishing.
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Shared handle to the (single) attached editor session.
#[derive(Clone)]
pub struct EditorLink {
    inner: Arc<Mutex<Option<Session>>>,
}

impl EditorLink {
    pub fn shared() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn set(&self, session: Option<Session>) {
        *self.inner.lock().await = session;
    }

    pub async fn session(&self) -> Option<Session> {
        self.inner.lock().await.clone()
    }

    /// Send a request to the attached editor and await its response. Errors when
    /// no editor is attached. (Used by the rmcp tool layer + the test client.)
    #[allow(dead_code)]
    pub async fn request(&self, req: &Request) -> Result<Response> {
        let session = self
            .session()
            .await
            .ok_or_else(|| anyhow!("no editor attached"))?;
        request(&session, req).await
    }
}

/// Run one request/response exchange over a fresh bidirectional stream.
pub async fn request(session: &Session, req: &Request) -> Result<Response> {
    let (mut send, mut recv) = session
        .clone()
        .open_bi()
        .await
        .map_err(|e| anyhow!("open_bi: {e}"))?;

    let bytes = serde_json::to_vec(req).map_err(|e| anyhow!("encode request: {e}"))?;
    write_all(&mut send, &bytes).await?;
    send.finish().map_err(|e| anyhow!("finish: {e}"))?;

    let resp_bytes = read_to_end(&mut recv).await?;
    let resp: Response =
        serde_json::from_slice(&resp_bytes).map_err(|e| anyhow!("decode response: {e}"))?;
    Ok(resp)
}

async fn write_all(send: &mut SendStream, mut buf: &[u8]) -> Result<()> {
    while !buf.is_empty() {
        let n = send.write(buf).await.map_err(|e| anyhow!("write: {e}"))?;
        buf = &buf[n..];
    }
    Ok(())
}

async fn read_to_end(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    while let Some(chunk) = recv
        .read(64 * 1024)
        .await
        .map_err(|e| anyhow!("read: {e}"))?
    {
        buf.extend_from_slice(&chunk);
        if buf.len() > MAX_RESPONSE_BYTES {
            return Err(anyhow!("response exceeded {MAX_RESPONSE_BYTES} bytes"));
        }
    }
    Ok(buf)
}
