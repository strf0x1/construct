//! web_fetch and web_search. Fetch turns HTML into markdown in-process.
//! Search (`search.rs`) uses Exa, Brave or Tavily when a key resolves,
//! otherwise DuckDuckGo's HTML endpoint (`ddg.rs`), so it works with no setup
//! at all. `http.rs` is the one place a search API's failures become prose.

mod ddg;
mod filter;
mod http;
mod search;
mod select;

pub use select::{Backend, Choice};

use super::{Ctx, Display, Group, Output, Tool, ToolError, ToolKind, def, obj};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

const FETCH_MAX_BYTES: usize = 5 * 1024 * 1024;

pub struct WebSearch {
    choice: Choice,
}

impl WebSearch {
    pub const NAME: &str = "web_search";

    /// Errors on a backend name nothing implements, at startup, not mid-turn.
    pub fn new(cfg: &WebConfig) -> anyhow::Result<Self> {
        Ok(Self { choice: Choice::parse(&cfg.search).map_err(anyhow::Error::msg)? })
    }
}

#[async_trait::async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Search
    }
    fn group(&self) -> Group {
        Group::Web
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Search the web. Returns titles, URLs, dates when known, and snippets.",
            obj(
                json!({
                    "query": { "type": "string" },
                    "limit": { "type": "integer" },
                    "recency": { "type": "string", "enum": filter::Recency::NAMES, "description": "Only results from the last day, week, month or year" },
                    "domains": { "type": "array", "items": { "type": "string" }, "description": "Only these sites, e.g. docs.rs" }
                }),
                &["query"],
            ),
        )
    }
    fn readonly(&self) -> bool {
        true
    }
    fn display(&self) -> Display {
        Display { primary_arg: "query", ..Display::default() }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        search::run(ctx, &a, self.choice).await.map(Output::plain).map_err(ToolError::from)
    }
}

pub struct WebFetch;

impl WebFetch {
    pub const NAME: &str = "web_fetch";
}

#[async_trait::async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Fetch
    }
    fn group(&self) -> Group {
        Group::Web
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Fetch a URL and return it as markdown (HTML is converted).",
            obj(json!({ "url": { "type": "string" } }), &["url"]),
        )
    }
    fn readonly(&self) -> bool {
        true
    }
    fn display(&self) -> Display {
        Display { primary_arg: "url", ..Display::default() }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        fetch(ctx, &a).await.map(Output::plain).map_err(ToolError::from)
    }
}

async fn fetch(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let url = a.get("url").and_then(Value::as_str).ok_or("url is required")?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("url must start with http:// or https://".into());
    }
    let resp = ctx
        .http
        .get(url)
        .header("accept", "text/markdown, text/html;q=0.9, text/plain;q=0.8, */*;q=0.5")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("fetch failed: {e}"))?;
    let status = resp.status();
    let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() > FETCH_MAX_BYTES {
        return Err(format!("response is {} bytes, over the 5MB limit", bytes.len()));
    }
    let body = String::from_utf8_lossy(&bytes);
    let text = if ctype.contains("html") || body.trim_start().starts_with('<') {
        html_to_markdown(&body)
    } else {
        body.to_string()
    };
    let text = super::cap_output(ctx, "web", &text);
    if !status.is_success() {
        return Err(format!("HTTP {status}\n{text}"));
    }
    Ok(text)
}

fn html_to_markdown(html: &str) -> String {
    let conv = htmd::HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "noscript", "svg", "nav", "footer", "iframe", "form"])
        .build();
    let md = conv.convert(html).unwrap_or_else(|_| html.to_string());
    // Collapse the blank-line runs that layout markup leaves behind.
    let mut out = String::with_capacity(md.len());
    let mut blank = 0;
    for line in md.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Knobs for the web tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Which backend answers `web_search`: "auto" (the keyed backends in
    /// order Exa, Brave, Tavily, else DuckDuckGo) or one of "exa", "brave",
    /// "tavily", "ddg". A named backend is the only one tried.
    pub search: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self { search: "auto".into() }
    }
}
