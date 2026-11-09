//! `zoom`, the one tool an `optchat` session offers (ADR 0043 decision 9): it
//! opens a line from the view, returning a node's two halves or a leaf's
//! original message, in pages of 32 KB.
//!
//! The one choice a reader must know: the strategy answers the call through
//! `Context::call`, never through the tool catalog, so the wire name is
//! declared here and nowhere else.

use super::summary::cut;
use super::tree::{Tree, raw};
use crate::llm::Message;
use serde::Deserialize;
use serde_json::{Value, json};

/// The tool's wire name, declared once.
pub const ZOOM: &str = "zoom";

/// A leaf page's byte ceiling. Pages cut on a char boundary.
pub const PAGE: usize = 32_768;

/// What `Optchat::tools` offers the model.
pub(super) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": ZOOM,
            "description": "Open a <chat> line id+n|text. A node returns the two lines it was made from; \
                            n = 1 returns the entry in full, in pages of 32768 bytes. Zoom when a line \
                            only mentions what you need, before you act, guess or ask.",
            "parameters": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "The line's first entry, before the +." },
                    "n": { "type": "integer", "description": "The line's width, after the +: a power of two." },
                    "page": { "type": "integer", "description": "A 0-based page of a leaf's text, when it has more than one." }
                },
                "required": ["id", "n"]
            }
        }
    })
}

/// The arguments `zoom` takes. `page` defaults to the first.
#[derive(Deserialize)]
struct Args {
    id: u64,
    n: u64,
    #[serde(default)]
    page: usize,
}

