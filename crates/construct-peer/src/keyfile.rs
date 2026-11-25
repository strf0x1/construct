//! A static keypair on disk: the hub's `hub.key` and a peer's `peer.key`
//! (#360, #361).
//!
//! Created once with `O_EXCL` and mode `0600` from the start (never written
//! wide and narrowed after), holding only the 32 private bytes; the public key
//! is derived. A key file that group or others can read is refused, as ssh
//! refuses a loose private key: the key is the side's whole identity. Both
//! sides share this so the rules cannot drift apart.

use crate::keys::{KEY_LEN, Keypair};
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Load the key at `path`, creating it if it does not exist.
pub fn load_or_create(path: &Path) -> Result<Keypair> {
    match load(path)? {
        Some(kp) => Ok(kp),
        None => create(path),
    }
}

/// Load the key at `path`; `None` when there is no key yet.
pub fn load(path: &Path) -> Result<Option<Keypair>> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::new(e.kind(), format!("open {}: {e}", path.display()))),
    };
    let mode = file.metadata()?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!("{} is readable by others (mode {mode:o}); run `chmod 600 {}`", path.display(), path.display()),
        ));
    }
    let mut raw = Vec::with_capacity(KEY_LEN);
    file.read_to_end(&mut raw)?;
    let private: [u8; KEY_LEN] = raw.as_slice().try_into().map_err(|_| {
        Error::new(
            ErrorKind::InvalidData,
            format!("{} is {} bytes, expected a {KEY_LEN}-byte key", path.display(), raw.len()),
        )
    })?;
    Ok(Some(Keypair::from_private(private)))
}

/// Delete the key at `path` (a revoked peer, #361). Missing is fine.
pub fn delete(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn create(path: &Path) -> Result<Keypair> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let kp = Keypair::generate().map_err(|e| Error::other(format!("generate key: {e}")))?;
    // Written whole to a private temp file, then hard-linked into place: the
    // key appears complete or not at all (a reader never sees 0 bytes), and
    // the link fails if another process got there first.
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| Error::new(e.kind(), format!("create {}: {e}", tmp.display())))?;
    let written = file.write_all(kp.private_bytes()).and_then(|()| file.sync_all());
    let linked = written.and_then(|()| std::fs::hard_link(&tmp, path));
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(kp),
        // Lost a race with another process creating it: use theirs.
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            load(path)?.ok_or_else(|| Error::other(format!("{} vanished while reading it", path.display())))
        }
        Err(e) => Err(Error::new(e.kind(), format!("create {}: {e}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("construct-peer-keyfile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn created_once_private_and_stable() {
        let d = dir("stable");
        let path = d.join("hub.key");
        let first = load_or_create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read(&path).unwrap().len(), KEY_LEN);
        let again = load_or_create(&path).unwrap();
        assert_eq!(first.public(), again.public());
        delete(&path).unwrap();
        assert!(load(&path).unwrap().is_none());
        delete(&path).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_loose_key_is_refused() {
        let d = dir("loose");
        let path = d.join("hub.key");
        load_or_create(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = load(&path).unwrap_err().to_string();
        assert!(err.contains("readable by others"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
