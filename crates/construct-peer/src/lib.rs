//! Wire protocol for construct-to-construct peers (#273, #368).
//!
//! Three layers shared by both sides (peer dialer and hub listener), testable
//! over an in-memory `FrameStream` without a network or a model:
//!
//! 1. **Transport (`frame`, `ws`)**: WebSocket over TCP (`ws://`) or TLS
//!    (`wss://`, reusing `construct_core::http::tls_client_config`, ADR 0053),
//!    plus length-prefixed framing over any `AsyncRead + AsyncWrite` stream.
//! 2. **Channel (`keys`, `noise`)**: `Noise_IK_25519_ChaChaPoly_SHA256` with
//!    ring AEAD, SHA-256, and CSPRNG plus a mandatory 1-RTT encrypted
//!    `Control::Hello` confirmation (anti-replay / KCI defense) and exposed
//!    handshake hash (`h`).
//! 3. **Envelopes (`envelope`, `keepalive`)**: `{channel, bytes}` chunked across
//!    Noise's 65,535-byte frame limit with strict pre-pin (4 KiB) and post-pin
//!    (16 MiB) reassembly caps, application-level `Ping`/`Pong` keep-alive, and
//!    jittered reconnect backoff.
//!
//! On top, the join flow's two shared pieces (#361): `keyfile` (a `0600`
//! static key on disk, for the hub and the peer alike) and, with `dial`,
//! `join` (the peer following the hub's verdict). For hub-proxied model
//! calls (#371): `llm` (the channel's frames, both sides) and, with `dial`,
//! `gateway` (the peer's loopback endpoint the engine is pointed at).

pub mod chunk;
pub mod crypto;
pub mod envelope;
pub mod error;
pub mod frame;
#[cfg(feature = "dial")]
pub mod gateway;
#[cfg(feature = "dial")]
pub mod join;
pub mod keepalive;
pub mod keyfile;
pub mod keys;
pub mod llm;
pub mod noise;
pub mod ws;

pub use crypto::{NOISE_PATTERN, NOISE_PROLOGUE};
pub use envelope::{
    Channel, Control, Envelope, Hello, NOISE_MAX_FRAME, NOISE_MAX_PLAINTEXT, PINNED_MAX_ENVELOPE, PRE_PIN_MAX_ENVELOPE,
    PROTOCOL_VERSION,
};
pub use error::WireError;
pub use frame::{FrameStream, LengthPrefixed};
pub use keepalive::{Backoff, KeepAlive, KeepAliveState, connect_with_backoff, recv_with_keepalive};
pub use keys::{HASH_LEN, HandshakeHash, KEY_LEN, Keypair, PublicKey, random_token};
pub use noise::{DEFAULT_HANDSHAKE_TIMEOUT, KeyDecision, PeerLink, handshake_initiator, handshake_responder};
pub use ws::WsFrames;
#[cfg(feature = "accept")]
pub use ws::accept_ws;
#[cfg(feature = "dial")]
pub use ws::{connect_ws, connect_ws_with_tls};
