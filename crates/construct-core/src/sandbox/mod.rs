//! The boundary around what the model can touch.
//!
//! Two layers, one policy:
//! - shell commands run under Landlock on Linux (`landlock.rs`) or a Seatbelt
//!   profile on macOS (`seatbelt.rs`, ADR 0028). construct re-executes itself
//!   as `construct __sandbox <policy> -- <shell> -c <cmd>`; that small process
//!   is single-threaded, restricts itself, then forks the shell and supervises
//!   it (`supervise.rs`, `supervise_macos.rs`), so everything the command
//!   starts inherits the restriction and nothing it starts outlives the call.
//!   On Linux, Landlock also blocks ptrace-style access to processes outside
//!   the sandbox, so /proc/<construct>/environ is out of reach.
//! - the in-process file tools (read, write, edit, grep, glob) check every
//!   path, after resolving symlinks, against the same roots.
//!
//! Tools also get an environment with credentials removed.
//!
//! Default ("workspace"): read and write the working directory, a private
//! temp dir and construct's spill dir; read system paths and toolchains;
//! nothing else under $HOME. "read-only" drops write access to the working
//! directory; "off" removes the boundary but keeps the helper, because
//! process lifetime is hygiene, not part of the filesystem boundary.

use crate::config::{McpServer, Paths, SandboxConfig};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod beneath;
mod denied;
mod env;
#[cfg(target_os = "linux")]
mod landlock;
pub mod open;
pub mod report;
mod roots;
#[cfg(target_os = "macos")]
mod seatbelt;
mod stop;
#[cfg(target_os = "linux")]
mod supervise;
#[cfg(target_os = "macos")]
#[path = "supervise_macos.rs"]
mod supervise;
pub use env::{is_secret_name, scrub_env};
pub use stop::{GRACE, StopOnDrop};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Policy {
    pub off: bool,
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    /// Exec and read only (system paths, toolchains); not offered to file tools'
    /// error messages as places to work.
    pub system: Vec<PathBuf>,
    pub tmp: PathBuf,
    /// construct's pid, so the helper can tell construct dying from the
    /// spawning thread exiting.
    #[serde(default)]
    pub parent: u32,
    /// Deny TCP connect and bind (Landlock ABI 4+; best effort below it).
    #[serde(default)]
    pub no_network: bool,
    /// Leave the program's stderr where the helper's goes instead of merging
    /// it into stdout: an MCP server's stdout is its protocol.
    #[serde(default)]
    pub own_stderr: bool,
}

/// Read-only, executable everywhere a shell needs to look.
#[cfg(target_os = "linux")]
const SYSTEM: &[&str] = &[
    "/dev/fd",
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/etc",
    "/opt",
    "/nix",
    "/run/current-system",
    "/sys",
    "/proc",
];
/// `/opt` covers Homebrew on Apple silicon; `/usr/local` (inside `/usr`) the
/// Intel prefix. `/etc` and `/var` are symlinks into `/private`, resolved
/// when the policy is built.
#[cfg(target_os = "macos")]
const SYSTEM: &[&str] = &[
    "/dev/fd",
    "/usr",
    "/bin",
    "/sbin",
    "/etc",
    "/opt",
    "/System",
    "/Library/Developer",
    "/Library/Frameworks",
    "/Library/Apple",
    "/Library/Preferences/com.apple.networkd.plist",
    "/Library/Preferences/SystemConfiguration",
    "/Applications/Xcode.app",
    "/private/var/db",
    "/private/var/select",
];
/// Writable devices the shell can't work without. Not all of /dev: /dev/shm
/// would be a shared scratch space outside the workspace.
const SYSTEM_RW: &[&str] =
    &["/dev/null", "/dev/zero", "/dev/full", "/dev/random", "/dev/urandom", "/dev/tty", "/dev/pts", "/dev/ptmx"];

impl Policy {
    pub fn new(cfg: &SandboxConfig, cwd: &Path, paths: &Paths) -> Self {
        let off = cfg.mode == "off";
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let expand = expand_home;
        let tmp = paths.home.join("tmp").join(std::process::id().to_string());
        let _ = std::fs::create_dir_all(&tmp);
        // Made now: a root that doesn't exist yet is dropped below, and the
        // first capped output of a fresh home would spill where no tool can read.
        let _ = std::fs::create_dir_all(paths.spill_dir());
        let mut write = vec![tmp.clone(), paths.spill_dir()];
        if cfg.mode != "read-only" {
            write.push(cwd.to_path_buf());
        }
        write.extend(cfg.allow_write.iter().map(|p| expand(p)));
        let mut read = vec![cwd.to_path_buf()];
        read.extend(cfg.allow_read.iter().map(|p| expand(p)));
        let mut system: Vec<PathBuf> = SYSTEM.iter().map(PathBuf::from).collect();
        if let Some(h) = &home {
            system.extend(roots::read(h));
        }
        // A skill's own files (scripts, references) are readable; changing
        // them goes through skill_manage (#64).
        system.push(paths.skills_dir());
        let mut sys_rw: Vec<PathBuf> = SYSTEM_RW.iter().map(PathBuf::from).collect();
        if let Some(h) = &home {
            sys_rw.extend(roots::write(h, !off));
        }
        write.extend(sys_rw);
        let canon = |v: Vec<PathBuf>| v.into_iter().filter_map(|p| p.canonicalize().ok()).collect::<Vec<_>>();
        Self {
            off,
            read: canon(read),
            write: canon(write),
            system: canon(system),
            tmp,
            parent: std::process::id(),
            no_network: false,
            own_stderr: false,
        }
    }

