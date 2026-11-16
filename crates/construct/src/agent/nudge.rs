//! An answer with no tool call that is not an answer: it stopped at the
//! output limit, it wrote a tool call out as text (the provider failed to
//! parse the model's own call syntax), or it reasoned and then said nothing.
//! Ending the turn there loses the work, so the harness says what happened
//! and asks the model to go on, a bounded number of times a turn. A call
//! written as text is never parsed and run: the formats differ by model, and
//! a guessed call is worse than asking again. The nudge is its own event,
//! never a `User` row.

use super::{Agent, Event, Events};
use crate::llm::{Completion, Message};

/// Nudges a turn gets before such an answer ends it.
const MAX: u32 = 3;
/// How models spell a tool call when it leaks into text.
const CALL_MARKS: &[&str] =
    &["<|tool_call", "<tool_call>", "<function_call>", "<function=", "[TOOL_CALLS]", "<\u{ff5c}tool\u{2581}call"];

/// Why an answer without tool calls should not end the turn, and what to
/// tell the model. None for a real answer.
fn why(c: &Completion) -> Option<(&'static str, &'static str)> {
    let content = c.message.content.as_deref().unwrap_or("").trim();
    let reasoning = c.message.reasoning.as_deref().unwrap_or("");
    let marked = |t: &str| CALL_MARKS.iter().any(|m| t.contains(m));
    if c.finish_reason == "length" {
        Some((
            "length",
            "Your last message was cut off at the output limit before it called a tool, so nothing in it ran. Do not repeat it. Go on from where it stopped: reason briefly, then call a tool or give a short final answer.",
        ))
    } else if marked(content) || (content.is_empty() && marked(reasoning)) {
        Some((
            "text_tool_call",
            "Your last message wrote a tool call out as text, so it did not run. Call the tool through the tool-calling interface, not in your message or your reasoning. If you are finished, give a short final answer.",
        ))
    } else if content.is_empty() && !reasoning.is_empty() {
        Some((
            "empty",
            "Your last message had reasoning but no answer and no tool call. Call a tool to go on, or if you are finished give a short final answer.",
        ))
    } else {
        None
    }
}

impl Agent {
    /// After an answer with no tool calls: whether the loop should ask the
    /// model again. `used` counts this turn's nudges.
    pub(super) fn nudge(&mut self, c: &Completion, used: &mut u32, tx: &Events) -> bool {
        let Some((why, text)) = why(c) else { return false };
        if *used >= MAX {
            if why == "length" {
                self.journal.emit(Event::Error { message: "the model hit its output limit mid-answer".into() }, tx);
            }
            return false;
        }
        *used += 1;
        let message = Message::user(format!("<system-reminder>\n{text}\n</system-reminder>"));
        self.journal.emit(Event::Nudge { why: why.into(), message: message.clone() }, tx);
        self.context.push(message);
        true
    }
}
