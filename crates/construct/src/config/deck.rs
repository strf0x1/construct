//! What Deck tells a guest agent through its environment: the boundary status
//! endpoint (ADR 0004 §6) and, for a task whose project has a repository, the
//! GitHub operation gateway (contract 1.2.0 §11).
//!
//! The design choice: both come only from the environment the VM runtime
//! sets, never from `config.toml`, and an absent one leaves its field `None`,
//! which is what keeps the boundary tools and gateway publishing out of every
//! other session.

use super::Config;
use serde::{Deserialize, Serialize};

/// Deck's boundary endpoint. The URL is a base: construct appends `/events`
/// and `/approvals/<id>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeckConfig {
    pub boundary_url: String,
}

/// A base URL from the environment, without a trailing slash; `None` when it
/// is unset or blank.
fn base_url(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let value = value.trim().trim_end_matches('/');
    (!value.is_empty()).then(|| value.to_string())
}

impl Config {
    /// `DECK_BOUNDARY_URL` and `DECK_GITHUB_URL`. With the gateway set, the
    /// `developer` tool publishes through it and never through host
    /// `git push` or `gh`.
    pub(super) fn apply_deck_env(&mut self) {
        if let Some(boundary_url) = base_url("DECK_BOUNDARY_URL") {
            self.deck = Some(DeckConfig { boundary_url });
        }
        if let Some(url) = base_url("DECK_GITHUB_URL") {
            self.github_gateway = Some(url);
        }
    }
}
