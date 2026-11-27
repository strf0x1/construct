//! Model calls a pinned peer makes through the hub (`llm = "hub"`, #371).
//!
//! The design choice: the credential boundary is this file. A peer sends a
//! `/chat/completions` body naming a model selector; the hub resolves that
//! selector against its own config (`Config::endpoint_for`, `wire_id`, ADR
//! 0027), takes the key from its own environment or keyring (ADR 0051),
//! shapes the body for the provider (`Provider::shape`), and relays the
//! provider's status and bytes back unchanged. Nothing the hub resolved goes
//! to the peer but the response itself. Calls belong to one link: when the
//! link ends (a revocation, a drop, shutdown) its `Calls` is dropped and
//! every call in flight is aborted with it. A call the peer cancels stops
//! upstream but still ends normally, so what it used is counted: the engine
//! hangs up as soon as it reads `[DONE]`, often before the upstream body has
//! quite ended. A model override set at the hub (#390, `model.rs`) replaces
//! the peer's selector here, before anything is resolved.

use crate::model;
use construct_core::config::Config;
use construct_core::llm::{Provider, Usage};
use construct_peer::llm::{Frame, Msg};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

/// Model calls one peer may have in flight; more are answered 429.
pub(crate) const MAX_INFLIGHT: usize = 4;

/// The hub's side of every proxied call: its config and one HTTP client.
pub(crate) struct Upstream {
    cfg: Config,
    http: reqwest::Client,
}

impl Upstream {
    pub(crate) fn new(cfg: Config) -> anyhow::Result<Self> {
        let http = construct_core::http::build(
            construct_core::http::engine().connect_timeout(Duration::from_secs(20)).tcp_nodelay(true),
        )?;
        Ok(Self { cfg, http })
    }

    /// The host's config, which every selector is resolved against.
    pub(crate) fn cfg(&self) -> &Config {
        &self.cfg
    }
}

/// What one peer's calls used, summed while the hub runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LlmUsage {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

impl LlmUsage {
    pub(crate) fn add(&mut self, u: Option<&Usage>) {
        self.requests += 1;
        if let Some(u) = u {
            self.prompt_tokens += u.prompt_tokens;
            self.completion_tokens += u.completion_tokens;
            if let Some(c) = u.cost {
                self.cost = Some(self.cost.unwrap_or(0.0) + c);
            }
        }
    }
}

/// A frame for the peer and, with a call's last frame, what it used.
pub(crate) struct Out {
    pub(crate) frame: Frame,
    pub(crate) usage: Option<Option<Usage>>,
}

/// The model calls in flight on one link.
pub(crate) struct Calls {
    up: Arc<Upstream>,
    tasks: JoinSet<()>,
    /// Each live call's cancel switch.
    live: HashMap<u64, oneshot::Sender<()>>,
    tx: mpsc::UnboundedSender<Out>,
    rx: mpsc::UnboundedReceiver<Out>,
}

impl Calls {
    pub(crate) fn new(up: Arc<Upstream>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { up, tasks: JoinSet::new(), live: HashMap::new(), tx, rx }
    }

