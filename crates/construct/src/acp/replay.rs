//! The ACP transcript projection, seeded from the journal once and advanced
//! by the same consumer that delivers live events. This shared boundary keeps
//! a replay from duplicating journal commits still queued for live delivery.

use super::map::{call, chunk};
use crate::event::Event;
use crate::tools::ToolKind;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Transcript {
    committed: Vec<Value>,
    partial: String,
    raw: bool,
    calls: BTreeMap<String, Pending>,
}

struct Pending {
    name: String,
    args: String,
    status: &'static str,
}

impl Transcript {
    pub fn observe(&mut self, event: &Event, kind_of: &dyn Fn(&str) -> ToolKind) {
        match event {
            Event::Submitted { text } => {
                self.finish_partial();
                self.committed.push(chunk("user_message_chunk", text));
                self.raw = true;
            }
            Event::User { message } => {
                if !self.raw {
                    let text = message.content.as_deref().unwrap_or("");
                    let text = text.split("\n\n<system-reminder>").next().unwrap_or("");
                    if !text.is_empty() {
                        self.committed.push(chunk("user_message_chunk", text));
                    }
                }
                self.raw = false;
            }
            Event::Text { text } => self.partial.push_str(text),
            Event::Assistant { message } => {
                self.partial.clear();
                if let Some(text) = message.content.as_deref().filter(|s| !s.is_empty()) {
                    self.committed.push(chunk("agent_message_chunk", text));
                }
                for c in &message.tool_calls {
                    self.calls.entry(c.id.clone()).or_insert_with(|| Pending {
                        name: c.function.name.clone(),
                        args: c.function.arguments.clone(),
                        status: "pending",
                    });
                }
            }
            Event::ToolStart { id, name, args } => {
                self.calls
                    .insert(id.clone(), Pending { name: name.clone(), args: args.clone(), status: "in_progress" });
            }
            Event::ToolEnd { id, name, ok, output, .. } => {
                let pending = self.calls.remove(id);
                let (name, args) = pending.as_ref().map(|p| (p.name.as_str(), p.args.as_str())).unwrap_or((name, ""));
                self.committed.push(call(id, name, args, if *ok { "completed" } else { "failed" }, output, kind_of));
            }
            Event::Recovered { message } => self.committed.push(chunk("agent_message_chunk", message)),
            Event::TurnEnd { .. } => self.finish_partial(),
            _ => {}
        }
    }

    /// Freeze text whose stream ended before an Assistant commit, so a later
    /// turn cannot clear text the client already saw.
    pub fn finish_partial(&mut self) {
        if !self.partial.is_empty() {
            self.committed.push(chunk("agent_message_chunk", &std::mem::take(&mut self.partial)));
        }
    }

    pub fn updates(&self, live: bool, kind_of: &dyn Fn(&str) -> ToolKind) -> Vec<Value> {
        let mut out = self.committed.clone();
        if !self.partial.is_empty() {
            out.push(chunk("agent_message_chunk", &self.partial));
        }
        for (id, pending) in &self.calls {
            let status = if live { pending.status } else { "failed" };
            out.push(call(id, &pending.name, &pending.args, status, "", kind_of));
        }
        out
    }
}
