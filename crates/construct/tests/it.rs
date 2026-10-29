//! The crate's one integration-test binary: every contract test in this
//! directory is a module here, so the lib links once, not once per file.
//! `autotests = false` means a new file is not run until it is listed; the
//! `docs` gate in `cargo xtask check` fails until it is. Filter with
//! `cargo test -p construct --test it <file>::`.

#[path = "acp_kinds.rs"]
mod acp_kinds;
#[path = "edit_contract.rs"]
mod edit_contract;
#[path = "grep_search.rs"]
mod grep_search;
#[path = "hashline_contract.rs"]
mod hashline_contract;
#[path = "skill_patch.rs"]
mod skill_patch;
#[path = "symlink_farm.rs"]
mod symlink_farm;
#[path = "tool_contract.rs"]
mod tool_contract;
