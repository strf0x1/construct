//! Permission waiters owned by the server actor. Internal keys remain stable
//! for cancellation; wire ids are retired at initialize and replaced at load.

use super::ask::{self, Answer};
use super::rpc;
use crate::permission::Request;
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::sync::{mpsc, oneshot};

struct Question {
    session: String,
    request: Request,
    answer: oneshot::Sender<Answer>,
    wire: Option<u64>,
}

#[derive(Default)]
pub(super) struct Questions {
    next_wire: u64,
    pending: BTreeMap<u64, Question>,
}

impl Questions {
    pub fn insert(&mut self, key: u64, session: String, request: Request, answer: oneshot::Sender<Answer>) {
        self.pending.insert(key, Question { session, request, answer, wire: None });
    }

    pub fn retire(&mut self) {
        for q in self.pending.values_mut() {
            q.wire = None;
        }
    }

    pub fn send(&mut self, session: &str, out: &mpsc::UnboundedSender<String>) {
        for (key, q) in
            self.pending.iter_mut().filter(|(_, q)| q.session == session && q.wire.is_none() && !q.answer.is_closed())
        {
            self.next_wire += 1;
            q.wire = Some(self.next_wire);
            let _ = out.send(rpc::request(
                self.next_wire,
                "session/request_permission",
                ask::params(session, *key, &q.request),
            ));
        }
    }

    pub fn waiting(&self, session: &str) -> bool {
        self.pending.values().any(|q| q.session == session && !q.answer.is_closed())
    }

    pub fn answer(&mut self, id: &Value, result: Answer) -> Option<String> {
        let wire = id.as_u64()?;
        let key = *self.pending.iter().find(|(_, q)| q.wire == Some(wire))?.0;
        let q = self.pending.remove(&key)?;
        let _ = q.answer.send(result);
        Some(q.session)
    }

    pub fn forget(&mut self, key: u64) -> Option<String> {
        self.pending.remove(&key).map(|q| q.session)
    }

    pub fn close_session(&mut self, session: &str) {
        self.pending.retain(|_, q| q.session != session);
    }

    pub fn close(&mut self) {
        self.pending.clear();
    }
}
