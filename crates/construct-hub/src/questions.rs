//! A peer's permission questions as the hub holds them (#362, #365): each
//! `session/request_permission` waits here, with the peer's wire id, until a
//! human answers it over the control socket.
//!
//! The design choice: an answer names the question by place (1-based) and
//! may also name its title, and a title that no longer matches is refused,
//! so a list that moved under a TUI never sends a yes to another question.

use crate::view::{self, Question};
use construct_core::acp::rpc;
use serde_json::{Value, json};

/// The open questions, oldest first: the peer's wire id and its params.
#[derive(Default)]
pub(crate) struct Questions(Vec<(Value, Value)>);

impl Questions {
    pub(crate) fn push(&mut self, wire: Value, params: Value) {
        self.0.push((wire, params));
    }

    /// Ids die with an attachment, and an idle turn has nothing to ask.
    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }

    pub(crate) fn list(&self) -> Vec<Question> {
        self.0.iter().map(|(_, p)| view::question(p)).collect()
    }

    /// Answer question `n` (1-based; may be omitted when only one is open)
    /// with `option`, one of the ids the peer offered; with `expect`, the
    /// question there must still have that title. The line for the peer.
    pub(crate) fn answer(&mut self, option: &str, n: Option<usize>, expect: Option<&str>) -> Result<String, String> {
        let at = match (n, self.0.len()) {
            (_, 0) => return Err("no question is open".into()),
            (None, 1) => 0,
            (None, k) => return Err(format!("{k} questions are open; say which (--question 1..{k})")),
            (Some(n), k) if (1..=k).contains(&n) => n - 1,
            (Some(n), k) => return Err(format!("no question {n}; {k} open")),
        };
        if expect.is_some_and(|want| view::question(&self.0[at].1).title != want) {
            return Err("that question is no longer open there; look again".into());
        }
        let offered = view::options(&self.0[at].1);
        if !offered.iter().any(|o| o == option) {
            return Err(format!("{option:?} is not offered; pick one of {}", offered.join(", ")));
        }
        let (wire, _) = self.0.remove(at);
        Ok(rpc::ok(&wire, json!({ "outcome": { "outcome": "selected", "optionId": option } })))
    }
}
