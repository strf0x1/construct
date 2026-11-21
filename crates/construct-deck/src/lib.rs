//! The coordinator's `deck` tool (#222, contract 1.6.0 §13, Deck ADR 0011
//! §4): the closed set of task-control operations a Deck coordinator uses to
//! create, integrate, prompt, wait for and stop worker tasks through Deck's gateway.
//!
//! The design choice: the tool is a typed mirror of the wire, nothing more.
//! Each operation takes exactly the wire's fields, is checked here so a
//! malformed call never leaves the process, and goes out as one bounded JSON
//! body; the host decides everything else. There is no operation Deck does
//! not serve, so the tool cannot answer approvals, grant, publish, destroy,
//! resume, start, mark done or change budgets, and it never retries.
//!
//! This crate sits above `construct-core` (#314): the composition root hands
//! `DeckTools` to the session builder, and the tool exists only for a
//! coordinator task, the one that has a control gateway URL.

mod boundary;
mod control;
mod developer;
mod source;

pub use boundary::Notice;
pub use control::*;
pub use source::DeckTools;
