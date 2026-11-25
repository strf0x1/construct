//! Join tokens for `construct hub invite` (#361).
//!
//! A token is 16 random bytes in hex, single use and short-lived; the
//! registry keeps only its SHA-256, so reading `peers.json` never yields a
//! usable token. The design choice: the token travels only inside the Noise
//! channel (in `Hello`), never in a URL, and what the operator copies is one
//! ready-to-paste command for the target's shell.

use crate::registry::Invite;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How long an invite stays valid unless `--expires` says otherwise.
pub const DEFAULT_TTL_SECS: u64 = 15 * 60;

/// Random bytes in a token.
const TOKEN_BYTES: usize = 16;

/// What `construct hub invite` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteReply {
    pub id: String,
    pub token: String,
    /// Unix seconds.
    pub expires: u64,
    /// The URL the peer dials.
    pub url: String,
    /// The hub's public key (hex).
    pub key: String,
    /// The command to run on the target.
    pub command: String,
}

/// A fresh token and the row entry that accepts it until `now + ttl`.
pub fn mint(ttl_secs: u64, now: u64) -> Result<(String, Invite)> {
    let token = construct_peer::random_token(TOKEN_BYTES).map_err(|e| anyhow::anyhow!("mint join token: {e}"))?;
    let invite = Invite { hash: hash(&token), expires: now.saturating_add(ttl_secs) };
    Ok((token, invite))
}

/// Hex SHA-256 of a token.
#[must_use]
pub fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `token` is the one `invite` was minted for. Hashes are compared,
/// so the comparison's timing says nothing about the token.
#[must_use]
pub fn matches(invite: &Invite, token: &str) -> bool {
    hash(token) == invite.hash
}

/// The reply for a minted token, with the command a target runs until
/// stamping (#363) bakes the URL, key and id into the binary.
#[must_use]
pub fn reply(id: &str, token: String, expires: u64, url: String, key: String) -> InviteReply {
    let command = format!("CONSTRUCT_JOIN={token} construct peer --url {url} --hub-key {key} --id {id}");
    InviteReply { id: id.into(), token, expires, url, key, command }
}

/// Seconds in a span like `90s`, `15m`, `2h` or `1d` (a bare number is
/// seconds).
pub fn parse_span(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, unit) = s.find(|c: char| !c.is_ascii_digit()).map_or((s, ""), |i| s.split_at(i));
    let n: u64 = num.parse().map_err(|_| format!("`{s}` is not a span like 90s, 15m, 2h or 1d"))?;
    let mult = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(format!("`{s}`: the unit must be s, m, h or d")),
    };
    n.checked_mul(mult).ok_or_else(|| format!("`{s}` is too long"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_parse_with_units() {
        assert_eq!(parse_span("90"), Ok(90));
        assert_eq!(parse_span("15m"), Ok(900));
        assert_eq!(parse_span("2h"), Ok(7200));
        assert_eq!(parse_span("1d"), Ok(86_400));
        assert!(parse_span("5w").is_err() && parse_span("m").is_err() && parse_span("").is_err());
    }

    #[test]
    fn minted_tokens_match_only_themselves_and_carry_their_expiry() {
        let (token, inv) = mint(60, 1000).unwrap();
        assert_eq!(token.len(), 2 * TOKEN_BYTES);
        assert_eq!(inv.expires, 1060);
        assert!(matches(&inv, &token));
        assert!(!matches(&inv, "nope"));
        assert_ne!(inv.hash, token, "only the hash is kept");
        let r = reply("old-pc", token.clone(), 1060, "ws://h:1".into(), "ab".into());
        assert!(r.command.starts_with(&format!("CONSTRUCT_JOIN={token} construct peer")), "{}", r.command);
        assert!(r.command.ends_with("--id old-pc"));
    }
}
