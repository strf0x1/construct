//! The model's accounting: what a turn costs, and the window compaction plans
//! against. Kept out of the loop so a pricing or window change has one home.

use super::Agent;
use crate::config::Config;
use crate::llm::Usage;

/// The catalog's price for `model` stands in when the provider sent none.
pub(super) fn price(cfg: &Config, model: &str, u: &mut Usage) {
    if u.cost.is_none()
        && let Some(p) = cfg.lookup(model).and_then(|e| e.pricing)
    {
        u.cost = Some(p.cost(u.prompt_tokens, u.completion_tokens));
    }
}

impl Agent {
    pub(super) fn price(&self, model: &str, u: &mut Usage) {
        price(&self.cfg, model, u);
    }

    /// The window a provider stated about this model wins while it is smaller
    /// than the configured one.
    pub(super) fn cfg_context_window(&self) -> u64 {
        self.window_seen.unwrap_or(u64::MAX).min(u64::from(self.cfg.context_window))
    }
}
