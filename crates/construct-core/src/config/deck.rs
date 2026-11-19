//! What Deck tells a guest agent through its environment: the boundary status
//! endpoint (ADR 0004 §6), for a task whose project has a repository the
//! GitHub operation gateway (contract 1.2.0 §11), and for a coordinator the
//! task-control gateway (contract 1.4.0 §13).
//!
//! The design choice: all of them come only from the environment Deck sets,
//! never from `config.toml`, and an absent one leaves its field `None`, which
//! is what keeps the boundary tools, gateway publishing and the `deck` tool
//! out of every other session. The control URL is not a `Config` field at
//! all, so it can never travel to a delegated child inside its config.

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

/// A coordinator's task-control gateway (contract 1.4.0 §13.1).
pub const CONTROL_URL: &str = "DECK_CONTROL_URL";
/// The coordinator's bearer token for that gateway (§13.2), set for a local
/// coordinator. Read when a request is sent, never stored or shown.
pub const CONTROL_TOKEN: &str = "DECK_CONTROL_TOKEN";
/// Both, which no tool, MCP server or delegated child inherits or may set:
/// coordination depth stays one.
pub const CONTROL_ENV: &[&str] = &[CONTROL_URL, CONTROL_TOKEN];

/// `DECK_CONTROL_URL` when it is a usable control address; `None` otherwise.
pub fn control_url() -> Option<String> {
    control_url_from(&std::env::var(CONTROL_URL).ok()?)
}

/// The value only when it is exactly `http://127.0.0.1:<port>` or
/// `http://[::1]:<port>`, with no userinfo, path, query, fragment or trailing
/// slash. Anything else is treated as absent and never sent to (§13.1).
pub fn control_url_from(value: &str) -> Option<String> {
    let rest = value.strip_prefix("http://")?;
    let port = rest.strip_prefix("127.0.0.1:").or_else(|| rest.strip_prefix("[::1]:"))?;
    let ok = !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|p| p > 0);
    ok.then(|| value.to_string())
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

#[cfg(test)]
mod tests {
    use super::control_url_from;

    #[test]
    fn only_an_exact_loopback_origin_is_a_control_url() {
        for good in ["http://127.0.0.1:8080", "http://[::1]:1"] {
            assert_eq!(control_url_from(good).as_deref(), Some(good));
        }
        for bad in [
            "",
            "http://127.0.0.1:8080/",
            "http://127.0.0.1:8080/control",
            "http://127.0.0.1",
            "http://127.0.0.1:",
            "http://127.0.0.1:0",
            "http://127.0.0.1:65536",
            "http://127.0.0.1:80?x",
            "http://localhost:8080",
            "http://10.0.0.2:8080",
            "https://127.0.0.1:8080",
            "http://user@127.0.0.1:8080",
            "http://127.0.0.1:+80",
            " http://127.0.0.1:8080",
        ] {
            assert_eq!(control_url_from(bad), None, "{bad:?}");
        }
    }
}
