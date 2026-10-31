//! Laying each provider's model list under its catalog: which providers are
//! asked, how a list entry becomes a catalog entry, and the startup refresh.
//!
//! The design choice: the lists only ever supply what the user's own config
//! lacks (ADR 0020, 0027), and only the cache is read on the UI path, so a
//! model switch is a lookup and a dead server costs nothing but its window.

use super::Client;
use super::catalog::{self, ModelInfo, cached, refresh};
use crate::config::{Config, Dialect, Paths};
use crate::endpoint::Endpoint;
use crate::models::{ModelEntry, Pricing};
use crate::providers::{DEFAULT, Discover};
use std::time::Duration;
use tokio::sync::mpsc;

/// Startup waits this long for a list at most; a model request is not worth
/// delaying for a catalog.
const STARTUP_WAIT: Duration = Duration::from_secs(3);

/// The providers whose lists are laid under the catalog, and how each is
/// fetched: `default` only when it is OpenRouter (ADR 0020, unchanged), and
/// every named provider whose `discover` is not `off`.
fn targets(cfg: &Config) -> Vec<(String, Discover)> {
    let default = Endpoint::declared(cfg);
    let mut out = vec![];
    if super::Provider::of(&default).dialect == Dialect::OpenRouter {
        out.push((DEFAULT.to_string(), Discover::Openai));
    }
    out.extend(cfg.providers.iter().filter(|(_, p)| p.discover != Discover::Off).map(|(n, p)| (n.clone(), p.discover)));
    out
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

/// Lay each provider's cached list under its catalog (tool-capable models
/// only) and re-select the model, so its window is the list's unless the
/// user's entry, the provider's `context_window` or a pinned top-level one
/// says otherwise. Reads the cache only, so it is safe on the UI path.
pub fn overlay(cfg: &mut Config, paths: &Paths) {
    lay_lists(cfg, paths);
    // The model resolved at load; the lists only add entries.
    let _ = cfg.select_model(&cfg.model.clone());
}

/// [`overlay`] without re-selecting the model: for a list that lands after
/// the session started, which may add rows and change what the picker says
/// but must not move the running model's window under the agent.
pub fn lay_lists(cfg: &mut Config, paths: &Paths) {
    for (name, _) in targets(cfg) {
        let Some((ep, _)) = cfg.provider(&name) else { continue };
        let pinned = name == DEFAULT && cfg.window_pinned;
        let list = cached(paths, &ep.base_url)
            .iter()
            .filter(|m| m.tools)
            .map(|m| (m.id.clone(), entry_of(m, pinned)))
            .collect();
        match cfg.providers.get_mut(&name) {
            Some(p) => (p.discovered, p.reach) = (list, catalog::reach(paths, &ep.base_url)),
            None => cfg.discovered = list,
        }
    }
}

/// Refresh the lists, then [`overlay`]. Startup waits briefly for the
/// selected model's provider only; the other named providers (each one the
/// user declared) refresh in the background and are laid under the catalog
/// at the next start. `default` is asked only while selected, as before
/// providers: nobody who moved off OpenRouter is sent there. For startup,
/// before an agent is built. `landed` hears of each fetch as it ends, so a
/// UI can lay a late list in ([`lay_lists`]).
pub async fn prepare(cfg: &mut Config, paths: &Paths, landed: Option<mpsc::UnboundedSender<()>>) {
    let selected = cfg.endpoint().name;
    let mut wait = None;
    for (name, how) in targets(cfg) {
        if name == DEFAULT && selected != DEFAULT {
            continue;
        }
        let Some((ep, _)) = cfg.provider(&name) else { continue };
        // OpenRouter's list is public; a named provider's may want its key.
        let key = if name == DEFAULT { None } else { ep.key() };
        let Ok(client) = Client::new(&ep, key) else { continue };
        let (paths, landed) = (paths.clone(), landed.clone());
        let task = tokio::spawn(async move {
            let _ = refresh(&client, &paths, false, how).await;
            landed.map(|tx| tx.send(()));
        });
        if name == selected {
            wait = Some(task);
        }
    }
    if let Some(task) = wait {
        let _ = tokio::time::timeout(STARTUP_WAIT, task).await;
    }
    overlay(cfg, paths);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::write_atomic;
    use crate::llm::catalog::{Cached, parse, store, tests::sample};

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
        let cache = Cached { fetched_at: 0, models: parse(&sample()), failed: None };
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

    #[test]
    fn a_named_providers_list_fills_windows_under_its_defaults_and_entries() {
        let dir = std::env::temp_dir().join(format!("construct-catalog-providers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths { home: dir.clone() };
        let url = "http://127.0.0.1:9/v1";
        let listed = vec![
            ModelInfo { id: "big".into(), context: Some(262_144), tools: true, ..ModelInfo::default() },
            ModelInfo { id: "small".into(), context: Some(8192), tools: true, ..ModelInfo::default() },
        ];
        store(&paths, url, &listed);
        let cfg_for = |extra: &str| {
            let mut c: Config =
                toml::from_str(&format!("model = \"box/big\"\n[providers.box]\nbase_url = \"{url}\"\n{extra}"))
                    .unwrap();
            c.select_model(&c.model.clone()).unwrap();
            overlay(&mut c, &paths);
            c
        };
        let plain = cfg_for("");
        assert_eq!(plain.context_window, 262_144, "the list states the window");
        assert!(plain.catalog_ids().contains(&"box/small".to_string()), "listed models are offered qualified");
        assert_eq!(plain.resolve("small").unwrap().provider, None, "a fetched list never routes a bare id");

        let provider_wins = cfg_for("context_window = 65536\n");
        assert_eq!(provider_wins.context_window, 65_536, "the provider's context_window over the list");
        let labelled = cfg_for("[providers.box.models.big]\nlabel = \"Big\"\n");
        assert_eq!(labelled.context_window, 262_144, "an entry without a window keeps the list's");
        let entry_wins = cfg_for("context_window = 65536\n[providers.box.models.big]\ncontext_window = 4096\n");
        assert_eq!(entry_wins.context_window, 4096, "the user's entry over everything");

        let off = cfg_for("discover = \"off\"\n");
        assert!(off.providers["box"].discovered.is_empty(), "off asks nothing and lays nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
