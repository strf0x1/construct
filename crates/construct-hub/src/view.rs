//! What `construct hub session <peer>` shows (#362): the peer's session as
//! the hub holds it, with the transcript rendered from its `session/update`s
//! and the permission questions waiting for a human.
//!
//! The design choice: the hub keeps the peer's raw updates and renders them
//! only when asked, so a reattach that replays the session (ADR 0033) simply
//! replaces what is kept and the rendering never has to merge two views.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::fmt::Write as _;

/// One peer's session, for the control socket and the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionView {
    pub peer: String,
    /// Whether an ACP attachment is live on the peer's link.
    pub attached: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The working directory the peer reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The last turn report: `{sessionId, turn, state, stopReason?}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<Value>,
    /// The prompt in flight, until its answer arrives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// The idempotency key of the prompt in flight, so a client that sent
    /// it can tell its own completion from a later prompt's (#366).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// How the last answered prompt ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
    /// The last answered prompt: its key, how it ended and what the peer's
    /// agent said in reply (#366).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered: Option<Answered>,
    /// Why the session could not be opened, if it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub transcript: String,
    pub questions: Vec<Question>,
}

/// A prompt the peer has answered, as the `peer` tool reports it (#366).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answered {
    /// The idempotency key it was sent with.
    pub key: String,
    /// `end_turn`, `cancelled`, `error: …`.
    pub stop: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    /// The agent's words after the prompt, tool lines left out.
    pub reply: String,
}

/// A permission question from the peer, waiting for `construct hub answer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub title: String,
    /// The option ids the peer offers (`once`, `session`, `deny`, …).
    pub options: Vec<String>,
    /// The peer's wording for each option, in the same order (#365).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

/// The option ids a `session/request_permission` offers.
pub(crate) fn options(params: &Value) -> Vec<String> {
    params["options"]
        .as_array()
        .map(|o| o.iter().filter_map(|o| o["optionId"].as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

pub(crate) fn question(params: &Value) -> Question {
    Question {
        title: params["toolCall"]["title"].as_str().unwrap_or("permission").to_string(),
        options: options(params),
        labels: params["options"]
            .as_array()
            .map(|o| o.iter().map(|o| o["name"].as_str().unwrap_or_default().to_string()).collect())
            .unwrap_or_default(),
    }
}

enum Part {
    Said { user: bool, text: String },
    Tool { id: String, title: String, status: String },
}

/// Keep one transcript update; the oldest drops past `cap`.
pub(crate) fn keep(updates: &mut VecDeque<Value>, update: Value, cap: usize) {
    if updates.len() == cap {
        updates.pop_front();
    }
    updates.push_back(update);
}

/// Show a prompt the hub sent: ACP agents do not echo a live prompt back,
/// only a `session/load` replays it, so a resend after one may find it
/// already shown as the latest user message.
pub(crate) fn show_prompt(updates: &mut VecDeque<Value>, text: &str, cap: usize) {
    let (mut last, mut open) = (String::new(), false);
    for u in updates.iter() {
        if u["sessionUpdate"] == "user_message_chunk" {
            if !open {
                last.clear();
            }
            last.push_str(u["content"]["text"].as_str().unwrap_or_default());
        }
        open = u["sessionUpdate"] == "user_message_chunk";
    }
    if last != text {
        let said = json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": text } });
        keep(updates, said, cap);
    }
}

/// What the agent said after the latest user message: its reply to the
/// prompt that turn answered.
pub(crate) fn reply<'a>(updates: impl DoubleEndedIterator<Item = &'a Value>) -> String {
    let mut said: Vec<&str> = updates
        .rev()
        .take_while(|u| u["sessionUpdate"] != "user_message_chunk")
        .filter(|u| u["sessionUpdate"] == "agent_message_chunk")
        .filter_map(|u| u["content"]["text"].as_str())
        .collect();
    said.reverse();
    said.concat()
}

/// The transcript as text: user turns as `> …`, the agent's words as they
/// streamed, and one `[status] title` line per tool call at its last status.
pub(crate) fn transcript<'a>(updates: impl Iterator<Item = &'a Value>) -> String {
    let mut parts: Vec<Part> = vec![];
    for u in updates {
        let text = || u["content"]["text"].as_str().unwrap_or_default().to_string();
        let str_of = |k: &str| u[k].as_str().unwrap_or_default().to_string();
        match u["sessionUpdate"].as_str().unwrap_or_default() {
            kind @ ("user_message_chunk" | "agent_message_chunk") => {
                let user = kind == "user_message_chunk";
                match parts.last_mut() {
                    Some(Part::Said { user: u2, text: t }) if *u2 == user => t.push_str(&text()),
                    _ => parts.push(Part::Said { user, text: text() }),
                }
            }
            "tool_call" | "tool_call_update" => {
                let id = str_of("toolCallId");
                let known = parts.iter_mut().find_map(|p| match p {
                    Part::Tool { id: i, title, status } if *i == id => Some((title, status)),
                    _ => None,
                });
                match known {
                    Some((title, status)) => {
                        if u["title"].is_string() {
                            *title = str_of("title");
                        }
                        if u["status"].is_string() {
                            *status = str_of("status");
                        }
                    }
                    None => parts.push(Part::Tool { id, title: str_of("title"), status: str_of("status") }),
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let _ = match p {
            Part::Said { user: true, text } => write!(out, "> {}", text.trim_end()),
            Part::Said { user: false, text } => write!(out, "{text}"),
            Part::Tool { title, status, .. } => write!(out, "[{status}] {title}"),
        };
    }
    out
}

impl SessionView {
    /// Human-readable lines for `construct hub session`.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let link = if self.attached { "attached" } else { "not attached" };
        let _ = writeln!(out, "{}: {link}, session {}", self.peer, self.session.as_deref().unwrap_or("none yet"));
        if let Some(t) = &self.turn {
            let reason = t["stopReason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default();
            let _ = writeln!(out, "turn {}: {}{reason}", t["turn"], t["state"].as_str().unwrap_or("?"));
        }
        if let Some(p) = &self.prompt {
            let _ = writeln!(out, "running: {p:?}");
        }
        if let Some(e) = &self.error {
            let _ = writeln!(out, "error: {e}");
        }
        if !self.transcript.is_empty() {
            let _ = writeln!(out, "\n{}\n", self.transcript);
        }
        for (i, q) in self.questions.iter().enumerate() {
            let _ = writeln!(
                out,
                "question {}: {}  `construct hub answer {} <{}>`",
                i + 1,
                q.title,
                self.peer,
                q.options.join("|")
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_transcript_merges_chunks_and_shows_each_tool_at_its_last_status() {
        let u = [
            json!({ "sessionUpdate": "user_message_chunk", "content": { "text": "read it" } }),
            json!({ "sessionUpdate": "agent_message_chunk", "content": { "text": "on " } }),
            json!({ "sessionUpdate": "agent_message_chunk", "content": { "text": "it" } }),
            json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "read a.txt", "status": "in_progress" }),
            json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed" }),
            json!({ "sessionUpdate": "agent_message_chunk", "content": { "text": "done" } }),
        ];
        assert_eq!(transcript(u.iter()), "> read it\non it\n[completed] read a.txt\ndone");
        assert_eq!(reply(u.iter()), "on itdone", "the reply is the agent's words after the prompt");
    }
}
