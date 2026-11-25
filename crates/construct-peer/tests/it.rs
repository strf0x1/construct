//! The crate's one integration-test binary: every contract test in this
//! directory is a module here, so the lib links once, not once per file.
//! `autotests = false` means a new file is not run until it is listed; the
//! `docs` gate in `cargo xtask check` fails until it is.

#![expect(clippy::unwrap_used, reason = "test helpers outside #[test] fns fail the test by panicking")]

#[path = "wire_contract.rs"]
mod wire_contract;
