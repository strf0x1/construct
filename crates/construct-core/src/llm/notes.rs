//! The conditions the client records instead of failing on: each is a branch
//! that used to be silent. Declared once here; call sites pass the constant
//! to `Notes::record` and nothing else. `Kind` and the `Notes` trait live
//! here so the client reports conditions through a type it owns; the
//! journal's `Coalescer` implements `Notes` and turns them into rows (see
//! `journal::notes`).

/// One condition worth a journal row: which subsystem saw it, what it is, and
/// how many rows of it a session keeps.
#[derive(Debug, Clone, Copy)]
pub struct Kind {
    pub class: &'static str,
    pub name: &'static str,
    pub cap: u32,
}

/// Where a subsystem reports a condition it recovered from.
pub trait Notes: Send {
    fn record(&mut self, kind: Kind, detail: &str);
}

/// A call whose notes nobody reads.
pub struct Discard;

impl Notes for Discard {
    fn record(&mut self, _: Kind, _: &str) {}
}

const CLASS: &str = "provider";

/// A 429, a 5xx or a connect/timeout error that was retried after a wait.
pub const RETRY: Kind = Kind { class: CLASS, name: "retry", cap: 20 };
/// An SSE `data:` line that was not JSON and was skipped.
pub const BAD_CHUNK: Kind = Kind { class: CLASS, name: "bad_chunk", cap: 20 };
/// The stream ended without a finish reason or `[DONE]`, or a length cutoff
/// took tool calls with it.
pub const TRUNCATED: Kind = Kind { class: CLASS, name: "truncated", cap: 20 };
/// A tool-call id changed at an index that already had one.
pub const INDEX_COLLISION: Kind = Kind { class: CLASS, name: "index_collision", cap: 20 };
/// A 400 that names a parameter we send (`max_tokens`, `temperature`,
/// `stream_options`).
pub const PARAM_REJECTED: Kind = Kind { class: CLASS, name: "param_rejected", cap: 20 };

/// Parameters whose rejection is worth a note.
pub const PARAMS: [&str; 3] = ["max_tokens", "temperature", "stream_options"];
