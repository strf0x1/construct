//! The `llm` channel's vocabulary (#371): a peer with `llm = "hub"` sends its
//! `/chat/completions` bodies to the hub, and the hub streams the upstream
//! response back, so no API key ever sits on the peer.
//!
//! The design choice: an envelope on this channel is one JSON header line,
//! `\n`, then raw bytes (the request body, or a slice of the response). The
//! response is relayed as the bytes the provider sent, status first, so the
//! peer's ordinary client reads it exactly as it would read the provider:
//! its retries, deadlines, SSE parser and error messages all stay as they
//! are. Each call has an id the peer picks; one link can carry several.

use crate::envelope::{Channel, Envelope};
use crate::error::WireError;
use serde::{Deserialize, Serialize};

/// One message on the `llm` channel; its bytes travel beside it in `Frame`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Msg {
    /// Peer to hub: a `/chat/completions` body follows.
    Request { id: u64 },
    /// Peer to hub: the caller gave up on call `id`; stop it upstream.
    Cancel { id: u64 },
    /// Hub to peer: the response status (and `Retry-After`, if any).
    Head {
        id: u64,
        status: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after: Option<u64>,
    },
    /// Hub to peer: the next slice of the response body follows.
    Data { id: u64 },
    /// Hub to peer: the response is over. An error means it was cut off and
    /// must read as an interrupted stream, never as a complete one.
    End {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl Msg {
    #[must_use]
    pub fn id(&self) -> u64 {
        match self {
            Self::Request { id }
            | Self::Cancel { id }
            | Self::Head { id, .. }
            | Self::Data { id }
            | Self::End { id, .. } => *id,
        }
    }
}

/// A message and the bytes it carries (empty for `Cancel`, `Head`, `End`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub msg: Msg,
    pub bytes: Vec<u8>,
}

impl Frame {
    #[must_use]
    pub fn new(msg: Msg, bytes: Vec<u8>) -> Self {
        Self { msg, bytes }
    }

    #[must_use]
    pub fn bare(msg: Msg) -> Self {
        Self { msg, bytes: Vec::new() }
    }

    /// This frame as an envelope on `Channel::Llm`.
    pub fn encode(&self) -> Result<Envelope, WireError> {
        let mut out =
            serde_json::to_vec(&self.msg).map_err(|e| WireError::Protocol(format!("encode llm frame: {e}")))?;
        out.push(b'\n');
        out.extend_from_slice(&self.bytes);
        Ok(Envelope::new(Channel::Llm, out))
    }

    /// Read an `llm` envelope back.
    pub fn decode(env: &Envelope) -> Result<Self, WireError> {
        if env.channel != Channel::Llm {
            return Err(WireError::Protocol(format!("expected llm channel, got {:?}", env.channel)));
        }
        let split = env
            .bytes
            .iter()
            .position(|&b| b == b'\n')
            .ok_or_else(|| WireError::Protocol("llm frame has no header line".into()))?;
        let (head, rest) = env.bytes.split_at(split);
        let msg = serde_json::from_slice(head).map_err(|e| WireError::Protocol(format!("decode llm frame: {e}")))?;
        Ok(Self { msg, bytes: rest.get(1..).unwrap_or_default().to_vec() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_with_raw_bytes_including_newlines() {
        let body = b"data: {\"a\":1}\n\ndata: [DONE]\n\n".to_vec();
        for f in [
            Frame::new(Msg::Request { id: 7 }, b"{\"model\":\"m\"}".to_vec()),
            Frame::new(Msg::Data { id: 7 }, body),
            Frame::bare(Msg::Head { id: 7, status: 429, retry_after: Some(2) }),
            Frame::bare(Msg::End { id: 7, error: Some("cut".into()) }),
            Frame::bare(Msg::Cancel { id: 7 }),
        ] {
            let env = f.encode().unwrap();
            assert_eq!(env.channel, Channel::Llm);
            assert_eq!(Frame::decode(&env).unwrap(), f);
            assert_eq!(f.msg.id(), 7);
        }
    }

    #[test]
    fn a_frame_without_a_header_line_is_refused() {
        assert!(Frame::decode(&Envelope::new(Channel::Llm, b"{}".to_vec())).is_err());
        assert!(Frame::decode(&Envelope::new(Channel::Acp, b"{}\n".to_vec())).is_err());
    }
}
