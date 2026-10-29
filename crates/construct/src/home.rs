//! Keeping `$CONSTRUCT_HOME` private. The home holds memory, project kits and
//! the journals (full transcripts), so other local users must not read it.
//! The one design choice: only the home directory's own mode is managed.
//! Everything under it is unreachable once the home is `0700`, so the stores
//! that create subdirectories need no chmods of their own. An existing home is
//! tightened once, never loosened, and a `.private` marker records that: a
//! user who widens it again afterwards is left alone (ADR 0021).

use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::Path;

/// Written once the home has been made private; its presence means "hands off".
pub const MARKER: &str = ".private";

/// What [`make_private`] did, so the caller can tell the user.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The marker was there: the mode is the user's choice.
    Settled,
    /// The home did not exist and was created `0700`.
    Created,
    /// The home was already closed to group and others.
    AlreadyPrivate,
    /// The home was opened to group or others; it was `from` and is now `to`.
    Tightened { from: u32, to: u32 },
    /// Another account owns the home; it was not touched.
    NotOwner,
}

/// Create the home `0700`, or drop group and other bits from an existing one
/// the first time construct sees it. One `stat` when already settled, so it
/// costs nothing on the first-frame path.
pub fn make_private(home: &Path) -> std::io::Result<Outcome> {
    let marker = home.join(MARKER);
    if marker.exists() {
        return Ok(Outcome::Settled);
    }
    let outcome = match std::fs::metadata(home) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = home.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::DirBuilder::new().mode(0o700).create(home)?;
            // The umask can only remove bits, but say what we mean.
            std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
            Outcome::Created
        }
        Err(e) => return Err(e),
        // SAFETY: getuid cannot fail.
        Ok(m) if m.uid() != unsafe { libc::getuid() } => return Ok(Outcome::NotOwner),
        Ok(m) => {
            let from = m.mode() & 0o7777;
            let to = from & !0o077;
            if from == to {
                Outcome::AlreadyPrivate
            } else {
                std::fs::set_permissions(home, std::fs::Permissions::from_mode(to))?;
                Outcome::Tightened { from, to }
            }
        }
    };
    std::fs::write(&marker, b"construct made this directory private; it will not change its mode again.\n")?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().mode() & 0o777
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("construct-home-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn new_home_is_created_private() {
        let root = scratch("new");
        let home = root.join("a/home");
        assert_eq!(make_private(&home).unwrap(), Outcome::Created);
        assert_eq!(mode(&home), 0o700);
        assert!(home.join(MARKER).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn open_home_is_tightened_once_and_never_loosened() {
        let home = scratch("open");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(make_private(&home).unwrap(), Outcome::Tightened { from: 0o755, to: 0o700 });
        assert_eq!(mode(&home), 0o700);
        // The user shares it again on purpose: construct leaves it alone.
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(make_private(&home).unwrap(), Outcome::Settled);
        assert_eq!(mode(&home), 0o750);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn stricter_home_keeps_its_mode() {
        let home = scratch("strict");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o500)).unwrap();
        // 0500 cannot take the marker; the mode must still not widen.
        assert!(make_private(&home).is_err() || mode(&home) == 0o500);
        assert_eq!(mode(&home), 0o500);
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(make_private(&home).unwrap(), Outcome::AlreadyPrivate);
        std::fs::remove_dir_all(home).unwrap();
    }
}
