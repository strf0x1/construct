//! The boundary around what the model can touch.
//!
//! Two layers, one policy:
//! - shell commands run under Linux Landlock. construct re-executes itself as
//!   `construct __sandbox <policy> -- <shell> -c <cmd>`; that small process is
//!   single-threaded, restricts itself, then forks the shell and supervises it
//!   (`supervise.rs`), so everything the command starts inherits the
//!   restriction and nothing it starts outlives the call. Landlock also blocks
//!   ptrace-style access to processes outside the sandbox, so
//!   /proc/<construct>/environ is out of reach.
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

use crate::agents::AgentDef;
use crate::config::{McpServer, Paths, SandboxConfig};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub mod open;
pub mod report;
mod stop;
mod supervise;
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
    /// TCP ports still reachable when `no_network` (a child's model endpoint).
    #[serde(default)]
    pub connect_ports: Vec<u16>,
    /// Leave the program's stderr where the helper's goes instead of merging
    /// it into stdout: an MCP server's stdout is its protocol.
    #[serde(default)]
    pub own_stderr: bool,
    /// Lock only network and signals, not the filesystem: the process is a
    /// delegated child, whose tools are confined per call instead (ADR 0017).
    #[serde(default)]
    pub net_only: bool,
}

/// Read-only, executable everywhere a shell needs to look.
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
/// Writable devices the shell can't work without. Not all of /dev: /dev/shm
/// would be a shared scratch space outside the workspace.
const SYSTEM_RW: &[&str] =
    &["/dev/null", "/dev/zero", "/dev/full", "/dev/random", "/dev/urandom", "/dev/tty", "/dev/pts", "/dev/ptmx"];
/// Toolchains and caches under $HOME. Read-only unless listed in HOME_RW.
const HOME_RO: &[&str] = &[
    ".local/share/mise",
    ".config/mise",
    ".local/bin",
    ".rustup",
    ".gitconfig",
    ".config/git",
    ".nvm",
    ".pyenv",
    ".bun/bin",
    "go/bin",
];
const HOME_RW: &[&str] = &[".cargo", ".local/state/mise", ".cache", ".npm", ".bun/install", "go/pkg"];

