//! Background jobs (ADR 0026, #34): work that outlives the tool call that
//! started it and tells the agent when it ends. A job is a background shell
//! command or, later, a background delegate; a watcher (CI, a review, a
//! delay) is just a command that exits on the thing it waits for.
//!
//! The design choice: the registry knows nothing about what a job does. A kind
//! hands it a body (a future that runs until the work ends and says how); the
//! registry owns the id, the limit, the cancel switch, the journal rows
//! (`job_start`, `job_end`) and the question the agent asks at a turn
//! boundary, "which endings have I not been told about". Delivery itself is
//! the agent's (`agent::wake`); jobs live and die with this process.

mod command;
mod config;
mod info;
mod spec;

pub use command::body as command_body;
pub use config::{JobsConfig, ShellConfig};
pub use info::{Delivered, Info, Kind, Started, Status, TAIL_MAX, Tail, highest_id, render, unconsumed};
pub use spec::{Body, Finish, Run, Spec};

use crate::event::{Event, Events};
use crate::journal::Writer;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, watch};

/// The session's jobs. Cheap to clone; every clone is the same registry.
#[derive(Clone)]
pub struct Jobs(Arc<Shared>);

struct Shared {
    limit: usize,
    state: Mutex<State>,
    /// Woken whenever a job ends or is delivered.
    changed: Notify,
}

#[derive(Default)]
struct State {
    jobs: Vec<Job>,
    next: u32,
}

struct Job {
    info: Info,
    started: Instant,
    tail: Tail,
    cancel: watch::Sender<bool>,
    /// The model has the ending, from a delivery or a `check`.
    delivered: bool,
}

impl State {
    fn running(&self) -> usize {
        self.jobs.iter().filter(|j| j.info.status == Status::Running).count()
    }
    fn pending(&self) -> Vec<Info> {
        self.jobs.iter().filter(|j| j.info.status.wakes() && !j.delivered).map(|j| j.info.clone()).collect()
    }
}

/// What `cancel` found.
pub enum Cancel {
    Sent,
    Over(Status),
    Unknown,
}

