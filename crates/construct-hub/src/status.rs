//! What `construct hub status` reports, running or not (#360).
//!
//! A running hub answers over its control socket with live connection state;
//! with no hub, the same shape is read from the files (`running: false`,
//! every peer offline), so the command always works, with or without a TUI.

use crate::files::Files;
use crate::registry::{Peer, Registry};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

/// The hub and its peers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// The URL peers dial (`ws://host:port`), when running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<String>,
    /// The controller's public key (hex), once one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub socket: String,
    /// Unix seconds until which unknown ids may wait for approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_until: Option<u64>,
    pub peers: Vec<PeerStatus>,
}

/// One registry row plus its live connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerStatus {
    #[serde(flatten)]
    pub peer: Peer,
    pub online: bool,
    /// The address the live link came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// The SAS code of a join waiting for approval; the peer shows the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl Status {
    /// Status read from disk, for when no hub answers.
    pub fn offline(files: &Files) -> Result<Self> {
        let key = files.public_key()?;
        let registry = Registry::load(&files.registry())?;
        Ok(Self {
            running: false,
            pid: None,
            listen: None,
            key,
            socket: files.socket().display().to_string(),
            pairing_until: None,
            peers: registry
                .peers
                .into_iter()
                .map(|peer| PeerStatus { peer, online: false, remote: None, code: None })
                .collect(),
        })
    }

    /// Human-readable lines; `now` is unix seconds, for "seen 3m ago".
    #[must_use]
    pub fn render(&self, now: u64) -> String {
        let mut out = String::new();
        match (&self.listen, self.pid) {
            (Some(url), Some(pid)) if self.running => {
                let _ = writeln!(out, "hub: running (pid {pid}), peers dial {url}");
            }
            _ => {
                let _ = writeln!(out, "hub: not running");
            }
        }
        let _ = writeln!(out, "key: {}", self.key.as_deref().unwrap_or("none yet (`construct hub key` creates it)"));
        let _ = writeln!(out, "socket: {}", self.socket);
        if let Some(until) = self.pairing_until.filter(|&u| u > now) {
            let _ = writeln!(out, "pairing: open for {}", left(until - now));
        }
        if self.peers.is_empty() {
            let _ = writeln!(out, "peers: none");
            return out;
        }
        let _ = writeln!(out, "peers:");
        for p in &self.peers {
            let link = if p.online { "online" } else { "offline" };
            let seen = p.peer.last_seen.map_or_else(|| "never seen".into(), |t| format!("seen {}", ago(now, t)));
            let key =
                p.peer.key.as_deref().and_then(|k| k.get(..16)).map(|k| format!("  key {k}…")).unwrap_or_default();
            let code = p.code.as_deref().map(|c| format!("  code {c}")).unwrap_or_default();
            let _ = writeln!(out, "  {}  {}  {link}  {seen}{key}{code}", p.peer.id, p.peer.state.label());
        }
        out
    }
}

fn ago(now: u64, then: u64) -> String {
    format!("{} ago", left(now.saturating_sub(then)))
}

/// A span of seconds, coarsely: `40s`, `3m`, `2h`, `1d`.
#[must_use]
pub fn left(s: u64) -> String {
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::State;

    #[test]
    fn renders_running_and_offline_rows() {
        let peer = Peer { key: Some("ab".repeat(32)), last_seen: Some(100), ..Peer::new("old-pc", State::Pending) };
        let s = Status {
            running: true,
            pid: Some(42),
            listen: Some("ws://127.0.0.1:9443".into()),
            key: Some("cd".repeat(32)),
            socket: "/h/hub.sock".into(),
            pairing_until: Some(400),
            peers: vec![PeerStatus { peer, online: true, remote: None, code: Some("K7M-4QP".into()) }],
        };
        let text = s.render(220);
        assert!(text.contains("hub: running (pid 42), peers dial ws://127.0.0.1:9443"), "{text}");
        assert!(text.contains("old-pc  pending  online  seen 2m ago  key abababababababab…  code K7M-4QP"), "{text}");
        assert!(text.contains("pairing: open for 3m"), "{text}");
        let none = Status { running: false, pid: None, listen: None, key: None, peers: vec![], ..s };
        assert!(none.render(0).contains("hub: not running") && none.render(0).contains("peers: none"));
    }
}
