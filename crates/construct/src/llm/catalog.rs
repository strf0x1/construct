//! The endpoint's model list: fetched from `{base_url}/models`, cached on disk,
//! and used to fill in what the config leaves out (the context window) and to
//! search by name. OpenRouter publishes context length, prices and supported
//! parameters there; other endpoints give ids at best. The design choice a
//! reader must know: the cache holds the whole list, so a `/model` switch
//! mid-session is a file read, never a request, and a failed fetch falls back
//! to a stale cache instead of failing anything.

use super::Client;
use crate::config::{Config, Dialect, Paths, write_atomic};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a fetched list is used without asking again.
const TTL_SECS: u64 = 2 * 60 * 60;
/// One `/models` request, before giving up.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Startup waits this long for a list at most; a model request is not worth
/// delaying for a catalog.
const STARTUP_WAIT: Duration = Duration::from_secs(3);

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
struct Cached {
    fetched_at: u64,
    models: Vec<ModelInfo>,
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
        context: count(e.get("context_length")),
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

/// One line per model: id, window, then USD per million tokens in and out.
pub fn row(m: &ModelInfo) -> String {
    let ctx = m.context.map_or("?".into(), tokens);
    let price = match (m.prompt_per_m, m.completion_per_m) {
        (Some(a), Some(b)) if a == 0.0 && b == 0.0 => "free".into(),
        (a, b) => format!("${}/${}", money(a), money(b)),
    };
    format!("{:<46} {:>6}  {}{}", m.id, ctx, price, if m.reasoning { "  reasoning" } else { "" })
}

/// `models` matching `query` as at most `limit` rows, with a count of the rest.
pub fn listing(models: &[ModelInfo], query: &str, limit: usize) -> String {
    let hits = search(models, query);
    if hits.is_empty() {
        return format!("no model matches \"{query}\" ({} known)", models.len());
    }
    let mut lines: Vec<String> = hits.iter().take(limit).map(|m| row(m)).collect();
    if hits.len() > limit {
        lines.push(format!("… and {} more; narrow the query", hits.len() - limit));
    }
    lines.join("\n")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn read(paths: &Paths, base_url: &str) -> Option<Cached> {
    serde_json::from_slice(&std::fs::read(paths.models_cache(base_url)).ok()?).ok()
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

/// The list: the cache when it is fresh (and `force` is off), else a fetch,
/// else a stale cache. Errors only when there is nothing at all.
pub async fn refresh(client: &Client, paths: &Paths, force: bool) -> Result<Vec<ModelInfo>> {
    let old = read(paths, &client.base_url);
    if let Some(c) = old.as_ref().filter(|c| !force && now().saturating_sub(c.fetched_at) <= TTL_SECS) {
        return Ok(c.models.clone());
    }
    match client.list_models().await {
        Ok(models) => {
            let cache = Cached { fetched_at: now(), models: models.clone() };
            if let Ok(bytes) = serde_json::to_vec(&cache) {
                let _ = write_atomic(&paths.models_cache(&client.base_url), &bytes);
            }
            Ok(models)
        }
        Err(e) => old.map(|c| c.models).ok_or(e),
    }
}

/// Whether `cfg` talks to OpenRouter, whose list states windows.
fn openrouter(cfg: &Config) -> bool {
    super::Provider::new(&cfg.provider, &cfg.base_url).dialect == Dialect::OpenRouter
}

/// Set `cfg.context_window` from the cached list when the config does not
/// pin it; a model the list lacks gets the default again, not the previous
/// model's window. Reads the cache only, so it is safe on the UI path and on
/// a model switch. Returns the window the list gave.
pub fn fit(cfg: &mut Config, paths: &Paths) -> Option<u32> {
    if cfg.window_pinned || !openrouter(cfg) {
        return None;
    }
    let found = find(&cached(paths, &cfg.base_url), &cfg.model).and_then(|m| m.context);
    cfg.context_window = found.unwrap_or(Config::default().context_window);
    found
}

/// Warm the cache (briefly), then [`fit`]. For startup, before an agent is built.
pub async fn prepare(cfg: &mut Config, paths: &Paths) {
    if cfg.window_pinned || !openrouter(cfg) {
        return;
    }
    if let Ok(client) = Client::new(cfg, None) {
        let _ = tokio::time::timeout(STARTUP_WAIT, refresh(&client, paths, false)).await;
    }
    fit(cfg, paths);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
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

    #[test]
    fn fit_sets_the_window_unless_pinned_or_not_openrouter() {
        let dir = std::env::temp_dir().join(format!("construct-catalog-fit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths { home: dir.clone() };
        let mut cfg = Config { model: "b/small".into(), ..Config::default() };
        assert_eq!(fit(&mut cfg, &paths), None, "no cache yet");
        let cache = Cached { fetched_at: 0, models: parse(&sample()) };
        write_atomic(&paths.models_cache(&cfg.base_url), &serde_json::to_vec(&cache).unwrap()).unwrap();

        // A stale cache still fits: age only matters to `refresh`.
        assert_eq!(fit(&mut cfg, &paths), Some(32768));
        assert_eq!(cfg.context_window, 32768);
        cfg.model = "z/unlisted".into();
        assert_eq!(fit(&mut cfg, &paths), None);
        assert_eq!(cfg.context_window, Config::default().context_window, "not the last model's window");

        let mut pinned = Config { model: "b/small".into(), window_pinned: true, ..Config::default() };
        assert_eq!(fit(&mut pinned, &paths), None);
        assert_eq!(pinned.context_window, Config::default().context_window);

        let mut local =
            Config { model: "b/small".into(), base_url: "http://127.0.0.1:1/v1".into(), ..Config::default() };
        write_atomic(&paths.models_cache(&local.base_url), &serde_json::to_vec(&cache).unwrap()).unwrap();
        assert_eq!(fit(&mut local, &paths), None, "only OpenRouter states windows");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
