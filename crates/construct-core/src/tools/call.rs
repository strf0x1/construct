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
use std::time::{Duration, Instant};

pub struct Call {
    pub id: String,
    sink: Option<(Arc<Writer>, Events)>,
}

/// Minimum spacing between live `ToolOutput` chunks so runaway output cannot
/// flood the event channel while normal line-by-line commands stream at once.
const STREAM_INTERVAL: Duration = Duration::from_millis(33);
const STREAM_FLUSH_BYTES: usize = 2048;
const STREAM_CAP_BYTES: usize = 4096;

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

    /// Send a non-durable live event without journaling it.
    pub fn stream(&self, ev: Event) {
        if let Some((_, tx)) = &self.sink {
            let _ = tx.send(ev);
        }
    }

    /// A throttled live-output emitter for a running command.
    pub fn output_stream(&self) -> OutputStream<'_> {
        OutputStream { call: self, pending: Vec::new(), last: None }
    }

    /// A call with nowhere to stream to (tests, the learning pass).
    pub fn detached(id: &str) -> Self {
        Self { id: id.to_string(), sink: None }
    }
}

/// Coalesces rapid command output chunks into bounded, ~30 fps `ToolOutput`
/// events, while flushing sparse lines immediately.
pub struct OutputStream<'a> {
    call: &'a Call,
    pending: Vec<u8>,
    last: Option<Instant>,
}

impl OutputStream<'_> {
    pub fn push(&mut self, bytes: &[u8]) {
        if self.call.sink.is_none() || bytes.is_empty() {
            return;
        }
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > STREAM_CAP_BYTES {
            let drop = self.pending.len() - STREAM_CAP_BYTES;
            self.pending.drain(..drop);
        }
        if self.pending.len() >= STREAM_FLUSH_BYTES || self.last.is_none_or(|t| t.elapsed() >= STREAM_INTERVAL) {
            self.flush();
        }
    }

    /// Resolves once a buffered chunk is due to flush; pends forever when empty.
    pub async fn ready(&self) {
        if self.pending.is_empty() {
            std::future::pending::<()>().await;
        }
        let wait = self.last.map_or(Duration::ZERO, |t| STREAM_INTERVAL.saturating_sub(t.elapsed()));
        tokio::time::sleep(wait).await;
    }

    pub fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        self.last = Some(Instant::now());
        self.call.stream(Event::ToolOutput { id: self.call.id.clone(), text });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_stream_drops_silently() {
        let call = Call::detached("c1");
        let mut s = call.output_stream();
        s.push(b"hello\n");
        assert!(s.pending.is_empty());
    }
}
