//! `peer`: the coordinator's way to send work to peers, constructs dropped
//! on other machines that dial home to this host's hub (#273, #366).
//!
//! The design choice: this crate never names the hub. It talks to a `Hub`,
//! a narrow trait the binary implements over the hub's control socket, the
//! same seam the TUI's peer tile uses (ADR 0074). An async `send` is an
//! ordinary background job of kind `remote` whose body only watches the hub
//! for that prompt's answer by its idempotency key, so a reconnect or a
//! resend never ends it twice and the coordinator is woken exactly once, at
//! a turn boundary, by core's job delivery (ADR 0026). What a peer says is
//! remote and untrusted: it reaches the model fenced (`fence`), never as
//! prose of the host's own. The tool cannot answer a peer's permission
//! question or revoke, build or invite one (ADR 0075).
//!
//! An optional tool crate (ADR 0064): the binary registers `Peers` when its
//! `hub` feature is on, and the tool exists only in a home that has a hub.

mod fence;
mod hub;
mod tool;
mod watch;

pub use fence::fence;
pub use hub::{Answered, Hub, PeerRow, Session};
pub use tool::PeerTool;

use construct_core::config::ToolsConfig;
use construct_core::tools::{Deck, Tool, ToolSource};
use std::sync::Arc;

/// What the composition root registers: the `peer` tool, when `hub` says
/// this home has a hub.
pub struct Peers(pub Arc<dyn Hub>);

impl ToolSource for Peers {
    fn tools(&self, _cfg: &ToolsConfig, _deck: &Deck) -> anyhow::Result<Vec<Arc<dyn Tool>>> {
        if !self.0.present() {
            return Ok(vec![]);
        }
        Ok(vec![Arc::new(PeerTool::new(self.0.clone()))])
    }
}
