//! Chunking and reassembly of envelopes across Noise's 65,535-byte frame ceiling (#368).
//!
//! Each chunk carries a 10-byte big-endian header:
//! `[channel: u8][flags: u8][seq: u32 BE][total_len: u32 BE]`.
//! The reassembler enforces the configured byte cap (`PRE_PIN_MAX_ENVELOPE`
//! before pinning, `PINNED_MAX_ENVELOPE` after) before allocating, preventing
//! unauthenticated OOM from spoofed lengths or unbounded streams.

use crate::envelope::{Channel, Envelope, MAX_CHUNK_PAYLOAD, NOISE_MAX_PLAINTEXT};
use crate::error::WireError;

/// Chunk header size inside each Noise plaintext frame.
pub const CHUNK_HEADER_LEN: usize = 10;

const FLAG_FINAL: u8 = 0x01;

/// Split an envelope into Noise-plaintext chunks, each at most
/// `NOISE_MAX_PLAINTEXT` bytes. Empty payloads emit one empty final chunk.
pub(crate) fn split_chunks(envelope: &Envelope, limit: usize) -> Result<Vec<Vec<u8>>, WireError> {
    let total = envelope.bytes.len();
    if total > limit {
        return Err(WireError::PayloadTooLarge { len: total, limit });
    }
    let total_u32 = u32::try_from(total).map_err(|_| WireError::PayloadTooLarge { len: total, limit })?;
    let mut out = Vec::new();
    if total == 0 {
        let mut buf = Vec::with_capacity(CHUNK_HEADER_LEN);
        write_header(&mut buf, envelope.channel, FLAG_FINAL, 0, 0);
        out.push(buf);
        return Ok(out);
    }
    let chunks = envelope.bytes.chunks(MAX_CHUNK_PAYLOAD);
    let count = chunks.len();
    for (idx, slice) in chunks.enumerate() {
        let flags = if idx + 1 == count { FLAG_FINAL } else { 0 };
        let seq = u32::try_from(idx).map_err(|_| WireError::PayloadTooLarge { len: total, limit })?;
        let mut buf = Vec::with_capacity(CHUNK_HEADER_LEN + slice.len());
        write_header(&mut buf, envelope.channel, flags, seq, total_u32);
        buf.extend_from_slice(slice);
        debug_assert!(buf.len() <= NOISE_MAX_PLAINTEXT);
        out.push(buf);
    }
    Ok(out)
}

fn write_header(out: &mut Vec<u8>, channel: Channel, flags: u8, seq: u32, total_len: u32) {
    out.push(channel.as_byte());
    out.push(flags);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&total_len.to_be_bytes());
}

/// Stateful reassembler for one in-flight chunked envelope.
#[derive(Debug)]
pub(crate) struct Reassembler {
    limit: usize,
    active: Option<InFlight>,
}

#[derive(Debug)]
struct InFlight {
    channel: Channel,
    next_seq: u32,
    expected_len: usize,
    buf: Vec<u8>,
}

impl Reassembler {
    #[must_use]
    pub(crate) fn new(limit: usize) -> Self {
        Self { limit, active: None }
    }

