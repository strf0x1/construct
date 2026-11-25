//! One peer connection, from TCP accept to disconnect (#360, #361).
//!
//! WebSocket upgrade, Noise IK and the 1-RTT `Hello` run under one deadline
//! before the registry sees anything, so a scanner or a replayed message 1
//! leaves no row, no log line and no pending join. Then admission decides,
//! and the hub says so on the wire exactly once: `JoinAck` (pinned),
//! `Pending` (held for a human; a `JoinAck` or `Revoke` follows when one
//! decides over the control socket), or `Close` with the reason. The design
//! choice: a decision reaches the link through its own channel, so the
//! control socket never touches a socket it does not own.

use crate::admit::{Admission, Offer};
use crate::state::{Conn, Hub, Verdict, Waiting, lock, now, stopped};
use construct_peer::{
    Channel, Control, DEFAULT_HANDSHAKE_TIMEOUT, Envelope, FrameStream, KeepAlive, KeepAliveState, Keypair,
    PINNED_MAX_ENVELOPE, PeerLink, accept_ws, handshake_responder, recv_with_keepalive,
};
use std::net::SocketAddr;
use std::sync::Arc;
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
        // Scanners, wrong or revoked keys, replays and slow handshakes leave no trace.
        Ok(Err(_)) | Err(_) => return,
    };
    let remote = addr.to_string();
    let id = hello.peer_id.clone();
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
        let waiting = matches!(admission, Admission::Pending { .. }).then(|| Waiting { key, code: code.clone() });
        if !matches!(admission, Admission::Refused(_)) {
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
        Admission::Pinned => {
            link.set_max_envelope_bytes(PINNED_MAX_ENVELOPE);
            if send(&mut link, &Control::JoinAck { accepted: true, reason: None }).await {
                eprintln!("construct hub: {id} connected from {remote} (pinned)");
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

    follow(&mut link, &id, rx, stop).await;

    let mut s = lock(&hub);
    if s.online.get(&id).is_some_and(|c| c.id == conn) {
        s.online.remove(&id);
    }
    s.registry.seen(&id, now());
    s.save();
    drop(s);
    eprintln!("construct hub: {id} disconnected");
}

/// Keep the link alive and carry out what a human decides, until it ends.
async fn follow<S: FrameStream>(
    link: &mut PeerLink<S>,
    id: &str,
    mut rx: mpsc::UnboundedReceiver<Verdict>,
    mut stop: watch::Receiver<bool>,
) {
    let mut ka = KeepAliveState::new();
    loop {
        tokio::select! {
            got = recv_with_keepalive(link, KeepAlive::default(), &mut ka) => match got {
                Ok(env) if env.channel == Channel::Control => {
                    if matches!(env.decode_control(), Ok(Control::Close { .. })) {
                        return;
                    }
                }
                // ACP and LLM channels arrive with #362 and #371.
                Ok(_) => {}
                Err(_) => return,
            },
            verdict = rx.recv() => match verdict {
                Some(Verdict::Approved) => {
                    link.set_max_envelope_bytes(PINNED_MAX_ENVELOPE);
                    send(link, &Control::JoinAck { accepted: true, reason: None }).await;
                    eprintln!("construct hub: {id} approved and pinned");
                }
                Some(Verdict::Denied(reason)) => {
                    send(link, &Control::JoinAck { accepted: false, reason: Some(reason) }).await;
                    let _ = link.close(None).await;
                    eprintln!("construct hub: {id} denied");
                    return;
                }
                Some(Verdict::Revoked(reason)) => {
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
