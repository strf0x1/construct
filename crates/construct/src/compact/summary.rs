//! The summarizer's request: a fixed system prompt and a transcript of the
//! rounds being replaced. The transcript always leads with the previous
//! summary, if any, so a second cut builds on the first instead of losing it;
//! the rest is filled newest first within the summarizer's own budget.

use super::{SUMMARY_HEAD, is_summary};
use crate::llm::{Message, truncate};
use std::fmt::Write;

pub const SYSTEM: &str = "You compact an agent's working context. The agent continues the task from your summary alone: the messages you are shown are removed from its context. Write these sections in Markdown, dense, no preamble:\n\
## Goal\nThe user's request and what counts as done. Copy every constraint and exact value the user gave (names, versions, numbers, paths, flags) verbatim.\n\
## Progress\nWhat has been done, in order. Every file created or changed, and its current state.\n\
## Facts\nWhat the agent found and will need again: exact identifiers, values, error messages, commands that worked, file locations and line numbers.\n\
## Open\nWhat failed or is unresolved, and why.\n\
## Next\nThe immediate next steps.\n\
If the transcript starts with an earlier summary, carry everything in it forward into yours; it is the only record of that work.";

/// Per-message caps: tool output is where the facts are, so it gets the most.
const TEXT_MAX: usize = 4000;
const ARGS_MAX: usize = 1000;
const RESULT_MAX: usize = 2000;

/// The user message for the summarizer over `msgs` (the rounds being
/// replaced), at most `max_chars`.
pub fn request(msgs: &[Message], max_chars: usize) -> String {
    let (prior, rest) = match msgs.first() {
        Some(m) if is_summary(m) => {
            let c = m.content.as_deref().unwrap_or("");
            (Some(format!("EARLIER SUMMARY:\n{}", c.trim_start_matches(SUMMARY_HEAD))), &msgs[1..])
        }
        _ => (None, msgs),
    };
    let mut budget = max_chars.saturating_sub(prior.as_ref().map_or(0, String::len));
    let mut parts: Vec<String> = vec![];
    let mut omitted = 0;
    for m in rest.iter().rev() {
        let p = render(m);
        if p.is_empty() {
            continue;
        }
        if p.len() + 2 > budget {
            omitted = rest.len() - parts.len();
            break;
        }
        budget -= p.len() + 2;
        parts.push(p);
    }
    if omitted > 0 {
        parts.push(format!("[… {omitted} earlier messages omitted …]"));
    }
    parts.extend(prior);
    parts.reverse();
    parts.join("\n\n")
}

fn render(m: &Message) -> String {
    let mut s = String::new();
    match m.role.as_str() {
        "user" => {
            let _ = write!(s, "USER: {}", truncate(m.content.as_deref().unwrap_or(""), TEXT_MAX));
        }
        "assistant" => {
            if let Some(c) = m.content.as_deref().filter(|c| !c.trim().is_empty()) {
                let _ = write!(s, "ASSISTANT: {}", truncate(c, TEXT_MAX));
            }
            for c in &m.tool_calls {
                let _ = write!(s, "\nCALL {} {}: {}", c.id, c.function.name, truncate(&c.function.arguments, ARGS_MAX));
            }
        }
        "tool" => {
            let id = m.tool_call_id.as_deref().unwrap_or("");
            let _ = write!(s, "RESULT {id}: {}", truncate(m.content.as_deref().unwrap_or(""), RESULT_MAX));
        }
        _ => {}
    }
    s.trim().to_string()
}
