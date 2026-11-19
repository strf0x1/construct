//! A delegated child's way of asking: up to its parent.
//!
//! The child has no terminal and a scratch home, so it neither shows a prompt
//! nor writes config. A question goes out as `Event::Ask` on its stdout and
//! waits for the parent's `ToChild::Answer`. The parent has already written
//! anything "always" asked for, so an `Always` comes back to the child as
//! `Session`: that is all the child itself has to remember.

use super::{Ask, Decision, Request, Verdict};
use crate::event::{Event, Events};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc::WeakUnboundedSender;
use tokio::sync::oneshot;

pub struct Upstream {
    /// Weak, for the reason `Perms::sink` is: the child's output ends when
    /// every sender is gone.
    tx: WeakUnboundedSender<Event>,
    next: AtomicU64,
    waiting: Mutex<HashMap<String, oneshot::Sender<Decision>>>,
}

impl Upstream {
    pub fn new(tx: Events) -> Self {
        Self { tx: tx.downgrade(), next: AtomicU64::new(1), waiting: Mutex::new(HashMap::new()) }
    }

    /// The parent's answer to question `id`. One for a question no longer
    /// waiting (it was cancelled) is dropped.
    pub fn answer(&self, id: &str, decision: Decision) {
        // A poisoned lock means a panic mid-insert; the map is still usable.
        let tx = self.waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(id);
        if let Some(tx) = tx {
            let _ = tx.send(decision);
        }
    }
}

#[async_trait::async_trait]
impl Ask for Upstream {
    async fn ask(&self, request: Request) -> Decision {
        let id = format!("ask-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (reply, answer) = oneshot::channel();
        self.waiting.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), reply);
        let Some(tx) = self.tx.upgrade() else { return Decision::DENY };
        if tx.send(Event::Ask { id: id.clone(), request }).is_err() {
            return Decision::DENY;
        }
        let d = answer.await.unwrap_or(Decision::DENY);
        // Asked and no longer waiting either way.
        self.waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        Decision { verdict: if d.verdict == Verdict::Always { Verdict::Session } else { d.verdict }, ..d }
    }
}
