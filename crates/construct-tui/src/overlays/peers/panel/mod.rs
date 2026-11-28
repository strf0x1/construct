//! The peer popup: approve or deny a join (with the SAS code to compare and
//! its countdown), or answer a peer's permission question.
//!
//! The design choice: it must never pass for the local permission panel. Its
//! badge names the peer, its border breathes cyan and lavender instead of
//! orange, its intro says the question is about the peer's own machine, and
//! "Always" says it is saved to that peer's config, not this host's. Keys are
//! ignored for the same 450 ms after it appears (ADR 0016), and esc puts an
//! item aside rather than answering it: the tile keeps showing it.

mod draw;

use super::types::{Act, Decision, PeerRow, Standing, clean};
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::collections::HashSet;
use std::time::Instant;

/// The peer popup as drawn this frame, for `motion`.
pub(crate) struct PopupDrawn {
    pub(crate) area: Rect,
    pub(crate) key: String,
    pub(crate) is_join: bool,
}

/// What a key did to the peer popup.
pub(super) enum PopupKey {
    Inactive,
    Consumed,
    Send(Act),
}

/// What the popup shows: a join to decide or a question to answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Item {
    Join(PeerRow),
    Ask(PeerRow, usize),
}

impl Item {
    /// Identity across polls: a new code or a new question is a new item.
    fn key(&self) -> String {
        match self {
            Item::Join(p) => format!("join\0{}\0{}", p.id, p.code.as_deref().unwrap_or_default()),
            Item::Ask(p, n) => format!("ask\0{}\0{n}\0{}", p.id, p.questions[*n].title),
        }
    }
}

/// One choice: what it sends, what it says, its color and its key.
pub(super) struct Choice {
    pub(super) option: String,
    pub(super) text: String,
    pub(super) color: Color,
    pub(super) key: Option<char>,
}

pub(super) fn choices(item: &Item) -> Vec<Choice> {
    let choice = |option: &str, text: String, color, key| Choice { option: option.into(), text, color, key };
    match item {
        Item::Join(_) => vec![
            choice("approve", "Approve · pins its key".into(), theme::GREEN, Some('a')),
            choice("deny", "Deny".into(), theme::RED, Some('d')),
        ],
        Item::Ask(p, n) => p.questions[*n]
            .options
            .iter()
            .map(|(id, label)| {
                let label = clean(if label.is_empty() { id } else { label });
                let peer = &p.id;
                match id.as_str() {
                    "once" => choice(id, format!("Allow once on {peer}"), theme::GREEN, Some('y')),
                    "session" => choice(id, format!("Allow for {peer}'s session"), theme::GREEN, Some('s')),
                    "always" => {
                        choice(id, format!("Always allow · saved to {peer}'s config"), theme::YELLOW, Some('a'))
                    }
                    "deny" => choice(id, "Deny".into(), theme::RED, Some('n')),
                    i if i.ends_with("always") => {
                        choice(id, format!("{label} · saved to {peer}'s config"), theme::YELLOW, None)
                    }
                    i if i.ends_with("session") => choice(id, format!("{label} on {peer}"), theme::GREEN, None),
                    _ => choice(id, label, theme::DIM, None),
                }
            })
            .collect(),
    }
}

/// Every item `row` could put on the popup.
fn open_items(row: &PeerRow) -> Vec<String> {
    let join = (row.standing == Standing::Pending && row.code.is_some()).then(|| Item::Join(row.clone()));
    join.into_iter().chain((0..row.questions.len()).map(|n| Item::Ask(row.clone(), n))).map(|i| i.key()).collect()
}

/// State of the peer join/question popup.
#[derive(Default)]
pub(super) struct Popup {
    /// The item on the popup and when it appeared (the input guard).
    shown: Option<(String, Instant)>,
    selected: usize,
    /// Items put off with esc, or answered and not yet gone from a poll.
    set_aside: HashSet<String>,
    /// The last popup decision, consumed on the next draw for `motion`.
    decision: Option<Decision>,
}

impl Popup {
    /// The popup is up (it breathes, and its key guard runs out).
    pub(super) fn asking(&self) -> bool {
        self.shown.is_some()
    }

    /// Reconcile set-aside keys and the active popup item against fresh peer rows.
    pub(super) fn sync(&mut self, rows: &[PeerRow]) {
        let live: HashSet<String> = rows.iter().flat_map(open_items).collect();
        self.set_aside.retain(|k| live.contains(k));
        self.current(rows);
    }

    /// Whatever was answered may still be open: show all items again.
    pub(super) fn clear_set_aside(&mut self) {
        self.set_aside.clear();
    }

    /// Bring any set-aside joins/questions for `id` back to the popup.
    pub(super) fn unhide_peer(&mut self, id: &str) {
        self.set_aside.retain(|k| !k.contains(&format!("\0{id}\0")));
    }

    /// The first join or question not set aside: joins first, then questions
    /// in the hub's order.
    fn next(&self, rows: &[PeerRow]) -> Option<Item> {
        let joins = rows.iter().filter(|r| r.standing == Standing::Pending && r.code.is_some());
        let asks = rows.iter().flat_map(|r| (0..r.questions.len()).map(move |n| Item::Ask(r.clone(), n)));
        joins.map(|r| Item::Join(r.clone())).chain(asks).find(|i| !self.set_aside.contains(&i.key()))
    }

