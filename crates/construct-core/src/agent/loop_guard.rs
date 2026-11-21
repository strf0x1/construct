//! The same tool calls round after round: a model that has stopped making
//! progress and will run to `max_steps` unless something says so. After
//! `max_repeats` identical rounds in a row the model is told, once, with the
//! call and its last result; a further identical round, or a second run of
//! repeats later in the same turn, ends the turn with an error the user can
//! act on (#232).
//!
//! The one choice a reader must know: a round is identical by its calls'
//! names and canonical arguments (keys sorted, calls sorted), never by ids or
//! results, so the order a model lists calls in or spaces its JSON cannot
//! hide a loop, and a call whose result changes (a poll) still counts. A
//! round with no tool calls resets the run.

use super::{Agent, Event, Events};
use crate::llm::{Message, ToolCall};
use crate::text;
use serde_json::Value;

/// Bytes of the last result quoted back in the reminder.
const QUOTE: usize = 600;

/// What the guard says after a round.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Go,
    /// The run just reached the threshold: tell the model.
    Warn,
    /// Told once already this turn and it repeated again: end the turn.
    Stop,
}

/// One turn's watch over consecutive tool rounds.
#[derive(Debug, Default)]
pub(super) struct Guard {
    last: Option<String>,
    run: u32,
    warned: bool,
}

impl Guard {
    /// Record a round. `threshold` 0 turns the guard off.
    pub(super) fn observe(&mut self, calls: &[ToolCall], threshold: u32) -> Verdict {
        if threshold == 0 || calls.is_empty() {
            self.last = None;
            self.run = 0;
            return Verdict::Go;
        }
        let key = round_key(calls);
        if self.last.as_deref() == Some(key.as_str()) {
            self.run += 1;
        } else {
            self.last = Some(key);
            self.run = 1;
        }
        match self.run {
            r if r < threshold => Verdict::Go,
            r if r == threshold && !self.warned => {
                self.warned = true;
                Verdict::Warn
            }
            _ => Verdict::Stop,
        }
    }

    pub(super) fn run(&self) -> u32 {
        self.run
    }
}

/// A round's calls as one comparable string: each call's name and
/// canonical arguments, sorted.
fn round_key(calls: &[ToolCall]) -> String {
    let mut parts: Vec<String> =
        calls.iter().map(|c| format!("{}\u{0}{}", c.function.name, canonical(&c.function.arguments))).collect();
    parts.sort();
    parts.join("\u{1}")
}

/// Arguments with object keys sorted at every depth; text that is not JSON
/// compares as written.
fn canonical(args: &str) -> String {
    fn sort(v: Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut kv: Vec<(String, Value)> = m.into_iter().map(|(k, v)| (k, sort(v))).collect();
                kv.sort_by(|a, b| a.0.cmp(&b.0));
                Value::Object(kv.into_iter().collect())
            }
            Value::Array(a) => Value::Array(a.into_iter().map(sort).collect()),
            v => v,
        }
    }
    serde_json::from_str::<Value>(args).map_or_else(|_| args.trim().to_string(), |v| sort(v).to_string())
}

impl Agent {
    /// After a round of tool calls: whether the turn goes on. A warning is a
    /// `Nudge` (`why`: `tool_loop`), so it is journaled and replays on resume
    /// like the others.
    pub(super) fn guard_loop(&mut self, guard: &mut Guard, calls: &[ToolCall], tx: &Events) -> bool {
        match guard.observe(calls, self.cfg.max_repeats) {
            Verdict::Go => true,
            Verdict::Warn => {
                let text = self.loop_reminder(calls, guard.run());
                let message = Message::user(format!("<system-reminder>\n{text}\n</system-reminder>"));
                self.journal.emit(Event::Nudge { why: "tool_loop".into(), message: message.clone() }, tx);
                self.context.push(message);
                true
            }
            Verdict::Stop => {
                let names: Vec<&str> = calls.iter().map(|c| c.function.name.as_str()).collect();
                self.journal.emit(
                    Event::Error {
                        message: format!(
                            "stopped: the model kept repeating the same tool call ({}) after being told (max_repeats)",
                            names.join(", ")
                        ),
                    },
                    tx,
                );
                false
            }
        }
    }

