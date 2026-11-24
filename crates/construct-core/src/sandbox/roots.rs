//! What a sandboxed shell gets under `$HOME`: toolchains, caches and a few
//! config files read-only, the state they write writable, and the single lock
//! files a toolchain opens in place. Nothing else under `$HOME` is in reach.

use std::path::{Path, PathBuf};

/// Toolchains, caches and config files under `$HOME`. Read-only unless listed in `WRITE`.
/// `~/.local/bin/cargo` points into `.local/share/mbx/bin`, so that target is
/// granted too: Landlock checks the resolved path of a link.
pub(super) const READ: &[&str] = &[
    ".local/share/mise",
    ".config/mise",
    ".local/share/mbx/bin",
    ".config/mbx/config.toml",
    ".local/bin",
    ".rustup",
    ".gitconfig",
    ".config/git",
    ".nvm",
    ".pyenv",
    ".bun/bin",
    "go/bin",
];
#[cfg(target_os = "linux")]
pub(super) const WRITE: &[&str] = &[".cargo", ".local/state/mise", ".cache", ".npm", ".bun/install", "go/pkg"];
/// `Library/Caches` is where macOS tools keep what `.cache` holds on Linux.
#[cfg(target_os = "macos")]
pub(super) const WRITE: &[&str] =
    &[".cargo", ".local/state/mise", ".cache", ".npm", ".bun/install", "go/pkg", "Library/Caches"];

/// Files under `$HOME` a toolchain writes in place rather than in a directory.
/// mise takes this lock beside its command-wrappers farm on every shim run and
/// fails the command when it can't create it (#279). Only the lock is
/// writable: the farm holds symlinks the user's own shell runs outside the
/// sandbox, and `installs/` holds the toolchain binaries. mbx's sibling
/// `.config.toml.lock` is only taken when editing settings, not on a build,
/// so a plain `cargo` never needs it (#372).
const LOCKS: &[&str] = &[".local/share/mise/command-wrappers/bin.lock"];

/// The readable (and executable) roots under `home`.
pub(super) fn read(home: &Path) -> Vec<PathBuf> {
    READ.iter().map(|p| home.join(p)).collect()
}

/// The writable roots under `home`. With `sandboxed` false the roots are
/// unused, so no lock file is made.
pub(super) fn write(home: &Path, sandboxed: bool) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = WRITE.iter().map(|p| home.join(p)).collect();
    if sandboxed {
        for f in LOCKS {
            let p = home.join(f);
            // A rule for a missing path is dropped later, and mise creates its
            // lock on first use: make it now, exactly where mise would.
            if p.parent().is_some_and(Path::exists)
                && std::fs::OpenOptions::new().create(true).append(true).open(&p).is_ok()
            {
                roots.push(p);
            }
        }
    }
    roots
}
