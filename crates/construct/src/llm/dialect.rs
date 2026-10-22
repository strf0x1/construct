//! What one endpoint needs beyond the plain OpenAI request body.
//!
//! The design choice: the client builds one standard body, and a `Provider`
//! then edits that JSON for its dialect. Every provider quirk lives here as a
//! field on `Provider` and a few lines in `shape`, never as a branch in the
//! client, the agent or the learning pass. The plain `openai` dialect leaves
//! the body untouched, so local and unknown servers see only standard fields.
//! Responses need no dialect: `usage::Usage::parse` reads every provider's
//! fields.

use crate::config::{CacheMode, Dialect, ProviderConfig};
use serde_json::{Value, json};

/// Where a request marks its cache breakpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breakpoint {
    None,
    /// A top-level `cache_control`: the provider puts it on the last
    /// cacheable block, so every request extends the cached prefix
    /// (OpenRouter's automatic caching for Anthropic).
    Request,
    /// `cache_control` on the newest system or user content part. Gemini
    /// behind OpenRouter honours only the last breakpoint, and documents it
    /// on those roles.
    LastPrompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    /// Resolved: never `Dialect::Auto`.
    pub dialect: Dialect,
    pub cache: CacheMode,
}

impl Provider {
    pub fn new(cfg: &ProviderConfig, base_url: &str) -> Self {
        let dialect = match cfg.dialect {
            Dialect::Auto if host(base_url).is_some_and(|h| h == "openrouter.ai" || h.ends_with(".openrouter.ai")) => {
                Dialect::OpenRouter
            }
            Dialect::Auto => Dialect::OpenAi,
            d => d,
        };
        Self { dialect, cache: cfg.cache }
    }

    pub fn name(&self) -> &'static str {
        match self.dialect {
            Dialect::OpenRouter => "openrouter",
            Dialect::OpenAi | Dialect::Auto => "openai",
        }
    }

    /// Which breakpoint a request for `model` carries. Model ids are
    /// OpenRouter's `vendor/name`.
    pub fn breakpoint(&self, model: &str) -> Breakpoint {
        if self.cache == CacheMode::Off || self.dialect != Dialect::OpenRouter {
            return Breakpoint::None;
        }
        match model.split_once('/').map(|(vendor, _)| vendor) {
            Some("anthropic") => Breakpoint::Request,
            Some("google") => Breakpoint::LastPrompt,
            _ => Breakpoint::None,
        }
    }

    /// Adjust a standard body for this provider.
    pub fn shape(&self, model: &str, body: &mut Value) {
        if self.dialect == Dialect::OpenRouter {
            // Cost and cache counts in the final usage chunk.
            body["usage"] = json!({ "include": true });
        }
        match self.breakpoint(model) {
            Breakpoint::None => {}
            Breakpoint::Request => body["cache_control"] = ephemeral(),
            Breakpoint::LastPrompt => mark_last_prompt(body),
        }
    }
}

fn ephemeral() -> Value {
    json!({ "type": "ephemeral" })
}

/// Turn the newest system/user message's text into one content part that
/// carries the breakpoint.
fn mark_last_prompt(body: &mut Value) {
    let Some(msgs) = body["messages"].as_array_mut() else { return };
    let last = msgs.iter_mut().rev().find(|m| matches!(m["role"].as_str(), Some("system" | "user")));
    if let Some(m) = last
        && let Some(text) = m["content"].as_str().filter(|t| !t.is_empty())
    {
        m["content"] = json!([{ "type": "text", "text": text, "cache_control": ephemeral() }]);
    }
}

fn host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split('/').next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    Some(host.split(':').next().unwrap_or(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(dialect: Dialect, cache: CacheMode, url: &str) -> Provider {
        Provider::new(&ProviderConfig { dialect, cache }, url)
    }

    fn body() -> Value {
        json!({ "model": "m", "messages": [
            { "role": "system", "content": "sys" },
            { "role": "user", "content": "hi" },
            { "role": "assistant", "content": null, "tool_calls": [] },
            { "role": "tool", "content": "out", "tool_call_id": "1" },
        ]})
    }

    #[test]
    fn auto_dialect_follows_the_host() {
        let auto = |u| provider(Dialect::Auto, CacheMode::Auto, u).dialect;
        assert_eq!(auto("https://openrouter.ai/api/v1"), Dialect::OpenRouter);
        assert_eq!(auto("http://127.0.0.1:8080/v1"), Dialect::OpenAi);
        assert_eq!(auto("https://evil.example/openrouter.ai/v1"), Dialect::OpenAi);
        assert_eq!(
            provider(Dialect::OpenRouter, CacheMode::Auto, "http://127.0.0.1:1/v1").dialect,
            Dialect::OpenRouter
        );
    }

    #[test]
    fn plain_openai_body_is_untouched() {
        let mut b = body();
        provider(Dialect::OpenAi, CacheMode::Auto, "http://localhost/v1").shape("anthropic/claude", &mut b);
        assert_eq!(b, body());
    }

    #[test]
    fn anthropic_gets_a_request_breakpoint_and_gemini_the_last_prompt() {
        let or = provider(Dialect::OpenRouter, CacheMode::Auto, "");
        let mut b = body();
        or.shape("anthropic/claude-haiku-5.5", &mut b);
        assert_eq!(b["cache_control"], ephemeral());
        assert_eq!(b["usage"]["include"], true);
        assert_eq!(b["messages"], body()["messages"]);

        let mut b = body();
        or.shape("google/gemini-3-flash", &mut b);
        assert!(b.get("cache_control").is_none());
        assert_eq!(b["messages"][1]["content"][0]["cache_control"], ephemeral());
        assert_eq!(b["messages"][1]["content"][0]["text"], "hi");
        assert_eq!(b["messages"][0]["content"], "sys");

        let mut b = body();
        or.shape("deepseek/deepseek-v4.1-flash", &mut b);
        assert!(b.get("cache_control").is_none() && b["messages"] == body()["messages"]);
    }

    #[test]
    fn cache_off_marks_nothing() {
        let off = provider(Dialect::OpenRouter, CacheMode::Off, "");
        assert_eq!(off.breakpoint("anthropic/claude"), Breakpoint::None);
        assert_eq!(off.breakpoint("google/gemini"), Breakpoint::None);
    }
}
