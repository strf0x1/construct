//! Tools defined above `construct-core`: the seam a crate such as
//! `construct-deck` plugs into, wired in by the composition root (#314).
//!
//! The design choice: a source is a factory, not a list of tools. It is asked
//! once per catalog with the config and Deck surfaces that catalog is built
//! for, so a tool can exist only when its surface does, and a catalog that
//! wants none (the learner's) is simply built without sources. Its tools are
//! added inside `Catalog::build_with` after the built-ins and before the
//! duplicate-name and `[tools] disabled` checks, so `disabled = ["deck"]` and
//! a name clash behave as they do for a built-in.

use super::{Deck, Tool};
use crate::config::ToolsConfig;
use anyhow::Result;
use std::sync::Arc;

/// Something that contributes tools to a catalog.
pub trait ToolSource: Send + Sync {
    /// The tools this source adds for `cfg` and `deck`, in definition order.
    fn tools(&self, cfg: &ToolsConfig, deck: &Deck) -> Result<Vec<Arc<dyn Tool>>>;
}

/// The sources a session's catalog is built with. Cheap to clone and share.
#[derive(Clone, Default)]
pub struct ToolSources(Vec<Arc<dyn ToolSource>>);

impl ToolSources {
    pub fn with(mut self, source: impl ToolSource + 'static) -> Self {
        self.0.push(Arc::new(source));
        self
    }

    pub(super) fn tools(&self, cfg: &ToolsConfig, deck: &Deck) -> Result<Vec<Arc<dyn Tool>>> {
        let mut out = Vec::new();
        for s in &self.0 {
            out.extend(s.tools(cfg, deck)?);
        }
        Ok(out)
    }
}
