//! Headless wake routing, background job wake-ups, concurrency cap, and idle
//! wake-session eviction inside the ACP server (ADR 0079, #416).
//!
//! The design choice: `construct peer` runs local `[[wakes]]` and background
//! shell job wakes on the same actor loop as ACP prompts, including while
//! `Input::Detached`. Interactive sessions are never evicted while attached;
//! wake sessions evict after `idle_evict` and rehydrate via `Head.origin`.

use super::Server;
use crate::acp::session::Session;
use crate::acp::turn::{self, Command};
use crate::agent::{self, Agent};
use crate::event::SessionOrigin;
use crate::journal;
use crate::wake::{SessionPolicy, WakeBatch};
use anyhow::Result;
use tokio::task::JoinSet;

impl Server {
    /// Queue a batch from `WakeBus` and dispatch whatever fits under the
    /// active-turn cap.
    pub(super) fn on_wake_batch(&mut self, batch: WakeBatch, turns: &mut JoinSet<()>) {
        if self.closing {
            return;
        }
        self.pending_wakes.push_back(batch);
        self.dispatch_wakes(turns);
    }

    /// Dispatch buffered wake batches while active wake turns are below
    /// `[wake].max_concurrent_turns` and the target session is idle.
    pub(super) fn dispatch_wakes(&mut self, turns: &mut JoinSet<()>) {
        if self.closing {
            return;
        }
        while self.active_wake_turns() < self.max_concurrent_wakes
            && let Some(batch) = self.pending_wakes.pop_front()
        {
            let sid = match self.resolve_wake_session(&batch) {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("acp: wake {:?} failed to open session: {e:#}", batch.trigger);
                    continue;
                }
            };
            if let Some(s) = self.sessions.get_mut(&sid) {
                s.enqueue_wake(batch.events);
            }
            self.start_wake_for(&sid, turns);
        }
    }

    fn active_wake_turns(&self) -> usize {
        self.sessions.values().filter(|s| !s.is_idle() && s.is_wake_session()).count()
    }

    fn resolve_wake_session(&mut self, batch: &WakeBatch) -> Result<String> {
        match &batch.session {
            SessionPolicy::Fresh => {
                let origin =
                    Some(SessionOrigin::Wake { source: batch.source_name.clone(), trigger: batch.trigger.clone() });
                self.open_wake_agent(None, origin, batch.learn, Some(format!("fresh:{}", batch.trigger)), true)
            }
            SessionPolicy::Dedicated(lane) => {
                if let Some(sid) = self.dedicated.get(lane).cloned() {
                    if self.sessions.contains_key(&sid) {
                        return Ok(sid);
                    }
                    return self.open_wake_agent(Some(sid), None, batch.learn, Some(lane.clone()), false);
                }
                if let Some(sid) = journal::find_dedicated(&self.paths, &self.cwd, lane)? {
                    let id = self.open_wake_agent(Some(sid), None, batch.learn, Some(lane.clone()), false)?;
                    self.dedicated.insert(lane.clone(), id.clone());
                    return Ok(id);
                }
                let origin = Some(SessionOrigin::Wake { source: batch.source_name.clone(), trigger: lane.clone() });
                let id = self.open_wake_agent(None, origin, batch.learn, Some(lane.clone()), false)?;
                self.dedicated.insert(lane.clone(), id.clone());
                Ok(id)
            }
            SessionPolicy::Shared => {
                if let Some(sid) = self.shared_target() {
                    if self.sessions.contains_key(&sid) {
                        return Ok(sid);
                    }
                    return self.open_wake_agent(Some(sid), None, batch.learn, None, false);
                }
                let origin =
                    Some(SessionOrigin::Wake { source: batch.source_name.clone(), trigger: batch.trigger.clone() });
                let id = self.open_wake_agent(None, origin, batch.learn, None, false)?;
                self.shared_session = Some(id.clone());
                Ok(id)
            }
        }
    }

    fn shared_target(&self) -> Option<String> {
        // Prefer an interactive (non-wake) session if one exists.
        if let Some(sid) = self.sessions.iter().find(|(_, s)| !s.is_wake_session()).map(|(id, _)| id.clone()) {
            return Some(sid);
        }
        self.shared_session.clone()
    }

    fn open_wake_agent(
        &mut self,
        resume: Option<String>,
        origin: Option<SessionOrigin>,
        learn: bool,
        wake_lane: Option<String>,
        fresh: bool,
    ) -> Result<String> {
        let agent = Agent::new_with_options(
            self.cfg.clone(),
            self.paths.clone(),
            self.cwd.clone(),
            &self.sources,
            agent::Options { resume, model_override: None, origin, learn: Some(learn) },
        )?;
        self.next_session += 1;
        // No client owns a wake session: `u64::MAX` matches no attachment (they
        // count up from 0), so its events stream to nobody until a
        // `session/load` attaches it.
        let (sid, mut session) = Session::from_agent(agent, self.next_session, u64::MAX)?;
        session.mark_wake(wake_lane, fresh, learn);
        self.sessions.insert(sid.clone(), session);
        Ok(sid)
    }

    /// Start the next queued wake or pending background-job wake for `sid` if
    /// it is idle. Returns `true` if a wake turn started.
    pub(super) fn start_wake_for(&mut self, sid: &str, turns: &mut JoinSet<()>) -> bool {
        if self.closing {
            return false;
        }
        let Some((agent, generation, events)) = self.sessions.get_mut(sid).and_then(Session::start_next_wake) else {
            return false;
        };
        turns.spawn(turn::run_wake(agent, sid.to_string(), generation, events, self.commands.clone()));
        self.turn_notice(sid);
        true
    }

    /// After any turn finishes on `sid`, start the next queued prompt, else
    /// start any pending wake/job completion, else arm a background waiter if
    /// jobs are still running.
    pub(super) fn start_next_or_watch(&mut self, sid: &str, turns: &mut JoinSet<()>) {
        if self.start_prompt(sid, turns) {
            return;
        }
        if self.start_wake_for(sid, turns) {
            return;
        }
        if let Some((generation, jobs)) = self.sessions.get(sid).and_then(Session::running_jobs_watch) {
            let commands = self.commands.clone();
            let sid = sid.to_string();
            tokio::spawn(async move {
                jobs.ready().await;
                let _ = commands.send(Command::JobReady { session: sid, generation });
            });
        }
        // Evict completed fresh wake sessions right away if they have no jobs.
        if self.sessions.get(sid).is_some_and(Session::is_done_fresh) {
            self.sessions.remove(sid);
        }
    }

    /// Evict idle wake sessions past `idle_evict` so RAM stays bounded;
    /// dedicated sessions remain in `self.dedicated` and rehydrate on next wake.
    pub(super) fn evict_idle_wake_sessions(&mut self) {
        let ttl = self.idle_evict;
        let evict: Vec<String> =
            self.sessions.iter().filter(|(_, s)| s.is_evictable(ttl)).map(|(id, _)| id.clone()).collect();
        for id in evict {
            self.sessions.remove(&id);
        }
    }
}
