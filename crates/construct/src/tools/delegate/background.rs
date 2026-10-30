//! A delegation as a background job (ADR 0026, #34): `delegate` with
//! `background = true` starts the child as a job and returns its id, and the
//! child's summary is the job's completion.
//!
//! The design choice: the job owns the child. Its cancel switch is the job's,
//! not the turn's, so interrupting the parent's turn leaves it running, while
//! `job cancel` and quitting stop it. The child's events are still journaled
//! under its own session, through a `Call` built from the session's sink, and
//! its end maps onto a job status (a timeout is `timeout`, an interrupt is
//! `cancelled`), so only the endings ADR 0026 lists wake the agent.

use super::{Prepared, cap, pointer};
use crate::agents::AgentDef;
use crate::child::run::{Outcome, Spec, Stop, run};
use crate::config::Config;
use crate::event::Events;
use crate::jobs::{self, Finish, Kind, Status};
use crate::journal::Writer;
use crate::permission::Perms;
use crate::sandbox::Policy;
use crate::tools::{Call, Ctx, Output, ToolError};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// One of an agent's `max_background` places, held until its job ends.
pub(super) struct Slot(Arc<AtomicUsize>);

impl Slot {
    /// A place if fewer than `max` are held.
    fn take(running: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        let mut held = running.load(Ordering::SeqCst);
        loop {
            if held >= max {
                return None;
            }
            match running.compare_exchange_weak(held, held + 1, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return Some(Self(running.clone())),
                Err(now) => held = now,
            }
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// What must be true before any sandbox or scratch directory is made: there
/// is a session to report to, and this agent has a place free.
pub(super) struct Reserved {
    slot: Slot,
    sink: (Arc<Writer>, Events),
}

pub(super) fn reserve(
    call: &Call,
    name: &str,
    def: &AgentDef,
    running: &Arc<AtomicUsize>,
) -> Result<Reserved, ToolError> {
    let Some(sink) = call.sink() else {
        return Err(ToolError::new("no_session", "background jobs need a session to report to"));
    };
    let max = def.max_background;
    let Some(slot) = Slot::take(running, max) else {
        let why = if max == 0 {
            format!("agent {name} is not set up to run in the background (max_background = 0)")
        } else {
            format!(
                "{max} background runs of agent {name} are already going, which is its limit. Wait for one to end, or cancel one with the job tool."
            )
        };
        return Err(ToolError::new("job_limit", why));
    };
    Ok(Reserved { slot, sink })
}

/// What the child needs, owned: the job outlives the call that built it.
pub(super) struct Launch<'a> {
    pub name: &'a str,
    pub task: &'a str,
    pub prepared: &'a Prepared,
    pub parent: &'a Config,
    pub policy: Policy,
    pub cwd: PathBuf,
}

struct Owned {
    name: String,
    task: String,
    def: AgentDef,
    config: Config,
    system: String,
    parent: Config,
    policy: Policy,
    cwd: PathBuf,
    perms: Arc<Perms>,
    call_id: String,
}

pub(super) fn start(ctx: &Ctx, call: &Call, reserved: Reserved, l: Launch<'_>) -> Result<Output, ToolError> {
    let Reserved { slot, sink } = reserved;
    let (name, def) = (l.name, &l.prepared.def);
    let tmp = l.policy.tmp.clone();
    let owned = Owned {
        name: name.to_string(),
        task: l.task.to_string(),
        def: def.clone(),
        config: l.prepared.config.clone(),
        system: l.prepared.system.clone(),
        parent: l.parent.clone(),
        policy: l.policy,
        cwd: l.cwd,
        perms: ctx.perms.clone(),
        call_id: call.id.clone(),
    };
    let secs = def.timeout;
    let body_sink = sink.clone();
    let id = ctx
        .jobs
        .start(jobs::Spec {
            kind: Kind::Delegate,
            label: format!("{name}: {}", l.task),
            call_id: call.id.clone(),
            timeout: Duration::from_secs(secs),
            sink,
            body: Box::new(move |job| Box::pin(execute(owned, slot, body_sink, job))),
        })
        .map_err(|e| {
            // The body never ran, so nothing else removes the scratch directory.
            let _ = std::fs::remove_dir_all(&tmp);
            ToolError::new("job_limit", e)
        })?;
    let text = format!(
        "started job {id}: agent {name} is working on it in the background (time limit {secs}s). You will be told when it ends (completed, failed or timed out), with its summary, so don't poll it. The job tool reads it sooner or cancels it."
    );
    Ok(Output::plain(text).with_data(json!({ "job": id, "agent": name })))
}

/// Run the child to its end. The job's own cancel switch stands in for the
/// turn's, and the child's own deadline is the job's time limit.
async fn execute(o: Owned, _slot: Slot, sink: (Arc<Writer>, Events), job: jobs::Run) -> Finish {
    let call = Call::new(&o.call_id, sink.0, sink.1);
    let spec = Spec {
        name: &o.name,
        def: &o.def,
        config: o.config.clone(),
        system: o.system.clone(),
        policy: o.policy.clone(),
        cwd: &o.cwd,
        parent: &o.parent,
        perms: o.perms.clone(),
    };
    match run(spec, &o.task, &call, job.cancel).await {
        Ok(out) => finish(&o.name, &o.def, &out),
        Err(e) => {
            Finish { status: Status::Failed, exit: None, note: format!("[starting agent {} failed: {e}]", o.name) }
        }
    }
}

/// How the child ended, as the job's status and the text the model reads: the
/// summary (capped as a foreground one is) and the pointer to its transcript.
fn finish(name: &str, def: &AgentDef, out: &Outcome) -> Finish {
    let (summary, _) = cap(&out.summary);
    let answer = summary.trim();
    let (status, head) = match (out.stopped, &out.error) {
        (Some(Stop::Timeout), _) => {
            (Status::Timeout, Some(format!("[agent {name} timed out after {}s. Its last answer:]", def.timeout)))
        }
        (Some(Stop::Interrupted), _) => {
            (Status::Cancelled, Some(format!("[agent {name} was cancelled. Its last answer:]")))
        }
        (None, Some(e)) => (Status::Failed, Some(format!("[agent {name} failed: {e}]"))),
        (None, None) if answer.is_empty() => {
            (Status::Failed, Some(format!("[agent {name} finished without an answer]")))
        }
        (None, None) => (Status::Completed, None),
    };
    // A failure has no answer worth the model's tokens; a stop's partial one is.
    let answer = if status == Status::Failed { "" } else { answer };
    let note = [head.unwrap_or_default(), answer.to_string(), pointer(out).trim().to_string()]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Finish { status, exit: None, note }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::delegate::SUMMARY_MAX;

    fn def() -> AgentDef {
        AgentDef { timeout: 30, ..Default::default() }
    }

    fn done(summary: &str) -> Outcome {
        Outcome { summary: summary.into(), session: Some("s1".into()), finished: true, ..Default::default() }
    }

    #[test]
    fn a_childs_end_maps_onto_a_job_status() {
        let status = |o: Outcome| finish("scout", &def(), &o).status;
        assert_eq!(status(done("found it")), Status::Completed);
        assert_eq!(status(done("  ")), Status::Failed);
        assert_eq!(status(Outcome { error: Some("boom".into()), ..done("") }), Status::Failed);
        assert_eq!(status(Outcome { stopped: Some(Stop::Timeout), ..done("half") }), Status::Timeout);
        assert_eq!(status(Outcome { stopped: Some(Stop::Interrupted), ..done("half") }), Status::Cancelled);
    }

    #[test]
    fn the_completion_is_the_summary_then_the_pointer() {
        let f = finish("scout", &def(), &done("found it"));
        assert_eq!(f.note, "found it\n[full transcript: journal session s1]");
        let f = finish("scout", &def(), &Outcome { stopped: Some(Stop::Timeout), ..done("half") });
        assert!(f.note.starts_with("[agent scout timed out after 30s. Its last answer:]\nhalf\n"), "{}", f.note);
    }

    #[test]
    fn a_long_summary_is_cut_like_a_foreground_one() {
        let f = finish("scout", &def(), &done(&"x".repeat(SUMMARY_MAX * 2)));
        assert!(f.note.contains("summary cut at") && f.note.len() < SUMMARY_MAX + 200);
    }

    #[test]
    fn a_place_is_held_until_its_job_ends_and_zero_means_never() {
        let running = Arc::new(AtomicUsize::new(0));
        let first = Slot::take(&running, 2).unwrap();
        let _second = Slot::take(&running, 2).unwrap();
        assert!(Slot::take(&running, 2).is_none());
        drop(first);
        assert!(Slot::take(&running, 2).is_some());
        assert!(Slot::take(&Arc::default(), 0).is_none());
        // Nowhere to report to is refused before a place is taken.
        let none = Arc::new(AtomicUsize::new(0));
        let refused = reserve(&Call::detached("c"), "scout", &def(), &none).err().map(|e| e.code);
        assert_eq!((refused, none.load(Ordering::SeqCst)), (Some("no_session"), 0));
    }
}
