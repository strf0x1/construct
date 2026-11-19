//! A provider's model list: fetched the provider's way (`llm::discover`),
//! cached on disk per url, and searched by name; `llm::overlay` lays it
//! under the hand-written catalog. OpenRouter states windows, prices and
//! supported parameters; vLLM and llama.cpp state windows; others give ids
//! at best. The design choice a reader must know: a fetch is never on the
//! UI path, and a failed one falls back to a stale cache instead of failing
//! anything.

use super::Client;
use crate::config::{Paths, write_atomic};
use crate::providers::{Discover, Reach};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a fetched list is used without asking again.
const TTL_SECS: u64 = 2 * 60 * 60;
/// One `/models` request, before giving up.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// What one `/models` entry says about a model. Everything but the id is
/// optional because only some endpoints state it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub context: Option<u32>,
    pub max_output: Option<u32>,
    /// USD per million tokens.
    pub prompt_per_m: Option<f64>,
    pub completion_per_m: Option<f64>,
    /// Whether it supports tool calls. An entry that lists no parameters is
    /// assumed to.
    pub tools: bool,
    pub reasoning: bool,
    pub images: bool,
}

#[derive(Serialize, Deserialize)]
pub(super) struct Cached {
    /// Unix seconds of the last fetch that worked; 0 for never.
    pub(super) fetched_at: u64,
    pub(super) models: Vec<ModelInfo>,
    /// The last fetch that failed after it: when, and the cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) failed: Option<(u64, String)>,
}

/// The entries of a `/models` body (`{"data": [...]}` or a bare array), by id.
pub fn parse(body: &Value) -> Vec<ModelInfo> {
    let entries = body.get("data").unwrap_or(body).as_array().map(Vec::as_slice).unwrap_or_default();
    let mut out: Vec<ModelInfo> = entries.iter().filter_map(entry).collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

fn entry(e: &Value) -> Option<ModelInfo> {
    let id = e.get("id")?.as_str().filter(|s| !s.is_empty())?.to_string();
    let name = e.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(&id).to_string();
    let count = |v: Option<&Value>| v.and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0);
    // OpenRouter prices are USD per token, as strings; a router model's
    // price is "-1" (it depends on the pick), which is unknown, not negative.
    let price = |k: &str| {
        let v = e.pointer(&format!("/pricing/{k}"))?;
        let p = v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64())?;
        (p >= 0.0).then_some(p * 1_000_000.0)
    };
    let params = e.get("supported_parameters").and_then(Value::as_array);
    let has = |p: &str| params.is_some_and(|ps| ps.iter().any(|v| v == p));
    let images = e
        .pointer("/architecture/input_modalities")
        .and_then(Value::as_array)
        .is_some_and(|m| m.iter().any(|v| v == "image"));
    Some(ModelInfo {
        id,
        name,
        // vLLM states the window it serves as `max_model_len`, beside the
        // model's trained `context_length`; the served one is what fits.
        context: count(e.get("max_model_len")).or_else(|| count(e.get("context_length"))),
        max_output: count(e.pointer("/top_provider/max_completion_tokens")),
        prompt_per_m: price("prompt"),
        completion_per_m: price("completion"),
        tools: params.is_none() || has("tools"),
        reasoning: has("reasoning"),
        images,
    })
}

/// The entry for `id`, or for the id without an OpenRouter variant suffix
/// (`vendor/model:nitro` is served by `vendor/model`'s window).
pub fn find<'a>(models: &'a [ModelInfo], id: &str) -> Option<&'a ModelInfo> {
    let get = |id: &str| models.iter().find(|m| m.id == id);
    get(id).or_else(|| id.split_once(':').and_then(|(base, _)| get(base)))
}

/// Tool-capable models whose id or name contains every word of `query`,
/// prefix matches on the id first.
pub fn search<'a>(models: &'a [ModelInfo], query: &str) -> Vec<&'a ModelInfo> {
    let q = query.to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut hits: Vec<&ModelInfo> = models
        .iter()
        .filter(|m| m.tools)
        .filter(|m| {
            let hay = format!("{} {}", m.id.to_lowercase(), m.name.to_lowercase());
            words.iter().all(|w| hay.contains(w))
        })
        .collect();
    hits.sort_by_key(|m| (!m.id.to_lowercase().starts_with(&q), m.id.clone()));
    hits
}

