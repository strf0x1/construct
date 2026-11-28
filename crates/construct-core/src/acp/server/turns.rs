//! Prompt queuing, turn execution dispatch, and actor command handling on
//! `Server`: every live event, permission question, and turn completion flows
//! through this loop in order.

use super::Server;
use crate::acp::session::{Enqueue, Session};
use crate::acp::turn::{self, Command};
use crate::acp::{keys, map, rpc};
use serde_json::Value;
use tokio::task::JoinSet;

impl Server {
    /// `session/prompt`: refuse a malformed key, answer a recorded one, and
    /// queue everything else.
    pub(super) fn prompt(&mut self, id: Value, params: &Value, turns: &mut JoinSet<()>) {
        let sid = params["sessionId"].as_str().unwrap_or_default();
        let attachment = self.attachment;
        let Some(s) = self.sessions.get_mut(sid) else {
            return self.reply(&id, Err((rpc::INVALID_PARAMS, format!("no session {sid:?}"))));
        };
        let key = match keys::parse(params) {
            Ok(key) => key,
            Err(e) => return self.reply(&id, Err((rpc::INVALID_PARAMS, e))),
        };
        let blocks = params["prompt"].as_array().cloned().unwrap_or_default();
        let text = map::prompt_text(&blocks);
        match s.enqueue_prompt(id.clone(), attachment, text, key) {
            Enqueue::Queued => self.start_prompt(sid, turns),
            Enqueue::Waits => {}
            Enqueue::Answer(outcome) => self.reply(&id, keys::reply_outcome(outcome)),
            Enqueue::Conflict(k) => {
                let _ = self.out.send(keys::conflict_error(&id, &k));
            }
        }
    }

    pub(super) fn turn_notice(&self, sid: &str) {
        if let Some(s) = self.sessions.get(sid) {
            let _ = self.out.send(rpc::notification(self.wire.turn, s.turn_value(sid)));
        }
    }

    pub(super) fn waiting_changed(&mut self, sid: &str) {
        let waiting = self.questions.waiting(sid);
        if self.sessions.get_mut(sid).is_some_and(|s| s.set_waiting(waiting)) {
            self.turn_notice(sid);
        }
    }

    fn start_prompt(&mut self, sid: &str, turns: &mut JoinSet<()>) {
        if self.closing {
            return;
        }
        let Some((agent, prompt)) = self.sessions.get_mut(sid).and_then(Session::start_next_prompt) else {
            return;
        };
        turns.spawn(turn::run(agent, sid.to_string(), prompt, self.commands.clone()));
        self.turn_notice(sid);
    }

    pub(super) fn command(&mut self, command: Command, turns: &mut JoinSet<()>) {
        match command {
            Command::Event { session, generation, event } => {
                let Some(s) = self.sessions.get_mut(&session).filter(|s| s.generation() == generation) else {
                    return;
                };
                if let Some(line) = s.observe_event(&event, self.attachment, self.wire, &session) {
                    let _ = self.out.send(line);
                }
            }
            Command::Ask { key, session, generation, request, answer } => {
                if self.closing
                    || answer.is_closed()
                    || !self.sessions.get(&session).is_some_and(|s| s.generation() == generation)
                {
                    return;
                }
                self.questions.insert(key, session.clone(), request, answer);
                self.waiting_changed(&session);
                if self.sessions.get(&session).is_some_and(|s| s.is_attached(self.attachment)) {
                    self.questions.send(&session, &self.out);
                }
            }
            Command::Forget(key) => {
                if let Some(sid) = self.questions.forget(key) {
                    self.waiting_changed(&sid);
                }
            }
            Command::Finished { session, prompt, result } => {
                let mut waiters = vec![];
                if let Some(s) = self.sessions.get_mut(&session).filter(|s| s.generation() == prompt.generation) {
                    waiters = s.finish_turn(&result);
                    self.turn_notice(&session);
                }
                // A resent prompt waiting on this one gets the same answer (§12.4).
                waiters.insert(0, (prompt.id.clone(), prompt.attachment));
                for (id, _) in waiters.into_iter().filter(|(_, a)| *a == self.attachment) {
                    self.reply(&id, keys::reply_outcome(result.clone()));
                }
                if self.sessions.get(&session).is_some_and(|s| s.generation() == prompt.generation) {
                    self.start_prompt(&session, turns);
                }
            }
        }
    }
}
