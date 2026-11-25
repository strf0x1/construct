//! `llm`: where a peer's model calls go (#371). `local` (the default) sends
//! them to the endpoint this config declares, with a key from this machine
//! (ADR 0051); `hub` sends every one through the hub the peer joined, which
//! resolves the provider, model and key on its own host.
//!
//! The design choice: `hub` is only a setting here. `construct peer` starts
//! the loopback gateway and sets `Config::llm_gateway`, a runtime field never
//! read from or written to a file; while it is set, every endpoint resolves
//! to the gateway and the model selector is sent as written, for the hub to
//! resolve. Anywhere else (`run`, the TUI, `acp`) `llm = "hub"` changes
//! nothing.

use serde::{Deserialize, Serialize};

/// The `llm` key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Llm {
    /// This machine's endpoint and key.
    #[default]
    Local,
    /// Through the hub, on the peer link's `llm` channel.
    Hub,
}

impl Llm {
    /// Skipped when serializing, so a config that never says `llm` hashes
    /// as it did before the key existed.
    pub fn is_local(&self) -> bool {
        *self == Self::Local
    }
}

/// The peer's loopback gateway to the hub, once it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmGateway {
    /// `http://127.0.0.1:<port>/v1`.
    pub base_url: String,
    /// The bearer token the gateway wants: held in this process only.
    pub token: String,
}