fn tokens(n: u32) -> String {
    match n {
        1_000_000.. => format!("{}M", (n as f64 / 1e6 * 10.0).round() / 10.0),
        _ => format!("{}k", n / 1000),
    }
}

fn money(p: Option<f64>) -> String {
    p.map_or("?".into(), |p| if p < 0.01 { "0".into() } else { format!("{p:.2}") })
}

/// A model's window, then USD per million tokens in and out.
fn figures(m: &ModelInfo) -> (String, String) {
    let ctx = m.context.map_or("?".into(), tokens);
    let price = match (m.prompt_per_m, m.completion_per_m) {
        (Some(a), Some(b)) if a == 0.0 && b == 0.0 => "free".into(),
        (a, b) => format!("${}/${}", money(a), money(b)),
    };
    (ctx, format!("{price}{}", if m.reasoning { "  reasoning" } else { "" }))
}

/// One line per model: id, window, then USD per million tokens in and out.
pub fn row(m: &ModelInfo) -> String {
    let (ctx, rest) = figures(m);
    format!("{:<46} {:>6}  {}", m.id, ctx, rest)
}

/// A model as two cells for a table: its id, and the rest of `row`.
pub fn cells(m: &ModelInfo) -> (String, String) {
    let (ctx, rest) = figures(m);
    (m.id.clone(), format!("{ctx:>5}  {rest}"))
}

/// The first `limit` models matching `query` and how many more there were;
/// the error is the sentence for no match.
pub fn page<'a>(models: &'a [ModelInfo], query: &str, limit: usize) -> Result<(Vec<&'a ModelInfo>, usize), String> {
    let mut hits = search(models, query);
    if hits.is_empty() {
        return Err(format!("no model matches \"{query}\" ({} known)", models.len()));
    }
    let more = hits.len().saturating_sub(limit);
    hits.truncate(limit);
    Ok((hits, more))
}

