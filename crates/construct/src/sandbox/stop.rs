//! Stopping a sandboxed process the way its helper expects: SIGTERM first, so
//! the helper can sweep everything the program started, and the process group
//! killed only if that fails. Shell calls and delegated children share it.

use std::time::Duration;

/// How long the helper gets to sweep after SIGTERM (its own cap is 2s) before
/// the whole group is killed.
pub const GRACE: Duration = Duration::from_secs(3);

/// Asks the helper to stop and sweep if the call is abandoned (the turn's
/// future dropped) before the helper was reaped. SIGKILL would kill the helper
/// before it could sweep, and orphans would escape to init.
pub struct StopOnDrop(pub Option<u32>);

impl StopOnDrop {
    pub fn stop(&self) {
        if let Some(pid) = self.0 {
            // SAFETY: plain syscall on our unreaped child.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }
    pub fn kill_group(&self) {
        if let Some(pid) = self.0 {
            // SAFETY: plain syscall on a process group we created.
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        }
    }
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.stop();
    }
}
