//! Idempotent prompts (contract 1.3.0 §12, docs/adr/0045): a `session/prompt`
//! that carries a key already on record starts no turn. It is answered with
//! the outcome of the prompt that first carried that key.
//!
//! The design choice to know: each session keeps its own book of keys,
//! seeded from the journal when a session is registered and advanced by the
//! events the actor already relays, so a lookup never waits on the agent lock
//! that a running turn holds. A prompt counts as running from the moment it
//! leaves the queue, before its `Submitted` row exists, so a resend in that
//! gap waits instead of running twice.

use super::map;
use super::rpc;
use crate::event::{Event, Submission};
use serde_json::{Value, json};
use std::collections::HashMap;

use map::{IDEMPOTENCY_CONFLICT as CONFLICT, IDEMPOTENCY_KEY as FIELD, IDEMPOTENCY_KEY_MAX as MAX_CHARS};

/// What a turn answered: a stop reason, or the error its request got.
pub(super) type Outcome = Result<&'static str, String>;
/// A request waiting on another prompt's outcome: its id and attachment.
pub(super) type Waiter = (Value, u64);

/// What to do with a prompt whose key may be on record or running.
pub(super) enum Seen {
    New,
    Waits,
    Conflict,
    Answer(Outcome),
}

/// A session's keys and the requests waiting on the turn now running.
#[derive(Default)]
pub(super) struct Keys {
    /// Every journaled key: the prompt text it came with and, once known, its outcome.
    recorded: HashMap<String, (String, Option<Outcome>)>,
    /// Seeding only: the key of the turn being read, and its last error.
    turn: Option<String>,
    error: Option<String>,
    /// The running prompt's key and text, set when it leaves the queue.
    running: Option<(String, String)>,
    waiters: Vec<Waiter>,
}

/// The key a prompt carries. Absent and `null` are no key; anything else
/// that is not 1 to 255 characters is refused, never treated as absent.
pub(super) fn parse(params: &Value) -> Result<Option<String>, String> {
    match &params["_meta"][map::META_KEY][FIELD] {
        Value::Null => Ok(None),
        Value::String(k) if (1..=MAX_CHARS).contains(&k.chars().count()) => Ok(Some(k.clone())),
        _ => Err(format!("{FIELD} must be a string of 1 to {MAX_CHARS} characters")),
    }
}

/// Format a turn outcome as a JSON-RPC reply payload or internal error.
pub(super) fn reply_outcome(outcome: Outcome) -> Result<Value, (i64, String)> {
    outcome.map(|reason| json!({ "stopReason": reason })).map_err(|e| (rpc::INTERNAL, e))
}

/// Format a reused-key conflict as a JSON-RPC error line.
pub(super) fn conflict_error(id: &Value, key: &str) -> String {
    let message = format!("{FIELD} {key:?} already names a different prompt");
    rpc::err_data(id, rpc::INVALID_PARAMS, &message, json!({ "reason": CONFLICT }))
}

impl Keys {
    /// Advance with one journaled event. A turn's outcome is read the way the
    /// server derives a loaded session's last stop reason; a live turn's is
    /// replaced by its actual result in `finish`.
    pub fn observe(&mut self, event: &Event) {
        match event {
            Event::TurnStart => (self.turn, self.error) = (None, None),
            Event::Submitted(Submission { text, idempotency_key: Some(k) }) => {
                self.recorded.entry(k.clone()).or_insert((text.clone(), None));
                self.turn = Some(k.clone());
            }
            Event::Error { message } => self.error = Some(message.clone()),
            Event::TurnEnd { interrupted, .. } => {
                let outcome = match (interrupted, self.error.take()) {
                    (true, _) => Ok("cancelled"),
                    (false, Some(m)) if m.ends_with("(max_steps)") => Ok("max_turn_requests"),
                    (false, Some(m)) => Err(m),
                    (false, None) => Ok("end_turn"),
                };
                self.settle_turn(outcome);
            }
            Event::Recovered { .. } => self.settle_turn(Ok("cancelled")),
            _ => {}
        }
    }

    fn settle_turn(&mut self, outcome: Outcome) {
        if let Some(k) = self.turn.take()
            && let Some((_, slot)) = self.recorded.get_mut(&k)
        {
            slot.get_or_insert(outcome);
        }
    }

    /// A prompt left the queue; its waiters now wait on the running turn.
    pub fn start(&mut self, key: Option<String>, text: String, waiters: Vec<Waiter>) {
        self.running = key.map(|k| (k, text));
        self.waiters = waiters;
    }

    /// Look up a key that is not queued: check the running turn first, then
    /// the journaled record.
    pub fn check(&mut self, key: &str, text: &str, active: bool, waiter: Waiter) -> Seen {
        if let Some((k, t)) = &self.running
            && k == key
            && active
        {
            if t == text {
                self.waiters.push(waiter);
                return Seen::Waits;
            }
            return Seen::Conflict;
        }
        match self.recorded.get(key) {
            None => Seen::New,
            Some((t, _)) if t != text => Seen::Conflict,
            Some((_, Some(outcome))) => Seen::Answer(outcome.clone()),
            // On record without an outcome and not running: a restart cut it off.
            Some((_, None)) => Seen::Answer(Ok("cancelled")),
        }
    }

    /// The running turn ended with what its first request is answered with.
    /// A key that never reached the journal (the turn failed before
    /// `Submitted`) stays unused, as §12.4 asks.
    pub fn finish(&mut self, result: &Outcome) -> Vec<Waiter> {
        if let Some((k, _)) = self.running.take()
            && let Some((_, slot)) = self.recorded.get_mut(&k)
        {
            *slot = Some(result.clone());
        }
        std::mem::take(&mut self.waiters)
    }
}
