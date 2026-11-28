//! Unified turn-boundary wake delivery (ADR 0026, ADR 0079). Both background
//! job completions (`jobs::Status::wakes`) and external `WakeEvent`s pass
//! through this one choke point: turn-boundary only, journaled before delivery,
//! and exactly once. Nothing here runs mid-turn, so the prompt cache is never
//! broken and a turn always knows whether it answers the user or a wake.

use super::{Agent, Event, Events};
use crate::jobs::{self, Jobs};
use crate::llm::Message;
use crate::tools::Group;
use crate::wake::{self, WakeEvent};
use anyhow::Result;

impl Agent {
    /// The session's jobs, for a UI or headless host to wait on.
    pub fn jobs(&self) -> Jobs {
        self.jobs.clone()
    }

    /// A user turn releases what a resume recovered: job endings and wakes
    /// held back so opening history alone never starts a turn.
    pub(super) fn activate_recovered(&self) {
        self.jobs.activate_recovered();
        self.wakes.activate_recovered();
    }

    /// Single turn-boundary wake choke point: deliver pending job endings and
    /// pending wake events into the conversation, journaled before pushing to
    /// context. Returns whether any wake or job completion was delivered.
    pub(super) fn deliver_wakes(&mut self, tx: &Events) -> bool {
        let jobs_sent = self.deliver_jobs(tx);
        let wakes_sent = self.deliver_wake_events(tx);
        jobs_sent || wakes_sent
    }

    fn deliver_jobs(&mut self, tx: &Events) -> bool {
        let ended = self.jobs.pending();
        if ended.is_empty() {
            return false;
        }
        let ids: Vec<String> = ended.iter().map(|j| j.id.clone()).collect();
        let message = Message::user(jobs::render(&ended));
        let delivery = jobs::Delivered { ids: ids.clone(), via: "turn".into(), message: Some(message.clone()) };
        self.journal.emit(Event::JobsDelivered(delivery), tx);
        self.context.push(message);
        self.jobs.delivered(&ids);
        true
    }

    fn deliver_wake_events(&mut self, tx: &Events) -> bool {
        let events = self.wakes.drain_pending();
        if events.is_empty() {
            return false;
        }
        // `WakeReceived` was journaled on arrival (`wake_with`, or a resumed
        // session's own rows); the UI sees it here, inside the turn it starts.
        for e in &events {
            let _ = tx.send(Event::WakeReceived(e.to_received()));
        }
        let message = Message::user(wake::render(&events));
        self.journal.emit(Event::WakesDelivered(wake::delivered_payload(&events, message.clone())), tx);
        self.context.push(message);
        true
    }

    /// Run a turn on pending job endings or queued wakes. False when none were.
    pub async fn wake(&mut self, tx: &Events) -> Result<bool> {
        if self.jobs.pending().is_empty() && !self.wakes.has_pending() {
            return Ok(false);
        }
        self.rearm();
        self.run_turn(None, tx).await?;
        Ok(true)
    }

    /// Journal `events` as received, queue them, and run one wake turn. A
    /// process that dies before the turn boundary finds them again on resume
    /// (`wake::unconsumed`), unless they outlived `[wake] default_ttl`.
    pub async fn wake_with(&mut self, events: Vec<WakeEvent>, tx: &Events) -> Result<bool> {
        if events.is_empty() {
            return Ok(false);
        }
        for e in &events {
            self.journal.record(&Event::WakeReceived(e.to_received()));
        }
        // A headless lane has no user turn to release what a resume recovered,
        // so a new wake carries the still-fresh ones along with it.
        self.wakes.activate_recovered();
        self.wakes.push(events);
        self.wake(tx).await
    }

    /// A run that exits when its work is done: waits for the next ending to
    /// wake it (true), or for nothing to be left running (false). Bounded by
    /// the jobs' own time limits.
    pub async fn next_wake(&self) -> bool {
        self.jobs.next().await
    }

    /// Cancel every job and wait for the cancellations to be journaled: what
    /// quitting does.
    pub async fn stop_jobs(&self) {
        self.jobs.shutdown().await;
    }

    /// Jobs the model started become tools it can use: `job` joins the
    /// request once there is one to look at, and stays.
    pub(super) fn offer_job_tool(&mut self) {
        if self.jobs.any() {
            self.groups.insert(Group::Jobs);
        }
    }
}
