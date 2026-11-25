//! `construct hub` behind the `hub` feature (#360, #361, #362): run the hub,
//! print its public key, show its status, invite, approve, deny or revoke
//! peers, open a pairing window, drive a peer's session (prompt, cancel,
//! session, answer), or stop it. The logic lives in `construct-hub`;
//! this only maps arguments onto it and prints the answers.

use anyhow::{Result, bail};
use construct_core::config::{Config, Paths};
use construct_hub::control::{self, Request, Response};
use construct_hub::invite::{DEFAULT_TTL_SECS, InviteReply, parse_span};
use construct_hub::registry::State;
use construct_hub::server::DEFAULT_LISTEN;
use construct_hub::view::SessionView;
use construct_hub::{Files, Options, Status, offline};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(clap::Args)]
pub(super) struct HubArgs {
    #[command(subcommand)]
    cmd: Option<HubCmd>,
    /// Address peers dial (`host:port`; default loopback, or
    /// `CONSTRUCT_HUB_LISTEN`). Give the tailnet or LAN address for a real drop.
    #[arg(long)]
    listen: Option<String>,
}

#[derive(clap::Subcommand)]
enum HubCmd {
    /// Print the controller's public key (created on first use). Back up
    /// `$CONSTRUCT_HOME/hub.key`: losing it orphans every peer.
    Key,
    /// Show the hub and its peers, whether or not a hub is running.
    Status {
        /// Print JSON instead of lines.
        #[arg(long)]
        json: bool,
    },
    /// Stop the running hub.
    Stop,
    /// Expect a peer and print the one-line command that joins it: a
    /// single-use token that pins the peer's key on first connect.
    Invite {
        /// The peer id (letters, digits, `-`, `_`, `.`).
        id: String,
        /// How long the token stays valid (`90s`, `15m`, `2h`, `1d`).
        #[arg(long, default_value = "15m", value_parser = parse_span)]
        expires: u64,
        /// The URL the peer dials, if not the hub's listen address.
        #[arg(long)]
        url: Option<String>,
        /// Print JSON instead of the command.
        #[arg(long)]
        json: bool,
    },
    /// List joins waiting for approval, with the code each peer shows.
    Pending,
    /// Approve the join waiting under this id (check its code first).
    Approve { id: String },
    /// Turn away the join waiting under this id.
    Deny { id: String },
    /// Revoke a peer: its key is refused from now on, and a connected peer
    /// deletes its key and stops.
    Revoke { id: String },
    /// Send a prompt to a peer's session; it runs on the peer even if this
    /// command, the link or the hub goes away.
    Prompt {
        id: String,
        #[arg(required = true)]
        text: Vec<String>,
        /// Wait for the turn to end and print the session.
        #[arg(long)]
        wait: bool,
    },
    /// Cancel the turn running on a peer.
    Cancel { id: String },
    /// Show a peer's session: its turn, transcript and open questions.
    Session {
        id: String,
        /// Print JSON instead of lines.
        #[arg(long)]
        json: bool,
    },
    /// Answer a peer's permission question with an option it offers
    /// (`once`, `session`, `always`, `deny`, …).
    Answer {
        id: String,
        option: String,
        /// Which open question (1-based), when more than one is open.
        #[arg(long)]
        question: Option<usize>,
    },
    /// Let peers the hub does not expect wait for approval for a while.
    Pair {
        /// How long the window stays open (`90s`, `10m`, `1h`).
        #[arg(long = "for", default_value = "10m", value_parser = parse_span)]
        span: u64,
        /// Close the window now.
        #[arg(long, conflicts_with = "span")]
        close: bool,
    },
}

const NO_HUB: &str = "no hub is running for this home; start it with `construct hub`";

