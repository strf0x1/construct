//! Folding a chat-completions SSE stream into one `Completion`: text and
//! reasoning deltas forwarded as they arrive, tool-call fragments joined by
//! index, and the usage from whichever chunk carries it.

use super::{Completion, Delta, FunctionCall, Message, ToolCall, Usage, notes};
use crate::journal::notes::Coalescer;
use anyhow::{Result, anyhow};
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::sync::mpsc;

#[derive(Default)]
pub(super) struct Accumulator {
    content: String,
    reasoning: String,
    calls: BTreeMap<usize, (String, String, String)>,
    usage: Usage,
    finish: String,
}

impl Accumulator {
    pub(super) fn feed(&mut self, v: &Value, tx: Option<&mpsc::UnboundedSender<Delta>>, notes: &mut Coalescer) {
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.usage = Usage::parse(u);
        }
        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else { return };
        if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = f.to_string();
        }
        let Some(d) = choice.get("delta") else { return };
        if let Some(t) = d.get("content").and_then(Value::as_str)
            && !t.is_empty()
        {
            self.content.push_str(t);
            if let Some(tx) = tx {
                let _ = tx.send(Delta::Text(t.to_string()));
            }
        }
        for key in ["reasoning_content", "reasoning"] {
            if let Some(t) = d.get(key).and_then(Value::as_str) {
                if !t.is_empty() {
                    self.reasoning.push_str(t);
                    if let Some(tx) = tx {
                        let _ = tx.send(Delta::Reasoning(t.to_string()));
                    }
                }
                break;
            }
        }
        if let Some(calls) = d.get("tool_calls").and_then(Value::as_array) {
            for (pos, c) in calls.iter().enumerate() {
                let index = c.get("index").and_then(Value::as_u64).map(|i| i as usize).unwrap_or(pos);
                let entry = self.calls.entry(index).or_default();
                if let Some(id) = c.get("id").and_then(Value::as_str)
                    && !id.is_empty()
                {
                    if !entry.0.is_empty() && entry.0 != id {
                        notes.record(notes::INDEX_COLLISION, &format!("index {index}: {} then {id}", entry.0));
                    }
                    entry.0 = id.to_string();
                }
                if let Some(f) = c.get("function") {
                    if let Some(n) = f.get("name").and_then(Value::as_str)
                        && !n.is_empty()
                        && entry.1.is_empty()
                    {
                        entry.1 = n.to_string();
                        if let Some(tx) = tx {
                            let _ = tx.send(Delta::ToolStarted { name: n.to_string() });
                        }
                    }
                    if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                        entry.2.push_str(a);
                    }
                }
            }
        }
    }

    /// Whether the provider said why it stopped.
    pub(super) fn finished(&self) -> bool {
        !self.finish.is_empty()
    }

    pub(super) fn finish(self) -> Result<Completion> {
        let tool_calls: Vec<ToolCall> = self
            .calls
            .into_iter()
            .filter(|(_, (_, name, _))| !name.is_empty())
            .map(|(i, (id, name, args))| ToolCall {
                id: if id.is_empty() { format!("call_{i}") } else { id },
                kind: "function".into(),
                function: FunctionCall { name, arguments: if args.trim().is_empty() { "{}".into() } else { args } },
            })
            .collect();
        if self.content.is_empty() && tool_calls.is_empty() && self.reasoning.is_empty() && self.finish.is_empty() {
            return Err(anyhow!("model returned an empty stream"));
        }
        Ok(Completion {
            message: Message {
                role: "assistant".into(),
                content: if self.content.is_empty() { None } else { Some(self.content) },
                tool_calls,
                tool_call_id: None,
                reasoning: if self.reasoning.is_empty() { None } else { Some(self.reasoning) },
            },
            usage: self.usage,
            finish_reason: self.finish,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, args: &str) -> Value {
        json!({ "choices": [{ "delta": { "tool_calls": [{ "index": 0, "id": id, "function": { "name": "read", "arguments": args } }] } }] })
    }

    #[test]
    fn a_new_id_at_a_used_index_is_noted_and_the_same_id_is_not() {
        let mut acc = Accumulator::default();
        let mut notes = Coalescer::default();
        acc.feed(&call("a", "{"), None, &mut notes);
        acc.feed(&call("a", "}"), None, &mut notes);
        assert!(notes.drain().is_empty(), "a repeated id is the stream continuing");
        acc.feed(&call("b", "{}"), None, &mut notes);
        match notes.drain().as_slice() {
            [crate::event::Event::Note { kind, detail, count: 1, .. }] => {
                assert_eq!(kind, "index_collision");
                assert!(detail.contains("a then b"), "{detail}");
            }
            other => panic!("{other:?}"),
        }
    }
}
