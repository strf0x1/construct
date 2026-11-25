//! Wire errors for the construct peer protocol (#368).
//!
//! Each failure mode a caller or reconnect loop distinguishes has its own
//! variant: authentication/tampering failures are never confused with clean
//! closes or transient IO drops.

use std::fmt;
use std::io;

/// Everything that can fail on a peer wire connection.
#[derive(Debug)]
pub enum WireError {
    /// Underlying stream or WebSocket transport I/O failure.
    Io(io::Error),
    /// Remote closed the link cleanly before a message completed.
    Closed,
    /// Noise handshake or transport AEAD failure (wrong key, tampered frame,
    /// or replayed handshake message).
    Crypto(String),
    /// The responder's peer-key check rejected the initiator's static key.
    Rejected(String),
    /// Malformed frame header, unknown channel, bad chunk index, or invalid
    /// control payload.
    Protocol(String),
    /// Incoming envelope exceeded the active reassembly cap (4 KiB pre-pin,
    /// 16 MiB post-pin).
    EnvelopeTooLarge { declared: usize, limit: usize },
    /// Outgoing envelope exceeded the maximum representable payload size.
    PayloadTooLarge { len: usize, limit: usize },
    /// Expected a `Control::Hello` as the first transport message after the
    /// Noise IK handshake (anti-replay / KCI 1-RTT confirmation).
    MissingHello(String),
    /// Operation exceeded its deadline (handshake, Hello, or keep-alive pong).
    Timeout(&'static str),
    /// WebSocket URL, TLS setup, or HTTP upgrade failure.
    Transport(String),
}

impl WireError {
    /// Whether an initiator should drop the socket silently without sending an
    /// error frame (e.g., failed Noise msg 1 AEAD from an unauthenticated
    /// scanner).
    #[must_use]
    pub fn is_silent_drop(&self) -> bool {
        matches!(self, Self::Crypto(_) | Self::EnvelopeTooLarge { .. })
    }
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "wire io error: {e}"),
            Self::Closed => f.write_str("wire closed"),
            Self::Crypto(m) => write!(f, "noise crypto error: {m}"),
            Self::Rejected(m) => write!(f, "peer rejected: {m}"),
            Self::Protocol(m) => write!(f, "wire protocol error: {m}"),
            Self::EnvelopeTooLarge { declared, limit } => {
                write!(f, "envelope size {declared} exceeds limit {limit}")
            }
            Self::PayloadTooLarge { len, limit } => {
                write!(f, "payload size {len} exceeds limit {limit}")
            }
            Self::MissingHello(m) => write!(f, "1-RTT Hello confirmation failed: {m}"),
            Self::Timeout(what) => write!(f, "wire timeout waiting for {what}"),
            Self::Transport(m) => write!(f, "transport error: {m}"),
        }
    }
}

impl std::error::Error for WireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for WireError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof { Self::Closed } else { Self::Io(e) }
    }
}
