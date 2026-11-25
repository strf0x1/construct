//! `construct peer` behind the `peer` feature (#361): join a hub and follow
//! it. The peer's key lives in `$CONSTRUCT_HOME/peer.key` (`0600`, created on
//! first run, reused after); a join token comes from `--join` or
//! `CONSTRUCT_JOIN` and rides inside the encrypted channel. The wire logic is
//! `construct_peer::join`; this prints what the hub said and, when the hub
//! revokes the key, deletes it. Remote sessions arrive with #362; stamping
//! (#363) bakes `--url`, `--hub-key` and `--id` into the binary.

use anyhow::{Context, Result, bail};
use construct_core::config::Paths;
use construct_peer::join::{self, Event, Outcome};
use construct_peer::{Hello, PublicKey, keyfile};

#[derive(clap::Args)]
pub(super) struct PeerArgs {
    /// The hub's URL (`ws://host:port` or `wss://…`).
    #[arg(long)]
    url: String,
    /// The hub's public key (`construct hub key` prints it).
    #[arg(long)]
    hub_key: String,
    /// This peer's id at the hub.
    #[arg(long)]
    id: String,
    /// A join token from `construct hub invite` (or `CONSTRUCT_JOIN`).
    #[arg(long)]
    join: Option<String>,
}

pub(super) async fn run(paths: &Paths, args: PeerArgs) -> Result<()> {
    let hub = PublicKey::from_hex(&args.hub_key).context("--hub-key is not a 64-character hex key")?;
    let path = paths.home.join("peer.key");
    let key = keyfile::load_or_create(&path).context("peer key")?;
    let token = args.join.or_else(|| std::env::var("CONSTRUCT_JOIN").ok()).filter(|t| !t.is_empty());
    let mut hello = Hello::new(&args.id);
    if let Some(t) = token {
        hello = hello.with_join_token(t);
    }
    let id = args.id;
    let outcome = join::run(&args.url, &key, hub, &hello, |event| match event {
        Event::Waiting { code } => {
            eprintln!("peer: waiting for approval at the hub; code {code} (it must match what the hub shows)");
        }
        Event::Joined => eprintln!("peer: joined the hub as {id}"),
    })
    .await
    .with_context(|| format!("joining the hub at {}", args.url))?;
    match outcome {
        Outcome::Closed(reason) => {
            eprintln!("peer: the hub closed the link{}", reason.map(|r| format!(": {r}")).unwrap_or_default());
            Ok(())
        }
        Outcome::Lost(why) => bail!("lost the hub: {why}"),
        Outcome::Refused(why) => bail!("the hub refused {id}: {why}"),
        Outcome::Denied(why) => bail!("the hub denied {id}: {why}"),
        Outcome::Revoked(reason) => {
            keyfile::delete(&path).with_context(|| format!("delete {}", path.display()))?;
            bail!(
                "the hub revoked {id}{}; deleted {}",
                reason.map(|r| format!(" ({r})")).unwrap_or_default(),
                path.display()
            )
        }
    }
}
