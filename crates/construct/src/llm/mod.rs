//! A streaming client for any OpenAI-compatible `/chat/completions` endpoint.
//! Hand-rolled SSE parsing keeps the dependency tree small and lets us see
//! every byte: reasoning deltas, tool-call fragments, and the final usage.
//! Provider quirks never live here: `dialect` shapes the body, `usage` reads
//! any provider's accounting.

pub mod catalog;
pub mod deck;
pub mod dialect;
pub mod discover;
pub mod notes;
pub mod overlay;
mod stream;
mod switch;
mod usage;

pub use dialect::Provider;
pub use usage::Usage;

use crate::endpoint::Endpoint;
use crate::journal::notes::Coalescer;
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// The message as content parts, sent instead of `content` when non-empty.
    /// A marked part is where the provider should cache (ADR 0043 decision 6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Reasoning text some providers stream separately. Kept for the log and
    /// the UI, never sent back (most endpoints reject or ignore it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

/// One text part of a [`Message`]. `mark` asks the dialect to put the cache
/// breakpoint on this part; only a strategy that splits a message sets it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Part {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mark: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_type")]
    pub kind: String,
    pub function: FunctionCall,
}

fn function_type() -> String {
    "function".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self::text("system", text)
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self::text("user", text)
    }
    pub fn text(role: &str, text: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(text.into()),
            parts: vec![],
            tool_calls: vec![],
            tool_call_id: None,
            reasoning: None,
        }
    }
    /// A message sent as content parts rather than one string.
    pub fn parts(role: &str, parts: Vec<Part>) -> Self {
        Self { role: role.into(), content: None, parts, tool_calls: vec![], tool_call_id: None, reasoning: None }
    }
    pub fn tool(id: &str, text: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(text.into()),
            parts: vec![],
            tool_calls: vec![],
            tool_call_id: Some(id.into()),
            reasoning: None,
        }
    }
    /// The wire form: reasoning stripped, empty assistant content as null.
    fn wire(&self) -> Value {
        let mut v = json!({ "role": self.role });
        if self.parts.is_empty() {
            match (&self.content, self.role.as_str()) {
                (Some(c), _) => v["content"] = json!(c),
                (None, "assistant") => v["content"] = Value::Null,
                (None, _) => v["content"] = json!(""),
            }
        } else {
            let parts: Vec<Value> = self
                .parts
                .iter()
                .filter(|p| !p.text.is_empty())
                .map(|p| {
                    let mut part = json!({ "type": "text", "text": p.text });
                    if p.mark {
                        part["cache_control"] = dialect::ephemeral();
                    }
                    part
                })
                .collect();
            v["content"] = json!(parts);
        }
        if !self.tool_calls.is_empty() {
            v["tool_calls"] = json!(self.tool_calls);
        }
        if let Some(id) = &self.tool_call_id {
            v["tool_call_id"] = json!(id);
        }
        v
    }
}

#[derive(Debug, Clone)]
pub enum Delta {
    Text(String),
    Reasoning(String),
    /// A tool call's name is known; its arguments are still streaming.
    ToolStarted {
        name: String,
    },
    /// The response stream has opened (first byte from the endpoint).
    Opened,
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub message: Message,
    pub usage: Usage,
    pub finish_reason: String,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    pub base_url: String,
    pub api_key: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub provider: Provider,
    prefix: String, // `Endpoint::error_prefix`, before every error
}

pub struct Request<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [Value],
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

impl Client {
    pub fn new(ep: &Endpoint, api_key: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .user_agent(concat!("construct/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base_url: ep.base_url.clone(),
            api_key,
            headers: ep.headers.clone(),
            provider: Provider::of(ep),
            prefix: ep.error_prefix(),
        })
    }

    /// Open a connection to the endpoint ahead of the first request so the
    /// TLS handshake is off the critical path.
    pub fn warm(&self) {
        let http = self.http.clone();
        let url = format!("{}/models", self.base_url);
        let key = self.api_key.clone();
        tokio::spawn(async move {
            // HEAD: the connection is the point, not OpenRouter's model list.
            let mut rb = http.head(url).timeout(Duration::from_secs(10));
            if let Some(k) = key {
                rb = rb.bearer_auth(k);
            }
            let _ = rb.send().await.map(|r| r.bytes());
        });
    }

    /// A call whose notes nobody reads; the agent loop uses [`Client::stream_body`].
    pub async fn stream(&self, req: Request<'_>, tx: Option<&mpsc::UnboundedSender<Delta>>) -> Result<Completion> {
        self.stream_body(self.body(&req), tx, &mut Coalescer::default()).await
    }

    /// The request body exactly as it goes on the wire, shaped for this
    /// endpoint's provider.
    pub fn body(&self, req: &Request<'_>) -> Vec<u8> {
        let mut body = dialect::standard_body(req);
        self.provider.shape(req.model, &mut body);
        serde_json::to_vec(&body).unwrap_or_default()
    }

