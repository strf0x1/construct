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
            Event::Submitted(crate::event::Submission { text, .. }) => {
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

/// The recorded conversation as the updates `session/load` replays, in order:
/// the user's prompts, the assistant's text, each tool call with its final
/// status, and the recovery notice. Compaction is not replayed (a client that
/// did not ask for `session.compaction` cannot apply it), and live-only
/// notices are not history. A call with no recorded outcome is `failed`,
/// never `pending`: recovery already told the model its outcome was unknown.
pub(super) fn history(entries: &[crate::journal::Entry], kind_of: &dyn Fn(&str) -> ToolKind) -> Vec<Value> {
    let mut transcript = Transcript::default();
    for entry in entries {
        transcript.observe(&entry.event, kind_of);
    }
    transcript.updates(false, kind_of)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Entry;
    use crate::llm::Message;
    use crate::tools::OutputKind;

    #[test]
    fn history_pairs_prompts_and_reports_final_tool_status() {
        let e = |seq, event| Entry { seq, at_ms: 0, event };
        let entries = vec![
            e(1, Event::submitted("go")),
            e(2, Event::User { message: Message::user("go\n\n<system-reminder>injected</system-reminder>") }),
            e(3, Event::Assistant { message: Message::text("assistant", "on it") }),
            e(4, Event::ToolStart { id: "c1".into(), name: "read".into(), args: r#"{"path":"a"}"#.into() }),
            e(
                5,
                Event::ToolEnd {
                    id: "c1".into(),
                    name: "read".into(),
                    ok: false,
                    output: "unknown".into(),
                    ms: 0,
                    kind: OutputKind::Plain,
                    data: None,
                    code: Some("interrupted_unknown".into()),
                },
            ),
            e(6, Event::ToolStart { id: "c2".into(), name: "bash".into(), args: r#"{"command":"ls"}"#.into() }),
            e(7, Event::Recovered { message: "interrupted".into() }),
        ];
        let h = history(&entries, &|_| ToolKind::Read);
        let kinds: Vec<&str> = h.iter().filter_map(|u| u["sessionUpdate"].as_str()).collect();
        assert_eq!(
            kinds,
            ["user_message_chunk", "agent_message_chunk", "tool_call", "agent_message_chunk", "tool_call"]
        );
        // The submitted prompt is the user's text; its model-facing copy is not repeated.
        assert_eq!(h[0]["content"]["text"], "go");
        assert_eq!((h[2]["status"].as_str(), h[2]["toolCallId"].as_str()), (Some("failed"), Some("c1")));
        assert_eq!(h[2]["content"][0]["content"]["text"], "unknown");
        // A start with no end is an unknown outcome too, never pending.
        assert_eq!((h[4]["status"].as_str(), h[4]["toolCallId"].as_str()), (Some("failed"), Some("c2")));
    }
}
