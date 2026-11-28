//! Extensible wake seams mirroring `tools::source::ToolSource` (ADR 0056,
//! ADR 0079): `WakeSource` produces raw `WakeEvent`s, `WakeFilter` drops or
//! transforms them before any LLM turn (where `construct-script` plugs in),
//! and `WakeSources` is the composition root's registry.

use super::WakeEvent;
use crate::config::{Paths, SandboxConfig, WakeTriggerConfig};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// Outcome of a `WakeFilter` pass over one event.
#[derive(Debug, Clone)]
pub enum FilterDecision {
    Pass(Box<WakeEvent>),
    Drop,
}

impl FilterDecision {
    pub fn pass(event: WakeEvent) -> Self {
        Self::Pass(Box::new(event))
    }
}

/// Pre-wake filter or correlator hook. Runs before `WakeBus` coalescing and
/// before any model call. Default is `MatchFilter`; `construct-script` plugs
/// in here in Slice 2 for zero-token JS filtering.
#[async_trait]
pub trait WakeFilter: Send + Sync {
    async fn filter(&self, event: WakeEvent) -> FilterDecision;
}

/// Default declarative substring matcher over `WakeEvent.summary`. Passes
/// through every trigger with no `match_summary` configured.
#[derive(Clone, Default)]
pub struct MatchFilter {
    patterns: HashMap<String, String>,
}

impl MatchFilter {
    pub fn from_config(triggers: &[WakeTriggerConfig]) -> Self {
        let mut patterns = HashMap::new();
        for t in triggers {
            if let Some(pat) = t.match_summary.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                patterns.insert(t.name.clone(), pat.to_ascii_lowercase());
            }
        }
        Self { patterns }
    }
}

#[async_trait]
impl WakeFilter for MatchFilter {
    async fn filter(&self, event: WakeEvent) -> FilterDecision {
        let Some(pat) = self.patterns.get(&event.trigger) else {
            return FilterDecision::pass(event);
        };
        if event.summary.to_ascii_lowercase().contains(pat) {
            FilterDecision::pass(event)
        } else {
            FilterDecision::Drop
        }
    }
}

/// Runtime context handed to each `WakeSource` when starting producers.
#[derive(Clone)]
pub struct SourceCtx {
    pub cwd: PathBuf,
    pub paths: Paths,
    pub sandbox: SandboxConfig,
}

/// A wake event producer factory registered at the composition root.
pub trait WakeSource: Send + Sync {
    /// The `[[wakes]]` `kind` values this source handles (e.g. `["timer"]`).
    fn kinds(&self) -> &[&str];

    /// Spawn producer tasks for the matching `triggers`, sending events to `tx`
    /// and exiting cleanly when `cancel` becomes `true`.
    fn start(
        &self,
        triggers: &[WakeTriggerConfig],
        ctx: &SourceCtx,
        tx: mpsc::UnboundedSender<WakeEvent>,
        cancel: watch::Receiver<bool>,
    ) -> Result<Vec<JoinHandle<()>>>;
}

/// The wake sources and filters composed for a headless host. Cheap to clone.
#[derive(Clone, Default)]
pub struct WakeSources {
    sources: Vec<Arc<dyn WakeSource>>,
    filters: Vec<Arc<dyn WakeFilter>>,
}

impl WakeSources {
    /// Register the zero-dependency built-in sources (`timer`, `command`, `cli`).
    pub fn builtin() -> Self {
        Self::default().with_source(super::TimerSource).with_source(super::CommandSource).with_source(super::CliSource)
    }

    pub fn with_source(mut self, source: impl WakeSource + 'static) -> Self {
        self.sources.push(Arc::new(source));
        self
    }

    pub fn with_filter(mut self, filter: impl WakeFilter + 'static) -> Self {
        self.filters.push(Arc::new(filter));
        self
    }

    pub fn filters(&self) -> &[Arc<dyn WakeFilter>] {
        &self.filters
    }

    /// Start every registered source against the subset of `triggers` whose
    /// `kind` it claims. Unknown kinds fail explicitly so typos never fail silently.
    pub fn start_all(
        &self,
        triggers: &[WakeTriggerConfig],
        ctx: &SourceCtx,
        tx: &mpsc::UnboundedSender<WakeEvent>,
        cancel: &watch::Receiver<bool>,
    ) -> Result<Vec<JoinHandle<()>>> {
        let mut handles = Vec::new();
        let mut handled_kinds = std::collections::BTreeSet::new();
        for src in &self.sources {
            let kinds = src.kinds();
            for &k in kinds {
                handled_kinds.insert(k);
            }
            let matching: Vec<WakeTriggerConfig> =
                triggers.iter().filter(|t| kinds.contains(&t.kind.as_str())).cloned().collect();
            if !matching.is_empty() {
                handles.extend(src.start(&matching, ctx, tx.clone(), cancel.clone())?);
            }
        }
        for t in triggers {
            if !handled_kinds.contains(t.kind.as_str()) {
                anyhow::bail!("wake trigger {:?} uses unknown source kind {:?}", t.name, t.kind);
            }
        }
        Ok(handles)
    }
}
