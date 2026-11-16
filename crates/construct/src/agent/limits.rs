//! The model's accounting: what a turn costs, and the window compaction plans
//! against. Kept out of the loop so a pricing or window change has one home.

use super::Agent;
use crate::config::Config;
use crate::llm::Usage;

/// Request bytes per prompt token a completed request measured, when it is
/// big enough to say: fixed overheads would skew a small one. The same ratio
/// sizes the next request and, on resume, the first one of the next turn.
pub(super) fn ratio(bytes: u64, prompt_tokens: u64) -> Option<f64> {
    (prompt_tokens >= 1024).then(|| (bytes as f64 / prompt_tokens as f64).clamp(1.0, 8.0))
}

/// The catalog's price for `model` stands in when the provider sent none.
pub(super) fn price(cfg: &Config, model: &str, u: &mut Usage) {
    if u.cost.is_none()
        && let Some(p) = cfg.lookup(model).and_then(|e| e.pricing)
    {
        u.cost = Some(p.cost(u.prompt_tokens, u.completion_tokens));
    }
}

impl Agent {
    /// Learn bytes per token from a request the provider counted. Small
    /// requests are skipped: fixed overheads would skew the ratio.
    pub(super) fn measure(&mut self, bytes: u64, prompt_tokens: u64) {
        if let Some(ratio) = ratio(bytes, prompt_tokens) {
            self.bytes_per_token = ratio;
        }
    }

    pub(super) fn price(&self, model: &str, u: &mut Usage) {
        price(&self.cfg, model, u);
    }

    /// The window a provider stated about this model wins while it is smaller
    /// than the configured one.
    pub(super) fn cfg_context_window(&self) -> u64 {
        self.window_seen.unwrap_or(u64::MAX).min(u64::from(self.cfg.context_window))
    }
}
