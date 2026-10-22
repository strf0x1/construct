//! Token and cost accounting from a response's `usage` object.
//!
//! One tolerant parser for every provider: each field is read from wherever
//! some provider puts it, and a missing field is zero (or no cost), never an
//! error. Absent cache counts mean the provider did not say, not a miss.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// All prompt tokens, cached or not.
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Prompt tokens read from the provider's cache.
    #[serde(default)]
    pub cached_tokens: u64,
    /// Prompt tokens written to the cache (providers that bill writes).
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// What the request cost, in the provider's currency (OpenRouter: USD).
    #[serde(default)]
    pub cost: Option<f64>,
}

impl Usage {
    pub fn parse(u: &Value) -> Self {
        let n = |p: &str| u.pointer(p).and_then(Value::as_u64);
        Self {
            prompt_tokens: n("/prompt_tokens").unwrap_or(0),
            completion_tokens: n("/completion_tokens").unwrap_or(0),
            // OpenAI and OpenRouter; DeepSeek reports hits at the top level.
            cached_tokens: n("/prompt_tokens_details/cached_tokens").or(n("/prompt_cache_hit_tokens")).unwrap_or(0),
            cache_write_tokens: n("/prompt_tokens_details/cache_write_tokens").unwrap_or(0),
            cost: u.get("cost").and_then(Value::as_f64),
        }
    }

    pub fn add(&mut self, o: &Usage) {
        self.prompt_tokens += o.prompt_tokens;
        self.completion_tokens += o.completion_tokens;
        self.cached_tokens += o.cached_tokens;
        self.cache_write_tokens += o.cache_write_tokens;
        self.cost = match (self.cost, o.cost) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
    }

    /// Share of prompt tokens served from cache, 0..=1.
    pub fn hit_rate(&self) -> f64 {
        if self.prompt_tokens == 0 { 0.0 } else { self.cached_tokens as f64 / self.prompt_tokens as f64 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_openrouter_cache_and_cost() {
        let u = Usage::parse(&json!({
            "prompt_tokens": 1000, "completion_tokens": 50, "cost": 0.0012,
            "prompt_tokens_details": { "cached_tokens": 800, "cache_write_tokens": 150 },
        }));
        assert_eq!((u.prompt_tokens, u.completion_tokens, u.cached_tokens, u.cache_write_tokens), (1000, 50, 800, 150));
        assert_eq!(u.cost, Some(0.0012));
        assert!((u.hit_rate() - 0.8).abs() < 1e-9);
    }

    #[test]
    fn reads_deepseek_hits_and_tolerates_bare_usage() {
        let u = Usage::parse(&json!({ "prompt_tokens": 10, "completion_tokens": 2, "prompt_cache_hit_tokens": 6 }));
        assert_eq!(u.cached_tokens, 6);
        let u = Usage::parse(&json!({ "prompt_tokens": 10, "prompt_tokens_details": null }));
        assert_eq!((u.cached_tokens, u.cache_write_tokens, u.cost), (0, 0, None));
    }

    #[test]
    fn cost_sums_only_when_reported() {
        let mut t = Usage::default();
        t.add(&Usage { prompt_tokens: 5, ..Default::default() });
        assert_eq!(t.cost, None);
        t.add(&Usage { cost: Some(0.5), ..Default::default() });
        t.add(&Usage { cost: Some(0.25), ..Default::default() });
        assert_eq!(t.cost, Some(0.75));
    }
}
