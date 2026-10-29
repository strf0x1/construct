//! The crate's one integration-test binary: every end-to-end test in this
//! directory is a module here, so the lib links once, not once per file.
//! `autotests = false` means a new file is not run until it is listed; the
//! `docs` gate in `cargo xtask check` fails until it is. Filter with
//! `cargo test -p construct-dev --test it <file>::`, e.g. `delegate::`.

mod common;

#[path = "acp.rs"]
mod acp;
#[path = "catalog.rs"]
mod catalog;
#[path = "child.rs"]
mod child;
#[path = "compaction.rs"]
mod compaction;
#[path = "delegate.rs"]
mod delegate;
#[path = "e2e.rs"]
mod e2e;
#[path = "models.rs"]
mod models;
#[path = "notes.rs"]
mod notes;
#[path = "permission.rs"]
mod permission;
#[path = "slash.rs"]
mod slash;
#[path = "snapshots.rs"]
mod snapshots;
