//! The only way the file tools touch the filesystem.
//!
//! `Policy::check_read` canonicalizes a path and compares it with the granted
//! roots; opening it by name afterwards leaves a window in which another
//! process (a parallel shell call, a delegated child) can swap a symlink in.
//! Here the check and the open are one step: find the granted root holding
//! the real path, open the root as a directory fd, and open the rest with
//! `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` plus `O_NOFOLLOW`, so
//! the kernel refuses anything that leaves the root. Created files and the
//! temp-file-plus-rename of an atomic replace go through a directory fd too
//! (`openat`, `renameat`), so the destination can't be redirected.
//!
//! Where `openat2` isn't there (kernel before 5.6, a seccomp filter, not
//! Linux) the open falls back to `openat` relative to the root fd on the
//! already-canonicalized path: today's canonicalize-and-check.
//!
//! Not covered: the granted root itself is opened by name, so replacing one
//! of the root's own parent directories is out of scope.

use super::Policy;
use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_BENEATH: u64 = 0x08;

fn cstr(p: &OsStr) -> io::Result<CString> {
    CString::new(p.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn owned(fd: libc::c_long) -> io::Result<OwnedFd> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh descriptor the kernel just returned to us alone.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

/// Open `rel` below `dir`, not following links out of it.
fn open_beneath(dir: RawFd, rel: &Path, flags: i32, mode: u32) -> io::Result<OwnedFd> {
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
    // SAFETY: as above.
    owned(unsafe { libc::openat(dir, c.as_ptr(), flags | libc::O_CLOEXEC, mode) } as libc::c_long)
}

fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    let c = cstr(path.as_os_str())?;
    // SAFETY: `c` is NUL-terminated and outlives the call.
    owned(unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) } as libc::c_long)
}

fn open_path(path: &Path, flags: i32, mode: u32) -> io::Result<OwnedFd> {
    let c = cstr(path.as_os_str())?;
    // SAFETY: `c` is NUL-terminated and outlives the call.
    owned(unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC, mode) } as libc::c_long)
}

/// Where a path really is, relative to the granted root that holds it.
struct At<'a> {
    policy: &'a Policy,
    /// None when the policy is off: no boundary, only the real path.
    root: Option<PathBuf>,
    real: PathBuf,
    rel: PathBuf,
    what: &'static str,
}

fn locate<'a>(policy: &'a Policy, path: &Path, write: bool) -> Result<At<'a>, String> {
    let what = if write { "write" } else { "read" };
    let real = Policy::real(path);
    if policy.off {
        return Ok(At { policy, root: None, rel: PathBuf::new(), real, what });
    }
    let roots: Vec<&PathBuf> = if write {
        policy.write.iter().collect()
    } else {
        policy.read.iter().chain(&policy.write).chain(&policy.system).collect()
    };
    let root = roots
        .into_iter()
        .filter(|r| real.starts_with(r))
        .max_by_key(|r| r.components().count())
        .ok_or_else(|| policy.denied(&real, what))?;
    let rel = real.strip_prefix(root).map(Path::to_path_buf).unwrap_or_default();
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("{}: not a usable path", path.display()));
    }
    Ok(At { policy, root: Some(root.clone()), rel, real, what })
}

