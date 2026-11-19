//! Idempotent prompts (contract 1.3.0 §12, docs/adr/0045): a `session/prompt`
//! that carries a key already on record starts no turn. It is answered with
//! the outcome of the prompt that first carried that key.
//!
//! The design choice to know: the server actor keeps its own book of keys,
//! seeded from the journal when a session is registered and advanced by the
//! events it already relays, so a lookup never waits on the agent lock that a
//! running turn holds. A prompt counts as running from the moment it leaves
//! the queue, before its `Submitted` row exists, so a resend in that gap
//! waits instead of running twice.

use super::live::{Phase, Session};
use super::map;
use super::rpc;
use super::server::Server;
use super::turn::Prompt;
use crate::event::{Event, Submission};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::task::JoinSet;

use map::{IDEMPOTENCY_CONFLICT as CONFLICT, IDEMPOTENCY_KEY as FIELD, IDEMPOTENCY_KEY_MAX as MAX_CHARS};

/// What a turn answered: a stop reason, or the error its request got.
pub(super) type Outcome = Result<&'static str, String>;
/// A request waiting on another prompt's outcome: its id and attachment.
pub(super) type Waiter = (Value, u64);

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
    pub waiters: Vec<Waiter>,
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

fn answer(outcome: Outcome) -> Result<Value, (i64, String)> {
    outcome.map(|reason| json!({ "stopReason": reason })).map_err(|e| (rpc::INTERNAL, e))
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
    pub fn start(&mut self, prompt: &mut Prompt) {
        self.running = prompt.key.clone().map(|k| (k, prompt.text.clone()));
        self.waiters = std::mem::take(&mut prompt.waiters);
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

/// What to do with a prompt whose key may be on record.
enum Seen {
    New,
    Waits,
    Conflict,
    Answer(Result<Value, (i64, String)>),
}

fn seen(s: &mut Session, key: &str, text: &str, waiter: Waiter) -> Seen {
    let same = |t: &str| if t == text { Seen::Waits } else { Seen::Conflict };
    if let Some(p) = s.prompts.iter_mut().find(|p| p.key.as_deref() == Some(key)) {
        let seen = same(&p.text);
        if matches!(seen, Seen::Waits) {
            p.waiters.push(waiter);
        }
        return seen;
    }
    if let Some((k, t)) = &s.keys.running
        && k == key
        && !matches!(s.turn.phase, Phase::Idle(_))
    {
        let seen = same(t);
        if matches!(seen, Seen::Waits) {
            s.keys.waiters.push(waiter);
        }
        return seen;
    }
    match s.keys.recorded.get(key) {
        None => Seen::New,
        Some((t, _)) if t != text => Seen::Conflict,
        Some((_, Some(outcome))) => Seen::Answer(answer(outcome.clone())),
        // On record without an outcome and not running: a restart cut it off.
        Some((_, None)) => Seen::Answer(Ok(json!({ "stopReason": "cancelled" }))),
    }
}

impl Server {
    /// `session/prompt`: refuse a malformed key, answer a recorded one, and
    /// queue everything else.
    pub(super) fn prompt(&mut self, id: Value, params: &Value, turns: &mut JoinSet<()>) {
        let sid = params["sessionId"].as_str().unwrap_or_default();
        let attachment = self.attachment;
        let Some(s) = self.sessions.get_mut(sid) else {
            return self.reply(&id, Err((rpc::INVALID_PARAMS, format!("no session {sid:?}"))));
        };
        let key = match parse(params) {
            Ok(key) => key,
            Err(e) => return self.reply(&id, Err((rpc::INVALID_PARAMS, e))),
        };
        let blocks = params["prompt"].as_array().cloned().unwrap_or_default();
        let text = map::prompt_text(&blocks);
        if let Some(k) = &key {
            match seen(s, k, &text, (id.clone(), attachment)) {
                Seen::New => {}
                Seen::Waits => return,
                Seen::Answer(r) => return self.reply(&id, r),
                Seen::Conflict => {
                    let message = format!("{FIELD} {k:?} already names a different prompt");
                    let data = json!({ "reason": CONFLICT });
                    let _ = self.out.send(rpc::err_data(&id, rpc::INVALID_PARAMS, &message, data));
                    return;
                }
            }
        }
        let prompt = Prompt { generation: s.generation, id, attachment, text, key, waiters: vec![] };
        s.prompts.push_back(prompt);
        self.start_prompt(sid, turns);
    }
}
