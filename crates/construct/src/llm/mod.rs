//! A streaming client for any OpenAI-compatible `/chat/completions` endpoint.
//! Hand-rolled SSE parsing keeps the dependency tree small and lets us see
//! every byte: reasoning deltas, tool-call fragments, and the final usage.
//! Provider quirks never live here: `dialect` shapes the body, `usage` reads
//! any provider's accounting.

pub mod catalog;
pub mod deck;
pub mod dialect;
pub mod discover;
mod message;
pub mod notes;
pub mod overlay;
mod stream;
mod switch;
mod usage;

pub use dialect::Provider;
pub use message::{FunctionCall, Message, Part, ToolCall};
pub use usage::Usage;

use crate::endpoint::Endpoint;
use crate::journal::notes::Coalescer;
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::mpsc;

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
    /// How long a request may wait for its first bytes, and how long the
    /// stream may then go quiet, before the call fails.
    first_byte: Duration,
    idle: Duration,
}

/// Default `[provider] idle_timeout` and `first_byte_timeout`, in seconds.
const IDLE_SECS: u64 = 120;
const FIRST_BYTE_SECS: u64 = 600;

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
            first_byte: Duration::from_secs(ep.provider.first_byte_timeout.unwrap_or(FIRST_BYTE_SECS)),
            idle: Duration::from_secs(ep.provider.idle_timeout.unwrap_or(IDLE_SECS)),
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
        let (resp, first_deadline) = loop {
            // Nothing a stalled server does may hang the turn: the response
            // and its first bytes get one deadline, every later chunk `idle`.
            let first_deadline = tokio::time::Instant::now() + self.first_byte;
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
            let Ok(res) = tokio::time::timeout_at(first_deadline, rb.send()).await else {
                bail!(
                    "{}model endpoint sent no response in {}s ([provider] first_byte_timeout)",
                    self.prefix,
                    self.first_byte.as_secs()
                );
            };
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
                break (r, first_deadline);
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
        let (mut deadline, mut started) = (first_deadline, false);
        'read: loop {
            let Ok(next) = tokio::time::timeout_at(deadline, stream.next()).await else {
                let (what, knob) =
                    if !started { (self.first_byte, "first_byte_timeout") } else { (self.idle, "idle_timeout") };
                bail!("{}model stream stalled: nothing for {}s ([provider] {knob})", self.prefix, what.as_secs());
            };
            let Some(chunk) = next else { break };
            (deadline, started) = (tokio::time::Instant::now() + self.idle, true);
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
