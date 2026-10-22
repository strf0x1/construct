//! Delegation to a child construct process. `wire` is the transport between
//! the two (JSON lines on the child's stdin and stdout) and the only module
//! that knows its shape, so ACP (#33) can replace it; `serve` is the child's
//! side of it.
//!
//! The design choice: the child is the same binary run as the hidden
//! `__child` subcommand under a narrowed sandbox, and its stdout is nothing
//! but `Event` lines, the type `--json` prints and the journal stores. The
//! parent turns those lines into its own events and rows (`tools::Call`); the
//! child's `CONSTRUCT_HOME` is a scratch directory that dies with it.

pub mod run;
pub mod serve;
pub mod wire;

pub use wire::Origin;

/// Children deeper than this cannot be started, whatever a definition says.
pub const MAX_DEPTH: u32 = 1;
/// How many delegations deep this process is (0 for a user's session).
pub const DEPTH_ENV: &str = "CONSTRUCT_DEPTH";

pub fn depth() -> u32 {
    std::env::var(DEPTH_ENV).ok().and_then(|v| v.parse().ok()).unwrap_or(0)
}
