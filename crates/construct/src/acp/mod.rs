//! The Agent Client Protocol (agentclientprotocol.com), both ways. `server`
//! is `construct acp`: construct as an agent an editor or another harness
//! drives. `client` runs any ACP agent as a delegated child. `rpc` is the
//! framing they share; `map` is `Event` to ACP updates and back.
//!
//! The design choice: ACP is a translation at the process edge. The agent
//! loop, the journal and the delegate tool know nothing of it; what crosses
//! the wire is `Event`s, as it does on the JSONL child transport.

pub mod client;
pub mod map;
pub mod rpc;
pub mod server;

use crate::child::Origin;
use crate::config::Config;
use serde::{Deserialize, Serialize};

/// The protocol version construct speaks.
pub const VERSION: u64 = 1;

/// What a parent hands a built-in child in `session/new`'s
/// `_meta.construct.start`: the setup the JSONL transport sends as `start`.
/// Only `construct __child --acp` honours it; the public `construct acp`
/// ignores it, so a client cannot widen what the user configured.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Start {
    pub config: Box<Config>,
    pub origin: Box<Origin>,
}