/// `models` matching `query` as at most `limit` rows, with a count of the rest.
pub fn listing(models: &[ModelInfo], query: &str, limit: usize) -> String {
    match page(models, query, limit) {
        Err(none) => none,
        Ok((hits, more)) => {
            let mut lines: Vec<String> = hits.iter().map(|m| row(m)).collect();
            if more > 0 {
                lines.push(format!("… and {more} more; narrow the query"));
            }
            lines.join("\n")
        }
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn read(paths: &Paths, base_url: &str) -> Option<Cached> {
    serde_json::from_slice(&std::fs::read(paths.models_cache(base_url)).ok()?).ok()
}

/// How old the cached list for `base_url` is; `None` when there is none.
pub fn cache_age(paths: &Paths, base_url: &str) -> Option<Duration> {
    read(paths, base_url).filter(|c| c.fetched_at > 0).map(|c| Duration::from_secs(now().saturating_sub(c.fetched_at)))
}

/// How the last fetches of `base_url`'s list went.
pub fn reach(paths: &Paths, base_url: &str) -> Reach {
    let Some(c) = read(paths, base_url) else { return Reach::default() };
    Reach { fetched_at: (c.fetched_at > 0).then_some(c.fetched_at), failed: c.failed }
}

/// The cached list for `base_url`, however old; empty when there is none.
pub fn cached(paths: &Paths, base_url: &str) -> Vec<ModelInfo> {
    read(paths, base_url).map(|c| c.models).unwrap_or_default()
}

impl Client {
    /// `GET {base_url}/models`. The list is public on OpenRouter, so no key
    /// goes with it.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let mut rb = self.http.get(format!("{}/models", self.base_url)).timeout(FETCH_TIMEOUT);
        for (k, v) in &self.headers {
            rb = rb.header(k, v);
        }
        let resp = rb.send().await.context("model list request failed")?;
        if !resp.status().is_success() {
            bail!("model list returned {}", resp.status());
        }
        let body: Value = resp.json().await.context("model list is not JSON")?;
        let models = parse(&body);
        if models.is_empty() {
            bail!("model list has no models");
        }
        Ok(models)
    }
}

/// Cache `models` as `base_url`'s list, fetched now.
pub fn store(paths: &Paths, base_url: &str, models: &[ModelInfo]) {
    let cache = Cached { fetched_at: now(), models: models.to_vec(), failed: None };
    if let Ok(bytes) = serde_json::to_vec(&cache) {
        let _ = write_atomic(&paths.models_cache(base_url), &bytes);
    }
}

/// The list: the cache when it is fresh (and `force` is off), else a fetch
/// the provider's way (`discover`), else a stale cache. Errors only when
/// there is nothing at all.
pub async fn refresh(client: &Client, paths: &Paths, force: bool, how: Discover) -> Result<Vec<ModelInfo>> {
    let old = read(paths, &client.base_url);
    if let Some(c) = old.as_ref().filter(|c| !force && now().saturating_sub(c.fetched_at) <= TTL_SECS) {
        return Ok(c.models.clone());
    }
    match super::discover::fetch(client, how).await {
        Ok(models) => {
            store(paths, &client.base_url, &models);
            Ok(models)
        }
        Err(e) => {
            // Kept beside the list it failed to replace, so the picker can say so.
            let cause = format!("{e:#}").rsplit(": ").next().unwrap_or_default().to_string();
            let (fetched_at, models) = old.map_or((0, vec![]), |c| (c.fetched_at, c.models));
            let cache = Cached { fetched_at, models: models.clone(), failed: Some((now(), cause)) };
            if let Ok(bytes) = serde_json::to_vec(&cache) {
                let _ = write_atomic(&paths.models_cache(&client.base_url), &bytes);
            }
            if models.is_empty() { Err(e) } else { Ok(models) }
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::json;

    pub(in crate::llm) fn sample() -> Value {
        json!({ "data": [
            { "id": "b/small", "name": "B: Small", "context_length": 32768,
              "pricing": { "prompt": "0.0000003", "completion": "0.0000025" },
              "supported_parameters": ["tools", "tool_choice", "reasoning"],
              "architecture": { "input_modalities": ["text", "image"] },
              "top_provider": { "max_completion_tokens": 8192 } },
            { "id": "a/big", "context_length": 1000000,
              "pricing": { "prompt": "0", "completion": "0" },
              "supported_parameters": ["tools"] },
            { "id": "c/chat-only", "context_length": 8192, "supported_parameters": ["temperature"] },
            { "id": "openrouter/auto", "pricing": { "prompt": "-1", "completion": "-1" } },
            { "id": "" },
            { "name": "no id" },
            { "id": "b/small" }
        ]})
    }

    #[test]
    fn parses_what_openrouter_states() {
        let models = parse(&sample());
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a/big", "b/small", "c/chat-only", "openrouter/auto"]);
        let small = find(&models, "b/small").unwrap();
        assert_eq!((small.context, small.max_output), (Some(32768), Some(8192)));
        assert_eq!(small.name, "B: Small");
        assert!((small.prompt_per_m.unwrap() - 0.3).abs() < 1e-9);
        assert!((small.completion_per_m.unwrap() - 2.5).abs() < 1e-9);
        assert!(small.tools && small.reasoning && small.images);
        assert!(!find(&models, "c/chat-only").unwrap().tools);
        // No parameters listed: assumed capable. "-1" prices are unknown.
        let auto = find(&models, "openrouter/auto").unwrap();
        assert!(auto.tools && auto.prompt_per_m.is_none() && auto.context.is_none());
    }

    #[test]
    fn parses_a_bare_array_and_rejects_junk() {
        assert_eq!(parse(&json!([{ "id": "x" }])).len(), 1);
        assert!(parse(&json!({ "error": "nope" })).is_empty());
    }

    #[test]
    fn find_drops_a_variant_suffix() {
        let models = parse(&sample());
        assert_eq!(find(&models, "b/small:nitro").map(|m| m.id.as_str()), Some("b/small"));
        assert!(find(&models, "z/none:free").is_none());
    }

    #[test]
    fn search_needs_tools_and_every_word() {
        let models = parse(&sample());
        let ids = |q: &str| search(&models, q).iter().map(|m| m.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(""), ["a/big", "b/small", "openrouter/auto"]);
        assert_eq!(ids("small"), ["b/small"]);
        assert_eq!(ids("b: sm"), ["b/small"]);
        assert!(ids("chat-only").is_empty());
        assert_eq!(ids("b/"), ["b/small"]);
    }

    #[test]
    fn rows_show_window_and_price() {
        let models = parse(&sample());
        let r = row(find(&models, "b/small").unwrap());
        assert!(r.contains("32k") && r.contains("$0.30/$2.50") && r.contains("reasoning"), "{r}");
        assert!(row(find(&models, "a/big").unwrap()).contains("1M  free"));
        assert!(listing(&models, "zzz", 5).starts_with("no model matches"));
        assert!(listing(&models, "", 1).contains("and 2 more"));
    }
}
