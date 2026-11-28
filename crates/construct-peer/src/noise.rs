//! Noise IK handshake and encrypted `PeerLink` transport (#368).
//!
//! Pattern: `Noise_IK_25519_ChaChaPoly_SHA256` with prologue `construct-peer-v1`.
//! Ring backs AEAD, SHA-256, and CSPRNG via `snow::resolvers::RingResolver`;
//! `DefaultResolver` supplies static X25519 DH via `curve25519-dalek` because
//! `ring::agreement` is ephemeral-only.
//!
//! Anti-replay / KCI defense: Noise IK message 1 (`-> e, es, s, ss`) gives the
//! responder the initiator's static key, but a replayed message 1 still
//! decrypts. The responder therefore requires a valid encrypted `Control::Hello`
//! frame after message 2 (`<- e, ee, se`) before reporting the peer as live or
//! pending. Only an initiator holding the ephemeral private key for `e` can
//! derive the transport keys and produce that `Hello`.

use crate::chunk::{Reassembler, split_chunks};
use crate::crypto::{NOISE_PROLOGUE, noise_builder};
use crate::envelope::{Control, Envelope, Hello, NOISE_MAX_FRAME, PINNED_MAX_ENVELOPE, PRE_PIN_MAX_ENVELOPE};
use crate::error::WireError;
use crate::frame::FrameStream;
use crate::keys::{HandshakeHash, Keypair, PublicKey};
use snow::{HandshakeState, TransportState};
use std::time::Duration;

/// Default timeout for completing the Noise IK handshake + 1-RTT Hello.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Decision returned by the responder's static-key callback after Noise IK
/// message 1 decrypts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyDecision {
    /// Proceed with message 2 and wait for 1-RTT `Hello` at the pre-pin
    /// envelope cap (`PRE_PIN_MAX_ENVELOPE`). A `pinned` key's link is raised
    /// to `PINNED_MAX_ENVELOPE` once its `Hello` arrives; any other stays at
    /// the pre-pin cap until the hub calls `PeerLink::pin`.
    Accept { pinned: bool },
    /// Reject this static key immediately before sending message 2.
    Reject(String),
}

/// Completed encrypted link after Noise IK + 1-RTT `Hello` confirmation.
pub struct PeerLink<S> {
    stream: S,
    transport: TransportState,
    remote_static: PublicKey,
    handshake_hash: HandshakeHash,
    reassembler: Reassembler,
    /// Scratch buffer for encrypting/decrypting one Noise frame (65,535 B).
    scratch: Vec<u8>,
}

impl<S: FrameStream> PeerLink<S> {
    /// Remote peer's authenticated X25519 static public key.
    #[must_use]
    pub const fn remote_static(&self) -> PublicKey {
        self.remote_static
    }

    /// Noise handshake hash (`h`), identical on both sides of the link.
    #[must_use]
    pub const fn handshake_hash(&self) -> HandshakeHash {
        self.handshake_hash
    }

    /// The peer is now pinned (admitted, or approved while pending): raise
    /// the incoming envelope cap to `PINNED_MAX_ENVELOPE`.
    pub fn pin(&mut self) {
        self.reassembler.pin();
    }

    /// Encrypt and send an envelope, chunking across Noise's 65,535-byte frame
    /// ceiling as needed.
    pub async fn send(&mut self, envelope: &Envelope) -> Result<(), WireError> {
        let chunks = split_chunks(envelope, PINNED_MAX_ENVELOPE)?;
        for chunk in chunks {
            self.send_plaintext(&chunk).await?;
        }
        Ok(())
    }

    /// Receive and decrypt the next complete envelope.
    pub async fn recv(&mut self) -> Result<Envelope, WireError> {
        loop {
            let plaintext = self.recv_plaintext().await?;
            if let Some(env) = self.reassembler.push(&plaintext)? {
                return Ok(env);
            }
        }
    }

    /// Send a `Control::Close` notice (best-effort) and close the transport.
    pub async fn close(&mut self, reason: Option<String>) -> Result<(), WireError> {
        if let Ok(env) = Envelope::control(&Control::Close { reason }) {
            let _ = self.send(&env).await;
        }
        self.stream.close().await
    }

    async fn send_plaintext(&mut self, plaintext: &[u8]) -> Result<(), WireError> {
        let n = self
            .transport
            .write_message(plaintext, &mut self.scratch)
            .map_err(|e| WireError::Crypto(format!("encrypt transport frame: {e}")))?;
        let frame = self.scratch.get(..n).ok_or_else(|| WireError::Crypto("encrypted length out of bounds".into()))?;
        self.stream.send_frame(frame).await
    }

    async fn recv_plaintext(&mut self) -> Result<Vec<u8>, WireError> {
        let ciphertext = self.stream.recv_frame().await?;
        let n = self
            .transport
            .read_message(&ciphertext, &mut self.scratch)
            .map_err(|e| WireError::Crypto(format!("decrypt transport frame: {e}")))?;
        let plain = self.scratch.get(..n).ok_or_else(|| WireError::Crypto("decrypted length out of bounds".into()))?;
        Ok(plain.to_vec())
    }
}

