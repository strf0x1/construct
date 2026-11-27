//! The account a system install runs the peer as (`--install --system --user
//! <name>`, #389): look it up, and hand the peer's home over to it so the
//! peer can read its key and write its journal, with the home `0700` and the
//! key and env file `0600` (ADR 0021).
//!
//! The design choice: hand over only what root or that account already owns.
//! A home that holds a third user's files is refused, never taken from them,
//! and a home the account could not even reach (a closed parent such as
//! `/root`) is refused before anything changes.

use std::ffi::{CStr, CString};
use std::io::{Error, ErrorKind, Result};
use std::os::unix::fs::{MetadataExt, PermissionsExt, lchown};
use std::path::{Path, PathBuf};

/// A local account, as the password database has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    /// The primary group's name, or its number when it has none.
    pub group: String,
    pub home: PathBuf,
}

/// Who this process acts as.
pub fn effective_uid() -> u32 {
    // SAFETY: geteuid cannot fail.
    unsafe { libc::geteuid() }
}

/// Look `name` up; an unknown account is `NotFound`.
pub fn lookup(name: &str) -> Result<Account> {
    let cname = CString::new(name).map_err(|_| Error::new(ErrorKind::InvalidInput, "a user name holds a NUL"))?;
    let mut buf = vec![0u8; 4096];
    loop {
        // SAFETY: an all-zero `passwd` is a valid out-parameter.
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call, `buf` for its length.
        let rc = unsafe { libc::getpwnam_r(cname.as_ptr(), &mut pw, buf.as_mut_ptr().cast(), buf.len(), &mut found) };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 {
            return Err(Error::from_raw_os_error(rc));
        }
        if found.is_null() {
            return Err(Error::new(ErrorKind::NotFound, format!("no user named {name:?}")));
        }
        // SAFETY: on success `pw_dir` points into `buf`, NUL-terminated.
        let home = unsafe { CStr::from_ptr(pw.pw_dir) }.to_string_lossy().into_owned();
        return Ok(Account {
            name: name.to_string(),
            uid: pw.pw_uid,
            gid: pw.pw_gid,
            group: group_name(pw.pw_gid).unwrap_or_else(|| pw.pw_gid.to_string()),
            home: PathBuf::from(home),
        });
    }
}

fn group_name(gid: u32) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    loop {
        // SAFETY: an all-zero `group` is a valid out-parameter.
        let mut gr: libc::group = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::group = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call, `buf` for its length.
        let rc = unsafe { libc::getgrgid_r(gid, &mut gr, buf.as_mut_ptr().cast(), buf.len(), &mut found) };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() {
            return None;
        }
        // SAFETY: on success `gr_name` points into `buf`, NUL-terminated.
        return Some(unsafe { CStr::from_ptr(gr.gr_name) }.to_string_lossy().into_owned());
    }
}

/// The directories above `path` that `acct` cannot pass through: it needs
/// search (`x`) permission on every one to reach its home.
pub fn closed_parents(path: &Path, acct: &Account) -> Vec<PathBuf> {
    path.ancestors()
        .skip(1)
        .filter_map(|dir| {
            let m = std::fs::metadata(dir).ok()?;
            let mode = m.mode();
            let open = if m.uid() == acct.uid {
                mode & 0o100 != 0
            } else if m.gid() == acct.gid {
                mode & 0o010 != 0
            } else {
                mode & 0o001 != 0
            };
            (!open).then(|| dir.to_path_buf())
        })
        .collect()
}

/// Give `home` and everything in it to `acct` (symlinks themselves, never
/// their targets), then make it `0700` and `secret` files in it `0600`.
/// Refused, with nothing changed, when a file there belongs to someone other
/// than root or `acct`.
pub fn hand_over(home: &Path, acct: &Account, secrets: &[&str]) -> Result<()> {
    let mut all = Vec::new();
    walk(home, &mut all)?;
    if let Some((p, uid)) = all.iter().find(|(_, uid)| *uid != 0 && *uid != acct.uid) {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!("{} belongs to uid {uid}, not root or {}: not handing it over", p.display(), acct.name),
        ));
    }
    for (p, _) in &all {
        lchown(p, Some(acct.uid), Some(acct.gid))
            .map_err(|e| Error::new(e.kind(), format!("chown {}: {e}", p.display())))?;
    }
    std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))?;
    for name in secrets {
        let p = home.join(name);
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

fn walk(path: &Path, out: &mut Vec<(PathBuf, u32)>) -> Result<()> {
    let m = std::fs::symlink_metadata(path)?;
    out.push((path.to_path_buf(), m.uid()));
    if m.is_dir() {
        for entry in std::fs::read_dir(path)? {
            walk(&entry?.path(), out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Account {
        // SAFETY: getuid/getgid cannot fail.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        Account { name: "me".into(), uid, gid, group: gid.to_string(), home: "/".into() }
    }

    #[test]
    fn root_is_found_and_strangers_are_not() {
        let root = lookup("root").unwrap();
        assert_eq!((root.uid, root.gid), (0, 0));
        assert!(!root.home.as_os_str().is_empty());
        assert!(!root.group.is_empty());
        let e = lookup("no-such-user-construct-389").unwrap_err();
        assert_eq!(e.kind(), ErrorKind::NotFound);
        assert!(lookup("a\0b").is_err());
    }

    #[test]
    fn handing_over_your_own_home_tightens_it() {
        let d = std::env::temp_dir().join(format!("construct-account-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("home/projects")).unwrap();
        std::fs::write(d.join("home/peer.key"), b"k").unwrap();
        std::fs::set_permissions(d.join("home"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(d.join("home/peer.key"), std::fs::Permissions::from_mode(0o644)).unwrap();
        hand_over(&d.join("home"), &me(), &["peer.key", "peer.env"]).unwrap();
        let mode = |p: &str| std::fs::metadata(d.join(p)).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode("home"), mode("home/peer.key")), (0o700, 0o600));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_closed_parent_is_named() {
        let d = std::env::temp_dir().join(format!("construct-account-closed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("shut/home")).unwrap();
        std::fs::set_permissions(d.join("shut"), std::fs::Permissions::from_mode(0o700)).unwrap();
        let stranger = Account { uid: 4_000_000, gid: 4_000_000, ..me() };
        assert!(closed_parents(&d.join("shut/home"), &stranger).contains(&d.join("shut")));
        assert!(!closed_parents(&d.join("shut/home"), &me()).contains(&d.join("shut")));
        let _ = std::fs::remove_dir_all(&d);
    }
}
