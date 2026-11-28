//! One peer connection, from TCP accept to disconnect (#360, #361).
//!
//! WebSocket upgrade, Noise IK and the 1-RTT `Hello` run under one deadline
//! before the registry sees anything, so a scanner or a replayed message 1
//! leaves no row, no log line and no pending join. Then admission decides,
//! and the hub says so on the wire exactly once: `JoinAck` (pinned),
//! `Pending` (held for a human; a `JoinAck` or `Revoke` follows when one
//! decides over the control socket), `Revoke` (this key was revoked while
//! the peer was away, #367), or `Close` with the reason. Only a link
//! that got `JoinAck { accepted: true }` carries ACP (#362): the hub attaches
//! to the peer's agent then, and detaches when the link ends, which cancels
//! nothing on the peer. The same link carries the peer's model calls on the
//! `llm` channel (#371, `proxy.rs`), served only while the peer is pinned
//! and aborted when the link ends. The design choice: a decision or an ACP line reaches
//! the link through its own channel, so the control socket never touches a
//! socket it does not own.

use crate::admit::{APPROVAL_TTL_SECS, Admission, Offer};
use crate::proxy::Calls;
use crate::state::{Conn, Hub, ToLink, Waiting, lock, now, stopped};
use construct_peer::{
    Channel, Control, DEFAULT_HANDSHAKE_TIMEOUT, Envelope, FrameStream, KeepAlive, KeepAliveState, Keypair, PeerLink,
    accept_ws, handshake_responder, llm, recv_with_keepalive,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

/// Run one connection until it ends.
pub(crate) async fn run(
    hub: Hub,
    keypair: Arc<Keypair>,
    stream: TcpStream,
    addr: SocketAddr,
    stop: watch::Receiver<bool>,
) {
    let check = hub.clone();
    let handshake = async {
        let ws = accept_ws(stream).await?;
        handshake_responder(ws, &keypair, move |k| lock(&check).registry.key_decision(k)).await
    };
    let (link, hello) = match tokio::time::timeout(DEFAULT_HANDSHAKE_TIMEOUT, handshake).await {
        Ok(Ok(done)) => done,
        // Scanners, wrong keys, replays and slow handshakes leave no trace.
        Ok(Err(_)) | Err(_) => return,
    };
    let remote = addr.to_string();
    let id = hello.peer_id.clone();
    let cwd = hello.cwd.clone();
    let key = link.remote_static();
    let code = link.handshake_hash().sas_code();
    let (tx, rx) = mpsc::unbounded_channel();

    let (admission, conn) = {
        let mut s = lock(&hub);
        let live = s.live(&id);
        let offer = Offer { id: &id, key, token: hello.join_token.as_deref(), now: now() };
        let admission = s.registry.admit(offer, live);
        let conn = s.next_conn;
        s.next_conn += 1;
        let until = now().saturating_add(APPROVAL_TTL_SECS);
        let waiting =
            matches!(admission, Admission::Pending { .. }).then(|| Waiting { key, code: code.clone(), until });
        if !matches!(admission, Admission::Refused(_) | Admission::Revoked) {
            s.online.insert(id.clone(), Conn { id: conn, remote: remote.clone(), tx, waiting });
        }
        // A pin, a strike or a burnt slot all change the file.
        s.save();
        (admission, conn)
    };
    let mut link = link;
    match admission {
        Admission::Refused(why) => {
            eprintln!("construct hub: refused {id} from {remote}: {why}");
            let _ = link.close(Some(why)).await;
            return;
        }
        Admission::Revoked => {
            eprintln!("construct hub: told {id} from {remote} its key is revoked");
            send(&mut link, &Control::Revoke { reason: Some("revoked at the hub".into()) }).await;
            let _ = link.close(None).await;
            return;
        }
        Admission::Pinned => {
            link.pin();
            if send(&mut link, &Control::JoinAck { accepted: true, reason: None }).await {
                eprintln!("construct hub: {id} connected from {remote} (pinned)");
                attach(&hub, &id, cwd.clone());
            }
        }
        Admission::Pending { slot } => {
            let why = if slot { "expected" } else { "pairing window" };
            eprintln!(
                "construct hub: {id} wants to join from {remote} ({why}), code {code}: `construct hub approve {id}` or `deny {id}`"
            );
            send(&mut link, &Control::Pending).await;
        }
    }

    follow(&mut link, &hub, &id, cwd, rx, stop).await;

    let mut s = lock(&hub);
    // A newer link for this id may already have attached; leave it be.
    if s.online.get(&id).is_some_and(|c| c.id == conn) {
        s.online.remove(&id);
        let _ = s.drive(&id, |r| {
            r.detach();
            Ok(vec![])
        });
    }
    s.registry.seen(&id, now());
    s.save();
    drop(s);
    eprintln!("construct hub: {id} disconnected");
}

/// Attach to pinned peer `id`'s agent: `initialize`, then its session.
fn attach(hub: &Hub, id: &str, cwd: Option<String>) {
    let _ = lock(hub).drive(id, |r| Ok(r.attach(cwd)));
}

/// Keep the link alive, relay ACP both ways and carry out what a human
/// decides, until it ends.
async fn follow<S: FrameStream>(
    link: &mut PeerLink<S>,
    hub: &Hub,
    id: &str,
    cwd: Option<String>,
    mut rx: mpsc::UnboundedReceiver<ToLink>,
    mut stop: watch::Receiver<bool>,
) {
    let mut ka = KeepAliveState::new();
    // Dropped when this returns: every model call still running is aborted.
    let mut calls = Calls::new(lock(hub).upstream.clone());
    // A join nobody decides on is turned away when its countdown ends.
    let mut expiry = lock(hub)
        .online
        .get(id)
        .and_then(|c| c.waiting.as_ref())
        .map(|w| tokio::time::Instant::now() + Duration::from_secs(w.until.saturating_sub(now())));
    loop {
        tokio::select! {
            got = recv_with_keepalive(link, KeepAlive::default(), &mut ka) => match got {
                Ok(env) if env.channel == Channel::Control => {
                    if matches!(env.decode_control(), Ok(Control::Close { .. })) {
                        return;
                    }
                }
                Ok(env) if env.channel == Channel::Acp => {
                    let line = String::from_utf8_lossy(&env.bytes);
                    // Refused (no-op) for a join still waiting for a human.
                    let _ = lock(hub).drive(id, |r| Ok(r.receive(&line)));
                }
                Ok(env) => match llm::Frame::decode(&env) {
                    Ok(llm::Frame { msg: llm::Msg::Request { id: call }, bytes }) => {
                        // Checked on every call: a revoked or waiting peer gets
                        // nothing, and an override set since the last call applies.
                        let (pinned, model) = {
                            let s = lock(hub);
                            let pinned = s.pinned(id) && s.online.get(id).is_some_and(|c| c.waiting.is_none());
                            (pinned, s.registry.get(id).and_then(|p| p.model.clone()))
                        };
                        if let Some(asked) = calls.request(call, bytes, pinned, model.as_deref()) {
                            lock(hub).asked.insert(id.to_string(), asked);
                        }
                    }
                    Ok(llm::Frame { msg: llm::Msg::Cancel { id: call }, .. }) => calls.cancel(call),
                    // Hub-to-peer frames from a peer, or garbage: ignored.
                    Ok(_) | Err(_) => {}
                },
                Err(_) => return,
            },
            out = calls.next() => {
                if let Some(usage) = out.usage {
                    lock(hub).usage.entry(id.to_string()).or_default().add(usage.as_ref());
                }
                let Ok(env) = out.frame.encode() else { continue };
                if link.send(&env).await.is_err() {
                    return;
                }
            }
            order = rx.recv() => match order {
                Some(ToLink::Acp(line)) => {
                    if link.send(&Envelope::new(Channel::Acp, line.into_bytes())).await.is_err() {
                        return;
                    }
                }
                Some(ToLink::Approved) => {
                    expiry = None;
                    link.pin();
                    send(link, &Control::JoinAck { accepted: true, reason: None }).await;
                    eprintln!("construct hub: {id} approved and pinned");
                    attach(hub, id, cwd.clone());
                }
                Some(ToLink::Denied(reason)) => {
                    send(link, &Control::JoinAck { accepted: false, reason: Some(reason) }).await;
                    let _ = link.close(None).await;
                    eprintln!("construct hub: {id} denied");
                    return;
                }
                Some(ToLink::Revoked(reason)) => {
                    send(link, &Control::Revoke { reason: Some(reason.clone()) }).await;
                    let _ = link.close(Some(reason)).await;
                    eprintln!("construct hub: {id} revoked");
                    return;
                }
                // A newer link for this id took the slot.
                None => {
                    let _ = link.close(Some("replaced by a newer connection".into())).await;
                    return;
                }
            },
            () = tokio::time::sleep_until(expiry.unwrap_or_else(tokio::time::Instant::now)), if expiry.is_some() => {
                let still = lock(hub).online.get_mut(id).and_then(|c| c.waiting.take()).is_some();
                if still {
                    send(link, &Control::JoinAck { accepted: false, reason: Some("nobody approved the join in time".into()) }).await;
                    let _ = link.close(None).await;
                    eprintln!("construct hub: {id}'s join expired");
                    return;
                }
                expiry = None;
            }
            () = stopped(&mut stop) => {
                let _ = link.close(Some("hub shutting down".into())).await;
                return;
            }
        }
    }
}

async fn send<S: FrameStream>(link: &mut PeerLink<S>, msg: &Control) -> bool {
    match Envelope::control(msg) {
        Ok(env) => link.send(&env).await.is_ok(),
        Err(_) => false,
    }
}
