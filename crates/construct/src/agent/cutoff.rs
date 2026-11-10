//! An answer that stops at the model's output limit before any tool call.
//! Ending the turn there loses the work: a model that reasons at length runs
//! out of output with nothing done. So the harness says what happened and
//! asks it to go on, a bounded number of times a turn, and only then gives
//! up with the error. The nudge is its own event, never a `User` row.

use super::{Agent, Event, Events};
use crate::llm::Message;

/// Nudges a turn gets before a cut-off answer ends it.
const MAX: u32 = 3;
const NUDGE: &str = "<system-reminder>\nYour last message was cut off at the output limit before it called a tool, so nothing in it ran. Do not repeat it. Go on from where it stopped: reason briefly, then call a tool or give a short final answer.\n</system-reminder>";

impl Agent {
    /// After an answer with no tool calls: whether the loop should ask the
    /// model again. `used` counts this turn's nudges.
    pub(super) fn go_on_after_cutoff(&mut self, finish_reason: &str, used: &mut u32, tx: &Events) -> bool {
        if finish_reason != "length" {
            return false;
        }
        if *used >= MAX {
            self.journal.emit(Event::Error { message: "the model hit its output limit mid-answer".into() }, tx);
            return false;
        }
        *used += 1;
        let message = Message::user(NUDGE);
        self.journal.emit(Event::Cutoff { message: message.clone() }, tx);
        self.context.push(message);
        true
    }
}
