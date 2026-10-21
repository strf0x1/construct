//! web_fetch and web_search. Fetch turns HTML into markdown in-process.
//! Search uses Exa or Brave when a key resolves, otherwise DuckDuckGo's HTML
//! endpoint, so it works with no setup at all.

use super::{Ctx, Display, Group, Output, Tool, ToolError, def, obj};
use serde_json::{Value, json};
use std::time::Duration;

const FETCH_MAX_BYTES: usize = 5 * 1024 * 1024;

pub struct WebSearch;

impl WebSearch {
    pub const NAME: &str = "web_search";
}

#[async_trait::async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn group(&self) -> Group {
        Group::Web
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Search the web. Returns titles, URLs and snippets.",
            obj(json!({ "query": { "type": "string" }, "limit": { "type": "integer" } }), &["query"]),
        )
    }
    fn readonly(&self) -> bool {
        true
    }
    fn display(&self) -> Display {
        Display { primary_arg: "query", ..Display::default() }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        search(ctx, &a).await.map(Output::plain).map_err(ToolError::from)
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

async fn search(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let q = a.get("query").and_then(Value::as_str).ok_or("query is required")?;
    let n = a.get("limit").and_then(Value::as_u64).unwrap_or(8).clamp(1, 20) as usize;
    let results = if let Some(key) = crate::secrets::get("EXA_API_KEY") {
        exa(ctx, &key, q, n).await?
    } else if let Some(key) = crate::secrets::get("BRAVE_API_KEY") {
        brave(ctx, &key, q, n).await?
    } else {
        ddg(ctx, q, n).await?
    };
    if results.is_empty() {
        return Ok("no results".into());
    }
    Ok(results
        .iter()
        .enumerate()
        .map(|(i, (t, u, s))| format!("{}. {t}\n   {u}\n   {}", i + 1, crate::llm::truncate(s, 400)))
        .collect::<Vec<_>>()
        .join("\n"))
}

type Hit = (String, String, String);

async fn exa(ctx: &Ctx, key: &str, q: &str, n: usize) -> Result<Vec<Hit>, String> {
    let v: Value = ctx
        .http
        .post("https://api.exa.ai/search")
        .header("x-api-key", key)
        .json(&json!({ "query": q, "numResults": n, "contents": { "highlights": { "numSentences": 2 } } }))
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(v["results"]
        .as_array()
        .map(|r| {
            r.iter()
                .map(|x| {
                    let snippet = x["highlights"]
                        .as_array()
                        .map(|h| h.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
                        .unwrap_or_default();
                    (
                        x["title"].as_str().unwrap_or("").to_string(),
                        x["url"].as_str().unwrap_or("").to_string(),
                        snippet,
                    )
                })
                .collect()
        })
        .unwrap_or_default())
}

async fn brave(ctx: &Ctx, key: &str, q: &str, n: usize) -> Result<Vec<Hit>, String> {
    let v: Value = ctx
        .http
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("X-Subscription-Token", key)
        .query(&[("q", q), ("count", &n.to_string())])
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(v.pointer("/web/results")
        .and_then(Value::as_array)
        .map(|r| {
            r.iter()
                .map(|x| {
                    (
                        strip_tags(x["title"].as_str().unwrap_or("")),
                        x["url"].as_str().unwrap_or("").to_string(),
                        strip_tags(x["description"].as_str().unwrap_or("")),
                    )
                })
                .collect()
        })
        .unwrap_or_default())
}

async fn ddg(ctx: &Ctx, q: &str, n: usize) -> Result<Vec<Hit>, String> {
    let html = ctx
        .http
        .post("https://html.duckduckgo.com/html/")
        .header("user-agent", "Mozilla/5.0 (X11; Linux) construct")
        .form(&[("q", q)])
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for block in html.split("class=\"result__a\"").skip(1) {
        let href = between(block, "href=\"", "\"").unwrap_or_default();
        let title = between(block, ">", "</a>").map(strip_tags).unwrap_or_default();
        let snippet = block
            .split("class=\"result__snippet\"")
            .nth(1)
            .and_then(|s| between(s, ">", "</a>"))
            .map(strip_tags)
            .unwrap_or_default();
        let url = match href.split("uddg=").nth(1) {
            Some(enc) => percent_decode(enc.split('&').next().unwrap_or(enc)),
            None => href.to_string(),
        };
        if url.contains("duckduckgo.com/y.js") || title.is_empty() {
            continue; // ads
        }
        out.push((decode_entities(&title), url, decode_entities(&snippet)));
        if out.len() >= n {
            break;
        }
    }
    if out.is_empty() && html.contains("anomaly") {
        return Err(
            "DuckDuckGo rate-limited this request; set EXA_API_KEY or BRAVE_API_KEY with `construct secrets set`"
                .into(),
        );
    }
    Ok(out)
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(&s[i..j])
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}
