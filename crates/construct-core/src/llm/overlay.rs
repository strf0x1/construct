//! Laying each provider's model list under its catalog: which providers are
//! asked, how a list entry becomes a catalog entry, and the startup refresh.
//!
//! The design choice: the lists only ever supply what the user's own config
//! lacks (ADR 0020, 0027), and only the cache is read on the UI path, so a
//! model switch is a lookup and a dead server costs nothing but its window.
//! OpenRouter's list is the one exception: it needs the default endpoint's
//! key to resolve, so a keyless machine is never sent there and never shown
//! another run's rows (#355).

use super::Client;
use super::catalog::{self, ModelInfo, cached};
use super::discover::refresh;
use crate::config::{Config, Dialect, Paths};
use crate::endpoint::{self, Endpoint};
use crate::models::{ModelEntry, Pricing};
use crate::providers::{DEFAULT, Discover};
use anyhow::bail;
use std::time::Duration;
use tokio::sync::mpsc;

/// Startup waits this long for a list at most; a model request is not worth
/// delaying for a catalog.
const STARTUP_WAIT: Duration = Duration::from_secs(3);

/// The default endpoint as an OpenRouter endpoint, when it speaks that
/// dialect. Named providers are not OpenRouter presets.
fn default_openrouter(cfg: &Config) -> Option<Endpoint> {
    let ep = Endpoint::declared(cfg);
    (super::Provider::of(&ep).dialect == Dialect::OpenRouter).then_some(ep)
}

/// Whether OpenRouter's list is available: the default endpoint speaks its
/// dialect and its key resolves. With no key, nothing is fetched and no
/// cached list is laid under the catalog (#355, ADR 0020 amendment). The key
/// is a gate, not a credential sent on the public `/models` request.
pub fn default_list_enabled(cfg: &Config) -> bool {
    default_openrouter(cfg).is_some_and(|ep| ep.key().is_some())
}

/// Whether the default OpenRouter list is blocked by a missing key. Other
/// dialects are untouched, including a loopback endpoint's own discovery.
pub fn default_list_blocked(cfg: &Config) -> bool {
    default_openrouter(cfg).is_some_and(|ep| ep.key().is_none())
}

/// The CLI's `models` command: refuse a keyless OpenRouter list before any
/// request or cache read. A selected named provider is not the default.
pub fn check_default_list(cfg: &Config, selected: &str) -> anyhow::Result<()> {
    if selected == endpoint::DEFAULT && default_list_blocked(cfg) {
        bail!("no key for OpenRouter: set {} to list its models", default_list_secret(cfg));
    }
    Ok(())
}

/// How to name the missing key in a message: the declared secret, or the
/// OpenRouter fallback when no other secret was declared.
pub fn default_list_secret(cfg: &Config) -> String {
    let secret = &cfg.api_key_secret;
    if secret.is_empty() || secret == "OPENROUTER_API_KEY" {
        "OPENROUTER_API_KEY".to_string()
    } else {
        format!("{secret} or OPENROUTER_API_KEY")
    }
}

/// The providers whose lists are laid under the catalog, and how each is
/// fetched: `default` only when it is OpenRouter with a resolving key
/// (#355, ADR 0020 amendment), and every named provider whose `discover` is
/// not `off`.
fn targets(cfg: &Config) -> Vec<(String, Discover)> {
    let mut out = vec![];
    if default_list_enabled(cfg) {
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
        aliases: m.aliases.clone(),
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
/// `default` is asked only while selected and only with a resolving key
/// ([#355]); nobody who moved off OpenRouter, or who has no key for it, is
/// sent there. For startup, before an agent is built. `landed` hears of each
/// fetch as it ends, so a UI can lay a late list in ([`lay_lists`]).
pub async fn prepare(cfg: &mut Config, paths: &Paths, landed: Option<mpsc::UnboundedSender<()>>) {
    let selected = cfg.endpoint().name;
    let mut wait = None;
    for (name, how) in targets(cfg) {
        if name == DEFAULT && selected != DEFAULT {
            continue;
        }
        let Some((ep, _)) = cfg.provider(&name) else { continue };
        // OpenRouter's list is public: the key only gates the fetch, so it
        // is still not sent. A named provider's may want its key.
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
    use crate::config::{ProviderConfig, write_atomic};
    use crate::llm::catalog::{Cached, parse, store, tests::sample};

    /// A resolvable key for the tests that exercise OpenRouter's list.
    fn test_key() -> &'static str {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| unsafe { std::env::set_var("CONSTRUCT_TEST_355_KEY", "x") });
        "CONSTRUCT_TEST_355_KEY"
    }

    #[test]
    fn overlay_lays_the_list_under_the_users_catalog() {
        let dir = std::env::temp_dir().join(format!("construct-catalog-overlay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths { home: dir.clone() };
        let cfg_for = |toml: &str| {
            let mut c: Config = toml::from_str(toml).unwrap();
            c.api_key_secret = test_key().into();
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
    fn a_keyless_default_openrouter_lists_nothing() {
        let dir = std::env::temp_dir().join(format!("construct-catalog-no-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths { home: dir.clone() };
        let cache = Cached { fetched_at: 0, models: parse(&sample()), failed: None };
        let cfg_for = |secret: &str| Config {
            api_key_secret: secret.into(),
            provider: ProviderConfig { dialect: Dialect::OpenRouter, ..ProviderConfig::default() },
            ..Config::default()
        };
        // An empty secret is keyless even if this developer has a key set.
        let mut keyless = cfg_for("");
        write_atomic(&paths.models_cache(&keyless.base_url), &serde_json::to_vec(&cache).unwrap()).unwrap();
        assert!(!default_list_enabled(&keyless));
        assert!(targets(&keyless).is_empty());
        overlay(&mut keyless, &paths);
        assert!(keyless.discovered.is_empty(), "a stale list is not laid without a key");

        // The same cache is laid as soon as the endpoint's key resolves.
        // Test-only env mutation; the name is unique to this test.
        unsafe { std::env::set_var("CONSTRUCT_TEST_355_KEY", "x") };
        let mut keyed = cfg_for("CONSTRUCT_TEST_355_KEY");
        assert!(default_list_enabled(&keyed));
        assert_eq!(targets(&keyed).len(), 1);
        overlay(&mut keyed, &paths);
        assert!(!keyed.discovered.is_empty(), "the keyed path is unchanged");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_named_providers_list_fills_windows_under_its_defaults_and_entries() {
        let dir = std::env::temp_dir().join(format!("construct-catalog-providers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths { home: dir.clone() };
        let url = "http://127.0.0.1:9/v1";
        let listed = vec![
            ModelInfo {
                id: "big".into(),
                aliases: vec!["the-big".into()],
                context: Some(262_144),
                tools: true,
                ..ModelInfo::default()
            },
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
        assert_eq!(plain.entry().unwrap().aliases, ["the-big"], "the list's aliases travel with the entry");
        assert_eq!(
            plain.catalog_ids().iter().filter(|id| id.as_str() == "box/the-big").count(),
            0,
            "an alias is not a row"
        );
        assert_eq!(plain.resolve("small").unwrap().provider, None, "a fetched list never routes a bare id");
        let mut alias = cfg_for("");
        alias.select_model("box/the-big").unwrap();
        assert_eq!(alias.context_window, 262_144, "an alias selector takes the canonical entry's window");

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
