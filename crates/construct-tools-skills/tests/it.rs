//! The crate's one integration-test binary: every contract test in this
//! directory is a module here. `autotests = false` means a new file is not
//! run until it is listed; the `docs` gate in `cargo xtask check` fails until
//! it is. Filter with `cargo test -p construct-tools-skills --test it <file>::`.

#![expect(clippy::unwrap_used, reason = "test helpers outside #[test] fns fail the test by panicking")]

#[path = "skill_patch.rs"]
mod skill_patch;
#[path = "skills_source.rs"]
mod skills_source;
