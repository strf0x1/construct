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
//! only then are turns cancelled. A peer the hub held for approval redials
//! the same way (#388): the new link is a new handshake, so it waits again
//! under a new code, and admission decides afresh (a closed pairing window
//! refuses it).
//!
//! Staying up (#367, ADR 0076): `--install` writes a systemd unit that runs
//! this with `--require-key` and restarts it after any exit but
//! `service::EXIT_NO_RESTART`, the code for an end restarting cannot fix:
//! revoked, refused, denied, or no key to rejoin with.

use anyhow::{Context, Result, bail};
use construct_core::acp::server::{self, Input};
use construct_core::config::{Config, Llm, LlmGateway, Paths, stamp};
use construct_core::tools::ToolSources;
use construct_peer::gateway::{self, Relay};
use construct_peer::join::{self, Event, Outbox, Outcome};
use construct_peer::{Backoff, Hello, PublicKey, WireError, keyfile, service};
use install::{Target, install, uninstall};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

mod install;

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
    /// Keep this joined peer running: copy the binary out of its workspace
    /// and write a systemd unit that starts it at boot and restarts it after
    /// a crash (Linux).
    #[arg(long, conflicts_with_all = ["uninstall", "join"])]
    install: bool,
    /// Stop and remove the unit `--install` wrote.
    #[arg(long)]
    uninstall: bool,
    /// With --install or --uninstall: a system unit (run as root, binary in
    /// /usr/local/bin) instead of a user unit.
    #[arg(long)]
    system: bool,
    /// With --install: copy this environment variable into a 0600 file the
    /// unit reads (API keys for `llm = "local"`); repeatable.
    #[arg(long = "env", value_name = "NAME", requires = "install")]
    env: Vec<String>,
    /// Never create a key: without `peer.key` this peer needs a new join and
    /// exits with the code that stops a restart. The installed unit runs so.
    #[arg(long)]
    require_key: bool,
}

/// End the peer for good: print why and exit with the code that tells the
/// installed unit not to restart it.
fn gone(why: &str) -> ! {
    eprintln!("construct: {why}");
    std::process::exit(service::EXIT_NO_RESTART)
}

/// A flag, else the stamped value, else an error naming the flag.
fn flag_or_stamp(flag: Option<String>, stamped: Option<&String>, name: &str) -> Result<String> {
    flag.or_else(|| stamped.cloned()).with_context(|| format!("--{name} is required (this binary is not stamped)"))
}

pub(super) async fn run(mut cfg: Config, paths: Paths, sources: ToolSources, args: PeerArgs) -> Result<()> {
    let stamped = stamp::current()?.map(|s| &s.peer);
    let id = flag_or_stamp(args.id.clone(), stamped.map(|p| &p.id), "id")?;
    if args.uninstall {
        return uninstall(&id, args.system);
    }
    let url = flag_or_stamp(args.url.clone(), stamped.map(|p| &p.url), "url")?;
    let hub_key = flag_or_stamp(args.hub_key.clone(), stamped.map(|p| &p.hub_key), "hub-key")?;
    let hub = PublicKey::from_hex(&hub_key).context("--hub-key is not a 64-character hex key")?;
    let path = paths.home.join("peer.key");
    let cwd = match args.cwd.clone().or_else(|| stamped.and_then(|p| p.cwd.as_ref()).map(PathBuf::from)) {
        Some(c) => c.canonicalize().with_context(|| format!("--cwd {}", c.display()))?,
        None => std::env::current_dir()?,
    };
    if args.install {
        let target = Target { id: &id, url: &url, hub_key: &hub_key, cwd: &cwd };
        return install(&cfg, &paths, &target, &args);
    }
    let key = if args.require_key {
        match keyfile::load(&path).context("peer key")? {
            Some(k) => k,
            None => gone(&format!(
                "no peer key at {}: {id} needs a new join (at the hub: `construct hub revoke {id}` if it is \
                 still pinned, then `construct hub invite {id}`)",
                path.display()
            )),
        }
    } else {
        keyfile::load_or_create(&path).context("peer key")?
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
    // An installed peer was joined before, so a hub it cannot reach yet
    // (booting, the network not up) is retried, not an exit.
    let links = follow(&url, &key, hub, &mut hello, &id, args.require_key, &to_agent, &relay, &mut outbox);
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
        Outcome::Refused(why) => gone(&format!("the hub refused {id}: {why}")),
        Outcome::Denied(why) => gone(&format!("the hub denied {id}: {why}")),
        Outcome::Revoked(reason) => {
            keyfile::delete(&path).with_context(|| format!("delete {}", path.display()))?;
            gone(&format!(
                "the hub revoked {id}{}; deleted {}",
                reason.map(|r| format!(" ({r})")).unwrap_or_default(),
                path.display()
            ))
        }
    }
}

/// Follow the hub, redialing after a lost link once this peer has joined or
/// been held for approval, until an outcome that ends the peer.
#[expect(clippy::too_many_arguments, reason = "one call site; each is a distinct piece of the peer's state")]
async fn follow(
    url: &str,
    key: &construct_peer::Keypair,
    hub: PublicKey,
    hello: &mut Hello,
    id: &str,
    joined_before: bool,
    to_agent: &mpsc::UnboundedSender<Input>,
    relay: &Relay,
    outbox: &mut Outbox,
) -> Result<Outcome, WireError> {
    let mut backoff = Backoff::new(Duration::from_millis(250), Duration::from_secs(15));
    let mut joined = joined_before;
    // Held for approval on some link, never joined: the hub knows this
    // peer is coming, so a lost link is redialed for a new code.
    let mut held = false;
    loop {
        let mut joined_now = false;
        let mut held_now = false;
        let got = join::run(url, key, hub, hello, outbox, |event| match event {
            Event::Waiting { code } => {
                held_now = true;
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
        } else if held_now {
            held = true;
            backoff.reset();
        }
        // A held link that ends was never approved: its code is void, and the
        // redial shows a new one.
        let again = if joined { "reconnecting" } else { "redialing for a new approval code" };
        match got {
            Ok(Outcome::Lost(why)) if joined || held => eprintln!("peer: lost the hub ({why}); {again}"),
            Ok(Outcome::Closed(why)) if joined || held => {
                eprintln!("peer: the hub closed the link ({}); {again}", why.as_deref().unwrap_or("no reason"));
            }
            Err(e) if joined || held => eprintln!("peer: cannot reach the hub ({e}); retrying"),
            other => return other,
        }
        if joined {
            // The hub's attachment is over; the agent keeps working for the next.
            let _ = to_agent.send(Input::Detached);
        }
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
