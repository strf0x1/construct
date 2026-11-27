//! Tools defined above `construct-core`: the seam a crate such as
//! `construct-deck` plugs into, wired in by the composition root (#314).
//!
//! The design choice: a source is a factory, not a list of tools. It is asked
//! once per catalog with the config, Deck surfaces and session job registry
//! that catalog is built for, so a tool can exist only when its surface does
//! and hold its own handle to what it operates on. Its tools are added inside
//! `Catalog::build_session` after the built-ins and before the duplicate-name
//! and `[tools] disabled` checks, so `disabled = ["deck"]` and a name clash
//! behave as they do for a built-in.

use super::boundary::BoundaryHost;
use super::{SHELL_NAME, Tool};
use crate::config::ToolsConfig;
use crate::jobs::Jobs;
use crate::permission::PolicyTools;
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

/// What Deck gave this task, each of which adds or changes tools. All off is
/// an ordinary session.
#[derive(Clone, Default)]
pub struct Deck {
    /// A boundary endpoint handle: the three boundary tools exist only then.
    pub boundary: Option<Arc<dyn BoundaryHost>>,
    /// Deck's GitHub gateway URL: the `developer` tool publishes through it.
    pub github: Option<String>,
    /// A coordinator's control gateway URL: the `deck` tool exists only then.
    pub control: Option<String>,
}

/// The names the engine's own prose (sandbox denials, the system prompt)
/// may use: core's shell, and whatever tools `sources` add.
pub fn policy_tools(sources: &ToolSources) -> PolicyTools {
    let mut tools = PolicyTools { shell: SHELL_NAME, ..Default::default() };
    sources.policy_tools(&mut tools);
    tools
}

/// Something that contributes tools to a catalog.
pub trait ToolSource: Send + Sync {
    /// The tools this source adds for `cfg`, `deck`, and `jobs`, in definition order.
    fn tools(&self, cfg: &ToolsConfig, deck: &Deck, jobs: &Jobs) -> Result<Vec<Arc<dyn Tool>>>;

    /// What to tell the model when a shell command failed because the
    /// sandbox can't reach the Git metadata `git_dir` of a linked worktree:
    /// the tools of this source that work there instead. `None` when it has
    /// none, which is why core never names those tools itself.
    fn worktree_note(&self, _git_dir: &Path) -> Option<String> {
        None
    }

    /// Name this source's tools that the sandbox's denials point the model
    /// at (`journal_query` for past sessions), so core never names a tool a
    /// build may not have.
    fn policy_tools(&self, _tools: &mut PolicyTools) {}

    /// This source's handle on Deck's boundary endpoint at `base_url`, for a
    /// session that has one. The engine holds it as a `BoundaryHost`, and
    /// passes it back in `Deck::boundary` when building the catalog's tools.
    fn boundary(&self, _base_url: &str) -> Option<Arc<dyn BoundaryHost>> {
        None
    }
}

/// The sources a session's catalog is built with. Cheap to clone and share.
#[derive(Clone, Default)]
pub struct ToolSources(Vec<Arc<dyn ToolSource>>);

impl ToolSources {
    pub fn with(mut self, source: impl ToolSource + 'static) -> Self {
        self.0.push(Arc::new(source));
        self
    }

    /// The first source's boundary handle for `base_url`.
    pub fn boundary(&self, base_url: &str) -> Option<Arc<dyn BoundaryHost>> {
        self.0.iter().find_map(|s| s.boundary(base_url))
    }

    /// The first source's note for a linked worktree out of reach.
    pub fn worktree_note(&self, git_dir: &Path) -> Option<String> {
        self.0.iter().find_map(|s| s.worktree_note(git_dir))
    }

    /// Every source's names for the sandbox's denials.
    pub fn policy_tools(&self, tools: &mut PolicyTools) {
        for s in &self.0 {
            s.policy_tools(tools);
        }
    }

    pub(super) fn tools(&self, cfg: &ToolsConfig, deck: &Deck, jobs: &Jobs) -> Result<Vec<Arc<dyn Tool>>> {
        let mut out = Vec::new();
        for s in &self.0 {
            out.extend(s.tools(cfg, deck, jobs)?);
        }
        Ok(out)
    }
}
