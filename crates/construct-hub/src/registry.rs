//! The peer registry, `$CONSTRUCT_HOME/hub/peers.json` (#360).
//!
//! One row per peer id: its name, the X25519 key it is pinned to (or the key
//! it offered while pending), a state, when it was last seen, and the ACP
//! session and turn it last reported (filled by #362; resume uses ACP
//! `liveAttach`, never a journal cursor). The design choice: admission is a
//! pure function of the registry and what the wire proved (peer id from the
//! encrypted `Hello`, key from Noise IK), so every trust rule is unit-tested
//! here and the server only applies the verdict. A key is never trusted
//! because of the id it claims: a known id arriving with another key is
//! refused, and nothing here ever promotes a peer to `pinned` (that is the
//! join flow, #361).

use anyhow::{Context, Result};
use construct_peer::{KeyDecision, PINNED_MAX_ENVELOPE, PRE_PIN_MAX_ENVELOPE, PublicKey};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Version of the file format.
pub const VERSION: u32 = 1;

/// Most `pending` rows kept at once. An unknown key that completes the
/// handshake (it must hold the hub's public key to get that far) evicts the
/// oldest pending row that is not connected, or is refused when every one is.
pub const MAX_PENDING: usize = 8;

/// Longest peer id accepted from a `Hello`.
pub const MAX_ID_LEN: usize = 64;

/// Where a peer stands with this hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// A slot `construct build` registered; no key seen yet (#363).
    Expected,
    /// Completed the handshake with a key nobody approved yet (#361).
    Pending,
    /// Approved: only this key may speak for this id.
    Pinned,
    /// Refused for good; its key is refused under any id.
    Revoked,
}

impl State {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Expected => "expected",
            Self::Pending => "pending",
            Self::Pinned => "pinned",
            Self::Revoked => "revoked",
        }
    }
}

/// One peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub state: State,
    /// Hex X25519 public key: pinned, or offered while pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Unix seconds of the last authenticated message or disconnect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<u64>,
    /// The ACP session this peer last ran (#362).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The last turn number observed in that session (#362).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
}

/// What admitting a confirmed connection decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Keep the link open; the peer is in this state.
    Admitted(State),
    /// Close the link with this reason.
    Refused(String),
}

/// The whole registry file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub version: u32,
    #[serde(default)]
    pub peers: Vec<Peer>,
}

impl Default for Registry {
    fn default() -> Self {
        Self { version: VERSION, peers: Vec::new() }
    }
}

impl Registry {
    /// Read the registry; a missing file is an empty registry.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Write the registry atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        construct_core::home::write_atomic(path, &bytes)
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Peer> {
        self.peers.iter().find(|p| p.id == id)
    }

    /// The verdict on a static key right after Noise IK message 1, before the
    /// peer id is known: a revoked key gets no message 2; a pinned key gets
    /// the large envelope cap; anything else stays at the pre-pin cap.
    #[must_use]
    pub fn key_decision(&self, key: PublicKey) -> KeyDecision {
        let hex = key.to_hex();
        let with_key = |state| self.peers.iter().any(|p| p.state == state && p.key.as_deref() == Some(hex.as_str()));
        if with_key(State::Revoked) {
            KeyDecision::Reject("revoked".into())
        } else if with_key(State::Pinned) {
            KeyDecision::Accept { max_envelope_bytes: PINNED_MAX_ENVELOPE }
        } else {
            KeyDecision::Accept { max_envelope_bytes: PRE_PIN_MAX_ENVELOPE }
        }
    }

    /// Admit a connection whose `Hello` (claiming `id`) arrived over a Noise
    /// session authenticated with `key`. `connected` says whether a peer id
    /// currently has a live link, so eviction never drops a connected peer.
    pub fn admit(&mut self, id: &str, key: PublicKey, now: u64, connected: &dyn Fn(&str) -> bool) -> Admission {
        if let Err(why) = check_id(id) {
            return Admission::Refused(why);
        }
        let hex = key.to_hex();
        if self.peers.iter().any(|p| p.state == State::Revoked && p.key.as_deref() == Some(hex.as_str())) {
            return Admission::Refused("revoked".into());
        }
        if let Some(peer) = self.peers.iter_mut().find(|p| p.id == id) {
            let same_key = peer.key.as_deref() == Some(hex.as_str());
            return match peer.state {
                State::Revoked => Admission::Refused("revoked".into()),
                State::Pinned | State::Pending if !same_key => {
                    Admission::Refused(format!("peer {id} is known with another key; it is never trusted"))
                }
                State::Pinned | State::Pending => {
                    peer.last_seen = Some(now);
                    Admission::Admitted(peer.state)
                }
                State::Expected => {
                    peer.state = State::Pending;
                    peer.key = Some(hex);
                    peer.last_seen = Some(now);
                    Admission::Admitted(State::Pending)
                }
            };
        }
        let pending: Vec<usize> =
            self.peers.iter().enumerate().filter(|(_, p)| p.state == State::Pending).map(|(i, _)| i).collect();
        if pending.len() >= MAX_PENDING {
            let oldest = pending
                .into_iter()
                .filter(|&i| !connected(&self.peers[i].id))
                .min_by_key(|&i| self.peers[i].last_seen.unwrap_or(0));
            match oldest {
                Some(i) => {
                    self.peers.remove(i);
                }
                None => return Admission::Refused("too many peers waiting for approval".into()),
            }
        }
        self.peers.push(Peer {
            id: id.into(),
            name: id.into(),
            state: State::Pending,
            key: Some(hex),
            last_seen: Some(now),
            session: None,
            turn: None,
        });
        Admission::Admitted(State::Pending)
    }

