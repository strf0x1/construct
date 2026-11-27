//! Expecting a peer (#361): mint its single-use token through the running
//! hub, or straight into the registry when none runs, and print the one-line
//! command that joins it. `construct build --invite` shares the minting.

use super::ask;
use anyhow::{Result, bail};
use construct_hub::control::Request;
use construct_hub::invite::{DEFAULT_TTL_SECS, InviteReply};
use construct_hub::{Files, offline};

/// Expect `id` and mint its token: through the running hub, or straight into
/// the registry when none runs (dialing `url`, else `offline_url`).
/// `construct build --invite` shares it.
pub(in crate::cli) async fn invite(
    files: &Files,
    id: &str,
    ttl: u64,
    url: Option<String>,
    offline_url: String,
) -> Result<InviteReply> {
    match ask(files, &Request::Invite { id: id.to_string(), ttl_secs: ttl, url: url.clone() }).await? {
        Some(resp) => match resp.invite {
            Some(i) => Ok(i),
            None => bail!("hub: {}", resp.error.unwrap_or_else(|| "no invite in the reply".into())),
        },
        None => offline::invite(files, id, ttl, url.unwrap_or(offline_url)),
    }
}

pub(super) fn print_invite(reply: &InviteReply, ttl: u64, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(reply)?);
        return Ok(());
    }
    let ttl = if ttl == DEFAULT_TTL_SECS { "15m".into() } else { construct_hub::status::left(ttl) };
    println!("invite for {}: single use, valid for {ttl}. On the target, run:\n", reply.id);
    println!("  {}\n", reply.command);
    if reply.url.contains("127.0.0.1") || reply.url.contains("localhost") {
        println!(
            "(the hub listens on loopback: start it with `--listen tailscale` or `--listen <LAN address>:<port>` for another machine)"
        );
    }
    Ok(())
}
