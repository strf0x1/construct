//! One hub per home: an exclusive OS lock on `$CONSTRUCT_HOME/hub.lock` (#360).
//!
//! `flock`-style (`File::try_lock`), so the kernel releases it when the hub
//! exits for any reason, crash included: no stale pid file to clean up. The
//! pid is written into the file only so a refused second hub can say which
//! process holds it.

use anyhow::{Context, Result, bail};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Held for the hub's lifetime; dropping it releases the lock.
#[derive(Debug)]
pub struct Instance {
    _file: File,
}

/// Take the lock at `path`, or say who holds it.
pub fn acquire(path: &Path) -> Result<Instance> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let mut pid = String::new();
            let _ = file.read_to_string(&mut pid);
            let pid = pid.trim();
            let who = if pid.is_empty() { String::new() } else { format!(" (pid {pid})") };
            bail!("a hub is already running for this home{who}; `construct hub status` shows it");
        }
        Err(TryLockError::Error(e)) => return Err(e).with_context(|| format!("lock {}", path.display())),
    }
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(file, "{}", std::process::id())?;
    file.flush()?;
    Ok(Instance { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_is_refused_until_the_first_drops() {
        let d = std::env::temp_dir().join(format!("construct-hub-lock-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let path = d.join("hub.lock");
        let first = acquire(&path).unwrap();
        let err = acquire(&path).unwrap_err().to_string();
        assert!(err.contains("already running") && err.contains(&std::process::id().to_string()), "{err}");
        drop(first);
        acquire(&path).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }
}
