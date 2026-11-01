//! The kernel-checked open beneath a directory fd, per platform (see
//! `open.rs`, which says why).
//!
//! Linux asks `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`, and where
//! that isn't there (kernel before 5.6, a seccomp filter) falls back to plain
//! `openat` on the already-canonicalized path. macOS has no `openat2`, but its
//! `O_NOFOLLOW_ANY` refuses a symlink anywhere in the path, which on a path
//! that was canonicalized a moment ago is the same guarantee.

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[cfg(target_os = "linux")]
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
#[cfg(target_os = "linux")]
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
#[cfg(target_os = "linux")]
const RESOLVE_BENEATH: u64 = 0x08;

/// An fd for metadata and directory walking that needs no read permission.
/// macOS has no `O_PATH`; `O_EVTONLY` is its closest match (and, unlike
/// `O_RDONLY`, does not block opening a FIFO).
#[cfg(target_os = "linux")]
pub(super) const O_PATH: i32 = libc::O_PATH;
#[cfg(target_os = "macos")]
pub(super) const O_PATH: i32 = libc::O_EVTONLY;
/// Fail with ELOOP on a symlink in any component of the path, not only the last.
#[cfg(target_os = "macos")]
const O_NOFOLLOW_ANY: i32 = 0x2000_0000;

pub(super) fn cstr(p: &OsStr) -> io::Result<CString> {
    CString::new(p.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

pub(super) fn owned(fd: libc::c_long) -> io::Result<OwnedFd> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh descriptor the kernel just returned to us alone.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

/// Open `rel` below `dir`, not following links out of it.
pub(super) fn open_beneath(dir: RawFd, rel: &Path, flags: i32, mode: u32) -> io::Result<OwnedFd> {
    let c = cstr(rel.as_os_str())?;
    #[cfg(target_os = "linux")]
    {
        let how = OpenHow {
            flags: (flags | libc::O_CLOEXEC) as u64,
            mode: mode as u64,
            resolve: RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS,
        };
        // SAFETY: `c` is NUL-terminated and outlives the call; `how` is a
        // repr(C) open_how whose size is passed alongside.
        let r = unsafe {
            libc::syscall(libc::SYS_openat2, dir, c.as_ptr(), &how as *const OpenHow, std::mem::size_of::<OpenHow>())
        };
        match owned(r) {
            Err(e) if matches!(e.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)) => {}
            r => return r,
        }
    }
    #[cfg(target_os = "macos")]
    // O_NOFOLLOW_ANY implies O_NOFOLLOW, and the two together are EINVAL.
    let flags = flags & !libc::O_NOFOLLOW | O_NOFOLLOW_ANY;
    // SAFETY: as above.
    owned(unsafe { libc::openat(dir, c.as_ptr(), flags | libc::O_CLOEXEC, mode) } as libc::c_long)
}
