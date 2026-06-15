//! The `/editor` WebSocket: one socket per editor tab. A single writer task owns
//! the sink (so concurrent replies/events never interleave a half-written frame);
//! the reader loop demuxes responses and push events. Frames are JSON text.

use axum::extract::ws::{Message, Utf8Bytes, WebSocket};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use awsm_editor_protocol::{WsClientMsg, WsServerMsg};

use crate::link::EditorLink;

pub async fn handle_socket(socket: WebSocket, link: EditorLink) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<WsServerMsg>();
    let conn = link.register_connection(tx);
    tracing::info!("editor attached (connection {})", conn.id);

    // The sole writer for this socket.
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let Ok(txt) = serde_json::to_string(&msg) else {
                continue;
            };
            if sink
                .send(Message::Text(Utf8Bytes::from(txt)))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    while let Some(frame) = stream.next().await {
        let msg = match frame {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("editor ws read error (connection {}): {e}", conn.id);
                break;
            }
        };
        match msg {
            Message::Text(txt) => match serde_json::from_str::<WsClientMsg>(txt.as_str()) {
                Ok(WsClientMsg::Response { id, resp }) => conn.complete(id, resp),
                Ok(WsClientMsg::Event(ev)) => link.publish_event(ev),
                Err(e) => tracing::warn!("connection {}: bad ws frame: {e}", conn.id),
            },
            Message::Close(_) => break,
            _ => {}
        }
    }

    link.remove_connection(conn.id);
    writer.abort();
    tracing::info!("editor detached (connection {})", conn.id);
}
