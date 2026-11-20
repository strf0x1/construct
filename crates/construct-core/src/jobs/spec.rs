//! What a kind of job hands the registry: a `Spec` (what it is and the body
//! that does the work), and what the body is given (`Run`) and answers
//! (`Finish`).

use super::{Kind, Status, Tail};
use crate::event::{Event, Events};
use futures::future::BoxFuture;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// What a body is handed when it starts.
pub struct Run {
    /// Where it writes its output as it goes.
    pub tail: Tail,
    /// Goes true when the job is cancelled.
    pub cancel: watch::Receiver<bool>,
    /// How long it may run.
    pub timeout: Duration,
}

/// How a body ended.
pub struct Finish {
    pub status: Status,
    pub exit: Option<i32>,
    /// Appended to the output (what the sandbox helper stopped, a spawn error).
    pub note: String,
}

/// Where a job's rows go: the session's journal, which writes each row and
/// then sends it. A trait here so `jobs` does not depend on `journal`; the
/// journal's `Writer` implements it.
pub trait Sink: Send + Sync {
    fn emit(&self, ev: Event, tx: &Events);
}

pub type Body = Box<dyn FnOnce(Run) -> BoxFuture<'static, Finish> + Send>;

pub struct Spec {
    pub kind: Kind,
    pub label: String,
    /// The tool call that started it.
    pub call_id: String,
    pub timeout: Duration,
    /// The session's journal and event stream, which its rows go to.
    pub sink: (Arc<dyn Sink>, Events),
    pub body: Body,
}
