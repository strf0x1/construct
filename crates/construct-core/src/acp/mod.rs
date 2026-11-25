//! The Agent Client Protocol (agentclientprotocol.com). `server` is
//! construct as an agent an editor, Deck or a hub drives, over any line
//! transport; `stdio` is `construct acp`, that server on stdin and stdout.
//! `rpc` is its framing; `map` is `Event` to ACP updates; `ask` puts
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
pub mod stdio;
mod turn;

/// The protocol version construct speaks.
pub const VERSION: u64 = 1;

/// The `cwd` every session method requires: an absolute-enough path the agent
/// resolves against the process's working directory.
pub fn cwd(params: &serde_json::Value) -> anyhow::Result<std::path::PathBuf> {
    params["cwd"]
        .as_str()
        .filter(|c| !c.is_empty())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("cwd is required"))
}
