//! The `[context]` table: which strategy owns the message list (ADR 0042).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    /// The strategy's name; unknown names fail when the agent is built.
    pub strategy: String,
    /// The `optchat` strategy's own settings (ADR 0043).
    pub optchat: OptchatConfig,
}

/// The `[context.optchat]` table (ADR 0043). `model` empty falls back to
/// `[compaction] model`, then to the run's model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OptchatConfig {
    /// The summary model.
    pub model: String,
    /// The view's size ceiling, in bytes.
    pub high: u64,
    /// How many summarization calls run at once.
    pub concurrency: usize,
}

impl Default for OptchatConfig {
    fn default() -> Self {
        Self { model: String::new(), high: 128_000, concurrency: 8 }
    }
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self { strategy: "compact".into(), optchat: OptchatConfig::default() }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optchat_defaults_are_the_documented_ones() {
        let cfg = OptchatConfig::default();
        assert_eq!(cfg.model, "");
        assert_eq!(cfg.high, 128_000);
        assert_eq!(cfg.concurrency, 8);
    }

    #[test]
    fn the_optchat_table_is_read_from_toml() {
        let cfg: ContextConfig = toml::from_str(
            r#"
strategy = "optchat"

[optchat]
model = "summarizer"
high = 64000
concurrency = 2
"#,
        )
        .expect("the [context] table");
        assert_eq!(cfg.strategy, "optchat");
        assert_eq!(cfg.optchat.model, "summarizer");
        assert_eq!(cfg.optchat.high, 64_000);
        assert_eq!(cfg.optchat.concurrency, 2);

        let bare: ContextConfig = toml::from_str("").expect("an absent table uses the defaults");
        assert_eq!(bare.optchat.high, 128_000);
    }
}