    /// The policy an MCP server runs under: the workspace's, plus the paths
    /// its entry adds, minus the network if it says so.
    pub fn for_mcp(cfg: &SandboxConfig, server: &McpServer, cwd: &Path, paths: &Paths) -> Self {
        let mut cfg = cfg.clone();
        cfg.allow_read.extend(server.allow_read.iter().cloned());
        cfg.allow_write.extend(server.allow_write.iter().cloned());
        let mut p = Self { no_network: !server.network, own_stderr: true, ..Self::new(&cfg, cwd, paths) };
        // The configured program itself may run, wherever it lives; its
        // directory and anything else it needs come from allow_read.
        if let Some(exe) = which(&server.command).and_then(|e| e.canonicalize().ok()) {
            p.system.push(exe);
        }
        p
    }

    fn under(roots: &[PathBuf], p: &Path) -> bool {
        roots.iter().any(|r| p.starts_with(r))
    }

    /// The real location of `p`; for a path that doesn't exist yet, its
    /// nearest existing ancestor resolved plus the rest.
    pub fn real(p: &Path) -> PathBuf {
        if let Ok(c) = p.canonicalize() {
            return c;
        }
        let mut rest = Vec::new();
        let mut cur = p;
        while let Some(parent) = cur.parent() {
            rest.push(cur.file_name().map(PathBuf::from).unwrap_or_default());
            if let Ok(c) = parent.canonicalize() {
                return rest.iter().rev().fold(c, |acc, r| acc.join(r));
            }
            cur = parent;
        }
        p.to_path_buf()
    }

    pub fn check_read(&self, p: &Path) -> Result<(), String> {
        if self.off {
            return Ok(());
        }
        let r = Self::real(p);
        if Self::under(&self.read, &r) || Self::under(&self.write, &r) || Self::under(&self.system, &r) {
            return Ok(());
        }
        Err(self.denied(&r, "read"))
    }

    pub fn check_write(&self, p: &Path) -> Result<(), String> {
        if self.off {
            return Ok(());
        }
        let r = Self::real(p);
        if Self::under(&self.write, &r) {
            return Ok(());
        }
        Err(self.denied(&r, "write"))
    }

    /// This policy with `read` and `write` roots added: what a grant the user
    /// approved looks like to the check and to the shell helper.
    pub fn widened(&self, read: &[PathBuf], write: &[PathBuf]) -> Self {
        let mut p = self.clone();
        p.read.extend(read.iter().cloned());
        p.write.extend(write.iter().cloned());
        p
    }

    /// Build the command that runs `shell -c script` inside the sandbox.
    pub fn command(&self, shell: &str, script: &str) -> tokio::process::Command {
        self.spawn(shell, ["-c", script])
    }

    /// Build the command that runs `prog args...` inside the sandbox, with a
    /// credential-free environment and the private TMPDIR.
    pub fn spawn<I, S>(&self, prog: &str, args: I) -> tokio::process::Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("construct"));
        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("__sandbox").arg(serde_json::to_string(self).unwrap_or_default()).arg("--").arg(prog);
        cmd.args(args);
        scrub_env(&mut cmd);
        cmd.env("TMPDIR", &self.tmp);
        cmd
    }
}

/// `p` with a leading `~/` replaced by $HOME.
pub fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(h)) => PathBuf::from(h).join(rest),
        _ => PathBuf::from(p),
    }
}

/// `prog` as the shell would find it: a path as given, a bare name on PATH.
fn which(prog: &str) -> Option<PathBuf> {
    if prog.contains('/') {
        return Some(PathBuf::from(prog));
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(prog)).find(|p| p.is_file())
}

/// `construct __sandbox <policy-json> -- <program> [args...]`: restrict this
/// process with the platform's sandbox (unless the policy is off), then run
/// the program under supervision and exit with its status.
pub fn exec_child(args: &[String]) -> ! {
    let fail = |msg: String| -> ! {
        eprintln!("construct sandbox: {msg}");
        std::process::exit(126)
    };
    let policy: Policy =
        args.first().and_then(|j| serde_json::from_str(j).ok()).unwrap_or_else(|| fail("bad policy".into()));
    let Some(sep) = args.iter().position(|a| a == "--") else { fail("missing --".into()) };
    let prog = &args[sep + 1..];
    if prog.is_empty() {
        fail("nothing to run".into());
    }
    if !policy.off {
        #[cfg(target_os = "linux")]
        landlock::restrict(&policy).unwrap_or_else(|e| fail(e));
        #[cfg(target_os = "macos")]
        seatbelt::restrict(&policy).unwrap_or_else(|e| fail(e));
    }
    supervise::run(policy.parent, prog, !policy.own_stderr)
}
