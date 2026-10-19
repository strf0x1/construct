//! A streaming client for any OpenAI-compatible `/chat/completions` endpoint.
//! Hand-rolled SSE parsing keeps the dependency tree small and lets us see
//! every byte: reasoning deltas, tool-call fragments, and the final usage.

use anyhow::{Context, Result, anyhow, bail};
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
        Self { role: "tool".into(), content: Some(text.into()), tool_calls: vec![], tool_call_id: Some(id.into()), reasoning: None }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.prompt_tokens += o.prompt_tokens;
        self.completion_tokens += o.completion_tokens;
        self.cached_tokens += o.cached_tokens;
    }
}

#[derive(Debug, Clone)]
pub enum Delta {
    Text(String),
    Reasoning(String),
    /// A tool call's name is known; its arguments are still streaming.
    ToolStarted { name: String },
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
}

pub struct Request<'a> {
    pub model: &'a str,
    pub messages: &'a [Message],
    pub tools: &'a [Value],
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

impl Client {
    pub fn new(base_url: &str, api_key: Option<String>, headers: BTreeMap<String, String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_nodelay(true)
            .user_agent(concat!("construct/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { http, base_url: base_url.trim_end_matches('/').to_string(), api_key, headers })
    }

    /// Open a connection to the endpoint ahead of the first request so the
    /// TLS handshake is off the critical path.
    pub fn warm(&self) {
        let http = self.http.clone();
        let url = format!("{}/models", self.base_url);
        let key = self.api_key.clone();
        tokio::spawn(async move {
            let mut rb = http.get(url).timeout(Duration::from_secs(10));
            if let Some(k) = key {
                rb = rb.bearer_auth(k);
            }
            let _ = rb.send().await.map(|r| r.bytes());
        });
    }

    pub async fn stream(&self, req: Request<'_>, tx: Option<&mpsc::UnboundedSender<Delta>>) -> Result<Completion> {
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

        let mut attempt = 0u32;
        let resp = loop {
            let mut rb = self.http.post(format!("{}/chat/completions", self.base_url)).json(&body);
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

        let mut acc = Accumulator::default();
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

#[derive(Default)]
struct Accumulator {
    content: String,
    reasoning: String,
    calls: BTreeMap<usize, (String, String, String)>,
    usage: Usage,
    finish: String,
}

impl Accumulator {
    fn feed(&mut self, v: &Value, tx: Option<&mpsc::UnboundedSender<Delta>>) {
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.usage.prompt_tokens = u["prompt_tokens"].as_u64().unwrap_or(0);
            self.usage.completion_tokens = u["completion_tokens"].as_u64().unwrap_or(0);
            self.usage.cached_tokens = u
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
        }
        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else { return };
        if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = f.to_string();
        }
        let Some(d) = choice.get("delta") else { return };
        if let Some(t) = d.get("content").and_then(Value::as_str) {
            if !t.is_empty() {
                self.content.push_str(t);
                if let Some(tx) = tx {
                    let _ = tx.send(Delta::Text(t.to_string()));
                }
            }
        }
        for key in ["reasoning_content", "reasoning"] {
            if let Some(t) = d.get(key).and_then(Value::as_str) {
                if !t.is_empty() {
                    self.reasoning.push_str(t);
                    if let Some(tx) = tx {
                        let _ = tx.send(Delta::Reasoning(t.to_string()));
                    }
                }
                break;
            }
        }
        if let Some(calls) = d.get("tool_calls").and_then(Value::as_array) {
            for (pos, c) in calls.iter().enumerate() {
                let index = c.get("index").and_then(Value::as_u64).map(|i| i as usize).unwrap_or(pos);
                let entry = self.calls.entry(index).or_default();
                if let Some(id) = c.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        entry.0 = id.to_string();
                    }
                }
                if let Some(f) = c.get("function") {
                    if let Some(n) = f.get("name").and_then(Value::as_str) {
                        if !n.is_empty() && entry.1.is_empty() {
                            entry.1 = n.to_string();
                            if let Some(tx) = tx {
                                let _ = tx.send(Delta::ToolStarted { name: n.to_string() });
                            }
                        }
                    }
                    if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                        entry.2.push_str(a);
                    }
                }
            }
        }
    }

    fn finish(self) -> Result<Completion> {
        let tool_calls: Vec<ToolCall> = self
            .calls
            .into_iter()
            .filter(|(_, (_, name, _))| !name.is_empty())
            .map(|(i, (id, name, args))| ToolCall {
                id: if id.is_empty() { format!("call_{i}") } else { id },
                kind: "function".into(),
                function: FunctionCall { name, arguments: if args.trim().is_empty() { "{}".into() } else { args } },
            })
            .collect();
        if self.content.is_empty() && tool_calls.is_empty() && self.reasoning.is_empty() && self.finish.is_empty() {
            return Err(anyhow!("model returned an empty stream"));
        }
        Ok(Completion {
            message: Message {
                role: "assistant".into(),
                content: if self.content.is_empty() { None } else { Some(self.content) },
                tool_calls,
                tool_call_id: None,
                reasoning: if self.reasoning.is_empty() { None } else { Some(self.reasoning) },
            },
            usage: self.usage,
            finish_reason: self.finish,
        })
    }
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
