//! Multi-session headless manager (`SessionManager`, ADR 0079, #416).
//!
//! The design choice: one process owns a bounded pool of live `Agent`s across
//! `Fresh`, `Dedicated(lane)`, and `Shared` session lanes. Idle `Agent`s are
//! evicted from memory after `[wake].idle_evict` (releasing their SQLite WAL
//! writer lock) and transparently rehydrated from the journal on the next wake
//! or prompt via `Head.origin`.

pub mod watch;

use crate::agent::{self, Agent};
use crate::config::{Config, Paths};
use crate::event::SessionOrigin;
use crate::jobs::Jobs;
use crate::journal;
use crate::tools::ToolSources;
use crate::wake::SessionPolicy;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, watch as tokio_watch};

struct Slot {
    agent: Arc<Mutex<Agent>>,
    cancel: tokio_watch::Sender<bool>,
    jobs: Jobs,
    last_active: Instant,
    busy: bool,
    learn: bool,
    fresh: bool,
}

/// Manages multiple headless sessions in one process: lane routing, concurrent
/// active-turn cap, and idle session eviction with SQLite rehydration.
pub struct SessionManager {
    cfg: Config,
    paths: Paths,
    cwd: PathBuf,
    sources: ToolSources,
    live: HashMap<String, Slot>,
    dedicated: HashMap<String, String>,
    shared: Option<String>,
    max_concurrent: usize,
    idle_evict: Duration,
    no_learn: bool,
}

impl SessionManager {
    pub fn new(cfg: Config, paths: Paths, cwd: PathBuf, sources: ToolSources) -> Self {
        let max_concurrent = cfg.wake.max_concurrent_turns.max(1);
        let idle_evict = cfg.wake.idle_evict_duration().unwrap_or(Duration::from_secs(300));
        Self {
            cfg,
            paths,
            cwd,
            sources,
            live: HashMap::new(),
            dedicated: HashMap::new(),
            shared: None,
            max_concurrent,
            idle_evict,
            no_learn: false,
        }
    }

    pub fn with_no_learn(mut self, no_learn: bool) -> Self {
        self.no_learn = no_learn;
        self
    }

    pub fn idle_evict_duration(&self) -> Duration {
        self.idle_evict
    }

    pub fn max_concurrent_turns(&self) -> usize {
        self.max_concurrent
    }

    pub fn active_turns(&self) -> usize {
        self.live.values().filter(|s| s.busy).count()
    }

    pub fn can_start_turn(&self) -> bool {
        self.active_turns() < self.max_concurrent
    }

    pub fn live_count(&self) -> usize {
        self.live.len()
    }

    pub fn is_busy(&self, sid: &str) -> bool {
        self.live.get(sid).is_some_and(|s| s.busy)
    }

    /// Return the currently mapped session id for `policy` if known without
    /// creating a new session (used to check whether a target lane is busy).
    pub fn peek_target(&self, policy: &SessionPolicy) -> Option<&str> {
        match policy {
            SessionPolicy::Fresh => None,
            SessionPolicy::Dedicated(lane) => self.dedicated.get(lane).map(String::as_str),
            SessionPolicy::Shared => self.shared.as_deref(),
        }
    }

    /// Resolve a `SessionPolicy` to a live session id, creating a fresh session
    /// or rehydrating an evicted/on-disk session from SQLite WAL as needed.
    pub fn resolve(&mut self, policy: &SessionPolicy, source_name: &str, trigger: &str, learn: bool) -> Result<String> {
        let allow_learn = !self.no_learn && learn;
        match policy {
            SessionPolicy::Fresh => {
                let origin =
                    Some(SessionOrigin::Wake { source: source_name.to_string(), trigger: trigger.to_string() });
                self.open_new(origin, allow_learn, true)
            }
            SessionPolicy::Dedicated(lane) => {
                if let Some(sid) = self.dedicated.get(lane).cloned() {
                    if self.live.contains_key(&sid) {
                        return Ok(sid);
                    }
                    self.rehydrate(&sid, allow_learn)?;
                    return Ok(sid);
                }
                if let Some(sid) = journal::find_dedicated(&self.paths, &self.cwd, lane)? {
                    self.rehydrate(&sid, allow_learn)?;
                    self.dedicated.insert(lane.clone(), sid.clone());
                    return Ok(sid);
                }
                let origin = Some(SessionOrigin::Wake { source: source_name.to_string(), trigger: lane.clone() });
                let sid = self.open_new(origin, allow_learn, false)?;
                self.dedicated.insert(lane.clone(), sid.clone());
                Ok(sid)
            }
            SessionPolicy::Shared => {
                if let Some(sid) = self.shared.clone() {
                    if self.live.contains_key(&sid) {
                        return Ok(sid);
                    }
                    self.rehydrate(&sid, allow_learn)?;
                    return Ok(sid);
                }
                let origin =
                    Some(SessionOrigin::Wake { source: source_name.to_string(), trigger: trigger.to_string() });
                let sid = self.open_new(origin, allow_learn, false)?;
                self.shared = Some(sid.clone());
                Ok(sid)
            }
        }
    }

