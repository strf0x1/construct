//! A streaming client for any OpenAI-compatible `/chat/completions` endpoint.
//! Hand-rolled SSE parsing keeps the dependency tree small and lets us see
//! every byte: reasoning deltas, tool-call fragments, and the final usage.
//! Provider quirks never live here: `dialect` shapes the body, `usage` reads
//! any provider's accounting.

pub mod dialect;
mod stream;
mod usage;

pub use dialect::Provider;
pub use usage::Usage;

use crate::config::Config;
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Reasoning text some providers stream separately. Kept for the log and
    /// the UI, never sent back (most endpoints reject or ignore it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
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
        Self { role: role.into(), content: Some(text.into()), tool_calls: vec![], tool_call_id: None, reasoning: None }
    }
    pub fn tool(id: &str, text: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(text.into()),
            tool_calls: vec![],
            tool_call_id: Some(id.into()),
            reasoning: None,
        }
    }
    /// The wire form: reasoning stripped, empty assistant content as null.
    fn wire(&self) -> Value {
        let mut v = json!({ "role": self.role });
        match (&self.content, self.role.as_str()) {
            (Some(c), _) => v["content"] = json!(c),
            (None, "assistant") => v["content"] = Value::Null,
            (None, _) => v["content"] = json!(""),
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
}

pub struct Request<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [Value],
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

impl Client {
    pub fn new(cfg: &Config, api_key: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .user_agent(concat!("construct/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key,
            headers: cfg.headers.clone(),
            provider: Provider::new(&cfg.provider, &cfg.base_url),
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

    pub async fn stream(&self, req: Request<'_>, tx: Option<&mpsc::UnboundedSender<Delta>>) -> Result<Completion> {
        self.stream_body(self.body(&req), tx).await
    }

    /// The request body exactly as it goes on the wire, shaped for this
    /// endpoint's provider.
    pub fn body(&self, req: &Request<'_>) -> Vec<u8> {
        let mut body = standard_body(req);
        self.provider.shape(req.model, &mut body);
        serde_json::to_vec(&body).unwrap_or_default()
    }

    /// Stream a request already serialized with [`Client::body`], so the caller can
    /// record its size without serializing it twice.
    pub async fn stream_body(&self, body: Vec<u8>, tx: Option<&mpsc::UnboundedSender<Delta>>) -> Result<Completion> {
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
            let retryable = match &res {
                Ok(r) => r.status().as_u16() == 429 || r.status().is_server_error(),
                Err(e) => e.is_connect() || e.is_timeout(),
            };
            if retryable && attempt < 4 {
                attempt += 1;
                let wait = res
                    .as_ref()
                    .ok()
                    .and_then(|r| r.headers().get("retry-after"))
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(|s| s.min(30) * 1000)
                    .unwrap_or(500 * 2u64.pow(attempt));
                tokio::time::sleep(Duration::from_millis(wait)).await;
                continue;
            }
            let r = res.context("model request failed")?;
            if !r.status().is_success() {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                bail!("model endpoint returned {status}: {}", truncate(&text, 600));
            }
            break r;
        };
        if let Some(tx) = tx {
            let _ = tx.send(Delta::Opened);
        }

        let mut acc = stream::Accumulator::default();
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        'read: while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("model stream interrupted")?;
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let line = line.trim_end_matches(['\r', '\n']);
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim_start();
                if data == "[DONE]" {
                    break 'read;
                }
                if data.is_empty() {
                    continue;
                }
                let v: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(err) = v.get("error") {
                    bail!("model endpoint error: {}", truncate(&err.to_string(), 600));
                }
                acc.feed(&v, tx);
            }
        }
        acc.finish()
    }

    /// One non-streaming completion; used by the learning pass.
    pub async fn complete(&self, req: Request<'_>) -> Result<Completion> {
        self.stream(req, None).await
    }
}

/// The body every OpenAI-compatible endpoint accepts.
fn standard_body(req: &Request<'_>) -> Value {
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

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}