/// Initiator (peer) handshake: perform Noise IK against `remote_static`, then
/// immediately send the authenticated 1-RTT `Control::Hello`.
pub async fn handshake_initiator<S: FrameStream>(
    mut stream: S,
    local: &Keypair,
    remote_static: PublicKey,
    hello: &Hello,
    max_envelope_bytes: usize,
) -> Result<PeerLink<S>, WireError> {
    let mut state: HandshakeState = noise_builder()?
        .local_private_key(local.private_bytes())
        .map_err(|e| WireError::Crypto(format!("set local key: {e}")))?
        .remote_public_key(remote_static.as_bytes())
        .map_err(|e| WireError::Crypto(format!("set remote key: {e}")))?
        .prologue(NOISE_PROLOGUE)
        .map_err(|e| WireError::Crypto(format!("set prologue: {e}")))?
        .build_initiator()
        .map_err(|e| WireError::Crypto(format!("build initiator: {e}")))?;

    let mut buf = vec![0u8; NOISE_MAX_FRAME];

    // -> e, es, s, ss
    let n = state.write_message(&[], &mut buf).map_err(|e| WireError::Crypto(format!("write IK msg 1: {e}")))?;
    let msg1 = buf.get(..n).ok_or_else(|| WireError::Crypto("msg 1 length out of bounds".into()))?;
    stream.send_frame(msg1).await?;

    // <- e, ee, se
    let msg2 = stream.recv_frame().await?;
    state.read_message(&msg2, &mut buf).map_err(|e| WireError::Crypto(format!("read IK msg 2: {e}")))?;

    let hash = HandshakeHash::try_from_slice(state.get_handshake_hash())?;
    let transport = state.into_transport_mode().map_err(|e| WireError::Crypto(format!("enter transport mode: {e}")))?;

    let mut link = PeerLink {
        stream,
        transport,
        remote_static,
        handshake_hash: hash,
        reassembler: Reassembler::new(max_envelope_bytes),
        scratch: buf,
    };

    // 1-RTT proof of ephemeral key possession + peer metadata.
    let hello_env = Envelope::control(&Control::Hello(hello.clone()))?;
    link.send(&hello_env).await?;

    Ok(link)
}

/// Responder (hub) handshake: read Noise IK msg 1, check the initiator's static
/// key via `check_key`, send msg 2, and require a valid encrypted
/// `Control::Hello` before returning the live `PeerLink` and `Hello`.
///
/// Connections whose msg 1 fails AEAD (scanners without the hub's static public
/// key) return `Err(WireError::Crypto(_))` without sending anything back.
pub async fn handshake_responder<S, F>(
    mut stream: S,
    local: &Keypair,
    check_key: F,
) -> Result<(PeerLink<S>, Hello), WireError>
where
    S: FrameStream,
    F: FnOnce(PublicKey) -> KeyDecision,
{
    let mut state: HandshakeState = noise_builder()?
        .local_private_key(local.private_bytes())
        .map_err(|e| WireError::Crypto(format!("set local key: {e}")))?
        .prologue(NOISE_PROLOGUE)
        .map_err(|e| WireError::Crypto(format!("set prologue: {e}")))?
        .build_responder()
        .map_err(|e| WireError::Crypto(format!("build responder: {e}")))?;

    let mut buf = vec![0u8; NOISE_MAX_FRAME];

    // -> e, es, s, ss
    let msg1 = stream.recv_frame().await?;
    state.read_message(&msg1, &mut buf).map_err(|e| WireError::Crypto(format!("read IK msg 1: {e}")))?;

    let remote_static = match state.get_remote_static() {
        Some(rs) => PublicKey::try_from_slice(rs)?,
        None => return Err(WireError::Crypto("missing initiator static key".into())),
    };

    let pinned = match check_key(remote_static) {
        KeyDecision::Accept { pinned } => pinned,
        KeyDecision::Reject(reason) => return Err(WireError::Rejected(reason)),
    };

    // <- e, ee, se
    let n = state.write_message(&[], &mut buf).map_err(|e| WireError::Crypto(format!("write IK msg 2: {e}")))?;
    let msg2 = buf.get(..n).ok_or_else(|| WireError::Crypto("msg 2 length out of bounds".into()))?;
    stream.send_frame(msg2).await?;

    let hash = HandshakeHash::try_from_slice(state.get_handshake_hash())?;
    let transport = state.into_transport_mode().map_err(|e| WireError::Crypto(format!("enter transport mode: {e}")))?;

    // Enforce `PRE_PIN_MAX_ENVELOPE` while awaiting Hello so an unconfirmed
    // initiator cannot buffer a huge first message; a pinned key is raised to
    // the pinned cap immediately after Hello succeeds.
    let mut link = PeerLink {
        stream,
        transport,
        remote_static,
        handshake_hash: hash,
        reassembler: Reassembler::new(PRE_PIN_MAX_ENVELOPE),
        scratch: buf,
    };

    // Mandatory 1-RTT confirmation: a replayed msg 1 cannot decrypt msg 2 or
    // forge a valid encrypted transport frame here.
    let first = link.recv().await?;
    let hello = match first.decode_control() {
        Ok(Control::Hello(h)) => h,
        Ok(other) => {
            return Err(WireError::MissingHello(format!("expected Hello, received {other:?}")));
        }
        Err(e) => return Err(WireError::MissingHello(e.to_string())),
    };

    if pinned {
        link.pin();
    }
    Ok((link, hello))
}