impl Jobs {
    pub fn new(cfg: &JobsConfig) -> Self {
        Self(Arc::new(Shared { limit: cfg.limit.max(1), state: Mutex::default(), changed: Notify::new() }))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // A poisoned lock means a panic mid-update; the registry is still usable.
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Take up a resumed session's journal: ids carry on from where they
    /// were, and an ending that woke nobody before is still owed.
    pub fn resume(&self, events: &[Event]) {
        let mut st = self.state();
        st.next = highest_id(events);
        for info in unconsumed(events) {
            st.jobs.push(Job {
                info,
                started: Instant::now(),
                tail: Tail::default(),
                cancel: watch::channel(false).0,
                delivered: false,
            });
        }
        drop(st);
        self.0.changed.notify_waiters();
    }

    /// Start a job and return its id. Refused over the limit.
    pub fn start(&self, spec: Spec) -> Result<String, String> {
        let (cancel, cancel_rx) = watch::channel(false);
        let tail = Tail::default();
        let id = {
            let mut st = self.state();
            let running = st.running();
            if running >= self.0.limit {
                return Err(format!(
                    "{running} background jobs are running, which is the limit. Wait for one to end, or cancel one with the job tool."
                ));
            }
            st.next += 1;
            let id = format!("j{}", st.next);
            let info = Info {
                id: id.clone(),
                kind: spec.kind,
                label: spec.label.clone(),
                status: Status::Running,
                exit: None,
                ms: 0,
                tail: String::new(),
            };
            st.jobs.push(Job { info, started: Instant::now(), tail: tail.clone(), cancel, delivered: false });
            id
        };
        let (journal, tx) = spec.sink;
        journal.emit(
            Event::JobStart(Started { id: id.clone(), kind: spec.kind, label: spec.label, call_id: spec.call_id }),
            &tx,
        );
        let run = Run { tail: tail.clone(), cancel: cancel_rx, timeout: spec.timeout };
        let (jobs, job_id, started) = (self.clone(), id.clone(), Instant::now());
        tokio::spawn(async move {
            // A body that panics still ends the job, or a waiting `run` never would.
            let fin = match tokio::spawn((spec.body)(run)).await {
                Ok(f) => f,
                Err(e) => Finish { status: Status::Failed, exit: None, note: format!("[the job crashed: {e}]") },
            };
            jobs.end(&job_id, fin, tail, started.elapsed(), &journal, &tx);
        });
        Ok(id)
    }

    /// Record an ending: the row first, then the registry, so the journal
    /// never shows a turn that read a completion before the completion.
    fn end(&self, id: &str, fin: Finish, tail: Tail, took: Duration, journal: &Writer, tx: &Events) {
        let mut text = tail.text();
        if !fin.note.is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&fin.note);
        }
        let known = self.state().jobs.iter().find(|j| j.info.id == id).map(|j| j.info.clone());
        let Some(mut info) = known else { return };
        info.status = fin.status;
        info.exit = fin.exit;
        info.ms = took.as_millis() as u64;
        info.tail = text;
        journal.emit(Event::JobEnd(info.clone()), tx);
        if let Some(j) = self.state().jobs.iter_mut().find(|j| j.info.id == id) {
            j.info = info;
        }
        self.0.changed.notify_waiters();
    }

    /// Endings that wake the agent and have not reached the model.
    pub fn pending(&self) -> Vec<Info> {
        self.state().pending()
    }

    /// The model has these endings now.
    pub fn delivered(&self, ids: &[String]) {
        for j in self.state().jobs.iter_mut().filter(|j| ids.contains(&j.info.id)) {
            j.delivered = true;
        }
        self.0.changed.notify_waiters();
    }

    /// One job as it is now, its output so far included. Reading an ended one
    /// is how the model gets its result, so it does not wake the agent again:
    /// the second field says this read consumed it.
    pub fn check(&self, id: &str) -> Option<(Info, bool)> {
        let mut st = self.state();
        let j = st.jobs.iter_mut().find(|j| j.info.id == id)?;
        let mut info = j.info.clone();
        if info.status == Status::Running {
            info.ms = j.started.elapsed().as_millis() as u64;
            info.tail = j.tail.text();
            return Some((info, false));
        }
        let consumed = info.status.wakes() && !j.delivered;
        j.delivered = true;
        Some((info, consumed))
    }

    /// Every job, oldest first, as `check` would show it (without consuming).
    pub fn list(&self) -> Vec<Info> {
        let st = self.state();
        st.jobs
            .iter()
            .map(|j| {
                let mut i = j.info.clone();
                if i.status == Status::Running {
                    i.ms = j.started.elapsed().as_millis() as u64;
                }
                i
            })
            .collect()
    }

    /// Ask a running job to stop; it ends as `cancelled` and wakes nobody.
    pub fn cancel(&self, id: &str) -> Cancel {
        match self.state().jobs.iter().find(|j| j.info.id == id) {
            None => Cancel::Unknown,
            Some(j) if j.info.status == Status::Running => {
                j.cancel.send_replace(true);
                Cancel::Sent
            }
            Some(j) => Cancel::Over(j.info.status),
        }
    }

    /// Whether any job has been started this session.
    pub fn any(&self) -> bool {
        !self.state().jobs.is_empty()
    }

    pub fn running(&self) -> usize {
        self.state().running()
    }

    /// Wait until `f` has an answer, rechecking whenever a job changes.
    async fn until<T>(&self, f: impl Fn(&State) -> Option<T>) -> T {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(v) = f(&self.state()) {
                return v;
            }
            changed.await;
        }
    }

    /// Resolves once an ending is waiting to be delivered.
    pub async fn ready(&self) {
        self.until(|s| (!s.pending().is_empty()).then_some(())).await;
    }

    /// For a run that exits when it is done: true once an ending is waiting,
    /// false once nothing is running and nothing waits.
    pub async fn next(&self) -> bool {
        self.until(|s| {
            if !s.pending().is_empty() {
                Some(true)
            } else if s.running() == 0 {
                Some(false)
            } else {
                None
            }
        })
        .await
    }

    /// Cancel everything and wait for the endings to be journaled.
    pub async fn shutdown(&self) {
        for j in self.state().jobs.iter().filter(|j| j.info.status == Status::Running) {
            j.cancel.send_replace(true);
        }
        self.until(|s| (s.running() == 0).then_some(())).await;
    }
}