    /// The reminder for a round repeated `n` times: what was called, with
    /// what, and what it last returned.
    fn loop_reminder(&self, calls: &[ToolCall], n: u32) -> String {
        let mut lines = vec![format!(
            "You have made the same tool call{} {n} times in a row with the same arguments, and the result is not changing:",
            if calls.len() == 1 { "" } else { "s" }
        )];
        let log = self.context.log();
        for c in calls {
            let result = log
                .iter()
                .rev()
                .find(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some(c.id.as_str()))
                .and_then(|m| m.content.as_deref())
                .unwrap_or("");
            lines.push(format!(
                "- {} {}\n  last result: {}",
                c.function.name,
                text::truncate(&c.function.arguments, QUOTE),
                text::truncate(result, QUOTE)
            ));
        }
        lines.push(
            "Do not make this call again with these arguments in this turn. Try something different: change the approach, read something you have not read, or if you are stuck, stop and tell the user what is blocking you.".into(),
        );
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::FunctionCall;

    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall { name: name.into(), arguments: args.into() },
            extra_content: None,
        }
    }

    #[test]
    fn warns_at_the_threshold_then_stops_on_the_next_repeat() {
        let mut g = Guard::default();
        let round = |i: u32| vec![call(&format!("c{i}"), "read", r#"{"path":"a"}"#)];
        let verdicts: Vec<Verdict> = (0..6).map(|i| g.observe(&round(i), 5)).collect();
        assert_eq!(verdicts, [Verdict::Go, Verdict::Go, Verdict::Go, Verdict::Go, Verdict::Warn, Verdict::Stop]);
    }

    #[test]
    fn key_order_call_order_and_whitespace_do_not_hide_a_loop() {
        let mut g = Guard::default();
        let a = [call("1", "grep", r#"{"pattern":"x","path":"."}"#), call("2", "read", r#"{"path":"a"}"#)];
        let b = [call("3", "read", r#"{ "path" : "a" }"#), call("4", "grep", r#"{"path":".","pattern":"x"}"#)];
        assert_eq!(g.observe(&a, 2), Verdict::Go);
        assert_eq!(g.observe(&b, 2), Verdict::Warn);
    }

    #[test]
    fn a_different_round_or_a_text_answer_resets_the_run() {
        let mut g = Guard::default();
        let same = [call("1", "read", r#"{"path":"a"}"#)];
        let other = [call("2", "read", r#"{"path":"b"}"#)];
        for _ in 0..2 {
            assert_eq!(g.observe(&same, 3), Verdict::Go);
        }
        assert_eq!(g.observe(&other, 3), Verdict::Go);
        assert_eq!(g.observe(&same, 3), Verdict::Go);
        assert_eq!(g.observe(&[], 3), Verdict::Go);
        assert_eq!(g.observe(&same, 3), Verdict::Go);
        assert_eq!(g.observe(&same, 3), Verdict::Go);
        assert_eq!(g.observe(&same, 3), Verdict::Warn);
    }

    #[test]
    fn a_second_loop_in_the_same_turn_stops_without_a_second_warning() {
        let mut g = Guard::default();
        let a = [call("1", "read", r#"{"path":"a"}"#)];
        let b = [call("2", "read", r#"{"path":"b"}"#)];
        assert_eq!(g.observe(&a, 2), Verdict::Go);
        assert_eq!(g.observe(&a, 2), Verdict::Warn);
        assert_eq!(g.observe(&b, 2), Verdict::Go);
        assert_eq!(g.observe(&b, 2), Verdict::Stop);
    }

    #[test]
    fn zero_turns_it_off() {
        let mut g = Guard::default();
        let a = [call("1", "read", r#"{"path":"a"}"#)];
        assert!((0..10).all(|_| g.observe(&a, 0) == Verdict::Go));
    }
}
