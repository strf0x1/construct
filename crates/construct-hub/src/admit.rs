//! Admission: what the hub does with a connection that completed Noise IK
//! and the 1-RTT `Hello` (#360, #361, ADR 0070).
//!
//! A pure function of the registry, what the wire proved (the key from Noise
//! IK, the id and optional token from the encrypted `Hello`) and two live
//! facts (how many joins wait for a human, whether a pairing window is open),
//! so every trust and anti-spam rule is unit-tested here and the server only
//! applies the verdict. The design choice: nothing reaches a human unasked.
//! A join waits for approval only for an `expected` slot or inside an
//! explicitly opened pairing window, at most `MAX_PENDING` at once, so a
//! leaked hub key (every stamped binary carries it) cannot spam prompts or
//! guess its way past the per-slot strike limit by trying fresh ids.

use crate::invite;
use crate::registry::{Registry, State, check_id};
use construct_peer::PublicKey;

/// Most joins waiting for a human at once.
pub const MAX_PENDING: usize = 5;

/// Wrong join tokens a slot takes before it is burnt.
pub const MAX_STRIKES: u32 = 5;

/// What a connection offered.
#[derive(Debug, Clone, Copy)]
pub struct Offer<'a> {
    pub id: &'a str,
    pub key: PublicKey,
    pub token: Option<&'a str>,
    pub now: u64,
}

/// What the hub knows only while running.
#[derive(Debug, Clone, Copy, Default)]
pub struct Live {
    /// Joins waiting for a human now.
    pub pending: usize,
    /// The key of a join already waiting under this id, if any.
    pub pending_key: Option<PublicKey>,
    /// `construct hub pair` opened a window that has not closed.
    pub pairing: bool,
}

/// The verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// The key is pinned for the id (before, or by this valid token).
    Pinned,
    /// Hold the link for a human; `slot` says an `expected` row exists.
    Pending { slot: bool },
    /// Close the link with this reason.
    Refused(String),
}

impl Registry {
    /// Decide on `offer`; a valid token pins and a wrong one strikes, both in
    /// the registry (the caller saves it).
    pub fn admit(&mut self, offer: Offer<'_>, live: Live) -> Admission {
        let Offer { id, key, token, now } = offer;
        if let Err(why) = check_id(id) {
            return Admission::Refused(why);
        }
        let hex = key.to_hex();
        if self.has_key(State::Revoked, &hex) {
            return Admission::Refused("revoked".into());
        }
        let Some(row) = self.get_mut(id) else {
            return match (token, live.pairing) {
                (Some(_), _) => Admission::Refused(format!("there is no invite for {id}")),
                (None, false) => Admission::Refused(format!(
                    "{id} is not expected; run `construct hub invite {id}` or `construct hub pair` at the hub first"
                )),
                (None, true) => wait(id, key, live, false),
            };
        };
        match row.state {
            State::Revoked => Admission::Refused(format!(
                "{id} was revoked; `construct hub invite {id}` at the hub lets it join again"
            )),
            State::Pinned if row.key.as_deref() == Some(hex.as_str()) => {
                row.last_seen = Some(now);
                Admission::Pinned
            }
            State::Pinned | State::Pending => {
                Admission::Refused(format!("{id} is known with another key; it is never trusted"))
            }
            State::Expected => match token {
                None => wait(id, key, live, true),
                Some(token) => match row.invite.as_ref().map(|inv| (invite::matches(inv, token), inv.expires > now)) {
                    Some((true, true)) => {
                        self.pin(id, key, now);
                        Admission::Pinned
                    }
                    Some((true, false)) => {
                        row.invite = None;
                        Admission::Refused(format!("the join token for {id} expired; ask for a new invite"))
                    }
                    _ => {
                        row.strikes += 1;
                        if row.strikes < MAX_STRIKES {
                            return Admission::Refused(format!("wrong join token for {id}"));
                        }
                        self.peers.retain(|p| p.id != id);
                        Admission::Refused(format!(
                            "too many wrong join tokens: the {id} slot is burnt; `construct hub invite {id}` makes a new one"
                        ))
                    }
                },
            },
        }
    }
}

