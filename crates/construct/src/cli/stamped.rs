//! A stamped binary (#363, ADR 0073) run with no arguments at all
//! (`./construct-oldpc`) is a peer dialing its hub, and so is one whose first
//! argument is a `construct peer` flag (`./construct-oldpc --install`,
//! `--join <token>`, #367); anything else means the usual commands. The
//! stamp itself is checked by `Config::load`.

use super::Cmd;
use anyhow::Result;
use construct_core::config::stamp;
use std::ffi::OsString;

/// The command line to parse: `argv` with `peer` put in front of a stamped
/// binary's peer flags.
pub(super) fn argv() -> Vec<OsString> {
    let mut args: Vec<OsString> = std::env::args_os().collect();
    let stamped = matches!(stamp::current(), Ok(Some(_)));
    if stamped && args.get(1).and_then(|a| a.to_str()).is_some_and(peer_flag) {
        args.insert(1, "peer".into());
    }
    args
}

/// The command to run: the parsed one, or `peer` for a bare stamped binary.
pub(super) fn command(parsed: Option<Cmd>) -> Result<Option<Cmd>> {
    if parsed.is_some() || std::env::args_os().len() != 1 || stamp::current()?.is_none() {
        return Ok(parsed);
    }
    peer()
}

/// `--install`, `--join=…`: a long flag `construct peer` takes.
#[cfg(feature = "peer")]
fn peer_flag(arg: &str) -> bool {
    use clap::CommandFactory;
    let Some(name) = arg.strip_prefix("--").map(|a| a.split('=').next().unwrap_or(a)) else {
        return false;
    };
    super::Cli::command()
        .find_subcommand("peer")
        .is_some_and(|c| c.get_arguments().any(|a| !a.is_global_set() && a.get_long() == Some(name)))
}

#[cfg(not(feature = "peer"))]
fn peer_flag(_: &str) -> bool {
    false
}

#[cfg(feature = "peer")]
fn peer() -> Result<Option<Cmd>> {
    Ok(Some(Cmd::Peer(super::peer::PeerArgs::default())))
}

#[cfg(not(feature = "peer"))]
fn peer() -> Result<Option<Cmd>> {
    anyhow::bail!("this binary is stamped as a peer but was built without the `peer` feature")
}
