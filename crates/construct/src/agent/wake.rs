//! Background jobs reaching the model (ADR 0026). A completion is told to the
//! model only at a turn boundary: it rides in right behind the user's message
//! when there is one, and starts a turn of its own when the agent is idle.
//! Nothing here runs mid-turn, so the prompt cache is never broken and a turn
//! always knows whether it answers the user or a job.

use super::{Agent, Event, Events};
use crate::jobs::{self, Jobs};
use crate::llm::Message;
use crate::tools::Group;
use anyhow::Result;

impl Agent {
    /// The session's jobs, for a UI to wait on.
    pub fn jobs(&self) -> Jobs {
        self.ctx.jobs.clone()
    }

    /// Put the endings no turn has told the model about into the
    /// conversation, journaled with the ids they consume. Returns whether any.
    pub(super) fn deliver_jobs(&mut self, tx: &Events) -> bool {
        let ended = self.ctx.jobs.pending();
        if ended.is_empty() {
            return false;
        }
        let ids: Vec<String> = ended.iter().map(|j| j.id.clone()).collect();
        let message = Message::user(jobs::render(&ended));
        let delivery = jobs::Delivered { ids: ids.clone(), via: "turn".into(), message: Some(message.clone()) };
        self.journal.emit(Event::JobsDelivered(delivery), tx);
        self.messages.push(message);
        self.ctx.jobs.delivered(&ids);
        true
    }

    /// Run a turn on the endings that are waiting. False when none were.
    pub async fn wake(&mut self, tx: &Events) -> Result<bool> {
        if self.ctx.jobs.pending().is_empty() {
            return Ok(false);
        }
        self.run_turn(None, None, tx).await?;
        Ok(true)
    }

    /// A run that exits when its work is done: waits for the next ending to
    /// wake it (true), or for nothing to be left running (false). Bounded by
    /// the jobs' own time limits.
    pub async fn next_wake(&self) -> bool {
        self.ctx.jobs.next().await
    }

    /// Cancel every job and wait for the cancellations to be journaled: what
    /// quitting does.
    pub async fn stop_jobs(&self) {
        self.ctx.jobs.shutdown().await;
    }

    /// Jobs the model started become tools it can use: `job` joins the
    /// request once there is one to look at, and stays.
    pub(super) fn offer_job_tool(&mut self) {
        if self.ctx.jobs.any() {
            self.groups.insert(Group::Jobs);
        }
    }
}
