//! Deck's boundary contract on the tool side (ADR 0004 §6): the three tools
//! the agent chooses between while a host policy holds an approval, and the
//! note machinery that tells it a tool was blocked.
//!
//! The design choice: one shared [`Boundary`] per session. Tools read it from
//! `Ctx`; the ACP reader pushes late decisions into the same handle, so a
//! decision and a wait cannot disagree. The endpoint is read-only: no call
//! here can allow anything.

mod client;
mod note;
mod status;
mod wait;
mod withdraw;

pub use client::{Boundary, Entry, MAX_WAIT, Outcome};
pub use note::{Notice, notifications};
pub use status::Status;
pub use wait::Wait;
pub use withdraw::Withdraw;

/// The longest late-decision queue one session keeps.
pub const MAX_QUEUE: usize = 8;

/// Wire names, declared once; the note text and the tool impls both use them.
pub const WAIT: &str = "boundary_wait";
pub const STATUS: &str = "boundary_status";
pub const WITHDRAW: &str = "boundary_withdraw";
