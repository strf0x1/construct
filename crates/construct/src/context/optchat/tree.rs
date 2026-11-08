//! The tree of lines: one line per log entry, and merged parents over pairs
//! (ADR 0043 decisions 2 and 3). A leaf whose flattened entry fits in `LINE`
//! bytes is derived from the entry, so replay needs no journal row for it;
//! only a line the summary model wrote is stored.

use crate::llm::Message;
use std::collections::BTreeMap;

/// A leaf's byte ceiling. Part of the format: replay depends on it.
pub const LINE: usize = 512;

/// One written line. Leaves have `l == 0`, merges `l >= 1`; `fallback` marks
/// a line written without the summary model succeeding (ADR 0043 decision 10).
#[derive(Debug, Clone)]
pub struct Node {
    pub text: String,
    /// Kept for slice 3, which writes fallback leaves and names them in the
    /// view; this slice only carries the flag through replay.
    #[expect(dead_code, reason = "the summary slice reads fallback nodes")]
    pub fallback: bool,
}

/// Nodes by `(l, i)`. Covered entries are `[i·2^l, (i+1)·2^l)`.
#[derive(Debug, Default, Clone)]
pub struct Tree {
    nodes: BTreeMap<(u32, u64), Node>,
}

/// A message's text, unflattened. A parts message (the rendered view) joins
/// its parts, though only log entries reach a line.
pub(crate) fn raw(m: &Message) -> String {
    match &m.content {
        Some(content) => content.clone(),
        None => m.parts.iter().map(|p| p.text.as_str()).collect(),
    }
}

/// `\s*\n\s*` to one space, so a line stays one line. Text with no newline is
/// unchanged.
pub fn flatten(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            space.push(ch);
            continue;
        }
        if !space.is_empty() {
            out.push_str(if space.contains('\n') { " " } else { &space });
            space.clear();
        }
        out.push(ch);
    }
    if space.contains('\n') {
        out.push(' ');
    } else {
        out.push_str(&space);
    }
    out
}

/// `user: …`, `assistant: …` (then each ` name(arguments)`), or
/// `result: …` for a tool message, flattened to one line.
pub fn entry_text(m: &Message) -> String {
    let body = raw(m);
    match m.role.as_str() {
        "assistant" => {
            let mut text = format!("assistant: {body}");
            for call in &m.tool_calls {
                text.push_str(&format!(" {}({})", call.function.name, call.function.arguments));
            }
            flatten(&text)
        }
        "tool" => flatten(&format!("result: {body}")),
        role => flatten(&format!("{role}: {body}")),
    }
}

/// The entry's own line when it fits, else `None`: it needs a written leaf.
pub fn derived(m: &Message) -> Option<String> {
    let text = entry_text(m);
    (text.len() <= LINE).then_some(text)
}

impl Tree {
    /// Store a written line. A leaf is `(0, i)`; a merge is `(l, i)` with
    /// `l >= 1`.
    pub fn insert(&mut self, l: u32, i: u64, text: String, fallback: bool) {
        self.nodes.insert((l, i), Node { text, fallback });
    }

    /// Whether `(l, i)` has a line: stored, or derived for a leaf.
    pub fn built(&self, entries: &[Message], l: u32, i: u64) -> bool {
        if l == 0 {
            entries.get(usize::try_from(i).unwrap_or(usize::MAX)).is_some_and(|m| derived(m).is_some())
                || self.nodes.contains_key(&(0, i))
        } else {
            self.nodes.contains_key(&(l, i))
        }
    }

    /// The line's text, stored or derived.
    pub fn text(&self, entries: &[Message], l: u32, i: u64) -> Option<String> {
        if let Some(node) = self.nodes.get(&(l, i)) {
            return Some(node.text.clone());
        }
        if l != 0 {
            return None;
        }
        entries.get(usize::try_from(i).ok()?).and_then(derived)
    }

    /// `{i·2^l}+{2^l}`, the node's name in a line and to `zoom`.
    pub fn ref_of(l: u32, i: u64) -> Option<String> {
        Some(format!("{}+{}", i.checked_shl(l)?, 1u64.checked_shl(l)?))
    }

    /// `{ref}|{text}`: what the model sees and `zoom` matches.
    pub fn line(&self, entries: &[Message], l: u32, i: u64) -> Option<String> {
        Some(format!("{}|{}", Self::ref_of(l, i)?, self.text(entries, l, i)?))
    }

    /// One past the last entry `(l, i)` covers.
    pub fn end(l: u32, i: u64) -> Option<u128> {
        let width = 1u128.checked_shl(l)?;
        (u128::from(i) + 1).checked_mul(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: "c".into(),
            kind: "function".into(),
            function: FunctionCall { name: name.into(), arguments: args.into() },
        }
    }

    #[test]
    fn entry_text_labels_each_role() {
        assert_eq!(entry_text(&Message::user("hello")), "user: hello");
        let mut answer = Message::text("assistant", "done");
        answer.tool_calls = vec![call("read", "{\"path\":\"a\"}"), call("grep", "{}")];
        assert_eq!(entry_text(&answer), "assistant: done read({\"path\":\"a\"}) grep({})");
        assert_eq!(entry_text(&Message::tool("c1", "ok")), "result: ok");
    }

    #[test]
    fn flatten_collapses_newlines_and_their_whitespace_but_leaves_plain_text_alone() {
        assert_eq!(flatten("one\ntwo"), "one two");
        assert_eq!(flatten("one \n\t two"), "one two");
        assert_eq!(flatten("one\n\n\ntwo"), "one two");
        assert_eq!(flatten("one   two"), "one   two");
        assert_eq!(flatten("no newline"), "no newline");
    }

    #[test]
    fn a_ref_is_the_covered_range() {
        assert_eq!(Tree::ref_of(0, 5).as_deref(), Some("5+1"));
        assert_eq!(Tree::ref_of(3, 2).as_deref(), Some("16+8"));
    }

    #[test]
    fn a_short_entry_is_derived_and_a_long_one_is_not() {
        assert_eq!(derived(&Message::user("short")).as_deref(), Some("user: short"));
        assert_eq!(derived(&Message::user("x".repeat(600))), None);
        let long = "x".repeat(600);
        assert_eq!(derived(&Message::user(format!("{long}\n\n{long}"))), None);
    }
}