impl At<'_> {
    /// A refused escape reads like the sandbox's own refusal, so the caller's
    /// prompt logic treats it the same; anything else is the OS's message.
    fn fail(&self, e: io::Error) -> String {
        match e.raw_os_error() {
            Some(libc::EXDEV | libc::ELOOP) => self.policy.denied(&self.real, self.what),
            _ => format!("{}: {e}", self.real.display()),
        }
    }

    fn open(&self, flags: i32, mode: u32) -> Result<OwnedFd, String> {
        let r = match &self.root {
            None => open_path(&self.real, flags, mode),
            Some(root) if self.rel.as_os_str().is_empty() => open_path(root, flags | libc::O_NOFOLLOW, mode),
            Some(root) => {
                open_dir(root).and_then(|d| open_beneath(d.as_raw_fd(), &self.rel, flags | libc::O_NOFOLLOW, mode))
            }
        };
        r.map_err(|e| self.fail(e))
    }

    /// The directory holding the target and the target's name, creating
    /// missing directories one component at a time when `mkdirs`. None when
    /// the target is a granted root itself (a single approved file).
    fn parent(&self, mkdirs: bool) -> Result<Option<(OwnedFd, OsString)>, String> {
        let name = self.real.file_name().ok_or_else(|| format!("{}: not a file path", self.real.display()))?.to_owned();
        let step = |dir: &OwnedFd, comp: &OsStr| -> io::Result<OwnedFd> {
            let flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW;
            match open_beneath(dir.as_raw_fd(), Path::new(comp), flags, 0) {
                Err(e) if mkdirs && e.kind() == io::ErrorKind::NotFound => {
                    let c = cstr(comp)?;
                    // SAFETY: `c` is NUL-terminated and outlives the call.
                    if unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), 0o777) } < 0 {
                        let e = io::Error::last_os_error();
                        if e.kind() != io::ErrorKind::AlreadyExists {
                            return Err(e);
                        }
                    }
                    open_beneath(dir.as_raw_fd(), Path::new(comp), flags, 0)
                }
                r => r,
            }
        };
        let walk = || -> io::Result<Option<(OwnedFd, OsString)>> {
            match &self.root {
                None => {
                    let dir = self.real.parent().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
                    if mkdirs {
                        std::fs::create_dir_all(dir)?;
                    }
                    Ok(Some((open_dir(dir)?, name)))
                }
                Some(_) if self.rel.as_os_str().is_empty() => Ok(None),
                Some(root) => {
                    let mut dir = open_dir(root)?;
                    let comps: Vec<_> = self.rel.components().collect();
                    for c in &comps[..comps.len() - 1] {
                        dir = step(&dir, c.as_os_str())?;
                    }
                    Ok(Some((dir, name)))
                }
            }
        };
        walk().map_err(|e| self.fail(e))
    }
}

fn file(fd: OwnedFd) -> File {
    File::from(fd)
}

/// Open `path` for reading: a file, or a directory (for its metadata).
pub fn open_read(policy: &Policy, path: &Path) -> Result<File, String> {
    locate(policy, path, false)?.open(libc::O_RDONLY, 0).map(file)
}

/// The bytes of a file.
pub fn read_file(policy: &Policy, path: &Path) -> Result<Vec<u8>, String> {
    let mut f = open_read(policy, path)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(bytes)
}

/// Metadata of what `path` really is, without opening it for reading.
pub fn metadata(policy: &Policy, path: &Path) -> Result<std::fs::Metadata, String> {
    let at = locate(policy, path, false)?;
    at.open(libc::O_PATH, 0).map(file).and_then(|f| f.metadata().map_err(|e| at.fail(e)))
}

/// Whether anything is at `path`, as far as the policy lets us see.
pub fn exists(policy: &Policy, path: &Path) -> bool {
    metadata(policy, path).is_ok()
}

/// The names in a directory, a subdirectory (a link that stays inside the
/// root counts) with a trailing `/`. Unsorted.
pub fn list_dir(policy: &Policy, path: &Path) -> Result<Vec<String>, String> {
    let at = locate(policy, path, false)?;
    let fd = at.open(libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    // SAFETY: fdopendir takes over the descriptor; closedir below releases it.
    let dir = unsafe { libc::fdopendir(fd.into_raw_fd()) };
    if dir.is_null() {
        return Err(at.fail(io::Error::last_os_error()));
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: `dir` is a live DIR* until closedir; the entry is read before the next call.
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            break;
        }
        // SAFETY: readdir returned a valid entry whose d_name is NUL-terminated.
        let (name, kind) =
            unsafe { (std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()).to_bytes().to_vec(), (*ent).d_type) };
        if name == b"." || name == b".." {
            continue;
        }
        let name = OsString::from_vec(name);
        let is_dir = match kind {
            libc::DT_DIR => true,
            libc::DT_REG => false,
            // A link or unknown type: a directory only if it opens as one beneath this one.
            _ => {
                // SAFETY: `dir` is still live.
                let dfd = unsafe { libc::dirfd(dir) };
                open_beneath(dfd, Path::new(&name), libc::O_PATH | libc::O_DIRECTORY, 0).is_ok()
            }
        };
        let n = name.to_string_lossy();
        names.push(if is_dir { format!("{n}/") } else { n.into_owned() });
    }
    // SAFETY: closes the DIR* and its descriptor, once.
    unsafe { libc::closedir(dir) };
    Ok(names)
}

