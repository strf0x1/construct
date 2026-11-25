//! The running hub: peer listener, control socket, shutdown (#360).
//!
//! Startup order is the design choice: take the instance lock first, so a
//! second hub fails before it touches the key, the socket or the port; then
//! load the key and registry, bind the peer port, and only then replace the
//! socket file. Each peer connection runs WebSocket upgrade, Noise IK and the
//! mandatory 1-RTT `Hello` under one deadline before the registry sees it, so
//! a scanner without the hub's public key never reaches a log line, a row, or
//! the status. Shutdown (`construct hub stop`, SIGINT or SIGTERM) closes every
//! link with a `Close`, saves the registry and removes the socket; the lock
//! goes with the process.

use crate::control::{self, Request, Response};
use crate::files::Files;
use crate::registry::{Admission, Registry, State};
use crate::status::{PeerStatus, Status};
use crate::{key, lock};
use anyhow::{Context, Result};
use construct_peer::{
    Channel, Control, DEFAULT_HANDSHAKE_TIMEOUT, KeepAlive, KeepAliveState, Keypair, WireError, accept_ws,
    handshake_responder, recv_with_keepalive,
};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream};
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

struct Shared {
    files: Files,
    key: String,
    listen: String,
    registry: Registry,
    online: HashMap<String, Conn>,
    next_conn: u64,
}

struct Conn {
    id: u64,
    remote: String,
}

type Hub = Arc<Mutex<Shared>>;

#[expect(
    clippy::unwrap_used,
    reason = "poisoned lock: a peer task panicked mid-update; the hub cannot trust its state"
)]
fn lock(hub: &Hub) -> MutexGuard<'_, Shared> {
    hub.lock().unwrap()
}

/// Resolves once the stop flag is set (and drops the non-`Send` borrow
/// `wait_for` returns before any later await).
async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|&s| s).await;
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Shared {
    fn status(&self) -> Status {
        let peers = self
            .registry
            .peers
            .iter()
            .map(|p| {
                let conn = self.online.get(&p.id);
                PeerStatus { peer: p.clone(), online: conn.is_some(), remote: conn.map(|c| c.remote.clone()) }
            })
            .collect();
        Status {
            running: true,
            pid: Some(std::process::id()),
            listen: Some(self.listen.clone()),
            key: Some(self.key.clone()),
            socket: self.files.socket().display().to_string(),
            peers,
        }
    }

    fn save(&self) {
        if let Err(e) = self.registry.save(&self.files.registry()) {
            eprintln!("construct hub: saving the peer registry failed: {e:#}");
        }
    }
}

/// Run a hub until it is stopped. `ready` is called once everything is bound,
/// with the first status (so a caller can print the URL and key).
pub async fn serve(opts: Options, ready: impl FnOnce(&Status)) -> Result<()> {
    let files = Files::new(&opts.home);
    let socket = files.checked_socket()?;
    let _instance = lock::acquire(&files.lock())?;
    let keypair = Arc::new(key::load_or_create(&files.key())?);
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
    }));
    let (stop_tx, stop_rx) = watch::channel(false);

    let ctl_hub = hub.clone();
    let ctl_stop = stop_tx.clone();
    let ctl_task = tokio::spawn(control::serve(ctl, move |req| match req {
        Request::Status => Response::status(lock(&ctl_hub).status()),
        Request::Stop => {
            let _ = ctl_stop.send(true);
            Response::ok()
        }
    }));

    ready(&lock(&hub).status());

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut peers = JoinSet::new();
    let mut stopping = stop_rx.clone();
    loop {
        tokio::select! {
            accepted = tcp.accept() => {
                let Ok((stream, addr)) = accepted else { continue };
                peers.spawn(peer(hub.clone(), keypair.clone(), stream, addr, stop_rx.clone()));
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
    let shared = lock(&hub);
    shared.save();
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

/// One peer connection, from TCP accept to disconnect.
async fn peer(hub: Hub, keypair: Arc<Keypair>, stream: TcpStream, addr: SocketAddr, mut stop: watch::Receiver<bool>) {
    let check = hub.clone();
    let handshake = async {
        let ws = accept_ws(stream).await?;
        handshake_responder(ws, &keypair, move |k| lock(&check).registry.key_decision(k)).await
    };
    let (mut link, hello) = match tokio::time::timeout(DEFAULT_HANDSHAKE_TIMEOUT, handshake).await {
        Ok(Ok(done)) => done,
        // Scanners, wrong keys, replays and slow handshakes leave no trace.
        Ok(Err(_)) | Err(_) => return,
    };
    let id = hello.peer_id.clone();
    let remote = addr.to_string();

    let admitted = {
        let mut s = lock(&hub);
        let connected: HashSet<String> = s.online.keys().cloned().collect();
        let verdict = s.registry.admit(&id, link.remote_static(), now(), &|p| connected.contains(p));
        match verdict {
            Admission::Admitted(state) => {
                let conn = s.next_conn;
                s.next_conn += 1;
                s.online.insert(id.clone(), Conn { id: conn, remote: remote.clone() });
                s.save();
                Ok((state, conn))
            }
            Admission::Refused(why) => Err(why),
        }
    };
    let (state, conn) = match admitted {
        Ok(ok) => ok,
        Err(why) => {
            eprintln!("construct hub: refused {id} from {remote}: {why}");
            let _ = link.close(Some(why)).await;
            return;
        }
    };
    eprintln!("construct hub: {id} connected from {remote} ({})", state.label());
    if state == State::Pending {
        eprintln!("construct hub: {id} is waiting for approval (code {})", link.handshake_hash().sas_code());
    }

    let mut ka = KeepAliveState::new();
    loop {
        tokio::select! {
            got = recv_with_keepalive(&mut link, KeepAlive::default(), &mut ka) => match got {
                Ok(env) if env.channel == Channel::Control => {
                    if matches!(env.decode_control(), Ok(Control::Close { .. })) {
                        break;
                    }
                }
                // ACP and LLM channels arrive with #362 and #371.
                Ok(_) => {}
                Err(WireError::Closed) | Err(_) => break,
            },
            () = stopped(&mut stop) => {
                let _ = link.close(Some("hub shutting down".into())).await;
                break;
            }
        }
    }

    let mut s = lock(&hub);
    if s.online.get(&id).is_some_and(|c| c.id == conn) {
        s.online.remove(&id);
    }
    s.registry.seen(&id, now());
    s.save();
    drop(s);
    eprintln!("construct hub: {id} disconnected");
}
