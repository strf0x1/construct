//! The Agent Client Protocol (agentclientprotocol.com), both ways. `server`
//! is `construct acp`: construct as an agent an editor or another harness
//! drives. `rpc` is its framing; `map` is `Event` to ACP updates; `ask` puts
//! the server's permission questions to its client.
//!
//! The design choice: ACP is a translation at the process edge. The agent
//! loop and the journal know nothing of it; what crosses the wire is `Event`s.

pub mod ask;
mod deck;
mod keys;
mod live;
mod load;
pub mod map;
mod questions;
mod replay;
pub mod rpc;
pub mod server;
mod turn;

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

/// The `cwd` every session method requires: an absolute-enough path the agent
/// resolves against the process's working directory.
pub fn cwd(params: &serde_json::Value) -> anyhow::Result<std::path::PathBuf> {
    params["cwd"]
        .as_str()
        .filter(|c| !c.is_empty())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("cwd is required"))
}