    pub(crate) fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
    }

    #[must_use]
    pub(crate) fn limit(&self) -> usize {
        self.limit
    }

    /// Feed one decrypted Noise plaintext frame. Returns `Ok(Some(envelope))`
    /// on the final chunk, `Ok(None)` when more chunks are expected.
    pub(crate) fn push(&mut self, frame: &[u8]) -> Result<Option<Envelope>, WireError> {
        let header = frame
            .get(..CHUNK_HEADER_LEN)
            .ok_or_else(|| WireError::Protocol(format!("chunk shorter than {CHUNK_HEADER_LEN}-byte header")))?;
        let payload =
            frame.get(CHUNK_HEADER_LEN..).ok_or_else(|| WireError::Protocol("missing chunk payload slice".into()))?;
        let channel = Channel::from_byte(header[0])?;
        let flags = header[1];
        if flags & !FLAG_FINAL != 0 {
            return Err(WireError::Protocol(format!("unknown chunk flags {flags:#x}")));
        }
        let seq = u32::from_be_bytes([header[2], header[3], header[4], header[5]]);
        let declared = u32::from_be_bytes([header[6], header[7], header[8], header[9]]) as usize;
        let is_final = flags & FLAG_FINAL != 0;

        if declared > self.limit {
            self.active = None;
            return Err(WireError::EnvelopeTooLarge { declared, limit: self.limit });
        }

        let state = match self.active.as_mut() {
            None => {
                if seq != 0 {
                    return Err(WireError::Protocol(format!("first chunk must have seq 0, got {seq}")));
                }
                let initial_cap = declared.min(payload.len().max(4096));
                self.active = Some(InFlight {
                    channel,
                    next_seq: 0,
                    expected_len: declared,
                    buf: Vec::with_capacity(initial_cap),
                });
                match self.active.as_mut() {
                    Some(s) => s,
                    None => unreachable!("just inserted"),
                }
            }
            Some(s) => {
                if s.channel != channel {
                    self.active = None;
                    return Err(WireError::Protocol("interleaved chunk from different channel".into()));
                }
                if s.expected_len != declared {
                    self.active = None;
                    return Err(WireError::Protocol("chunk declared length changed mid-envelope".into()));
                }
                s
            }
        };

        if seq != state.next_seq {
            let expected = state.next_seq;
            self.active = None;
            return Err(WireError::Protocol(format!("out-of-order chunk seq {seq}, expected {expected}")));
        }

        let new_len = state.buf.len().saturating_add(payload.len());
        if new_len > self.limit {
            self.active = None;
            return Err(WireError::EnvelopeTooLarge { declared: new_len, limit: self.limit });
        }
        if new_len > state.expected_len {
            let expected = state.expected_len;
            self.active = None;
            return Err(WireError::Protocol(format!(
                "chunk bytes {new_len} exceed declared envelope length {expected}"
            )));
        }
        state.buf.extend_from_slice(payload);
        state.next_seq =
            state.next_seq.checked_add(1).ok_or_else(|| WireError::Protocol("chunk sequence overflow".into()))?;

        if is_final {
            let finished = match self.active.take() {
                Some(s) => s,
                None => unreachable!("active state present"),
            };
            if finished.buf.len() != finished.expected_len {
                return Err(WireError::Protocol(format!(
                    "final chunk length {} != declared {}",
                    finished.buf.len(),
                    finished.expected_len
                )));
            }
            Ok(Some(Envelope { channel: finished.channel, bytes: finished.buf }))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{PINNED_MAX_ENVELOPE, PRE_PIN_MAX_ENVELOPE};

    #[test]
    fn empty_and_multi_chunk_round_trip() {
        for len in [0usize, 1, MAX_CHUNK_PAYLOAD, MAX_CHUNK_PAYLOAD + 1, 200_000] {
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let env = Envelope::new(Channel::Acp, payload);
            let chunks = split_chunks(&env, PINNED_MAX_ENVELOPE).unwrap();
            assert!(!chunks.is_empty());
            for c in &chunks {
                assert!(c.len() <= NOISE_MAX_PLAINTEXT);
            }
            let mut r = Reassembler::new(PINNED_MAX_ENVELOPE);
            let mut got = None;
            for c in &chunks {
                assert!(got.is_none());
                got = r.push(c).unwrap();
            }
            assert_eq!(got, Some(env));
        }
    }

    #[test]
    fn oversized_declared_envelope_rejected_before_buffering() {
        let env = Envelope::new(Channel::Control, vec![0u8; PRE_PIN_MAX_ENVELOPE + 1]);
        let chunks = split_chunks(&env, PINNED_MAX_ENVELOPE).unwrap();
        let mut r = Reassembler::new(PRE_PIN_MAX_ENVELOPE);
        let err = r.push(&chunks[0]).unwrap_err();
        assert!(matches!(err, WireError::EnvelopeTooLarge { .. }), "{err}");
    }
}
