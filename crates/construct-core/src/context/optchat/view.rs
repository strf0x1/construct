//! The view and the request shape (ADR 0043 decisions 4 and 5): the ordered
//! nodes that cover the log before the current turn, rendered as one user
//! message of `<chat>` blocks, then the current turn raw.
//!
//! The one choice a reader must know: the view only ever grows at its end,
//! and only the last full block of four lines is cache-marked, so the blocks
//! before it stay a stable prefix (decision 6).

use super::guide::GUIDE;
use super::tree::{Tree, raw};
use crate::llm::{Message, Part};

/// Lines in one cache-marked block.
pub const BLOCK: usize = 4;

/// The ordered nodes covering `[0, frontier)`, oldest first, and that
/// frontier. Nodes are never reordered in this slice: `advance` appends
/// leaves only.
#[derive(Debug, Default, Clone)]
pub struct View {
    pub nodes: Vec<(u32, u64)>,
    pub frontier: usize,
}

impl View {
    /// Replace the view from a replayed journal event.
    pub fn set(&mut self, nodes: Vec<(u32, u64)>) {
        self.frontier = nodes.iter().fold(0usize, |end, &(l, _)| {
            let width = 1u128.checked_shl(l).unwrap_or(0);
            end.saturating_add(usize::try_from(width).unwrap_or(usize::MAX))
        });
        self.nodes = nodes;
    }

    /// Append a leaf for every entry from the frontier to `to` and advance.
    /// Callers check that each leaf is written first.
    pub fn advance(&mut self, to: usize) {
        for i in self.frontier..to {
            self.nodes.push((0, i as u64));
        }
        self.frontier = self.frontier.max(to);
    }
}

/// The opening entry of the current turn: the last user message. Before one
/// exists, the end of the history, so everything is the opening's turn.
pub fn turn_of(entries: &[Message]) -> usize {
    entries.iter().rposition(|m| m.role == "user").unwrap_or(entries.len())
}

/// The request: system, the view and the turn's opening entry in one marked
/// user message, then the turn's raw messages.
///
/// When `frontier < turn` (nothing in this slice can write a missing leaf),
/// the raw entries `[frontier, turn)` follow the user message unchanged. Slice
/// 3 makes that unreachable at request time; it is kept for replay of a log
/// that has no line yet.
pub fn render(system: Option<&Message>, entries: &[Message], tree: &Tree, view: &View) -> Vec<Message> {
    let turn = turn_of(entries);
    let lines: Vec<String> =
        view.nodes.iter().filter_map(|&(l, i)| tree.line(entries, l, i)).map(|line| format!("{line}\n")).collect();
    let whole = lines.len() / BLOCK;
    let mut parts = vec![Part { text: "<chat>\n".into(), mark: whole == 0 }];
    for block in 0..whole {
        let text = lines[block * BLOCK..(block + 1) * BLOCK].concat();
        parts.push(Part { text, mark: block + 1 == whole });
    }
    let rest = lines[whole * BLOCK..].concat();
    parts.push(Part { text: format!("{rest}</chat>\n"), mark: false });
    if let Some(opening) = entries.get(turn) {
        parts.push(Part { text: raw(opening), mark: false });
    }

    let mut out = Vec::new();
    match system.and_then(|m| m.content.as_deref()) {
        Some(text) => out.push(Message::system(format!("{text}\n\n{GUIDE}"))),
        None => out.push(Message::system(GUIDE)),
    }
    out.push(Message::parts("user", parts));
    if view.frontier < turn {
        let end = turn.min(entries.len());
        out.extend_from_slice(&entries[view.frontier.min(end)..end]);
    }
    let start = turn.saturating_add(1).max(view.frontier).min(entries.len());
    out.extend_from_slice(&entries[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` short entries, all assistant, so `turn` is the end and the view is
    /// the whole request body.
    fn session(n: usize) -> (Vec<Message>, Tree, View) {
        let entries: Vec<Message> = (0..n).map(|i| Message::text("assistant", format!("m{i}"))).collect();
        let tree = Tree::default();
        let mut view = View::default();
        view.advance(n);
        (entries, tree, view)
    }

    fn lines(parts: &[Part]) -> Vec<String> {
        parts.iter().map(|p| p.text.clone()).collect()
    }

    #[test]
    fn fewer_than_four_lines_mark_the_chat_opening() {
        for n in [0, 3] {
            let (entries, tree, view) = session(n);
            let out = render(None, &entries, &tree, &view);
            assert_eq!(out.len(), 2);
            assert_eq!(out[0].content.as_deref(), Some(GUIDE));
            let parts = &out[1].parts;
            let view_text: String = (0..n).map(|i| format!("{i}+1|assistant: m{i}\n")).collect();
            assert_eq!(lines(parts)[0], "<chat>\n");
            assert_eq!(lines(parts)[1], format!("{view_text}</chat>\n"));
            assert!(parts[0].mark);
            assert!(!parts[1].mark);
        }
    }

    #[test]
    fn four_lines_mark_exactly_the_block() {
        let (entries, tree, view) = session(4);
        let out = render(None, &entries, &tree, &view);
        let parts = &out[1].parts;
        let block: String = (0..4).map(|i| format!("{i}+1|assistant: m{i}\n")).collect();
        assert_eq!(lines(parts), vec!["<chat>\n".to_string(), block, "</chat>\n".to_string()]);
        assert_eq!(parts.iter().map(|p| p.mark).collect::<Vec<_>>(), [false, true, false]);
    }

    #[test]
    fn nine_lines_mark_the_last_full_block_and_leave_the_fifth_line_unmarked() {
        let (entries, tree, view) = session(9);
        let out = render(None, &entries, &tree, &view);
        let parts = &out[1].parts;
        let block =
            |from: usize| -> String { (from..from + BLOCK).map(|i| format!("{i}+1|assistant: m{i}\n")).collect() };
        assert_eq!(
            lines(parts),
            vec!["<chat>\n".to_string(), block(0), block(4), "8+1|assistant: m8\n</chat>\n".to_string()]
        );
        assert_eq!(parts.iter().map(|p| p.mark).collect::<Vec<_>>(), [false, false, true, false]);
    }

    #[test]
    fn the_opening_entry_is_raw_after_the_closing_tag_and_later_entries_follow() {
        let entries =
            vec![Message::text("assistant", "old"), Message::user("open"), Message::text("assistant", "answer")];
        let mut view = View::default();
        view.advance(1);
        let out = render(Some(&Message::system("sys")), &entries, &Tree::default(), &view);
        assert_eq!(out[0].content.as_deref(), Some(format!("sys\n\n{GUIDE}").as_str()));
        let parts = &out[1].parts;
        assert_eq!(lines(parts), vec!["<chat>\n", "0+1|assistant: old\n</chat>\n", "open"]);
        assert_eq!(out[2].content.as_deref(), Some("answer"));
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn a_frontier_behind_the_turn_leaves_the_older_entries_raw_after_the_user_message() {
        let entries = vec![Message::user("u1"), Message::text("assistant", "a1"), Message::user("u2")];
        let out = render(None, &entries, &Tree::default(), &View::default());
        let parts = &out[1].parts;
        assert_eq!(lines(parts), vec!["<chat>\n", "</chat>\n", "u2"]);
        assert_eq!(out[2].content.as_deref(), Some("u1"));
        assert_eq!(out[3].content.as_deref(), Some("a1"));
        assert_eq!(out.len(), 4);
    }
}
