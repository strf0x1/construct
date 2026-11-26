//! `construct peer --install` and `--uninstall` (#367, ADR 0076): place the
//! binary outside the peer's workspace, write the systemd unit
//! (`construct_peer::service`) and an optional `0600` env file, and drive
//! `systemctl`. The design choice: install only a peer that has joined
//! (`peer.key` exists), so the unit never needs a join token, and refuse a
//! binary directory the peer's own tools could write to.

use super::PeerArgs;
use anyhow::{Context, Result, bail};
use construct_core::config::{Config, Llm, Paths};
use construct_peer::{keyfile, service};
use std::path::{Path, PathBuf};

/// What the installed unit dials, as `run` resolved it.
pub(super) struct Target<'a> {
    pub(super) id: &'a str,
    pub(super) url: &'a str,
    pub(super) hub_key: &'a str,
    pub(super) cwd: &'a Path,
}

/// `--install`: place the binary, write the unit (and the env file), enable
/// and start it.
pub(super) fn install(cfg: &Config, paths: &Paths, t: &Target<'_>, args: &PeerArgs) -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("--install writes a systemd unit, which needs Linux");
    }
    let key = paths.home.join("peer.key");
    if keyfile::load(&key).context("peer key")?.is_none() {
        bail!(
            "{} has not joined yet (no {}): run it once with the join token until it says it joined, then --install",
            t.id,
            key.display()
        );
    }
    let layout = service::Layout::new(args.system, |k| std::env::var_os(k))?;
    if layout.bin_dir.starts_with(t.cwd) {
        bail!(
            "the peer works in {} and its tools may write there, so a binary in {} is not safe from them: \
             pass a narrower --cwd, or --system",
            t.cwd.display(),
            layout.bin_dir.display()
        );
    }
    let unit_path = layout.unit_path(t.id)?;
    let unit_name = service::unit_name(t.id)?;
    let env_file = local_keys(cfg, paths, &args.env)?;
    let binary = service::place_binary(&std::env::current_exe()?, &layout.bin_dir)
        .with_context(|| format!("place the binary in {}", layout.bin_dir.display()))?;
    let run_args: Vec<String> = ["peer", "--require-key", "--url", t.url, "--hub-key", t.hub_key, "--id", t.id]
        .into_iter()
        .map(String::from)
        .chain(["--cwd".to_string(), t.cwd.display().to_string()])
        .collect();
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    let unit = service::render(&service::Unit {
        id: t.id,
        url: t.url,
        system: args.system,
        binary: &binary,
        args: &run_args,
        cwd: t.cwd,
        home: &paths.home,
        user_home: user_home.as_deref(),
        env_file: env_file.as_deref(),
    });
    service::write_file(&unit_path, &unit, 0o644).with_context(|| {
        format!("write {}{}", unit_path.display(), if args.system { " (run with sudo)" } else { "" })
    })?;
    println!("installed {} and {}", binary.display(), unit_path.display());
    if !args.system
        && let Err(e) = service::enable_linger()
    {
        eprintln!(
            "construct: could not turn on lingering ({e}); without it the peer starts at your login, not at boot: \
             `sudo loginctl enable-linger $USER`"
        );
    }
    service::systemctl(args.system, &["daemon-reload"])?;
    service::systemctl(args.system, &["enable", "--now", &unit_name])?;
    let ctl = if args.system { "systemctl" } else { "systemctl --user" };
    let logs = if args.system { "journalctl -u" } else { "journalctl --user -u" };
    println!("{} runs now and after every reboot: `{ctl} status {unit_name}`, `{logs} {unit_name}`", t.id);
    Ok(())
}

/// `--env NAME…`: write the named variables to `$CONSTRUCT_HOME/peer.env`
/// (`0600`); none named, none written.
fn local_keys(cfg: &Config, paths: &Paths, names: &[String]) -> Result<Option<PathBuf>> {
    if names.is_empty() {
        if cfg.llm != Llm::Hub {
            eprintln!(
                "construct: this peer calls models itself (llm is not \"hub\") but no --env NAME was given: \
                 the service gets no API keys"
            );
        }
        return Ok(None);
    }
    let vars = names
        .iter()
        .map(|n| std::env::var(n).map(|v| (n.clone(), v)).with_context(|| format!("--env {n}: not set here")))
        .collect::<Result<Vec<_>>>()?;
    let path = paths.home.join("peer.env");
    service::write_file(&path, &service::env_file(&vars)?, 0o600)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(Some(path))
}

/// `--uninstall`: stop, disable and remove the unit; the binary, key and
/// home stay.
pub(super) fn uninstall(id: &str, system: bool) -> Result<()> {
    let layout = service::Layout::new(system, |k| std::env::var_os(k))?;
    let (unit_path, unit_name) = (layout.unit_path(id)?, service::unit_name(id)?);
    if !unit_path.exists() {
        bail!("{} is not installed", unit_path.display());
    }
    if let Err(e) = service::systemctl(system, &["disable", "--now", &unit_name]) {
        eprintln!("construct: {e}");
    }
    std::fs::remove_file(&unit_path).with_context(|| format!("remove {}", unit_path.display()))?;
    service::systemctl(system, &["daemon-reload"])?;
    println!("stopped and removed {}; the binary, peer.key and the home stay", unit_path.display());
    Ok(())
}
