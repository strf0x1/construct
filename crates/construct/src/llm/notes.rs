//! The conditions the client records instead of failing on: each is a branch
//! that used to be silent. Declared once here; call sites pass the constant
//! to `Coalescer::record` and nothing else (see `journal::notes`).

use crate::journal::notes::Kind;

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
