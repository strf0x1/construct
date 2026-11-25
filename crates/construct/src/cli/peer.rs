//! `construct peer` behind the `peer` feature (#361, #362): join a hub, then
//! run this machine's agent for it. The peer's key lives in
//! `$CONSTRUCT_HOME/peer.key` (`0600`, created on first run, reused after); a
//! join token comes from `--join` or `CONSTRUCT_JOIN` and rides inside the
//! encrypted channel. The wire logic is `construct_peer::join`; the agent is
//! `construct acp`'s server (`acp::server::serve`) on the link's `acp`
//! channel. A stamped binary (#363) supplies `--url`, `--hub-key`, `--id`
//! and `--cwd` when they are not given, and runs this with no arguments. With
//! `llm = "hub"` (#371) the agent's model calls go to a loopback gateway
//! (`construct_peer::gateway`) that relays them to the hub, so the peer
//! holds no API key.
//!
//! The design choice: once joined, a lost link is a detach, not an exit. The
//! agent keeps its sessions, running turns and unanswered questions while the
//! peer redials with backoff, and the hub reattaches with `session/load`
//! (ADR 0071). Only a revocation, a refusal, or SIGTERM/SIGINT stops it, and
//! only then are turns cancelled.

use anyhow::{Context, Result, bail};
use construct_core::acp::server::{self, Input};
use construct_core::config::{Config, Llm, LlmGateway, Paths, stamp};
use construct_core::tools::ToolSources;
use construct_peer::gateway::{self, Relay};
use construct_peer::join::{self, Event, Outbox, Outcome};
use construct_peer::{Backoff, Hello, PublicKey, WireError, keyfile};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(clap::Args, Default)]
pub(super) struct PeerArgs {
    /// The hub's URL (`ws://host:port` or `wss://…`; default: the stamped one).
    #[arg(long)]
    url: Option<String>,
    /// The hub's public key (`construct hub key` prints it; default: the
    /// stamped one).
    #[arg(long)]
    hub_key: Option<String>,
    /// This peer's id at the hub (default: the stamped one).
    #[arg(long)]
    id: Option<String>,
    /// A join token from `construct hub invite` (or `CONSTRUCT_JOIN`).
    #[arg(long)]
    join: Option<String>,
    /// The directory the agent works in (default: the stamped one, else
    /// where this was started).
    #[arg(long)]
    cwd: Option<PathBuf>,
}

/// A flag, else the stamped value, else an error naming the flag.
fn flag_or_stamp(flag: Option<String>, stamped: Option<&String>, name: &str) -> Result<String> {
    flag.or_else(|| stamped.cloned()).with_context(|| format!("--{name} is required (this binary is not stamped)"))
}

pub(super) async fn run(mut cfg: Config, paths: Paths, sources: ToolSources, args: PeerArgs) -> Result<()> {
    let stamped = stamp::current()?.map(|s| &s.peer);
    let url = flag_or_stamp(args.url, stamped.map(|p| &p.url), "url")?;
    let hub_key = flag_or_stamp(args.hub_key, stamped.map(|p| &p.hub_key), "hub-key")?;
    let id = flag_or_stamp(args.id, stamped.map(|p| &p.id), "id")?;
    let hub = PublicKey::from_hex(&hub_key).context("--hub-key is not a 64-character hex key")?;
    let path = paths.home.join("peer.key");
    let key = keyfile::load_or_create(&path).context("peer key")?;
    let cwd = match args.cwd.or_else(|| stamped.and_then(|p| p.cwd.as_ref()).map(PathBuf::from)) {
        Some(c) => c.canonicalize().with_context(|| format!("--cwd {}", c.display()))?,
        None => std::env::current_dir()?,
    };
    let token = args.join.or_else(|| std::env::var("CONSTRUCT_JOIN").ok()).filter(|t| !t.is_empty());
    let mut hello = Hello::new(&id).with_cwd(cwd.display().to_string());
    if let Some(t) = token {
        hello = hello.with_join_token(t);
    }

    let (llm_out, llm) = mpsc::unbounded_channel();
    let relay = Relay::new(llm_out);
    if cfg.llm == Llm::Hub {
        let gw = gateway::start(relay.clone()).await.context("start the model gateway")?;
        cfg.llm_gateway = Some(LlmGateway { base_url: gw.base_url, token: gw.token });
        eprintln!("peer: model calls go through the hub (llm = \"hub\")");
    }
    let (to_agent, input) = mpsc::unbounded_channel();
    let (out, acp) = mpsc::unbounded_channel();
    let agent = tokio::spawn(server::serve(cfg, paths, sources, input, out));
    let mut outbox = Outbox { acp, llm };
    let links = follow(&url, &key, hub, &mut hello, &id, &to_agent, &relay, &mut outbox);
    let outcome = tokio::select! {
        o = links => Some(o),
        () = stop_signal() => None,
    };
    // Closing the agent's input cancels its turns and lets them drain.
    drop(to_agent);
    let _ = agent.await;
    let outcome = match outcome {
        None => {
            eprintln!("peer: stopping");
            return Ok(());
        }
        Some(o) => o.with_context(|| format!("joining the hub at {url}"))?,
    };
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

/// Follow the hub, redialing after a lost link once this peer has joined,
/// until an outcome that ends the peer.
#[expect(clippy::too_many_arguments, reason = "one call site; each is a distinct piece of the peer's state")]
async fn follow(
    url: &str,
    key: &construct_peer::Keypair,
    hub: PublicKey,
    hello: &mut Hello,
    id: &str,
    to_agent: &mpsc::UnboundedSender<Input>,
    relay: &Relay,
    outbox: &mut Outbox,
) -> Result<Outcome, WireError> {
    let mut backoff = Backoff::new(Duration::from_millis(250), Duration::from_secs(15));
    let mut joined = false;
    loop {
        let mut joined_now = false;
        let got = join::run(url, key, hub, hello, outbox, |event| match event {
            Event::Waiting { code } => {
                eprintln!("peer: waiting for approval at the hub; code {code} (it must match what the hub shows)");
            }
            Event::Joined => {
                joined_now = true;
                relay.linked();
                eprintln!("peer: joined the hub as {id}");
            }
            Event::Acp(line) => {
                let _ = to_agent.send(Input::Line(line));
            }
            Event::Llm(frame) => relay.deliver(frame),
        })
        .await;
        // Model calls in flight on that link are cut; new ones wait for the next.
        relay.unlinked();
        if joined_now {
            joined = true;
            backoff.reset();
            // The token is single use and now spent.
            hello.join_token = None;
        }
        match got {
            Ok(Outcome::Lost(why)) if joined => eprintln!("peer: lost the hub ({why}); reconnecting"),
            Ok(Outcome::Closed(why)) if joined => {
                eprintln!("peer: the hub closed the link ({}); reconnecting", why.as_deref().unwrap_or("no reason"));
            }
            Err(e) if joined => eprintln!("peer: cannot reach the hub ({e}); retrying"),
            other => return other,
        }
        // The hub's attachment is over; the agent keeps working for the next.
        let _ = to_agent.send(Input::Detached);
        tokio::time::sleep(backoff.next_delay()).await;
    }
}

/// SIGTERM or SIGINT: the explicit ways to stop a joined peer.
async fn stop_signal() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}
