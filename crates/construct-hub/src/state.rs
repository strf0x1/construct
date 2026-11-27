//! What the running hub's tasks share (#360, #361, #362): the registry, live
//! links by peer id, the pairing window, each peer's ACP session, and the
//! model proxy with what each peer's calls used and the model each asked for
//! (#371, #390), behind one mutex.
//!
//! The design choice: a link's task owns its socket, and everyone else
//! reaches it only through `Conn::tx` with a `ToLink`, so the control socket
//! and the peer tasks never contend for a stream. Every ACP line the hub
//! sends goes the same way, so lines leave in the order they were made.

use crate::files::Files;
use crate::proxy::{LlmUsage, Upstream};
use crate::registry::{Peer, Registry, State};
use crate::remote::Remote;
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
    /// Each pinned peer's ACP session, made on first use from its row.
    pub(crate) remotes: HashMap<String, Remote>,
    /// Where proxied model calls go (`proxy.rs`).
    pub(crate) upstream: Arc<Upstream>,
    /// What each peer's proxied calls used since the hub started.
    pub(crate) usage: HashMap<String, LlmUsage>,
    /// The model selector each peer last sent, override or not (#390).
    pub(crate) asked: HashMap<String, String>,
}

/// One live link.
pub(crate) struct Conn {
    pub(crate) id: u64,
    pub(crate) remote: String,
    /// Tells the link's task what a human decided, or what to send.
    pub(crate) tx: mpsc::UnboundedSender<ToLink>,
    /// Set while the join waits for approval.
    pub(crate) waiting: Option<Waiting>,
}

/// A join held for a human.
pub(crate) struct Waiting {
    pub(crate) key: PublicKey,
    pub(crate) code: String,
    /// Unix seconds when the link turns the join away (`APPROVAL_TTL_SECS`).
    pub(crate) until: u64,
}

/// What a link's task is told to do: carry out a decision made over the
/// control socket, or send a line to the peer's agent.
#[derive(Debug)]
pub(crate) enum ToLink {
    Approved,
    Denied(String),
    Revoked(String),
    Acp(String),
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

/// Request ids a new `Remote` starts after: the clock in ms, so a restarted
/// hub never reuses an id an earlier one sent the same peer.
fn first_id() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
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
            let remote = self.remotes.get(&p.id);
            PeerStatus {
                peer,
                online: conn.is_some(),
                remote: conn.map(|c| c.remote.clone()),
                code: waiting.map(|w| w.code.clone()),
                llm: self.usage.get(&p.id).cloned(),
                asked_model: self.asked.get(&p.id).cloned(),
                expires: waiting.map(|w| w.until),
                working: remote.is_some_and(Remote::working),
                questions: remote.map(Remote::questions).unwrap_or_default(),
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

    /// Step pinned peer `id`'s session with `f`, keep its persistent part in
    /// the registry (saved when it changed), and send the lines `f` made down
    /// the live link, if there is one.
    pub(crate) fn drive(
        &mut self,
        id: &str,
        f: impl FnOnce(&mut Remote) -> Result<Vec<String>, String>,
    ) -> Result<(), String> {
        let Some(row) = self.registry.get_mut(id).filter(|p| p.state == State::Pinned) else {
            return Err(format!("no pinned peer {id}"));
        };
        let remote = self.remotes.entry(id.to_string()).or_insert_with(|| Remote::new(Some(row), first_id()));
        let lines = f(remote)?;
        let changed = remote.sync(row);
        if let Some(conn) = self.online.get(id) {
            for line in lines {
                let _ = conn.tx.send(ToLink::Acp(line));
            }
        }
        if changed {
            self.save();
        }
        Ok(())
    }

    /// Whether `id` may make model calls: only while its key is pinned.
    pub(crate) fn pinned(&self, id: &str) -> bool {
        self.registry.get(id).is_some_and(|p| p.state == State::Pinned)
    }

    pub(crate) fn save(&self) {
        if let Err(e) = self.registry.save(&self.files.registry()) {
            eprintln!("construct hub: saving the peer registry failed: {e:#}");
        }
    }
}