    fn open_new(&mut self, origin: Option<SessionOrigin>, learn: bool, fresh: bool) -> Result<String> {
        let agent = Agent::new_with_options(
            self.cfg.clone(),
            self.paths.clone(),
            self.cwd.clone(),
            &self.sources,
            agent::Options { resume: None, model_override: None, origin, learn: Some(learn) },
        )?;
        Ok(self.insert(agent, learn, fresh))
    }

    fn rehydrate(&mut self, sid: &str, learn: bool) -> Result<()> {
        let agent = Agent::new_with_options(
            self.cfg.clone(),
            self.paths.clone(),
            self.cwd.clone(),
            &self.sources,
            agent::Options { resume: Some(sid.to_string()), model_override: None, origin: None, learn: Some(learn) },
        )?;
        self.insert(agent, learn, false);
        Ok(())
    }

    fn insert(&mut self, agent: Agent, learn: bool, fresh: bool) -> String {
        let sid = agent.session_id().to_string();
        let cancel = agent.canceller();
        let jobs = agent.jobs();
        self.live.insert(
            sid.clone(),
            Slot {
                agent: Arc::new(Mutex::new(agent)),
                cancel,
                jobs,
                last_active: Instant::now(),
                busy: false,
                learn,
                fresh,
            },
        );
        sid
    }

    /// Mark `sid` busy and return its `Agent` handle if it is live and idle.
    pub fn start_turn(&mut self, sid: &str) -> Option<Arc<Mutex<Agent>>> {
        let slot = self.live.get_mut(sid)?;
        if slot.busy {
            return None;
        }
        slot.busy = true;
        slot.last_active = Instant::now();
        Some(slot.agent.clone())
    }

    /// Mark `sid` idle after a turn completes. Returns `Some(jobs)` if the
    /// session still has running or pending background jobs to wait on.
    pub fn finish_turn(&mut self, sid: &str) -> Option<Jobs> {
        let slot = self.live.get_mut(sid)?;
        slot.busy = false;
        slot.last_active = Instant::now();
        let has_jobs = slot.jobs.running() > 0 || !slot.jobs.pending().is_empty();
        if slot.fresh && !has_jobs {
            if let Some(removed) = self.live.remove(sid)
                && removed.learn
                && self.cfg.learn.enabled
                && let Ok(a) = removed.agent.try_lock()
            {
                a.spawn_detached_learning();
            }
            return None;
        }
        has_jobs.then(|| slot.jobs.clone())
    }

    /// Drop idle sessions whose `last_active` exceeds `idle_evict` from RAM.
    /// Dedicated/shared ids remain mapped so the next wake transparently
    /// rehydrates from SQLite WAL.
    pub fn evict_idle(&mut self) -> Vec<String> {
        let now = Instant::now();
        let ttl = self.idle_evict;
        let evict: Vec<String> = self
            .live
            .iter()
            .filter(|(_, s)| {
                !s.busy
                    && s.jobs.running() == 0
                    && s.jobs.pending().is_empty()
                    && now.saturating_duration_since(s.last_active) >= ttl
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &evict {
            if let Some(slot) = self.live.remove(id)
                && slot.learn
                && self.cfg.learn.enabled
                && let Ok(a) = slot.agent.try_lock()
            {
                a.spawn_detached_learning();
            }
        }
        evict
    }

    /// Cancel all active turns.
    pub fn cancel_all(&self) {
        for slot in self.live.values() {
            slot.cancel.send_replace(true);
        }
    }

    /// Stop remaining background jobs and trigger detached learning for
    /// sessions that opted in.
    pub async fn shutdown(&mut self) {
        for (_, slot) in self.live.drain() {
            let agent = slot.agent.lock().await;
            if agent.jobs().running() > 0 {
                agent.stop_jobs().await;
            }
            if slot.learn && self.cfg.learn.enabled {
                agent.spawn_detached_learning();
            }
        }
    }
}
