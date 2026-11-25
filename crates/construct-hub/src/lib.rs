//! `construct hub`: the long-running host process peers dial home to (#273, #360).
//!
//! It owns the controller X25519 key (`hub.key`), the peer registry
//! (`hub/peers.json`), a WebSocket listener speaking the peer wire
//! (`construct-peer`, ADR 0068), and a `0600` unix control socket
//! (`hub.sock`) that TUIs and CLI commands attach to. Everything lives inside
//! `$CONSTRUCT_HOME`, never `/tmp`, so the home's `0700` (ADR 0021) guards it
//! twice. The one design choice: the hub process is the only writer of the
//! registry, and one hub per home is enforced by an OS file lock, so
//! `construct hub status` can read the file without coordination when no hub
//! runs. Linked only behind the binary's `hub` feature (in `full`, not `lean`
//! or bare, ADR 0064, 0069). Joining, approval and revocation are ADR 0070.

pub mod admit;
mod conn;
pub mod control;
pub mod files;
pub mod invite;
pub mod lock;
pub mod offline;
mod ops;
pub mod registry;
pub mod server;
mod state;
pub mod status;

pub use files::Files;
pub use server::{Options, serve};
pub use status::Status;
