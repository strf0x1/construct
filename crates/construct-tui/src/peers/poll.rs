//! The background task between the TUI and its hub: it asks for the peers
//! once a second (and for the attached peer's session), carries out what a
//! human decided, and reports back as [`Msg`]s.
//!
//! The design choice: it is spawned after the first frame and talks to the
//! UI only through a channel, so a slow or missing hub never holds a draw.

use super::{Act, Msg, PeerHub};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// How often the tile is refreshed.
const EVERY: Duration = Duration::from_secs(1);

pub(super) async fn run(
    hub: Arc<dyn PeerHub>,
    mut acts: mpsc::UnboundedReceiver<Act>,
    mut attached: watch::Receiver<Option<String>>,
    tx: mpsc::UnboundedSender<Msg>,
) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            Ok(()) = attached.changed() => {}
            act = acts.recv() => {
                let Some(act) = act else { return };
                let said = said(&act);
                if tx.send(Msg::Done(hub.act(act).await.map(|()| said))).is_err() {
                    return;
                }
            }
        }
        let rows = hub.peers().await.ok().flatten();
        if tx.send(Msg::Peers(rows)).is_err() {
            return;
        }
        let watching = attached.borrow_and_update().clone();
        if let Some(id) = watching {
            let session = hub.session(&id).await;
            if tx.send(Msg::Session(id, session)).is_err() {
                return;
            }
        }
    }
}

/// The transcript's line for an act that went through.
fn said(act: &Act) -> String {
    match act {
        Act::Approve(id) => format!("{id} approved: its key is pinned"),
        Act::Deny(id) => format!("{id}'s join denied"),
        Act::Prompt { id, text } => format!("sent to {id}: {text}"),
        Act::Model { id, model: Some(m) } => format!("{id}'s model calls now go to {m}"),
        Act::Model { id, model: None } => format!("{id} uses the model it asks for"),
        Act::Answer { id, title, option, .. } => format!("{id}: answered {option:?} to {title}"),
    }
}
