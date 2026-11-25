//! `construct hub` behind the `hub` feature (#360): run the hub, print its
//! public key, show its status, or stop it. The logic lives in
//! `construct-hub`; this only maps arguments onto it.

use anyhow::{Result, bail};
use construct_core::config::Paths;
use construct_hub::control::{self, Request, Response};
use construct_hub::server::DEFAULT_LISTEN;
use construct_hub::{Files, Options, Status};
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
}

pub(super) async fn run(paths: &Paths, args: HubArgs) -> Result<()> {
    let files = Files::new(&paths.home);
    match args.cmd {
        None => {
            let listen = args
                .listen
                .or_else(|| std::env::var("CONSTRUCT_HUB_LISTEN").ok().filter(|s| !s.is_empty()))
                .unwrap_or_else(|| DEFAULT_LISTEN.into());
            construct_hub::serve(Options { home: paths.home.clone(), listen }, |s| {
                eprintln!("construct hub: peers dial {}", s.listen.as_deref().unwrap_or("?"));
                eprintln!("construct hub: key {}", s.key.as_deref().unwrap_or("?"));
                eprintln!("construct hub: control socket {}", s.socket);
            })
            .await
        }
        Some(HubCmd::Key) => {
            println!("{}", construct_hub::key::load_or_create(&files.key())?.public().to_hex());
            Ok(())
        }
        Some(HubCmd::Status { json }) => {
            let status = match ask(&files, &Request::Status).await? {
                Some(resp) => match resp.status {
                    Some(s) => *s,
                    None => bail!("hub: {}", resp.error.unwrap_or_else(|| "no status in the reply".into())),
                },
                None => Status::offline(&files)?,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
                print!("{}", status.render(now));
            }
            Ok(())
        }
        Some(HubCmd::Stop) => match ask(&files, &Request::Stop).await? {
            Some(resp) if resp.ok => {
                println!("hub: stopping");
                Ok(())
            }
            Some(resp) => bail!("hub: {}", resp.error.unwrap_or_default()),
            None => bail!("no hub is running for this home"),
        },
    }
}

/// Ask the running hub, if any. A home too deep for a unix socket can have no
/// running hub, so it reads as "not running" rather than a connect error.
async fn ask(files: &Files, req: &Request) -> Result<Option<Response>> {
    match files.checked_socket() {
        Ok(socket) => control::request(&socket, req).await,
        Err(_) => Ok(None),
    }
}
