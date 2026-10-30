//! What a kind of job hands the registry: a `Spec` (what it is and the body
//! that does the work), and what the body is given (`Run`) and answers
//! (`Finish`).

use super::{Kind, Status, Tail};
use crate::event::Events;
use crate::journal::Writer;
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

pub type Body = Box<dyn FnOnce(Run) -> BoxFuture<'static, Finish> + Send>;

pub struct Spec {
    pub kind: Kind,
    pub label: String,
    /// The tool call that started it.
    pub call_id: String,
    pub timeout: Duration,
    /// The session's journal and event stream, which its rows go to.
    pub sink: (Arc<Writer>, Events),
    pub body: Body,
}
