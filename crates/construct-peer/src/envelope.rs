//! Logical channels, envelopes, and control-channel messages (#368).
//!
//! Envelopes are multiplexed by a single-byte `Channel` tag (`acp`, `control`,
//! `llm`) and chunked (`chunk.rs`) across Noise's 65,535-byte transport frame
//! limit. Keep this vocabulary strictly outside the ACP contract: `acp`
//! carries opaque JSON-RPC bytes, while `control` carries peer lifecycle
//! messages (`Hello`, ping/pong, close, and join/revoke placeholders for #361).

use crate::error::WireError;
use serde::{Deserialize, Serialize};

/// Noise protocol ceiling for a single transport message (2-byte length in the
/// Noise spec, including the 16-byte AEAD tag).
pub const NOISE_MAX_FRAME: usize = 65_535;

/// AEAD authentication tag length for ChaChaPoly (Poly1305 = 16 bytes).
pub const NOISE_TAG_LEN: usize = 16;

/// Maximum plaintext bytes inside one Noise transport frame.
pub const NOISE_MAX_PLAINTEXT: usize = NOISE_MAX_FRAME - NOISE_TAG_LEN;

/// Maximum envelope payload bytes carried by a single Noise frame (leaving
/// room for the 10-byte chunk header).
pub const MAX_CHUNK_PAYLOAD: usize = NOISE_MAX_PLAINTEXT - 10;

/// Default reassembled envelope cap before the peer is pinned (anti-OOM).
pub const PRE_PIN_MAX_ENVELOPE: usize = 4 * 1024;

/// Default reassembled envelope cap once the peer is pinned (large ACP tool
/// results and `session/load` replays).
pub const PINNED_MAX_ENVELOPE: usize = 16 * 1024 * 1024;

/// Protocol version carried in `Hello`.
pub const PROTOCOL_VERSION: u16 = 1;

/// Logical channel inside an encrypted peer connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Channel {
    /// ACP JSON-RPC stream (agent protocol).
    Acp = 1,
    /// Link lifecycle: Hello confirmation, keep-alive ping/pong, join/revoke, close.
    Control = 2,
    /// Hub-proxied `/chat/completions` requests and SSE chunks (`llm = "hub"`, #371).
    Llm = 3,
}

impl Channel {
    /// Decode a wire channel byte.
    pub fn from_byte(b: u8) -> Result<Self, WireError> {
        match b {
            1 => Ok(Self::Acp),
            2 => Ok(Self::Control),
            3 => Ok(Self::Llm),
            other => Err(WireError::Protocol(format!("unknown channel tag {other}"))),
        }
    }

    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self as u8
    }
}

/// One reassembled logical message on a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub channel: Channel,
    pub bytes: Vec<u8>,
}

impl Envelope {
    #[must_use]
    pub fn new(channel: Channel, bytes: Vec<u8>) -> Self {
        Self { channel, bytes }
    }

    /// Encode a `Control` message as a JSON envelope on `Channel::Control`.
    pub fn control(msg: &Control) -> Result<Self, WireError> {
        let bytes = serde_json::to_vec(msg).map_err(|e| WireError::Protocol(format!("encode control message: {e}")))?;
        Ok(Self { channel: Channel::Control, bytes })
    }

    /// Decode a `Control` envelope.
    pub fn decode_control(&self) -> Result<Control, WireError> {
        if self.channel != Channel::Control {
            return Err(WireError::Protocol(format!("expected control channel, got {:?}", self.channel)));
        }
        serde_json::from_slice(&self.bytes).map_err(|e| WireError::Protocol(format!("decode control message: {e}")))
    }
}

/// Messages carried on `Channel::Control`.
///
/// `Hello` is mandatory as the first transport message from the initiator
/// after Noise IK message 2 so the responder gets 1-RTT confirmation of the
/// initiator's ephemeral key before exposing the peer to upper layers. The
/// join flow (#361, ADR 0070) answers it with exactly one verdict: `JoinAck
/// { accepted: true }` (pinned, now or before), `Pending` (shown at the hub
/// with the handshake's SAS code; a `JoinAck` follows), or a `Close` with the
/// reason. A join token rides in `Hello`, inside the encrypted channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    /// First encrypted transport message from initiator (1-RTT confirmation).
    Hello(Hello),
    /// Keep-alive probe carrying a caller-chosen nonce echoed in `Pong`.
    Ping { nonce: u64 },
    /// Keep-alive response echoing the peer's `Ping` nonce.
    Pong { nonce: u64 },
    /// Graceful shutdown notice with an optional human-readable reason.
    Close {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The hub holds this join for a human: the peer shows its SAS code
    /// (`HandshakeHash::sas_code`) and waits for a `JoinAck`.
    Pending,
    /// The hub's verdict: accepted means this key is pinned for the peer id;
    /// refused is followed by a `Close`.
    JoinAck {
        accepted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The hub revoked this peer's key: the peer deletes it and stops.
    Revoke {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// Initiator metadata carried in the mandatory 1-RTT `Control::Hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub version: u16,
    pub peer_id: String,
    /// Optional single-use invite token (`construct hub invite`, #361).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_token: Option<String>,
}

impl Hello {
    #[must_use]
    pub fn new(peer_id: impl Into<String>) -> Self {
        Self { version: PROTOCOL_VERSION, peer_id: peer_id.into(), join_token: None }
    }

    #[must_use]
    pub fn with_join_token(mut self, token: impl Into<String>) -> Self {
        self.join_token = Some(token.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_hello_round_trip() {
        let msg = Control::Hello(Hello::new("old-pc").with_join_token("tok-123"));
        let env = Envelope::control(&msg).unwrap();
        assert_eq!(env.decode_control().unwrap(), msg);
    }
}
