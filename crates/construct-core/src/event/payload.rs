//! Payloads that rows and events carry, as plain data: the job rows
//! (`job_start`, `job_end`, `jobs_delivered`) and what a compaction cut did to
//! the view. They live here so `event` depends on nothing in `jobs` or
//! `compact`; the behavior that goes with them (`Status::wakes`,
//! `Info::report`, planning and applying cuts) stays in those modules, where
//! ADR 0026 lists the wake sources.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Shell,
    /// A delegated child: only in journals from before delegation was
    /// removed (#294), kept so their job rows still read.
    Delegate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    /// The previous process stopped before recording a completion.
    Interrupted,
    /// Ended on its own and well: a command that exited 0.
    Completed,
    /// Ended on its own and badly: a nonzero exit.
    Failed,
    /// Stopped at its time limit.
    Timeout,
    /// Stopped by the model, the user or quitting.
    Cancelled,
}

/// A job at one moment, and the payload of its `job_end` row. It may still be
/// running (`check`); a row is written only once it is not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub id: String,
    pub kind: Kind,
    /// The command: what the job is.
    pub label: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    pub ms: u64,
    pub tail: String,
}

/// The payload of a `job_start` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Started {
    pub id: String,
    pub kind: Kind,
    pub label: String,
    /// The tool call that made it.
    pub call_id: String,
}

/// The payload of a `jobs_delivered` row: the model has these endings, either
/// because they rode in at a turn boundary (`via` "turn", with the `message`)
/// or because it read them itself (`via` "check"). What the journal says is
/// unconsumed is every wake-worthy ending no such row names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delivered {
    pub ids: Vec<String>,
    pub via: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<crate::llm::Message>,
}

/// What a cut did to the view, enough to replay it on the same messages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Cut {
    /// Call ids whose results became stubs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elide: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    /// Messages `1..upto` (after eliding) were replaced by the summary.
    pub upto: usize,
    pub text: String,
    /// The user's latest request, when it was among the replaced messages:
    /// pinned verbatim under the summary rather than paraphrased by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
}
