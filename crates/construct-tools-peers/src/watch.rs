//! Waiting for one sent prompt to be answered, for a synchronous `send` and
//! for the background jobs an async one becomes.
//!
//! The design choice: a prompt is known by its idempotency key, never by
//! "the peer went idle". The hub resends an unanswered prompt with the same
//! key after any reconnect (ADR 0071), and a later prompt has another key,
//! so the answer a watch reports is this prompt's and arrives once. A peer
//! blocked on a permission question only a human may answer also wakes the
//! coordinator: that job ends `waiting` and hands the rest of the watch to a
//! new job that already knows the question, so it is told once per question.

use crate::hub::{Answered, Hub};
use construct_core::event::Events;
use construct_core::jobs::{self, Finish, Jobs, Kind, Run, Sink, Status};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

/// How often the hub is asked; it is a local socket.
const POLL: Duration = Duration::from_millis(250);

/// Which job watches each peer, shared with the tool so `cancel` finds the
/// latest one after a watch has handed over.
pub(crate) type Watching = Arc<Mutex<HashMap<String, String>>>;

/// How a watch ended.
pub(crate) enum Outcome {
    Answered(Answered),
    /// The peer opened permission questions nobody has told the model about.
    Asked(Vec<String>),
    /// The hub no longer shows the prompt, in flight or answered.
    Lost,
    /// The time ran out with the hub unreachable: its last error.
    Unreachable(String),
    TimedOut,
    Cancelled,
}

/// Watch `peer` until the prompt sent with `key` is answered, it asks a
/// question not in `seen`, `deadline` passes, or `cancel` goes true. `seen`
/// is forgotten once no question is open, so a question asked again later
/// is news again.
pub(crate) async fn answer(
    hub: &dyn Hub,
    peer: &str,
    key: &str,
    seen: &mut Vec<String>,
    deadline: Instant,
    mut cancel: Option<&mut tokio::sync::watch::Receiver<bool>>,
) -> Outcome {
    loop {
        let unreachable = match hub.session(peer).await {
            Ok(s) => match s.answered {
                Some(a) if a.key == key => return Outcome::Answered(a),
                _ if s.key.as_deref() == Some(key) => {
                    let new: Vec<String> = s.questions.iter().filter(|q| !seen.contains(q)).cloned().collect();
                    if !new.is_empty() {
                        seen.extend(new.iter().cloned());
                        return Outcome::Asked(new);
                    }
                    if s.questions.is_empty() {
                        seen.clear();
                    }
                    None
                }
                _ => return Outcome::Lost,
            },
            // A hub restarting keeps the prompt in its registry: wait it out.
            Err(e) => Some(e),
        };
        let now = Instant::now();
        if now >= deadline {
            return unreachable.map_or(Outcome::TimedOut, Outcome::Unreachable);
        }
        let nap = tokio::time::sleep(POLL.min(deadline - now));
        match cancel.as_deref_mut() {
            Some(rx) => {
                tokio::select! {
                    () = nap => {}
                    _ = rx.changed() => if *rx.borrow() { return Outcome::Cancelled },
                }
            }
            None => nap.await,
        }
    }
}

/// Everything a background watch needs to run and to hand over to the next.
pub(crate) struct Watch {
    pub hub: Arc<dyn Hub>,
    pub peer: String,
    pub key: String,
    /// The tool's wire name, for notes that point the model back at it.
    pub tool: &'static str,
    /// Questions the model has been told about.
    pub seen: Vec<String>,
    pub label: String,
    pub call_id: String,
    pub jobs: Jobs,
    pub sink: (Arc<dyn Sink>, Events),
    pub watching: Watching,
}

impl Watch {
    /// Start this watch as a job of kind `remote` for at most `secs`.
    pub(crate) fn start(self, secs: u64) -> Result<String, String> {
        let (jobs, peer, watching) = (self.jobs.clone(), self.peer.clone(), self.watching.clone());
        let id = jobs.start(jobs::Spec {
            kind: Kind::Remote,
            label: self.label.clone(),
            call_id: self.call_id.clone(),
            timeout: Duration::from_secs(secs.max(1)),
            sink: self.sink.clone(),
            body: Box::new(move |run: Run| Box::pin(self.run(run))),
        })?;
        watching.lock().unwrap_or_else(|e| e.into_inner()).insert(peer, id.clone());
        Ok(id)
    }

    /// The job's body: the peer's reply becomes its output, which
    /// `jobs::render` delivers inside a tag it cannot close.
    async fn run(mut self, mut run: Run) -> Finish {
        let started = Instant::now();
        let deadline = started + run.timeout;
        let outcome =
            answer(self.hub.as_ref(), &self.peer, &self.key, &mut self.seen, deadline, Some(&mut run.cancel)).await;
        let (peer, tool) = (self.peer.clone(), self.tool);
        let (status, note) = match outcome {
            Outcome::Answered(a) => {
                let turn = a.turn.map(|t| format!(" · turn {t}")).unwrap_or_default();
                run.tail.push(format!("peer {peer}{turn} · {}\n{}", a.stop, a.reply).as_bytes());
                ended(&a.stop)
            }
            Outcome::Asked(titles) => {
                run.tail.push(format!("peer {peer} · waiting for the user\n{}", titles.join("\n")).as_bytes());
                let left = run.timeout.saturating_sub(started.elapsed()).as_secs();
                let next = match self.start(left) {
                    Ok(id) => format!("Still watching as job {id}: you will be told when its turn ends."),
                    Err(e) => format!("Could not keep watching ({e}); {tool} status reads {peer}'s session."),
                };
                (
                    Status::Waiting,
                    format!(
                        "[{peer} is paused on the permission question above, which only the user can answer, at the hub or in the TUI; you cannot answer it. Tell the user if they may not have seen it. {next}]"
                    ),
                )
            }
            Outcome::Lost => {
                (Status::Failed, format!("[the hub no longer shows this prompt; {tool} status reads {peer}'s session]"))
            }
            Outcome::Unreachable(e) => (Status::Timeout, format!("[the hub stayed unreachable: {e}]")),
            Outcome::TimedOut => (
                Status::Timeout,
                format!(
                    "[stopped watching; the peer's turn may still be running. {tool} status reads {peer}'s session]"
                ),
            ),
            Outcome::Cancelled => {
                let _ = self.hub.cancel(&peer).await;
                (Status::Cancelled, String::new())
            }
        };
        Finish { status, exit: None, note }
    }
}

/// A stop reason as a job ending, and what to add when it is not plain.
pub(crate) fn ended(stop: &str) -> (Status, String) {
    match stop {
        "end_turn" => (Status::Completed, String::new()),
        "cancelled" => (Status::Failed, "[the peer's turn was cancelled at the hub or on the peer]".into()),
        s if s.starts_with("error") => (Status::Failed, format!("[the peer's turn failed: {s}]")),
        s => (Status::Completed, format!("[the peer's turn stopped: {s}]")),
    }
}
