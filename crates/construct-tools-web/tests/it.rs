//! The crate's one integration-test binary: every contract test in this
//! directory is a module here. `autotests = false` means a new file is not
//! run until it is listed; the `docs` gate in `cargo xtask check` fails until
//! it is. Filter with `cargo test -p construct-tools-web --test it <file>::`.

#[path = "web_source.rs"]
mod web_source;
