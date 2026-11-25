//! What the running hub's tasks share (#360, #361): the registry, live links
//! by peer id, and the pairing window, behind one mutex.
//!
//! The design choice: a link's task owns its socket, and everyone else
//! reaches it only through `Conn::tx` with a `Verdict`, so the control socket
//! and the peer tasks never contend for a stream.

use crate::files::Files;
use crate::registry::{Peer, Registry, State};
use crate::status::{PeerStatus, Status};
use construct_peer::PublicKey;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};

/// Everything the hub's tasks share, behind one mutex.
pub(crate) struct Shared {
    pub(crate) files: Files,
    pub(crate) key: String,
    pub(crate) listen: String,
    pub(crate) registry: Registry,
    /// Live links by peer id (pinned, or waiting for a human).
    pub(crate) online: HashMap<String, Conn>,
    pub(crate) next_conn: u64,
    pub(crate) pairing_until: Option<u64>,
}

/// One live link.
pub(crate) struct Conn {
    pub(crate) id: u64,
    pub(crate) remote: String,
    /// Tells the link's task what a human decided.
    pub(crate) tx: mpsc::UnboundedSender<Verdict>,
    /// Set while the join waits for approval.
    pub(crate) waiting: Option<Waiting>,
}

/// A join held for a human.
pub(crate) struct Waiting {
    pub(crate) key: PublicKey,
    pub(crate) code: String,
}

/// A decision made over the control socket, for a link's task to carry out.
#[derive(Debug)]
pub(crate) enum Verdict {
    Approved,
    Denied(String),
    Revoked(String),
}

pub(crate) type Hub = Arc<Mutex<Shared>>;

#[expect(
    clippy::unwrap_used,
    reason = "poisoned lock: a peer task panicked mid-update; the hub cannot trust its state"
)]
pub(crate) fn lock(hub: &Hub) -> MutexGuard<'_, Shared> {
    hub.lock().unwrap()
}

/// Resolves once the stop flag is set (and drops the non-`Send` borrow
/// `wait_for` returns before any later await).
pub(crate) async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|&s| s).await;
}

pub(crate) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Shared {
    pub(crate) fn status(&self) -> Status {
        let row = |p: &Peer| {
            let conn = self.online.get(&p.id);
            let waiting = conn.and_then(|c| c.waiting.as_ref());
            let mut peer = p.clone();
            if let Some(w) = waiting {
                peer.state = State::Pending;
                peer.key = Some(w.key.to_hex());
            }
            PeerStatus {
                peer,
                online: conn.is_some(),
                remote: conn.map(|c| c.remote.clone()),
                code: waiting.map(|w| w.code.clone()),
            }
        };
        let mut peers: Vec<PeerStatus> = self.registry.peers.iter().map(row).collect();
        // Joins inside a pairing window have no row until approved.
        let mut strays: Vec<PeerStatus> = self
            .online
            .iter()
            .filter(|(id, c)| c.waiting.is_some() && self.registry.get(id).is_none())
            .map(|(id, _)| row(&Peer::new(id, State::Pending)))
            .collect();
        strays.sort_by(|a, b| a.peer.id.cmp(&b.peer.id));
        peers.extend(strays);
        Status {
            running: true,
            pid: Some(std::process::id()),
            listen: Some(self.listen.clone()),
            key: Some(self.key.clone()),
            socket: self.files.socket().display().to_string(),
            pairing_until: self.pairing_until.filter(|&u| u > now()),
            peers,
        }
    }

    pub(crate) fn save(&self) {
        if let Err(e) = self.registry.save(&self.files.registry()) {
            eprintln!("construct hub: saving the peer registry failed: {e:#}");
        }
    }
}
