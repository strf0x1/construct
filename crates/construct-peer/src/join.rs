//! The peer's side of joining a hub (#361): dial, Noise IK + `Hello` (with
//! the join token, if any, inside the encrypted channel), then follow the
//! hub's verdict until the link ends.
//!
//! The design choice: the peer never decides trust. It reports what the hub
//! said as `Event`s (joined, or waiting with the SAS code a human compares at
//! the hub) and ends with an `Outcome`; the caller prints them and, on
//! `Revoked`, deletes the key. There is nothing to type back on the peer.

use crate::envelope::{Channel, Control, Hello, PINNED_MAX_ENVELOPE};
use crate::error::WireError;
use crate::keepalive::{KeepAlive, KeepAliveState, recv_with_keepalive};
use crate::keys::{Keypair, PublicKey};
use crate::noise::{DEFAULT_HANDSHAKE_TIMEOUT, handshake_initiator};
use crate::ws::connect_ws;

/// Something the hub said while the link is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The hub holds the join for a human; the same code shows at the hub.
    Waiting { code: String },
    /// This key is pinned for the peer id.
    Joined,
}

/// How the link ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Turned away before joining (wrong or expired token, unknown id, a
    /// known id with another key, too many waiting), with the hub's reason.
    Refused(String),
    /// A human denied the join at the hub.
    Denied(String),
    /// The hub revoked this key: delete it and stop.
    Revoked(Option<String>),
    /// The hub closed a joined link (shutting down), with its reason.
    Closed(Option<String>),
    /// The transport dropped.
    Lost(String),
}

/// Dial `url`, authenticate as `key` to the hub holding `hub`, send `hello`,
/// and follow the hub until the link ends. A handshake that fails (wrong hub
/// key, a revoked key the hub answers with silence) is an `Err`.
pub async fn run(
    url: &str,
    key: &Keypair,
    hub: PublicKey,
    hello: &Hello,
    mut on: impl FnMut(Event),
) -> Result<Outcome, WireError> {
    let dial = async {
        let ws = connect_ws(url).await?;
        handshake_initiator(ws, key, hub, hello, PINNED_MAX_ENVELOPE).await
    };
    let mut link = tokio::time::timeout(DEFAULT_HANDSHAKE_TIMEOUT, dial)
        .await
        .map_err(|_| WireError::Protocol("the hub did not answer the handshake".into()))??;
    let mut joined = false;
    let mut ka = KeepAliveState::new();
    loop {
        let env = match recv_with_keepalive(&mut link, KeepAlive::default(), &mut ka).await {
            Ok(env) => env,
            Err(e) if joined => return Ok(Outcome::Lost(e.to_string())),
            Err(e) => return Ok(Outcome::Refused(format!("the hub dropped the link: {e}"))),
        };
        if env.channel != Channel::Control {
            continue; // ACP and LLM channels arrive with #362 and #371.
        }
        match env.decode_control()? {
            Control::Pending => on(Event::Waiting { code: link.handshake_hash().sas_code() }),
            Control::JoinAck { accepted: true, .. } => {
                joined = true;
                on(Event::Joined);
            }
            Control::JoinAck { accepted: false, reason } => {
                let _ = link.close(None).await;
                return Ok(Outcome::Denied(reason.unwrap_or_else(|| "denied at the hub".into())));
            }
            Control::Revoke { reason } => {
                let _ = link.close(None).await;
                return Ok(Outcome::Revoked(reason));
            }
            Control::Close { reason } if joined => return Ok(Outcome::Closed(reason)),
            Control::Close { reason } => {
                return Ok(Outcome::Refused(reason.unwrap_or_else(|| "closed without a reason".into())));
            }
            Control::Hello(_) | Control::Ping { .. } | Control::Pong { .. } => {}
        }
    }
}
