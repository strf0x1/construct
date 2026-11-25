//! Changing the registry while no hub runs (#361): `construct hub invite`
//! and `revoke` work before a hub ever started.
//!
//! The design choice keeps ADR 0069's one-writer rule: the command takes the
//! same instance lock a hub takes, so it is the hub for a moment; if a hub
//! starts first, the lock refuses and the command should ask that hub
//! instead. Approving and denying need the live link, so they never run here.

use crate::files::Files;
use crate::invite::{self, InviteReply};
use crate::lock;
use crate::registry::Registry;
use anyhow::Result;
use std::time::{SystemTime, UNIX_EPOCH};

/// Load, change and save the registry under the instance lock.
pub fn with_registry<T>(files: &Files, f: impl FnOnce(&mut Registry) -> Result<T>) -> Result<T> {
    let _instance = lock::acquire(&files.lock())?;
    let mut registry = Registry::load(&files.registry())?;
    let out = f(&mut registry)?;
    registry.save(&files.registry())?;
    Ok(out)
}

/// Open an invite for `id`; the command names `url`.
pub fn invite(files: &Files, id: &str, ttl_secs: u64, url: String) -> Result<InviteReply> {
    let key = files.keypair()?.public().to_hex();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (token, inv) = invite::mint(ttl_secs, now)?;
    let expires = inv.expires;
    with_registry(files, |r| r.invite(id, inv))?;
    Ok(invite::reply(id, token, expires, url, key))
}

/// Revoke `id`; its key stays on the row so it is refused before message 2.
pub fn revoke(files: &Files, id: &str) -> Result<()> {
    with_registry(files, |r| r.revoke(id, None))
}
