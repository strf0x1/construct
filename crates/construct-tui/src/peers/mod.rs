//! Peers in the TUI (#365, ADR 0074): the tile in the upper right, the popup
//! that approves a join or answers a peer's permission question, `/peer`, and
//! the attach view of a peer's session.
//!
//! The TUI never names the hub crate (ADR 0069): the binary hands it a
//! [`PeerHub`], which speaks the hub's control socket, and a background task
//! polls it once a second, after the first frame. No hub, no tile. The one
//! design choice: a peer's question is answered only by a key pressed in the
//! peer popup. Nothing on the agent's side holds a `PeerHub`, so the
//! coordinator's model can never approve anything on a peer.

mod attach;
mod panel;
mod poll;
mod tile;

use super::theme;
use ratatui::style::Color;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, watch};

pub(super) use attach::command;
pub(super) use panel::key;
pub(super) use tile::draw;

/// Where a peer stands in the hub's registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// A slot an invite or `construct build` made; nobody has joined it yet.
    Expected,
    /// A join waiting for a human, with its link open.
    Pending,
    Pinned,
    Revoked,
}

/// A permission question a peer is waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub title: String,
    /// `(option id, the peer's wording)`, in the order offered.
    pub options: Vec<(String, String)>,
}

/// One peer as the hub reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRow {
    pub id: String,
    pub standing: Standing,
    pub online: bool,
    /// A prompt is in flight or the peer's turn is running.
    pub working: bool,
    /// Unix seconds of the last message or disconnect.
    pub last_seen: Option<u64>,
    /// The address the live link came from.
    pub remote: Option<String>,
    /// The SAS code of a waiting join; the peer prints the same.
    pub code: Option<String>,
    /// Unix seconds when a waiting join is turned away.
    pub expires: Option<u64>,
    pub questions: Vec<Question>,
    /// The model its calls use, and whether the hub set it (#390).
    pub model: Option<(String, bool)>,
}

/// What a human did about a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    Approve(String),
    Deny(String),
    Prompt {
        id: String,
        text: String,
    },
    /// Use `model` for a peer's model calls; `None` clears the override.
    Model {
        id: String,
        model: Option<String>,
    },
    /// Answer question `question` (1-based) with `option`; `title` is the
    /// question the human saw, so a list that moved under them is refused.
    Answer {
        id: String,
        question: usize,
        title: String,
        option: String,
    },
}

/// The hub, as the TUI reaches it. The binary implements it over the hub's
/// control socket when it is built with the `hub` feature.
#[async_trait::async_trait]
pub trait PeerHub: Send + Sync {
    /// Every peer; `Ok(None)` when no hub runs for this home.
    async fn peers(&self) -> Result<Option<Vec<PeerRow>>, String>;
    async fn act(&self, act: Act) -> Result<(), String>;
    /// One peer's session as text (transcript, turn, questions).
    async fn session(&self, id: &str) -> Result<String, String>;
}

/// What the poller tells the UI.
pub(super) enum Msg {
    Peers(Option<Vec<PeerRow>>),
    /// An act finished: what to say about it, or why it failed.
    Done(Result<String, String>),
    Session(String, Result<String, String>),
}

/// What the popup shows: a join to decide or a question to answer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
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

/// The attach view: one peer's session, refreshed while it is open.
pub(super) struct Attach {
    id: String,
    text: String,
    scroll: usize,
}

/// The TUI's peer state.
#[derive(Default)]
pub(super) struct Peers {
    rows: Vec<PeerRow>,
    acts: Option<mpsc::UnboundedSender<Act>>,
    watching: Option<watch::Sender<Option<String>>>,
    attach: Option<Attach>,
    /// The item on the popup and when it appeared (the input guard).
    shown: Option<(String, Instant)>,
    selected: usize,
    /// Items put off with esc, or answered and not yet gone from a poll.
    set_aside: HashSet<String>,
}

impl Peers {
    /// Poll `hub` from now on; messages arrive on `tx`.
    pub(super) fn start(&mut self, hub: Arc<dyn PeerHub>, tx: mpsc::UnboundedSender<Msg>) {
        let (acts, rx) = mpsc::unbounded_channel();
        let (watching, attached) = watch::channel(None);
        tokio::spawn(poll::run(hub, rx, attached, tx));
        (self.acts, self.watching) = (Some(acts), Some(watching));
    }

    /// The popup is up (it breathes, and its key guard runs out).
    pub(super) fn asking(&self) -> bool {
        self.shown.is_some()
    }

    fn send(&self, act: Act) {
        if let Some(tx) = &self.acts {
            let _ = tx.send(act);
        }
    }

