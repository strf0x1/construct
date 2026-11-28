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
mod types;

use crate::theme;
use attach::{Attach, AttachDrawn, AttachKey};
use crossterm::event::KeyEvent;
use panel::{Popup, PopupDrawn, PopupKey};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::sync::Arc;
use tile::TileRow;
use tokio::sync::{mpsc, watch};
use types::now;

pub(crate) use tile::Tile;
pub use types::{Act, PeerHub, PeerRow, Question, Standing};
pub(crate) use types::{Decision, Msg};

/// Where everything peer-related landed this frame, for `motion`.
pub(crate) struct Drawn {
    pub(crate) tile: Option<(Rect, Vec<TileRow>)>,
    pub(crate) popup: Option<PopupDrawn>,
    pub(crate) decision: Option<Decision>,
    pub(crate) attach: Option<AttachDrawn>,
}

/// The TUI's peer state: coordinates the tile, popup, attach view and hub poller.
#[derive(Default)]
pub(crate) struct Peers {
    rows: Vec<PeerRow>,
    acts: Option<mpsc::UnboundedSender<Act>>,
    watching: Option<watch::Sender<Option<String>>>,
    attach: Option<Attach>,
    popup: Popup,
}

impl Peers {
    /// Poll `hub` from now on; messages arrive on `tx`.
    pub(crate) fn start(&mut self, hub: Arc<dyn PeerHub>, tx: mpsc::UnboundedSender<Msg>) {
        let (acts, rx) = mpsc::unbounded_channel();
        let (watching, attached) = watch::channel(None);
        tokio::spawn(poll::run(hub, rx, attached, tx));
        (self.acts, self.watching) = (Some(acts), Some(watching));
    }

    /// The popup is up (it breathes, and its key guard runs out).
    pub(crate) fn asking(&self) -> bool {
        self.popup.asking()
    }

    /// Fold in one message from the poller; what to say about it, if anything.
    pub(crate) fn on(&mut self, msg: Msg) -> Option<(Color, String)> {
        match msg {
            Msg::Peers(rows) => {
                self.rows = rows.unwrap_or_default();
                self.popup.sync(&self.rows);
                None
            }
            Msg::Done(Ok(said)) => Some((theme::CYAN, format!("⇄ {said}"))),
            Msg::Done(Err(why)) => {
                self.popup.clear_set_aside();
                Some((theme::RED, format!("⇄ {why}")))
            }
            Msg::Session(id, text) => {
                if let Some(a) = self.attach.as_mut().filter(|a| a.id() == id) {
                    a.set_text(text.unwrap_or_else(|e| format!("session unavailable: {e}")));
                }
                None
            }
        }
    }

    /// Handle a key for the peer popup or the attach view; true when used here.
    pub(crate) fn key(&mut self, k: KeyEvent) -> bool {
        match self.popup.key(&self.rows, k) {
            PopupKey::Consumed => return true,
            PopupKey::Send(act) => {
                self.send(act);
                return true;
            }
            PopupKey::Inactive => {}
        }
        let Some(a) = self.attach.as_mut() else {
            return false;
        };
        match a.key(k) {
            AttachKey::Close => {
                self.detach();
                true
            }
            AttachKey::Consumed => true,
            AttachKey::Ignored => false,
        }
    }

    /// `/peer`, `/peer <name>`, `/peer <name> <prompt>`,
    /// `/peer <name> --model [<selector> | --clear]`; what to say about it.
    pub(crate) fn command(&mut self, arg: &str) -> Option<(Color, String)> {
        let (id, text) = arg.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((arg, ""));
        if self.acts.is_none() {
            return Some((theme::DIM, "no peers: this build has no hub".into()));
        }
        if id.is_empty() {
            let list = tile::lines(&self.rows, now());
            if list.is_empty() {
                let none = "no peers yet (is `construct hub` running? `construct hub invite <id>` adds one)";
                return Some((theme::DIM, none.into()));
            }
            let rows: Vec<String> = list.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
            let how = "/peer <name> watches one · /peer <name> <prompt> sends · /peer <name> --model <model> switches";
            return Some((theme::DIM, format!("peers:\n{}\n{how}", rows.join("\n"))));
        }
        let Some(row) = self.row(id) else {
            return Some((theme::YELLOW, format!("no peer {id} — /peer lists them")));
        };
        if let Some(rest) = text.strip_prefix("--model").filter(|r| r.is_empty() || r.starts_with(' ')) {
            return self.model_command(id, rest.trim());
        }
        if text.is_empty() {
            // Whatever it is waiting on comes back to the popup.
            self.popup.unhide_peer(id);
            self.attach(id);
            return None;
        }
        if row.standing != Standing::Pinned {
            let state = Tile::of(row, now()).word();
            return Some((theme::YELLOW, format!("{id} is {state}: only a joined peer takes prompts")));
        }
        self.send(Act::Prompt { id: id.to_string(), text: text.to_string() });
        None
    }

    /// Draw everything peer-related over `area` (the transcript's rows): the
    /// attach view, the tile, then the popup when `popup` (no local question is
    /// on screen, which takes the keys first).
    pub(crate) fn draw(&mut self, f: &mut Frame, area: Rect, popup: bool, phase: f32) -> Drawn {
        let now = now();
        let attach = self.attach.as_mut().map(|a| {
            let state = self.rows.iter().find(|r| r.id == a.id()).map(|r| Tile::of(r, now));
            a.render(f, area, state)
        });
        let tile = tile::render(f, area, &self.rows, now);
        let (popup_drawn, decision) = self.popup.draw(f, area, &self.rows, popup, phase, now);
        Drawn { tile, popup: popup_drawn, decision, attach }
    }

    fn send(&self, act: Act) {
        if let Some(tx) = &self.acts {
            let _ = tx.send(act);
        }
    }

    fn row(&self, id: &str) -> Option<&PeerRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    fn attach(&mut self, id: &str) {
        self.attach = Some(Attach::new(id.to_string()));
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

    fn model_command(&self, id: &str, arg: &str) -> Option<(Color, String)> {
        let row = self.row(id)?;
        match arg {
            "" => {
                let shown = match &row.model {
                    Some((m, true)) => format!("{id}: {m}, set at the hub · /peer {id} --model --clear goes back"),
                    Some((m, false)) => format!("{id}: {m}, its own"),
                    None => format!("{id}: its own model (no model call through the hub yet)"),
                };
                Some((theme::DIM, shown))
            }
            _ if row.standing == Standing::Revoked => Some((theme::YELLOW, format!("{id} is revoked"))),
            "--clear" => {
                self.send(Act::Model { id: id.to_string(), model: None });
                None
            }
            m => {
                self.send(Act::Model { id: id.to_string(), model: Some(m.to_string()) });
                None
            }
        }
    }
}