    /// Record that `id` was seen at `now` (on disconnect).
    pub fn seen(&mut self, id: &str, now: u64) {
        if let Some(p) = self.peers.iter_mut().find(|p| p.id == id) {
            p.last_seen = Some(now);
        }
    }
}

/// A peer id is short, printable and unambiguous: it comes from an
/// unauthenticated-until-approved `Hello` and ends up in a TUI tile.
fn check_id(id: &str) -> Result<(), String> {
    let ok_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return Err(format!("peer id must be 1 to {MAX_ID_LEN} characters"));
    }
    if !id.starts_with(|c: char| c.is_ascii_alphanumeric()) || !id.chars().all(ok_char) {
        return Err("peer id may only use letters, digits, '-', '_' and '.', starting with a letter or digit".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use construct_peer::Keypair;

    fn key() -> PublicKey {
        Keypair::generate().unwrap().public()
    }

    fn nobody(_: &str) -> bool {
        false
    }

    fn pinned(id: &str, k: PublicKey) -> Peer {
        Peer {
            id: id.into(),
            name: id.into(),
            state: State::Pinned,
            key: Some(k.to_hex()),
            last_seen: None,
            session: None,
            turn: None,
        }
    }

    #[test]
    fn an_unknown_peer_waits_as_pending_at_the_small_cap() {
        let mut r = Registry::default();
        let k = key();
        assert_eq!(r.key_decision(k), KeyDecision::Accept { max_envelope_bytes: PRE_PIN_MAX_ENVELOPE });
        assert_eq!(r.admit("old-pc", k, 10, &nobody), Admission::Admitted(State::Pending));
        let p = r.get("old-pc").unwrap();
        assert_eq!((p.state, p.key.clone(), p.last_seen), (State::Pending, Some(k.to_hex()), Some(10)));
        // Reconnecting with the same key stays pending, never pinned.
        assert_eq!(r.admit("old-pc", k, 20, &nobody), Admission::Admitted(State::Pending));
    }

    #[test]
    fn a_pinned_peer_comes_back_online_and_another_key_is_refused() {
        let k = key();
        let mut r = Registry { version: VERSION, peers: vec![pinned("pi", k)] };
        assert_eq!(r.key_decision(k), KeyDecision::Accept { max_envelope_bytes: PINNED_MAX_ENVELOPE });
        assert_eq!(r.admit("pi", k, 5, &nobody), Admission::Admitted(State::Pinned));
        let other = key();
        assert!(matches!(r.admit("pi", other, 6, &nobody), Admission::Refused(why) if why.contains("another key")));
        assert_eq!(r.get("pi").unwrap().key, Some(k.to_hex()), "the pinned key is untouched");
    }

    #[test]
    fn a_pending_id_is_not_taken_over_by_another_key() {
        let mut r = Registry::default();
        r.admit("box", key(), 1, &nobody);
        assert!(matches!(r.admit("box", key(), 2, &nobody), Admission::Refused(_)));
    }

    #[test]
    fn an_expected_slot_records_the_offered_key_as_pending() {
        let mut r = Registry {
            version: VERSION,
            peers: vec![Peer { state: State::Expected, key: None, ..pinned("old-pc", key()) }],
        };
        let k = key();
        assert_eq!(r.admit("old-pc", k, 3, &nobody), Admission::Admitted(State::Pending));
        assert_eq!(r.get("old-pc").unwrap().key, Some(k.to_hex()));
    }

    #[test]
    fn a_revoked_key_is_refused_under_any_id() {
        let k = key();
        let mut r = Registry { version: VERSION, peers: vec![Peer { state: State::Revoked, ..pinned("gone", k) }] };
        assert_eq!(r.key_decision(k), KeyDecision::Reject("revoked".into()));
        assert_eq!(r.admit("fresh-name", k, 1, &nobody), Admission::Refused("revoked".into()));
    }

    #[test]
    fn bad_ids_are_refused() {
        let mut r = Registry::default();
        for id in ["", "-x", "has space", "ünï", &"a".repeat(MAX_ID_LEN + 1)] {
            assert!(matches!(r.admit(id, key(), 1, &nobody), Admission::Refused(_)), "{id:?}");
        }
        assert!(r.peers.is_empty());
    }

    #[test]
    fn pending_rows_are_capped_by_evicting_the_oldest_disconnected() {
        let mut r = Registry::default();
        for i in 0..MAX_PENDING {
            r.admit(&format!("p{i}"), key(), i as u64, &nobody);
        }
        // p0 is connected, so p1 (the oldest disconnected) goes.
        let connected = |id: &str| id == "p0";
        assert_eq!(r.admit("new", key(), 100, &connected), Admission::Admitted(State::Pending));
        assert!(r.get("p0").is_some() && r.get("p1").is_none() && r.get("new").is_some());
        // Everyone connected: refused.
        let all = |_: &str| true;
        assert!(matches!(r.admit("one-more", key(), 101, &all), Admission::Refused(_)));
    }

    #[test]
    fn round_trips_through_the_file() {
        let d = std::env::temp_dir().join(format!("construct-hub-reg-{}", std::process::id()));
        let path = d.join("hub/peers.json");
        let mut r = Registry::default();
        r.admit("old-pc", key(), 7, &nobody);
        r.save(&path).unwrap();
        assert_eq!(Registry::load(&path).unwrap(), r);
        assert_eq!(Registry::load(&d.join("missing.json")).unwrap(), Registry::default());
        let _ = std::fs::remove_dir_all(&d);
    }
}
