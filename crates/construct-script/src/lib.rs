//! construct's script layer (ADR 0079, #417): one confined QuickJS runtime,
//! one capability-scoped Host Bridge (`ctx`) and one hot-reload lifecycle,
//! shared by every script role. Today the role is wake scripts
//! (`WakeScripts`, a `WakeFilter`); learned tool scripts (#186) and Code Mode
//! (#267) build on the same `sandbox`, `bridge` and `reload`.
//!
//! The design choice: this crate sits above the engine behind the binary's
//! `script` feature, so `construct-core`, `lean` and bare peers never link
//! QuickJS. Core only offers the `WakeFilter` seam.

mod bridge;
mod reload;
pub mod sandbox;
mod state;
mod wake;

pub use sandbox::Limits;
pub use wake::{EVENT_FAILED, NOTE_CLASS, RELOAD_FAILED, WakeScripts};
