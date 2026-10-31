//! The macOS backend: a Seatbelt (SBPL) profile applied to the helper process,
//! which every descendant then inherits (ADR 0028).
//!
//! The profile is deny-by-default and built from the same `Policy` as the
//! Landlock ruleset. It is applied in-process with `sandbox_init_with_parameters`
//! (libSystem; the call Chromium and Firefox use), not by wrapping the shell in
//! `sandbox-exec`, so the helper keeps its restrict-then-supervise shape.

use super::Policy;
use std::ffi::{CStr, CString, c_char, c_int};
use std::path::{Path, PathBuf};

unsafe extern "C" {
    fn sandbox_init_with_parameters(
        profile: *const c_char,
        flags: u64,
        parameters: *const *const c_char,
        errorbuf: *mut *mut c_char,
    ) -> c_int;
    fn sandbox_free_error(errorbuf: *mut c_char);
}

/// Apply the profile for `policy` to this process and everything it will start.
///
/// A sandboxed process cannot apply another profile (`sandbox_init` fails
/// with EPERM; Landlock stacks, Seatbelt does not). A delegated child's own
/// process therefore gets none (`net_only`): its tools are confined per call
/// (ADR 0017), and each shell call is a fresh helper built from the child's
/// policy, network included. What this gives up is the kernel's hold on the
/// child's in-process network use, ADR 0028.
pub(super) fn restrict(policy: &Policy) -> Result<(), String> {
    if policy.net_only {
        return Ok(());
    }
    let profile = CString::new(profile(policy)).map_err(|e| format!("seatbelt: {e}"))?;
    let mut err: *mut c_char = std::ptr::null_mut();
    // SAFETY: `profile` is a NUL-terminated string; no parameters; `err` is
    // either left null or set to a string we free with sandbox_free_error.
    let rc = unsafe { sandbox_init_with_parameters(profile.as_ptr(), 0, std::ptr::null(), &mut err) };
    if rc == 0 {
        return Ok(());
    }
    let msg = if err.is_null() {
        "unknown error".to_string()
    } else {
        // SAFETY: a non-null errorbuf is a NUL-terminated C string.
        let m = unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned();
        // SAFETY: allocated by sandbox_init_with_parameters, freed once.
        unsafe { sandbox_free_error(err) };
        m
    };
    Err(format!("seatbelt: {msg}. Set [sandbox] mode = \"off\" in config.toml to run without it."))
}

/// An SBPL string literal.
fn q(p: &Path) -> Option<String> {
    let s = p.to_str()?;
    Some(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// `(subpath ...)` filters for `paths` that exist, skipping any that can't be
/// written as a string literal.
fn subpaths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .filter(|p| p.exists())
        .filter_map(|p| q(p))
        .map(|s| format!("(subpath {s})"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The profile for `policy`. Kept free of syscalls so it can be tested.
pub(super) fn profile(policy: &Policy) -> String {
    let mut out = String::from(
        "(version 1)\n(deny default)\n\
         (allow process-exec*)\n(allow process-fork)\n\
         (allow signal (target same-sandbox))\n\
         (allow process-info* (target same-sandbox))\n\
         (allow sysctl-read)\n\
         (allow system-info)\n\
         (allow user-preference-read)\n\
         (allow mach-lookup)\n\
         (allow ipc-posix-shm-read-data ipc-posix-shm-read-metadata)\n\
         (allow pseudo-tty)\n",
    );
    let read: Vec<PathBuf> = policy.system.iter().chain(&policy.read).chain(&policy.write).cloned().collect();
    // stat() on the directories leading to a granted root, so path
    // resolution works, without listing or reading anything beside it.
    // `/etc`, `/var` and `/tmp` are symlinks into `/private`; programs lstat
    // them on the way, though the roots above are the resolved paths.
    let mut ancestors: Vec<PathBuf> = ["/", "/etc", "/var", "/tmp"].map(PathBuf::from).into();
    for r in read.iter().filter(|p| p.exists()) {
        ancestors.extend(r.ancestors().skip(1).map(Path::to_path_buf));
    }
    ancestors.sort();
    ancestors.dedup();
    let lits = ancestors.iter().filter_map(|p| q(p)).map(|s| format!("(literal {s})")).collect::<Vec<_>>().join(" ");
    out.push_str(&format!("(allow file-read-metadata {lits})\n"));
    // dyld reads the root directory itself while resolving paths.
    out.push_str("(allow file-read-data (literal \"/\"))\n");
    // libSystem opens this at startup.
    out.push_str("(allow file-read* file-write* file-ioctl (literal \"/dev/dtracehelper\"))\n");
    let r = subpaths(&read);
    if !r.is_empty() {
        // Landlock's read access includes execute; Seatbelt keeps mapping
        // code (dyld, dlopen, a binary built in the workspace) separate.
        out.push_str(&format!("(allow file-read* file-map-executable {r})\n"));
    }
    // xcrun (behind git, cc, make on a Mac without full Xcode) caches in the
    // per-user temp dir whatever TMPDIR says. Only its own file and the temp
    // names it renames from, not the dir: that would be a shared scratch space
    // outside the workspace.
    out.push_str(
        "(allow file-read* file-write* (regex #\"^/private/var/folders/[^/]+/[^/]+/T/xcrun_db(-[^/]*)?$\"))\n",
    );
    let w = subpaths(&policy.write);
    if !w.is_empty() {
        out.push_str(&format!("(allow file-write* file-ioctl {w})\n"));
    }
    if policy.no_network {
        for port in &policy.connect_ports {
            out.push_str(&format!("(allow network-outbound (remote ip \"localhost:{port}\"))\n"));
        }
    } else {
        out.push_str("(allow network*)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            read: vec!["/tmp".into()],
            write: vec!["/private/var".into()],
            system: vec!["/usr".into()],
            ..Default::default()
        }
    }

    #[test]
    fn denies_by_default_and_names_the_roots() {
        let p = profile(&policy());
        assert!(p.contains("(deny default)"));
        assert!(p.contains("(allow file-read* file-map-executable ") && p.contains("(subpath \"/usr\")"));
        assert!(p.contains("(allow file-write* file-ioctl (subpath \"/private/var\"))"), "{p}");
        assert!(p.contains("(allow network*)"));
    }

    #[test]
    fn network_is_closed_to_all_but_the_listed_ports() {
        let p = profile(&Policy { no_network: true, connect_ports: vec![8080], ..policy() });
        assert!(!p.contains("(allow network*)"));
        assert!(p.contains("(allow network-outbound (remote ip \"localhost:8080\"))"));
    }

    #[test]
    fn a_path_with_quotes_stays_inside_its_literal() {
        assert_eq!(q(Path::new("/a\"b\\c")).unwrap(), "\"/a\\\"b\\\\c\"");
    }
}
