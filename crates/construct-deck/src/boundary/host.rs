//! `Boundary` as the engine's [`BoundaryHost`]: what the agent loop and the
//! ACP reader call without naming Deck's client.

use super::{Boundary, Notice, note};
use construct_core::tools::boundary::{BoundaryHost, Late};
use serde_json::Value;
use std::any::Any;

#[async_trait::async_trait]
impl BoundaryHost for Boundary {
    async fn cursor(&self) -> Option<u64> {
        Boundary::cursor(self).await
    }

    async fn tool_note(&self, since: u64) -> Option<String> {
        Boundary::tool_note(self, since).await
    }

    /// A late decision: informational, and only for the session it names (or
    /// every session when it names none). It grants nothing.
    fn push_notice(&self, params: &Value, session: &str) {
        let Ok(notice) =
            serde_json::from_value::<Notice>(params.clone()).map_err(|e| e.to_string()).and_then(Notice::validate)
        else {
            return;
        };
        if notice.session.as_deref().is_none_or(|id| id == session) {
            self.queue_notice(notice);
        }
    }

    fn take_late(&self) -> Option<Late> {
        let notices = self.take_notices();
        if notices.is_empty() {
            return None;
        }
        Some(Late { ids: notices.iter().map(|n| n.id.clone()).collect(), text: note::notifications(&notices) })
    }

    fn mark_seen(&self, id: &str) {
        Boundary::mark_seen(self, id);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
