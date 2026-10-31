//! The endpoint's model list: fetched from `{base_url}/models`, cached on disk,
//! searched by name, and laid under the hand-written model catalog
//! (`crate::models`) as `Config::discovered`. OpenRouter publishes context
//! length, prices and supported parameters there; other endpoints give ids at
//! best. The design choice a reader must know: the list only supplies entries
//! the user's own catalog lacks, and `select_model` resolves them like any
//! other, so a switch is a cached lookup, never a request, and a failed fetch
//! falls back to a stale cache instead of failing anything.

use super::Client;
use crate::config::{Config, Dialect, Paths, write_atomic};
use crate::models::{ModelEntry, Pricing};
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
    super::Provider::of(&cfg.endpoint()).dialect == Dialect::OpenRouter
}

/// A list entry as a catalog entry. Only what OpenRouter states and the
/// harness uses: the output cap stays the user's, and a pinned top-level
/// `context_window` is not replaced.
fn entry_of(m: &ModelInfo, pinned: bool) -> ModelEntry {
    let tags = [(m.reasoning, "reasoning"), (m.images, "vision")]
        .into_iter()
        .filter(|&(on, _t)| on)
        .map(|(_on, t)| t.to_string())
        .collect();
    ModelEntry {
        label: if m.name == m.id { String::new() } else { m.name.clone() },
        tags,
        context_window: if pinned { None } else { m.context },
        pricing: m.prompt_per_m.zip(m.completion_per_m).map(|(prompt, completion)| Pricing { prompt, completion }),
        ..ModelEntry::default()
    }
}

/// Lay the cached list under the catalog (OpenRouter only, tool-capable
/// models only) and re-select the model, so its window is the list's unless
/// the user's own entry or `context_window` says otherwise. Reads the cache
/// only, so it is safe on the UI path.
pub fn overlay(cfg: &mut Config, paths: &Paths) {
    if !openrouter(cfg) {
        return;
    }
    cfg.discovered = cached(paths, &cfg.endpoint().base_url)
        .iter()
        .filter(|m| m.tools)
        .map(|m| (m.id.clone(), entry_of(m, cfg.window_pinned)))
        .collect();
    // The model resolved at load; the list only adds entries.
    let _ = cfg.select_model(&cfg.model.clone());
}

/// Warm the cache (briefly), then [`overlay`]. For startup, before an agent is built.
pub async fn prepare(cfg: &mut Config, paths: &Paths) {
    if !openrouter(cfg) {
        return;
    }
    if let Ok(client) = Client::new(&cfg.endpoint(), None) {
        let _ = tokio::time::timeout(STARTUP_WAIT, refresh(&client, paths, false)).await;
    }
    overlay(cfg, paths);
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
    fn overlay_lays_the_list_under_the_users_catalog() {
        let dir = std::env::temp_dir().join(format!("construct-catalog-overlay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths { home: dir.clone() };
        let cfg_for = |toml: &str| {
            let mut c: Config = toml::from_str(toml).unwrap();
            c.select_model(&c.model.clone()).unwrap();
            c
        };
        let mut cfg = cfg_for("model = \"b/small\"");
        overlay(&mut cfg, &paths);
        assert!(cfg.discovered.is_empty(), "no cache yet");
        let cache = Cached { fetched_at: 0, models: parse(&sample()) };
        write_atomic(&paths.models_cache(&cfg.base_url), &serde_json::to_vec(&cache).unwrap()).unwrap();

        // A stale cache still overlays: age only matters to `refresh`.
        overlay(&mut cfg, &paths);
        assert_eq!(cfg.context_window, 32768);
        let e = cfg.entry().unwrap();
        assert_eq!((e.label.as_str(), e.tags.clone()), ("B: Small", vec!["reasoning".to_string(), "vision".into()]));
        assert!(e.pricing.is_some() && e.max_tokens.is_none());
        assert!(!cfg.discovered.contains_key("c/chat-only"), "not tool-capable");
        // A variant id takes its base's window; an unlisted id gets the default back.
        cfg.select_model("b/small:nitro").unwrap();
        assert_eq!(cfg.context_window, 32768);
        cfg.select_model("z/unlisted").unwrap();
        assert_eq!(cfg.context_window, Config::default().context_window);

        // The user's own entry wins, and a pinned window is kept.
        let mut own = cfg_for("model = \"b/small\"\n[models.\"b/small\"]\ncontext_window = 9000");
        overlay(&mut own, &paths);
        assert_eq!(own.context_window, 9000);
        let mut pinned = Config { window_pinned: true, ..cfg_for("model = \"b/small\"\ncontext_window = 50000") };
        overlay(&mut pinned, &paths);
        assert_eq!(pinned.context_window, 50_000);
        assert_eq!(pinned.entry().unwrap().label, "B: Small", "the rest still comes from the list");

        let mut local =
            Config { model: "b/small".into(), base_url: "http://127.0.0.1:1/v1".into(), ..Config::default() };
        write_atomic(&paths.models_cache(&local.base_url), &serde_json::to_vec(&cache).unwrap()).unwrap();
        overlay(&mut local, &paths);
        assert!(local.discovered.is_empty(), "only OpenRouter states windows");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
