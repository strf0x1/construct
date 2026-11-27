//! A boundary decision Deck pushed over ACP reaches the model exactly once,
//! on its next request. It informs; it grants nothing.
//!
//! The design choice: the note is a real conversation message with its own
//! durable event, so a resume replays exactly what the model saw. It is not a
//! `Note`: that kind is for stream diagnostics and never reaches the model.

use super::{Agent, Event, Events};
use crate::llm::Message;
use crate::tools::boundary::BoundaryHost;
use std::sync::Arc;

/// The gateway sequence before a tool runs; `None` when there is no boundary
/// endpoint or it is unreachable.
pub(super) async fn before_tool(boundary: Option<&Arc<dyn BoundaryHost>>) -> Option<u64> {
    match boundary {
        Some(b) => b.cursor().await,
        None => None,
    }
}

/// Append what was blocked while the tool ran. A dead endpoint costs the note,
/// never the tool.
pub(super) async fn after_tool(boundary: Option<&Arc<dyn BoundaryHost>>, before: Option<u64>, out: &mut String) {
    let (Some(b), Some(before)) = (boundary, before) else {
        return;
    };
    if let Some(note) = b.tool_note(before).await {
        out.push_str("\n\n");
        out.push_str(&note);
    }
}

impl Agent {
    /// Put late boundary decisions into the conversation. Returns whether any
    /// were queued.
    pub(super) fn deliver_boundary(&mut self, tx: &Events) -> bool {
        let Some(boundary) = self.boundary.clone() else {
            return false;
        };
        let Some(late) = boundary.take_late() else {
            return false;
        };
        let message = Message::user(late.text);
        self.journal.emit(Event::DeckBoundary { ids: late.ids.clone(), message: message.clone() }, tx);
        self.context.push(message);
        for id in late.ids {
            boundary.mark_seen(&id);
        }
        true
    }
}
