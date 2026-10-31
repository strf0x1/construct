//! Laying a selection into the running config: which provider and entry a
//! selector names, the window and cap that results, and the endpoint its
//! requests go to.
//!
//! The design choice (ADR 0027): a switch always lands on the same layers in
//! the same order, whatever was selected before: the top-level defaults,
//! what discovery found, the provider's defaults, then the user's entry.
//! The endpoint is never laid over anything; it is resolved on each call.

use super::ModelEntry;
use crate::config::Config;
use crate::endpoint::Endpoint;
use crate::providers::Resolved;
use anyhow::Result;

/// The top-level limits an entry can cover, as they were before any entry.
#[derive(Debug, Clone)]
pub struct Defaults {
    context_window: u32,
    max_tokens: Option<u32>,
}

impl Defaults {
    fn of(cfg: &Config) -> Self {
        Self { context_window: cfg.context_window, max_tokens: cfg.max_tokens }
    }

    fn restore(&self, cfg: &mut Config) {
        cfg.context_window = self.context_window;
        cfg.max_tokens = self.max_tokens;
    }
}

impl Config {
    /// Make `selector` the model: the defaults, then what discovery found for
    /// it, its provider's defaults, and its catalog entry, each over the last
    /// (ADR 0027 decision 6). A selector that resolves to more than one
    /// provider changes nothing.
    pub fn select_model(&mut self, selector: &str) -> Result<()> {
        let to = self.resolve(selector)?;
        let defaults = match &self.defaults {
            Some(d) => d.clone(),
            None => {
                let d = Box::new(Defaults::of(self));
                self.defaults = Some(d.clone());
                d
            }
        };
        defaults.restore(self);
        if let Some(d) = self.discovered_entry(selector).cloned() {
            d.apply(self);
        }
        if let Some(p) = to.provider.as_ref().and_then(|p| self.providers.get(p)) {
            self.context_window = p.context_window.unwrap_or(self.context_window);
            self.max_tokens = p.max_tokens.or(self.max_tokens);
        }
        self.model = selector.to_string();
        if let Some(e) = self.catalogued(selector).cloned() {
            e.apply(self);
        }
        Ok(())
    }

    /// The entry for `selector`: its provider's catalog; for `default`, the
    /// user's catalog, then the endpoint's own list (which also knows
    /// `vendor/model:variant` by `vendor/model`).
    pub fn lookup(&self, selector: &str) -> Option<&ModelEntry> {
        self.catalogued(selector).or_else(|| self.discovered_entry(selector))
    }

    /// Where the selected model's window came from, in `select_model`'s order
    /// read backwards: the first layer that set it.
    pub fn window_source(&self) -> String {
        let provider = self.provider_name();
        let set = |e: Option<&ModelEntry>| e.is_some_and(|e| e.context_window.is_some());
        if set(self.catalogued(&self.model)) {
            "catalog entry".into()
        } else if let Some(p) =
            provider.as_ref().filter(|p| self.providers.get(*p).is_some_and(|d| d.context_window.is_some()))
        {
            format!("[providers.{p}]")
        } else if set(self.discovered_entry(&self.model)) {
            "the server's model list".into()
        } else if self.window_pinned {
            "config.toml".into()
        } else {
            "the default".into()
        }
    }

    /// The user's own entry for `selector`, in its provider's catalog.
    fn catalogued(&self, selector: &str) -> Option<&ModelEntry> {
        match self.resolve(selector).ok()? {
            Resolved { provider: Some(p), id } => self.providers.get(&p)?.models.get(&id),
            Resolved { provider: None, id } => self.models.get(&id),
        }
    }

    /// What its provider's list says about `selector`.
    fn discovered_entry(&self, selector: &str) -> Option<&ModelEntry> {
        let Resolved { provider, id } = self.resolve(selector).ok()?;
        let list = match provider {
            Some(p) => &self.providers.get(&p)?.discovered,
            None => &self.discovered,
        };
        list.get(&id).or_else(|| list.get(id.split_once(':')?.0))
    }

    /// Where the selected model's requests go.
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint_for(&self.model)
    }

    /// Where `selector`'s requests go: its provider's endpoint (the declared
    /// one for `default`), moved by its catalog entry if that names one.
    pub fn endpoint_for(&self, selector: &str) -> Endpoint {
        let named = self.resolve(selector).ok().and_then(|r| r.provider);
        let base = match named.as_ref().and_then(|p| self.providers.get(p).map(|d| (p, d))) {
            Some((name, p)) => p.endpoint(name),
            None => Endpoint::declared(self),
        };
        match self.lookup(selector) {
            Some(e) => e.endpoint(base),
            None => base,
        }
    }

    /// Make `ep` the declared endpoint and the selected model a plain id on
    /// it: a delegated child is handed its endpoint this way, already
    /// resolved, so nothing in its config can send it elsewhere.
    pub fn pin_endpoint(&mut self, ep: Endpoint) {
        let id = self.wire_id(&self.model);
        let entry = self.lookup(&self.model).cloned();
        // A summarizer elsewhere is not the child's to reach; it uses its own model.
        let summarizer = std::mem::take(&mut self.compaction.model);
        if !summarizer.is_empty() && self.endpoint_for(&summarizer).base_url == ep.base_url {
            self.compaction.model = self.wire_id(&summarizer);
        }
        self.base_url = ep.base_url;
        self.api_key_secret = ep.api_key_secret;
        self.headers = ep.headers;
        self.provider = ep.provider;
        self.providers.clear();
        if let Some(mut e) = entry {
            e.base_url.clear();
            e.api_key_secret.clear();
            e.dialect = None;
            e.cache = None;
            self.models.insert(id.clone(), e);
        }
        self.model = id;
    }

    /// The catalog entry of the selected model.
    pub fn entry(&self) -> Option<&ModelEntry> {
        self.lookup(&self.model)
    }
}
