//! The boundary around what the model can touch.
//!
//! Two layers, one policy:
//! - shell commands run under Linux Landlock. construct re-executes itself as
//!   `construct __sandbox <policy> -- <shell> -c <cmd>`; that small process is
//!   single-threaded, restricts itself, then execs the shell, so everything
//!   the command starts inherits the restriction. Landlock also blocks
//!   ptrace-style access to processes outside the sandbox, so
//!   /proc/<construct>/environ is out of reach.
//! - the in-process file tools (read, write, edit, grep, glob) check every
//!   path, after resolving symlinks, against the same roots.
//! Tools also get an environment with credentials removed.
//!
//! Default ("workspace"): read and write the working directory, a private
//! temp dir and construct's spill dir; read system paths and toolchains;
//! nothing else under $HOME. "read-only" drops write access to the working
//! directory; "off" removes the boundary.

use crate::config::{Paths, SandboxConfig};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Policy {
    pub off: bool,
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    /// Exec and read only (system paths, toolchains); not offered to file tools'
    /// error messages as places to work.
    pub system: Vec<PathBuf>,
    pub tmp: PathBuf,
}

/// Read-only, executable everywhere a shell needs to look.
const SYSTEM: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/opt", "/nix", "/run/current-system", "/sys", "/proc"];
/// Writable devices and scratch the shell can't work without.
const SYSTEM_RW: &[&str] = &["/dev"];
/// Toolchains and caches under $HOME. Read-only unless listed in HOME_RW.
const HOME_RO: &[&str] = &[".local/share/mise", ".config/mise", ".local/bin", ".rustup", ".gitconfig", ".config/git", ".nvm", ".pyenv", ".bun/bin", "go/bin"];
const HOME_RW: &[&str] = &[".cargo", ".local/state/mise", ".cache", ".npm", ".bun/install", "go/pkg"];

impl Policy {
    pub fn new(cfg: &SandboxConfig, cwd: &Path, paths: &Paths) -> Self {
        let off = cfg.mode == "off";
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let expand = |p: &str| match (p.strip_prefix("~/"), &home) {
            (Some(rest), Some(h)) => h.join(rest),
            _ => PathBuf::from(p),
        };
        let tmp = paths.home.join("tmp").join(std::process::id().to_string());
        let _ = std::fs::create_dir_all(&tmp);
        let mut write = vec![tmp.clone(), paths.spill_dir()];
        if cfg.mode != "read-only" {
            write.push(cwd.to_path_buf());
        }
        write.extend(cfg.allow_write.iter().map(|p| expand(p)));
        let mut read = vec![cwd.to_path_buf()];
        read.extend(cfg.allow_read.iter().map(|p| expand(p)));
        let mut system: Vec<PathBuf> = SYSTEM.iter().map(PathBuf::from).collect();
        if let Some(h) = &home {
            system.extend(HOME_RO.iter().map(|p| h.join(p)));
        }
        let mut sys_rw: Vec<PathBuf> = SYSTEM_RW.iter().map(PathBuf::from).collect();
        if let Some(h) = &home {
            sys_rw.extend(HOME_RW.iter().map(|p| h.join(p)));
        }
        write.extend(sys_rw);
        let canon = |v: Vec<PathBuf>| v.into_iter().filter_map(|p| p.canonicalize().ok()).collect::<Vec<_>>();
        Self { off, read: canon(read), write: canon(write), system: canon(system), tmp }
    }

    fn under(roots: &[PathBuf], p: &Path) -> bool {
        roots.iter().any(|r| p.starts_with(r))
    }

    /// The real location of `p`; for a path that doesn't exist yet, its
    /// nearest existing ancestor resolved plus the rest.
    fn real(p: &Path) -> PathBuf {
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

    fn denied(&self, p: &Path, what: &str) -> String {
        format!(
            "blocked by the sandbox: {} is outside the workspace ({what}). Work inside {}. If the user wants this path available, they can add it to [sandbox] allow_{what} in construct's config.toml.",
            p.display(),
            self.read.first().map(|p| p.display().to_string()).unwrap_or_default()
        )
    }

    /// Build the command that runs `shell -c script` inside the sandbox.
    pub fn command(&self, shell: &str, script: &str) -> tokio::process::Command {
        let mut cmd = if self.off {
            tokio::process::Command::new(shell)
        } else {
            let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("construct"));
            let mut c = tokio::process::Command::new(exe);
            c.arg("__sandbox").arg(serde_json::to_string(self).unwrap_or_default()).arg("--").arg(shell);
            c
        };
        cmd.arg("-c").arg(script);
        scrub_env(&mut cmd);
        cmd.env("TMPDIR", &self.tmp);
        cmd
    }
}

/// Names that look like credentials. Tools never see these, whatever the mode.
pub fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    const MARKS: &[&str] = &["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH", "COOKIE", "SESSION"];
    MARKS.iter().any(|m| n.contains(m)) && !matches!(n.as_str(), "SSH_AUTH_SOCK" | "XDG_SESSION_TYPE" | "XDG_SESSION_ID" | "XDG_SESSION_CLASS" | "XDG_SESSION_DESKTOP")
}

pub fn scrub_env(cmd: &mut tokio::process::Command) {
    for (k, _) in std::env::vars_os() {
        if let Some(k) = k.to_str() {
            if is_secret_name(k) || crate::secrets::KNOWN.iter().any(|(n, _)| *n == k) {
                cmd.env_remove(k);
            }
        }
    }
    // An agent with the user's ssh-agent can push anywhere the user can.
    cmd.env_remove("SSH_AUTH_SOCK");
}

/// `construct __sandbox <policy-json> -- <program> [args...]`: restrict this
/// process with Landlock, then exec the program. Never returns on success.
pub fn exec_child(args: &[String]) -> ! {
    use landlock::{ABI, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, Scope, path_beneath_rules};
    use std::os::unix::process::CommandExt;
    let fail = |msg: String| -> ! {
        eprintln!("construct sandbox: {msg}");
        std::process::exit(126)
    };
    let policy: Policy = args.first().and_then(|j| serde_json::from_str(j).ok()).unwrap_or_else(|| fail("bad policy".into()));
    let Some(sep) = args.iter().position(|a| a == "--") else { fail("missing --".into()) };
    let prog = &args[sep + 1..];
    if prog.is_empty() {
        fail("nothing to run".into());
    }
    let abi = ABI::V6;
    let ro = AccessFs::from_read(abi);
    let rw = AccessFs::from_all(abi);
    let status = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(rw)
        .and_then(|r| r.scope(Scope::Signal))
        .and_then(|r| r.create())
        .and_then(|r| r.add_rules(path_beneath_rules(&policy.system, ro)))
        .and_then(|r| r.add_rules(path_beneath_rules(&policy.read, ro)))
        .and_then(|r| r.add_rules(path_beneath_rules(&policy.write, rw)))
        .and_then(|r| r.restrict_self())
        .unwrap_or_else(|e| fail(format!("landlock: {e}")));
    if status.ruleset == RulesetStatus::NotEnforced {
        fail("Landlock isn't available on this kernel, so commands can't be sandboxed. Set [sandbox] mode = \"off\" in config.toml to run without it.".into());
    }
    let err = std::process::Command::new(&prog[0]).args(&prog[1..]).exec();
    fail(format!("exec {}: {err}", prog[0]))
}
