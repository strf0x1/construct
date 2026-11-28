//! The crate's one integration-test binary: every end-to-end test in this
//! directory is a module here, so the lib links once, not once per file.
//! `autotests = false` means a new file is not run until it is listed; the
//! `docs` gate in `cargo xtask check` fails until it is. Filter with
//! `cargo test -p construct-dev --test it <file>::`, e.g. `jobs::`.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] fns fail the test by panicking"
)]

mod common;

#[path = "acp.rs"]
mod acp;
#[path = "acp_keys.rs"]
mod acp_keys;
#[path = "acp_live.rs"]
mod acp_live;
#[path = "acp_payloads.rs"]
mod acp_payloads;
#[path = "catalog.rs"]
mod catalog;
#[path = "compaction.rs"]
mod compaction;
#[path = "crash_sweep.rs"]
mod crash_sweep;
#[path = "deck.rs"]
mod deck;
#[path = "deck_control.rs"]
mod deck_control;
#[path = "deck_github.rs"]
mod deck_github;
#[path = "developer.rs"]
mod developer;
#[path = "e2e.rs"]
mod e2e;
#[path = "hub.rs"]
mod hub;
#[path = "jobs.rs"]
mod jobs;
#[path = "join.rs"]
mod join;
#[path = "mcp.rs"]
mod mcp;
#[path = "models.rs"]
mod models;
#[path = "notes.rs"]
mod notes;
#[path = "optchat.rs"]
mod optchat;
#[path = "peer_acp.rs"]
mod peer_acp;
#[path = "peer_install.rs"]
mod peer_install;
#[path = "peer_llm.rs"]
mod peer_llm;
#[path = "peer_restart.rs"]
mod peer_restart;
#[path = "peer_tool.rs"]
mod peer_tool;
#[path = "peer_tui.rs"]
mod peer_tui;
#[path = "permission.rs"]
mod permission;
#[path = "providers.rs"]
mod providers;
#[path = "proxy.rs"]
mod proxy;
#[path = "recovery.rs"]
mod recovery;
#[path = "sandbox_memory.rs"]
mod sandbox_memory;
#[path = "sessions.rs"]
mod sessions;
#[path = "slash.rs"]
mod slash;
#[path = "snapshots.rs"]
mod snapshots;
#[path = "special_files.rs"]
mod special_files;
#[path = "stamp.rs"]
mod stamp;
#[path = "stream.rs"]
mod stream;
#[path = "tls.rs"]
mod tls;
#[path = "turn.rs"]
mod turn;
#[path = "wakes.rs"]
mod wakes;
