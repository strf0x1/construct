//! A running tool call's handle on its session: its id and, for a tool that
//! starts work which reports on its own later (a background job), the
//! session's journal and event stream.
//!
//! The design choice: that later work goes through the session's
//! `journal::Writer`, never through storage of the tool's own, so the session
//! is the only journal writer. Tools that need neither take `Tool::call` and
//! never see this.

use crate::event::{Event, Events};
use crate::journal::Writer;
use std::sync::Arc;

pub struct Call {
    pub id: String,
    sink: Option<(Arc<Writer>, Events)>,
}

impl Call {
    pub fn new(id: &str, journal: Arc<Writer>, tx: Events) -> Self {
        Self { id: id.to_string(), sink: Some((journal, tx)) }
    }

    /// The session's journal and event stream, for a tool that starts work
    /// which reports on its own later (`jobs`).
    pub fn sink(&self) -> Option<(Arc<Writer>, Events)> {
        self.sink.clone()
    }

    /// Journal and send one event of the session's own.
    pub fn emit(&self, ev: Event) {
        if let Some((journal, tx)) = &self.sink {
            journal.emit(ev, tx);
        }
    }

    /// A call with nowhere to stream to (tests, the learning pass).
    pub fn detached(id: &str) -> Self {
        Self { id: id.to_string(), sink: None }
    }
}
