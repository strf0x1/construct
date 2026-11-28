//! `construct watch` and `construct wake` (`emit`, `list`) subcommands
//! (ADR 0079, #416): standalone local multi-session headless runner and local
//! control-socket CLI escape hatch for external scripts and testing.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use construct_core::config::{Config, Paths};
use construct_core::llm;
use construct_core::sessions::watch::{WatchMsg, WatchOptions};
use construct_core::tools::ToolSources;
use construct_core::wake::{WakeSources, socket};
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use tokio::sync::watch as tokio_watch;

#[derive(Args)]
pub(super) struct WatchArgs {
    /// Working directory for sessions and command probes (default: current dir).
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Emit every session event as JSON lines on stdout.
    #[arg(long)]
    json: bool,
    /// Disable background learning for all wake turns in this run.
    #[arg(long)]
    no_learn: bool,
}

#[derive(Args)]
pub(super) struct WakeArgs {
    #[command(subcommand)]
    cmd: WakeCmd,
}

#[derive(Subcommand)]
enum WakeCmd {
    /// Emit a wake event to a running `construct watch` or `construct peer`.
    Emit {
        /// Configured `[[wakes]]` trigger name.
        name: String,
        /// Optional JSON payload (defaults to null).
        #[arg(long)]
        json: Option<String>,
        /// Optional human-readable summary line.
        #[arg(long)]
        summary: Option<String>,
        /// Optional deduplication key for burst coalescing.
        #[arg(long)]
        dedup_key: Option<String>,
        /// Override trust classification (`true` = untrusted data).
        #[arg(long)]
        untrusted: Option<bool>,
    },
    /// List configured `[[wakes]]` triggers and local watcher liveness.
    List {
        /// Print as JSON.
        #[arg(long)]
        json: bool,
    },
}

pub(super) async fn run_watch(
    mut cfg: Config,
    paths: Paths,
    tool_sources: ToolSources,
    wake_sources: WakeSources,
    args: WatchArgs,
) -> Result<()> {
    llm::overlay::prepare(&mut cfg, &paths, None).await;
    for w in cfg.shadowed() {
        eprintln!("construct: {w}");
    }
    let cwd = match args.cwd {
        Some(c) => c.canonicalize().with_context(|| format!("--cwd {}", c.display()))?,
        None => std::env::current_dir()?,
    };
    let (cancel_tx, cancel_rx) = tokio_watch::channel(false);
    let sig_tx = cancel_tx.clone();
    tokio::spawn(async move {
        stop_signal().await;
        let _ = sig_tx.send(true);
    });
    let json_mode = args.json;
    let res = construct_core::sessions::watch::run(
        cfg,
        paths,
        tool_sources,
        wake_sources,
        WatchOptions { cwd, no_learn: args.no_learn },
        cancel_rx,
        move |msg| match msg {
            WatchMsg::Status(line) => {
                eprintln!("watch: {line}");
            }
            WatchMsg::Event { session, event } => {
                if json_mode {
                    let mut out = std::io::stdout();
                    if let Ok(mut v) = serde_json::to_value(&event) {
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("session".into(), Value::String(session));
                        }
                        let _ = writeln!(out, "{v}");
                        let _ = out.flush();
                    }
                } else if let construct_core::event::Event::Error { message } = event.as_ref() {
                    eprintln!("watch: [{session}] error: {message}");
                }
            }
        },
    )
    .await;
    let _ = cancel_tx.send(true);
    res
}

pub(super) async fn run_wake(cfg: &Config, paths: &Paths, args: WakeArgs) -> Result<()> {
    match args.cmd {
        WakeCmd::Emit { name, json: payload, summary, dedup_key, untrusted } => {
            let data = match payload {
                Some(raw) => serde_json::from_str::<Value>(&raw).context("parsing --json payload")?,
                None => Value::Null,
            };
            let req =
                socket::EmitRequest::Emit { name, summary: summary.unwrap_or_default(), data, dedup_key, untrusted };
            match socket::send(paths, &req).await? {
                socket::EmitReply::Ok { id, trigger } => {
                    println!("emitted {id} to {trigger}");
                    Ok(())
                }
                socket::EmitReply::Error { message } => bail!("{message}"),
                socket::EmitReply::Pong { .. } => bail!("unexpected pong reply"),
            }
        }
        WakeCmd::List { json: as_json } => {
            let listening = socket::is_listening(paths).await;
            if as_json {
                let out = json!({
                    "listening": listening,
                    "socket": socket::socket_path(paths).display().to_string(),
                    "wakes": cfg.wakes,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
                return Ok(());
            }
            println!(
                "watcher: {} ({})",
                if listening { "running" } else { "not running" },
                socket::socket_path(paths).display()
            );
            if cfg.wakes.is_empty() {
                println!("no [[wakes]] configured");
                return Ok(());
            }
            for w in &cfg.wakes {
                let sched = w.every.as_deref().map(|e| format!("every {e}")).unwrap_or_else(|| "on-demand".into());
                let cd = w.cooldown.as_deref().map(|c| format!(" · cooldown {c}")).unwrap_or_default();
                let co = w.coalesce.as_deref().map(|c| format!(" · coalesce {c}")).unwrap_or_default();
                println!("  {}  [{}]  session={}  {sched}{cd}{co}", w.name, w.kind, w.session);
            }
            Ok(())
        }
    }
}

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
