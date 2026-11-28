//! The view the context strategy sends to the model. The strategy owns the
//! message list (ADR 0042); the rest of the agent reads it through here so no
//! caller depends on how a strategy stores it.

use super::*;

impl Agent {
    /// The view the strategy sends to the model.
    pub fn messages(&self) -> &[Message] {
        self.context.messages()
    }

    /// Every message the session logged, including ones the view dropped
    /// (ADR 0043 decision 12): learning and routing read this.
    pub fn log(&self) -> &[Message] {
        self.context.log()
    }

    /// The tool sources this session was built with, for background learning.
    pub fn tool_sources(&self) -> &ToolSources {
        &self.sources
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
