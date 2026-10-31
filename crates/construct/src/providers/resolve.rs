//! How a selector finds its provider, and what the providers look like
//! together: the lists the picker offers, the labels `/status` shows, and
//! the checks run at load.
//!
//! The design choice (ADR 0027 decision 5): resolution reads only what the
//! user configured and catalogued, never a fetched list, so the same config
//! always routes the same way.

use super::{DEFAULT, Discover, Resolved, preset};
use crate::config::Config;
use crate::endpoint::Endpoint;
use anyhow::{Result, bail};

impl Config {
    /// Which provider serves `selector`, and the id it is sent as. An id that
    /// more than one provider lists must be qualified.
    pub fn resolve(&self, selector: &str) -> Result<Resolved> {
        if let Some((first, rest)) = selector.split_once('/') {
            if first == DEFAULT {
                return Ok(Resolved { provider: None, id: rest.to_string() });
            }
            if self.providers.contains_key(first) {
                let id = match preset(first).and_then(|p| p.routers) {
                    Some(prefix) if !rest.contains('/') => format!("{prefix}{rest}"),
                    _ => rest.to_string(),
                };
                return Ok(Resolved { provider: Some(first.to_string()), id });
            }
        }
        let mut named = self.providers.iter().filter(|(_, p)| p.models.contains_key(selector)).map(|(n, _)| n);
        // Only what the user catalogued: a fetched list never changes where an id goes.
        let default_lists = self.models.contains_key(selector);
        match (named.next(), named.next(), default_lists) {
            (Some(n), None, false) => Ok(Resolved { provider: Some(n.clone()), id: selector.to_string() }),
            (None, _, _) => Ok(Resolved { provider: None, id: selector.to_string() }),
            _ => {
                let mut all: Vec<String> = self
                    .providers
                    .iter()
                    .filter(|(_, p)| p.models.contains_key(selector))
                    .map(|(n, _)| format!("{n}/{selector}"))
                    .collect();
                if default_lists {
                    all.insert(0, format!("{DEFAULT}/{selector}"));
                }
                bail!("{selector:?} is served by more than one provider; pick one: {}", all.join(", "))
            }
        }
    }

    /// The id `selector` is sent as: the selector itself if it doesn't resolve.
    pub fn wire_id(&self, selector: &str) -> String {
        self.resolve(selector).map_or_else(|_| selector.to_string(), |r| r.id)
    }

    /// The named provider serving the selected model; `None` for `default`.
    pub fn provider_name(&self) -> Option<String> {
        self.resolve(&self.model).ok().and_then(|r| r.provider)
    }

    /// How provider `name` learns its models: `openai` for `default`.
    pub fn discovery(&self, name: &str) -> Discover {
        self.providers.get(name).map_or(Discover::Openai, |p| p.discover)
    }

    /// How many models provider `name`'s fetched list holds.
    pub fn listed(&self, name: &str) -> usize {
        self.providers.get(name).map_or(if name == DEFAULT { self.discovered.len() } else { 0 }, |p| p.discovered.len())
    }

    /// Every provider's name, `default` first.
    pub fn provider_names(&self) -> Vec<String> {
        std::iter::once(DEFAULT.to_string()).chain(self.providers.keys().cloned()).collect()
    }

    /// Provider `name`'s endpoint and the ids its catalog lists (as sent).
    pub fn provider(&self, name: &str) -> Option<(Endpoint, Vec<String>)> {
        if name == DEFAULT {
            return Some((Endpoint::declared(self), self.models.keys().cloned().collect()));
        }
        let p = self.providers.get(name)?;
        Some((p.endpoint(name), p.models.keys().cloned().collect()))
    }

    /// Whether the selected model is one of OpenRouter's own routers, judged
    /// on the id sent, not the selector (`openrouter/anthropic/x` is not one).
    pub fn on_router(&self) -> bool {
        crate::models::is_router(&self.wire_id(&self.model))
    }

    /// The selected model as `provider/id` when a named provider serves it,
    /// so a bare id that resolved elsewhere says where.
    pub fn qualified_model(&self) -> String {
        match self.provider_name() {
            Some(p) if !self.model.starts_with(&format!("{p}/")) => format!("{p}/{}", self.model),
            _ => self.model.clone(),
        }
    }

    /// Where requests go, for display: `name · url`, or the url for `default`.
    pub fn endpoint_label(&self) -> String {
        let ep = self.endpoint();
        if ep.name == DEFAULT { ep.base_url } else { format!("{} · {}", ep.name, ep.base_url) }
    }

    /// Every catalogued selector, for the picker and the palette: the
    /// top-level catalog and its list, then each provider's as `name/id`, so
    /// each provider's models are together.
    pub fn catalog_ids(&self) -> Vec<String> {
        // A top-level id another provider also lists is shown qualified.
        let plain = |id: &String| if self.resolve(id).is_ok() { id.clone() } else { format!("{DEFAULT}/{id}") };
        let mut ids: Vec<String> = self.models.keys().map(plain).collect();
        ids.extend(self.discovered.keys().filter(|id| !self.models.contains_key(*id)).map(plain));
        for (name, p) in &self.providers {
            ids.extend(p.models.keys().map(|id| format!("{name}/{id}")));
            ids.extend(p.discovered.keys().filter(|id| !p.models.contains_key(*id)).map(|id| format!("{name}/{id}")));
        }
        ids
    }

    /// The `[providers.*]` tables and every model setting, checked at load.
    pub(crate) fn check_providers(&self) -> Result<()> {
        for (name, p) in &self.providers {
            p.check(name)?;
        }
        let agents = self.agents.iter().map(|(n, a)| (format!("[agents.{n}] model"), &a.model));
        let named = [("model".to_string(), &self.model), ("[learn] model".into(), &self.learn.model)]
            .into_iter()
            .chain([("[compaction] model".to_string(), &self.compaction.model)])
            .chain(agents);
        for (field, selector) in named.filter(|(_, s)| !s.is_empty()) {
            self.resolve(selector).map_err(|e| anyhow::anyhow!("{field}: {e}"))?;
        }
        Ok(())
    }

    /// What a provider's name hides. A configured provider `n` takes every
    /// selector that starts `n/`, so an id some list spells `n/…` (OpenRouter's
    /// `minimax/…` under a provider called `minimax`) is reachable only as
    /// `<its provider>/n/…`. One line per name and list; empty when nothing
    /// collides. A preset's own routers are not hidden: the router rule reads
    /// them (`openrouter/free`).
    pub fn shadowed(&self) -> Vec<String> {
        let mut lists: Vec<(&str, Vec<&String>)> =
            vec![(DEFAULT, self.models.keys().chain(self.discovered.keys()).collect())];
        lists.extend(
            self.providers.iter().map(|(n, p)| (n.as_str(), p.models.keys().chain(p.discovered.keys()).collect())),
        );
        let mut out = vec![];
        for name in self.providers.keys() {
            let prefix = format!("{name}/");
            let own_routers = preset(name).and_then(|p| p.routers) == Some(prefix.as_str());
            for (from, ids) in &lists {
                let mut hit: Vec<&&String> = ids.iter().filter(|id| id.starts_with(&prefix) && !own_routers).collect();
                hit.sort();
                hit.dedup();
                if let Some(first) = hit.first() {
                    out.push(format!(
                        "provider {name:?} takes every {name}/… selector, so {} id(s) in {from}'s models (e.g. {first}) \
                         are reached as {from}/{first}; rename the provider to reach them by their own name",
                        hit.len()
                    ));
                }
            }
        }
        out
    }
}
