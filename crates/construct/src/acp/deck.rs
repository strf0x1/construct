//! Deck's boundary extension on the ACP server: the capability advertised in
//! `initialize` and the informational boundary notification accepted after a
//! late decision (Deck ADR 0004 §6), under the reverse-DNS method or the bare
//! `_deck/boundary` a released Deck sends.
//!
//! The design choice: the capability is advertised only when this process has
//! a boundary endpoint. An agent without one would have nowhere to put the
//! note, and Deck only sends to an agent that advertised it. Both the
//! reverse-DNS names and the bare ones are advertised for one release, so a
//! released Deck still finds them (issue #171).

use super::map;
use super::server::{Mode, Server};
use serde_json::{Value, json};

impl Server {
    /// The `agentCapabilities` object for `initialize`.
    pub(super) fn capabilities(&self) -> Value {
        let mut caps = json!({
            "loadSession": self.mode == Mode::User,
            "promptCapabilities": { "image": false, "audio": false, "embeddedContext": true },
            "mcpCapabilities": { "http": false, "sse": false },
            "_meta": {
                map::META_KEY: { "events": true },
                map::LEGACY_META_KEY: { "events": true },
            },
        });
        if self.mode == Mode::User && self.cfg.deck.is_some() {
            caps["_meta"][map::DECK_KEY] = json!({ "boundary": true });
            caps["_meta"][map::LEGACY_DECK_KEY] = json!({ "boundary": true });
        }
        // The GitHub operation API has no bare form (contract 1.2.0 §11.1).
        if self.mode == Mode::User && self.cfg.github_gateway.is_some() {
            caps["_meta"][map::DECK_KEY][map::GITHUB_OPERATIONS] = json!(map::GITHUB_OPERATIONS_VERSION);
        }
        if self.mode == Mode::User {
            caps["sessionCapabilities"] = json!({ "resume": {}, "close": {} });
            caps["_meta"][map::META_KEY]["liveAttach"] = json!(true);
            caps["_meta"][map::LEGACY_META_KEY]["liveAttach"] = json!(true);
        }
        caps
    }

    /// A late boundary decision: informational, and only for the sessions the
    /// client named (or every session when it named none). It grants nothing.
    pub(super) fn boundary_notice(&self, params: Value) {
        let Ok(notice) = serde_json::from_value::<crate::tools::boundary::Notice>(params)
            .and_then(|notice| notice.validate().map_err(serde::de::Error::custom))
        else {
            return;
        };
        for (sid, s) in &self.sessions {
            if notice.session.as_deref().is_none_or(|id| id == sid)
                && let Some(b) = &s.boundary
            {
                b.push_notice(notice.clone());
            }
        }
    }
}
