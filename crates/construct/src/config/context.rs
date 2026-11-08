//! The `[context]` table: which strategy owns the message list (ADR 0042).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    /// The strategy's name; unknown names fail when the agent is built.
    pub strategy: String,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self { strategy: "compact".into() }
    }
}

impl ContextConfig {
    /// `CONSTRUCT_CONTEXT_STRATEGY` overrides the file for one run.
    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("CONSTRUCT_CONTEXT_STRATEGY") {
            self.strategy = v;
        }
    }
}
