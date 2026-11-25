//! The controller's static X25519 keypair, `$CONSTRUCT_HOME/hub.key` (#360).
//!
//! Created once with `O_EXCL` and mode `0600` from the start (never written
//! wide and narrowed after), holding only the 32 private bytes; the public key
//! is derived. A key file that group or others can read is refused, as ssh
//! refuses a loose private key: the key is the hub's whole identity, and
//! every stamped peer pins its public half.

use anyhow::{Context, Result, bail};
use construct_peer::{KEY_LEN, Keypair};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Load the key at `path`, creating it if it does not exist.
pub fn load_or_create(path: &Path) -> Result<Keypair> {
    match load(path) {
        Ok(Some(kp)) => Ok(kp),
        Ok(None) => create(path),
        Err(e) => Err(e),
    }
}

/// Load the key at `path`; `None` when there is no key yet.
pub fn load(path: &Path) -> Result<Option<Keypair>> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("open {}", path.display())),
    };
    let mode = file.metadata()?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!("{} is readable by others (mode {mode:o}); run `chmod 600 {}`", path.display(), path.display());
    }
    let mut raw = Vec::with_capacity(KEY_LEN);
    file.read_to_end(&mut raw)?;
    let private: [u8; KEY_LEN] = raw
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("{} is {} bytes, expected a {KEY_LEN}-byte key", path.display(), raw.len()))?;
    Ok(Some(Keypair::from_private(private)))
}

fn create(path: &Path) -> Result<Keypair> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let kp = Keypair::generate().map_err(|e| anyhow::anyhow!("generate hub key: {e}"))?;
    let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
        Ok(f) => f,
        // Lost a race with another `construct hub key`: use theirs.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return load(path)?.context("hub key vanished while reading it");
        }
        Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
    };
    file.write_all(kp.private_bytes())?;
    file.sync_all()?;
    Ok(kp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("construct-hub-key-{name}-{}", std::process::id()));
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
