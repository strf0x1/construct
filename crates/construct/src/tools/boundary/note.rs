//! What the model reads about a boundary: the note appended to a tool result,
//! the answer a boundary tool returns, and the short note for a late decision
//! pushed over ACP.
//!
//! The design choice: the words are here, once. The tool-result note and the
//! tool answers must agree with the descriptions the model read, so the tool
//! names are the constants from the parent module, not retyped prose.

use super::client::{Entry, Outcome};
use super::{WAIT, WITHDRAW};
use serde::Deserialize;
use std::collections::BTreeMap;

/// A decision Deck pushed over ACP (either boundary method). It informs; it
/// grants nothing and the agent cannot answer it with a decision.
#[derive(Debug, Clone, Deserialize)]
pub struct Notice {
    #[serde(rename = "approval_id", alias = "id")]
    pub id: String,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default, alias = "state")]
    pub outcome: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default, rename = "sessionId", alias = "session_id")]
    pub session: Option<String>,
}

impl Notice {
    pub fn outcome(&self) -> &str {
        self.outcome.as_deref().unwrap_or("decided")
    }
}

/// The note appended to a tool result. `seen` is the set of ids whose terminal
/// state the agent already saw through a tool or an earlier note.
pub fn tool(entries: &[Entry], seen: impl Fn(&str) -> bool) -> Option<String> {
    let mut latest: BTreeMap<&str, &Entry> = BTreeMap::new();
    for e in entries {
        if seen(&e.approval_id) {
            continue;
        }
        match latest.get(e.approval_id.as_str()) {
            Some(prev) if prev.seq > e.seq => {}
            _ => {
                latest.insert(&e.approval_id, e);
            }
        }
    }
    if latest.is_empty() {
        return None;
    }
    let lines: Vec<String> = latest.values().map(|e| describe(e)).collect();
    let guidance = format!(
        "This is the user's policy, not a network failure; do not look for another route to the same resource. Call {WAIT} if the task needs it, or carry on and {WITHDRAW} if it does not."
    );
    if lines.len() == 1 {
        Some(format!("[deck boundary] blocked while this ran: {}. {guidance}", lines[0]))
    } else {
        Some(format!("[deck boundary] blocked while this ran:\n- {}\n{guidance}", lines.join("\n- ")))
    }
}

fn describe(e: &Entry) -> String {
    let dest = e.destination.as_deref().unwrap_or("an unnamed destination");
    let mut s = format!("{dest} (approval {}, {}", e.approval_id, state_words(e.state()));
    if let Some(scope) = &e.scope {
        s.push_str(&format!("; scope: {scope}"));
    }
    s.push(')');
    s
}

fn state_words(state: &str) -> String {
    match state {
        "pending" => "pending with the user".into(),
        "allowed" => "allowed".into(),
        "denied" => "denied".into(),
        "expired" => "expired".into(),
        "withdrawn" => "withdrawn".into(),
        other => other.to_string(),
    }
}

/// The answer a status call returns.
pub fn status_text(id: &str, out: &Outcome) -> String {
    let state = if out.outcome.is_empty() { "unknown".to_string() } else { state_words(&out.outcome) };
    let mut text = format!("{state}: approval {id}");
    if let Some(scope) = &out.scope {
        text.push_str(&format!(", scope {scope}"));
    }
    if let Some(dest) = &out.destination {
        text.push_str(&format!(", for {dest}"));
    }
    text
}

/// The short note for one or more late decisions.
pub fn notifications(notices: &[Notice]) -> String {
    let lines: Vec<String> = notices
        .iter()
        .map(|n| {
            let dest = n.destination.as_deref().unwrap_or("an unnamed destination");
            let mut s = format!("{dest} (approval {}) is now {}", n.id, n.outcome());
            if let Some(scope) = &n.scope {
                s.push_str(&format!(" (scope: {scope})"));
            }
            s
        })
        .collect();
    let guidance = "This updates an earlier denial; it grants nothing. Retry only if the task needs it.";
    if lines.len() == 1 {
        format!("[deck boundary] {}. {guidance}", lines[0])
    } else {
        format!("[deck boundary] decisions:\n- {}\n{guidance}", lines.join("\n- "))
    }
}