    fn row(&self, id: &str) -> Option<&PeerRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    /// The first join or question not set aside: joins first, then questions
    /// in the hub's order.
    fn next(&self) -> Option<Item> {
        let joins = self.rows.iter().filter(|r| r.standing == Standing::Pending && r.code.is_some());
        let asks = self.rows.iter().flat_map(|r| (0..r.questions.len()).map(move |n| Item::Ask(r.clone(), n)));
        joins.map(|r| Item::Join(r.clone())).chain(asks).find(|i| !self.set_aside.contains(&i.key()))
    }

    /// The item on the popup, restarting the input guard when it changed.
    fn current(&mut self) -> Option<Item> {
        let item = self.next();
        let key = item.as_ref().map(Item::key);
        if self.shown.as_ref().map(|s| &s.0) != key.as_ref() {
            self.shown = key.map(|k| (k, Instant::now()));
            self.selected = 0;
        }
        item
    }

    fn attach(&mut self, id: &str) {
        self.attach = Some(Attach { id: id.to_string(), text: String::new(), scroll: 0 });
        if let Some(w) = &self.watching {
            w.send_replace(Some(id.to_string()));
        }
    }

    fn detach(&mut self) {
        self.attach = None;
        if let Some(w) = &self.watching {
            w.send_replace(None);
        }
    }
}

impl Peers {
    /// Fold in one message from the poller; what to say about it, if anything.
    pub(super) fn on(&mut self, msg: Msg) -> Option<(Color, String)> {
        match msg {
            Msg::Peers(rows) => {
                self.rows = rows.unwrap_or_default();
                let live: HashSet<String> = self.rows.iter().flat_map(open_items).collect();
                self.set_aside.retain(|k| live.contains(k));
                self.current();
                None
            }
            Msg::Done(Ok(said)) => Some((theme::CYAN, format!("⇄ {said}"))),
            Msg::Done(Err(why)) => {
                // Whatever was answered may still be open: show it again.
                self.set_aside.clear();
                Some((theme::RED, format!("⇄ {why}")))
            }
            Msg::Session(id, text) => {
                if let Some(a) = self.attach.as_mut().filter(|a| a.id == id) {
                    a.text = text.unwrap_or_else(|e| format!("session unavailable: {e}"));
                }
                None
            }
        }
    }
}

/// Every item `row` could put on the popup.
fn open_items(row: &PeerRow) -> Vec<String> {
    let join = (row.standing == Standing::Pending && row.code.is_some()).then(|| Item::Join(row.clone()));
    join.into_iter().chain((0..row.questions.len()).map(|n| Item::Ask(row.clone(), n))).map(|i| i.key()).collect()
}

/// Peer text cannot emit terminal commands or hide content with carriage returns.
fn clean(text: &str) -> String {
    text.chars()
        .flat_map(|c| if c.is_control() && c != '\n' { c.escape_default().collect() } else { vec![c] })
        .collect()
}

/// Unix seconds now.
fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn row(id: &str, standing: Standing) -> PeerRow {
        PeerRow {
            id: id.into(),
            standing,
            online: true,
            working: false,
            last_seen: None,
            remote: Some("10.0.0.5:5000".into()),
            code: None,
            expires: None,
            questions: vec![],
            model: None,
        }
    }

    fn ask(title: &str) -> Question {
        Question { title: title.into(), options: vec![("once".into(), "Allow once".into())] }
    }

    #[test]
    fn joins_come_before_questions_and_set_aside_items_wait() {
        let mut busy = row("pi", Standing::Pinned);
        busy.questions = vec![ask("read /etc/shadow"), ask("write /tmp/x")];
        let join = PeerRow { code: Some("K7M-4QP".into()), ..row("old-pc", Standing::Pending) };
        let mut p = Peers { rows: vec![busy, join], ..Peers::default() };
        let first = p.current().unwrap();
        assert!(matches!(&first, Item::Join(r) if r.id == "old-pc"), "{first:?}");
        p.set_aside.insert(first.key());
        assert!(matches!(p.current(), Some(Item::Ask(r, 0)) if r.id == "pi"));
        p.set_aside.insert(Item::Ask(p.rows[0].clone(), 0).key());
        assert!(matches!(p.current(), Some(Item::Ask(_, 1))));
    }

    #[test]
    fn a_new_item_restarts_the_input_guard() {
        let mut busy = row("pi", Standing::Pinned);
        busy.questions = vec![ask("one")];
        let mut p = Peers { rows: vec![busy], selected: 1, ..Peers::default() };
        p.current();
        let first = p.shown.clone().unwrap();
        assert_eq!(p.selected, 0);
        p.current();
        assert_eq!(p.shown.as_ref().unwrap().1, first.1, "the same question keeps its clock");
        p.rows[0].questions = vec![ask("two")];
        p.current();
        assert_ne!(p.shown.unwrap().0, first.0);
    }

    #[test]
    fn peer_text_cannot_carry_escapes() {
        assert_eq!(clean("a\x1b[2Jb\rc\nd"), "a\\u{1b}[2Jb\\rc\nd");
    }
}
