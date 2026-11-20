//! How a tool result is shown: a field of `Event::ToolEnd`, so it lives with
//! the vocabulary that journals it, and tools set it without the event
//! module knowing any tool.

use serde::{Deserialize, Serialize};

/// How the UI renders a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    #[default]
    Plain,
    /// A one-line summary, then a unified diff.
    Diff,
    /// Only the end matters (command output).
    Tail,
}