    /// A request from the peer: refused when `allowed` is false (the peer is
    /// not pinned), the body is not JSON, it cannot fit the window of the
    /// `model` the hub set for this peer, or too many are in flight; else
    /// started. Returns the selector the peer asked for, when it named one.
    pub(crate) fn request(&mut self, id: u64, body: Vec<u8>, allowed: bool, model: Option<&str>) -> Option<String> {
        while self.tasks.try_join_next().is_some() {}
        if !allowed {
            refuse(&self.tx, id, 403, None, "hub: this peer is not pinned; no model calls".into());
            return None;
        }
        let Ok(json @ Value::Object(_)) = serde_json::from_slice::<Value>(&body) else {
            refuse(&self.tx, id, 400, None, "hub: the request body is not a JSON object".into());
            return None;
        };
        let asked = json["model"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        let selector = model.map(str::to_string).or_else(|| asked.clone()).unwrap_or_else(|| self.up.cfg.model.clone());
        if let Some(sel) = model
            && let Some(why) = model::window(&self.up.cfg, sel).and_then(|w| model::too_big(body.len(), w, sel))
        {
            refuse(&self.tx, id, 400, None, why);
            return asked;
        }
        if self.live.len() >= MAX_INFLIGHT {
            let why = format!("hub: {MAX_INFLIGHT} model calls already in flight for this peer");
            refuse(&self.tx, id, 429, Some(1), why);
            return asked;
        }
        let (up, tx) = (self.up.clone(), self.tx.clone());
        let (stop, cancelled) = oneshot::channel();
        self.tasks.spawn(async move {
            let (error, usage) = call(&up, id, json, selector, &tx, cancelled).await;
            let _ = tx.send(Out { frame: Frame::bare(Msg::End { id, error }), usage: Some(usage) });
        });
        self.live.insert(id, stop);
        asked
    }

    /// The peer gave up on `id`: stop it upstream (its usage is still counted).
    pub(crate) fn cancel(&mut self, id: u64) {
        if let Some(stop) = self.live.remove(&id) {
            let _ = stop.send(());
        }
    }

    /// The next frame for the peer. A call's last frame retires it.
    pub(crate) async fn next(&mut self) -> Out {
        loop {
            if let Some(out) = self.rx.recv().await {
                if matches!(out.frame.msg, Msg::End { .. }) {
                    self.live.remove(&out.frame.msg.id());
                }
                return out;
            }
        }
    }
}

/// Answer `id` with a complete error response, OpenAI-shaped.
fn refuse(tx: &mpsc::UnboundedSender<Out>, id: u64, status: u16, retry_after: Option<u64>, message: String) {
    // A window refusal carries the code providers use for one (`model.rs`).
    let code = message.contains("maximum context length").then_some("context_length_exceeded");
    let body = serde_json::json!({ "error": { "message": message, "type": "hub_proxy", "code": code } }).to_string();
    let frames = [Frame::bare(Msg::Head { id, status, retry_after }), Frame::new(Msg::Data { id }, body.into_bytes())];
    for frame in frames {
        let _ = tx.send(Out { frame, usage: None });
    }
    // Refused calls carry no usage and are not counted.
    let _ = tx.send(Out { frame: Frame::bare(Msg::End { id, error: None }), usage: None });
}

/// Make one call for `selector` upstream, relaying its head and bytes to
/// `tx`. Returns how the body ended (an error means it was cut off) and the
/// usage it reported.
async fn call(
    up: &Upstream,
    id: u64,
    mut body: Value,
    selector: String,
    tx: &mpsc::UnboundedSender<Out>,
    mut cancelled: oneshot::Receiver<()>,
) -> (Option<String>, Option<Usage>) {
    const CANCELLED: &str = "cancelled by the peer";
    let send = |frame| {
        let _ = tx.send(Out { frame, usage: None });
    };
    let ep = up.cfg.endpoint_for(&selector);
    let key = match ep.require(ep.key()) {
        Ok(k) => k,
        Err(e) => {
            refuse(tx, id, 403, None, format!("hub: {e:#}"));
            return (None, None);
        }
    };
    let wire = up.cfg.wire_id(&selector);
    body["model"] = Value::String(wire.clone());
    Provider::of(&ep).shape(&wire, &mut body);
    let mut rb = up
        .http
        .post(format!("{}/chat/completions", ep.base_url))
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&body).unwrap_or_default());
    if let Some(k) = key {
        rb = rb.bearer_auth(k);
    }
    for (k, v) in &ep.headers {
        rb = rb.header(k, v);
    }
    let sent = tokio::select! {
        r = rb.send() => r,
        _ = &mut cancelled => return (Some(CANCELLED.into()), None),
    };
    let resp = match sent {
        Ok(r) => r,
        Err(e) => {
            refuse(tx, id, 502, None, format!("hub: {}model request failed: {e}", ep.error_prefix()));
            return (None, None);
        }
    };
    let status = resp.status().as_u16();
    let retry_after =
        resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|s| s.trim().parse::<u64>().ok());
    send(Frame::bare(Msg::Head { id, status, retry_after }));
    let mut scan = UsageScan::default();
    let mut stream = resp.bytes_stream();
    loop {
        let chunk = tokio::select! {
            c = stream.next() => match c {
                Some(c) => c,
                None => break,
            },
            // Dropping the stream hangs up on the provider.
            _ = &mut cancelled => return (Some(CANCELLED.into()), scan.usage),
        };
        match chunk {
            Ok(bytes) => {
                scan.push(&bytes);
                send(Frame::new(Msg::Data { id }, bytes.to_vec()));
            }
            Err(e) => return (Some(format!("upstream stream interrupted: {e}")), scan.usage),
        }
    }
    (None, scan.usage)
}

/// The last `usage` object an SSE stream (or a plain JSON body) carried.
#[derive(Default)]
struct UsageScan {
    line: Vec<u8>,
    usage: Option<Usage>,
}

