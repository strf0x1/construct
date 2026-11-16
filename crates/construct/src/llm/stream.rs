//! Folding a chat-completions SSE stream into one `Completion`: text and
//! reasoning deltas forwarded as they arrive, tool-call fragments joined by
//! index, reasoning kept with the key it came under (and OpenRouter's
//! `reasoning_details` merged block by block) so it can be sent back, and the
//! usage from whichever chunk carries it. Framing works on
//! bytes and decodes whole lines only, so a chunk boundary inside a
//! multibyte character never reaches the decoder.

use super::{Completion, Delta, FunctionCall, Message, ToolCall, Usage, notes, truncate};
use crate::journal::notes::Coalescer;
use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::sync::mpsc;

/// SSE framing: bytes in, the payload of each complete `data:` line out.
#[derive(Default)]
pub(super) struct Sse {
    buf: Vec<u8>,
}

impl Sse {
    pub(super) fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = vec![];
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            if let Some(data) = line.trim_end_matches(['\r', '\n']).strip_prefix("data:") {
                out.push(data.trim_start().to_string());
            }
        }
        out
    }
}

/// The delta key of OpenRouter's structured reasoning, and the message key it
/// goes back under.
pub(super) const DETAILS: &str = "reasoning_details";

/// The key of provider data on a tool call (Gemini's thought signature).
const EXTRA: &str = "extra_content";

/// The delta keys reasoning text streams under, in order of preference.
pub(super) const REASONING_KEYS: [&str; 2] = ["reasoning_content", "reasoning"];

/// The string fields of a `reasoning_details` block that stream in pieces.
const DETAIL_TEXT: [&str; 3] = ["text", "summary", "data"];

#[derive(Default)]
pub(super) struct Accumulator {
    content: String,
    reasoning: String,
    reasoning_field: Option<&'static str>,
    details: Vec<Value>,
    calls: BTreeMap<usize, (String, String, String)>,
    /// A call's `extra_content`, by index, as the provider last sent it.
    extras: BTreeMap<usize, Value>,
    usage: Usage,
    finish: String,
    model: Option<String>,
}

