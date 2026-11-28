//! The read-only attach view of a peer's session: lies over the transcript's
//! rows only, so `/peer <name> <prompt>` can be typed while watching, and
//! esc (or pgup/pgdn to scroll) are the only keys it takes.

use super::tile::Tile;
use super::types::clean;
use crate::markdown::wrap;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

/// The peer attach view as drawn this frame, for `motion`.
pub(crate) struct AttachDrawn {
    pub(crate) area: Rect,
    pub(crate) inner: Rect,
    pub(crate) id: String,
    pub(crate) top: usize,
    pub(crate) text_len: usize,
    pub(crate) working: bool,
}

/// What a key did to the attach view.
pub(super) enum AttachKey {
    Close,
    Consumed,
    Ignored,
}

/// The attach view: one peer's session, refreshed while it is open.
pub(super) struct Attach {
    id: String,
    text: String,
    scroll: usize,
}

impl Attach {
    pub(super) fn new(id: String) -> Self {
        Self { id, text: String::new(), scroll: 0 }
    }

    pub(super) fn id(&self) -> &str {
        &self.id
    }

    pub(super) fn set_text(&mut self, text: String) {
        self.text = text;
    }

    pub(super) fn key(&mut self, k: KeyEvent) -> AttachKey {
        match k.code {
            KeyCode::Esc => AttachKey::Close,
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_add(10);
                AttachKey::Consumed
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                AttachKey::Consumed
            }
            _ => AttachKey::Ignored,
        }
    }

    /// Draw the attach view over `area`, the transcript's rows.
    pub(super) fn render(&mut self, f: &mut Frame, area: Rect, state: Option<Tile>) -> AttachDrawn {
        let word = state.map_or("gone", Tile::word);
        let title = Span::styled(
            format!(" ⇄ {} · {word} ", self.id),
            Style::new().fg(theme::CYAN).add_modifier(Modifier::BOLD),
        );
        let hint = format!(" esc close · pgup/pgdn scroll · /peer {} <prompt> sends ", self.id);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme::CYAN))
            .padding(Padding::horizontal(1))
            .title_top(Line::from(title))
            .title_bottom(Line::from(Span::styled(hint, theme::dim())));
        let inner = block.inner(area);
        let text = if self.text.is_empty() { "loading…".to_string() } else { clean(&self.text) };
        let w = inner.width as usize;
        let body: Vec<Line<'static>> =
            text.lines().flat_map(|l| wrap(vec![Span::raw(l.to_string())], w, vec![], vec![])).collect();
        // Pinned to the bottom, like the transcript; pgup walks back.
        let bottom = body.len().saturating_sub(inner.height as usize);
        self.scroll = self.scroll.min(bottom);
        let top = bottom - self.scroll;
        let shown: Vec<Line<'static>> = body.into_iter().skip(top).take(inner.height as usize).collect();
        f.render_widget(Clear, area);
        f.render_widget(block, area);
        f.render_widget(Paragraph::new(shown), inner);
        AttachDrawn {
            area,
            inner,
            id: self.id.clone(),
            top,
            text_len: self.text.len(),
            working: state == Some(Tile::Working),
        }
    }
}
