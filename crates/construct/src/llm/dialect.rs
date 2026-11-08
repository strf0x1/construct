//! What one endpoint needs beyond the plain OpenAI request body.
//!
//! The design choice: the client builds one standard body, and a `Provider`
//! then edits that JSON for its dialect. Every provider quirk lives here as a
//! field on `Provider` and a few lines in `shape`, never as a branch in the
//! client, the agent or the learning pass. The plain `openai` dialect leaves
//! the body untouched, so local and unknown servers see only standard fields.
//! Responses need no dialect: `usage::Usage::parse` reads every provider's
//! fields.

use super::{Message, Request};
use crate::config::{CacheMode, Dialect, ProviderConfig};
use crate::endpoint::Endpoint;
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

    /// The dialect `ep` speaks.
    pub fn of(ep: &Endpoint) -> Self {
        Self::new(&ep.provider, &ep.base_url)
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
        match self.breakpoint(model) {
            Breakpoint::None => {
                shape_part_marks(body, false);
            }
            Breakpoint::Request => body["cache_control"] = ephemeral(),
            Breakpoint::LastPrompt => {
                // A strategy that marked a part knows where its prefix ends;
                // only that one breakpoint survives.
                if !shape_part_marks(body, true) {
                    mark_last_prompt(body);
                }
            }
        }
    }
}

/// The body every OpenAI-compatible endpoint accepts.
pub(super) fn standard_body(req: &Request<'_>) -> Value {
    let mut body = json!({
        "model": req.model,
        "messages": req.messages.iter().map(Message::wire).collect::<Vec<_>>(),
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if !req.tools.is_empty() {
        body["tools"] = json!(req.tools);
    }
    if let Some(m) = req.max_tokens {
        body["max_tokens"] = json!(m);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    body
}

pub(super) fn ephemeral() -> Value {
    json!({ "type": "ephemeral" })
}

/// Remove the `cache_control` of every marked content part, keeping only the
/// last marked one when `keep_last`. Returns whether the body had any.
fn shape_part_marks(body: &mut Value, keep_last: bool) -> bool {
    let Some(msgs) = body["messages"].as_array_mut() else { return false };
    let mut marks = Vec::new();
    for m in msgs.iter_mut() {
        let Some(parts) = m.get_mut("content").and_then(Value::as_array_mut) else { continue };
        for part in parts.iter_mut() {
            if part.get("cache_control").is_some() {
                marks.push(part);
            }
        }
    }
    let n = marks.len();
    for (i, part) in marks.into_iter().enumerate() {
        let remove = !keep_last || i + 1 != n;
        if remove && let Some(obj) = part.as_object_mut() {
            obj.remove("cache_control");
        }
    }
    n > 0
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

    /// Two marked parts in different messages, so a single-breakpoint
    /// provider has something to remove.
    fn marked() -> Value {
        json!({ "model": "m", "messages": [
            { "role": "system", "content": [
                { "type": "text", "text": "sys", "cache_control": ephemeral() },
            ]},
            { "role": "user", "content": [
                { "type": "text", "text": "a" },
                { "type": "text", "text": "b", "cache_control": ephemeral() },
            ]},
            { "role": "tool", "content": "out", "tool_call_id": "1" },
        ]})
    }

    /// The same messages with every part mark removed.
    fn unmarked() -> Value {
        json!({ "model": "m", "messages": [
            { "role": "system", "content": [{ "type": "text", "text": "sys" }] },
            { "role": "user", "content": [{ "type": "text", "text": "a" }, { "type": "text", "text": "b" }] },
            { "role": "tool", "content": "out", "tool_call_id": "1" },
        ]})
    }

    fn marks(v: &Value) -> usize {
        match v {
            Value::Object(o) => {
                o.keys().filter(|k| *k == "cache_control").count() + o.values().map(marks).sum::<usize>()
            }
            Value::Array(a) => a.iter().map(marks).sum(),
            _ => 0,
        }
    }

    #[test]
    fn a_plain_endpoint_never_sees_a_part_mark() {
        let mut b = marked();
        provider(Dialect::OpenAi, CacheMode::Auto, "http://localhost/v1").shape("anthropic/claude", &mut b);
        assert_eq!(marks(&b), 0);
        assert_eq!(b, unmarked());
    }

    #[test]
    fn request_keeps_part_marks_and_adds_the_top_level_one() {
        let mut b = marked();
        provider(Dialect::OpenRouter, CacheMode::Auto, "").shape("anthropic/claude-haiku-5.5", &mut b);
        assert_eq!(b["cache_control"], ephemeral());
        assert_eq!(b["messages"], marked()["messages"]);
    }

    #[test]
    fn last_prompt_keeps_only_the_last_part_mark() {
        let mut b = marked();
        provider(Dialect::OpenRouter, CacheMode::Auto, "").shape("google/gemini-3-flash", &mut b);
        assert!(b.get("cache_control").is_none());
        assert_eq!(marks(&b), 1);
        assert_eq!(b["messages"][1]["content"][1]["cache_control"], ephemeral());
        let mut expected = marked();
        expected["messages"][0]["content"][0].as_object_mut().unwrap().remove("cache_control");
        assert_eq!(b, expected);
    }

    #[test]
    fn deepseek_gets_no_part_mark() {
        let mut b = marked();
        provider(Dialect::OpenRouter, CacheMode::Auto, "").shape("deepseek/deepseek-v4.1-flash", &mut b);
        assert!(b.get("cache_control").is_none());
        assert_eq!(marks(&b), 0);
    }
}
