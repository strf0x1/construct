//! A short alias for the private scratch dir.
//!
//! Unix socket paths cap at about 108 bytes (`sun_path`), and a deep
//! `$CONSTRUCT_HOME` leaves tools like mbx no room for their sockets. The
//! alias is a symlink in `/tmp`: scratch files stay in the protected home and
//! on disk, so privacy and the memory ceiling are unchanged. Only the alias
//! lives in the shared temp dir; literal `/tmp` is still outside the grant.

use std::path::{Path, PathBuf};

/// A short path to `target`, or `None` if this system will not make the link.
pub(super) fn short(target: &Path) -> Option<PathBuf> {
    let link = PathBuf::from("/tmp").join(format!("construct-{}", std::process::id()));
    if std::fs::read_link(&link).ok().as_deref() != Some(target) {
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(target, &link).ok()?;
    }
    Some(link)
}