/// Answer one call. The `Err` text is the refusal, starting `{ZOOM}: `.
pub(super) fn call(entries: &[Message], tree: &Tree, args: &str) -> Result<String, String> {
    let Args { id: raw_id, n: raw_n, page } = serde_json::from_str(args)
        .map_err(|_| format!(r#"{ZOOM}: expected {{"id": integer, "n": integer, "page": integer}}"#))?;
    let count = entries.len() as u128;
    let (id, n) = (u128::from(raw_id), u128::from(raw_n));
    if !n.is_power_of_two() || id % n != 0 || id + n > count {
        return Err(format!(
            "{ZOOM}: {raw_id}+{raw_n} is not a node: \
             n must be a power of two, id a multiple of n, and id+n at most {count}"
        ));
    }
    let l = n.trailing_zeros();
    let i = (id >> l) as u64;
    if !tree.built(entries, l, i) {
        return Err(not_in_tree(raw_id, raw_n));
    }
    if l == 0 {
        return leaf(entries, raw_id as usize, page);
    }
    match (tree.line(entries, l - 1, i * 2), tree.line(entries, l - 1, i * 2 + 1)) {
        (Some(first), Some(second)) => Ok(format!("{first}\n{second}")),
        _ => Err(not_in_tree(raw_id, raw_n)),
    }
}

fn not_in_tree(id: u64, n: u64) -> String {
    format!("{ZOOM}: {id}+{n} is not in the tree; zoom a line from <chat> or one zoom returned")
}

/// A leaf: the entry's raw content, one page of it, under the leaf's own ref.
fn leaf(entries: &[Message], id: usize, page: usize) -> Result<String, String> {
    let text = entries.get(id).map(raw).unwrap_or_default();
    let pages = pages(&text);
    let count = pages.len();
    let Some(chunk) = pages.get(page) else {
        return Err(format!("{ZOOM}: page {page} of {count} does not exist"));
    };
    let mut out = format!("{id}+1|{chunk}");
    if count > 1 {
        out.push_str(&format!("\n(page {} of {count})", page + 1));
    }
    Ok(out)
}

/// `text` in pages of `PAGE` bytes, each ending on a char boundary.
fn pages(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let chunk = cut(rest, PAGE);
        out.push(chunk);
        rest = rest.get(chunk.len()..).unwrap_or_default();
    }
    if out.is_empty() {
        out.push("");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Entry 0 is short; entry 1 is a node of two; entry 2 is three pages.
    fn entries() -> Vec<Message> {
        vec![
            Message::user("first message"),
            Message::text("assistant", "answer"),
            Message::user("x".repeat(2 * PAGE + 10)),
        ]
    }

    fn node() -> Tree {
        let mut tree = Tree::default();
        tree.insert(1, 0, "merged".into(), false);
        tree.insert(0, 2, "written".into(), false);
        tree
    }

    fn call_on(args: &str) -> String {
        call(&entries(), &node(), args).expect("an answer")
    }

    fn refusal(args: &str) -> String {
        call(&entries(), &node(), args).expect_err("a refusal")
    }

    #[test]
    fn the_description_says_when_to_zoom() {
        let description = definition()["function"]["description"].as_str().expect("a description").to_string();
        assert!(description.contains("before you act, guess or ask"), "{description}");
    }

    #[test]
    fn a_node_returns_its_two_children_lines() {
        assert_eq!(call_on(r#"{"id":0,"n":2}"#), "0+1|user: first message\n1+1|assistant: answer");
    }

    #[test]
    fn a_leaf_with_one_page_is_the_original_message() {
        assert_eq!(call_on(r#"{"id":0,"n":1}"#), "0+1|first message");
    }

    #[test]
    fn a_three_page_leaf_pages_on_char_boundaries_and_footers_each_page() {
        let first = call_on(r#"{"id":2,"n":1}"#);
        assert_eq!(first, format!("2+1|{}\n(page 1 of 3)", "x".repeat(PAGE)));
        let second = call_on(r#"{"id":2,"n":1,"page":1}"#);
        assert_eq!(second, format!("2+1|{}\n(page 2 of 3)", "x".repeat(PAGE)));
        let third = call_on(r#"{"id":2,"n":1,"page":2}"#);
        assert_eq!(third, format!("2+1|{}\n(page 3 of 3)", "x".repeat(10)));
    }

    #[test]
    fn a_page_never_splits_a_char() {
        let mut tree = Tree::default();
        tree.insert(0, 0, "written".into(), false);
        let mut entries = vec![Message::user("é".repeat(PAGE / 2 + 1))];
        let args = r#"{"id":0,"n":1,"page":1}"#;
        assert_eq!(call(&entries, &tree, args).expect("an answer"), "0+1|é\n(page 2 of 2)");
        entries[0] = Message::user("short");
        assert_eq!(call(&entries, &tree, r#"{"id":0,"n":1}"#).expect("an answer"), "0+1|short");
    }

    #[test]
    fn a_misaligned_node_names_what_is_valid() {
        let msg = "zoom: 0+3 is not a node: n must be a power of two, id a multiple of n, and id+n at most 3";
        assert_eq!(refusal(r#"{"id":0,"n":3}"#), msg);
        let msg = "zoom: 1+2 is not a node: n must be a power of two, id a multiple of n, and id+n at most 3";
        assert_eq!(refusal(r#"{"id":1,"n":2}"#), msg);
        let msg = "zoom: 2+2 is not a node: n must be a power of two, id a multiple of n, and id+n at most 3";
        assert_eq!(refusal(r#"{"id":2,"n":2}"#), msg);
    }

    #[test]
    fn an_unwritten_node_or_leaf_is_not_in_the_tree() {
        let msg = "zoom: 0+2 is not in the tree; zoom a line from <chat> or one zoom returned";
        assert_eq!(call(&entries(), &Tree::default(), r#"{"id":0,"n":2}"#).expect_err("a refusal"), msg);
        let mut tree = Tree::default();
        tree.insert(1, 0, "merged".into(), false);
        let msg = "zoom: 2+1 is not in the tree; zoom a line from <chat> or one zoom returned";
        assert_eq!(call(&entries(), &tree, r#"{"id":2,"n":1}"#).expect_err("a refusal"), msg);
    }

    #[test]
    fn a_page_past_the_end_is_an_error() {
        assert_eq!(refusal(r#"{"id":0,"n":1,"page":1}"#), "zoom: page 1 of 1 does not exist");
    }
}