    /// Stream a request already serialized with [`Client::body`], so the caller can
    /// record its size without serializing it twice. What went wrong without
    /// failing the call (retries, skipped chunks, a cut stream) goes to `notes`.
    pub async fn stream_body(
        &self,
        body: Vec<u8>,
        tx: Option<&mpsc::UnboundedSender<Delta>>,
        notes: &mut Coalescer,
    ) -> Result<Completion> {
        let mut attempt = 0u32;
        let resp = loop {
            let mut rb = self
                .http
                .post(format!("{}/chat/completions", self.base_url))
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.clone());
            if let Some(k) = &self.api_key {
                rb = rb.bearer_auth(k);
            }
            for (k, v) in &self.headers {
                rb = rb.header(k, v);
            }
            let res = rb.send().await;
            let r = match res {
                Ok(r) => r,
                Err(e) => {
                    // A transport error has no body to classify.
                    if (e.is_connect() || e.is_timeout()) && attempt < 4 {
                        attempt += 1;
                        notes.record(notes::RETRY, if e.is_timeout() { "timeout" } else { "connect error" });
                        tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt))).await;
                        continue;
                    }
                    return Err(e).with_context(|| format!("{}model request failed", self.prefix));
                }
            };
            let status = r.status();
            if status.is_success() {
                break r;
            }
            // The body decides retryability for a Deck gateway: a 503
            // `deck_credential_missing` is policy and must not be retried,
            // while a provider 503 still is (ADR 0004 §5, #166).
            let retry_after =
                r.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<u64>().ok());
            let approval = r.headers().get("x-deck-approval").and_then(|v| v.to_str().ok()).map(str::to_string);
            let text = r.text().await.unwrap_or_default();
            let denial = deck::Denial::parse(status.as_u16(), approval.as_deref(), &text);
            let retryable = denial.as_ref().is_some_and(deck::Denial::retryable)
                || (denial.is_none() && (status.as_u16() == 429 || status.is_server_error()));
            if retryable && attempt < 4 {
                attempt += 1;
                notes.record(notes::RETRY, &format!("status {status}"));
                let wait = retry_after.map(|s| s.min(30) * 1000).unwrap_or_else(|| 500 * 2u64.pow(attempt));
                tokio::time::sleep(Duration::from_millis(wait)).await;
                continue;
            }
            if status.as_u16() == 400
                && let Some(p) = notes::PARAMS.iter().find(|p| text.contains(**p))
            {
                notes.record(notes::PARAM_REJECTED, &format!("{p}: {}", truncate(&text, 200)));
            }
            if let Some(d) = denial {
                let d = anyhow::Error::new(d);
                return Err(if self.prefix.is_empty() { d } else { d.context(self.prefix.clone()) });
            }
            bail!("{}model endpoint returned {status}: {}", self.prefix, truncate(&text, 600));
        };
        if let Some(tx) = tx {
            let _ = tx.send(Delta::Opened);
        }

        let mut acc = stream::Accumulator::default();
        let mut sse = stream::Sse::default();
        let mut stream = resp.bytes_stream();
        let mut done = false;
        'read: while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("model stream interrupted")?;
            for data in sse.push(&chunk) {
                if acc.data(&data, tx, notes)? {
                    done = true;
                    break 'read;
                }
            }
        }
        if !done && !acc.finished() {
            notes.record(notes::TRUNCATED, "stream ended without a finish reason or [DONE]");
        }
        let c = acc.finish()?;
        if c.finish_reason == "length" && !c.message.tool_calls.is_empty() {
            notes.record(notes::TRUNCATED, &format!("length cutoff with {} tool call(s)", c.message.tool_calls.len()));
        }
        Ok(c)
    }

    /// One non-streaming completion; used by the learning pass.
    pub async fn complete(&self, req: Request<'_>) -> Result<Completion> {
        self.stream(req, None).await
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    format!("{}…", crate::text::head(s, max))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(text: &str, mark: bool) -> Part {
        Part { text: text.into(), mark }
    }

    #[test]
    fn a_message_without_parts_serializes_as_before() {
        let v = serde_json::to_value(Message::user("hi")).unwrap();
        assert!(v.get("parts").is_none());
        // Old journal rows have no `parts` key and still load.
        let old: Message = serde_json::from_value(json!({ "role": "user", "content": "hi" })).unwrap();
        assert_eq!(old, Message::user("hi"));
    }

    #[test]
    fn marked_parts_wire_as_content_parts() {
        let m = Message::parts("user", vec![part("a", false), part("b", true)]);
        let v = m.wire();
        assert_eq!(v["content"][0], json!({ "type": "text", "text": "a" }));
        assert_eq!(v["content"][1], json!({ "type": "text", "text": "b", "cache_control": { "type": "ephemeral" } }));
    }

    #[test]
    fn empty_parts_are_skipped() {
        let m = Message::parts("user", vec![part("a", true), part("", false), part("", true)]);
        let v = m.wire();
        assert_eq!(v["content"].as_array().unwrap().len(), 1);
        assert_eq!(v["content"][0]["text"], "a");
    }
}
