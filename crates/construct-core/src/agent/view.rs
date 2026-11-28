//! Narrow read-only views and session metadata operations on `Agent`. Callers
//! above the engine (TUI, ACP, CLI) inspect the session through these methods
//! instead of reaching into `Agent`'s internal fields.

use super::*;

impl Agent {
    /// A handle the UI can use to interrupt the running turn.
    pub fn canceller(&self) -> watch::Sender<bool> {
        self.cancel_tx.clone()
    }

    /// This session's durable identifier.
    pub fn session_id(&self) -> &str {
        &self.journal.session
    }

    /// All journal rows recorded for this session so far.
    pub fn entries(&self) -> Result<Vec<journal::Entry>> {
        self.journal.entries()
    }

    /// Record a human-chosen title for this session.
    pub fn rename(&self, title: &str) -> Result<()> {
        self.journal.rename(title)
    }

    /// The session's active configuration.
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// The session's tool catalog.
    pub fn catalog(&self) -> Arc<Catalog> {
        self.catalog.clone()
    }

    /// Active non-core tool group labels, for status display.
    pub fn active_group_labels(&self) -> Vec<String> {
        self.groups.iter().filter(|g| **g != Group::Core).map(Group::label).collect()
    }

    /// Deck's boundary host for this session, if running in a Deck task.
    pub fn boundary(&self) -> Option<Arc<dyn crate::tools::boundary::BoundaryHost>> {
        self.boundary.clone()
    }

    /// Wire the interactive permission prompt channel (`TuiAsk`, ACP `ClientAsk`, …).
    pub fn set_permission_ask(&self, ask: Arc<dyn crate::permission::Ask>) {
        self.perms.set_ask(ask);
    }

    /// The session's working directory.
    pub fn cwd(&self) -> &std::path::Path {
        self.ctx.cwd()
    }
}
