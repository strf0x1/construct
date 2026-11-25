//! Peer dialer subcommand hook behind the `peer` feature (#368).
//!
//! Full peer sessions (`acp` over the wire, join flow, stamping) land in
//! #361–#363. Wiring a real `dial` probe here ensures the `peer` feature
//! actually links the dialing code path (WebSocket + TLS + Noise IK + chunked
//! envelopes + keep-alive) into `construct`, so release binary size
//! measurements reflect the real linked footprint rather than dead code
//! stripped by LTO.

use anyhow::{Context, Result};
use construct_peer::{
    Backoff, Hello, KeepAlive, KeepAliveState, Keypair, PINNED_MAX_ENVELOPE, PublicKey, connect_with_backoff,
    connect_ws, handshake_initiator, recv_with_keepalive,
};

#[derive(clap::Args)]
pub(super) struct DialArgs {
    #[arg(long)]
    pub url: String,
    #[arg(long)]
    pub hub_key: String,
    #[arg(long)]
    pub peer_id: String,
}

/// Dial a hub endpoint, complete Noise IK + 1-RTT `Hello`, and keep the link
/// alive until the hub closes it. Used to exercise the wire layer before #361
/// adds persisted keys and #362 attaches the ACP server.
pub(super) async fn dial(args: DialArgs) -> Result<()> {
    let hub_pub = PublicKey::from_hex(&args.hub_key).context("invalid --hub-key hex")?;
    let local = Keypair::generate().context("generate ephemeral peer keypair")?;
    let hello = Hello::new(args.peer_id);
    let url = args.url;
    let mut backoff = Backoff::default();
    let mut link = connect_with_backoff(&mut backoff, Some(3), |_| {
        let url = url.clone();
        let local = local.clone();
        let hello = hello.clone();
        async move {
            let ws = connect_ws(&url).await?;
            handshake_initiator(ws, &local, hub_pub, &hello, PINNED_MAX_ENVELOPE).await
        }
    })
    .await
    .context("peer dial handshake")?;

    let mut ka = KeepAliveState::new();
    while let Ok(_env) = recv_with_keepalive(&mut link, KeepAlive::default(), &mut ka).await {}
    let _ = link.close(None).await;
    Ok(())
}
