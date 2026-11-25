//! The running hub: peer listener, control socket, shutdown (#360).
//!
//! Startup order is the design choice: take the instance lock first, so a
//! second hub fails before it touches the key, the socket or the port; then
//! load the key and registry, bind the peer port, and only then replace the
//! socket file. Each peer connection runs WebSocket upgrade, Noise IK and the
//! mandatory 1-RTT `Hello` under one deadline before the registry sees it, so
//! a scanner without the hub's public key never reaches a log line, a row, or
//! the status; what happens after is `conn.rs`, the shared state
//! `state.rs`, and the control socket's operations `ops.rs`. Shutdown (`construct hub stop`, SIGINT or SIGTERM) closes every
//! link with a `Close`, saves the registry and removes the socket; the lock
//! goes with the process.

use crate::conn;
use crate::control::{self, Request};
use crate::files::Files;
use crate::lock;
use crate::registry::Registry;
use crate::state::{Hub, Shared, lock as hub_lock, stopped};
use crate::status::Status;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;

/// The default peer listen address: loopback only. A Tailscale or LAN drop
/// (#364) passes the tailnet or LAN interface explicitly.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:9443";

/// How long shutdown waits for peer links to say goodbye.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

/// How to run a hub.
#[derive(Debug, Clone)]
pub struct Options {
    pub home: PathBuf,
    /// `host:port` to accept peer WebSocket connections on (`:0` picks a port).
    pub listen: String,
}

/// Run a hub until it is stopped. `ready` is called once everything is bound,
/// with the first status (so a caller can print the URL and key).
pub async fn serve(opts: Options, ready: impl FnOnce(&Status)) -> Result<()> {
    let files = Files::new(&opts.home);
    let socket = files.checked_socket()?;
    let _instance = lock::acquire(&files.lock())?;
    let keypair = Arc::new(files.keypair()?);
    let registry = Registry::load(&files.registry())?;
    let tcp = TcpListener::bind(&opts.listen).await.with_context(|| format!("listen on {}", opts.listen))?;
    let listen = format!("ws://{}", tcp.local_addr()?);
    let ctl = control::bind(&socket)?;

    let hub: Hub = Arc::new(Mutex::new(Shared {
        files,
        key: keypair.public().to_hex(),
        listen,
        registry,
        online: HashMap::new(),
        next_conn: 1,
        pairing_until: None,
    }));
    let (stop_tx, stop_rx) = watch::channel(false);

    let ctl_hub = hub.clone();
    let ctl_stop = stop_tx.clone();
    let ctl_task = tokio::spawn(control::serve(ctl, move |req: Request| hub_lock(&ctl_hub).handle(req, &ctl_stop)));

    ready(&hub_lock(&hub).status());

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut peers = JoinSet::new();
    let mut stopping = stop_rx.clone();
    loop {
        tokio::select! {
            accepted = tcp.accept() => {
                let Ok((stream, addr)) = accepted else { continue };
                peers.spawn(conn::run(hub.clone(), keypair.clone(), stream, addr, stop_rx.clone()));
            }
            () = stopped(&mut stopping) => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = term.recv() => break,
        }
        // Reap finished peer tasks so the set does not grow without bound.
        while peers.try_join_next().is_some() {}
    }

    let _ = stop_tx.send(true);
    let _ = tokio::time::timeout(CLOSE_GRACE, async { while peers.join_next().await.is_some() {} }).await;
    peers.abort_all();
    ctl_task.abort();
    let shared = hub_lock(&hub);
    shared.save();
    let _ = std::fs::remove_file(&socket);
    Ok(())
}
