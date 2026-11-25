//! WebSocket `FrameStream` transport over TCP (`ws://`) and TLS (`wss://`) (#368).
//!
//! Binary WebSocket messages map 1-to-1 to Noise frames (≤65,535 bytes). TLS
//! connections reuse `construct_core::http::tls_client_config()` (ring crypto
//! provider + platform store merged with `CONSTRUCT_CA_FILE` and baked musl
//! roots, ADR 0053) so peer dialing follows the exact same trust policy as
//! model API requests.

use crate::envelope::NOISE_MAX_FRAME;
use crate::error::WireError;
use crate::frame::FrameStream;
use futures::{SinkExt, StreamExt};
#[cfg(feature = "dial")]
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
#[cfg(feature = "dial")]
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// A WebSocket connection adapted to [`FrameStream`].
///
/// WebSocket control frames (ping/pong) are handled transparently while waiting
/// for the next binary Noise frame; text frames are rejected as protocol errors.
pub struct WsFrames<S> {
    ws: WebSocketStream<S>,
}

impl<S> WsFrames<S> {
    #[must_use]
    pub fn new(ws: WebSocketStream<S>) -> Self {
        Self { ws }
    }
}

/// WebSocket framing limits matched to the Noise frame ceiling so an
/// unauthenticated peer cannot buffer megabytes inside tungstenite before
/// Noise even runs.
#[must_use]
pub fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default().max_message_size(Some(NOISE_MAX_FRAME)).max_frame_size(Some(NOISE_MAX_FRAME))
}

impl<S> FrameStream for WsFrames<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    async fn send_frame(&mut self, frame: &[u8]) -> Result<(), WireError> {
        if frame.len() > NOISE_MAX_FRAME {
            return Err(WireError::PayloadTooLarge { len: frame.len(), limit: NOISE_MAX_FRAME });
        }
        self.ws
            .send(Message::Binary(frame.to_vec().into()))
            .await
            .map_err(|e| WireError::Transport(format!("ws send: {e}")))
    }

    async fn recv_frame(&mut self) -> Result<Vec<u8>, WireError> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Binary(data))) => {
                    if data.is_empty() {
                        return Err(WireError::Protocol("empty ws binary frame".into()));
                    }
                    if data.len() > NOISE_MAX_FRAME {
                        return Err(WireError::Protocol(format!(
                            "ws binary frame {} exceeds noise max {NOISE_MAX_FRAME}",
                            data.len()
                        )));
                    }
                    return Ok(data.to_vec());
                }
                Some(Ok(Message::Ping(payload))) => {
                    let _ = self.ws.send(Message::Pong(payload)).await;
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => return Err(WireError::Closed),
                Some(Ok(Message::Text(_))) => {
                    return Err(WireError::Protocol("unexpected text websocket frame on binary peer wire".into()));
                }
                Some(Ok(Message::Frame(_))) => {}
                Some(Err(tokio_tungstenite::tungstenite::Error::ConnectionClosed))
                | Some(Err(tokio_tungstenite::tungstenite::Error::AlreadyClosed)) => {
                    return Err(WireError::Closed);
                }
                Some(Err(e)) => return Err(WireError::Transport(format!("ws recv: {e}"))),
            }
        }
    }

    async fn close(&mut self) -> Result<(), WireError> {
        let _ = self.ws.close(None).await;
        Ok(())
    }
}

/// Dial a `ws://` or `wss://` URL using construct's shared TLS trust policy
/// (`construct_core::http::tls_client_config`, ADR 0053).
#[cfg(feature = "dial")]
pub async fn connect_ws(url: &str) -> Result<WsFrames<MaybeTlsStream<tokio::net::TcpStream>>, WireError> {
    let tls = if url.starts_with("wss://") {
        let cfg =
            construct_core::http::tls_client_config().map_err(|e| WireError::Transport(format!("tls config: {e}")))?;
        Some(cfg)
    } else if url.starts_with("ws://") {
        None
    } else {
        return Err(WireError::Transport(format!("unsupported peer URL scheme (expected ws:// or wss://): {url}")));
    };
    connect_ws_with_tls(url, tls).await
}

/// Dial a `ws://` or `wss://` URL with an explicit `rustls::ClientConfig`
/// (used by tests with an ephemeral loopback CA and by `connect_ws`).
#[cfg(feature = "dial")]
pub async fn connect_ws_with_tls(
    url: &str,
    tls_config: Option<Arc<rustls::ClientConfig>>,
) -> Result<WsFrames<MaybeTlsStream<tokio::net::TcpStream>>, WireError> {
    let connector = match tls_config {
        Some(cfg) => Some(tokio_tungstenite::Connector::Rustls(cfg)),
        None => Some(tokio_tungstenite::Connector::Plain),
    };
    let (ws, _resp) = tokio_tungstenite::connect_async_tls_with_config(url, Some(ws_config()), true, connector)
        .await
        .map_err(|e| WireError::Transport(format!("ws connect {url}: {e}")))?;
    Ok(WsFrames::new(ws))
}

/// Upgrade an accepted stream (plain TCP or TLS-wrapped `tokio_rustls::server::TlsStream`)
/// to a server-side WebSocket `FrameStream`.
#[cfg(feature = "accept")]
pub async fn accept_ws<S>(stream: S) -> Result<WsFrames<S>, WireError>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    let ws = tokio_tungstenite::accept_async_with_config(stream, Some(ws_config()))
        .await
        .map_err(|e| WireError::Transport(format!("ws accept: {e}")))?;
    Ok(WsFrames::new(ws))
}