/// Create or truncate a file (and its missing parent directories) and write `bytes`.
pub fn write_file(policy: &Policy, path: &Path, bytes: &[u8]) -> Result<(), String> {
    let at = locate(policy, path, true)?;
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW;
    let fd = match at.parent(true)? {
        Some((dir, name)) => open_beneath(dir.as_raw_fd(), Path::new(&name), flags, 0o666).map_err(|e| at.fail(e))?,
        None => at.open(flags, 0o666)?,
    };
    file(fd).write_all(bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Replace a file's content atomically: a temp file in the same directory,
/// given `mode`, then `renameat` within that directory fd.
pub fn replace_file(policy: &Policy, path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    let at = locate(policy, path, true)?;
    let Some((dir, name)) = at.parent(false)? else {
        // A single approved file has no sibling to stage in; write it in place.
        let f = file(at.open(libc::O_WRONLY | libc::O_TRUNC, 0)?);
        return f
            .set_permissions(std::fs::Permissions::from_mode(mode))
            .and_then(|()| (&f).write_all(bytes))
            .map_err(|e| at.fail(e));
    };
    let mut tmp = OsString::from(".");
    tmp.push(&name);
    tmp.push(format!(".hashline-{}.tmp", std::process::id()));
    let staged = || -> io::Result<()> {
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW;
        let mut f = file(open_beneath(dir.as_raw_fd(), Path::new(&tmp), flags, 0o600)?);
        f.write_all(bytes)?;
        f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        let (from, to) = (cstr(&tmp)?, cstr(&name)?);
        // SAFETY: both names are NUL-terminated and outlive the call.
        if unsafe { libc::renameat(dir.as_raw_fd(), from.as_ptr(), dir.as_raw_fd(), to.as_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    };
    staged().map_err(|e| {
        if let Ok(c) = cstr(&tmp) {
            // SAFETY: `c` is NUL-terminated and outlives the call.
            unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), 0) };
        }
        at.fail(e)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("construct-open-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("work/d")).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::fs::write(root.join("outside/secret.txt"), "secret").unwrap();
        let root = root.canonicalize().unwrap();
        (root.join("work"), root.join("outside"))
    }

    /// The race itself, without the race: `rel` is what a check made before a
    /// swap would have produced (`d` was a directory then). The swap is done;
    /// the open must be refused by the kernel, not by a second look at names.
    #[test]
    fn a_directory_swapped_for_a_link_after_the_check_is_refused() {
        let (work, outside) = tree("swap");
        std::fs::write(work.join("d/ok.txt"), "ok").unwrap();
        let policy = Policy { read: vec![work.clone()], write: vec![work.clone()], ..Policy::default() };
        let at = locate(&policy, &work.join("d/ok.txt"), false).unwrap();
        assert!(at.open(libc::O_RDONLY, 0).is_ok());
        std::fs::remove_dir_all(work.join("d")).unwrap();
        std::os::unix::fs::symlink(&outside, work.join("d")).unwrap();
        std::fs::write(outside.join("ok.txt"), "STOLEN").unwrap();
        let err = at.open(libc::O_RDONLY, 0).unwrap_err();
        assert!(err.contains("blocked by the sandbox"), "{err}");
    }

    #[test]
    fn a_file_swapped_for_a_link_after_the_check_is_refused() {
        let (work, outside) = tree("swapfile");
        std::fs::write(work.join("f"), "ok").unwrap();
        let policy = Policy { read: vec![work.clone()], write: vec![work.clone()], ..Policy::default() };
        let at = locate(&policy, &work.join("f"), true).unwrap();
        std::fs::remove_file(work.join("f")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), work.join("f")).unwrap();
        assert!(at.open(libc::O_WRONLY | libc::O_TRUNC, 0).unwrap_err().contains("blocked by the sandbox"));
        assert_eq!(std::fs::read_to_string(outside.join("secret.txt")).unwrap(), "secret");
    }
}