impl UsageScan {
    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if b != b'\n' {
                // A line past this size is not one to read usage from.
                if self.line.len() < 1 << 20 {
                    self.line.push(b);
                }
                continue;
            }
            let line = std::mem::take(&mut self.line);
            let text = String::from_utf8_lossy(&line);
            let data = text.trim().strip_prefix("data:").map_or(text.trim(), str::trim);
            if let Ok(v) = serde_json::from_str::<Value>(data)
                && let Some(u) = v.get("usage").filter(|u| u.is_object())
            {
                self.usage = Some(Usage::parse(u));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_is_read_from_the_last_sse_chunk_that_carries_it_across_splits() {
        let mut scan = UsageScan::default();
        let sse = "data: {\"choices\":[],\"usage\":null}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"cost\":0.5}}\n\ndata: [DONE]\n\n";
        let (a, b) = sse.split_at(60);
        scan.push(a.as_bytes());
        scan.push(b.as_bytes());
        let u = scan.usage.unwrap();
        assert_eq!((u.prompt_tokens, u.completion_tokens, u.cost), (12, 3, Some(0.5)));
        let mut total = LlmUsage::default();
        total.add(Some(&u));
        total.add(None);
        assert_eq!((total.requests, total.prompt_tokens, total.cost), (2, 12, Some(0.5)));
    }

    fn calls() -> Calls {
        Calls::new(Arc::new(Upstream::new(Config::default()).unwrap()))
    }

    async fn drain(c: &mut Calls, id: u64) -> (u16, String) {
        let mut status = 0;
        let mut body = Vec::new();
        loop {
            let out = c.next().await;
            assert_eq!(out.frame.msg.id(), id);
            match out.frame.msg {
                Msg::Head { status: s, .. } => status = s,
                Msg::Data { .. } => body.extend(out.frame.bytes),
                Msg::End { .. } => return (status, String::from_utf8(body).unwrap()),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_peer_that_is_not_pinned_gets_403_and_nothing_goes_upstream() {
        let mut c = calls();
        c.request(1, b"{\"model\":\"m\"}".to_vec(), false, None);
        let (status, body) = drain(&mut c, 1).await;
        assert_eq!(status, 403);
        assert!(body.contains("not pinned"), "{body}");
        assert!(c.live.is_empty());
    }

    #[tokio::test]
    async fn a_fifth_call_in_flight_is_429_and_a_cancel_frees_a_slot() {
        // An upstream that accepts and never answers keeps calls in flight.
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cfg = Config {
            base_url: format!("http://{}/v1", silent.local_addr().unwrap()),
            api_key_secret: String::new(),
            ..Default::default()
        };
        let mut c = Calls::new(Arc::new(Upstream::new(cfg).unwrap()));
        for id in 1..=MAX_INFLIGHT as u64 {
            c.request(id, b"{\"model\":\"m\"}".to_vec(), true, None);
        }
        c.request(9, b"{\"model\":\"m\"}".to_vec(), true, None);
        let (status, body) = drain(&mut c, 9).await;
        assert_eq!(status, 429, "{body}");
        c.cancel(1);
        assert_eq!(c.live.len(), MAX_INFLIGHT - 1);
        c.request(10, b"{\"model\":\"m\"}".to_vec(), true, None);
        assert_eq!(c.live.len(), MAX_INFLIGHT, "the cancelled call's slot went to the next");
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_refused_400() {
        let mut c = calls();
        c.request(2, b"not json".to_vec(), true, None);
        assert_eq!(drain(&mut c, 2).await.0, 400);
        assert!(c.live.is_empty(), "the end retires the call");
    }

    #[tokio::test]
    async fn an_override_with_a_smaller_window_refuses_what_cannot_fit_before_anything_goes_upstream() {
        // Nothing listens on port 9: a call that went upstream would be 502.
        let cfg: Config = toml::from_str(
            "base_url = \"http://127.0.0.1:9/v1\"\napi_key_secret = \"\"\n[models.small]\ncontext_window = 10\n",
        )
        .unwrap();
        let mut c = Calls::new(Arc::new(Upstream::new(cfg).unwrap()));
        let big = format!("{{\"model\":\"big\",\"messages\":\"{}\"}}", "x".repeat(100));
        let asked = c.request(3, big.clone().into_bytes(), true, Some("small"));
        assert_eq!(asked.as_deref(), Some("big"), "the hub still learns what the peer asked for");
        let (status, body) = drain(&mut c, 3).await;
        assert_eq!(status, 400, "{body}");
        let err: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(err["error"]["code"], "context_length_exceeded", "{body}");
        assert!(body.contains("maximum context length is 10 tokens"), "{body}");
        assert!(c.live.is_empty(), "nothing went upstream");
        // Without the override the same body goes upstream (and fails there).
        c.request(4, big.into_bytes(), true, None);
        assert_eq!(drain(&mut c, 4).await.0, 502);
    }
}