impl Accumulator {
    /// One `data:` payload. True at `[DONE]`; an `error` object ends the stream.
    pub(super) fn data(
        &mut self,
        data: &str,
        tx: Option<&mpsc::UnboundedSender<Delta>>,
        notes: &mut Coalescer,
    ) -> Result<bool> {
        if data == "[DONE]" {
            return Ok(true);
        }
        if data.is_empty() {
            return Ok(false);
        }
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                notes.record(notes::BAD_CHUNK, &format!("{e}: {}", truncate(data, 80)));
                return Ok(false);
            }
        };
        if let Some(err) = v.get("error") {
            bail!("model endpoint error: {}", truncate(&err.to_string(), 600));
        }
        self.feed(&v, tx, notes);
        Ok(false)
    }

    pub(super) fn feed(&mut self, v: &Value, tx: Option<&mpsc::UnboundedSender<Delta>>, notes: &mut Coalescer) {
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.usage = Usage::parse(u);
        }
        if let Some(m) = v.get("model").and_then(Value::as_str).filter(|m| !m.is_empty()) {
            self.model = Some(m.to_string());
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
        for key in REASONING_KEYS {
            if let Some(t) = d.get(key).and_then(Value::as_str) {
                if !t.is_empty() {
                    self.reasoning_field.get_or_insert(key);
                    self.reasoning.push_str(t);
                    if let Some(tx) = tx {
                        let _ = tx.send(Delta::Reasoning(t.to_string()));
                    }
                }
                break;
            }
        }
        if let Some(blocks) = d.get(DETAILS).and_then(Value::as_array) {
            for b in blocks {
                self.detail(b);
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
                if let Some(x) = c.get(EXTRA).filter(|x| !x.is_null()) {
                    self.extras.insert(index, x.clone());
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

    /// Fold one streamed `reasoning_details` piece: a piece of the block
    /// before it (same type and index) extends its text, anything else starts
    /// a block. The provider wants the sequence back unchanged.
    fn detail(&mut self, b: &Value) {
        let Some(piece) = b.as_object() else { return };
        let same = |prev: &Value| {
            prev.get("type") == piece.get("type")
                && prev.get("index") == piece.get("index")
                && match (prev.get("id").filter(|v| !v.is_null()), piece.get("id").filter(|v| !v.is_null())) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                }
        };
        let Some(prev) = self.details.last_mut().filter(|p| same(p)).and_then(Value::as_object_mut) else {
            self.details.push(b.clone());
            return;
        };
        for (k, v) in piece {
            // A null (a signature not yet known) never erases a value.
            if v.is_null() {
                prev.entry(k.clone()).or_insert(Value::Null);
                continue;
            }
            match (prev.get_mut(k), v.as_str()) {
                (Some(Value::String(acc)), Some(more)) if DETAIL_TEXT.contains(&k.as_str()) => acc.push_str(more),
                _ => {
                    prev.insert(k.clone(), v.clone());
                }
            }
        }
    }

    /// Whether the provider said why it stopped.
    pub(super) fn finished(&self) -> bool {
        !self.finish.is_empty()
    }

    pub(super) fn finish(mut self) -> Result<Completion> {
        let tool_calls: Vec<ToolCall> = self
            .calls
            .into_iter()
            .filter(|(_, (_, name, _))| !name.is_empty())
            .map(|(i, (id, name, args))| ToolCall {
                id: if id.is_empty() { format!("call_{i}") } else { id },
                kind: "function".into(),
                function: FunctionCall { name, arguments: if args.trim().is_empty() { "{}".into() } else { args } },
                extra_content: self.extras.remove(&i),
            })
            .collect();
        if self.content.is_empty()
            && tool_calls.is_empty()
            && self.reasoning.is_empty()
            && self.details.is_empty()
            && self.finish.is_empty()
        {
            return Err(anyhow!("model returned an empty stream"));
        }
        Ok(Completion {
            message: Message {
                role: "assistant".into(),
                content: if self.content.is_empty() { None } else { Some(self.content) },
                parts: vec![],
                tool_calls,
                tool_call_id: None,
                reasoning_field: self.reasoning_field.filter(|_| !self.reasoning.is_empty()).map(String::from),
                reasoning: if self.reasoning.is_empty() { None } else { Some(self.reasoning) },
                reasoning_details: if self.details.is_empty() { None } else { Some(Value::Array(self.details)) },
            },
            usage: Usage { served: self.model, ..self.usage },
            finish_reason: self.finish,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::prop;
    use proptest::prelude::*;
    use serde_json::json;

    /// One line of a stream as a provider might send it.
    fn line() -> impl Strategy<Value = String> {
        let delta = |d: Value| json!({ "choices": [{ "delta": d }] }).to_string();
        prop_oneof![
            4 => prop::text().prop_map(move |t| delta(json!({ "content": t }))),
            2 => prop::text().prop_map(move |t| delta(json!({ "reasoning_content": t }))),
            3 => (0u64..3, proptest::option::of("[a-z_]{1,8}"), prop::text()).prop_map(move |(i, name, args)| {
                delta(json!({ "tool_calls": [{ "index": i, "id": format!("c{i}"), "function": { "name": name, "arguments": args } }] }))
            }),
            1 => Just(json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }], "usage": { "prompt_tokens": 3 } }).to_string()),
            1 => proptest::sample::select(&["not json {", "", "[DONE"][..]).prop_map(String::from),
        ]
        .prop_map(|d| format!("data: {d}"))
    }

    /// The stream's bytes: lines ended by LF or CRLF, comments and blank
    /// lines between them, then `[DONE]`.
    fn stream() -> impl Strategy<Value = Vec<u8>> {
        let framed =
            (prop_oneof![3 => line(), 1 => Just(": keep-alive".to_string()), 1 => Just(String::new())], any::<bool>())
                .prop_map(|(l, crlf)| format!("{l}{}", if crlf { "\r\n" } else { "\n" }));
        proptest::collection::vec(framed, 0..24).prop_map(|ls| format!("{}data: [DONE]\n", ls.concat()).into_bytes())
    }

    fn fold<'a>(chunks: impl Iterator<Item = &'a [u8]>) -> (Option<(Message, String)>, bool) {
        let (mut sse, mut acc, mut notes) = (Sse::default(), Accumulator::default(), Coalescer::default());
        for chunk in chunks {
            for data in sse.push(chunk) {
                if acc.data(&data, None, &mut notes).unwrap() {
                    return (acc.finish().ok().map(|c| (c.message, c.finish_reason)), true);
                }
            }
        }
        (acc.finish().ok().map(|c| (c.message, c.finish_reason)), false)
    }

    proptest! {
        #![proptest_config(prop::config())]

        /// However the network cuts the bytes (inside a CRLF, inside a
        /// four-byte emoji), the stream folds to the same completion.
        #[test]
        fn a_stream_split_anywhere_folds_like_the_whole(bytes in stream(), cuts in proptest::collection::vec(any::<prop::Index>(), 0..12)) {
            let mut at: Vec<usize> = cuts.iter().map(|c| c.index(bytes.len() + 1)).collect();
            at.push(0);
            at.push(bytes.len());
            at.sort_unstable();
            let whole = fold(std::iter::once(&bytes[..]));
            let split = fold(at.windows(2).map(|w| &bytes[w[0]..w[1]]));
            prop_assert!(whole.1, "the [DONE] line always ends the stream");
            prop_assert_eq!(whole, split);
        }
    }

    #[test]
    fn the_completion_names_the_model_the_stream_said_served_it() {
        let (mut acc, mut notes) = (Accumulator::default(), Coalescer::default());
        acc.feed(
            &json!({ "model": "vendor/real-model", "choices": [{ "delta": { "content": "hi" } }] }),
            None,
            &mut notes,
        );
        acc.feed(
            &json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }], "usage": { "prompt_tokens": 3 } }),
            None,
            &mut notes,
        );
        let c = acc.finish().unwrap();
        assert_eq!((c.usage.served.as_deref(), c.usage.prompt_tokens), (Some("vendor/real-model"), 3));
    }

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

    fn delta(d: Value) -> Value {
        json!({ "choices": [{ "delta": d }] })
    }

    #[test]
    fn reasoning_remembers_the_key_it_came_in() {
        let mut acc = Accumulator::default();
        let mut notes = Coalescer::default();
        acc.feed(&delta(json!({ "reasoning": "think" })), None, &mut notes);
        acc.feed(&delta(json!({ "reasoning": "ing" })), None, &mut notes);
        let m = acc.finish().unwrap().message;
        assert_eq!((m.reasoning.as_deref(), m.reasoning_field.as_deref()), (Some("thinking"), Some("reasoning")));
    }

    #[test]
    fn reasoning_details_pieces_join_into_blocks_in_order() {
        let mut acc = Accumulator::default();
        let mut notes = Coalescer::default();
        let text = |t: &str, sig: Value| json!({ "type": "reasoning.text", "text": t, "signature": sig, "id": "r1", "format": "anthropic-claude-v1", "index": 0 });
        acc.feed(&delta(json!({ DETAILS: [text("Let me ", Value::Null)] })), None, &mut notes);
        acc.feed(&delta(json!({ DETAILS: [text("look.", Value::Null)] })), None, &mut notes);
        acc.feed(&delta(json!({ DETAILS: [text("", json!("sig=="))] })), None, &mut notes);
        acc.feed(
            &delta(json!({ DETAILS: [{ "type": "reasoning.encrypted", "data": "xyz", "index": 1 }] })),
            None,
            &mut notes,
        );
        let m = acc.finish().unwrap().message;
        assert_eq!(
            m.reasoning_details,
            Some(json!([
                { "type": "reasoning.text", "text": "Let me look.", "signature": "sig==", "id": "r1", "format": "anthropic-claude-v1", "index": 0 },
                { "type": "reasoning.encrypted", "data": "xyz", "index": 1 },
            ]))
        );
        assert!(m.reasoning.is_none(), "details alone are not plain reasoning");
    }

    #[test]
    fn a_thought_signature_stays_with_its_call() {
        let mut acc = Accumulator::default();
        let mut notes = Coalescer::default();
        let sig = json!({ "google": { "thought_signature": "c2ln" } });
        let first =
            json!({ "index": 0, "id": "a", "function": { "name": "read", "arguments": "{}" }, "extra_content": sig });
        let second = json!({ "index": 1, "id": "b", "function": { "name": "read", "arguments": "{}" } });
        acc.feed(&delta(json!({ "tool_calls": [first, second] })), None, &mut notes);
        let calls = acc.finish().unwrap().message.tool_calls;
        assert_eq!(calls[0].extra_content, Some(sig), "parallel calls: only the first is signed");
        assert_eq!(calls[1].extra_content, None);
    }
}