fn wait(id: &str, key: PublicKey, live: Live, slot: bool) -> Admission {
    match live.pending_key {
        Some(k) if k != key => Admission::Refused(format!("{id} is already waiting for approval with another key")),
        Some(_) => Admission::Pending { slot },
        None if live.pending >= MAX_PENDING => Admission::Refused("too many peers waiting for approval".into()),
        None => Admission::Pending { slot },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Invite, MAX_ID_LEN};
    use construct_peer::Keypair;

    fn key() -> PublicKey {
        Keypair::generate().unwrap().public()
    }

    fn offer<'a>(id: &'a str, key: PublicKey, token: Option<&'a str>) -> Offer<'a> {
        Offer { id, key, token, now: 10 }
    }

    fn invited(id: &str, token: &str, expires: u64) -> Registry {
        let mut r = Registry::default();
        r.invite(id, Invite { hash: invite::hash(token), expires }).unwrap();
        r
    }

    fn refused(a: &Admission, why: &str) -> bool {
        matches!(a, Admission::Refused(w) if w.contains(why))
    }

    #[test]
    fn a_valid_token_pins_once_and_reconnects_need_none() {
        let mut r = invited("old-pc", "tok", 100);
        let k = key();
        assert_eq!(r.admit(offer("old-pc", k, Some("tok")), Live::default()), Admission::Pinned);
        assert_eq!(r.get("old-pc").unwrap().key, Some(k.to_hex()));
        assert!(r.get("old-pc").unwrap().invite.is_none(), "single use");
        assert_eq!(r.admit(offer("old-pc", k, None), Live::default()), Admission::Pinned);
        // The token is spent: another key presenting it gets nothing.
        assert!(refused(&r.admit(offer("old-pc", key(), Some("tok")), Live::default()), "another key"));
    }

    #[test]
    fn an_expired_token_is_refused_and_cleared() {
        let mut r = invited("late", "tok", 5);
        assert!(refused(&r.admit(offer("late", key(), Some("tok")), Live::default()), "expired"));
        assert!(r.get("late").unwrap().invite.is_none());
    }

    #[test]
    fn five_wrong_tokens_burn_the_slot() {
        let mut r = invited("box", "right", 100);
        for i in 1..MAX_STRIKES {
            assert!(refused(&r.admit(offer("box", key(), Some("wrong")), Live::default()), "wrong join token"));
            assert_eq!(r.get("box").unwrap().strikes, i);
        }
        assert!(refused(&r.admit(offer("box", key(), Some("wrong")), Live::default()), "burnt"));
        assert!(r.get("box").is_none());
        // Even the right token is refused now: the slot is gone.
        assert!(refused(&r.admit(offer("box", key(), Some("right")), Live::default()), "no invite"));
    }

    #[test]
    fn unknown_ids_are_refused_unless_a_pairing_window_is_open() {
        let mut r = Registry::default();
        assert!(refused(&r.admit(offer("rand-1", key(), None), Live::default()), "not expected"));
        assert!(refused(&r.admit(offer("rand-2", key(), Some("guess")), Live::default()), "no invite"));
        assert!(r.peers.is_empty(), "nothing to strike, nothing written");
        let pairing = Live { pairing: true, ..Live::default() };
        assert_eq!(r.admit(offer("walk-up", key(), None), pairing), Admission::Pending { slot: false });
        assert!(r.peers.is_empty(), "pending is never a row");
    }

    #[test]
    fn an_expected_slot_waits_without_a_token_within_the_cap() {
        let mut r = invited("old-pc", "tok", 100);
        let k = key();
        assert_eq!(r.admit(offer("old-pc", k, None), Live::default()), Admission::Pending { slot: true });
        let full = Live { pending: MAX_PENDING, ..Live::default() };
        assert!(refused(&r.admit(offer("old-pc", k, None), full), "too many"));
        // The same key reconnecting while waiting is not a new slot.
        let again = Live { pending: MAX_PENDING, pending_key: Some(k), ..Live::default() };
        assert_eq!(r.admit(offer("old-pc", k, None), again), Admission::Pending { slot: true });
        assert!(refused(&r.admit(offer("old-pc", key(), None), again), "another key"));
    }

    #[test]
    fn known_ids_never_change_keys_and_revoked_ones_stay_out() {
        let k = key();
        let mut r = Registry::default();
        r.pin("pi", k, 1);
        assert!(refused(&r.admit(offer("pi", key(), None), Live::default()), "another key"));
        assert_eq!(r.get("pi").unwrap().key, Some(k.to_hex()), "the pinned key is untouched");
        r.revoke("pi", None).unwrap();
        assert!(refused(&r.admit(offer("pi", key(), None), Live::default()), "was revoked"));
        assert_eq!(r.admit(offer("fresh-name", k, None), Live::default()), Admission::Refused("revoked".into()));
    }

    #[test]
    fn bad_ids_are_refused() {
        let mut r = Registry::default();
        let pairing = Live { pairing: true, ..Live::default() };
        for id in ["", "-x", "has space", "ünï", &"a".repeat(MAX_ID_LEN + 1)] {
            assert!(matches!(r.admit(offer(id, key(), None), pairing), Admission::Refused(_)), "{id:?}");
        }
    }
}
