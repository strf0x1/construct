//! `construct peer --install` and `--uninstall` (#367, #389, ADR 0076, 0077):
//! place the binary outside the peer's workspace, write the service (a
//! systemd unit on Linux, `construct_peer::service`; a launchd job on macOS,
//! `construct_peer::launchd`) and an optional `0600` env file, and drive
//! `systemctl` or `launchctl`. `--system --user <name>` runs a system
//! service as that account and hands it the peer's home
//! (`construct_peer::account`).
//!
//! The design choice: install only a peer that has joined (`peer.key`
//! exists), so the service never needs a join token, and refuse a binary
//! directory the peer's own tools could write to.

use super::PeerArgs;
use anyhow::{Context, Result, bail};
use construct_core::config::{Config, Llm, Paths};
use construct_peer::account::{self, Account};
use construct_peer::{keyfile, launchd, service};
use std::path::{Path, PathBuf};

/// What the installed service dials, as `run` resolved it.
pub(super) struct Target<'a> {
    pub(super) id: &'a str,
    pub(super) url: &'a str,
    pub(super) hub_key: &'a str,
    pub(super) cwd: &'a Path,
}

/// The service manager this build talks to.
enum Manager {
    Systemd(service::Layout),
    Launchd(launchd::Layout),
}

impl Manager {
    fn new(system: bool) -> Result<Self> {
        let var = |k: &str| std::env::var_os(k);
        if cfg!(target_os = "linux") {
            Ok(Self::Systemd(service::Layout::new(system, var)?))
        } else if cfg!(target_os = "macos") {
            Ok(Self::Launchd(launchd::Layout::new(system, var)?))
        } else {
            bail!("--install writes a systemd unit (Linux) or a launchd job (macOS); this system has neither")
        }
    }

    fn bin_dir(&self) -> &Path {
        match self {
            Self::Systemd(l) => &l.bin_dir,
            Self::Launchd(l) => &l.bin_dir,
        }
    }
}

/// Everything both managers need, resolved and checked once.
struct Plan<'a> {
    t: &'a Target<'a>,
    system: bool,
    /// The home holding the key: the account's own with `--user`, unless
    /// `CONSTRUCT_HOME` names one.
    home: PathBuf,
    run_as: Option<Account>,
    binary: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

/// `--install`: place the binary, write the service (and the env file),
/// enable and start it.
pub(super) fn install(cfg: &Config, paths: &Paths, t: &Target<'_>, args: &PeerArgs) -> Result<()> {
    let manager = Manager::new(args.system)?;
    let run_as = args.user.as_deref().map(account::lookup).transpose().context("--user")?;
    let home = match &run_as {
        Some(a) if std::env::var_os("CONSTRUCT_HOME").is_none_or(|h| h.is_empty()) => a.home.join(".construct"),
        _ => paths.home.clone(),
    };
    if let Some(a) = &run_as {
        check_account(a, &home)?;
    }
    let key = home.join("peer.key");
    if keyfile::load(&key).context("peer key")?.is_none() {
        bail!(
            "{} has not joined yet (no {}): run it once with the join token until it says it joined, then --install{}",
            t.id,
            key.display(),
            if run_as.is_some() { " (CONSTRUCT_HOME names another home)" } else { "" }
        );
    }
    let bin_dir = manager.bin_dir();
    if bin_dir.starts_with(t.cwd) {
        bail!(
            "the peer works in {} and its tools may write there, so a binary in {} is not safe from them: \
             pass a narrower --cwd, or --system",
            t.cwd.display(),
            bin_dir.display()
        );
    }
    let env = local_keys(cfg, &args.env)?;
    let binary = service::place_binary(&std::env::current_exe()?, bin_dir)
        .with_context(|| format!("place the binary in {}", bin_dir.display()))?;
    let run_args: Vec<String> = ["peer", "--require-key", "--url", t.url, "--hub-key", t.hub_key, "--id", t.id]
        .into_iter()
        .map(String::from)
        .chain(["--cwd".to_string(), t.cwd.display().to_string()])
        .collect();
    let plan = Plan { t, system: args.system, home, run_as, binary, args: run_args, env };
    // An earlier stop for good is over: this is the new start.
    service::clear_stopped(&plan.home).context("clear the last stop")?;
    match manager {
        Manager::Systemd(layout) => systemd_install(&plan, &layout),
        Manager::Launchd(layout) => launchd_install(&plan, &layout),
    }
}

/// `--user`: an account the installer may hand the home to, and that can
/// reach it.
fn check_account(a: &Account, home: &Path) -> Result<()> {
    let euid = account::effective_uid();
    if euid != 0 && euid != a.uid {
        bail!("--user {} hands the peer's home to that account, which needs root: run with sudo", a.name);
    }
    if let Some(dir) = account::closed_parents(home, a).first() {
        bail!(
            "{} cannot reach {}: {} is closed to it. Join as {} (its home is {}), or set CONSTRUCT_HOME to a \
             home it can reach",
            a.name,
            home.display(),
            dir.display(),
            a.name,
            a.home.display()
        );
    }
    Ok(())
}

/// Write the env file, if any, and give the home to `--user`.
fn prepare_home(
    p: &Plan<'_>,
    env_file: impl Fn(&[(String, String)]) -> std::io::Result<String>,
) -> Result<Option<PathBuf>> {
    let path = (!p.env.is_empty()).then(|| p.home.join("peer.env"));
    if let Some(path) = &path {
        service::write_file(path, &env_file(&p.env)?, 0o600).with_context(|| format!("write {}", path.display()))?;
    }
    if let Some(a) = &p.run_as {
        account::hand_over(&p.home, a, &["peer.key", "peer.env"])
            .with_context(|| format!("hand {} to {}", p.home.display(), a.name))?;
        println!("{} now belongs to {} (0700; peer.key 0600)", p.home.display(), a.name);
    }
    Ok(path)
}

