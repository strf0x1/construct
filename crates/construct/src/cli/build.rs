//! `construct build` behind the `hub` feature (#363): stamp a peer binary for
//! this home's hub and, with `--invite`, register it and print the join
//! command. The stamping and release fetching live in
//! `construct_hub::build`; this picks the source binary and prints.

use anyhow::{Context, Result};
use construct_core::config::Paths;
use construct_core::config::stamp::BUILD_TARGET;
use construct_hub::Files;
use construct_hub::build::{self, Preset, Spec};
use construct_hub::invite::parse_span;
use construct_hub::server::DEFAULT_PORT;
use construct_hub::tailnet;
use std::path::PathBuf;

#[derive(clap::Args)]
pub(super) struct BuildArgs {
    /// The peer's id at the hub; the binary is `./construct-<name>`.
    #[arg(long)]
    name: String,
    /// The URL the peer dials (`ws://host:port` or `wss://…`).
    #[arg(long, required_unless_present = "tailscale")]
    url: Option<String>,
    /// Dial this machine's tailnet IP (`tailscale status`) on the running
    /// hub's port, else 9443, instead of giving `--url`.
    #[arg(long, conflicts_with = "url")]
    tailscale: bool,
    /// The target triple (default: this machine's). Another target's binary
    /// comes from the published release for this version.
    #[arg(long)]
    target: Option<String>,
    /// The release profile to fetch (`bare`: core tools and the peer dialer).
    #[arg(long, default_value = "bare")]
    profile: String,
    /// Stamp this local binary instead of fetching one.
    #[arg(long, conflicts_with = "release")]
    from: Option<PathBuf>,
    /// Fetch the release even for this machine's target, instead of
    /// stamping this binary.
    #[arg(long)]
    release: bool,
    /// A `[sandbox]` preset: `workspace`, `readonly-host` (read the host's
    /// config and logs, ask before any write) or `off`.
    #[arg(long)]
    sandbox: Option<Preset>,
    /// A config.toml whose keys are stamped too (`llm` defaults to "hub").
    #[arg(long)]
    settings: Option<PathBuf>,
    /// The directory the peer's agent works in (default: where it is started).
    #[arg(long)]
    cwd: Option<String>,
    /// The controller's public key (default: this home's, `construct hub key`).
    #[arg(long)]
    hub_key: Option<String>,
    /// Where to write the binary (default: `./construct-<name>`).
    #[arg(long, short)]
    out: Option<PathBuf>,
    /// Also expect the peer at the hub and print the command that joins it.
    #[arg(long)]
    invite: bool,
    /// How long the `--invite` token stays valid (`90s`, `15m`, `2h`, `1d`).
    #[arg(long, default_value = "15m", value_parser = parse_span, requires = "invite")]
    expires: u64,
}

pub(super) async fn run(paths: &Paths, args: BuildArgs) -> Result<()> {
    let files = Files::new(&paths.home);
    let url = match args.url {
        Some(u) => u,
        None => tailnet_url(&files).await?,
    };
    let hub_key = match args.hub_key {
        Some(k) => k,
        None => files.keypair()?.public().to_hex(),
    };
    let settings = match &args.settings {
        Some(p) => {
            let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
            toml::from_str(&text).with_context(|| format!("{} is not valid TOML", p.display()))?
        }
        None => toml::Table::new(),
    };
    let stamp =
        Spec { url: url.clone(), hub_key, id: args.name.clone(), cwd: args.cwd, sandbox: args.sandbox, settings }
            .stamp()?;

    let target = args.target.as_deref().unwrap_or(BUILD_TARGET);
    let (source, what) = match args.from {
        Some(p) => (p.clone(), format!("from {}", p.display())),
        None if target == BUILD_TARGET && !args.release => {
            (std::env::current_exe().context("finding this binary")?, "this binary".into())
        }
        None => {
            let version = env!("CARGO_PKG_VERSION");
            let p = build::release(&paths.home, version, target, &args.profile).await?;
            (p, format!("release v{version} {}", build::release_name(target, &args.profile)))
        }
    };
    let bytes = std::fs::read(&source).with_context(|| format!("reading {}", source.display()))?;
    let out = args.out.unwrap_or_else(|| PathBuf::from(format!("construct-{}", args.name)));
    build::write(&bytes, &stamp, &out)?;

    println!("stamped {} ({what}) as peer {}", out.display(), args.name);
    println!("  dials {url}");
    println!("  hub key {}", stamp.peer.hub_key);
    if let Some(llm) = stamp.config.get("llm") {
        println!("  llm = {llm}");
    }
    if let Some(p) = &stamp.peer.sandbox {
        println!("  sandbox preset {p}");
    }
    if target.contains("apple-darwin") {
        println!(
            "note: on macOS the appended stamp breaks `codesign --strict`; the binary runs, but cannot be notarized as is"
        );
    }
    if args.invite {
        let reply = super::hub::invite(&files, &args.name, args.expires, Some(url.clone()), url.clone()).await?;
        let file = out.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        println!("\ninvite for {}: single use. Copy the binary over, then on the target run:\n", reply.id);
        println!("  CONSTRUCT_JOIN={} ./{file}\n", reply.token);
    }
    Ok(())
}

/// `--tailscale`: this node's tailnet IP on the running hub's port (else the
/// default), with a warning when that hub only listens on loopback.
async fn tailnet_url(files: &Files) -> Result<String> {
    let node = tailnet::detect()?;
    let listen = super::hub::status(files).await?.listen;
    let port =
        listen.as_deref().and_then(|l| l.rsplit(':').next()).and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
    if listen.as_deref().is_some_and(|l| l.contains("127.0.0.1") || l.contains("localhost")) {
        eprintln!("warning: the running hub listens on loopback; restart it with `construct hub --listen tailscale`");
    }
    if let Some(name) = &node.name {
        eprintln!("tailnet: this machine is {name} ({})", node.ip);
    }
    Ok(node.url(port))
}