pub(super) async fn run(cfg: Config, paths: &Paths, args: HubArgs) -> Result<()> {
    let files = Files::new(&paths.home);
    let listen = args
        .listen
        .or_else(|| std::env::var("CONSTRUCT_HUB_LISTEN").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| DEFAULT_LISTEN.into());
    let Some(cmd) = args.cmd else {
        return construct_hub::serve(Options { home: paths.home.clone(), listen, cfg }, |s| {
            eprintln!("construct hub: peers dial {}", s.listen.as_deref().unwrap_or("?"));
            eprintln!("construct hub: key {}", s.key.as_deref().unwrap_or("?"));
            eprintln!("construct hub: control socket {}", s.socket);
        })
        .await;
    };
    match cmd {
        HubCmd::Key => println!("{}", files.keypair()?.public().to_hex()),
        HubCmd::Status { json } => {
            let status = status(&files).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                print!("{}", status.render(now()));
            }
        }
        HubCmd::Stop => {
            expect_ok(ask(&files, &Request::Stop).await?)?;
            println!("hub: stopping");
        }
        HubCmd::Invite { id, expires, url, json } => {
            let reply =
                match ask(&files, &Request::Invite { id: id.clone(), ttl_secs: expires, url: url.clone() }).await? {
                    Some(resp) => match resp.invite {
                        Some(i) => i,
                        None => bail!("hub: {}", resp.error.unwrap_or_else(|| "no invite in the reply".into())),
                    },
                    None => offline::invite(&files, &id, expires, url.unwrap_or_else(|| format!("ws://{listen}")))?,
                };
            print_invite(&reply, expires, json)?;
        }
        HubCmd::Pending => {
            let status = status(&files).await?;
            let waiting: Vec<_> = status.peers.iter().filter(|p| p.peer.state == State::Pending).collect();
            if waiting.is_empty() {
                println!("no joins are waiting{}", if status.running { "" } else { " (no hub is running)" });
            }
            for p in waiting {
                let code = p.code.as_deref().unwrap_or("?");
                let from = p.remote.as_deref().unwrap_or("?");
                println!("{}  code {code}  from {from}  `construct hub approve {}`", p.peer.id, p.peer.id);
            }
        }
        HubCmd::Approve { id } => {
            expect_ok(ask(&files, &Request::Approve { id: id.clone() }).await?)?;
            println!("hub: {id} approved and pinned");
        }
        HubCmd::Deny { id } => {
            expect_ok(ask(&files, &Request::Deny { id: id.clone() }).await?)?;
            println!("hub: {id} denied");
        }
        HubCmd::Revoke { id } => {
            match ask(&files, &Request::Revoke { id: id.clone() }).await? {
                Some(resp) => expect_ok(Some(resp))?,
                None => offline::revoke(&files, &id)?,
            }
            println!("hub: {id} revoked");
        }
        HubCmd::Prompt { id, text, wait } => {
            let view = session(&files, Request::Prompt { id: id.clone(), text: text.join(" ") }).await?;
            if !wait {
                println!("hub: sent to {id}; `construct hub session {id}` follows it");
                return Ok(());
            }
            let mut view = view;
            while view.prompt.is_some() && view.error.is_none() {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                view = session(&files, Request::Session { id: id.clone() }).await?;
            }
            print!("{}", view.render());
        }
        HubCmd::Cancel { id } => {
            session(&files, Request::Cancel { id: id.clone() }).await?;
            println!("hub: cancel sent to {id}");
        }
        HubCmd::Session { id, json } => {
            let view = session(&files, Request::Session { id }).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                print!("{}", view.render());
            }
        }
        HubCmd::Answer { id, option, question } => {
            session(&files, Request::Answer { id: id.clone(), option: option.clone(), question }).await?;
            println!("hub: answered {id} with {option}");
        }
        HubCmd::Pair { span, close } => {
            let secs = if close { 0 } else { span };
            expect_ok(ask(&files, &Request::Pair { secs }).await?)?;
            if close {
                println!("hub: pairing window closed");
            } else {
                println!(
                    "hub: pairing window open for {}; `construct hub pending` shows who asks",
                    construct_hub::status::left(secs)
                );
            }
        }
    }
    Ok(())
}

fn print_invite(reply: &InviteReply, ttl: u64, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(reply)?);
        return Ok(());
    }
    let ttl = if ttl == DEFAULT_TTL_SECS { "15m".into() } else { construct_hub::status::left(ttl) };
    println!("invite for {}: single use, valid for {ttl}. On the target, run:\n", reply.id);
    println!("  {}\n", reply.command);
    if reply.url.contains("127.0.0.1") || reply.url.contains("localhost") {
        println!(
            "(the hub listens on loopback: start it with `--listen <tailnet or LAN address>:<port>` for another machine)"
        );
    }
    Ok(())
}

async fn status(files: &Files) -> Result<Status> {
    match ask(files, &Request::Status).await? {
        Some(resp) => match resp.status {
            Some(s) => Ok(*s),
            None => bail!("hub: {}", resp.error.unwrap_or_else(|| "no status in the reply".into())),
        },
        None => Status::offline(files),
    }
}

/// A session request to the running hub; sessions need one.
async fn session(files: &Files, req: Request) -> Result<SessionView> {
    let resp = ask(files, &req).await?;
    expect_ok(resp.clone())?;
    match resp.and_then(|r| r.session) {
        Some(v) => Ok(*v),
        None => bail!("hub: no session in the reply"),
    }
}

fn expect_ok(resp: Option<Response>) -> Result<()> {
    match resp {
        Some(r) if r.ok => Ok(()),
        Some(r) => bail!("hub: {}", r.error.unwrap_or_default()),
        None => bail!(NO_HUB),
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Ask the running hub, if any. A home too deep for a unix socket can have no
/// running hub, so it reads as "not running" rather than a connect error.
async fn ask(files: &Files, req: &Request) -> Result<Option<Response>> {
    match files.checked_socket() {
        Ok(socket) => control::request(&socket, req).await,
        Err(_) => Ok(None),
    }
}