fn sudo_hint(system: bool) -> &'static str {
    if system { " (run with sudo)" } else { "" }
}

fn systemd_install(p: &Plan<'_>, layout: &service::Layout) -> Result<()> {
    let (unit_path, unit_name) = (layout.unit_path(p.t.id)?, service::unit_name(p.t.id)?);
    let env_file = prepare_home(p, service::env_file)?;
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    let unit = service::render(&service::Unit {
        id: p.t.id,
        url: p.t.url,
        system: p.system,
        binary: &p.binary,
        args: &p.args,
        cwd: p.t.cwd,
        home: &p.home,
        user_home: user_home.as_deref(),
        env_file: env_file.as_deref(),
        run_as: p.run_as.as_ref(),
    });
    service::write_file(&unit_path, &unit, 0o644)
        .with_context(|| format!("write {}{}", unit_path.display(), sudo_hint(p.system)))?;
    println!("installed {} and {}", p.binary.display(), unit_path.display());
    if !p.system
        && let Err(e) = service::enable_linger()
    {
        eprintln!(
            "construct: could not turn on lingering ({e}); without it the peer starts at your login, not at boot: \
             `sudo loginctl enable-linger $USER`"
        );
    }
    service::systemctl(p.system, &["daemon-reload"])?;
    service::systemctl(p.system, &["enable", "--now", &unit_name])?;
    let ctl = if p.system { "systemctl" } else { "systemctl --user" };
    let logs = if p.system { "journalctl -u" } else { "journalctl --user -u" };
    println!("{} runs now and after every reboot: `{ctl} status {unit_name}`, `{logs} {unit_name}`", p.t.id);
    Ok(())
}

fn launchd_install(p: &Plan<'_>, layout: &launchd::Layout) -> Result<()> {
    let (plist, label) = (layout.plist_path(p.t.id)?, launchd::label(p.t.id)?);
    let env_file = prepare_home(p, launchd::env_file)?;
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    let job = launchd::render(&launchd::Job {
        label: &label,
        binary: &p.binary,
        args: &p.args,
        cwd: p.t.cwd,
        home: &p.home,
        user_home: user_home.as_deref(),
        env_file: env_file.as_deref(),
        run_as: p.run_as.as_ref(),
    });
    service::write_file(&plist, &job, 0o644)
        .with_context(|| format!("write {}{}", plist.display(), sudo_hint(p.system)))?;
    println!("installed {} and {}", p.binary.display(), plist.display());
    let (domain, target) = (layout.domain(), format!("{}/{label}", layout.domain()));
    // A job already loaded (a reinstall) is replaced, and one disabled is
    // allowed to load again.
    let _ = launchd::launchctl(&["bootout", &target]);
    launchd::launchctl(&["enable", &target])?;
    let log = p.home.join(launchd::LOG);
    if let Err(e) = launchd::launchctl(&["bootstrap", &domain, &plist.display().to_string()]) {
        if p.system {
            return Err(e.into());
        }
        eprintln!("construct: {e}");
        eprintln!(
            "construct: no login session to start it in (over ssh?); it starts at your next login. To run at boot \
             with no one logged in: `sudo {} --install --system --user $USER`",
            p.binary.display()
        );
        return Ok(());
    }
    println!("{} runs now and {}: `launchctl print {target}`, log {}", p.t.id, after(p.system), log.display());
    Ok(())
}

fn after(system: bool) -> &'static str {
    if system {
        "after every reboot"
    } else {
        "whenever you are logged in (for boot with no one logged in: --system --user $USER)"
    }
}

/// `--env NAME…`: the named variables from this shell; none named, none.
fn local_keys(cfg: &Config, names: &[String]) -> Result<Vec<(String, String)>> {
    if names.is_empty() && cfg.llm != Llm::Hub {
        eprintln!(
            "construct: this peer calls models itself (llm is not \"hub\") but no --env NAME was given: \
             the service gets no API keys"
        );
    }
    names
        .iter()
        .map(|n| std::env::var(n).map(|v| (n.clone(), v)).with_context(|| format!("--env {n}: not set here")))
        .collect()
}

/// `--uninstall`: stop, disable and remove the service; the binary, key and
/// home stay.
pub(super) fn uninstall(id: &str, system: bool) -> Result<()> {
    let (path, stopped) = match Manager::new(system)? {
        Manager::Systemd(layout) => {
            let path = installed(layout.unit_path(id)?)?;
            (path, service::systemctl(system, &["disable", "--now", &service::unit_name(id)?]))
        }
        Manager::Launchd(layout) => {
            let path = installed(layout.plist_path(id)?)?;
            (path, launchd::launchctl(&["bootout", &format!("{}/{}", layout.domain(), launchd::label(id)?)]))
        }
    };
    if let Err(e) = stopped {
        eprintln!("construct: {e}");
    }
    std::fs::remove_file(&path).with_context(|| format!("remove {}{}", path.display(), sudo_hint(system)))?;
    if cfg!(target_os = "linux") {
        service::systemctl(system, &["daemon-reload"])?;
    }
    println!("stopped and removed {}; the binary, peer.key and the home stay", path.display());
    Ok(())
}

fn installed(path: PathBuf) -> Result<PathBuf> {
    if !path.exists() {
        bail!("{} is not installed", path.display());
    }
    Ok(path)
}
