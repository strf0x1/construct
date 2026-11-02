//! The Agent Client Protocol (agentclientprotocol.com), both ways. `server`
//! is `construct acp`: construct as an agent an editor or another harness
//! drives. `client` runs any ACP agent as a delegated child. `rpc` is the
//! framing they share; `map` is `Event` to ACP updates and back; `ask` puts
//! the server's permission questions to its client.
//!
//! The design choice: ACP is a translation at the process edge. The agent
//! loop, the journal and the delegate tool know nothing of it; what crosses
//! the wire is `Event`s, as it does on the JSONL child transport.

pub mod ask;
pub mod client;
mod live;
mod load;
pub mod map;
mod questions;
mod replay;
pub mod rpc;
pub mod server;
mod turn;

use crate::child::Origin;
use crate::config::Config;
use serde::{Deserialize, Serialize};

/// The protocol version construct speaks.
pub const VERSION: u64 = 1;

/// Take the protocol's own handle on stdout, then point fd 1 at stderr, so a
/// `println!` or a dependency's log becomes a diagnostic instead of a frame
/// (ADR 0035). `None` when the descriptor could not be taken or repointed; fd
/// 1 is unchanged then and the caller falls back to it.
pub(crate) fn claim_stdout() -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::AsFd;
    let protocol = std::io::stdout().as_fd().try_clone_to_owned().ok()?;
    // SAFETY: both descriptors are open for the life of the process.
    if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } == -1 {
        // fd 1 still names the protocol stream. Say so and return `None`: the
        // caller keeps writing frames there, exactly as before the change, so
        // the failure is a missing guarantee rather than a broken session.
        eprintln!(
            "construct acp: could not point stdout at stderr ({}); diagnostics can reach the protocol stream",
            std::io::Error::last_os_error()
        );
        return None;
    }
    Some(protocol)
}

/// What a parent hands a built-in child in `session/new`'s
/// `_meta.construct.start`: the setup the JSONL transport sends as `start`.
/// Only `construct __child --acp` honours it; the public `construct acp`
/// ignores it, so a client cannot widen what the user configured.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Start {
    pub config: Box<Config>,
    pub origin: Box<Origin>,
}
