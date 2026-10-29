//! The OpenRouter model list end to end: the window it fills in, when the
//! config wins, and `construct models`. The scripted endpoint serves
//! `/models` in OpenRouter's shape, and `dialect = "openrouter"` makes the
//! harness treat the loopback address as OpenRouter.

use crate::common::*;
use serde_json::{Value, json};

const OPENROUTER: &str = "[provider]\ndialect = \"openrouter\"\n";

fn env(name: &str) -> Env {
    Env::new(name, json!({ "steps": [{ "text": "ok" }] }))
}

/// The window a headless run planned against, from its manifest event.
fn window(env: &Env, model: &str) -> u64 {
    let out = env.run_with(&[("CONSTRUCT_MODEL", model)], &["run", "--json", "--no-learn", "hi"]);
    let events: Vec<Value> =
        String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    events.iter().find(|e| e["event"] == "manifest").unwrap_or_else(|| panic!("no manifest: {}", text(&out)))
        ["context_window"]
        .as_u64()
        .unwrap()
}

#[test]
fn an_openrouter_models_window_replaces_the_default() {
    let env = env("models-fill");
    env.config(OPENROUTER);
    assert_eq!(window(&env, "mock/small"), 32768);
    let cache = std::fs::read_dir(env.home.join("models")).unwrap().next().expect("a cached list").unwrap();
    assert!(std::fs::read_to_string(cache.path()).unwrap().contains("mock/small"));
    // The second run needs no fetch: the cache serves, and a variant id maps to its base.
    assert_eq!(window(&env, "mock/small:nitro"), 32768);
    // A model the list lacks keeps the default.
    assert_eq!(window(&env, "nobody/none"), 131_072);
}

#[test]
fn a_configured_window_is_never_replaced() {
    let env = env("models-pinned");
    env.config(&format!("context_window = 16384\n{OPENROUTER}"));
    assert_eq!(window(&env, "mock/small"), 16384);
}

#[test]
fn other_endpoints_keep_the_default_and_fetch_nothing() {
    let env = env("models-other");
    assert_eq!(window(&env, "mock/small"), 131_072);
    assert!(!env.home.join("models").exists(), "only OpenRouter's list is fetched at startup");
}

#[test]
fn models_searches_the_list() {
    let env = env("models-search");
    let out = env.run(&["models", "small"]);
    let t = text(&out);
    assert!(out.status.success(), "{t}");
    assert!(t.contains("mock/small") && t.contains("32k") && t.contains("$0.30/$2.50"), "{t}");
    assert!(!t.contains("mock  "), "the query narrows the list: {t}");
    let none = text(&env.run(&["models", "zzz"]));
    assert!(none.contains("no model matches"), "{none}");
}
