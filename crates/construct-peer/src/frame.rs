//! Low-level frame stream abstraction (#368).
//!
//! Noise messages are discrete datagrams (≤65,535 bytes each). Framing over a
//! `FrameStream` trait lets contract tests drive the entire protocol over an
//! in-memory `tokio::io::DuplexStream` while production runs over WebSocket
//! binary frames without double-framing.

use crate::envelope::NOISE_MAX_FRAME;
use crate::error::WireError;
use std::future::Future;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// A bidirectional transport carrying discrete binary frames (each ≤65,535 B).
pub trait FrameStream: Send + Unpin {
    /// Send one frame atomically.
    fn send_frame(&mut self, frame: &[u8]) -> impl Future<Output = Result<(), WireError>> + Send;

    /// Receive the next frame, returning `Err(WireError::Closed)` on clean EOF.
    fn recv_frame(&mut self) -> impl Future<Output = Result<Vec<u8>, WireError>> + Send;

    /// Close the underlying transport cleanly.
    fn close(&mut self) -> impl Future<Output = Result<(), WireError>> + Send;
}

/// Length-prefixed (`u16` big-endian) `FrameStream` over any Tokio byte stream.
///
/// Matches the Noise spec's 2-byte length prefix so tests can run the wire
/// over `tokio::io::duplex` (or raw TCP/Unix sockets) with zero extra deps.
#[derive(Debug)]
pub struct LengthPrefixed<S> {
    inner: S,
}

impl<S> LengthPrefixed<S> {
    #[must_use]
    pub fn new(inner: S) -> Self {
        Self { inner }
    }

    /// Consume the adapter and return the underlying stream.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S> FrameStream for LengthPrefixed<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    async fn send_frame(&mut self, frame: &[u8]) -> Result<(), WireError> {
        let len = u16::try_from(frame.len())
            .map_err(|_| WireError::PayloadTooLarge { len: frame.len(), limit: NOISE_MAX_FRAME })?;
        self.inner.write_all(&len.to_be_bytes()).await?;
        self.inner.write_all(frame).await?;
        self.inner.flush().await?;
        Ok(())
    }

    async fn recv_frame(&mut self) -> Result<Vec<u8>, WireError> {
        let mut hdr = [0u8; 2];
        match self.inner.read_exact(&mut hdr).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(WireError::Closed);
            }
            Err(e) => return Err(WireError::Io(e)),
        }
        let len = u16::from_be_bytes(hdr) as usize;
        if len == 0 {
            return Err(WireError::Protocol("zero-length noise frame".into()));
        }
        let mut buf = vec![0u8; len];
        self.inner.read_exact(&mut buf).await?;
        Ok(buf)
    }

    async fn close(&mut self) -> Result<(), WireError> {
        self.inner.shutdown().await?;
        Ok(())
    }
}