    /// The item on the popup, restarting the input guard when it changed.
    fn current(&mut self, rows: &[PeerRow]) -> Option<Item> {
        let item = self.next(rows);
        let key = item.as_ref().map(Item::key);
        if self.shown.as_ref().map(|s| &s.0) != key.as_ref() {
            self.shown = key.map(|k| (k, Instant::now()));
            self.selected = 0;
        }
        item
    }

    /// Handle a key while the popup may be up; while active it takes every key.
    pub(super) fn key(&mut self, rows: &[PeerRow], k: KeyEvent) -> PopupKey {
        let Some(item) = self.current(rows) else {
            return PopupKey::Inactive;
        };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if k.code == KeyCode::Esc || (ctrl && k.code == KeyCode::Char('c')) {
            self.decision = Some(Decision::Later);
            self.set_aside.insert(item.key());
            self.current(rows);
            return PopupKey::Consumed;
        }
        if !self.shown.as_ref().is_some_and(|s| s.1.elapsed() >= theme::GRACE) {
            return PopupKey::Consumed;
        }
        let options = choices(&item);
        let n = options.len().max(1);
        let pick = match k.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = (self.selected + n - 1) % n;
                None
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.selected = (self.selected + 1) % n;
                None
            }
            KeyCode::Enter => options.get(self.selected).map(|o| (self.selected, o)),
            KeyCode::Char(c) if !ctrl => {
                options.iter().enumerate().find(|(_, o)| o.key == Some(c.to_ascii_lowercase()))
            }
            _ => None,
        };
        let Some((idx, pick)) = pick else { return PopupKey::Consumed };
        let pop = idx == self.selected;
        let (act, decision) = match &item {
            Item::Join(p) if pick.option == "approve" => (Act::Approve(p.id.clone()), Decision::Allow { pop }),
            Item::Join(p) => (Act::Deny(p.id.clone()), Decision::Deny { pop }),
            Item::Ask(p, q) => {
                let decision = match pick.option.as_str() {
                    "deny" => Decision::Deny { pop },
                    o if o == "always" || o.ends_with("always") => Decision::Always { pop },
                    _ => Decision::Allow { pop },
                };
                (
                    Act::Answer {
                        id: p.id.clone(),
                        question: q + 1,
                        title: p.questions[*q].title.clone(),
                        option: pick.option.clone(),
                    },
                    decision,
                )
            }
        };
        self.decision = Some(decision);
        self.set_aside.insert(item.key());
        self.current(rows);
        PopupKey::Send(act)
    }

    /// Draw the active popup item centered in `area` (when `allow`), returning
    /// its frame placement and any decision made since the last draw.
    pub(super) fn draw(
        &mut self,
        f: &mut Frame,
        area: Rect,
        rows: &[PeerRow],
        allow: bool,
        phase: f32,
        now: u64,
    ) -> (Option<PopupDrawn>, Option<Decision>) {
        let drawn = if allow && let Some(item) = self.current(rows) {
            let ready = self.shown.as_ref().is_some_and(|s| s.1.elapsed() >= theme::GRACE);
            let at = draw::render_item(f, area, &item, self.selected, ready, phase, now);
            Some(PopupDrawn { area: at, key: item.key(), is_join: matches!(item, Item::Join(_)) })
        } else {
            None
        };
        (drawn, self.decision.take())
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::{Question, test_row as row};
    use super::*;

    fn ask(title: &str) -> Question {
        Question { title: title.into(), options: vec![("once".into(), "Allow once".into())] }
    }

    #[test]
    fn joins_come_before_questions_and_set_aside_items_wait() {
        let mut busy = row("pi", Standing::Pinned);
        busy.questions = vec![ask("read /etc/shadow"), ask("write /tmp/x")];
        let join = PeerRow { code: Some("K7M-4QP".into()), ..row("old-pc", Standing::Pending) };
        let rows = vec![busy, join];
        let mut p = Popup::default();
        let first = p.current(&rows).unwrap();
        assert!(matches!(&first, Item::Join(r) if r.id == "old-pc"), "{first:?}");
        p.set_aside.insert(first.key());
        assert!(matches!(p.current(&rows), Some(Item::Ask(r, 0)) if r.id == "pi"));
        p.set_aside.insert(Item::Ask(rows[0].clone(), 0).key());
        assert!(matches!(p.current(&rows), Some(Item::Ask(_, 1))));
    }

    #[test]
    fn a_new_item_restarts_the_input_guard() {
        let mut busy = row("pi", Standing::Pinned);
        busy.questions = vec![ask("one")];
        let mut rows = vec![busy];
        let mut p = Popup { selected: 1, ..Popup::default() };
        p.current(&rows);
        let first = p.shown.clone().unwrap();
        assert_eq!(p.selected, 0);
        p.current(&rows);
        assert_eq!(p.shown.as_ref().unwrap().1, first.1, "the same question keeps its clock");
        rows[0].questions = vec![ask("two")];
        p.current(&rows);
        assert_ne!(p.shown.unwrap().0, first.0);
    }
}