impl Policy {
    pub fn new(cfg: &SandboxConfig, cwd: &Path, paths: &Paths) -> Self {
        let off = cfg.mode == "off";
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let expand = expand_home;
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
        // A skill's own files (scripts, references) are readable; changing
        // them goes through skill_manage (#64).
        system.push(paths.skills_dir());
        let mut sys_rw: Vec<PathBuf> = SYSTEM_RW.iter().map(PathBuf::from).collect();
        if let Some(h) = &home {
            sys_rw.extend(HOME_RW.iter().map(|p| h.join(p)));
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
            connect_ports: vec![],
            own_stderr: false,
            net_only: false,
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

    /// The policy a delegated child runs under: only what its definition
    /// lists, each path inside what `self` (the parent's) allows, plus a
    /// private temp dir that holds the child's whole home. The parent's
    /// working directory is not implied: a child with nothing listed cannot
    /// read it. The child may run the construct binary and nothing more of
    /// the parent's tree.
    pub fn for_agent(&self, def: &AgentDef, paths: &Paths, tag: &str, endpoint_port: u16) -> Result<Self, String> {
        let (read, write) = (AgentDef::expanded(&def.allow_read), AgentDef::expanded(&def.allow_write));
        for p in &read {
            self.check_read(p)?;
        }
        for p in &write {
            self.check_write(p)?;
        }
        let tmp = paths.home.join("tmp").join(format!("{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&tmp).map_err(|e| format!("creating {}: {e}", tmp.display()))?;
        let canon = |v: Vec<PathBuf>| v.into_iter().filter_map(|p| p.canonicalize().ok()).collect::<Vec<_>>();
        let mut system = self.system.clone();
        system.extend(std::env::current_exe().and_then(|e| e.canonicalize()));
        let mut write = canon(write);
        write.extend(tmp.canonicalize());
        // Devices a program can't start without, as for any tool.
        write.extend(self.write.iter().filter(|p| p.starts_with("/dev")).cloned());
        Ok(Self {
            off: self.off,
            read: canon(read),
            write,
            system,
            tmp,
            parent: std::process::id(),
            no_network: !def.network,
            connect_ports: vec![endpoint_port],
            own_stderr: true,
            net_only: false,
        })
    }

    /// This policy as the delegated child's own process is started under it:
    /// the filesystem is left to its per-call confinement (ADR 0017), so an
    /// approval can widen what its next tool call may reach.
    pub fn net_only(&self) -> Self {
        Self { net_only: true, ..self.clone() }
    }

    fn under(roots: &[PathBuf], p: &Path) -> bool {
        roots.iter().any(|r| p.starts_with(r))
    }

    /// The real location of `p`; for a path that doesn't exist yet, its
    /// nearest existing ancestor resolved plus the rest.
    pub(crate) fn real(p: &Path) -> PathBuf {
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

    fn denied(&self, p: &Path, what: &str) -> String {
        format!(
            "blocked by the sandbox: {} is outside the workspace ({what}). Work inside {}. If the user wants this path available, they can add it to [sandbox] allow_{what} in construct's config.toml.",
            p.display(),
            self.read.first().map(|p| p.display().to_string()).unwrap_or_default()
        )
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

/// Names that look like credentials. Tools never see these, whatever the mode.
pub fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    const MARKS: &[&str] = &["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH", "COOKIE", "SESSION"];
    MARKS.iter().any(|m| n.contains(m))
        && !matches!(
            n.as_str(),
            "SSH_AUTH_SOCK" | "XDG_SESSION_TYPE" | "XDG_SESSION_ID" | "XDG_SESSION_CLASS" | "XDG_SESSION_DESKTOP"
        )
}

pub fn scrub_env(cmd: &mut tokio::process::Command) {
    for (k, _) in std::env::vars_os() {
        if let Some(k) = k.to_str()
            && (is_secret_name(k) || crate::secrets::KNOWN.iter().any(|(n, _)| *n == k))
        {
            cmd.env_remove(k);
        }
    }
    // An agent with the user's ssh-agent can push anywhere the user can.
    cmd.env_remove("SSH_AUTH_SOCK");
}

/// `construct __sandbox <policy-json> -- <program> [args...]`: restrict this
/// process with Landlock (unless the policy is off), then run the program
/// under supervision and exit with its status.
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
        restrict(&policy).unwrap_or_else(|e| fail(e));
    }
    supervise::run(policy.parent, prog, !policy.own_stderr)
}

/// Apply the Landlock ruleset to this process and everything it will start.
fn restrict(policy: &Policy) -> Result<(), String> {
    use landlock::{
        ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, NetPort, Ruleset, RulesetAttr, RulesetCreatedAttr,
        RulesetStatus, Scope, path_beneath_rules,
    };
    let abi = ABI::V6;
    let ro = AccessFs::from_read(abi);
    let rw = AccessFs::from_all(abi);
    // A grant for a place that doesn't exist (yet) has nothing to restrict,
    // and a rule for it would fail the whole ruleset.
    let live = |v: &[PathBuf]| v.iter().filter(|p| p.exists()).cloned().collect::<Vec<_>>();
    let (system, read, write) = if policy.net_only {
        Default::default()
    } else {
        (live(&policy.system), live(&policy.read), live(&policy.write))
    };
    let base = Ruleset::default().set_compatibility(CompatLevel::BestEffort);
    // A delegated child's process handles no filesystem access at all.
    let status = Ok(base)
        .and_then(|r: Ruleset| if policy.net_only { Ok(r) } else { r.handle_access(rw) })
        // Handling TCP access with no rule allowing a port denies it all.
        .and_then(|r| if policy.no_network { r.handle_access(AccessNet::from_all(abi)) } else { Ok(r) })
        .and_then(|r| r.scope(Scope::Signal))
        .and_then(|r| r.create())
        // A port rule for access the ruleset doesn't handle is an error, so
        // the exceptions exist only when TCP is denied.
        .and_then(|r| {
            let ports = if policy.no_network { policy.connect_ports.as_slice() } else { &[] };
            r.add_rules(ports.iter().map(|p| Ok(NetPort::new(*p, AccessNet::ConnectTcp))))
        })
        .and_then(|r| r.add_rules(path_beneath_rules(&system, ro)))
        .and_then(|r| r.add_rules(path_beneath_rules(&read, ro)))
        .and_then(|r| r.add_rules(path_beneath_rules(&write, rw)))
        .and_then(|r| r.restrict_self())
        .map_err(|e| format!("landlock: {e}"))?;
    if status.ruleset == RulesetStatus::NotEnforced {
        return Err("Landlock isn't available on this kernel, so commands can't be sandboxed. Set [sandbox] mode = \"off\" in config.toml to run without it.".into());
    }
    Ok(())
}
