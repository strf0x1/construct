//! The peer registry, `$CONSTRUCT_HOME/hub/peers.json` (#360, #361).
//!
//! One row per peer id: its name, the X25519 key it is pinned to, a state,
//! when it was last seen, an open invite (the token's SHA-256, never the
//! token) with its wrong-token strikes, and the ACP session and turn it last
//! reported (filled by #362). The design choice: only three states are ever
//! written — `expected` (a slot an invite or `construct build` made),
//! `pinned` and `revoked`. A join waiting for a human is `pending` only while
//! its link is open (the SAS code exists only there), so a dead connection
//! leaves no row to approve and no slot to clog. The transitions here and
//! admission (`admit.rs`) are pure, so every trust rule is unit-tested.

use anyhow::{Context, Result, bail};
use construct_peer::{KeyDecision, PINNED_MAX_ENVELOPE, PRE_PIN_MAX_ENVELOPE, PublicKey};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Version of the file format.
pub const VERSION: u32 = 1;

/// Longest peer id accepted from a `Hello`.
pub const MAX_ID_LEN: usize = 64;

/// Where a peer stands with this hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// A slot an invite (or `construct build`, #363) made; no key yet.
    Expected,
    /// Waiting for a human with its link open. Never written to the file.
    Pending,
    /// Only this key may speak for this id.
    Pinned,
    /// Refused for good: the id, and its key under any id.
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

/// An open single-use join token for an `expected` slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    /// Hex SHA-256 of the token.
    pub hash: String,
    /// Unix seconds after which the token is refused.
    pub expires: u64,
}

/// One peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub id: String,
    pub name: String,
    pub state: State,
    /// Hex X25519 public key: pinned or revoked (or offered, while pending).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Unix seconds of the last authenticated message or disconnect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite: Option<Invite>,
    /// Wrong join tokens offered for this slot since its last invite.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub strikes: u32,
    /// The ACP session this peer last ran (#362).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The last turn number observed in that session (#362).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
}

const fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Peer {
    #[must_use]
    pub fn new(id: &str, state: State) -> Self {
        Self {
            id: id.into(),
            name: id.into(),
            state,
            key: None,
            last_seen: None,
            invite: None,
            strikes: 0,
            session: None,
            turn: None,
        }
    }
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
    /// Read the registry; a missing file is an empty registry. A `pending`
    /// row (an older hub wrote them) goes back to an empty `expected` slot.
    pub fn load(path: &Path) -> Result<Self> {
        let mut r: Self = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        for p in r.peers.iter_mut().filter(|p| p.state == State::Pending) {
            p.state = State::Expected;
            p.key = None;
        }
        Ok(r)
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

    pub(crate) fn get_mut(&mut self, id: &str) -> Option<&mut Peer> {
        self.peers.iter_mut().find(|p| p.id == id)
    }

    pub(crate) fn has_key(&self, state: State, hex: &str) -> bool {
        self.peers.iter().any(|p| p.state == state && p.key.as_deref() == Some(hex))
    }

    /// The verdict on a static key right after Noise IK message 1, before the
    /// peer id is known: a revoked key gets no message 2; a pinned key gets
    /// the large envelope cap; anything else stays at the pre-pin cap.
    #[must_use]
    pub fn key_decision(&self, key: PublicKey) -> KeyDecision {
        let hex = key.to_hex();
        if self.has_key(State::Revoked, &hex) {
            KeyDecision::Reject("revoked".into())
        } else if self.has_key(State::Pinned, &hex) {
            KeyDecision::Accept { max_envelope_bytes: PINNED_MAX_ENVELOPE }
        } else {
            KeyDecision::Accept { max_envelope_bytes: PRE_PIN_MAX_ENVELOPE }
        }
    }

    /// Open (or replace) the invite for `id`: a new or revoked id becomes an
    /// `expected` slot; strikes start over. A pinned peer needs no invite.
    pub fn invite(&mut self, id: &str, invite: Invite) -> Result<()> {
        check_id(id).map_err(anyhow::Error::msg)?;
        let row = match self.get_mut(id) {
            Some(p) if p.state == State::Pinned => {
                bail!("{id} is already pinned; `construct hub revoke {id}` first to join it with a new key")
            }
            Some(p) => p,
            None => {
                self.peers.push(Peer::new(id, State::Expected));
                self.peers.last_mut().context("pushed a row")?
            }
        };
        row.state = State::Expected;
        row.key = None;
        row.invite = Some(invite);
        row.strikes = 0;
        Ok(())
    }

    /// Pin `key` for `id` (a valid token, or a human's approval).
    pub fn pin(&mut self, id: &str, key: PublicKey, now: u64) {
        if self.get(id).is_none() {
            self.peers.push(Peer::new(id, State::Pinned));
        }
        if let Some(row) = self.get_mut(id) {
            row.state = State::Pinned;
            row.key = Some(key.to_hex());
            row.invite = None;
            row.strikes = 0;
            row.last_seen = Some(now);
        }
    }

    /// Revoke `id`: its row (made if only a live pending join knew it) keeps
    /// the key it had, or `offered`, so that key is refused before message 2.
    pub fn revoke(&mut self, id: &str, offered: Option<PublicKey>) -> Result<()> {
        if self.get(id).is_none() {
            if offered.is_none() {
                bail!("no peer {id}");
            }
            self.peers.push(Peer::new(id, State::Revoked));
        }
        let row = self.get_mut(id).context("row exists")?;
        row.state = State::Revoked;
        if let Some(k) = offered {
            row.key = Some(k.to_hex());
        }
        row.invite = None;
        Ok(())
    }

    /// Record that `id` was seen at `now` (on disconnect).
    pub fn seen(&mut self, id: &str, now: u64) {
        if let Some(p) = self.get_mut(id) {
            p.last_seen = Some(now);
        }
    }
}

/// A peer id is short, printable and unambiguous: it comes from a `Hello`
/// and ends up in a TUI tile and a shell command.
pub fn check_id(id: &str) -> Result<(), String> {
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

    fn invite() -> Invite {
        Invite { hash: "ab".into(), expires: 100 }
    }

    #[test]
    fn invite_makes_a_slot_and_refuses_a_pinned_peer() {
        let mut r = Registry::default();
        r.invite("old-pc", invite()).unwrap();
        let p = r.get("old-pc").unwrap();
        assert_eq!((p.state, p.invite.is_some()), (State::Expected, true));
        r.pin("old-pc", key(), 5);
        let p = r.get("old-pc").unwrap();
        assert_eq!((p.state, p.invite.is_none(), p.last_seen), (State::Pinned, true, Some(5)));
        assert!(r.invite("old-pc", invite()).unwrap_err().to_string().contains("already pinned"));
        assert!(r.invite("bad id", invite()).is_err());
    }

    #[test]
    fn revoke_keeps_the_key_refused_and_a_new_invite_reopens_the_id() {
        let k = key();
        let mut r = Registry::default();
        r.pin("pi", k, 1);
        r.revoke("pi", None).unwrap();
        assert_eq!(r.key_decision(k), KeyDecision::Reject("revoked".into()));
        // A pending join only the live link knew becomes a revoked row.
        let stranger = key();
        r.revoke("walk-up", Some(stranger)).unwrap();
        assert_eq!(r.key_decision(stranger), KeyDecision::Reject("revoked".into()));
        assert!(r.revoke("nobody", None).is_err());
        r.invite("pi", invite()).unwrap();
        let p = r.get("pi").unwrap();
        assert_eq!((p.state, p.key.clone()), (State::Expected, None));
    }

    #[test]
    fn round_trips_and_drops_old_pending_rows() {
        let d = std::env::temp_dir().join(format!("construct-hub-reg-{}", std::process::id()));
        let path = d.join("hub/peers.json");
        let mut r = Registry::default();
        r.pin("old-pc", key(), 7);
        r.invite("slot", invite()).unwrap();
        r.save(&path).unwrap();
        assert_eq!(Registry::load(&path).unwrap(), r);
        let old = Peer { key: Some("cd".into()), ..Peer::new("was-pending", State::Pending) };
        Registry { version: VERSION, peers: vec![old] }.save(&path).unwrap();
        let p = Registry::load(&path).unwrap().peers.remove(0);
        assert_eq!((p.state, p.key), (State::Expected, None));
        assert_eq!(Registry::load(&d.join("missing.json")).unwrap(), Registry::default());
        let _ = std::fs::remove_dir_all(&d);
    }
}
