//! A stamped binary (#363, ADR 0073) run with no arguments at all
//! (`./construct-oldpc`) is a peer dialing its hub; any argument means the
//! usual commands. The stamp itself was already checked by `Config::load`.

use super::Cmd;
use anyhow::Result;
use construct_core::config::stamp;

/// The command to run: the parsed one, or `peer` for a bare stamped binary.
pub(super) fn command(parsed: Option<Cmd>) -> Result<Option<Cmd>> {
    if parsed.is_some() || std::env::args_os().len() != 1 || stamp::current()?.is_none() {
        return Ok(parsed);
    }
    peer()
}

#[cfg(feature = "peer")]
fn peer() -> Result<Option<Cmd>> {
    Ok(Some(Cmd::Peer(super::peer::PeerArgs::default())))
}

#[cfg(not(feature = "peer"))]
fn peer() -> Result<Option<Cmd>> {
    anyhow::bail!("this binary is stamped as a peer but was built without the `peer` feature")
}
